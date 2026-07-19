//! Contracts shared between the API and the worker. Kept in one place so the two can never drift
//! out of agreement about the key format, or about how text becomes a vector.
pub mod chunk;
pub mod sparse;

/// The Qdrant collection every tenant's vectors live in, **versioned**.
///
/// The version is the phase-10 migration's rollback, and this system has never had one. Changing
/// the chunker, the embedding model or the payload invalidates every stored vector, and
/// `ensure_collection` early-returns when a collection already exists — so mutating in place is not
/// merely inadvisable, it silently does not happen. A new name means the old collection stays intact
/// and queryable while the new one fills, cutover is this constant, and so is backing out.
///
/// It also makes the "partially re-indexed collection degrades quietly" hazard visible: `_v2` is
/// provably empty until the re-index runs, rather than half-right and silent.
///
/// v1 = 800/100 fixed-window chunks, payload {text, tenant_id, document_id}.
/// v2 = 500/60 boundary-aware chunks, payload additionally {chunk_index, char_start, char_end,
///      created_at}, plus a sparse vector written for phase 10b.
pub const COLLECTION: &str = "documents_v2";
pub mod embedding;
pub mod key;
