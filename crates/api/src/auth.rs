use crate::error::AppError;
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::{header, request::Parts, StatusCode};
use sha2::{Digest, Sha256};
use sqlx::Row;

pub struct AuthTenant {
    pub tenant_id: String,
    pub kind: String,
}

impl FromRequestParts<AppState> for AuthTenant {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 1. Read the Authorization header as a string.
        let auth = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AppError::client(StatusCode::UNAUTHORIZED, "missing authorization header")
            })?;

        // 2. Require the "Bearer <key>" scheme, then trim stray whitespace.
        let token = auth
            .strip_prefix("Bearer ")
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::UNAUTHORIZED,
                    "expected 'Bearer <key>' authorization",
                )
            })?
            .trim();

        // 3. Hash the raw key (shared helper).
        let hash = hash_key(token);

        // 4. Resolve the key: tenant + kind + (for publishable) its allowed origins.
        let row = sqlx::query(
            "SELECT tenant_id, kind, allowed_origins FROM api_keys WHERE key_hash = $1",
        )
        .bind(&hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::client(StatusCode::UNAUTHORIZED, "invalid API key"))?;

        let tenant_id: String = row.get("tenant_id");
        let kind: String = row.get("kind");
        let allowed_origins: Vec<String> = row.get("allowed_origins");

        // 5. Publishable keys are browser-facing: only valid from an allowed Origin.
        if kind == "publishable" {
            let origin = parts
                .headers
                .get(header::ORIGIN)
                .and_then(|v| v.to_str().ok());
            let allowed = matches!(origin, Some(o) if allowed_origins.iter().any(|a| a == o));
            if !allowed {
                return Err(AppError::client(
                    StatusCode::FORBIDDEN,
                    "origin not allowed for this key",
                ));
            }
        }

        Ok(AuthTenant { tenant_id, kind })
    }
}

/// SHA-256 hex of a raw key — the form stored in api_keys.key_hash.
pub fn hash_key(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Generate a fresh random API key. `sk_` = secret, `pk_` = publishable.
/// Two v4 UUIDs (~244 bits) of entropy; we only ever store its hash.
pub fn generate_key(kind: &str) -> String {
    let prefix = if kind == "publishable" { "pk" } else { "sk" };
    format!(
        "{prefix}_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Generate a fresh session token. The `sess_` prefix keeps it from ever being confused with
/// an `sk_`/`pk_` API key; same entropy, and — like a key — only its hash reaches the database.
pub fn generate_session_token() -> String {
    format!(
        "sess_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Guards admin endpoints: requires `Authorization: Bearer <ADMIN_API_KEY>`.
pub struct AdminAuth;

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or_else(|| {
                AppError::client(StatusCode::UNAUTHORIZED, "missing authorization header")
            })?;

        if token == state.admin_api_key {
            Ok(AdminAuth)
        } else {
            Err(AppError::client(
                StatusCode::UNAUTHORIZED,
                "invalid admin key",
            ))
        }
    }
}

/// Authenticates a logged-in human via a session token, for the `/auth/*` dashboard routes.
/// Resolves the token to its account and tenant. Because it yields a `tenant_id`, a session-authed
/// handler can drive `db::tenant_tx()` exactly like `AuthTenant` — RLS applies unchanged.
pub struct SessionAuth {
    pub account_id: uuid::Uuid,
    pub tenant_id: String,
    /// The hash of the presented token, so `/auth/logout` can delete *this* session (not all of
    /// the account's) without re-reading the header.
    pub token_hash: String,
}

impl FromRequestParts<AppState> for SessionAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or_else(|| {
                AppError::client(StatusCode::UNAUTHORIZED, "missing authorization header")
            })?;

        let hash = hash_key(token);

        // The `expires_at > now()` guard is done in SQL so an expired token and an unknown token
        // are indistinguishable here — both yield no row, hence the same 401. No existence oracle.
        let row = sqlx::query(
            "SELECT account_id, tenant_id FROM sessions WHERE token_hash = $1 AND expires_at > now()",
        )
        .bind(&hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| {
            AppError::client(StatusCode::UNAUTHORIZED, "invalid or expired session")
        })?;

        Ok(SessionAuth {
            account_id: row.get("account_id"),
            tenant_id: row.get("tenant_id"),
            token_hash: hash,
        })
    }
}

impl AuthTenant {
    /// Reject publishable (browser) keys on management/ingest endpoints.
    pub fn require_secret(&self) -> Result<(), AppError> {
        if self.kind == "secret" {
            Ok(())
        } else {
            Err(AppError::client(
                StatusCode::FORBIDDEN,
                "this endpoint requires a secret key",
            ))
        }
    }
}
