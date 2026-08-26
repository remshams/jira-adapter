# jcli: read-only Jira Cloud CLI

## Context

New tool, built from scratch in an empty repo (`/home/remshams/coding/jira-adapter`). Goal: a small CLI that reads Jira Cloud issues so AI agents (primary consumer) can pull ticket content into their context, and later download ticket attachments (zips, screenshots, videos). Read-only by design for now; write operations are an explicit future extension, so the HTTP layer is built GET-only from day one.

Steps 0 through 7 are complete. The CLI authenticates with Jira Cloud, validates exact issue keys, renders stable issue Markdown with an attachment count, and downloads issue attachments safely on Unix systems.

Primary consumers are agents, which drives the output design: predictable markdown on stdout, no colors or spinners, errors as one clear line on stderr with non-zero exit.

## Settled decisions (from Q&A rounds)

| Decision | Outcome |
|---|---|
| Language | Rust, stable toolchain (single static binary, good CLI ecosystem) |
| Instance type | Jira Cloud; scoped API token sent with Basic auth using email + token |
| CLI shape | Nested subcommands: `jcli issue read <KEY>` (mirrors `gh`/`az`, leaves room for future verbs) |
| Binary name | `jcli` |
| Issue reference forms | Exact, canonical issue keys such as `ABC-123`; URLs and bare numbers are rejected |
| Output | Predictable Markdown for agents on stdout: title line, fields (status, assignee, reporter, labels), description rendered from ADF. No comments in the first version. No `--json` yet. |
| Config | Env vars `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN`, plus optional `.env` in CWD loaded via dotenvy (real env wins). `JIRA_BASE_URL` is the HTTPS Atlassian API gateway URL containing the Jira cloud ID. Plain HTTP is rejected except loopback testing with `JCLI_ALLOW_INSECURE_HTTP=1`. Fail fast naming the missing variable. |
| HTTP | Blocking reqwest (sync, no tokio). GET-only client using Basic auth. JSON redirects stay on the configured origin; attachment-content redirects are disabled. |
| ADF rendering | Hand-rolled ADF-to-Markdown renderer, unit tested per node type. Unknown nodes preserve known child content where possible instead of failing the issue. |
| Errors | thiserror in client/parser modules and explicit exit handling at the application boundary |
| Tests | Inline unit tests for parsers and renderer; httpmock server tests for the HTTP client (auth header, status mapping, field parsing) |
| Comments | Deferred (fixes earlier over-scope: no comment pagination work) |
| Attachments | `jcli issue attachments <key>` downloads all attachments on Unix into `./<issue-key>/`, with `--out` as a direct directory override, ID-prefixed safe filenames, no overwrite, bounded streaming, staged publication, and exact size verification. |

## Approach

Small sequential steps, one component per commit (commit scope in PascalCase, e.g. `feat(JiraClient): ...`). The user reads through each step before we move to the next. Quality-first: every piece of logic gets tests, `cargo clippy` stays warning-free.

## Files

- `Cargo.toml`, `Cargo.lock`, `.gitignore`, `src/main.rs` (clap skeleton)
- `src/config.rs` (env + dotenvy config)
- `src/jira/client.rs` (blocking GET-only HTTP client, Basic auth)
- `src/jira/issue.rs` (issue models: fields, ADF document struct)
- `src/issue_key.rs` (exact issue-key parsing)
- `src/adf/markdown.rs` (ADF-to-Markdown renderer)
- `src/attachments.rs` (safe attachment planning, streaming, staging, and publication)
- `src/jira/attachment.rs` (minimal attachment metadata models)
- Inline unit and httpmock-based client tests
- `PLAN.md` (this file)

## Steps

- [x] 0. Reset the pre-plan skeleton: remove the committed cargo project and git history (`rm -rf` repo contents, fresh `git init -b main`)
- [x] 1. cargo init (`--bin --name jcli`), add clap (derive), `jcli issue read <issue>` subcommand that parses args and exits 1 with "not implemented" — commit scope `chore(IssueRead)`
- [x] 2. `config.rs`: dotenvy `.env` loading, read/validate `JIRA_BASE_URL` | `JIRA_EMAIL` | `JIRA_API_TOKEN`, clear error naming the missing var, unit tests — commit scope `feat(Config)`
- [x] 3. `jira/client.rs`: blocking reqwest GET client with Basic auth, typed errors (thiserror), `/rest/api/3/myself` smoke method, httpmock tests for auth header and status mapping — commit scope `feat(JiraClient)`
- [x] 4. `issue_key.rs`: accept exact canonical keys such as `ABC-123`, reject URLs and malformed values with a useful error, unit tests — commit scope `feat(IssueKey)`
- [x] 5. `jira/issue.rs` + client method: GET `/rest/api/3/issue/{key}` while requesting only summary, status, assignee, reporter, labels, and description; serde models and an httpmock test with a canned issue response — commit scope `feat(IssueRead)`
- [x] 6. `adf/markdown.rs`: recursive ADF-to-Markdown renderer (headings, paragraphs, bold/italic/code/links, bullet/ordered lists, code blocks, blockquotes), preserve known child content under unknown nodes, unit tests per node type, wire into `issue read` output — commit scope `feat(AdfRenderer)`
- [x] 7. Attachments (`jcli issue attachments <key>`, Unix-only download into `./<issue-key>/`, direct `--out` override, no overwrite, safe ID-prefixed names, bounded metadata and content, staged no-clobber publication, cleanup and process tests) — commit scope `feat(Attachments)`

## Verification

- `cargo test` green at every step (unit + httpmock client tests)
- `cargo clippy` warning-free
- `jcli issue read ABC-123` with wrong/missing env vars prints a clear error naming the variable and exits non-zero
- Manual smoke test completed against the real Jira Cloud instance after step 6: `jcli issue read` succeeded with a real key and its output matched the ticket.
- Attachment downloads remain covered by local mock-server tests only until the user chooses to run a real attachment smoke test.
