# specs/

## What lives here

[`spec.md`](spec.md) is the **Single Source of Truth** for this system's business logic and
architecture. It states what must be true and why. It is the document a behaviour change starts in.

Three documents govern this repository, and they do not overlap:

| | Audience | Answers |
| --- | --- | --- |
| [`README.md`](../README.md) | a human, once | *How do I run and understand this?* |
| [`steering/`](../steering/) | Claude, every session | *How is code written here?* |
| `specs/` | both, before any change | *What must the system do, and why?* |

If you want to know why the relevance floor is `0.70`, read `steering/rag-pipeline.md`. If you want
to know why an ungrounded answer is refused at all, read `spec.md`. The first is a tuning parameter;
the second is a product promise.

## Changing system behaviour

**Define it here first, then write the code.** This is the whole point. Code written before the rule
is agreed encodes a decision nobody made.

For a small change — closing a gap listed in §5, tightening an existing rule — edit `spec.md`
directly and say so in the commit.

For a feature, create `specs/<feature-name>/` with three files:

```
specs/delete-document/
├── requirements.md   # user stories + business rules. WHAT and WHY. No technology.
├── design.md         # tech stack, data model, endpoints, failure modes. HOW.
└── tasks.md          # ordered, checkable steps. Reviewed BEFORE any code is written.
```

### requirements.md

User stories in the form *As a [role], I want [capability], so that [outcome]*, followed by
numbered business rules — the allowed and the forbidden. Rules must be decidable: "the title is at
most 50 characters" is a rule; "the title should be reasonable" is not.

State which invariants from `spec.md` the feature touches. If it needs a new one, propose it here.

### design.md

The technologies and constraints, the data model, the endpoints and their status codes, and — the
part most often skipped — **what happens when each step fails.** For this system that usually means
naming the order of writes across Postgres, Qdrant and MinIO, and what state a half-completed
operation leaves behind.

Bind it to reality: this is a Rust workspace on Axum, SQLx and Qdrant. See `steering/`.

### tasks.md

An ordered checklist, each item small enough to review. Ordering is the substance, not the ceremony:
the migration precedes the query that uses it; the data model precedes the endpoint that exposes it;
the test that proves an invariant precedes the code that relies on it.

**Review the task list before approving any code.** If the order is wrong, or a task contradicts
`requirements.md`, fix the document — not the code that was generated from it. Correcting generated
code leaves the specification wrong, and the next change regenerates the same mistake.

Ship `tasks.md` with every box unchecked. Check them as they land.

## Iterating

Specifications are refined continuously, not written once.

When requirements change, edit `requirements.md`, then propagate: `design.md` is revised to match,
and `tasks.md` is regenerated from the revised design. Skipping the propagation is how a project ends
up with a map of a place it no longer is.

When implementation reveals the specification was wrong — and it will — fix the specification **in
the same change** as the code. A document that has silently drifted is more dangerous than no
document, because it is still trusted.
