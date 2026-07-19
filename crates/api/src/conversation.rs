//! Conversation memory + query rewriting.
//!
//! Why this exists: retrieval embeds the query verbatim, so a follow-up like "what is his
//! mobile number?" carries no semantic link to the document and scores below the relevance
//! floor. We resolve the pronouns against stored history *before* embedding.
//!
//! Every function takes its dependencies as arguments (`&PgPool`, `&LlmClient`) rather than
//! reaching into AppState, so each stage can be exercised on its own.

use axum::http::StatusCode;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db;
use crate::error::AppError;
use crate::llm::LlmClient;

/// How many past messages are fed to the rewriter. Enough to resolve a reference,
/// small enough to keep the rewrite prompt cheap.
pub const HISTORY_LIMIT: i64 = 10;

/// A rewritten query longer than this is a sign the model ignored the instruction and
/// started answering. We fall back to the original rather than embed prose.
const MAX_REWRITE_CHARS: usize = 500;

const REWRITE_SYSTEM_PROMPT: &str = "You rewrite a user's latest message into a standalone question. \
    Use the conversation to resolve every pronoun and implicit reference (\"he\", \"his\", \"that one\", \
    \"there\") into the explicit name or noun it refers to. Preserve names, numbers, emails and spelling \
    EXACTLY as they appear. Do NOT answer the question. Do NOT explain. Output only the rewritten \
    question and nothing else. If the message is already standalone, output it unchanged.";

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Resolve the caller's conversation, creating one when they didn't supply an id.
///
/// The lookup runs under the tenant transaction, so RLS scopes it: an id belonging to another
/// tenant is simply not visible and comes back as 404 — the same response as an id that never
/// existed, which is what keeps it from being a cross-tenant probe.
pub async fn ensure(
    db: &PgPool,
    tenant_id: &str,
    conversation_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    let mut tx = db::tenant_tx(db, tenant_id).await?;

    let id = match conversation_id {
        Some(id) => {
            let found = sqlx::query("SELECT id FROM conversations WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            if found.is_none() {
                return Err(AppError::client(
                    StatusCode::NOT_FOUND,
                    "conversation not found",
                ));
            }
            id
        }
        None => {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO conversations (id, tenant_id) VALUES ($1, $2)")
                .bind(id)
                .bind(tenant_id)
                .execute(&mut *tx)
                .await?;
            id
        }
    };

    tx.commit().await?;
    Ok(id)
}

/// The last `limit` messages, oldest first (the order a transcript reads in).
pub async fn recent(
    db: &PgPool,
    tenant_id: &str,
    conversation_id: Uuid,
    limit: i64,
) -> Result<Vec<Message>, AppError> {
    let mut tx = db::tenant_tx(db, tenant_id).await?;
    // Take the newest `limit` rows, then flip them back into chronological order.
    let rows = sqlx::query(
        "SELECT role, content FROM (
             SELECT role, content, seq FROM messages
             WHERE conversation_id = $1 ORDER BY seq DESC LIMIT $2
         ) AS newest ORDER BY seq ASC",
    )
    .bind(conversation_id)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(rows
        .iter()
        .map(|r| Message {
            role: r.get("role"),
            content: r.get("content"),
        })
        .collect())
}

/// Record a completed exchange: the user's question and the answer they were given.
///
/// Both rows go in one transaction, and only once an answer exists. Persisting the question
/// eagerly (before the LLM responds) would leave a dangling `user` row behind every failed
/// request — and the next rewrite would then reason over a transcript of unanswered questions.
/// `seq` (not `created_at`) orders them: inside one transaction `now()` is identical for both.
pub async fn append_turn(
    db: &PgPool,
    tenant_id: &str,
    conversation_id: Uuid,
    question: &str,
    answer: &str,
    // The documents whose passages were in the model's context for this answer.
    //
    // `messages` never stored the passages themselves — but an answer is *derived* from them and
    // routinely quotes them, so erasing a document while leaving the answers that recite it is an
    // erasure with a hole in it. Nothing could find those answers until they carried this.
    // Empty for a refusal: no context, nothing to attribute (invariant 4).
    source_documents: &[String],
) -> Result<(), AppError> {
    let mut tx = db::tenant_tx(db, tenant_id).await?;
    for (role, content) in [("user", question), ("assistant", answer)] {
        // Only the assistant's turn cites anything. The user's question is their own words.
        let metadata = if role == "assistant" && !source_documents.is_empty() {
            serde_json::json!({ "document_ids": source_documents })
        } else {
            serde_json::json!({})
        };
        sqlx::query(
            "INSERT INTO messages (id, conversation_id, tenant_id, role, content, metadata)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(conversation_id)
        .bind(tenant_id)
        .bind(role)
        .bind(content)
        .bind(&metadata)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Render history as the plain transcript the rewriter reads.
pub fn transcript(history: &[Message]) -> String {
    history
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Turn a context-dependent question into a standalone one.
///
/// No history means nothing to resolve against, so we skip the LLM entirely — the first turn of
/// every conversation costs exactly what it did before this feature existed.
pub async fn rewrite(llm: &LlmClient, history: &[Message], query: &str) -> String {
    if history.is_empty() {
        return query.to_string();
    }

    let user = format!(
        "CONVERSATION:\n{}\n\nLATEST MESSAGE: {query}\n\nRewritten standalone question:",
        transcript(history)
    );

    match llm.answer(REWRITE_SYSTEM_PROMPT, &user).await {
        Ok(rewritten) => {
            let rewritten = rewritten.trim().trim_matches('"').trim();
            // An empty or runaway response means the model ignored us. The original query is a
            // worse retrieval key but a safe one; a hallucinated paragraph is neither.
            if rewritten.is_empty() || rewritten.chars().count() > MAX_REWRITE_CHARS {
                tracing::warn!("discarding unusable rewrite, falling back to the raw query");
                query.to_string()
            } else {
                tracing::debug!(original = query, rewritten = rewritten, "rewrote query");
                rewritten.to_string()
            }
        }
        // A rewriter outage must not take the whole answer down with it.
        Err(e) => {
            tracing::warn!("query rewrite failed ({e:#}), falling back to the raw query");
            query.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn transcript_is_role_prefixed_and_chronological() {
        let history = vec![
            msg("user", "Who is ridwan hidayat?"),
            msg("assistant", "A Backend Engineer."),
        ];
        assert_eq!(
            transcript(&history),
            "user: Who is ridwan hidayat?\nassistant: A Backend Engineer."
        );
    }

    #[test]
    fn transcript_of_empty_history_is_empty() {
        assert_eq!(transcript(&[]), "");
    }

    /// The empty-history branch must not touch the LLM. We pass a client pointed at an
    /// unroutable address: if `rewrite` tried to call it, this would hang or error rather
    /// than return the query untouched.
    #[tokio::test]
    async fn rewrite_skips_the_llm_when_there_is_no_history() {
        let llm = LlmClient::new(
            "http://127.0.0.1:1".to_string(),
            "unused".to_string(),
            "unused".to_string(),
        );
        let out = rewrite(&llm, &[], "What is his mobile number?").await;
        assert_eq!(out, "What is his mobile number?");
    }

    /// When the LLM is unreachable, a follow-up still answers — degraded, using the raw query.
    #[tokio::test]
    async fn rewrite_falls_back_to_the_raw_query_when_the_llm_fails() {
        let llm = LlmClient::new(
            "http://127.0.0.1:1".to_string(),
            "unused".to_string(),
            "unused".to_string(),
        );
        let history = vec![msg("user", "Who is ridwan hidayat?")];
        let out = rewrite(&llm, &history, "What is his mobile number?").await;
        assert_eq!(out, "What is his mobile number?");
    }
}
