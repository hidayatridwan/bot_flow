# Feature: Retrieval quality — an irreversible change to the one thing nothing measures (phase 10)

> Status: **design for review. No code.**
>
> This is the phase the last one deferred. Phase 9's header named the fork out loud — *"the other
> candidate for phase 9 is retrieval quality (chunking + hybrid search), which is the bigger *product*
> win"* — and argued for the harness first, because it is better to be able to **prove** isolation
> still holds before rewriting the ingest path underneath it. That argument was accepted, the harness
> was built, and the debt it bought time for is now due.

## Context — why

**Answer quality is the product's core promise, and it is the only property in the system with no
instrument at all.** CLAUDE.md's own inventory of what phase 9 covered ends on exactly this: *"Not
covered, and worth knowing precisely: `/ask/stream`; retrieval **quality**, which these phases say
nothing about."* That sentence is the brief.

It is not merely untested — it is *unmeasured*. The only quality instrument in the repo is a
`tracing::info!` of raw cosine scores in `retrieve` (`handlers.rs:549`), and README says plainly whose
job it is to read it: *"Set it from your own data: ingest a document, ask a question you know it
answers, read the logged retrieval scores, and put the floor just below them."* The instrument is a
human squinting at a log line.

Three symptoms, each verified, each downstream of that:

- **The relevance floor has three live values and no owner.** `0.70` compiled into `config.rs`, `0.35`
  recommended by README's `.env` block, `0.20` actually set. Phase 9 recorded what `0.20` rests on:
  *"fitted to a three-chunk corpus with a 0.22-vs-0.13 margin."* A 0.09 separation between signal and
  noise, on three chunks, is not a tuned parameter — it is a coin landing on its edge. And **invariant
  4** — the rule that an admission of ignorance beats a hallucinated refund policy — is enforced by
  precisely that number.
- **Chunks are cut mid-word.** `chunk_text` is a fixed-width sliding window over `Vec<char>`, and
  `800, 100` is hard-coded at its single call site in `worker/main.rs` — not even a named constant.
  There is no boundary awareness of any kind: a cut lands wherever character 800 falls. Its entire
  test suite is `assert_eq!(chunk_text("abcdefghij", 4, 1), vec!["abcd", "defg", "ghij"])`.
- **A passage cannot say where it came from.** The Qdrant payload has exactly three keys — `text`,
  `tenant_id`, `document_id`. No `chunk_index`, no offsets, no filename. The context handed to the
  model is `format!("[{}] {}", i + 1, h.text)`, so the model could not name a source even if the
  prompt asked it to. `chunk_index` exists only *inside* the UUIDv5 point id, and a hash does not
  invert — so adjacent-chunk merging, positional ordering and page citation are not *unimplemented*,
  they are **impossible on the current index**.

## The trap that decides the whole design

**Every improvement in this phase requires re-indexing every vector in the system, and the system has
no way to tell whether the re-index helped.**

Those are two known problems. The trap is what they become together.

CLAUDE.md is unambiguous about the first half: *"Vector storage has no migration path. Changing the
embedding model, its dimension, or **the chunking parameters** invalidates every stored vector, and
there is no rollback… A **partially** re-indexed collection produces quietly degraded retrieval with no
error anywhere. Any such change is a migration project, not a configuration change."*

So the change is irreversible, and the thing it changes is the thing with no meter. Ship a new chunker,
watch a handful of questions still answer plausibly, and you have learned nothing — *plausible is what
this system produces when retrieval is wrong.* That is the entire reason invariant 4 exists.

**The second half of the trap: the harness we just built cannot help, by construction.** Phase 9's fake
gateway uses a deterministic content-addressed embedder — the same string scores ~1.0, a different one
~0.0. That was the right call for what it was for; it is what makes "tenant B retrieved nothing" mean
*the filter worked* rather than *nothing cleared the floor*. But under it **semantic similarity does not
exist**: a better chunker and a worse chunker score identically, because only byte-identity is visible.
Worse, a chunking change *breaks* any golden assertion written against it, since the chunk strings
themselves move.

The harness cannot be extended into an evaluator. **A second instrument is required, and it must exist
and produce a recorded baseline before a single line of `chunk.rs` changes.** Otherwise "better
retrieval" is an opinion about an unrollbackable change.

Two corollaries fall straight out, and both shape the phase:

1. **Everything requiring a re-index must ship in the same re-index.** The migration is the expensive
   part; paying it twice — once for chunking, again for hybrid — is the one avoidable mistake here.
2. **`/ingest`-only tenants cannot be migrated at all.** Those points carry random ids and no
   `document_id` — CLAUDE.md: *"they can never be listed, re-indexed or removed. They are permanent."*
   At cutover they are not re-indexed, they are **abandoned**. The largest single piece of debt in the
   system presents its bill in this phase, and the honest answer is that we pay it rather than fix it.

## A worked example: the superseded policy

Everything above argues about quality in the abstract. This is what it looks like as behaviour, and it
is the failure a tenant will actually hit.

**The scenario.** A tenant uploads `handbook_v1.pdf` — *"Refunds are accepted within 14 days"* — and
later uploads `handbook_v2.pdf`, which supersedes it: *"Refunds are accepted within 30 days."* Nothing
is deleted, because uploading a correction feels like making a correction.

**Observed, on a minimal two-chunk reproduction of exactly this shape** (a live stack, an `/ingest` of
two contradictory one-line facts, the older one first — the numbers below are that run, not the
handbook wording):

- **Ranking is semantic, never temporal.** For a neutral question the **older** chunk won, `0.747` to
  `0.597`, purely because its wording sat closer to the question's. Insertion order contributed
  nothing. There is no timestamp in the payload to rank by — `text`, `tenant_id`, `document_id` is the
  whole of it — so recency is not deprioritised, it is *absent*.
- **The system merges the contradiction.** Both chunks clear the floor, both land in the CONTEXT block
  as `[1]` and `[2]`, and the model reconciles them however it sees fit. A neutral question produced a
  both-things-are-true summary. **A leading question confirmed whichever premise it was handed** — the
  matching chunk jumped to ~`0.91` and the model agreed, so the same corpus answered *yes* to two
  mutually exclusive questions, confidently, with no hedge.
- **Invariant 4 held perfectly, and that is the uncomfortable part.** Nothing was hallucinated. The
  model reported its context faithfully; the context genuinely contained both policies. Every rule in
  this system worked as written, and the tenant still gets told the wrong refund window — which is
  precisely the outcome invariant 4 exists to prevent, arrived at by a route it does not cover.

**Diagnosis.** The system has no notion of document *hygiene*. It treats every indexed chunk as an
undifferentiated mass of truth, with no way to express that one document supersedes another and no
logic to arbitrate when two disagree. Today the only defence is operational: delete the stale document
(`DELETE /documents/{id}`, phase 8). That works, it is supported, and it is *easily forgotten* — which
makes it a defence in the same sense that "remember to run the migration" is a defence.

Note also how this compounds with D6: on a two-chunk corpus both claims are retrieved and the conflict
is at least *visible* in `sources`. At a realistic corpus size with `limit = 3`, whether the current
policy appears at all becomes a matter of ranking luck — and the failure stops being "the bot said
both" and becomes "the bot confidently stated the expired policy, alone."

## Decisions to settle (open — recommendations given)

### D1 — what is the meter, and what may its ground truth key on?

The subtle part is not the metric, it is **what ground truth is allowed to reference**. The obvious
encoding — "question Q should retrieve chunk #4" — is destroyed by the very change it exists to
evaluate, because re-chunking renumbers and rewrites every chunk. Ground truth must be
**chunk-independent**: each golden question names a short *substring* a correct passage must contain (a
fact, a figure, a clause). Recall@k is then "did any returned passage contain it", which is stable
across every chunking strategy we might ever compare.

**Recommendation: a checked-in golden set of ~30 questions over a small committed fixture corpus,
scored by recall@3, recall@10 and MRR**, run against real embeddings by an `#[ignore]`d test or a small
binary. Reject an LLM-judge as the *primary* meter: it grades retrieval and generation jointly, so a
chunking regression can hide behind a competent model, and it is nondeterministic on the exact axis
that needs stability. It belongs to a later answer-quality phase.

**Risk, stated plainly:** I author the golden set, so it encodes what I already believe retrieval
should return. Mitigation is weak but real — derive the questions from the corpus *before* looking at
any retrieval output, and keep the corpus multilingual, because invariant 5's language rule makes
cross-lingual retrieval a supported case and that is where a naive chunker fails hardest.

### D2 — where does the meter run? (This is phase 9's boundary, and it holds.)

Real embeddings cost money and need `EMBEDDING_API_KEY`. Phase 9 established that CI is free and
secret-free — its whole fake-gateway design exists for that reason.

**Recommendation: two instruments, split exactly on that line.**

- **Quality is manual.** A documented command, run deliberately, against real embeddings, cost bounded
  to tens of embed calls per run. It writes its numbers into this document.
- **The plumbing that carries quality is pinned in CI, offline.** That surface is large and free: the
  chunker is a pure function (no chunk exceeds the ceiling; no chunk splits a word; overlap is
  preserved; non-ASCII does not panic), and payload *shape* (`chunk_index` present and monotonic,
  `document_id` set) is assertable under the existing fake embedder, because it is a claim about
  structure rather than about meaning.

**Risk:** a manual gate is a gate that gets skipped. Mitigated only by the baseline table below being
part of the document a reviewer reads.

### D3 — boundary-aware chunking, and what kind of boundary

**Recommendation: recursive character splitting with a hard character ceiling** — paragraph → sentence
→ word, falling back to a hard cut only when a single word exceeds the ceiling. Reject token-based
sizing: it adds a tokenizer dependency and, worse, a *second source of truth* about a remote model's
tokenizer — a drift surface of exactly the kind invariant 6 exists to prevent, bought for a marginal
packing improvement.

**Honest limit, which must be stated or the eval will be misread:** `sidecar/parser.py` joins PDF pages
with `"\n"`, so page structure is already destroyed before chunking ever sees the text. Paragraph-aware
splitting will therefore help `md`/`txt` considerably and PDFs much less. Page-aware parsing is a
sidecar change and is deliberately **not** in this phase — but it is the prerequisite for page
citations, and whoever wants those starts there, not here.

### D4 — do size and overlap become env vars?

**Recommendation: named constants in `chunk.rs`, and never env vars.** CLAUDE.md's rule is already
written twice: invariant 28 puts the gateway bounds *"beside the code they bound… not env vars — the
same choice, for the same reason, as `MAX_TOKENS` and `EMBED_BATCH`"*.

Chunk size is a stronger case than either. Two deployments with different chunk sizes produce
collections that are not comparable and cannot be merged, with nothing erroring — which is invariant
6's argument verbatim, one layer down. An env var would make an unrollbackable data-format decision
editable by anyone with `.env` access. Moving `800, 100` out of the call site into named constants is
itself part of the fix.

### D5 — payload metadata: the enabler, and the one field that does *not* belong

Nothing downstream — merging adjacent chunks, ordering by position, citing a location — is possible
without it, and it costs a re-index we are already paying. So it rides along.

**Recommendation: add `chunk_index` and `char_start`/`char_end`; index `document_id` as a keyword
field; do *not* store `filename`.** (`created_at` is the fourth addition and gets its own decision —
D12 — because it is justified by a different argument: not an enabler for merging or ordering, but the
only thing that makes a superseded document distinguishable from a current one.)

- `chunk_index` and offsets cannot be reconstructed from anything (the point id is a hash) and cannot
  be joined from Postgres. They must be stored.
- `document_id` needs a payload index regardless of this phase: the worker's re-index delete *and*
  phase 8's deletion saga both filter on it today, unindexed, and scan.
- **`filename` is the one I recommend against.** It is a denormalised copy of a `documents` column, so
  it goes stale the moment a rename feature exists, and it duplicates tenant data into a store with no
  RLS. The handler can join `document_id → documents` under `tenant_tx` in one query per request. That
  join is also *more honest* about `/ingest` chunks: no row, no name — which is exactly what the
  playground's "Unattributed passage" already says.

### D6 — over-fetch before filtering, and cap `limit`

Two defects, one fix. The floor is applied **after** the limit, client-side, so filtering *shrinks* the
result set instead of digging deeper: ask for 3, have one fall below the floor, get 2 — when a
perfectly good 4th was one rank away. And `limit: u64` comes straight off the request body with no
maximum.

**Recommendation: fetch `max(limit * 4, limit + 8)`, apply the floor, keep the top `limit`; cap `limit`
at 20 and 422 above it.** Reject Qdrant's native `score_threshold` despite it being the tidier call:
10b's fused scores are not cosines (D8), and keeping the floor in one place in our own code means the
grounding decision does not change shape when fusion lands.

### D7 — hybrid: why the ingest half ships in 10 and the query half does not

What exists, verified against the pinned versions rather than assumed: `qdrant/qdrant:v1.18.0` and
`qdrant-client 1.18.0` are already what this repo runs, and the client exposes `SparseVectorConfig`,
`sparse_vectors_config` on `CreateCollection`, `Fusion::{Rrf, Dbsf}`, and — checked in the vendored
source — `Modifier::Idf` on `SparseVectorParams`. The code already uses the modern `QueryPointsBuilder`
Query API, so prefetch-plus-fusion is **additive, not a client migration**. The missing piece is
entirely server-side: nothing produces sparse vectors.

On producing them, be concrete about cost. A SPLADE-class learned sparse model means a new inference
dependency (ONNX in the worker) or a new service. Qdrant's own BM25/miniCOIL inference is *not*
available in a plain self-hosted `qdrant/qdrant` container. What is free: **a term-frequency encoder in
`common/`, with `Modifier::Idf` set on the sparse vector config so Qdrant computes IDF server-side at
query time.** That detail is what makes the cheap option *correct* rather than merely cheap — a
client-side IDF would need corpus statistics that change on every ingest, and would silently drift.

**Recommendation: write sparse vectors during phase 10's re-index; do not query them until 10b.**

This is the structural idea of the phase. The write is cheap, deterministic and idempotent; the query
side is a flag. Splitting it this way buys the two things that otherwise conflict: **one migration**
(corollary 1), and **one variable per measurement** — phase 10's delta is attributable to chunking
alone, 10b's to fusion alone, with no second re-index between them.

### D8 — after fusion, the floor would be comparing a similarity to a rank artifact

The consequence most likely to be missed, so it gets its own decision. An RRF score is `Σ 1/(k + rank)`
— for `k=60` it lives around 0.016–0.03 and has **no relationship to semantic similarity whatsoever**.
Compare `0.20` against it and every answer is refused, forever, silently — invariant 4 behaving exactly
as designed while being completely wrong. It is the README's `0.70` story again, one abstraction up.

**Recommendation: the grounding floor stays on the dense leg.** Apply it to the cosine prefetch before
fusion; let the sparse leg contribute recall but never the decision to answer. The floor keeps its
units, its tuned value survives fusion, and invariant 4 keeps meaning what it says.

And in phase 10, independently: **collapse the three live threshold values into one.** The compiled
default becomes whatever the baseline supports, README stops recommending a third number, and the note
explaining the E5-era `0.70` becomes history rather than live advice.

### D9 — cutover: a versioned collection, because it is the only option with a rollback

`ensure_collection` early-returns when the collection exists. So a new HNSW config, a sparse vector
config, anything — **silently does not take effect** on a live deployment. README already documents the
manual dance from the E5 cutover: drop the collection, then `TRUNCATE messages, conversations,
documents` as the superuser, because invariant 10 skips a redelivered document whose fingerprint is
unchanged and the collection would otherwise stay empty forever with no error.

**Recommendation: version the collection — `documents_v2`.** It is the only option that gives this
system something it has never had: a rollback. The old collection stays intact and queryable while the
new one fills; cutover is a constant change and back-out is the same constant. It also makes the
"partially re-indexed collection degrades quietly" hazard *visible* — the new collection is provably
empty until re-indexing runs, rather than half-right and silent. Reject in-place mutation outright: it
cannot add sparse vectors to existing points anyway, so it buys nothing and hides the early-return trap.

This needs a **re-index driver**, which does not exist: nothing today re-indexes a document from its
stored object. The bytes are in MinIO and the row has `object_key`, so a worker subcommand can
republish an ingest job per document, **looping tenant by tenant** (a bulk `UPDATE` under RLS matches
zero rows and reports success). The trap to design against up front: **invariant 10's fingerprint skip
will make the entire migration a silent no-op** unless the driver deliberately defeats it. That is the
exact failure README's cutover section had to document by hand; this time it should be a property of
the tool rather than a paragraph of instructions.

### D10 — reranking, and why it is not here

A cross-encoder needs a model server (new infra) or an LLM call per query — a second serial round-trip
on a path that already pays one for the conversation rewrite.

**Recommendation: out of scope, and the eval says when it stops being.** A reranker only reorders what
retrieval already found, so it pays nothing while recall is poor. The signal to build one is
specifically **high recall@10 with weak recall@3** — a gap the golden set measures directly. Deferring
it on a number rather than on a feeling is the point.

### D11 — does `/search` adopt the floor?

`/search` applies no floor at all today, diverging from `/ask`. The reflex is to converge them.

**Recommendation: no — keep the divergence, and document it as deliberate.** README's tuning procedure
*is* `/search`: read the raw scores and put the floor just below them. Applying the floor there
destroys the only instrument for choosing the floor. It is a management-gated diagnostic route, not an
end-user surface. Make it legible instead: return the configured threshold alongside the hits, so a
caller can see which hits `/ask` would have dropped.

### D12 — does `created_at` ride along in the payload?

The worked example above is not a retrieval-ranking bug; it is a missing *property*. The system cannot
prefer the newer of two contradictory passages, cannot tie-break on age, and cannot tell a user that
its sources disagree — not because the logic is unwritten, but because **the fact needed to write it is
not stored**. `documents` has the row's `created_at` in Postgres; the vector knows nothing about it.

**Recommendation: write `created_at` into the payload during this phase's re-index, derived from the
`documents` row, and use it for nothing yet.**

The case rests on cost and timing rather than on the feature. Adding it later is a *second* full
migration — the expensive, irreversible thing corollary 1 exists to avoid — while adding it now costs
one more payload key on a re-index already being paid for. That asymmetry is the entire argument, and
it is the same argument that puts `chunk_index` in D5.

What it buys is optionality, stated honestly: **it converts recency from an absent property into an
expressible one.** Temporal tie-breaking, a recency boost, a "these sources disagree and one is newer"
signal in the UI, or a retention policy — none are built here, and any of them can be built afterwards
without touching the index. Without it, all four require another migration before they can even be
prototyped.

**Why not use it in phase 10's ranking too?** Because that is a scoring change, and D7's whole
structure exists to keep one variable per measurement. A recency boost mixed into the same release as
the chunker would make the baseline delta unattributable — and worse, recency is not obviously
desirable: an older FAQ entry is not less true than a recently uploaded invoice. *Newer wins* is a
policy decision that deserves its own evidence, and storing the field is what makes gathering that
evidence possible.

**Risk if this is dropped:** the system stays structurally incapable of arbitrating superseded
documents, and manual deletion remains the sole defence — an operational discipline, not a property of
the product, and one that fails silently and in the tenant's favour exactly never.

**Consistency note:** `/ingest` chunks have no `documents` row and therefore no `created_at`. The field
is absent there, like `document_id` — the same debt surfacing in the same place, and one more reason
that path is not a supported one.

## Verification

Phase 9's principle was *"a passing test proves nothing until you have watched it fail."* The analogue
here is sharper, because the output is a number rather than a boolean:

**A metric proves nothing until you have watched it go down.**

A green suite that asserts nothing is a familiar failure. A metric that reports `0.83` no matter what
the system does is the same failure wearing a decimal point, and it is far more convincing. So the eval
is not trusted until it has been **sabotaged** — the mirror of phase 9's break table, run *before* the
baseline is believed:

| Sabotage | Metric that must drop, measurably |
| --- | --- |
| `chunk_size = 40` — chunks too small to contain any answer | recall@3 collapses |
| `chunk_size = 8000` — one chunk per document, no locality | MRR flattens toward chance |
| Return results in reverse rank order | MRR drops; recall@10 must **not** move — proving the two measure different things |
| `RAG_SCORE_THRESHOLD = 0.9` | recall@3 → ~0, reproducing README's `0.70` bug as a number |
| Replace the query embedding with a random vector | every metric → floor. If anything survives, the golden set is matching on something other than retrieval |

If a sabotage leaves a number unmoved, that metric is decoration and the eval is wrong before the
chunker is touched.

**Then the baseline, recorded here before any change ships**, and re-measured after:

| Measurement | recall@3 | recall@10 | MRR | Notes |
| --- | --- | --- | --- | --- |
| Baseline — 800/100 fixed window, dense only | *TBD* | *TBD* | *TBD* | pre-change, on the fixture corpus |
| Phase 10 — boundary-aware chunking + metadata | *TBD* | *TBD* | *TBD* | one variable |
| Phase 10b — hybrid + RRF | *TBD* | *TBD* | *TBD* | same index, query-side only |

**"Better" is defined before the numbers exist, so it cannot be defined by them:** phase 10 ships only
if recall@3 improves and neither other metric regresses. **A wash is a legitimate outcome and must be
reported as one** — an irreversible migration that bought nothing is worth knowing about, and it is
exactly the result a motivated reader will be tempted to round up.

The **regression guard** is the recorded baseline plus a floor the eval asserts against, so a later
change that quietly degrades retrieval fails a command rather than a customer. It cannot live in CI
(D2), so it lives in this table and in the eval's exit code.

## Known debt & traps

| Don't | Do | Why |
| --- | --- | --- |
| Extend phase 9's harness to measure quality | Build a separate eval on real embeddings | Its embedder is content-addressed *by design* — exact match 1.0, everything else ~0.0. A better chunker and a worse one score identically, and a chunking change breaks its assertions outright |
| Key golden answers to a chunk id or chunk text | Key them to a substring the passage must contain | Re-chunking renumbers and rewrites every chunk. Ground truth tied to chunks evaporates on the change it exists to evaluate |
| Ship chunking now and hybrid later | One re-index, both payload changes; flip fusion on afterwards | The migration is the expensive, irreversible part. Paying it twice is the one avoidable mistake in this phase |
| Change chunking and restart | Treat it as a migration | `ensure_collection` early-returns on an existing collection, and invariant 10 skips unchanged fingerprints. The change silently does not happen and nothing anywhere errors |
| Compare `RAG_SCORE_THRESHOLD` to a fused score | Keep the floor on the dense leg | An RRF score is `Σ 1/(k+rank)` — ~0.02, not a cosine. The floor would refuse every answer, silently, with invariant 4 working exactly as written |
| Make chunk size an env var | Named constants in `chunk.rs` | Two deployments with different sizes produce incomparable collections and nothing errors — invariant 6's argument, one layer down |
| Take `limit` from the client unbounded | Cap it; 422 above the cap | It is `u64` straight off the request body today |
| Assume the re-index covered everything | Audit point counts per tenant, per collection | A partially re-indexed collection degrades quietly with no error — and `/ingest` points cannot be covered at all |
| Leave `created_at` out because nothing reads it yet | Write it in this re-index anyway (D12) | Adding a payload field later is a *second* full migration. The field is what makes recency expressible at all — without it, superseded-policy arbitration cannot even be prototyped |
| Assume uploading a correction supersedes the original | Delete the stale document | Nothing in the system relates two documents. A correction is an additional voice, not a replacement, and the older one can outrank it — see the worked example |

**Left standing, deliberately:** page-aware PDF parsing (a sidecar change, and the true prerequisite for
page citations); adjacent-chunk merging and MMR de-duplication (both become *possible* once
`chunk_index` and offsets exist, neither is built here); reranking (D10); LLM-judge answer scoring (D1);
query expansion, and the serial rewrite round-trip on every follow-up turn; per-tenant threshold tuning.
**And every use of `created_at`** — D12 stores it and reads it nowhere. Temporal tie-breaking, recency
boosting, a sources-disagree signal and document supersession are all deliberately unbuilt; the field
exists so that none of them costs a migration, not because any of them is decided.
And the one that is not a deferral but a loss: **`/ingest`-only tenants are abandoned at cutover**,
because their vectors carry no `document_id` and no source object, and nothing in the product can find
them.

## Invariants this creates or touches

Per *Working here* — *"A behaviour change starts here. Edit the invariant, then write the code, in the
same commit."*

- **Invariant 6 widens from the model to the whole index recipe.** It already says changing
  `EMBEDDING_MODEL` invalidates every vector, and calls it *"a correctness rule wearing the costume of a
  configuration detail."* Chunking strategy, size and overlap belong inside that sentence: a collection
  may not hold chunks cut two different ways any more than vectors from two models.
- **New invariant — a chunk carries its provenance.** Every indexed point carries `document_id`,
  `chunk_index` and `created_at`. A passage that cannot say where it came from cannot be cited, merged
  with its neighbour, or ordered — and one that cannot say *when* it came from cannot be weighed
  against a passage that contradicts it (D12). This turns `/ingest`'s missing `document_id` from a debt
  entry into a rule it visibly breaks.
- **New invariant (10b) — the grounding floor applies to a similarity, never to a fused rank.** D8.
  Invariant 4 is unchanged in *meaning* and changes entirely in *mechanism*, which is the dangerous kind
  of change.
- **Invariant 9 is applied, not changed** — but the migration leans on it hard: deterministic point ids
  make a re-index an overwrite, and the delete-before-upsert is what stops a re-chunk stranding the old
  tail when the new chunking yields fewer chunks. It will.
- **Invariant 10's fingerprint skip is the migration's principal hazard** (D9), and needs a named escape
  hatch rather than a paragraph of instructions.

## Open questions for review

1. **Is the phase 10 / 10b split right, or is it too clever?** Writing sparse vectors we do not query
   means shipping a payload we cannot yet prove is correct. The alternative — hybrid in one phase —
   costs either a second re-index or an unattributable measurement. I think the split is worth it, but
   this is the decision I am least sure of.
2. **What is the fixture corpus, and can it be committed?** It must be public-safe, multilingual, large
   enough that recall@3 is not trivially 1.0, and small enough to re-embed for pennies. Roughly 5–10
   documents. Nothing in the repo is a candidate today.
3. **Does the versioned collection name become permanent policy** — `documents_v3`, `v4` — or is it a
   one-off? Permanent gives every future embedding change a rollback this system has never had. It also
   means the collection name is forever something that can be stale in a deployment.
4. **Who runs the eval, and when?** A manual gate is a skipped gate. Is *"before merging any change to
   `chunk.rs`, `retrieve`, or the threshold"* a rule anyone will keep — or does it need to become a CI
   job with a budget and a secret, reversing phase 9's free-and-secret-free line?
5. **Does the `filename`-by-join (D5) hold under load?** It adds one RLS-scoped query per `/ask`. I
   believe correctness beats the round-trip, but the denormalised copy is defensible and I may be
   over-weighting a rename feature that does not exist.
6. **Is "newer wins" even the right policy, once `created_at` exists (D12)?** A superseded handbook
   says yes; an old FAQ entry versus a recently uploaded invoice says no. The honest answer may be that
   recency should surface a *conflict* to the user rather than silently resolve one — which is a
   product decision, not a retrieval one, and is why D12 stores the field and rules on nothing.
