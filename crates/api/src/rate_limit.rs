use axum::http::StatusCode;
use redis::AsyncCommands;

use crate::error::AppError;
use crate::state::AppState;

/// Fixed-window per-tenant limiter. Key = tenant + current minute bucket; INCR each request,
/// set the TTL on the first hit so the window self-expires. Over the limit -> 429.
pub async fn check(state: &AppState, tenant_id: &str) -> Result<(), AppError> {
    const WINDOW_SECS: i64 = 60;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let key = format!("ratelimit:{tenant_id}:{}", now / WINDOW_SECS as u64);

    // ConnectionManager is cheap to clone; commands need &mut.
    let mut conn = state.redis.clone();
    let count: u64 = conn.incr(&key, 1).await?;
    if count == 1 {
        // First request in this window — set expiry so the counter cleans itself up.
        let _: () = conn.expire(&key, WINDOW_SECS).await?;
    }

    if count > state.rate_limit_per_minute {
        return Err(AppError::client(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded, slow down",
        ));
    }
    Ok(())
}
