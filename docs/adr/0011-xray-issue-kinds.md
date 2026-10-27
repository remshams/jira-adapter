# 0011: Xray issue kinds and paging for test runs

## Status

Accepted

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

A run in the output has only its ID, status, start, finish and Test Execution
ID. To see why a run failed, agents download and parse the HTML report
attachment instead. Xray's `TestRun` type also holds the run comment, linked
defects, evidence, manual step results and automated results, where `log`
carries the error or failure output.

jcli asked `getTestRuns` for one page with `limit: 100` and `start: 0`, Xray's
largest page. Comparing nightly runs over time needs more than 100 runs, and
jcli had no way to fetch the next page. Xray reports `total` on every page.

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
- Both kinds share one selection set and differ only in the filter, which
  names both the `getTestRuns` argument and its variable. The client builds the
  query text from the filter name and the selection set. Tests pin every query
  text verbatim and the exact variables sent per kind. All queries call only
  `getTestRuns`, so the read-only rule of
  [ADR 0005](0005-read-only-get-only-http-layer.md) holds.
- `--details` adds detail fields to the selection set, for both kinds:
  `comment`, `defects`, `evidence { filename downloadLink }`,
  `steps { status { name } actualResult comment evidence { filename downloadLink } }`
  and `results { name status { name } log duration }`. Without `--details` the
  query text and the output stay as before.
- With `--details` each run in the output also has `comment`, `defects` (issue
  IDs), `evidence`, `steps` and `results`, in camelCase. Statuses are flattened
  to their name, like the run status. A missing value or list is null, and a
  null list entry stays null. jcli doesn't download evidence; `downloadLink` is
  passed through as a reference. The 16 MiB GraphQL response limit stays.
- The JSON output renames `testKey` to `issueKey` and adds `issueKind` with the
  value `test` or `testExecution`. An empty `runs` list now has one meaning: the
  queried key has no runs. This changes the contract described in
  [ADR 0007](0007-output-for-agents.md) on purpose.
- `--status`, `--sort` and `--direction` work the same for both kinds.

### Paging

- `xray test-runs` takes `--limit`, which accepts 1 to 1000 and defaults to 100.
  With the default, the first request is the same as before, and the same runs
  give the same output.
- Each request asks for `limit: min(100, remaining)` runs, where `remaining` is
  `--limit` minus the runs collected so far. `start` is the number of runs
  collected so far. The query text and the other variables (the issue filter,
  `statuses`) are the same on every page, for both kinds and with or without
  `--details`.
- Paging stops when `--limit` runs are collected, when `start` plus the page's
  runs reaches the page's `total`, when a page has no `total`, or when a page
  returns no runs. The empty-page rule keeps a server that reports too high a
  total from keeping jcli requesting forever.
- The output keeps its shape. `runs` holds the runs of all pages in fetch order,
  and `returned` counts them. `total` is the total of the last fetched page.
  `truncated` is `total > returned`, or null without a total. `start` is 0 and
  `limit` is the requested `--limit`, not the values Xray echoes.
- `--sort` and `--direction` order the runs of all fetched pages.
- A failed page fails the whole command and stdout stays empty. Each response is
  read through the 16 MiB limit on its own.
- The page loop lives in the Xray client. The page type stays in `src/xray/`,
  and the client hands the command one merged page.

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

With `--details` an agent reads the failure reason, such as an automated
result's `log`, from the JSON output instead of parsing report attachments. A
page of 100 runs with long logs can exceed the 16 MiB response limit; that page
fails with the existing `Xray response exceeds the safety limit` error.

With `--limit` an agent reads up to 1000 runs with one command. A higher limit
sends up to ten GraphQL requests, so the command takes longer and counts more
against Xray's rate limit. A failure on a later page discards the runs already
fetched. Each page is a separate request, so a run added between requests can
shift the pages: a run can be missed or appear twice.

The detail field names and types come from Xray Cloud's GraphQL schema
documentation (`TestRun`, `TestRunStep`, `Evidence`, `Result`, `StepStatus`)
and have not been checked against a real response. If Xray rejects one,
`--details` fails with `Xray GraphQL query failed` while the default query keeps
working. The same smoke test should run `--details` against a failed run.
