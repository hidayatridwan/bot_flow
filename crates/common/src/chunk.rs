//! How text becomes chunks — one definition, because it is part of the index recipe.
//!
//! **This lives in `common` for the same reason `EmbeddingClient` does.** A collection may not hold
//! chunks cut two different ways any more than it may hold vectors from two models: the scores are
//! computed against whatever was indexed, so two chunkers produce a collection whose retrieval is
//! quietly wrong with nothing erroring. The worker writes chunks and the retrieval bench must
//! reproduce them **byte for byte**, or the bench measures something other than production and its
//! numbers are worse than no numbers at all.

/// Characters per chunk, and how many of them are shared with the previous chunk.
///
/// **Constants, deliberately not env vars** — the same choice, for the same reason, as `MAX_TOKENS`,
/// `EMBED_BATCH` and the gateway timeouts. Two deployments with different chunk sizes produce
/// collections that cannot be compared or merged, and nothing anywhere errors; that is invariant 6's
/// argument one layer down. Changing either value invalidates every stored vector, which makes it a
/// migration rather than a setting, and a setting is exactly what an env var would make it look like.
pub const CHUNK_SIZE: usize = 800;
pub const CHUNK_OVERLAP: usize = 100;

/// Split text into overlapping chunks by character count (UTF-8 safe — we index over
/// `chars`, never bytes). Overlap preserves context across boundaries so a fact that
/// straddles a cut still appears whole in at least one chunk. Whitespace-only chunks dropped.
pub fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let step = chunk_size.saturating_sub(overlap).max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        let piece: String = chars[start..end].iter().collect();
        let trimmed = piece.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        if end == chars.len() {
            break;
        }
        start += step;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_chunks() {
        // size 4, overlap 1 => step 3
        assert_eq!(chunk_text("abcdefghij", 4, 1), vec!["abcd", "defg", "ghij"]);
    }

    /// The chunker indexes over `chars`, so a multi-byte document must not panic or split a
    /// codepoint. It is the whole reason this is not a byte slice — and the corpus is multilingual.
    #[test]
    fn multibyte_text_is_chunked_by_character_not_byte() {
        // Each character here is 3 bytes. A byte-slicing implementation panics on this input;
        // that is the trap this function exists to avoid, and the corpus is multilingual.
        let chunks = chunk_text("日本語のテキストです", 4, 1);
        // 10 chars, size 4, overlap 1 => step 3: [0..4], [3..7], [6..10].
        assert_eq!(chunks, vec!["日本語の", "のテキス", "ストです"]);
        for c in &chunks {
            assert!(
                c.chars().count() <= 4,
                "chunk exceeded the char budget: {c:?}"
            );
        }
    }

    /// `overlap >= chunk_size` would make `step` zero and loop forever. `saturating_sub().max(1)`
    /// is what stops it, and nothing else tests that path.
    #[test]
    fn overlap_at_or_above_chunk_size_still_terminates() {
        let chunks = chunk_text("abcdefghij", 3, 3);
        assert!(!chunks.is_empty());
        let chunks = chunk_text("abcdefghij", 3, 99);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn whitespace_only_input_yields_nothing() {
        assert!(chunk_text("   \n\t  ", 4, 1).is_empty());
        assert!(chunk_text("", 4, 1).is_empty());
    }
}
