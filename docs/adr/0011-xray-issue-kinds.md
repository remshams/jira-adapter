# 0011: Xray issue kinds for test runs

## Status

Proposed

## Date

2026-10-27

## Context

`jcli xray test-runs <KEY>` looked up the issue's numeric ID in Jira and always
sent it to Xray as `getTestRuns(testIssueIds: ...)`. Agents also pass the keys of
nightly test report tickets. If such a ticket is a Test Execution, no test has
its ID, and Xray answers with `total: 0`. That answer looks the same as a test
that never ran, so the agent cannot tell a wrong key from an empty history.

Xray's `getTestRuns` filters by Test issue IDs (`testIssueIds`) or by Test
Execution issue IDs (`testExecIssueIds`). The Jira issue type says which one
applies, and jcli already sends one Jira request before it calls Xray.

Jira was unreachable while this was planned, so three assumptions are
unverified:

- Nightly report tickets are Xray Test Execution issues.
- The instance uses Xray's default issue type names, `Test`,
  `Test Execution` and `Sub Test Execution`.
- `getTestRuns` accepts `testExecIssueIds` and returns the same page shape as
  with `testIssueIds`.

## Decision

- The Jira lookup changes from `GET rest/api/3/issue/{key}?fields=summary` to
  `?fields=issuetype`. It is still one request through the bounded metadata
  reader, and it returns the numeric ID and the issue type name.
- The Xray side maps the type name to an `XrayIssueKind`. `Test` becomes `Test`
  and is queried with `testIssueIds: [id]`. `Test Execution` becomes
  `TestExecution` and is queried with `testExecIssueIds: [id]`. Xray's default
  sub-task type `Sub Test Execution` is a Test Execution too and maps to
  `TestExecution`. Names match exactly and case-sensitively. Jira's response
  types stay in `src/jira/`.
- Any other type, or a missing or empty type name, exits 3 with one line such as
  `error: ABC-1 is not an Xray Test or Test Execution (issue type: Story)`, and
  jcli sends no Xray request. The type name is Jira data, so it is reduced to one
  terminal-safe line. Without a name the message says `issue type: unknown`.
- Each kind has its own GraphQL query constant, and tests pin both query texts
  and the exact variables sent per kind. Both call only `getTestRuns`, so the
  read-only rule of [ADR 0005](0005-read-only-get-only-http-layer.md) holds.
- The JSON output renames `testKey` to `issueKey` and adds `issueKind` with the
  value `test` or `testExecution`. An empty `runs` list now has one meaning: the
  queried key has no runs. This changes the contract described in
  [ADR 0007](0007-output-for-agents.md) on purpose.
- `--status`, `--sort` and `--direction` work the same for both kinds.

## Consequences

Agents can pass a Test or a Test Execution key and get that key's runs. A key of
any other type fails before Xray is called, instead of returning an empty page
that hides the mistake.

Consumers that read `testKey` break and must switch to `issueKey`.

A renamed Test or Test Execution type in a Jira instance hits the error path.
Supporting custom names would need configuration, which this decision leaves out.
If `getTestRuns` rejects `testExecIssueIds` or returns another shape, Test
Execution keys fail with the generic Xray error. A smoke test against a real
Test Execution should confirm the three assumptions above; report mismatches in
the umbrella issue.
