use anyhow::Context;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// MUST match crates/api/src/embedding.rs — same model, same prefix, or retrieval degrades.
pub fn init_embedder() -> anyhow::Result<TextEmbedding> {
    TextEmbedding::try_new(
        TextInitOptions::new(EmbeddingModel::MultilingualE5Small).with_show_download_progress(true),
    )
    .context("failed to load embedding model")
}

/// E5 requires the "passage: " prefix for STORED text.
pub fn embed_passages(
    model: &mut TextEmbedding,
    texts: &[String],
) -> anyhow::Result<Vec<Vec<f32>>> {
    let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
    model
        .embed(prefixed, None)
        .context("failed to embed passages")
}
