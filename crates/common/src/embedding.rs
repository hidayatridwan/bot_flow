//! The embedding client, shared by the api and the worker.
//!
//! One definition on purpose. If the two binaries embedded through separate code they could drift
//! to different models or different request shapes, and each would then write vectors the other
//! cannot meaningfully search — a silent retrieval failure, not a build error.

use serde::{Deserialize, Serialize};

/// Vector dimension of `text-embedding-3-small` at its native size. MUST match the Qdrant
/// collection config. Changing it invalidates every stored vector; see the cutover note in README.
pub const EMBEDDING_DIM: u64 = 1536;

/// Inputs per `/embeddings` request. The endpoint caps both input count and total tokens, and a
/// large PDF chunks into thousands of pieces, so a document cannot go up in one call.
const EMBED_BATCH: usize = 96;

#[derive(Clone)]
pub struct EmbeddingClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    // No `dimensions` field. A proxy that does not understand it ignores it silently, and we would
    // then write native-width vectors into a narrower collection. Take the native width instead.
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    /// Position in the request's `input` array. The response is *not* guaranteed to preserve order.
    index: usize,
    embedding: Vec<f32>,
}

/// Failures are typed rather than flattened into `anyhow`, because the worker must decide whether
/// redelivering the message could ever succeed. See [`EmbedError::is_fatal`].
#[derive(Debug)]
pub enum EmbedError {
    /// Could not reach the endpoint, or the connection broke: DNS, TLS, timeout, reset.
    Transport(reqwest::Error),
    /// The endpoint answered with a non-2xx status.
    Status { status: u16, body: String },
    /// The endpoint answered 2xx with something we cannot use.
    Protocol(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "embedding endpoint unreachable: {e}"),
            Self::Status { status, body } => {
                write!(f, "embedding endpoint replied {status}: {body}")
            }
            Self::Protocol(msg) => write!(f, "malformed embedding response: {msg}"),
        }
    }
}

impl std::error::Error for EmbedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl EmbedError {
    /// Would retrying this exact request ever succeed?
    ///
    /// The two mistakes here are not symmetric. Calling a transient failure fatal **acks the message
    /// and destroys the document**. Calling a permanent failure retryable costs five redeliveries and
    /// then dead-letters it, where it can still be recovered. So anything ambiguous is retryable.
    ///
    /// That is why a `401` is *not* fatal: a wrong `EMBEDDING_API_KEY` is an operator mistake, and the
    /// document should still be waiting once the key is fixed. Likewise `429` and every `5xx`.
    ///
    /// Only the document itself being un-embeddable is fatal, because no amount of retrying changes
    /// the bytes. `413` says so unambiguously. A `400` does not — it covers a bad model name (an
    /// operator mistake) as well as an oversized input — so we look at the body, and default to
    /// retryable when it does not clearly blame the input.
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Transport(_) => false,
            Self::Protocol(_) => false,
            Self::Status { status: 413, .. } => true,
            Self::Status { status: 400, body } => {
                let b = body.to_lowercase();
                b.contains("maximum context length")
                    || b.contains("too long")
                    || b.contains("too many tokens")
            }
            Self::Status { .. } => false,
        }
    }
}

impl EmbeddingClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        }
    }

    /// Embed many texts, in batches. The returned vectors line up with `texts` by position — callers
    /// zip them against their chunks, so order is a correctness property, not a convenience.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(EMBED_BATCH) {
            out.extend(self.embed_one_batch(batch).await?);
        }
        Ok(out)
    }

    /// Embed a single text — a question. No prefix: `text-embedding-3-small` is symmetric, unlike the
    /// E5 model this replaced, which needed `passage: ` / `query: `. Adding one back would embed the
    /// literal word into the vector.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut v = self
            .embed_batch(std::slice::from_ref(&text.to_string()))
            .await?;
        if v.len() != 1 {
            return Err(EmbedError::Protocol(format!(
                "expected 1 embedding, got {}",
                v.len()
            )));
        }
        Ok(v.remove(0))
    }

    async fn embed_one_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&EmbedRequest {
                model: &self.model,
                input: texts,
            })
            .send()
            .await
            .map_err(EmbedError::Transport)?;

        // Status first, same as llm.rs: a bad key or model is the common failure, and we want the
        // upstream's error body rather than a parse error about a body we never expected.
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(EmbedError::Status {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| EmbedError::Protocol(e.to_string()))?;

        order_embeddings(parsed.data, texts.len())
    }
}

/// Restore request order and validate shape.
///
/// Split out from the HTTP call so it can be tested without a network. Both checks matter: callers
/// zip the result against their input by position, so a reordered response would attach each chunk's
/// text to a different chunk's vector — a corruption that no assertion downstream would catch.
fn order_embeddings(
    mut data: Vec<EmbeddingData>,
    expected: usize,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if data.len() != expected {
        return Err(EmbedError::Protocol(format!(
            "expected {expected} embeddings, got {}",
            data.len()
        )));
    }
    data.sort_by_key(|d| d.index);

    for (i, d) in data.iter().enumerate() {
        if d.index != i {
            return Err(EmbedError::Protocol(format!(
                "response indices are not a permutation of 0..{expected}"
            )));
        }
        // Guards the collection against a model that quietly returns a different width — a wrong
        // EMBEDDING_MODEL, or a proxy that honoured a `dimensions` param we did not send.
        if d.embedding.len() != EMBEDDING_DIM as usize {
            return Err(EmbedError::Protocol(format!(
                "expected {EMBEDDING_DIM}-dim vectors, got {}",
                d.embedding.len()
            )));
        }
    }
    Ok(data.into_iter().map(|d| d.embedding).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datum(index: usize, fill: f32) -> EmbeddingData {
        EmbeddingData {
            index,
            embedding: vec![fill; EMBEDDING_DIM as usize],
        }
    }

    #[test]
    fn out_of_order_response_is_restored_to_request_order() {
        let data = vec![datum(2, 2.0), datum(0, 0.0), datum(1, 1.0)];
        let got = order_embeddings(data, 3).unwrap();
        assert_eq!(got[0][0], 0.0);
        assert_eq!(got[1][0], 1.0);
        assert_eq!(got[2][0], 2.0);
    }

    #[test]
    fn short_response_is_an_error() {
        let err = order_embeddings(vec![datum(0, 0.0)], 2).unwrap_err();
        assert!(matches!(err, EmbedError::Protocol(_)));
    }

    #[test]
    fn duplicate_indices_are_an_error() {
        // Right length, wrong identity: without the permutation check this would silently return
        // the same vector twice.
        let err = order_embeddings(vec![datum(0, 0.0), datum(0, 1.0)], 2).unwrap_err();
        assert!(matches!(err, EmbedError::Protocol(_)));
    }

    #[test]
    fn wrong_dimension_is_an_error() {
        let data = vec![EmbeddingData {
            index: 0,
            embedding: vec![0.0; 384], // what the old MultilingualE5Small returned
        }];
        let err = order_embeddings(data, 1).unwrap_err();
        assert!(matches!(err, EmbedError::Protocol(_)));
    }

    #[test]
    fn batching_splits_on_the_cap_and_preserves_order() {
        let texts: Vec<String> = (0..200).map(|i| i.to_string()).collect();
        let batches: Vec<&[String]> = texts.chunks(EMBED_BATCH).collect();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), EMBED_BATCH);
        assert_eq!(batches[1].len(), EMBED_BATCH);
        assert_eq!(batches[2].len(), 200 - 2 * EMBED_BATCH);
        let flat: Vec<String> = batches.concat();
        assert_eq!(flat.first().unwrap().as_str(), "0");
        assert_eq!(flat.last().unwrap().as_str(), "199");
    }

    #[test]
    fn transient_failures_are_retryable_so_the_document_survives() {
        for status in [401, 403, 429, 500, 502, 503] {
            let e = EmbedError::Status {
                status,
                body: String::new(),
            };
            assert!(!e.is_fatal(), "{status} must not discard the document");
        }
    }

    #[test]
    fn only_an_unembeddable_document_is_fatal() {
        assert!(EmbedError::Status {
            status: 413,
            body: String::new()
        }
        .is_fatal());
        assert!(EmbedError::Status {
            status: 400,
            body: "This model's maximum context length is 8192 tokens".into()
        }
        .is_fatal());
        // A 400 that blames the request, not the input, must not destroy the document.
        assert!(!EmbedError::Status {
            status: 400,
            body: "The model `text-embedding-3-large` does not exist".into()
        }
        .is_fatal());
    }
}
