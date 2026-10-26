# 0004: gate changes with Rust tooling, mutants and CRAP

## Status

Accepted

## Date

2026-10-26

## Context

Agents write most changes to jcli. Passing tests alone do not show that the tests
check behavior, and a growing function can keep full coverage while becoming hard
to change. The checks have to run the same way locally and in CI, and a full
mutation run takes too long to repeat on every pull request.

## Decision

Every change passes these gates:

- `cargo fmt --check`. The `.githooks/pre-commit` hook runs it; enable the hook
  per clone with `git config core.hooksPath .githooks`.
- `cargo clippy --all-targets -- -D warnings`, with `clippy::all` denied in
  `Cargo.toml`.
- `cargo test`.
- `cargo llvm-cov` 0.9.0 for coverage and `cargo crap` 0.4.3 for the CRAP score.
  The threshold lives in `.cargo-crap.toml`, and uncovered code counts
  pessimistically.
- `cargo machete` 0.9.2 for unused dependencies.
- `cargo mutants` 27.1.0. Pull requests check only the mutants in their diff
  against the base branch with `--in-diff`. Pushes to `main` and manual runs check
  the full suite in four shards. Missed and timed-out mutants fail the gate.

CI runs the gates in jobs; `fmt` and `clippy` share the Lint job. An aggregate
`ci-success` job fails when any job failed or was cancelled, and a ruleset on
`main` requires it.

Considered and deferred:

- cargo-deny for licenses and advisories. jcli has a short, stable dependency
  list; add it when that changes.
- Duplication detection such as jscpd. The test fixtures repeat JSON bodies and
  requests on purpose, which makes its reports mostly noise.
- Architecture-boundary tooling such as cargo-modules. All modules are private to
  one binary crate, and Rust's module privacy covers most boundaries at this
  size. The allowed imports are documented in the
  [architecture document](../architecture.md).

## Consequences

A surviving mutant or a complex, untested function blocks a merge, so tests have
to check behavior, not just run code. In-diff mutation keeps pull request checks
short; the full run on `main` catches gaps outside the diff after the fact.
Contributors need the pinned tool versions locally. Module boundaries rely on
review, and a new record is needed to add one of the deferred tools.
