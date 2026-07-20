//! Self-serve tenant accounts: register, login, session lifecycle, and self-service key
//! management. This is the *human* side of auth; `api_keys` remains the *machine* side (a
//! tenant's server and widget). Registration mints both — an account and the tenant's first `sk_`.
//!
//! Everything here touches only the global, non-RLS tables (`accounts`, `sessions`, `tenants`,
//! `api_keys`), so queries run on the plain pool — the tenant context these establish does not yet
//! exist when they run.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::{self, SessionAuth};
use crate::error::AppError;
use crate::handlers::{insert_api_key, provision_tenant};
use crate::rate_limit;
use crate::state::AppState;

/// Hash a password with Argon2id (random per-password salt). Returns a PHC string that carries the
/// algorithm, params and salt — the only form that reaches the database.
fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        // A hashing failure is an internal fault, not the caller's mistake.
        .map_err(|e| AppError::Internal(anyhow::anyhow!("argon2 hash: {e}")))
}

/// Verify a password against a stored PHC hash. A malformed stored hash verifies as `false` rather
/// than erroring — the caller only ever needs "did this password match".
fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A permissive email sanity check — enough to reject obvious garbage, not an RFC validator. The
/// real proof an address exists would be a confirmation email, which this MVP does not send.
fn is_plausible_email(s: &str) -> bool {
    let s = s.trim();
    match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !s.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// Derive a tenant slug from free text (used when `register` omits an explicit `slug`). Lowercases,
/// collapses runs of non-alphanumerics to single dashes, trims dashes, and caps length. May yield
/// an empty string (e.g. all punctuation) — the caller must handle that.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').chars().take(63).collect()
}

/// Insert a session for an account and return the raw token (shown once; only its hash is stored).
/// `expires_at` is computed in SQL from the configured TTL so the app never touches a clock.
async fn create_session<'e, E>(
    exec: E,
    account_id: uuid::Uuid,
    tenant_id: &str,
    ttl_secs: i64,
) -> Result<String, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let token = auth::generate_session_token();
    sqlx::query(
        "INSERT INTO sessions (token_hash, account_id, tenant_id, expires_at) \
         VALUES ($1, $2, $3, now() + ($4 * interval '1 second'))",
    )
    .bind(auth::hash_key(&token))
    .bind(account_id)
    .bind(tenant_id)
    .bind(ttl_secs)
    .execute(exec)
    .await?;
    Ok(token)
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    email: String,
    password: String,
    tenant_name: String,
    /// Optional: the tenant slug (the value baked into object keys and the Qdrant filter). Derived
    /// from `tenant_name` when omitted.
    #[serde(default)]
    slug: String,
}

/// `POST /auth/register` — public. Creates a tenant + owner account + session atomically, and
/// reveals the tenant's first `sk_` exactly once. Public and tenant-creating, so it is rate limited.
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    // Global bucket: this is the one endpoint that *creates tenants*, so the cap bounds abuse and
    // spend even before a caller is identifiable. Coarse by design; a captcha would refine it.
    rate_limit::check(&state, "auth:register").await?;

    // A body that parsed as JSON but carries a bad field value is a 422 (per the repo convention),
    // distinct from the 400 a malformed body / bad slug earns.
    if !is_plausible_email(&req.email) {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "email is not a valid address",
        ));
    }
    if req.password.len() < 8 {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "password must be at least 8 characters",
        ));
    }

    let slug = if req.slug.is_empty() {
        slugify(&req.tenant_name)
    } else {
        req.slug.clone()
    };
    if slug.is_empty() {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "could not derive a tenant slug from tenant_name; provide 'slug' explicitly",
        ));
    }

    let email = req.email.trim();
    let password_hash = hash_password(&req.password)?;

    // One unit of work: if the account or session insert fails, the tenant and its key are rolled
    // back too — no half-provisioned tenant with a key but no owner.
    let mut tx = state.db.begin().await?;

    // Pre-check for a clean 409 rather than surfacing the unique-index violation as a 500. A rare
    // concurrent double-register can still trip the index and fall through to a 500 — acceptable.
    let taken = sqlx::query("SELECT 1 FROM accounts WHERE lower(email) = lower($1)")
        .bind(email)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    if taken {
        return Err(AppError::client(
            StatusCode::CONFLICT,
            "an account with this email already exists",
        ));
    }

    let api_key = provision_tenant(&mut tx, &slug, &req.tenant_name).await?;

    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accounts (id, tenant_id, email, password_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(account_id)
    .bind(&slug)
    .bind(email)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;

    let session_token = create_session(&mut *tx, account_id, &slug, state.session_ttl_secs).await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session_token": session_token,
            "tenant_id": slug,
            "api_key": api_key,
            "note": "store the api_key now; it won't be shown again"
        })),
    ))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

/// `POST /auth/login` — public. Verifies credentials and mints a session. Never re-reveals an API
/// key. Rate limited per email to blunt brute force against one account.
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<Value>, AppError> {
    let email = req.email.trim();
    rate_limit::check(&state, &format!("auth:login:{}", email.to_lowercase())).await?;

    let row = sqlx::query(
        "SELECT id, tenant_id, password_hash FROM accounts WHERE lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await?;

    // Uniform failure: an unknown email and a wrong password return the identical 401, so the
    // endpoint is not an oracle for which emails are registered.
    let unauthorized = || AppError::client(StatusCode::UNAUTHORIZED, "invalid email or password");

    let row = row.ok_or_else(unauthorized)?;
    let stored: String = row.get("password_hash");
    if !verify_password(&req.password, &stored) {
        return Err(unauthorized());
    }

    let account_id: uuid::Uuid = row.get("id");
    let tenant_id: String = row.get("tenant_id");
    let session_token =
        create_session(&state.db, account_id, &tenant_id, state.session_ttl_secs).await?;

    Ok(Json(json!({
        "session_token": session_token,
        "tenant_id": tenant_id
    })))
}

/// `POST /auth/logout` — deletes just the current session (identified by its token hash).
pub async fn logout(
    session: SessionAuth,
    State(state): State<AppState>,
) -> Result<StatusCode, AppError> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(&session.token_hash)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /auth/me` — the account + tenant behind the current session. Used by the web BFF to hydrate
/// its request-local user state.
pub async fn me(
    session: SessionAuth,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let row = sqlx::query(
        "SELECT a.email, t.id AS tenant_id, t.name AS tenant_name \
         FROM accounts a JOIN tenants t ON t.id = a.tenant_id WHERE a.id = $1",
    )
    .bind(session.account_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({
        "account": { "email": row.get::<String, _>("email") },
        "tenant": {
            "id": row.get::<String, _>("tenant_id"),
            "name": row.get::<String, _>("tenant_name"),
        }
    })))
}

/// `GET /auth/keys` — the tenant's API keys as metadata only. Never the raw key (invariant: shown
/// once at mint). `key_hash` is returned so a client can name a key to revoke; the hash is not secret.
pub async fn list_keys(
    session: SessionAuth,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT key_hash, kind, label, allowed_origins, created_at::text AS created_at \
         FROM api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(&session.tenant_id)
    .fetch_all(&state.db)
    .await?;

    let keys: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "key_hash": r.get::<String, _>("key_hash"),
                "kind": r.get::<String, _>("kind"),
                "label": r.get::<String, _>("label"),
                "allowed_origins": r.get::<Vec<String>, _>("allowed_origins"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "keys": keys })))
}

#[derive(Deserialize)]
pub struct CreateKeyRequest {
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    allowed_origins: Vec<String>,
}
fn default_kind() -> String {
    "secret".to_string()
}

/// `POST /auth/keys` — a logged-in tenant mints its own key (the self-serve equivalent of the
/// admin-only `mint_key`). Raw key shown once.
pub async fn create_key(
    session: SessionAuth,
    State(state): State<AppState>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    // The last route that created state without a meter. Minting does not multiply LLM spend, so
    // this is not a cost bound — it bounds the **audit and revocation surface**: an unmetered mint
    // lets one session write unbounded rows into `api_keys`, every one of them a live credential
    // someone must later enumerate and revoke.
    //
    // A bucket of its own (`keys:`), not the bare tenant id, and that prefix is the point rather
    // than tidiness. `check` keys on whatever string it is handed, so reusing `tenant_id` would put
    // key-minting in the *same* 60/min window as `/ask` — a tenant provisioning keys would spend
    // their own question budget, and the widget would start 429ing for a reason no log connects to
    // the dashboard tab that caused it.
    rate_limit::check(&state, &format!("keys:{}", session.tenant_id)).await?;

    if req.kind != "secret" && req.kind != "publishable" {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "kind must be 'secret' or 'publishable'",
        ));
    }

    let label = if req.label.is_empty() {
        "default"
    } else {
        &req.label
    };

    let raw = insert_api_key(
        &state.db,
        &session.tenant_id,
        &req.kind,
        label,
        &req.allowed_origins,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "kind": req.kind,
            "allowed_origins": req.allowed_origins,
            "api_key": raw,
            "note": "store this now; it won't be shown again"
        })),
    ))
}

#[derive(Deserialize)]
pub struct UpdateKeyRequest {
    allowed_origins: Vec<String>,
}

/// `PATCH /auth/keys/{key_hash}` — change a key's allowed origins, and nothing else.
///
/// Adding a domain must not mean minting a new key: a `pk_` is printed in public page source and is
/// expected to be stolen, so rotating it to add `www.` buys nothing — the allow-list *is* the
/// containment (invariant 15), and editing it has to be cheap or tenants will reach for a wildcard.
/// `kind` and the hash are deliberately immutable: changing `kind` would silently turn a public key
/// into a secret one, or vice versa, under an unchanged snippet.
///
/// Same isolation boundary as revoke: the `tenant_id` in the WHERE clause, not RLS.
pub async fn update_key(
    session: SessionAuth,
    State(state): State<AppState>,
    Path(key_hash): Path<String>,
    Json(req): Json<UpdateKeyRequest>,
) -> Result<Json<Value>, AppError> {
    // Read the kind first: the rules differ by kind, and validating against the wrong one either
    // rejects a legal secret key or waves through a dead publishable one. Two queries rather than one
    // clever UPDATE, so "not found" and "invalid origin" stay distinct answers instead of collapsing
    // into an ambiguous zero-row result.
    let row = sqlx::query("SELECT kind FROM api_keys WHERE tenant_id = $1 AND key_hash = $2")
        .bind(&session.tenant_id)
        .bind(&key_hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::client(StatusCode::NOT_FOUND, "key not found"))?;
    let kind: String = row.get("kind");

    let allowed_origins = crate::handlers::checked_origins(&kind, &req.allowed_origins)?;

    sqlx::query("UPDATE api_keys SET allowed_origins = $1 WHERE tenant_id = $2 AND key_hash = $3")
        .bind(&allowed_origins)
        .bind(&session.tenant_id)
        .bind(&key_hash)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({
        "key_hash": key_hash,
        "kind": kind,
        "allowed_origins": allowed_origins,
    })))
}

/// `DELETE /auth/keys/{key_hash}` — revoke one of the tenant's keys. The `tenant_id` guard is the
/// isolation boundary here (api_keys carries no RLS), so a session can only revoke its own keys.
pub async fn revoke_key(
    session: SessionAuth,
    State(state): State<AppState>,
    Path(key_hash): Path<String>,
) -> Result<StatusCode, AppError> {
    let res = sqlx::query("DELETE FROM api_keys WHERE tenant_id = $1 AND key_hash = $2")
        .bind(&session.tenant_id)
        .bind(&key_hash)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::client(StatusCode::NOT_FOUND, "key not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_round_trips_and_rejects_wrong_password() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(phc.starts_with("$argon2"));
        assert!(verify_password("correct horse battery staple", &phc));
        assert!(!verify_password("Tr0ub4dour", &phc));
    }

    #[test]
    fn a_garbage_stored_hash_verifies_false_not_panics() {
        assert!(!verify_password("anything", "not-a-phc-string"));
    }

    #[test]
    fn session_tokens_are_prefixed_and_hash_deterministically() {
        let t = auth::generate_session_token();
        assert!(t.starts_with("sess_"));
        assert_eq!(auth::hash_key(&t), auth::hash_key(&t));
        assert_ne!(auth::hash_key(&t), t); // the stored form is not the token
    }

    #[test]
    fn email_validation_accepts_and_rejects() {
        assert!(is_plausible_email("owner@acme.test"));
        assert!(is_plausible_email("a.b+tag@sub.example.com"));
        assert!(!is_plausible_email("no-at-sign"));
        assert!(!is_plausible_email("@acme.test"));
        assert!(!is_plausible_email("owner@localhost")); // no dot in domain
        assert!(!is_plausible_email("owner @acme.test")); // whitespace
    }

    #[test]
    fn slugify_produces_valid_slugs() {
        assert_eq!(slugify("Acme Corp"), "acme-corp");
        assert_eq!(slugify("  Föö & Bar!!  "), "f-bar"); // non-ascii dropped, runs collapsed, trimmed
        assert_eq!(slugify("already-good"), "already-good");
        assert_eq!(slugify("!!!"), ""); // pure punctuation → empty, caller rejects
                                        // Whatever it emits (when non-empty) must satisfy the tenant-slug contract.
        for name in ["Acme Corp", "already-good", "X"] {
            let s = slugify(name);
            assert!(common::key::is_valid_slug(&s), "slug {s:?} from {name:?}");
        }
    }
}
