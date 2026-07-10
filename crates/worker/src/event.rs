//! Parsing MinIO's S3-compatible `ObjectCreated` notifications.
//!
//! We don't own this schema, so everything here is defensive: unknown fields are ignored, and the
//! two documented traps are handled explicitly.
//!
//! Pure functions over strings — testable without MinIO or RabbitMQ.

use anyhow::{bail, Context};
use percent_encoding::percent_decode_str;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Notification {
    #[serde(rename = "EventName")]
    pub event_name: String,
    #[serde(rename = "Records", default)]
    pub records: Vec<Record>,
}

#[derive(Debug, Deserialize)]
pub struct Record {
    pub s3: S3Entity,
}

#[derive(Debug, Deserialize)]
pub struct S3Entity {
    pub object: S3Object,
}

#[derive(Debug, Deserialize)]
pub struct S3Object {
    /// TRAP: percent-encoded. `tenants%2Facme%2F…`, and a space arrives as `%20` or `+`.
    pub key: String,
    #[serde(default)]
    pub size: i64,
    #[serde(rename = "eTag", default)]
    pub etag: String,
}

/// What the worker actually needs from a notification.
#[derive(Debug, PartialEq)]
pub struct UploadedObject {
    pub tenant_id: String,
    pub document_id: uuid::Uuid,
    /// Percent-decoded. Carried through rather than rebuilt, so the bytes we fetch are exactly
    /// the bytes MinIO announced — extension and all.
    pub object_key: String,
    pub size: i64,
    pub etag: String,
}

/// Decode a raw AMQP body into the object it announces.
///
/// Returns `Ok(None)` for events we don't care about (deletes, etc.) — those are acked and dropped.
/// Returns `Err` only for messages that are structurally broken; the caller acks those too, since
/// no amount of retrying will make an unparseable key parse.
pub fn parse(body: &[u8]) -> anyhow::Result<Option<UploadedObject>> {
    let n: Notification = serde_json::from_slice(body).context("event is not valid JSON")?;

    // Both `s3:ObjectCreated:Put` and `s3:ObjectCreated:CompleteMultipartUpload` land here. A
    // large file uploaded as S3 multipart NEVER fires `:Put`, so matching on the prefix (rather
    // than the exact string) is what keeps big uploads from being silently ignored.
    if !n.event_name.starts_with("s3:ObjectCreated:") {
        return Ok(None);
    }

    let record = n
        .records
        .into_iter()
        .next()
        .context("ObjectCreated event carried no records")?;

    // Use `Records[].s3.object.key`, NOT the top-level `Key` — the latter is bucket-prefixed.
    let raw_key = record.s3.object.key;
    let decoded = percent_decode_str(&raw_key)
        .decode_utf8()
        .context("object key is not valid UTF-8 after percent-decoding")?;

    let Some((tenant_id, document_id)) = common::key::parse_key(&decoded) else {
        bail!("object key does not match the expected layout: {decoded}");
    };

    Ok(Some(UploadedObject {
        tenant_id,
        document_id,
        object_key: decoded.into_owned(),
        size: record.s3.object.size,
        // MinIO quotes eTags, matching S3. Strip so it compares equal to what a PUT returns.
        etag: record.s3.object.etag.trim_matches('"').to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "7045945d-3a0e-4b69-9749-326871ef7516";

    fn body(event: &str, key: &str) -> Vec<u8> {
        format!(
            r#"{{"EventName":"{event}","Key":"documents/{key}",
                "Records":[{{"eventTime":"2026-07-09T12:00:00Z",
                  "s3":{{"bucket":{{"name":"documents"}},
                         "object":{{"key":"{key}","size":148213,"eTag":"\"9b2cf\""}}}}}}]}}"#
        )
        .into_bytes()
    }

    /// The trap: MinIO percent-encodes the key. A naive `split('/')` on the raw value finds
    /// nothing, because every separator arrives as `%2F`.
    #[test]
    fn percent_encoded_key_is_decoded_before_parsing() {
        let encoded = format!("tenants%2Facme%2Fdocuments%2F{DOC}%2Foriginal.pdf");
        let got = parse(&body("s3:ObjectCreated:Put", &encoded))
            .unwrap()
            .unwrap();
        assert_eq!(got.tenant_id, "acme");
        assert_eq!(got.document_id.to_string(), DOC);
        assert_eq!(got.size, 148213);
        assert_eq!(got.etag, "9b2cf", "quotes must be stripped");
    }

    #[test]
    fn plain_key_also_parses() {
        let got = parse(&body(
            "s3:ObjectCreated:Put",
            &format!("tenants/acme/documents/{DOC}/original.pdf"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(got.tenant_id, "acme");
    }

    /// A >64MB file uploaded as S3 multipart fires this and never `:Put`. Forgetting it means
    /// large uploads are silently never ingested.
    #[test]
    fn multipart_completion_is_an_object_created_event() {
        let key = format!("tenants%2Facme%2Fdocuments%2F{DOC}%2Foriginal.pdf");
        assert!(parse(&body("s3:ObjectCreated:CompleteMultipartUpload", &key))
            .unwrap()
            .is_some());
    }

    #[test]
    fn non_creation_events_are_ignored_not_errors() {
        let key = format!("tenants%2Facme%2Fdocuments%2F{DOC}%2Foriginal.pdf");
        assert!(parse(&body("s3:ObjectRemoved:Delete", &key)).unwrap().is_none());
    }

    #[test]
    fn malformed_key_is_an_error_so_the_caller_can_ack_and_log() {
        assert!(parse(&body("s3:ObjectCreated:Put", "nonsense/path")).is_err());
        assert!(parse(b"not json").is_err());
    }

    /// A traversal slug must not survive decoding into a usable tenant id.
    #[test]
    fn traversal_in_the_encoded_key_is_rejected() {
        let key = "tenants%2Fa%2F..%2Fb%2Fdocuments%2Fx%2Foriginal.pdf";
        assert!(parse(&body("s3:ObjectCreated:Put", key)).is_err());
    }
}
