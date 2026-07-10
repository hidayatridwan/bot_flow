use anyhow::Context;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// Vector dimension of MultilingualE5Small. MUST match the Qdrant collection config.
pub const EMBEDDING_DIM: u64 = 384;

/// Load the model. Slow: download (once) + load ONNX. Call once at startup.
pub fn init_embedder() -> anyhow::Result<TextEmbedding> {
    TextEmbedding::try_new(
        TextInitOptions::new(EmbeddingModel::MultilingualE5Small).with_show_download_progress(true),
    )
    .context("failed to load embedding model")
}

/// E5 needs a prefix: "passage: " for text that is STORED.
pub fn embed_passages(
    model: &mut TextEmbedding,
    texts: &[String],
) -> anyhow::Result<Vec<Vec<f32>>> {
    let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {t}")).collect();
    model.embed(prefixed, None).context("failed to embed passages")
}

/// E5 needs a prefix: "query: " for QUESTIONS. Wrong prefix = lower quality.
pub fn embed_query(model: &mut TextEmbedding, query: &str) -> anyhow::Result<Vec<f32>> {
    let mut out = model
        .embed(vec![format!("query: {query}")], None)
        .context("failed to embed query")?;
    Ok(out.remove(0))
}
