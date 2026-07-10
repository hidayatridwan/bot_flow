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
}
