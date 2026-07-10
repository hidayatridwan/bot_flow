# Deployment Workflow

> **Scope:** running the stack locally, building images, configuration, migrations, rollback.

**There is no CI.** No `.github/workflows`, no `vercel.json`, no `fly.toml`. Everything below is
run by hand today. Do not write instructions that assume a pipeline exists.

---

## Local development

```bash
docker compose up -d      # five backing services
cargo run -p api          # terminal 1  → http://localhost:3000
cargo run -p worker       # terminal 2
```

`docker-compose.yml` starts **only the backing services** — the two Rust binaries run on the host.

**Why the split:** a Rust rebuild inside Docker invalidates the layer cache on every source change
and takes minutes. On the host, `cargo` reuses `target/` and rebuilds in seconds. The Dockerfile
exists for shipping, not for iterating.

| Service | Image | Ports |
| --- | --- | --- |
| postgres | `postgres:16-alpine` | 5432 |
| qdrant | `qdrant/qdrant:v1.18.0` | 6333 REST, 6334 gRPC |
| minio | `minio/minio:latest` | 9000 S3, 9001 console |
| rabbitmq | `rabbitmq:3.13-management-alpine` | 5672, 15672 UI |
| redis | `redis:7-alpine` | 6379 |

A one-shot `minio-init` container creates the `documents` bucket and binds its `ObjectCreated`
notification to the `minio.events` AMQP exchange with routing key `document.uploaded`. Without it,
uploads land in storage and nothing ever ingests them.

MinIO's notifier is configured **durable, delivery-mode 2 (persistent), mandatory, with publisher
confirms and an on-disk `QUEUE_DIR` buffer**. That combination is what makes an upload survive a
RabbitMQ restart: an unroutable event is returned and logged rather than dropped, and events queue
to disk while the broker is down.

All five have healthchecks; named volumes persist data across `docker compose down`. Use
`docker compose down -v` to reset — it destroys every document, vector and conversation.

First `cargo run` downloads ~465 MB of embedding weights into `.fastembed_cache/`. It is slow once,
then cached; both binaries rebuild the cache automatically if deleted.

The Python sidecar needs its own venv on the host:

```bash
python3 -m venv sidecar/.venv && sidecar/.venv/bin/pip install -r sidecar/requirements.txt
```

Rust is pinned to **1.95.0** by `rust-toolchain.toml`; rustup honours it automatically.

---

## Migrations

Plain `.sql` files in `crates/api/migrations/`, numbered `0001`–`0008`. They run **automatically at
API startup** via `sqlx::migrate!()` — there is no separate migrate command.

They run on the **admin (superuser) pool**, which is then closed immediately. The runtime pool
connects as the non-superuser `app_user` so RLS applies. This is not incidental; see
[tenant-isolation.md](tenant-isolation.md).

Migrations are **forward-only**. There are no `down` scripts.

Adding one:

1. `crates/api/migrations/0009_<what_it_does>.sql` — next number, descriptive snake_case name.
2. Never edit a migration that has run anywhere. `sqlx` checksums them and will refuse to start.
3. New table holding tenant data? `enable` **and** `force row level security`, plus a policy
   mirroring `0004_rls_documents.sql`. `0005` grants `app_user` default privileges on future
   tables, so no grant is needed — but the policy is not automatic.

---

## Configuration

Loaded from the environment via `dotenvy` (a root `.env` in dev), parsed in
`crates/api/src/config.rs`. A missing required variable aborts startup with a named error rather
than failing later at first use.

**Required** — no default, startup fails without them:

`DATABASE_URL`, `APP_DATABASE_URL`, `QDRANT_URL`, `RABBITMQ_URL`, `REDIS_URL`,
`LLM_BASE_URL`, `LLM_API_KEY`, `S3_ENDPOINT`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`, `ADMIN_API_KEY`

**Optional** — with defaults:

| Variable | Default | Notes |
| --- | --- | --- |
| `BIND_ADDR` | `0.0.0.0:3000` | |
| `LLM_MODEL` | `gemini/gemini-2.5-flash-lite` | any OpenAI-compatible `/chat/completions` model |
| `S3_PUBLIC_ENDPOINT` | falls back to `S3_ENDPOINT` | see below |
| `S3_BUCKET` | `documents` | |
| `S3_REGION` | `us-east-1` | |
| `PRESIGN_TTL_SECS` | `900` | |
| `RAG_SCORE_THRESHOLD` | `0.70` | see [rag-pipeline.md](rag-pipeline.md) |
| `RATE_LIMIT_PER_MINUTE` | `60` | per tenant, fixed window |
| `MAX_UPLOAD_BYTES` | 25 MiB | enforced by the worker |
| `RUST_LOG` | — | `info` in the images |
| `PARSER_PYTHON` / `PARSER_SCRIPT` | `python3` / `sidecar/parser.py` | set explicitly in the worker image |

**`S3_PUBLIC_ENDPOINT` is the one that will bite you.** A presigned URL's signature covers the Host
header, so it must be signed against the endpoint the *client's browser* will connect to — not the
one the API uses internally. In dev they are the same (`localhost:9000`) and it defaults correctly.
In any deployment where the API reaches MinIO over an internal network name, set this to the public
address or every presigned upload fails with a signature mismatch.

> **Never commit values.** `.gitignore` excludes `.env` and `.env.*` while allowing `.env.example` —
> but **`.env.example` does not exist yet**. Creating one, with the names above and blank values, is
> a real outstanding task. See [security-policies.md](security-policies.md).

---

## Building images

Multi-stage `Dockerfile`, three targets:

```bash
docker build --target api    -t bot_flow-api:latest .
docker build --target worker -t bot_flow-worker:latest .
```

1. **builder** (`rust:1.95-trixie`) — builds both binaries in one `cargo build --release -p api -p
   worker`. Cargo's registry and `target/` are BuildKit **cache mounts**. Cache mounts do not persist
   into image layers, which is why the binaries are explicitly `cp`'d out of `/build/target` before
   the stage ends. Remove those `cp` lines and the next stage finds nothing.
2. **api** (`debian:trixie-slim`) — needs `ca-certificates` (TLS to the LLM) and `libgomp1` (OpenMP,
   required by the ONNX runtime under `fastembed`). Exposes 3000.
3. **worker** (`debian:trixie-slim`) — the same, plus `python3` and the sidecar's `pypdf`.

Both runtime images download the embedding model on first start. For a real deployment, bake
`.fastembed_cache/` in or mount it as a volume — otherwise every restart re-downloads 465 MB before
the process becomes ready.

Neither image is in `docker-compose.yml`. Composing the full stack, including the app containers, is
an open task.

---

## Deploying

There is no automation. The manual sequence:

1. Build and push both images.
2. Ensure `DATABASE_URL` points at a superuser role — the API runs migrations on boot.
3. Start **the API first**. It creates the Qdrant collection with its `tenant_id` index and runs
   migrations. Starting the worker against a database missing `0005_app_role.sql` fails: it connects
   as `app_user`, which does not exist yet.
4. Start the worker. It declares its own RabbitMQ topology (`minio.events` → `document_events`, dead
   lettering to `doc.dlx` → `document_events.dlq`) and reconnects on its own after a broker drop.
5. `GET /health` returns `{"status": "ok"}` only when all five dependencies answer. Use it as the
   readiness probe — it does a real `HEAD` on the bucket and a real `PING` on Redis, not just a
   socket check.

---

## Rollback

Also manual, and constrained by one fact: **migrations are forward-only.**

- **Code only.** Redeploy the previous image tag. Safe whenever the schema did not change.
- **Schema changed.** The old binary must still work against the new schema. Additive migrations
  (new nullable column, new table) satisfy this; a dropped or renamed column does not. Plan
  destructive changes as two deploys — stop using the column, ship, then drop it in a later
  migration.
- **Undoing a migration** means writing a new forward migration that reverses it. Never delete or
  edit an applied one: `sqlx` checksums the directory and the API will refuse to start.
- **Qdrant** is not migrated at all. Changing the embedding model or dimension means recreating the
  collection and re-indexing every document — there is no rollback, only a rebuild.

---

## Maintenance

Review when the compose file, the Dockerfile, the migration set or the environment contract changes,
and at sprint planning. Treat edits as production changes. If CI is ever added, this file is where it
gets documented — and the "there is no CI" line at the top must go.

Related: [security-policies.md](security-policies.md), [tenant-isolation.md](tenant-isolation.md),
[rag-pipeline.md](rag-pipeline.md).
