# jcli: read-only Jira Cloud CLI

## Context

New tool, built from scratch in an empty repo (`/home/remshams/coding/jira-adapter`). Goal: a small CLI that reads Jira Cloud issues so AI agents (primary consumer) can pull ticket content into their context, and later download ticket attachments (zips, screenshots, videos). Read-only by design for now; write operations are an explicit future extension, so the HTTP layer is built GET-only from day one.

A step 1 skeleton (cargo project + clap subcommand) was already committed before planning mode started. The first implementation step resets that work so everything after it follows this plan.

Primary consumers are agents, which drives the output design: predictable markdown on stdout, no colors or spinners, errors as one clear line on stderr with non-zero exit.

## Settled decisions (from Q&A rounds)

| Decision | Outcome |
|---|---|
| Language | Rust, stable toolchain (single static binary, good CLI ecosystem) |
| Instance type | Jira Cloud; auth is Basic auth with email + API token (NOT Bearer PAT, Cloud has no PATs) |
| CLI shape | Nested subcommands: `jcli issue read <KEY or URL>` (mirrors `gh`/`az`, leaves room for future verbs) |
| Binary name | `jcli` |
| Issue reference forms | Issue key (`ABC-123`) and browse URL (`https://host/browse/ABC-123`); no bare numbers |
| Output | Markdown to stdout: title line, fields (status, assignee, reporter, labels), description rendered from ADF. No comments in the first version. No `--json` yet. |
| Config | Env vars `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN`, plus optional `.env` in CWD loaded via dotenvy (real env wins). Fail fast naming the missing variable. |
| HTTP | Blocking reqwest (sync, no tokio). GET-only client. |
| ADF rendering | Hand-rolled ADF-to-markdown (no mature crate exists), unit tested per node type |
| Errors | thiserror in client/parser modules, anyhow at the application boundary |
| Tests | Unit tests for parsers/renderer; wiremock mock-server tests for the HTTP client (auth header, 401/404/429 mapping, field parsing) |
| Comments | Deferred (fixes earlier over-scope: no comment pagination work) |
| Attachments | Deferred until `read` is done (step 7) |

## Approach

Small sequential steps, one component per commit (commit scope in PascalCase, e.g. `feat(JiraClient): ...`). The user reads through each step before we move to the next. Quality-first: every piece of logic gets tests, `cargo clippy` stays warning-free.

## Files to create

- `Cargo.toml`, `Cargo.lock`, `.gitignore`, `src/main.rs` (clap skeleton)
- `src/config.rs` (env + dotenvy config)
- `src/jira/client.rs` (blocking GET-only HTTP client, Basic auth)
- `src/jira/issue.rs` (issue models: fields, ADF doc struct)
- `src/issue_key.rs` (key/URL parsing)
- `src/adf/markdown.rs` (ADF-to-markdown renderer)
- `tests/` (wiremock-based client tests)
- `PLAN.md` (this file)

## Steps

- [ ] 0. Reset the pre-plan skeleton: remove the committed cargo project and git history (`rm -rf` repo contents, fresh `git init -b main`)
- [ ] 1. cargo init (`--bin --name jcli`), add clap (derive), `jcli issue read <issue>` subcommand that parses args and exits 1 with "not implemented" — commit scope `chore(IssueRead)`
- [ ] 2. `config.rs`: dotenvy `.env` loading, read/validate `JIRA_BASE_URL` | `JIRA_EMAIL` | `JIRA_API_TOKEN`, clear error naming the missing var, unit tests — commit scope `feat(Config)`
- [ ] 3. `jira/client.rs`: blocking reqwest GET client with Basic auth, typed errors (thiserror), `/rest/api/3/myself` smoke method, wiremock tests for auth header and 401/404/500 mapping — commit scope `feat(JiraClient)`
- [ ] 4. `issue_key.rs`: parse `ABC-123` and browse URLs to a key, reject malformed input with a useful error, unit tests — commit scope `feat(IssueKey)`
- [ ] 5. `jira/issue.rs` + client method: GET `/rest/api/3/issue/{key}` (fields: summary, status, assignee, reporter, labels, description), serde models, wiremock test with a canned issue response — commit scope `feat(IssueRead)`
- [ ] 6. `adf/markdown.rs`: recursive ADF-to-markdown renderer (headings, paragraphs, bold/italic/code/links, bullet/ordered lists, code blocks, blockquotes), unit tests per node type, wire into `issue read` output — commit scope `feat(AdfRenderer)`
- [ ] 7. Attachments (`jcli issue attachments <key>`, download all into `./<issue-key>/`, `--out` override, no overwrite) — deferred, planned only after 1-6 land and are reviewed

## Verification

- `cargo test` green at every step (unit + wiremock integration tests)
- `cargo clippy` warning-free
- `jcli issue read ABC-123` with wrong/missing env vars prints a clear error naming the variable and exits non-zero
- Manual smoke test against the real Jira Cloud instance after step 5/6: `jcli issue read` with a real key and with a full browse URL, output compared against the ticket in the browser (requires a user-provided API token from https://id.atlassian.com/manage-profile/security/api-tokens)
