# Architecture decision records

This directory holds jcli's architecture decisions. Each record is one Markdown
file named `NNNN-kebab-title.md`, numbered in decision order. Use the
[template](template.md), with five sections: Status, Date, Context, Decision and
Consequences. No front matter is needed.

## When to write a record

Record decisions that change module responsibilities or dependencies, the CLI
contract (commands, flags, output shapes, exit codes), the requests jcli sends to
Jira or Xray, credential handling, or the quality gates contributors must follow.
Explain the constraint, the choice and the tradeoff. Test names, routine
dependency bumps and implementation details within an existing module belong in
commits and code review.

## Rules

- A record starts as Proposed. Once approved, its status becomes Accepted.
- Accepted records are immutable. Fixing a typo is fine; changing the decision
  requires a new record.
- If a whole decision changes, keep the old content and update only its Status
  to `Superseded by ADR NNNN`, linking the replacement.
- If only part changes, use `Partially superseded by ADR NNNN; all other decisions
  remain accepted.` The replacement must name the clauses it supersedes and link
  the old record.
- Add each record to the index and keep its status current.

## Index

Records 0002 to 0009 were written up on 2026-10-26 for decisions made while jcli
was first built; their Date is the date they were recorded.

| Record | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-build-jcli-as-a-single-rust-binary.md) | Build jcli as a single Rust binary | Accepted |
| [0003](0003-keep-the-tool-small-and-trust-local-input.md) | Keep the tool small and trust local input | Accepted |
| [0004](0004-quality-gates.md) | Gate changes with Rust tooling, mutants and CRAP | Accepted |
| [0005](0005-read-only-get-only-http-layer.md) | Read-only, GET-only Jira client | Accepted |
| [0006](0006-configuration-sources-and-precedence.md) | Configuration sources and precedence | Accepted |
| [0007](0007-output-for-agents.md) | Output for agents | Accepted |
| [0008](0008-hand-rolled-adf-to-markdown-renderer.md) | Hand-rolled ADF-to-Markdown renderer | Accepted |
| [0009](0009-attachment-downloads.md) | Stream attachments into verified, atomically replaced files | Accepted |
