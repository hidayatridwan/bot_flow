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

/// The prefix that marks a bearer token as a session rather than an API key.
///
/// This is *not* decoration. `Actor` dispatches on it to decide which table to resolve the token
/// against — `sessions` or `api_keys`. Drop the prefix and every session token is looked up in
/// `api_keys`, misses, and 401s the entire dashboard. See invariant 17.
pub const SESSION_PREFIX: &str = "sess_";

/// Generate a fresh session token. The `sess_` prefix keeps it from ever being confused with
/// an `sk_`/`pk_` API key; same entropy, and — like a key — only its hash reaches the database.
pub fn generate_session_token() -> String {
    format!(
        "{SESSION_PREFIX}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Which credential family a bearer token belongs to, decided by its prefix alone — before any
/// database round-trip. Keeping this pure is what lets the dispatch rule be unit-tested.
fn is_session_token(token: &str) -> bool {
    token.starts_with(SESSION_PREFIX)
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

/// What kind of credential a request arrived with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Secret,
    Publishable,
    Session,
}

/// The union of the two principals, for routes that *both* may legitimately reach.
///
/// This does not conflate `AuthTenant` (the machine: a tenant's server, the widget) with
/// `SessionAuth` (the human: the dashboard). Both remain intact and independently usable. `Actor`
/// exists because the document-management routes are reachable by a tenant's own server *and* by a
/// logged-in human, and there is no credential the dashboard could present other than its session —
/// the one-time `sk_` is unrecoverable by design (invariant 22).
///
/// It yields a `tenant_id` like both of its delegates, so it drives `db::tenant_tx()` unchanged:
/// RLS is keyed on the string, not on how the string was obtained.
pub struct Actor {
    pub tenant_id: String,
    pub kind: ActorKind,
}

impl FromRequestParts<AppState> for Actor {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Peek at the prefix to choose a delegate — one table, one query. A malformed or absent
        // header falls through to AuthTenant, which words the 401 for it.
        let is_session = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .is_some_and(is_session_token);

        if is_session {
            let session = SessionAuth::from_request_parts(parts, state).await?;
            return Ok(Actor {
                tenant_id: session.tenant_id,
                kind: ActorKind::Session,
            });
        }

        let tenant = AuthTenant::from_request_parts(parts, state).await?;
        let kind = match tenant.kind.as_str() {
            "secret" => ActorKind::Secret,
            "publishable" => ActorKind::Publishable,
            // A DB CHECK constrains kind to those two. An unknown value means it was bypassed —
            // fail closed rather than guess which side of the management gate it belongs on. Same
            // message as an unknown key, so this is not an oracle either.
            _ => {
                return Err(AppError::client(
                    StatusCode::UNAUTHORIZED,
                    "invalid API key",
                ))
            }
        };

        Ok(Actor {
            tenant_id: tenant.tenant_id,
            kind,
        })
    }
}

impl Actor {
    /// Management routes: the tenant's own server (`sk_`) or a logged-in human (`sess_`).
    ///
    /// A publishable key is printed in public page source and is *expected* to be stolen
    /// (invariant 15) — the containment for that theft is that it may only ask questions. The gate
    /// widened for sessions. It must never widen for `pk_`.
    pub fn require_management(&self) -> Result<(), AppError> {
        match self.kind {
            ActorKind::Secret | ActorKind::Session => Ok(()),
            ActorKind::Publishable => Err(AppError::client(
                StatusCode::FORBIDDEN,
                "this endpoint requires a secret key or a session",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(kind: ActorKind) -> Actor {
        Actor {
            tenant_id: "acme".to_string(),
            kind,
        }
    }

    fn status_of(err: AppError) -> StatusCode {
        match err {
            AppError::Client(status, _) => status,
            AppError::Internal(e) => panic!("expected a client error, got internal: {e:#}"),
        }
    }

    #[test]
    fn management_gate_admits_secret_and_session_and_rejects_publishable() {
        assert!(actor(ActorKind::Secret).require_management().is_ok());
        assert!(actor(ActorKind::Session).require_management().is_ok());

        // Invariant 15: a pk_ is public, expected to be stolen, and may only ask questions.
        // Widening this gate to admit it would delete the containment the whole design rests on.
        let err = actor(ActorKind::Publishable)
            .require_management()
            .expect_err("a publishable key must never reach a management route");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    #[test]
    fn secret_gate_is_unchanged_and_still_admits_only_secret() {
        let secret = AuthTenant {
            tenant_id: "acme".to_string(),
            kind: "secret".to_string(),
        };
        assert!(secret.require_secret().is_ok());

        // /ingest and the deprecated multipart upload stay key-only: a session must NOT reach them.
        for kind in ["publishable", "session", ""] {
            let tenant = AuthTenant {
                tenant_id: "acme".to_string(),
                kind: kind.to_string(),
            };
            let err = tenant
                .require_secret()
                .expect_err("require_secret is an allow-list of exactly one kind");
            assert_eq!(status_of(err), StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn session_tokens_are_told_from_api_keys_by_prefix_alone() {
        assert!(is_session_token(&generate_session_token()));
        assert!(!is_session_token(&generate_key("secret")));
        assert!(!is_session_token(&generate_key("publishable")));

        // The dispatch rule, spelled out: this prefix is what sends a token to `sessions` rather
        // than `api_keys`. Renaming it silently 401s every dashboard request.
        assert!(is_session_token("sess_abc"));
        assert!(!is_session_token("sk_abc"));
        assert!(!is_session_token("pk_abc"));
        assert!(!is_session_token(""));
    }
}
