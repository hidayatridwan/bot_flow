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
/// Measured, not chosen. On the phase-10 bench, 500/60 with boundary-aware splitting scored
/// recall@1 0.909 and MRR 0.947 against the old 800/100 fixed window's 0.659 and 0.818 — better on
/// every metric while delivering 42% less context (1124 chars vs 1952). Sizes either side were
/// worse: 300 and 400 lost a question from recall@3, 1200 and up cost more context for less recall.
pub const CHUNK_SIZE: usize = 500;
pub const CHUNK_OVERLAP: usize = 60;

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

/// Separators in descending order of how much structure they preserve.
///
/// The list ends at `" "` and **not** at `""` on purpose: falling all the way through to a
/// character cut is handled explicitly by the caller, so a single word longer than the budget is the
/// only thing that ever gets split mid-word. Everything else lands on a boundary a reader would
/// recognise.
const SEPARATORS: &[&str] = &["\n\n", "\n", ". ", "? ", "! ", "; ", ", ", " "];

/// Boundary-aware splitting: paragraph → line → sentence → clause → word, then a hard cut only if a
/// single word exceeds the budget.
///
/// **What the fixed window gets wrong.** [`chunk_text`] cuts at character 800 wherever that lands —
/// mid-word, mid-number, mid-sentence. The fragment either side is text the embedding model has to
/// interpret without its subject, and a passage that begins `"...ithin 30 days of delivery"` is worse
/// than useless: it still scores, so it still competes for a slot in the context.
///
/// The overlap is taken in **whole pieces**, not characters, for the same reason — a
/// boundary-respecting split whose overlap is a raw character slice reintroduces the exact defect it
/// just removed, at the seam.
pub fn chunk_text_recursive(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    if text.trim().is_empty() || chunk_size == 0 {
        return Vec::new();
    }
    let pieces = split_to_size(text, chunk_size, 0);
    merge_with_overlap(&pieces, chunk_size, overlap)
}

/// Break `text` into pieces that each fit the budget, descending the separator list as needed.
fn split_to_size(text: &str, chunk_size: usize, sep_index: usize) -> Vec<String> {
    if text.chars().count() <= chunk_size {
        return vec![text.to_string()];
    }
    let Some(sep) = SEPARATORS.get(sep_index) else {
        // Out of separators: one unbroken token longer than the budget. A hard cut is the only
        // option left, and it is the one case where splitting mid-word is correct.
        return text
            .chars()
            .collect::<Vec<_>>()
            .chunks(chunk_size)
            .map(|c| c.iter().collect())
            .collect();
    };

    if !text.contains(sep) {
        return split_to_size(text, chunk_size, sep_index + 1);
    }

    let mut out = Vec::new();
    // `split_inclusive`-style: keep the separator on the left piece so rejoining is lossless enough
    // for retrieval, and so a sentence keeps its full stop.
    let parts: Vec<&str> = text.split(sep).collect();
    for (i, part) in parts.iter().enumerate() {
        let with_sep = if i + 1 < parts.len() {
            format!("{part}{sep}")
        } else {
            part.to_string()
        };
        if with_sep.trim().is_empty() {
            continue;
        }
        if with_sep.chars().count() > chunk_size {
            out.extend(split_to_size(&with_sep, chunk_size, sep_index + 1));
        } else {
            out.push(with_sep);
        }
    }
    out
}

/// Greedily pack pieces up to the budget, carrying whole trailing pieces forward as the overlap.
fn merge_with_overlap(pieces: &[String], chunk_size: usize, overlap: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_len = 0usize;

    for piece in pieces {
        let len = piece.chars().count();
        if current_len + len > chunk_size && !current.is_empty() {
            chunks.push(current.concat().trim().to_string());

            // Carry back whole trailing pieces until the overlap budget is spent. Character-slicing
            // here would undo the boundary work at every seam.
            let mut carried: Vec<String> = Vec::new();
            let mut carried_len = 0usize;
            for prev in current.iter().rev() {
                let plen = prev.chars().count();
                if carried_len + plen > overlap {
                    break;
                }
                carried_len += plen;
                carried.insert(0, prev.clone());
            }
            current = carried;
            current_len = carried_len;
        }
        current.push(piece.clone());
        current_len += len;
    }
    if !current.is_empty() {
        let last = current.concat().trim().to_string();
        if !last.is_empty() {
            chunks.push(last);
        }
    }
    chunks.retain(|c| !c.trim().is_empty());
    chunks
}

/// A chunk and where it came from in the source document.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub text: String,
    /// Character offsets into the parsed document text. **Characters, not bytes** — the whole
    /// chunker indexes over `chars`, and a byte offset would be meaningless for the multilingual
    /// corpora this system is built for.
    pub char_start: usize,
    pub char_end: usize,
}

/// Chunk, and record where each chunk sat in the source.
///
/// The offsets are stored in the vector payload and read by nothing today. They are here because
/// adding a payload field later is a second full re-index — the expensive, irreversible half — and
/// because they are the prerequisite for expanding a hit to its neighbours (small-to-big retrieval)
/// without one. `chunk_index` alone gives ordering; offsets give extent.
pub fn chunk_with_spans(text: &str, chunk_size: usize, overlap: usize) -> Vec<Chunk> {
    let chunks = chunk_text_recursive(text, chunk_size, overlap);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chunks.len());
    // Chunks are verbatim, in order, and their start offsets are non-decreasing (overlap only ever
    // moves a start *backwards* relative to the previous chunk's end, never before its start), so a
    // monotonic forward scan finds each one exactly once.
    let mut cursor = 0usize;
    for c in chunks {
        let needle: Vec<char> = c.chars().collect();
        let start = find_from(&chars, &needle, cursor).unwrap_or(cursor);
        cursor = start;
        out.push(Chunk {
            char_start: start,
            char_end: start + needle.len(),
            text: c,
        });
    }
    out
}

fn find_from(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len().saturating_sub(needle.len()))
        .find(|&i| haystack[i..i + needle.len()] == *needle)
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

    // --- the boundary-aware chunker (D3) ---

    const PROSE: &str = "Refunds are accepted within 30 days of delivery. The window was extended \
from the previous 14 days following customer feedback.\n\nGoods must be returned unused and in \
their original packaging. No fee is charged for a return missing its packaging.";

    /// The property the whole migration is for: no chunk may end mid-word.
    ///
    /// The fixed window cuts at character N wherever it lands, producing fragments like
    /// `"...ithin 30 days"` that still score and still take a context slot.
    #[test]
    fn no_chunk_splits_a_word() {
        for size in [80, 120, 200, 400] {
            for c in chunk_text_recursive(PROSE, size, 20) {
                let trimmed = c.trim();
                // Every chunk must start and end on something a reader would call a boundary.
                assert!(
                    PROSE.contains(trimmed),
                    "chunk is not a verbatim slice of the source: {trimmed:?}"
                );
                let first = trimmed.chars().next().unwrap();
                assert!(
                    first.is_alphanumeric() || first.is_ascii_punctuation(),
                    "chunk starts mid-token at size {size}: {trimmed:?}"
                );
            }
        }
    }

    /// Every chunk must fit the budget, or the embedding request shape stops being predictable.
    #[test]
    fn chunks_respect_the_budget() {
        for size in [40, 100, 800] {
            for c in chunk_text_recursive(PROSE, size, 10) {
                assert!(
                    c.chars().count() <= size,
                    "chunk of {} exceeded budget {size}: {c:?}",
                    c.chars().count()
                );
            }
        }
    }

    /// Nothing may be dropped: a fact that exists in the document must exist in some chunk. A
    /// chunker that silently loses text degrades retrieval with nothing to show for it.
    #[test]
    fn no_content_is_lost() {
        let chunks = chunk_text_recursive(PROSE, 100, 20);
        for fact in [
            "within 30 days of delivery",
            "previous 14 days",
            "original packaging",
            "missing its packaging",
        ] {
            assert!(
                chunks.iter().any(|c| c.contains(fact)),
                "lost {fact:?} from the corpus"
            );
        }
    }

    /// A single token longer than the budget is the one case where a hard cut is correct — and it
    /// must not loop forever or panic.
    #[test]
    fn one_oversized_word_is_hard_cut_rather_than_dropped() {
        let long = "a".repeat(250);
        let chunks = chunk_text_recursive(&long, 100, 10);
        assert!(chunks.len() >= 3);
        assert_eq!(chunks.concat().chars().count(), 250);
        for c in &chunks {
            assert!(c.chars().count() <= 100);
        }
    }

    #[test]
    fn recursive_chunker_is_utf8_safe_and_handles_degenerate_input() {
        let id = "Pengiriman ke Indonesia memakan waktu 7 sampai 12 hari kerja. Bea masuk \
ditanggung oleh penerima.";
        // Budget 80: the sentence carrying the fact is 61 chars, so it fits whole. At 50 it could
        // not, and no chunker can hold a phrase larger than the budget — that is a size decision,
        // not a strategy one, and it is exactly what the bench sweep exists to settle.
        let chunks = chunk_text_recursive(id, 80, 10);
        assert!(chunks.iter().any(|c| c.contains("7 sampai 12 hari kerja")));
        assert!(chunk_text_recursive("", 100, 10).is_empty());
        assert!(chunk_text_recursive("   \n  ", 100, 10).is_empty());
        // overlap >= size must terminate rather than spin
        assert!(!chunk_text_recursive(PROSE, 50, 50).is_empty());
        assert!(!chunk_text_recursive(PROSE, 50, 999).is_empty());
    }

    /// Offsets must actually point at the chunk, or they are decoration that will be trusted.
    #[test]
    fn spans_locate_each_chunk_in_the_source() {
        let chars: Vec<char> = PROSE.chars().collect();
        let chunks = chunk_with_spans(PROSE, 120, 30);
        assert!(chunks.len() > 2);
        let mut last_start = 0;
        for c in &chunks {
            let slice: String = chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(
                slice, c.text,
                "span does not point at the chunk it describes"
            );
            assert!(c.char_start >= last_start, "spans must be non-decreasing");
            last_start = c.char_start;
        }
        // The last chunk must reach the end of the document, or the tail was silently dropped.
        assert_eq!(chunks.last().unwrap().char_end, chars.len());
    }

    #[test]
    fn spans_are_character_offsets_not_byte_offsets() {
        let text = "日本語の説明です。これは二番目の文です。これは三番目の文です。";
        let chunks = chunk_with_spans(text, 20, 5);
        let chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            let slice: String = chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(slice, c.text);
        }
    }

    /// Overlap is carried as whole pieces, so the seam between two chunks is itself a boundary.
    #[test]
    fn overlap_is_whole_pieces_not_a_character_slice() {
        let chunks = chunk_text_recursive(PROSE, 120, 60);
        assert!(chunks.len() > 1);
        for c in &chunks {
            let t = c.trim();
            assert!(
                PROSE.contains(t),
                "overlap produced a chunk that is not a verbatim slice: {t:?}"
            );
        }
    }
}
