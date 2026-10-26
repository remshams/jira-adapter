# 0002: build jcli as a single Rust binary

## Status

Accepted

## Date

2026-10-26

## Context

Coding agents call jcli many times per task, from any working directory and on
machines the maintainer does not control. The tool has to start fast, install
without a runtime and stay easy to place on PATH. Every command sends one to a few
HTTP requests in sequence and waits for each answer.

## Decision

Write jcli in Rust on the stable toolchain, edition 2024, as one binary crate
named `jcli`. Install it with `cargo install --path <checkout>`.

Use reqwest's blocking client and no async runtime. Requests run one after
another; nothing in jcli benefits from concurrency. Use clap's derive API for
nested subcommands in the style of `gh` and `az` (`jcli issue read <KEY>`), which
leaves room for new verbs. Use thiserror for error types in the client and parser
modules, and handle exit codes explicitly in `src/commands.rs`.

## Consequences

Users get one self-contained executable and no interpreter or package manager at
run time. The build needs a Rust toolchain, and a change ships only after a
rebuild and reinstall. Blocking I/O keeps the code linear and the dependency tree
small. Parallel downloads or concurrent requests would need either threads or an
async runtime, and a new record.
