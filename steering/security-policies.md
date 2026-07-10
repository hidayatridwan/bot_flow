# Security Policies

> **Scope:** credentials, authn/authz, input validation, and what may never appear in this repo.
> Tenant data separation has its own file: [tenant-isolation.md](tenant-isolation.md).

---

## Zero tolerance: no secrets in this repository

Treat every file under `steering/`, every code comment, every commit message and every log line as
**public-facing**. Steering files in particular get shared, pasted into issues, and fed to third
parties.

**Never commit or write down:** API keys (`sk_…`, `pk_…`), `ADMIN_API_KEY`, `LLM_API_KEY`,
`S3_SECRET_KEY`, database or broker passwords, customer data, or the contents of `.env`.

Referring to a variable **by name** is fine and expected: `LLM_API_KEY` is documentation.
`LLM_API_KEY=sk-abc123` is an incident.

`.gitignore` already excludes `.env` and `.env.*` (allowing `.env.example`). If you need to show a
config, show the key with an empty value.

> **Open gap:** `.gitignore` instructs committing `.env.example`, but no `.env.example` exists in the
> repo. Creating one — names only, values blank — is a genuine task. See
> [deployment-workflow.md](deployment-workflow.md) for the full variable list.

---

## API keys

Two kinds, distinguished by prefix, both stored **only as a SHA-256 hex hash**.

| Prefix | Kind | Where it lives | Can do |
| --- | --- | --- | --- |
| `sk_` | secret | your server | everything: ingest, upload, list, ask |
| `pk_` | publishable | a browser, in page source | chat only, and only from an allowed `Origin` |

```rust
// crates/api/src/auth.rs

/// SHA-256 hex of a raw key — the form stored in api_keys.key_hash.
pub fn hash_key(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Two v4 UUIDs (~244 bits) of entropy; we only ever store its hash.
pub fn generate_key(kind: &str) -> String { /* ... */ }
```

**The raw key is shown exactly once**, in the response to `create_tenant` / `mint_key`, and is then
unrecoverable. The mint response says so:
`"note": "store this now; it won't be shown again"`.

**Why hash and not encrypt:** an encrypted key can be decrypted by whoever holds the key-encryption
key, which is on the same machine. A hash cannot be reversed at all, so a database dump is not a
credential dump. The cost is that we can never show a key again — that is the intended trade.

### Before / After

```rust
// ❌ BEFORE — the database is now a list of live credentials.
sqlx::query("INSERT INTO api_keys (key_hash, tenant_id, kind, label) VALUES ($1, $2, $3, $4)")
    .bind(&raw)                       // storing the raw key
    ...
tracing::info!("minted key {raw} for {tenant_id}");   // ...and logging it

// ✅ AFTER — only the hash is ever persisted, and the key is never logged.
sqlx::query("INSERT INTO api_keys (key_hash, tenant_id, kind, label) VALUES ($1, $2, $3, $4)")
    .bind(auth::hash_key(&raw))
    ...
```

**Rules**

- Never store, log, or `Debug`-print a raw key. Hash on the way in, always.
- Never add an endpoint that reads a key back out.
- New key kinds go through `generate_key` — do not roll a different random source.

---

## Authentication

`Authorization: Bearer <key>` on every non-`/health` route. Implemented as Axum extractors so it
cannot be forgotten: if a handler wants a tenant, it names `AuthTenant` in its signature and the
framework refuses to run it otherwise.

`AuthTenant` resolves the hash to `(tenant_id, kind, allowed_origins)`. If `kind == "publishable"`,
it additionally checks the request's `Origin` header against that key's allow-list, and returns
`403` on a mismatch.

`AdminAuth` guards `/admin/*` by comparing the bearer token to the `ADMIN_API_KEY` environment
variable — **not** a database row. It has no tenant, by design: it is the endpoint that *creates*
tenants.

---

## Authorization

```rust
/// Reject publishable (browser) keys on management/ingest endpoints.
pub fn require_secret(&self) -> Result<(), AppError>
```

Call `tenant.require_secret()?` as the **first line** of any handler that ingests, uploads, lists or
otherwise manages data. A `pk_` key is printed in a customer's page source; anyone can read it. Its
only legitimate power is asking questions.

Present on: `/ingest`, `/documents` (both verbs), `/documents/upload-url`,
`/documents/{id}/upload-url`. Absent, correctly, on `/ask` and `/ask/stream`. Absent, **as an
outstanding gap**, on `/search` — see the note in [api-standards.md](api-standards.md).

---

## Why CORS is `allow_origin(Any)` — and why that is not a bug

```rust
// crates/api/src/main.rs
let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);
```

Read this before "fixing" it.

CORS is a *browser* mechanism, not a server authorization mechanism. It cannot stop a curl request
or a server-side caller, so it was never the thing protecting this API. Two facts make a permissive
policy safe here:

1. **The real check is the publishable key's `allowed_origins` list**, enforced server-side in
   `AuthTenant`. That check runs regardless of what the browser decided to send.
2. **No cookies or session state are used.** Authentication is a Bearer token the page must
   deliberately attach. A malicious site cannot get the victim's credentials attached automatically,
   so there is no CSRF surface for a restrictive CORS policy to defend.

Tightening `allow_origin` would break every tenant's widget the moment they add a new domain, while
protecting nothing. The per-key allow-list is where origin policy belongs, because it is per tenant.

---

## Input validation

**Tenant slugs** are validated twice — in the application (`common::key::is_valid_slug`, to return a
`400`) and in the database (a `CHECK` constraint, so it can never be bypassed by any other writer).
The rule is `^[a-z0-9][a-z0-9-]{0,62}$`.

The slug is interpolated into every object key, and the key is precisely what a presigned URL
authorises. `a/../b` would escape its own prefix into another tenant's. `crates/common/src/key.rs`
tests this directly.

**File types** are restricted to what the parser can read (`pdf`, `txt`, `md`). Since a presigned PUT
cannot inspect content, `extension_of()` at mint time is the last moment we can refuse a file. The
content type is derived from the validated extension — *never* taken from the client:

```rust
/// Derived from the extension we validated, never taken from the client — a
/// client-supplied type would be a lie we then stored.
pub fn content_type_for(ext: &str) -> &'static str
```

**Upload size.** A presigned URL's signature binds the method, the key and the expiry — it does
**not** bind the body length. A client holding one can PUT a file of any size. `MAX_UPLOAD_BYTES` is
therefore enforced by the *worker*, on the `ObjectCreated` event: the bandwidth is already spent, so
all we can do is refuse to keep the object. Oversize documents are marked `quarantined` and the bytes
are deleted. This is the only place the cap can be applied — do not "move it earlier".

**SQL.** Every value is a bound parameter, including the RLS tenant setting (see
[tenant-isolation.md](tenant-isolation.md)). There is no string-formatted SQL in this codebase and
there must never be.

**The parser sidecar** (`sidecar/parser.py`) processes untrusted, attacker-supplied files. It runs
as a subprocess and signals failure with exit codes (`2` unreadable, `3` unsupported type) rather
than raising into the worker. Keep its dependency surface minimal — it is currently `pypdf` and
nothing else.

---

## Error messages

`AppError::Internal` logs the full error and returns `{"error": "internal server error"}`. Never
widen that. Stack traces, SQL fragments, hostnames and upstream response bodies all tell an attacker
how the system is built.

`AppError::client(status, msg)` messages are shown verbatim to callers, so they must describe the
caller's mistake and nothing about the system's internals. `"tenant 'acme' does not exist; create it
first"` is good. `"api_keys_tenant_id_fkey violation"` is not.

---

## Checklist for any change

- [ ] No secret, key, token or `.env` value added to any tracked file.
- [ ] New management endpoint starts with `tenant.require_secret()?`.
- [ ] No raw API key stored, logged, or returned outside the one-time mint response.
- [ ] All SQL values are bound parameters.
- [ ] New user-supplied string that reaches an object key or a filesystem path is validated.
- [ ] New 4xx messages leak nothing about internals.

---

## Maintenance

Review on every auth, key-handling or upload-path change, and at sprint planning. Treat edits as
production changes. Before merging, re-scan the diff for accidental credentials — a rotated key is
cheap, a leaked one in git history is forever.

Related: [tenant-isolation.md](tenant-isolation.md), [api-standards.md](api-standards.md),
[deployment-workflow.md](deployment-workflow.md).
