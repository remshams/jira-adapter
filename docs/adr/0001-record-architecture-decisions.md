# 0001: record architecture decisions

## Status

Accepted

## Date

2026-10-26

## Context

jcli was built in small steps from a single planning file, `PLAN.md`. It mixed the
settled decisions with a step checklist and verification notes, and each decision
was edited in place as it changed. The reason behind a rule and its earlier form
were hard to find.

## Decision

Record architecture decisions as short Markdown ADRs in `docs/adr/`, using Status,
Date, Context, Decision and Consequences. The [README](README.md) indexes every
record. Accepted records are immutable apart from typo fixes and superseding
status notes. A changed decision needs a new record that links the old one;
partial replacements name the clauses they supersede.

The decisions that `PLAN.md` held are recorded as ADRs 0002 to 0009, and the
[architecture document](../architecture.md) describes the current structure.
`PLAN.md` is removed. Routine implementation choices stay in commit messages and
code review.

## Consequences

Contributors can find why a rule exists and what replaced it. Each significant
decision adds a file and an index entry; ordinary changes need neither. The step
history of the first version lives only in git history.
