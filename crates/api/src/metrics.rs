//! Counters and gauges, rendered as Prometheus text.
//!
//! **What this exists to catch.** Almost everything that goes wrong in this system goes wrong
//! *quietly*: a refusal is a `200`, a half-re-indexed collection answers nothing while looking
//! healthy, a dead worker leaves rows that simply stop moving. `/health` says five dependencies
//! answer, which is true right up until none of it works.
//!
//! **No tenant labels. Ever.** See invariant 30. A time series labelled with a tenant id is a store
//! the erasure saga cannot reach — Prometheus is *designed* not to be deleted from (the admin API is
//! off by default, deletes are tombstones until compaction, and a remote-write copy may have no
//! delete path you control at all). `DELETE /admin/tenants/{id}` returns counts as evidence; it
//! would be returning evidence for three stores while silently omitting a fourth. The rule that
//! enforces this mechanically: **every label value is a variant of a closed enum or a `const`,
//! never a runtime string.** That one rule also keeps invariant 16 out of the label space (an
//! error body as a label is invariant 16 inverted *and* a cardinality bomb) and keeps cardinality
//! bounded.
//!
//! "Which tenant?" is answered by `GET /admin/ops/tenants`, live from Postgres — always current,
//! retaining nothing, and gone the moment the tenant is.
//!
//! **No dependency, and a stated stopping point.** Eleven numbers do not justify a metrics crate,
//! a global recorder and a registry. But the day someone wants latency percentiles, take the
//! `metrics` crate rather than extending this: a hand-rolled histogram with correct `le`/`_bucket`
//! semantics is where thirty lines becomes a hundred and fifty, and where being subtly wrong
//! produces plausible-looking numbers — this system's characteristic failure mode.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-lifetime counters. Reset on restart, which Prometheus handles natively; with several
/// replicas you `sum()`.
#[derive(Default)]
pub struct Metrics {
    /// Every answered question, refusals included.
    pub ask_total: AtomicU64,
    /// The subset that refused. **The canary**: invariant 4 refuses when nothing clears the floor,
    /// and a refusal is a `200` with `ok: true` — invisible to every status-code metric, every
    /// error rate and every log-level filter. The ratio is the only production signal that
    /// retrieval has quietly stopped working.
    pub ask_refused_total: AtomicU64,
    /// Passages returned across answered asks. Average depth falling is the earlier warning — it
    /// moves before the floor starts refusing outright.
    pub ask_sources_total: AtomicU64,
    /// Question-embedding outcomes on the ask/search path. `outcome` comes from
    /// `EmbedError::is_fatal`, so it cannot drift from the worker's own retry decision.
    pub embed_ok: AtomicU64,
    pub embed_fatal: AtomicU64,
    pub embed_retryable: AtomicU64,
    pub llm_ok: AtomicU64,
    pub llm_error: AtomicU64,
    /// 429s from `rate_limit::check`. Rising alongside `ask_total` is the shape of a tenant pinning
    /// their own bucket — the *aggregate* half of the spend question.
    pub rate_limited_total: AtomicU64,
}

impl Metrics {
    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }
    fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// Values read fresh on each scrape rather than tracked. Everything here is an aggregate over the
/// whole fleet; none of it can name a tenant.
#[derive(Default)]
pub struct Gauges {
    /// `(status, count)` over every tenant — see the `SECURITY DEFINER` note in migration 0014.
    pub documents: Vec<(String, i64)>,
    /// `(kind, count)` of rows the reaper should have settled and has not.
    pub overdue: Vec<(String, i64)>,
    pub tenants: i64,
    /// `(queue, messages, consumers)`. **Absent rather than zero** when the broker could not be
    /// asked: a `0` that means "I could not ask" is indistinguishable from a dead worker, and would
    /// page someone on a broker hiccup.
    pub queues: Vec<(String, u64, u64)>,
}

/// Escape a label value per the exposition format: backslash, double quote, newline.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

pub fn render(m: &Metrics, g: &Gauges) -> String {
    let mut s = String::with_capacity(2048);

    let mut counter = |name: &str, help: &str, value: u64| {
        let _ = writeln!(s, "# HELP {name} {help}");
        let _ = writeln!(s, "# TYPE {name} counter");
        let _ = writeln!(s, "{name} {value}");
    };
    counter(
        "botflow_ask_total",
        "Questions answered, including refusals.",
        Metrics::get(&m.ask_total),
    );
    counter(
        "botflow_ask_refused_total",
        "Questions refused because nothing cleared the relevance floor (invariant 4). A refusal is a 200, so this is its only signal.",
        Metrics::get(&m.ask_refused_total),
    );
    counter(
        "botflow_ask_sources_total",
        "Passages returned across answered questions.",
        Metrics::get(&m.ask_sources_total),
    );
    counter(
        "botflow_rate_limited_total",
        "Requests refused by the per-tenant rate limit.",
        Metrics::get(&m.rate_limited_total),
    );

    let _ = writeln!(
        s,
        "# HELP botflow_embed_requests_total Question-embedding calls by outcome."
    );
    let _ = writeln!(s, "# TYPE botflow_embed_requests_total counter");
    for (outcome, v) in [
        ("ok", Metrics::get(&m.embed_ok)),
        ("fatal", Metrics::get(&m.embed_fatal)),
        ("retryable", Metrics::get(&m.embed_retryable)),
    ] {
        let _ = writeln!(
            s,
            "botflow_embed_requests_total{{outcome=\"{outcome}\"}} {v}"
        );
    }

    let _ = writeln!(
        s,
        "# HELP botflow_llm_requests_total Chat-completion calls by outcome."
    );
    let _ = writeln!(s, "# TYPE botflow_llm_requests_total counter");
    for (outcome, v) in [
        ("ok", Metrics::get(&m.llm_ok)),
        ("error", Metrics::get(&m.llm_error)),
    ] {
        let _ = writeln!(s, "botflow_llm_requests_total{{outcome=\"{outcome}\"}} {v}");
    }

    let _ = writeln!(
        s,
        "# HELP botflow_documents Documents by status, across all tenants."
    );
    let _ = writeln!(s, "# TYPE botflow_documents gauge");
    for (status, n) in &g.documents {
        let _ = writeln!(s, "botflow_documents{{status=\"{}\"}} {n}", escape(status));
    }

    let _ = writeln!(
        s,
        "# HELP botflow_documents_overdue Rows the reaper should have settled and has not. Persistently non-zero means the reaper is dead or erroring."
    );
    let _ = writeln!(s, "# TYPE botflow_documents_overdue gauge");
    for (kind, n) in &g.overdue {
        let _ = writeln!(
            s,
            "botflow_documents_overdue{{kind=\"{}\"}} {n}",
            escape(kind)
        );
    }

    let _ = writeln!(s, "# HELP botflow_tenants Tenants on this deployment.");
    let _ = writeln!(s, "# TYPE botflow_tenants gauge");
    let _ = writeln!(s, "botflow_tenants {}", g.tenants);

    if !g.queues.is_empty() {
        let _ = writeln!(
            s,
            "# HELP botflow_queue_messages Messages ready on a queue. The DLQ being non-empty means a document is permanently unindexed."
        );
        let _ = writeln!(s, "# TYPE botflow_queue_messages gauge");
        for (q, msgs, _) in &g.queues {
            let _ = writeln!(
                s,
                "botflow_queue_messages{{queue=\"{}\"}} {msgs}",
                escape(q)
            );
        }
        let _ = writeln!(
            s,
            "# HELP botflow_queue_consumers Consumers registered on a queue. Zero on document_events IS worker death, reported by the broker rather than asserted by the worker."
        );
        let _ = writeln!(s, "# TYPE botflow_queue_consumers gauge");
        for (q, _, consumers) in &g.queues {
            let _ = writeln!(
                s,
                "botflow_queue_consumers{{queue=\"{}\"}} {consumers}",
                escape(q)
            );
        }
    }

    let _ = writeln!(
        s,
        "# HELP botflow_build_info Build metadata; value is always 1."
    );
    let _ = writeln!(s, "# TYPE botflow_build_info gauge");
    let _ = writeln!(
        s,
        "botflow_build_info{{version=\"{}\",collection=\"{}\"}} 1",
        escape(env!("CARGO_PKG_VERSION")),
        escape(common::COLLECTION)
    );

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_are_escaped() {
        // Not reachable from the closed enums we actually emit — which is the point of invariant
        // 30's rule. Escaped anyway, because the day someone adds a label from a runtime string is
        // the day a stray quote breaks every scrape silently.
        assert_eq!(escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape(r"a\b"), r"a\\b");
        assert_eq!(escape("a\nb"), r"a\nb");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn exposition_has_help_type_and_value_for_every_counter() {
        let m = Metrics::default();
        let out = render(&m, &Gauges::default());
        for name in [
            "botflow_ask_total",
            "botflow_ask_refused_total",
            "botflow_embed_requests_total",
            "botflow_llm_requests_total",
            "botflow_tenants",
            "botflow_build_info",
        ] {
            assert!(
                out.contains(&format!("# HELP {name} ")),
                "missing HELP for {name}"
            );
            assert!(
                out.contains(&format!("# TYPE {name} ")),
                "missing TYPE for {name}"
            );
        }
    }

    #[test]
    fn counters_render_their_values() {
        let m = Metrics::default();
        Metrics::incr(&m.ask_total);
        Metrics::incr(&m.ask_total);
        Metrics::incr(&m.ask_refused_total);
        Metrics::add(&m.ask_sources_total, 5);
        let out = render(&m, &Gauges::default());
        assert!(out.contains("\nbotflow_ask_total 2\n"));
        assert!(out.contains("\nbotflow_ask_refused_total 1\n"));
        assert!(out.contains("\nbotflow_ask_sources_total 5\n"));
    }

    #[test]
    fn gauges_render_one_series_per_label_value() {
        let g = Gauges {
            documents: vec![("ready".into(), 3), ("failed".into(), 1)],
            overdue: vec![("stuck_processing".into(), 2)],
            tenants: 7,
            queues: vec![("document_events".into(), 0, 1)],
        };
        let out = render(&Metrics::default(), &g);
        assert!(out.contains(r#"botflow_documents{status="ready"} 3"#));
        assert!(out.contains(r#"botflow_documents{status="failed"} 1"#));
        assert!(out.contains(r#"botflow_documents_overdue{kind="stuck_processing"} 2"#));
        assert!(out.contains("botflow_tenants 7"));
        assert!(out.contains(r#"botflow_queue_consumers{queue="document_events"} 1"#));
    }

    /// A `0` that means "the broker did not answer" is indistinguishable from a dead worker, and
    /// would page someone on a hiccup. `absent()` in the alert rule handles the missing case.
    #[test]
    fn queue_series_are_omitted_entirely_when_the_broker_could_not_be_asked() {
        let out = render(&Metrics::default(), &Gauges::default());
        assert!(!out.contains("botflow_queue_messages"));
        assert!(!out.contains("botflow_queue_consumers"));
    }

    /// The invariant, asserted on the rendered output rather than trusted to review.
    #[test]
    fn no_series_carries_a_tenant_label() {
        let g = Gauges {
            documents: vec![("ready".into(), 1)],
            overdue: vec![("stuck_processing".into(), 1)],
            tenants: 1,
            queues: vec![("document_events".into(), 1, 1)],
        };
        let out = render(&Metrics::default(), &g);
        assert!(
            !out.contains("tenant=\""),
            "invariant 30: a tenant label is a store the erasure saga cannot reach"
        );
    }
}
