//! Why a document failed, in terms the tenant can act on.
//!
//! `documents.error` holds raw parser stderr and our own "worker presumed dead" post-mortem, so
//! invariant 16 forbids showing it to anyone. The consequence, before this module existed, was
//! that the UI could only say "failed" and name *both* possible causes — leaving the tenant unable
//! to tell whether to re-upload a broken PDF or wait for us to recover.
//!
//! This enum is the safe half of that column: a closed set the API may expose alongside the raw
//! text it may not. The variants are cut by **what the tenant should do** — re-upload, or wait —
//! not by where in the stack the error arose, which is why everything internal collapses into a
//! single [`FailureReason::SystemError`] on purpose. A tenant cannot act on the difference between
//! Qdrant being down and MinIO being down, and naming it would leak our topology for no benefit.
//!
//! The re-upload-or-wait split is rendered in `web/src/lib/features/documents/status.ts` and lives
//! only there. Mirroring it back here as a helper would be a second source of truth for a decision
//! this crate never makes.
//!
//! Closed rather than free-form for three reasons: the DB `CHECK` in migration 0015 can enforce
//! it, the UI can switch exhaustively over it, and if a reason ever reaches a metric label it
//! already satisfies invariant 30's "closed enum, never a runtime string" rule.

use crate::parser::{SidecarExit, EXIT_UNREADABLE, EXIT_UNSUPPORTED_TYPE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// The file is the right type but its contents could not be read — encrypted, truncated or
    /// malformed. Re-uploading a good copy is the fix.
    UnreadableFile,
    /// We do not support this file type. Converting it is the fix.
    UnsupportedType,
    /// The object exceeded `MAX_UPLOAD_BYTES`. Its bytes were deleted.
    TooLarge,
    /// Anything on our side of the line: the sidecar crashed, an embedding call failed, a store
    /// was unreachable, or a worker died holding the lease. The tenant's file may be perfectly
    /// good, so they are told to wait, not to re-upload.
    SystemError,
}

impl FailureReason {
    /// The wire and database representation. Pinned by the `CHECK` in migration 0015 and by the
    /// TypeScript union in `web/src/lib/types/documents.ts` — all three must agree.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnreadableFile => "unreadable_file",
            Self::UnsupportedType => "unsupported_type",
            Self::TooLarge => "too_large",
            Self::SystemError => "system_error",
        }
    }
}

/// Classify an indexing failure into what the tenant should be told.
///
/// Deliberately conservative: anything not *provably* the document's fault is [`SystemError`].
/// Getting this backwards is worse than not classifying at all — telling someone their file is
/// damaged when our sidecar is down sends them to re-upload a good file, repeatedly, while the
/// real fault goes unreported. An unclassified failure is a wait; only a recognised one is a
/// re-upload.
///
/// [`SystemError`]: FailureReason::SystemError
pub fn classify(e: &anyhow::Error) -> FailureReason {
    // Only the sidecar can implicate the document, and only via the two exit codes it promises.
    // Codes 1 and 2 are its own crash and its own usage error — both ours (`sidecar/parser.py`).
    match e.downcast_ref::<SidecarExit>().and_then(|s| s.code) {
        Some(EXIT_UNSUPPORTED_TYPE) => FailureReason::UnsupportedType,
        Some(EXIT_UNREADABLE) => FailureReason::UnreadableFile,
        _ => FailureReason::SystemError,
    }
    // Note what is *not* here: a fatal `EmbedError` (a 413, or a context-length rejection) reads
    // like "their document was too big", but the chunk sizes we send are ours — `CHUNK_SIZE`, not
    // anything the tenant chose. So it is our bug, and it stays `SystemError`.
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn exit(code: i32) -> anyhow::Error {
        SidecarExit {
            code: Some(code),
            stderr: "boom".into(),
        }
        .into()
    }

    #[test]
    fn sidecar_codes_map_to_the_tenant_facing_reason() {
        assert_eq!(classify(&exit(3)), FailureReason::UnsupportedType);
        assert_eq!(classify(&exit(4)), FailureReason::UnreadableFile);
    }

    #[test]
    fn our_own_faults_are_never_blamed_on_the_document() {
        // 1 = uncaught traceback, 2 = usage error. Both are the sidecar misbehaving, and a tenant
        // must never be told to re-upload because of either.
        assert_eq!(classify(&exit(1)), FailureReason::SystemError);
        assert_eq!(classify(&exit(2)), FailureReason::SystemError);
        assert_eq!(
            classify(&anyhow::anyhow!("failed to upsert points")),
            FailureReason::SystemError
        );
        assert_eq!(
            classify(
                &SidecarExit {
                    code: None, // killed by a signal
                    stderr: String::new(),
                }
                .into()
            ),
            FailureReason::SystemError
        );
    }

    #[test]
    fn classification_survives_a_context_layer() {
        // The failure travels up through `verify_and_ingest`, and anyone may add `.context()` on
        // the way. anyhow downcasts through context layers, so this holds — but it is the kind of
        // thing that would silently degrade every parser failure to `SystemError` if it did not,
        // and nothing else in the stack would notice. Asserted rather than assumed.
        let wrapped = Err::<(), _>(exit(EXIT_UNREADABLE))
            .context("while parsing document")
            .context("while indexing")
            .unwrap_err();
        assert_eq!(classify(&wrapped), FailureReason::UnreadableFile);
    }

    #[test]
    fn wire_strings_match_the_db_check_and_the_typescript_union() {
        // Changing any of these is a three-file change: here, migration 0015's CHECK, and
        // `web/src/lib/types/documents.ts`. A mismatch fails closed at the DB, loudly.
        assert_eq!(FailureReason::UnreadableFile.as_str(), "unreadable_file");
        assert_eq!(FailureReason::UnsupportedType.as_str(), "unsupported_type");
        assert_eq!(FailureReason::TooLarge.as_str(), "too_large");
        assert_eq!(FailureReason::SystemError.as_str(), "system_error");
    }
}
