# Feature: telling a tenant whether to re-upload or to wait (phase 14)

> Status: **built.** The sidecar gained an exit code, the worker gained a classifier, `documents`
> gained a column, and the dashboard gained four sentences where it had one. Closes
> [production blocker 4](../production-readiness.md), with its residues named.

## Context — why

A `failed` badge was honest and useless. `mark_failed` wrote the parser's stderr; the reaper wrote
`'processing lease expired; worker presumed dead'`. Both landed in `documents.error`, which no
endpoint exposes — correctly, because invariant 16 forbids shipping a gateway body or a stack trace
to a client. So the UI had exactly one string to work with, and it had to cover both cases at once:

> We couldn't process this file. This can happen if the file is damaged, or if something went wrong
> on our side. Try uploading it again.

Read that as someone whose worker died mid-lease. Their PDF is fine. They are being told, politely,
that it might be their fault, and being sent to re-upload a good file into a system that is currently
down. They will do it more than once.

The blocker's own closing condition named the shape of the fix: *the worker writes a classified
reason code beside the raw text, and the API exposes the code, not the text.*

## The trap: the two errors are not symmetric

This is the whole design, and it is the thing to get right.

Misclassifying **our** failure as **theirs** is much worse than the reverse. Excusing a genuinely
broken file costs one support ticket. Blaming a tenant for our outage sends them round a loop that
cannot succeed, while the real fault goes unreported — and it is *convincing*, because a specific
message reads as diagnosis. The vague copy above was at least visibly vague; a confident wrong answer
is not.

So the classifier is conservative by construction: **only two sidecar exit codes implicate the
document, and everything else is `system_error`.** There is one narrow `match` and a catch-all, and
the catch-all is the safe direction. Same rule at the wire boundary — `toFailureReason` degrades an
unrecognised reason to `null` (the old cause-agnostic copy) rather than to a guess.

## The bug found on the way: the exit-code contract was wrong in CLAUDE.md

CLAUDE.md had said, for eleven phases:

> `2` = unreadable, `3` = unsupported type

`3` was right. `2` was not, and it mattered. From `sidecar/parser.py`, `2` is *wrong argv count* — a
usage error `parser.rs` cannot trigger, since it always passes exactly one argument. A genuinely
unreadable PDF (pypdf raising on an encrypted or truncated file) was an **uncaught traceback: exit
1** — the same code as a missing `pypdf`, a syntax error, or any other way our own sidecar breaks.

Building the classifier on the documented contract would have mapped exit 1 → "your file is
damaged", which is precisely the inversion above: every deployment fault would have been reported to
tenants as their broken document. Found by running the sidecar, not by reading it.

So the sidecar gained **exit 4 = unreadable**, wrapping only the extraction call:

```python
from pypdf import PdfReader          # OUTSIDE the try — a missing pypdf is a broken
                                     # deployment (1), not a broken document (4)
try:
    reader = PdfReader(str(path))
    text = "\n".join((page.extract_text() or "") for page in reader.pages)
except Exception as e:
    sys.exit(4)
```

The import placement is the load-bearing line. Verified against the real interpreter:

| Input | Exit | Reason |
| --- | --- | --- |
| valid PDF | `0` | — |
| truncated PDF (`EOF marker not found`) | `4` | `unreadable_file` |
| non-PDF bytes named `.pdf` (`invalid pdf header`) | `4` | `unreadable_file` |
| `.xyz` | `3` | `unsupported_type` |
| **pypdf not installed** | `1` | `system_error` |
| no arguments | `2` | `system_error` |

The last two are the point: both are ours, and both are told to wait.

## What was built

**`crates/worker/src/failure.rs`** — `FailureReason`, a closed enum cut by *what the tenant should
do* rather than by what broke, plus `classify`. Everything internal collapses into `SystemError` on
purpose: a tenant cannot act on the difference between Qdrant and MinIO being down, and naming it
would leak our topology for nothing.

**`SidecarExit`** in `parser.rs` — the parser's `bail!("parser failed: {stderr}")` became a typed
error carrying the exit code, because a formatted string cannot be classified. This mirrors the
existing `EmbedError` downcast pattern rather than introducing a new one.

**Migration 0015** — `failure_reason text`, nullable, with a `CHECK`. Nullable and **not backfilled**:
nothing ever recorded a cause for old rows, and deriving one by grepping `error` for the reaper's
string would be inventing a fact from free text that was never a contract.

**The reaper** writes `system_error` unconditionally — a lease expires because our worker died. Its
SQL was also **extracted into `reclaim_stale_leases_sql()`**, because the test held a verbatim copy of
the statement: a test asserting against its own private copy of the SQL passes no matter what the
reaper does, so the fix would have landed in production only.

**`GET /documents`** returns `failure_reason`. It does not return `error`, and the two now sit one
column apart in the same `SELECT` — noted in the code, because that is a one-word diff away from an
invariant-16 breach that reviews clean.

**The dashboard** switches on the reason. The `system_error` branch is the one that must never say
"upload".

## Verification

Unit and integration suites are the routine part; these are the checks that were about *this*.

**The classifier goes red on the regression it exists to catch.** Rewriting the `system_error` copy
to say "Try uploading it again" fails `never tells the tenant to re-upload when the fault is ours` —
watched failing before being trusted.

**`classify` survives a `.context()` layer**, asserted rather than assumed. The failure travels up
through `verify_and_ingest` and anyone may wrap it; if anyhow did not downcast through context, every
parser failure would silently degrade to `SystemError` and nothing else in the stack would notice.

**The `CHECK` fails closed**, probed live: `system_error` accepted, `totally_made_up` rejected with
`documents_failure_reason_check`.

**The wire contract, end to end.** A probe tenant with three failed rows — one `unreadable_file`, one
`system_error`, one pre-classification `NULL` — returned exactly:

```
keys returned: ['created_at', 'failure_reason', 'filename', 'id', 'status']
has raw error column: False
```

with `presumed dead`, `lease`, `parser` and `PdfReadError` all absent from the body, despite being
present in those rows' `error` column. The probe tenant was deleted afterwards; its key and account
cascaded.

## What is deliberately still open

- **Pre-phase-14 `failed` rows carry no reason** and render as the old both-causes copy. Not
  recoverable.
- **The enum lives in three places** — `failure.rs`, the `CHECK`, and the TS union — and only the
  `CHECK` fails closed. Adding a variant to TS *first* is the dangerous order: the UI promises copy
  for a value nothing writes.
- **The reason is coarse for operators.** It answers "re-upload or wait", not "which store was
  down"; that still means reading logs. Deliberate — the column is tenant-facing, and invariant 16
  is why it cannot be both.
- **Exit 3 is still classified `Retryable` for the queue**, so an unsupported file type burns five
  redeliveries before dead-lettering. Harmless to the tenant — they see `unsupported_type`
  immediately, since the reason is written on the first attempt — but it is wasted work, and the two
  axes (what the tenant is told, what the queue does) are now visibly separate in a way that makes
  the fix obvious. Left out of this phase because it changes retry behaviour, not reporting.
- **`quarantined` still hardcodes "25 MB"** in its copy rather than reading the cap.
