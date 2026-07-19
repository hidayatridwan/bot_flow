//! The retrieval bench (phase 10).
//!
//! **Why this exists.** Every improvement to retrieval requires re-indexing every vector in the
//! system, which CLAUDE.md calls *"a migration project, not a configuration change"* — irreversible,
//! with no rollback. And the property being changed is the only one in the system with no meter. Ship
//! a new chunker, watch a few questions still answer plausibly, and you have learned nothing:
//! plausible is exactly what this system produces when retrieval is wrong.
//!
//! **Why it is not the phase-9 harness.** That harness embeds with a deterministic content-addressed
//! fake — same string 1.0, different string ~0.0 — which is what makes "tenant B retrieved nothing"
//! mean *the filter worked*. Under it semantic similarity does not exist, so a better chunker and a
//! worse one score identically, and a chunking change breaks its assertions outright. It cannot be
//! extended into an evaluator; this is the second instrument.
//!
//! **What makes it safe to run.** It writes to its own collection (`eval_bench`), never `documents`,
//! and drops it at the start of every run. It touches no tenant, no database and no production index.
//! It does make **real, billed** `/embeddings` calls — that is the whole point, and it is why this is
//! a deliberate command rather than a CI job (phase 9's line: CI stays free and secret-free).
//!
//! ```text
//! cargo run -p eval               # baseline + the full sabotage table
//! cargo run -p eval -- baseline   # one variant
//! ```

use std::collections::HashMap;

use anyhow::{Context, Result};
use common::chunk::{chunk_text, chunk_text_recursive, CHUNK_OVERLAP, CHUNK_SIZE};
use common::embedding::{EmbeddingClient, EMBEDDING_DIM};
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, Distance, Filter, PointStruct, QueryPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::Qdrant;
use serde::Deserialize;

/// Never `documents`. A bench that shared the production collection could not drop it between runs,
/// and a half-cleaned collection is the exact silent-degradation hazard this phase is about.
const COLLECTION: &str = "eval_bench";

/// The bench indexes as one tenant and filters on it, mirroring production: every real query carries
/// `.filter(tenant_filter(..))`, and a filtered ANN search does not behave identically to an
/// unfiltered one. Measuring the unfiltered path would measure a system we do not run.
const TENANT: &str = "eval-bench-tenant";

/// How deep to fetch. `recall@10` needs ten; production's default is 3, and D6 proposes over-fetching
/// precisely because the floor is applied afterwards and shrinks the result set.
const FETCH: u64 = 10;

#[derive(Deserialize)]
struct Golden {
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct Question {
    q: String,
    expects: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// One row of the bench. A variant is a recipe plus a deliberate defect; the defects exist to prove
/// the metrics can move at all.
#[derive(Clone, Copy, PartialEq)]
struct Variant {
    name: &'static str,
    chunk_size: usize,
    overlap: usize,
    threshold: f32,
    /// Reverse the ranking before scoring. MRR must collapse; recall@10 must NOT — proving the two
    /// measure different things, which every later reading of the table depends on.
    reverse_rank: bool,
    /// Replace the query vector with noise. Every metric must hit the floor; anything that survives
    /// means the golden set is matching on something other than retrieval.
    random_query: bool,
    /// D3 — boundary-aware splitting instead of the fixed window.
    recursive: bool,
    /// D13 — prepend the document title to each chunk's embedded text.
    inject_title: bool,
    /// D14 — at most this many chunks from any one document may occupy the top-k. `None` = no cap.
    per_doc_cap: Option<usize>,
}

impl Variant {
    const fn baseline() -> Self {
        Self {
            name: "production (boundary-aware, CHUNK_SIZE/CHUNK_OVERLAP)",
            chunk_size: CHUNK_SIZE,
            overlap: CHUNK_OVERLAP,
            // The live value. Read from the environment so the bench measures the deployment rather
            // than a number this file invented — see `resolve_threshold`.
            threshold: f32::NAN,
            reverse_rank: false,
            random_query: false,
            // Production uses the boundary-aware chunker as of phase 10, so the bench's reference
            // row must too — a "baseline" that measures a recipe nobody runs is a number that
            // will be compared against and is wrong.
            recursive: true,
            inject_title: false,
            per_doc_cap: None,
        }
    }
}

/// The candidate recipes (D3, D13, D14). None of these touches production: the bench builds its own
/// index, so an irreversible decision gets made reversibly here first.
fn candidates() -> Vec<Variant> {
    let b = Variant::baseline();
    let mut v = vec![Variant {
        name: "D3 boundary-aware, 800/100",
        recursive: true,
        ..b
    }];
    // The size sweep. The `chunk_size=8000` sabotage scored the BEST retrieval on this corpus at
    // 1.8x the context cost, which is evidence that 800 may simply be too small — so size is a
    // variable to measure, not a constant to assume.
    for (size, overlap) in [
        (300usize, 40usize),
        (400, 50),
        (500, 60),
        (600, 75),
        (1200, 150),
        (1600, 200),
    ] {
        v.push(Variant {
            name: Box::leak(format!("D3 boundary-aware, {size}/{overlap}").into_boxed_str()),
            recursive: true,
            chunk_size: size,
            overlap,
            ..b
        });
    }
    // D13 and D14 are measured at 400/50 as well as 800/100: a modifier that helps at one chunk
    // size need not help at another, and shipping it on the strength of the wrong size is exactly
    // the unattributable-delta mistake this bench exists to prevent.
    for (size, overlap) in [(800usize, 100usize), (400, 50)] {
        v.push(Variant {
            name: Box::leak(format!("D3 {size}/{overlap} + D13 title").into_boxed_str()),
            recursive: true,
            chunk_size: size,
            overlap,
            inject_title: true,
            ..b
        });
        v.push(Variant {
            name: Box::leak(format!("D3 {size}/{overlap} + D14 cap 2").into_boxed_str()),
            recursive: true,
            chunk_size: size,
            overlap,
            per_doc_cap: Some(2),
            ..b
        });
        v.push(Variant {
            name: Box::leak(format!("D3 {size}/{overlap} + D13 + D14").into_boxed_str()),
            recursive: true,
            chunk_size: size,
            overlap,
            inject_title: true,
            per_doc_cap: Some(2),
            ..b
        });
    }
    v
}

fn sabotages() -> Vec<Variant> {
    let b = Variant::baseline();
    vec![
        Variant {
            name: "SABOTAGE chunk_size=40 (too small to hold an answer)",
            chunk_size: 40,
            overlap: 5,
            ..b
        },
        Variant {
            name: "SABOTAGE chunk_size=8000 (one chunk per document)",
            chunk_size: 8000,
            overlap: 100,
            ..b
        },
        Variant {
            name: "SABOTAGE reversed ranking",
            reverse_rank: true,
            ..b
        },
        Variant {
            name: "SABOTAGE threshold=0.9 (the README's 0.70 bug, exaggerated)",
            threshold: 0.9,
            ..b
        },
        Variant {
            name: "SABOTAGE random query vector",
            random_query: true,
            ..b
        },
    ]
}

struct Scores {
    /// The discriminating metric at this corpus size. `recall@3` saturates at 1.000 on a corpus this
    /// small — a metric pinned at its ceiling can detect neither improvement nor regression — while
    /// `recall@1` and MRR retain headroom. Reported first for that reason.
    recall_at_1: f32,
    recall_at_3: f32,
    recall_at_10: f32,
    mrr: f32,
    /// Mean characters of context delivered in the top 3.
    ///
    /// **Recall alone cannot see an over-large chunk, and that is not a small gap.** "Did a returned
    /// passage contain the substring" is trivially satisfied by returning the whole document, so a
    /// one-chunk-per-document recipe scores a *perfect* recall and a *better* MRR while handing the
    /// model 8 KB to answer a twenty-character question. Measured on the bench: `chunk_size=8000`
    /// took MRR from 0.818 to 0.932. Without this column that recipe looks like an improvement.
    /// Retrieval quality is finding the answer *and* not burying it; this is the second half.
    ctx_chars_at_3: f32,
    /// Questions where the corpus deliberately contradicts itself. Reported separately: a miss here
    /// is the superseded-policy problem, not a chunking failure, and averaging it into the headline
    /// would hide the very thing the worked example exists to show.
    conflict_hits: usize,
    conflict_total: usize,
    crosslingual_hits: usize,
    crosslingual_total: usize,
    misses: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let which = std::env::args().nth(1);
    let threshold = resolve_threshold();

    let corpus = load_corpus()?;
    let golden: Golden = serde_json::from_str(include_str!("../fixtures/golden.json"))
        .context("failed to parse fixtures/golden.json")?;

    println!(
        "corpus: {} documents, golden set: {} questions, threshold: {threshold}\n",
        corpus.len(),
        golden.questions.len()
    );

    let embedder = EmbeddingClient::new(
        std::env::var("EMBEDDING_BASE_URL")
            .or_else(|_| std::env::var("LLM_BASE_URL"))
            .context("neither EMBEDDING_BASE_URL nor LLM_BASE_URL is set")?,
        std::env::var("EMBEDDING_API_KEY").context("EMBEDDING_API_KEY is not set")?,
        std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| "text-embedding-3-small".to_string()),
    );
    let qdrant = Qdrant::from_url(&std::env::var("QDRANT_URL").context("QDRANT_URL is not set")?)
        .build()
        .context("failed to build Qdrant client")?;

    // The question vectors do not depend on the chunking recipe, so they are embedded once and
    // reused across every variant — one billed call per question for the whole run, not per variant.
    let questions: Vec<String> = golden.questions.iter().map(|q| q.q.clone()).collect();
    let query_vectors = embedder
        .embed_batch(&questions)
        .await
        .map_err(|e| anyhow::anyhow!("failed to embed the golden questions: {e}"))?;

    let mut variants = vec![Variant {
        threshold,
        ..Variant::baseline()
    }];
    if which.as_deref() == Some("candidates") {
        variants.extend(candidates().into_iter().map(|v| Variant { threshold, ..v }));
    } else if which.as_deref() != Some("baseline") {
        variants.extend(sabotages().into_iter().map(|v| Variant {
            threshold: if v.threshold.is_nan() {
                threshold
            } else {
                v.threshold
            },
            ..v
        }));
    }

    let mut results = Vec::new();
    for variant in &variants {
        let scores = run_variant(
            &embedder,
            &qdrant,
            &corpus,
            &golden,
            &query_vectors,
            variant,
        )
        .await
        .with_context(|| format!("variant '{}' failed", variant.name))?;
        results.push((*variant, scores));
    }

    report(&results);
    Ok(())
}

/// The threshold the *deployment* uses, not one this file invented. Same default as `config.rs`, so
/// an unset environment measures what a fresh checkout would do.
fn resolve_threshold() -> f32 {
    std::env::var("RAG_SCORE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.70)
}

/// A human-ish title from the filename. Production would join `documents.filename` (D5 argues
/// against denormalising it into the payload); the bench has only the file, which is the same
/// information.
fn title_of(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename)
        .replace(['-', '_'], " ")
}

fn load_corpus() -> Result<Vec<(String, String)>> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/corpus");
    let mut docs: Vec<(String, String)> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read the fixture corpus at {dir}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path())?;
            Ok((name, text))
        })
        .collect::<Result<Vec<_>>>()?;
    // Deterministic order, so a run is reproducible and two runs are comparable.
    docs.sort_by(|a, b| a.0.cmp(&b.0));
    anyhow::ensure!(!docs.is_empty(), "the fixture corpus is empty");
    Ok(docs)
}

async fn run_variant(
    embedder: &EmbeddingClient,
    qdrant: &Qdrant,
    corpus: &[(String, String)],
    golden: &Golden,
    query_vectors: &[Vec<f32>],
    variant: &Variant,
) -> Result<Scores> {
    // Rebuild the collection from scratch. A variant that inherited the previous variant's points
    // would be measuring a blend of two recipes and would look plausible doing it.
    let _ = qdrant.delete_collection(COLLECTION).await;
    qdrant
        .create_collection(
            CreateCollectionBuilder::new(COLLECTION)
                .vectors_config(VectorParamsBuilder::new(EMBEDDING_DIM, Distance::Cosine)),
        )
        .await
        .context("failed to create the bench collection")?;

    let mut chunks: Vec<(String, String)> = Vec::new(); // (document, text)
    for (name, text) in corpus {
        let pieces = if variant.recursive {
            chunk_text_recursive(text, variant.chunk_size, variant.overlap)
        } else {
            chunk_text(text, variant.chunk_size, variant.overlap)
        };
        for c in pieces {
            chunks.push((name.clone(), c));
        }
    }

    // D13: the title goes into the EMBEDDED text, so it shapes the vector — that is the whole
    // point, and also why it can never be corrected without a re-embed.
    let texts: Vec<String> = chunks
        .iter()
        .map(|(doc, t)| {
            if variant.inject_title {
                format!("[{}]\n{}", title_of(doc), t)
            } else {
                t.clone()
            }
        })
        .collect();
    let vectors = embedder
        .embed_batch(&texts)
        .await
        .map_err(|e| anyhow::anyhow!("failed to embed chunks: {e}"))?;

    let points: Vec<PointStruct> = chunks
        .iter()
        .zip(texts.iter())
        .zip(vectors)
        .map(|(((doc, _raw), embedded), vector)| {
            PointStruct::new(
                uuid::Uuid::new_v4().to_string(),
                vector,
                [
                    ("text", embedded.clone().into()),
                    ("document", doc.clone().into()),
                    ("tenant_id", TENANT.into()),
                ],
            )
        })
        .collect();

    qdrant
        .upsert_points(UpsertPointsBuilder::new(COLLECTION, points).wait(true))
        .await
        .context("failed to upsert bench points")?;

    let mut hits_at_1 = 0usize;
    let mut hits_at_3 = 0usize;
    let mut hits_at_10 = 0usize;
    let mut ctx_chars_total = 0usize;
    let mut reciprocal_sum = 0f32;
    let mut conflict_hits = 0usize;
    let mut conflict_total = 0usize;
    let mut crosslingual_hits = 0usize;
    let mut crosslingual_total = 0usize;
    let mut misses = Vec::new();

    for (i, question) in golden.questions.iter().enumerate() {
        let vector = if variant.random_query {
            noise_vector(i as u64)
        } else {
            query_vectors[i].clone()
        };

        let response = qdrant
            .query(
                QueryPointsBuilder::new(COLLECTION)
                    .query(vector)
                    .limit(FETCH)
                    .filter(Filter::must([Condition::matches(
                        "tenant_id",
                        TENANT.to_string(),
                    )]))
                    .with_payload(true),
            )
            .await
            .context("bench query failed")?;

        // Mirrors production exactly: fetch, then filter by the floor. The floor SHRINKS the result
        // set rather than digging deeper — that is the defect D6 exists to fix, and the bench must
        // reproduce it or the baseline flatters the system.
        let scored: Vec<(String, String)> = response
            .result
            .into_iter()
            .filter(|p| p.score >= variant.threshold)
            .filter_map(|p| {
                let text = match p.payload.get("text")?.kind.as_ref()? {
                    qdrant_client::qdrant::value::Kind::StringValue(s) => s.clone(),
                    _ => return None,
                };
                let doc = match p.payload.get("document").and_then(|v| v.kind.as_ref()) {
                    Some(qdrant_client::qdrant::value::Kind::StringValue(s)) => s.clone(),
                    _ => String::new(),
                };
                Some((doc, text))
            })
            .collect();

        // D14: bound how much of the context one document may occupy. Applied AFTER the floor and
        // over the over-fetched set, so a capped slot is refilled from deeper rather than lost —
        // which is the same defect D6 fixes for the threshold.
        let mut passages: Vec<String> = Vec::new();
        if let Some(cap) = variant.per_doc_cap {
            let mut seen: HashMap<String, usize> = HashMap::new();
            for (doc, text) in scored {
                let n = seen.entry(doc).or_insert(0);
                if *n < cap {
                    *n += 1;
                    passages.push(text);
                }
            }
        } else {
            passages = scored.into_iter().map(|(_, t)| t).collect();
        }

        if variant.reverse_rank {
            passages.reverse();
        }

        // What the model would actually be handed at production's default limit of 3.
        ctx_chars_total += passages
            .iter()
            .take(3)
            .map(|p| p.chars().count())
            .sum::<usize>();

        let rank = passages
            .iter()
            .position(|p| contains_normalised(p, &question.expects));

        let is_conflict = question.tags.iter().any(|t| t == "conflict");
        let is_crosslingual = question.tags.iter().any(|t| t == "crosslingual");
        if is_conflict {
            conflict_total += 1;
        }
        if is_crosslingual {
            crosslingual_total += 1;
        }

        match rank {
            Some(r) => {
                if r == 0 {
                    hits_at_1 += 1;
                }
                if r < 3 {
                    hits_at_3 += 1;
                    if is_conflict {
                        conflict_hits += 1;
                    }
                    if is_crosslingual {
                        crosslingual_hits += 1;
                    }
                }
                hits_at_10 += 1;
                reciprocal_sum += 1.0 / (r as f32 + 1.0);
            }
            None => misses.push(question.q.clone()),
        }
    }

    let n = golden.questions.len() as f32;
    Ok(Scores {
        recall_at_1: hits_at_1 as f32 / n,
        recall_at_3: hits_at_3 as f32 / n,
        recall_at_10: hits_at_10 as f32 / n,
        mrr: reciprocal_sum / n,
        ctx_chars_at_3: ctx_chars_total as f32 / n,
        conflict_hits,
        conflict_total,
        crosslingual_hits,
        crosslingual_total,
        misses,
    })
}

/// Case-insensitive, whitespace-collapsed containment.
///
/// A chunk boundary or a wrapped line turns `"3 to 5 business days"` into `"3 to 5\nbusiness days"`,
/// and a naive `contains` would score that a miss — punishing the chunker for the corpus's line
/// wrapping rather than for anything about retrieval.
fn contains_normalised(haystack: &str, needle: &str) -> bool {
    fn norm(s: &str) -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }
    norm(haystack).contains(&norm(needle))
}

/// A deterministic unit vector with no relationship to any document. Seeded so the sabotage is
/// reproducible rather than differently-wrong on every run.
fn noise_vector(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z as i64 as f64 / i64::MAX as f64) as f32
    };
    let mut v: Vec<f32> = (0..EMBEDDING_DIM).map(|_| next()).collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn report(results: &[(Variant, Scores)]) {
    println!("| variant | recall@1 | recall@3 | recall@10 | MRR | ctx chars@3 |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for (v, s) in results {
        println!(
            "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.0} |",
            v.name, s.recall_at_1, s.recall_at_3, s.recall_at_10, s.mrr, s.ctx_chars_at_3
        );
    }

    let (baseline_variant, baseline) = &results[0];
    println!("\nbaseline detail — {}", baseline_variant.name);
    println!(
        "  conflict cases (superseded policy): {}/{} retrieved the CURRENT policy in the top 3",
        baseline.conflict_hits, baseline.conflict_total
    );
    println!(
        "  cross-lingual cases: {}/{} in the top 3",
        baseline.crosslingual_hits, baseline.crosslingual_total
    );
    if baseline.misses.is_empty() {
        println!("  no misses — if this holds, the golden set is too easy to detect a regression");
    } else {
        println!("  missed entirely (not in the top {FETCH}):");
        for m in &baseline.misses {
            println!("    - {m}");
        }
    }

    // The sabotage check, asserted rather than eyeballed. A metric that does not move when the
    // system is deliberately broken is decoration, and a decorative metric is worse than none: it
    // is a number that will be believed.
    //
    // Candidates are NOT sabotages: they are supposed to score better, so running this assertion
    // over them would report every improvement as a failure.
    let sabotages_only = results[1..]
        .iter()
        .all(|(v, _)| v.name.starts_with("SABOTAGE"));
    if results.len() > 1 && !sabotages_only {
        println!(
            "\ncandidate comparison — baseline recall@1 {:.3}, MRR {:.3}, ctx {:.0}",
            baseline.recall_at_1, baseline.mrr, baseline.ctx_chars_at_3
        );
        let mut ranked: Vec<&(Variant, Scores)> = results[1..].iter().collect();
        ranked.sort_by(|a, b| b.1.recall_at_1.partial_cmp(&a.1.recall_at_1).unwrap());
        for (v, s) in ranked.iter().take(3) {
            println!(
                "  best: {} — recall@1 {:+.3}, MRR {:+.3}, ctx {:+.0}",
                v.name,
                s.recall_at_1 - baseline.recall_at_1,
                s.mrr - baseline.mrr,
                s.ctx_chars_at_3 - baseline.ctx_chars_at_3
            );
        }
        return;
    }
    if results.len() > 1 {
        println!(
            "\nsabotage verification (a metric proves nothing until you have watched it drop):"
        );
        let mut all_ok = true;
        for (v, s) in &results[1..] {
            let (ok, why): (bool, String) = if v.chunk_size > CHUNK_SIZE {
                // Recall CANNOT see this one — a whole-document chunk always contains the answer, so
                // recall stays 1.000 and MRR even improves. Context cost is the metric that sees it.
                (
                    s.ctx_chars_at_3 > baseline.ctx_chars_at_3 * 1.5,
                    format!(
                        "ctx chars@3 {:.0} -> {:.0} (recall is blind here: {:.3} -> {:.3})",
                        baseline.ctx_chars_at_3,
                        s.ctx_chars_at_3,
                        baseline.recall_at_3,
                        s.recall_at_3
                    ),
                )
            } else if v.reverse_rank {
                (
                    s.mrr < baseline.mrr && (s.recall_at_10 - baseline.recall_at_10).abs() < 0.001,
                    format!(
                        "MRR {:.3} -> {:.3} while recall@10 held at {:.3}",
                        baseline.mrr, s.mrr, s.recall_at_10
                    ),
                )
            } else {
                (
                    s.recall_at_3 < baseline.recall_at_3,
                    format!(
                        "recall@3 {:.3} -> {:.3}",
                        baseline.recall_at_3, s.recall_at_3
                    ),
                )
            };
            all_ok &= ok;
            println!(
                "  [{}] {} — {}",
                if ok { "PASS" } else { "FAIL" },
                v.name,
                why
            );
        }
        println!(
            "\n{}",
            if all_ok {
                "All sabotages moved the metric they should. The meter is trustworthy."
            } else {
                "A sabotage left its metric unmoved. THAT METRIC IS DECORATION — fix the eval \
                 before trusting any baseline it reports."
            }
        );
        if !all_ok {
            std::process::exit(1);
        }
    }
}
