use anyhow::{Context, Result};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}
#[derive(Deserialize)]
struct StreamChoice {
    delta: Delta,
}
#[derive(Deserialize)]
struct Delta {
    content: Option<String>, // absent on the first/last chunks (role-only, finish)
}

impl LlmClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        }
    }

    /// Send the system + user prompt, return the answer text.
    pub async fn answer(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            temperature: 0.2, // low = answers faithful to the context, not creative
            max_tokens: 512,
            stream: false,
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to send request to the LLM")?;

        // Important: check the status first. If it's not 2xx, surface the error body —
        // this is the most common headache when the model/key is wrong.
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM replied {status}: {text}");
        }

        let parsed: ChatResponse = resp.json().await.context("failed to parse LLM response")?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .context("LLM response has no choices")
    }

    /// Like `answer`, but streams the reply as text deltas (OpenAI SSE).
    /// The stream ends when the upstream sends the `[DONE]` sentinel.
    pub async fn answer_stream(
        &self,
        system: &str,
        user: &str,
    ) -> Result<impl Stream<Item = Result<String>>> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            temperature: 0.2,
            max_tokens: 512,
            stream: true,
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to send streaming request to the LLM")?;

        // Status check BEFORE streaming — same rule as `answer`: a bad key/model is the
        // common failure, and we want a clean error, not a half-open stream.
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM replied {status}: {text}");
        }

        // Parse SSE byte stream -> text deltas; map_while stops the stream at `[DONE]`.
        let stream = resp.bytes_stream().eventsource().map(|event| match event {
            Err(e) => Err(anyhow::Error::new(e).context("SSE stream error")),
            // `[DONE]` is the end sentinel; the connection closes right after, so emitting an
            // empty string here is harmless (the endpoint skips empty deltas).
            Ok(ev) if ev.data == "[DONE]" => Ok(String::new()),
            Ok(ev) => {
                let chunk: StreamChunk = serde_json::from_str(&ev.data).context("bad SSE chunk")?;
                Ok(chunk
                    .choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.delta.content)
                    .unwrap_or_default())
            }
        });

        Ok(stream)
    }
}
