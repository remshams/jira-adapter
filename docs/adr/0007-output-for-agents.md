# 0007: output for agents

## Status

Partially superseded by [ADR 0010](0010-list-and-select-attachments.md); all other decisions remain accepted.

## Date

2026-10-26

## Context

jcli's main users are coding agents. They read stdout into their context, parse
JSON, and decide what to do from the exit code. Colors, spinners, partial output
or a multi-line error make that harder. Jira text can also carry terminal control
sequences or bidirectional formatting characters that change what a human sees
in a terminal.

## Decision

- Results go to stdout: Markdown for `issue read` and `issue attachments`, pretty
  JSON for `issue search` and `xray test-runs`, one line for `whoami`. No colors,
  progress or prompts.
- jcli renders the complete result before writing it. stdout stays empty on
  failure.
- `issue search` and `xray test-runs` require `--json`. That keeps room for a
  Markdown form later without changing the default.
- Once the arguments parse, errors go to stderr as one line starting with
  `error: `. Invalid arguments get clap's multi-line usage message. Exit codes say where
  the failure happened: 1 for an invalid issue key, 2 for configuration or usage,
  3 for Jira or Xray, 4 for the filesystem, the platform or stdout.
- Messages never contain credentials, file contents or response bodies. The one
  exception is Jira's explanation of rejected JQL, reduced to one line with the
  API token and Basic authorization value redacted. The email stays because JQL
  errors quote it. Xray GraphQL messages are dropped.
- Jira values in Markdown are reduced to safe text: single-line metadata, control
  and bidirectional formatting characters removed, Markdown characters escaped.
  JSON escapes DEL, C1 controls and bidirectional formatting characters as
  `\uXXXX`, so parsing still returns the original text.
- Missing values are explicit: `Unknown`, `Unassigned` or `None` in Markdown and
  `null` in JSON.

## Consequences

Agents can rely on a stable output contract and a meaningful exit code. Changing a
command, flag, output field or exit code is a contract change and needs care.
Exit code 2 covers both clap usage errors and configuration errors, so a caller
has to read the message to tell them apart.
