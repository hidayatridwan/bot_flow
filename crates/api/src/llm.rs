use std::time::Duration;

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

/// The ceiling on one completion.
///
/// **This budget is shared with the model's reasoning, and that is the whole reason it is not 512.**
/// A reasoning model bills its thinking against `max_tokens` alongside its prose, and the thinking is
/// emitted as `reasoning_content` deltas, which `Delta` deliberately ignores. So a question that
/// thinks hard enough spends the entire budget before writing a word: `finish_reason: "length"`, zero
/// `content` deltas, and an answer that is silently empty. Nothing errors — the stream completes
/// normally and the endpoint yields `done`, exactly as it should, having been given nothing to say.
///
/// Reproduced against the configured gateway: squeeze the budget below what thinking needs and the
/// content is empty every time. At 512 a simple question survives on ~80-180 reasoning tokens, which
/// is why this went unnoticed — the margin was real but thin, and a longer document or a
/// cross-lingual question eats it.
///
/// Raising it does not raise the cost of a normal answer: the model stops when it is done, and this
/// is a ceiling, not a target. What it buys is headroom for the thinking we cannot see and do not
/// control. Note that on `answer_stream` this is still the only bound on *length* — `READ_TIMEOUT`
/// bounds silence, not duration (invariant 28) — so it is load-bearing beyond the empty-answer bug
/// it was raised for.
const MAX_TOKENS: u32 = 4096;

/// TCP + TLS to the gateway. A handshake that has not completed in this long is not slow, it is
/// pointed at nothing — the one failure here that is never worth waiting out.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// **Max silence between reads — not a deadline on the answer.** This is the bound that makes
/// invariant 28 true, and the reason it is `read_timeout` rather than `timeout` is the whole trap:
/// reqwest's client `timeout` is a *total* deadline that includes the body, and `answer_stream`'s
/// body is the answer itself. A total there would cap how long an answer may be.
///
/// Generous on purpose. It bounds a *hang*, and the cost of guessing low is real: a reasoning model
/// can sit quiet between deltas, and killing that looks identical to the gateway dying. Sixty seconds
/// of total silence is not a slow model, it is a dead socket.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Total deadline for the **non-streaming** `answer` only, applied per request.
///
/// Safe here for the reason it is unsafe on the stream: this body is one short JSON document, so its
/// duration is the gateway's latency rather than the answer's length. Sized above `READ_TIMEOUT` so a
/// single stall still surfaces as a stall; this only catches a gateway that trickles bytes forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

impl LlmClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            // No total `.timeout()`: it would include the body, and `answer_stream`'s body is a
            // long-lived SSE. `answer` adds one per request instead. See invariant 28.
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                .build()
                // The builder only fails on a bad TLS backend — a compile-time-ish fault, not a
                // runtime one, and there is no sane fallback client to construct.
                .expect("llm http client"),
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
            max_tokens: MAX_TOKENS,
            stream: false,
        };

        let resp = self
            .http
            .post(&url)
            .timeout(ANSWER_TIMEOUT) // per-request; the client carries no total (invariant 28)
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
            max_tokens: MAX_TOKENS,
            stream: true,
        };

        // Deliberately no `.timeout()` — see invariant 28 and `READ_TIMEOUT`. A total deadline here
        // would bound the answer's *length*, not the gateway's health, and every answer still
        // streaming when it fired would be truncated into an `error` frame and then dropped from
        // history by invariant 7. The stall bound on the client is what covers this call.
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

        // Parse SSE byte stream -> text deltas; the `[DONE]` sentinel ends it.
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

/// Why these tests exist, and why they test `reqwest` rather than us.
///
/// Invariant 28 rests on one factual claim about a dependency: that a client-wide `timeout` is a
/// **total deadline covering the response body**, while `read_timeout` bounds only the gap *between*
/// reads and so is indifferent to how long a body runs. Every design decision in this file follows
/// from that claim — `answer_stream` carries no total precisely because its body *is* the answer.
///
/// The claim came from reqwest's documentation. Documentation is where the expensive mistakes in this
/// repo have always lived, so it is pinned here by observation instead: a real socket, a real body
/// arriving in real pieces. If a future reqwest reverses either behaviour, this fails loudly rather
/// than every long answer failing quietly in production.
///
/// No backing service — the server is a `TcpListener` on an ephemeral port, in-process.
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve one close-delimited response whose body arrives in `pieces` writes, `gap` apart.
    /// Deliberately hand-rolled HTTP: the point is to control *when* bytes land, which no client
    /// library will let us do.
    async fn dribbling_server(pieces: usize, gap: Duration) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();

            // Drain the request head; we do not care what it says, only that it is complete.
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;

            // No Content-Length and no Transfer-Encoding: the body ends when we close. That is the
            // simplest framing that lets a reader see bytes before the end.
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..pieces {
                tokio::time::sleep(gap).await;
                if sock.write_all(b"x").await.is_err() {
                    return; // client hung up — which is the whole point of one of these tests
                }
                sock.flush().await.unwrap();
            }
        });

        format!("http://{addr}")
    }

    /// Read the body to the end, reporting only whether it survived.
    async fn drain(resp: reqwest::Response) -> Result<usize, reqwest::Error> {
        let mut stream = resp.bytes_stream();
        let mut n = 0;
        while let Some(chunk) = stream.next().await {
            n += chunk?.len();
        }
        Ok(n)
    }

    /// The trap, demonstrated. A body that arrives over ~300ms dies under a 100ms *total* timeout —
    /// even though the connection is perfectly healthy and delivering bytes the whole time.
    ///
    /// This is what the drafted phase-6 design would have done to every answer longer than the
    /// timeout: not a hung gateway, just a long reply.
    #[tokio::test]
    async fn a_total_timeout_kills_a_healthy_body_that_merely_takes_a_while() {
        let url = dribbling_server(10, Duration::from_millis(30)).await;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();

        let err = match client.get(&url).send().await {
            Ok(resp) => drain(resp).await.expect_err("the total deadline must fire"),
            Err(e) => e, // headers arrive fast, so the failure is normally in the body
        };
        assert!(
            err.is_timeout(),
            "expected a timeout, got a different failure: {err}"
        );
    }

    /// The instrument we actually use. The same body, the same duration, a read timeout *shorter*
    /// than the total elapsed time — and it completes, because no single gap exceeds it.
    ///
    /// This is the property `answer_stream` depends on: an answer may run as long as it likes, so
    /// long as the gateway keeps talking.
    #[tokio::test]
    async fn a_read_timeout_lets_a_slow_body_finish_because_it_bounds_the_gap_not_the_total() {
        let url = dribbling_server(10, Duration::from_millis(30)).await;

        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(500))
            .build()
            .unwrap();

        let resp = client.get(&url).send().await.unwrap();
        let n = drain(resp)
            .await
            .expect("a talking gateway must not be cut off");
        assert_eq!(n, 10, "every piece should have arrived");
    }

    /// And it still catches the thing it is for: a gateway that accepts, answers, then goes quiet.
    /// One long gap, no total deadline anywhere — the read timeout is the only thing that can end
    /// this, and it must.
    #[tokio::test]
    async fn a_read_timeout_aborts_a_gateway_that_stops_talking() {
        let url = dribbling_server(1, Duration::from_secs(30)).await;

        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(100))
            .build()
            .unwrap();

        let err = match client.get(&url).send().await {
            Ok(resp) => drain(resp).await.expect_err("the read timeout must fire"),
            Err(e) => e,
        };
        assert!(
            err.is_timeout(),
            "expected a timeout, got a different failure: {err}"
        );
    }
}
