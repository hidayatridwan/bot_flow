//! A sparse (lexical) vector for each chunk — **written from phase 10, queried from 10b**.
//!
//! **Why write something nothing reads.** Adding a vector to existing points is not an update, it is
//! a re-index: every point must be rewritten, and that is the expensive, irreversible half of this
//! phase. Writing the sparse leg now means hybrid search later costs a query change and a flag
//! instead of a second migration. And splitting write from query keeps one variable per measurement
//! — phase 10's retrieval delta is attributable to the chunker alone, 10b's to fusion alone.
//!
//! **Why term frequency and not SPLADE.** A learned sparse model means an ONNX runtime in the worker
//! or a new service; Qdrant's own BM25/miniCOIL inference is not available in a plain self-hosted
//! `qdrant/qdrant` container. What *is* free is a term-frequency vector plus `Modifier::Idf` on the
//! collection, which makes **Qdrant compute IDF server-side at query time**. That last part is what
//! makes the cheap option correct rather than merely cheap: a client-side IDF would need corpus
//! statistics that change on every ingest, and would drift silently as a tenant's corpus grew.
//!
//! **The honest limit.** This is a bag of lowercased word-ish tokens with no stemming and no
//! language-specific handling. It will help most where dense retrieval is weakest — exact codes,
//! identifiers, product names, `INV-2024-00317` — and least on paraphrase, which is what the dense
//! leg is already good at. That complementarity is the entire argument for hybrid; it is not a
//! second, better retriever.

use std::collections::HashMap;

/// The named sparse vector. Named because a collection may hold several, and the dense vector stays
/// unnamed (the default) so every existing query keeps working untouched.
pub const SPARSE_VECTOR: &str = "lexical";

/// Tokens shorter than this carry no lexical signal worth an index entry.
const MIN_TOKEN_CHARS: usize = 2;

/// Lowercase, split on anything that is not alphanumeric, and count.
///
/// Deliberately **not** UTF-8 naive: `char::is_alphanumeric` keeps CJK and accented letters, which a
/// simple `is_ascii_alphanumeric` would silently discard — and this corpus is multilingual by design
/// (invariant 5). A tokenizer that drops every Indonesian or Japanese token would produce an empty
/// sparse vector for those documents and nothing would report it.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TOKEN_CHARS)
        .map(|t| t.to_lowercase())
        .collect()
}

/// A sparse vector as Qdrant wants it: parallel `(indices, values)` arrays.
///
/// The index is a hash of the token, so there is no vocabulary to build, persist, or keep in step
/// between the worker and the api — the same drift argument that puts the chunker and the embedding
/// client in this crate. Collisions are possible and harmless at this scale: two unrelated terms
/// sharing a dimension slightly blurs one lexical match, it does not corrupt anything.
pub fn encode(text: &str) -> (Vec<u32>, Vec<f32>) {
    let mut counts: HashMap<u32, f32> = HashMap::new();
    for token in tokenize(text) {
        *counts.entry(token_id(&token)).or_insert(0.0) += 1.0;
    }
    // Sorted so the same text always produces byte-identical output — a re-index must overwrite a
    // point with the same content, not merely an equivalent one.
    let mut pairs: Vec<(u32, f32)> = counts.into_iter().collect();
    pairs.sort_by_key(|(i, _)| *i);
    pairs.into_iter().unzip()
}

/// FNV-1a. Stable across processes and releases, which a `DefaultHasher` explicitly is not — and an
/// id that changed between builds would silently orphan every sparse dimension already written.
fn token_id(token: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in token.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Qdrant sparse indices are u32; keep the top bit clear so the value is comfortably in range
    // for any client that treats it as signed.
    hash & 0x7fff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizing_keeps_non_ascii_words() {
        // A tokenizer built on is_ascii_alphanumeric returns nothing for either of these, and the
        // sparse leg would be silently empty for every non-English document.
        assert!(tokenize("pengiriman ke Indonesia").contains(&"pengiriman".to_string()));
        assert!(!tokenize("日本語のテキスト").is_empty());
        assert!(tokenize("café Ø hello").contains(&"café".to_string()));
    }

    #[test]
    fn single_characters_and_punctuation_are_dropped() {
        assert_eq!(tokenize("a b, c! dd"), vec!["dd".to_string()]);
    }

    #[test]
    fn encoding_is_deterministic_and_sorted() {
        let (i1, v1) = encode("refunds within 30 days of delivery");
        let (i2, v2) = encode("refunds within 30 days of delivery");
        assert_eq!(i1, i2, "the same text must encode identically across calls");
        assert_eq!(v1, v2);
        assert!(
            i1.windows(2).all(|w| w[0] < w[1]),
            "indices must be sorted and unique"
        );
        assert_eq!(i1.len(), v1.len());
    }

    #[test]
    fn repeated_terms_raise_their_weight() {
        let (indices, values) = encode("refund refund refund shipping");
        let refund = token_id("refund");
        let shipping = token_id("shipping");
        let at = |id: u32| values[indices.iter().position(|i| *i == id).unwrap()];
        assert_eq!(at(refund), 3.0);
        assert_eq!(at(shipping), 1.0);
    }

    #[test]
    fn empty_text_encodes_to_an_empty_vector() {
        let (indices, values) = encode("   ,,, !  ");
        assert!(indices.is_empty());
        assert!(values.is_empty());
    }

    /// The ids must not move between builds. A `DefaultHasher` is explicitly not stable across
    /// releases, and a changed id orphans every sparse dimension already written — silently, since
    /// a sparse miss just means a slightly worse score.
    /// These are recorded values, not derived ones — that is the point. If this test fails, the
    /// hash changed, and every sparse dimension already written to `documents_v2` now refers to a
    /// different term. Recovering means a re-index, so the failure must be loud here rather than
    /// quiet in retrieval.
    #[test]
    fn token_ids_are_stable_constants() {
        assert_eq!(token_id("refund"), 854_262_879);
        assert_eq!(token_id("pengiriman"), 1_423_045_411);
        assert!(token_id("anything") <= 0x7fff_ffff, "must stay in range");
    }
}
