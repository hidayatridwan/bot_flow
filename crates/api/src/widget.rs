//! Serving the embeddable widget from the API.
//!
//! Tenants used to self-host a copy of `widget.js`, so a fix we shipped could never reach a
//! deployed site — the tenant kept running whatever snapshot they pasted. Serving it here makes the
//! embed snippet copy-pasteable *and* lets a fix reach every visitor on the next API restart.
//!
//! That second property is the whole feature, and it lives entirely in the cache header. `no-cache`
//! does **not** mean "do not cache"; it means "revalidate before use". A browser keeps its copy and
//! asks with `If-None-Match`; if unchanged it gets a ~200-byte 304 and reuses what it holds. A long
//! `max-age` here would rebuild self-hosting inside our own API — the stale copy would just be ours
//! instead of the tenant's. See `doc/feature/phase-7-widget.md`, decision D1.

use std::sync::LazyLock;

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// The widget, embedded at compile time. `include_str!` resolves relative to *this* file, so the
/// path climbs from `crates/api/src/` to the repo root. The binary therefore carries its own asset:
/// no `ServeDir`, no filesystem path at runtime, no traversal surface for one file that never
/// changes while the process runs.
const WIDGET_JS: &str = include_str!("../../../widget/widget.js");

/// The type a browser expects for a `<script src>`. `charset=utf-8` because the source is UTF-8 and
/// a future translation of its strings must not silently break the declared encoding.
const CONTENT_TYPE: &str = "application/javascript; charset=utf-8";

/// A **strong** ETag over the served bytes, computed once.
///
/// Strong — no `W/` prefix — because the bytes are byte-identical, not merely equivalent, which is
/// also what lets a CDN placed in front serve its own 304s later. `include_str!` fixes the content
/// at compile time, so this hashes exactly once (on first request) and can never change while the
/// process lives.
static ETAG: LazyLock<String> =
    LazyLock::new(|| format!("\"{}\"", hex::encode(Sha256::digest(WIDGET_JS.as_bytes()))));

/// `GET /widget.js` — public and cache-revalidated.
///
/// Public because it is the code that has not authenticated yet: the file a visitor's browser loads
/// before it holds any credential. It makes no gateway call, so it is not metered — there is no
/// tenant to key a limit on and nothing is spent. It is a bandwidth surface, and only that.
pub async fn serve(headers: HeaderMap) -> Response {
    // The browser echoes back exactly the ETag we handed it. If it still matches, the copy it holds
    // is current — answer 304 and let it reuse what it has. That reuse is the entire caching win.
    if if_none_match_matches(&headers, &ETAG) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, ETAG.as_str()),
                (header::CACHE_CONTROL, "no-cache"),
            ],
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, CONTENT_TYPE),
            // "revalidate before use", not "never cache". See the module doc and D1.
            (header::CACHE_CONTROL, "no-cache"),
            (header::ETAG, ETAG.as_str()),
            // We serve a declared type; forbid the browser from sniffing a different one.
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        WIDGET_JS,
    )
        .into_response()
}

/// RFC 7232 `If-None-Match`: `*` matches anything, otherwise a comma-separated list of entity tags.
///
/// We hold exactly one strong tag, so this is a membership test against it. A `W/` prefix a proxy
/// might have added is stripped before comparing: we always send a strong tag, so a weak echo of it
/// still names the same bytes and should still 304 rather than force a pointless 200.
fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_NONE_MATCH) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let value = value.trim();
    if value == "*" {
        return true;
    }
    value
        .split(',')
        .any(|candidate| candidate.trim().trim_start_matches("W/") == etag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    fn with_inm(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::IF_NONE_MATCH, value.parse().unwrap());
        h
    }

    async fn body_of(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn etag_is_strong_and_quoted() {
        // A leading `W/` would make it weak, which a CDN cannot use to serve its own 304s (D1).
        assert!(ETAG.starts_with('"'), "strong tags open with a quote");
        assert!(!ETAG.starts_with("W/"), "the tag must not be weak");
        assert!(ETAG.ends_with('"'));
    }

    #[tokio::test]
    async fn a_fresh_request_gets_the_widget_with_caching_headers() {
        let resp = serve(HeaderMap::new()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_TYPE], CONTENT_TYPE);
        assert_eq!(h[header::CACHE_CONTROL], "no-cache");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::ETAG], ETAG.as_str());
        assert!(body_of(resp).await.contains("ChatWidget"));
    }

    #[tokio::test]
    async fn a_matching_etag_gets_a_304_with_no_body() {
        let resp = serve(with_inm(&ETAG)).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        // The 304 must still carry the tag, so the browser keeps revalidating against it.
        assert_eq!(resp.headers()[header::ETAG], ETAG.as_str());
        assert!(body_of(resp).await.is_empty(), "304 sends no body");
    }

    #[tokio::test]
    async fn a_stale_etag_gets_the_full_body() {
        // What a browser holding an *old* build sends: a tag that no longer matches. It must get the
        // new bytes, or a fix never lands — the exact failure this whole phase exists to prevent.
        let resp = serve(with_inm(
            "\"0000000000000000000000000000000000000000000000000000000000000000\"",
        ))
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_of(resp).await.contains("ChatWidget"));
    }

    #[test]
    fn if_none_match_parsing() {
        let tag = "\"abc\"";
        assert!(if_none_match_matches(&with_inm("*"), tag), "star matches");
        assert!(if_none_match_matches(&with_inm("\"abc\""), tag));
        assert!(
            if_none_match_matches(&with_inm("W/\"abc\""), tag),
            "a weak echo of our strong tag still names the same bytes"
        );
        assert!(
            if_none_match_matches(&with_inm("\"x\", \"abc\""), tag),
            "a tag anywhere in the list matches"
        );
        assert!(!if_none_match_matches(&with_inm("\"x\""), tag));
        assert!(
            !if_none_match_matches(&HeaderMap::new(), tag),
            "absent header"
        );
    }
}
