# bot_flow

Multi-tenant RAG customer-service chatbot SaaS. Tenants upload support documents (`pdf`/`txt`/`md`);
the platform parses, chunks, embeds and indexes them per tenant. End users ask questions through an
embeddable JS widget and get answers grounded **only** in that tenant's documents, with citations,
streamed over SSE.

> **This is a Rust Cargo workspace, not a Node project.** No `package.json`, no npm, no eslint.
> Reach for `cargo`, `sqlx` and `docker compose`.

`crates/api` (Axum HTTP server) · `crates/worker` (RabbitMQ consumer) · `crates/common` (shared
object-key contract) · `sidecar/` (Python `pypdf` extractor) · `widget/` (vanilla JS, no build step).

Backing services: Postgres 16, Qdrant, MinIO, RabbitMQ, Redis. Embeddings run locally
(`MultilingualE5Small`, 384-dim). The LLM is any OpenAI-compatible `/chat/completions` endpoint.

## Commands

```bash
docker compose up -d      # five backing services (the binaries run on the host)
cargo run -p api          # http://localhost:3000 — also runs DB migrations on boot
cargo run -p worker       # ingestion consumer
cargo test                # inline #[cfg(test)] unit tests; no integration suite
cargo clippy && cargo fmt # stock defaults, no config files
```

Rust is pinned to 1.95.0 (`rust-toolchain.toml`). First run downloads ~465 MB of model weights.

## Three invariants that must never break

1. **Every Qdrant search is filtered by tenant.** `.filter(tenant_filter(&tenant.tenant_id))` is not
   optional. A search without it returns other customers' documents.
2. **Every query on `documents` / `conversations` / `messages` goes through `db::tenant_tx()`.**
   Postgres RLS denies by default, so a forgotten tenant scope silently returns zero rows — or, on a
   dirty pooled connection, the wrong tenant's.
3. **API keys are stored as SHA-256 hashes and never logged.** The raw key is shown exactly once, at
   mint. No secret, token or `.env` value belongs in any tracked file.

## Steering files

Read the relevant file **before** starting, not after.

| Doing this | Read first |
| --- | --- |
| Changing what the system *does* — any business rule or invariant | [specs/spec.md](specs/spec.md) |
| Adding or changing an endpoint, error, or response shape | [steering/api-standards.md](steering/api-standards.md) |
| Writing any query, or anything touching `tenant_id` | [steering/tenant-isolation.md](steering/tenant-isolation.md) |
| Touching auth, API keys, CORS, uploads, or input validation | [steering/security-policies.md](steering/security-policies.md) |
| Changing chunking, embedding, retrieval, prompts, or the worker | [steering/rag-pipeline.md](steering/rag-pipeline.md) |
| Writing Rust anywhere in the workspace | [steering/code-conventions.md](steering/code-conventions.md) |
| Adding a test | [steering/testing-standards.md](steering/testing-standards.md) |
| Migrations, Docker, env vars, deploying, rolling back | [steering/deployment-workflow.md](steering/deployment-workflow.md) |

`README.md` covers the architecture narrative and a full walkthrough. The steering files cover the
rules and the reasons. [specs/spec.md](specs/spec.md) is the Single Source of Truth for business
logic — **behaviour changes are defined there before they are coded.**

## Known state

- `POST /ingest` writes vectors with random ids and **no `document_id` payload**, so raw-text chunks
  duplicate on re-ingest, cite an empty document, and can never be listed or removed. Demo path, not
  a supported one. Full detail in [specs/spec.md](specs/spec.md) §5.1.
- `POST /documents` (multipart proxy) is **deprecated** — it buffers whole files in API memory. Use
  `POST /documents/upload-url`. It gets deleted along with `crates/api/src/queue.rs` and the worker's
  `consume_legacy` / `LEGACY_QUEUE`.
- There is **no delete path** for a document — bytes, vectors and row all persist forever.
- No CI, no integration tests, no `.env.example` (though `.gitignore` expects one). Nothing tests
  that RLS actually denies a cross-tenant read.
- `/search` accepts publishable (`pk_`) keys and is not rate limited. Believed to be an oversight,
  not a decision — raise it before relying on either behaviour.
