//! Object key construction and parsing.
//!
//! The key is the authorisation boundary of a presigned URL — it is the *only* thing binding an
//! upload to a tenant. Everything here is a pure function over strings so it can be tested
//! exhaustively without MinIO or Postgres.

/// `tenants/{tenant_id}/documents/{document_id}/original.{ext}`
///
/// Tenant first so MinIO policies, lifecycle rules and offboarding can all work on a prefix.
/// A directory per document leaves room for derived artifacts (`extracted.txt`) later.
pub fn object_key(tenant_id: &str, document_id: &uuid::Uuid, ext: &str) -> String {
    format!("tenants/{tenant_id}/documents/{document_id}/original.{ext}")
}

/// The extensions `sidecar/parser.py` can actually read. A presigned PUT cannot inspect content,
/// so this is the last moment we are able to refuse a file.
pub const ALLOWED_EXTENSIONS: [&str; 3] = ["pdf", "txt", "md"];

/// Lowercased extension of `filename`, if the parser supports it.
pub fn extension_of(filename: &str) -> Option<String> {
    let ext = std::path::Path::new(filename)
        .extension()?
        .to_str()?
        .to_lowercase();
    ALLOWED_EXTENSIONS.contains(&ext.as_str()).then_some(ext)
}

/// The content type MinIO should report. Derived from the extension we validated, never taken
/// from the client — a client-supplied type would be a lie we then stored.
pub fn content_type_for(ext: &str) -> &'static str {
    match ext {
        "pdf" => "application/pdf",
        "md" => "text/markdown",
        _ => "text/plain",
    }
}

/// Recover `(tenant_id, document_id)` from a key. The worker uses this to know which row an
/// `ObjectCreated` event refers to, so a malformed key must be rejected rather than guessed at.
pub fn parse_key(key: &str) -> Option<(String, uuid::Uuid)> {
    let mut parts = key.split('/');
    if parts.next()? != "tenants" {
        return None;
    }
    let tenant_id = parts.next()?;
    if parts.next()? != "documents" {
        return None;
    }
    let document_id = parts.next()?;
    // Reject anything deeper or shallower than the exact shape we mint.
    if !parts.next()?.starts_with("original.") || parts.next().is_some() {
        return None;
    }
    if !is_valid_slug(tenant_id) {
        return None;
    }
    Some((tenant_id.to_string(), document_id.parse().ok()?))
}

/// Mirrors the `tenants_id_slug` CHECK constraint. A slug containing `/` or `..` would let a
/// tenant's object key escape its own prefix.
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> uuid::Uuid {
        uuid::Uuid::parse_str("7045945d-3a0e-4b69-9749-326871ef7516").unwrap()
    }

    #[test]
    fn key_round_trips() {
        let key = object_key("acme", &doc(), "pdf");
        assert_eq!(
            key,
            "tenants/acme/documents/7045945d-3a0e-4b69-9749-326871ef7516/original.pdf"
        );
        assert_eq!(parse_key(&key), Some(("acme".to_string(), doc())));
    }

    #[test]
    fn malformed_keys_are_rejected_not_guessed() {
        for bad in [
            "",
            "tenants/acme",
            "tenants/acme/documents",
            "documents/acme/documents/x/original.pdf", // wrong root
            "tenants/acme/docs/7045945d-3a0e-4b69-9749-326871ef7516/original.pdf", // wrong segment
            "tenants/acme/documents/not-a-uuid/original.pdf",
            "tenants/acme/documents/7045945d-3a0e-4b69-9749-326871ef7516/other.pdf",
            // Deeper than the shape we mint — must not be silently accepted.
            "tenants/acme/documents/7045945d-3a0e-4b69-9749-326871ef7516/original.pdf/x",
        ] {
            assert_eq!(parse_key(bad), None, "should reject: {bad}");
        }
    }

    /// The whole reason `is_valid_slug` exists: a tenant id with a slash escapes its prefix.
    #[test]
    fn path_traversal_slugs_are_rejected() {
        for bad in ["a/../b", "..", "a/b", "ACME", "-acme", "a_b", "a b"] {
            assert!(!is_valid_slug(bad), "should reject slug: {bad}");
        }
        for good in ["acme", "a", "globex-inc", "tenant123", "0abc"] {
            assert!(is_valid_slug(good), "should accept slug: {good}");
        }
    }

    /// A key built from a traversal slug must not parse back as a legitimate tenant.
    #[test]
    fn traversal_key_does_not_parse() {
        assert_eq!(parse_key("tenants/a/../b/documents/x/original.pdf"), None);
    }

    #[test]
    fn only_parser_supported_extensions_pass() {
        assert_eq!(extension_of("cv.pdf").as_deref(), Some("pdf"));
        assert_eq!(extension_of("NOTES.MD").as_deref(), Some("md")); // case-insensitive
        assert_eq!(extension_of("a.b.txt").as_deref(), Some("txt"));
        assert_eq!(extension_of("resume.docx"), None); // parser exits 3 on this
        assert_eq!(extension_of("noext"), None);
    }

    /// `Path::extension` has rules that a hand-rolled `split('.').last()` does not, and the web
    /// client mirrors this function in TypeScript. Pin the cases where the two would disagree —
    /// drift shows the user a client-side "valid" that this function then 400s.
    #[test]
    fn dotfiles_have_no_extension() {
        // A leading dot with no other dot is a *stem*, not an extension. `split('.').last()`
        // would say "pdf" and wave it through.
        assert_eq!(extension_of(".pdf"), None);
        assert_eq!(extension_of(".txt"), None);
        // ...but a second dot makes the last segment a real extension again.
        assert_eq!(extension_of("..pdf").as_deref(), Some("pdf"));
        assert_eq!(extension_of(".hidden.md").as_deref(), Some("md"));
        // A trailing dot is an empty extension, not an inherited one.
        assert_eq!(extension_of("file."), None);
        assert_eq!(extension_of(".."), None);
        // file_name() first: a client may send a path, and only the last segment counts.
        assert_eq!(extension_of("dir/file.pdf").as_deref(), Some("pdf"));
        assert_eq!(extension_of("a.pdf/b.txt").as_deref(), Some("txt"));
    }

    #[test]
    fn content_type_follows_the_extension_not_the_client() {
        assert_eq!(content_type_for("pdf"), "application/pdf");
        assert_eq!(content_type_for("md"), "text/markdown");
        assert_eq!(content_type_for("txt"), "text/plain");
    }
}
