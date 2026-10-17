# jcli: read-only Jira Cloud CLI

## Context

New tool, built from scratch in an empty repo (`/home/remshams/coding/jira-adapter`). Goal: a small CLI that reads Jira Cloud issues so AI agents (primary consumer) can pull ticket content into their context, and later download ticket attachments (zips, screenshots, videos). Read-only by design for now; write operations are an explicit future extension, so the HTTP layer is built GET-only from day one.

Steps 0 through 11 are complete. The CLI authenticates with Jira Cloud, validates exact issue keys, renders stable issue Markdown with the issue type, created timestamp, and attachment count, downloads issue attachments safely on Unix systems, and searches issues with JQL as JSON. Every Jira request has explicit timeouts.

Primary consumers are agents, which drives the output design: predictable markdown on stdout, no colors or spinners, errors as one clear line on stderr with non-zero exit.

## Settled decisions (from Q&A rounds)

| Decision | Outcome |
|---|---|
| Language | Rust, stable toolchain (single static binary, good CLI ecosystem) |
| Instance type | Jira Cloud; scoped API token sent with Basic auth using email + token |
| CLI shape | Nested subcommands: `jcli issue read <KEY>` (mirrors `gh`/`az`, leaves room for future verbs) |
| Binary name | `jcli` |
| Issue reference forms | Exact, canonical issue keys such as `ABC-123`; URLs and bare numbers are rejected |
| Output | Predictable Markdown for agents on stdout: title line, fields (type, status, assignee, reporter, created, labels, attachment count), description rendered from ADF. No comments in the first version. `issue read` has no `--json` yet; `issue search` is JSON-only for now. |
| Config | Three sources with per-setting precedence: process environment > exact `.env` in the CWD (parsed without mutating the environment, first duplicate key wins) > optional per-user TOML file at `$XDG_CONFIG_HOME/jcli/config.toml` (only when absolute and non-empty) or `~/.config/jcli/config.toml` (absolute home). A present-but-empty or non-Unicode value is authoritative and never falls through; only absence does. `JIRA_BASE_URL` only has to parse as a URL; a trailing slash is added so relative API paths append. Per `AGENTS.md`, jcli assumes properly set up files and well-formed values: no size caps, symlink/FIFO hardening, scheme checks, or `.env`/TOML mixing policies. A malformed or unreadable file fails with a generic one-line error. Fail fast naming the missing variable. |
| HTTP | Blocking reqwest (sync, no tokio). GET-only client using Basic auth. JSON redirects stay on the configured origin; attachment-content redirects are disabled. Both Jira clients use a 10 s connect timeout and a 30 s request timeout, the same values as the Xray client. |
| ADF rendering | Hand-rolled ADF-to-Markdown renderer, unit tested per node type. Unknown nodes preserve known child content where possible instead of failing the issue. |
| Errors | thiserror in client/parser modules and explicit exit handling at the application boundary |
| Tests | Inline unit tests for parsers and renderer; httpmock server tests for the HTTP client (auth header, status mapping, field parsing) |
| Comments | Deferred (fixes earlier over-scope: no comment pagination work) |
| Timeouts | reqwest's blocking client applies the request timeout to sending the request and receiving the response headers, and then to each read call on a streamed body. For attachments it is an idle timeout, not a cap on total download time, so large videos and archives keep downloading while data arrives. |
| Issue search | `jcli issue search --jql "<JQL>" --json [--limit N]`. `--json` is required, which leaves room for Markdown later. `--limit` defaults to 20 and clap accepts 1 to 100. GET `/rest/api/3/search/jql` with `jql` (unchanged, not validated), `maxResults`, and `fields=summary,status,issuetype,created,updated,labels`. First page only, no pagination flags, read through the bounded metadata reader. Output is pretty JSON with `jql`, `returned`, `truncated` (`!isLast`, null when `isLast` is absent), and `issues` with `key`, `issueType`, `status`, `summary`, `created`, `updated`, `labels`. Missing or null values stay null, except `labels`, which becomes `[]`. Timestamps pass through unchanged, issues keep Jira's order (sort with `ORDER BY`), and an empty result exits 0 with `"issues": []`. Output uses the same C1, DEL, and bidi escaping as `xray test-runs`. A 400 from the search endpoint reports Jira's `errorMessages` and `errors` as one sanitized line (`invalid JQL query (400): ...`), read from a body bounded to 64 KiB. A field error with a blank message is dropped, and without usable messages the error falls back to the generic 400. Before the text is surfaced, the API token and the Basic authorization value (with and without `Basic `) become `[REDACTED]` in field names and messages. The email alone is not redacted, because JQL errors quote it. Other endpoints keep generic errors. |
| Attachments | `jcli issue attachments <key>` downloads all attachments on Unix into `./<issue-key>/`, with `--out` as a direct directory override. Each attachment streams into a private temporary file, passes exact-size verification, and then atomically overwrites its final ID-prefixed filename. If a later attachment fails, earlier replacements remain. The user owns and controls the output directory. |

## Config file

Create `config.toml` at `$XDG_CONFIG_HOME/jcli/config.toml`, or at `~/.config/jcli/config.toml` when `XDG_CONFIG_HOME` is unset, empty, or relative:

```toml
base_url = "https://api.atlassian.com/ex/jira/your-cloud-id"
email = "agent@example.com"
api_token = "..."
```

Environment variables override the file. Keep token-bearing files readable only by the owner, for example with mode `0600`.

## Approach

Small sequential steps, one component per commit (commit scope in PascalCase, e.g. `feat(JiraClient): ...`). The user reads through each step before we move to the next. Quality-first: every piece of logic gets tests, `cargo clippy` stays warning-free.

## Files

- `Cargo.toml`, `Cargo.lock`, `.gitignore`, `src/main.rs` (clap skeleton)
- `src/config.rs` (env + dotenvy config)
- `src/jira/client.rs` (blocking GET-only HTTP client, Basic auth)
- `src/jira/issue.rs` (issue models: fields, ADF document struct)
- `src/issue_key.rs` (exact issue-key parsing)
- `src/adf/markdown.rs` (ADF-to-Markdown renderer)
- `src/attachments.rs` (safe attachment planning, streaming, and per-file atomic replacement)
- `src/jira/attachment.rs` (minimal attachment metadata models)
- `src/jira/search.rs` (JQL search response models)
- `src/issue_search.rs` (issue search JSON output)
- `src/output.rs` (shared escaped JSON rendering and single-line terminal sanitizing)
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
- [x] 7. Attachments (`jcli issue attachments <key>`, Unix-only download into `./<issue-key>/`, direct `--out` override, safe ID-prefixed names, bounded metadata and content, per-file temporary downloads, exact-size verification, atomic overwrite, cleanup and process tests) — commit scopes `feat(Attachments)` and `refactor(Attachments)`
- [x] 8. Config file support (`feat(Config)`): optional per-user flat TOML file (only `$XDG_CONFIG_HOME/jcli/config.toml` with an absolute, non-empty `XDG_CONFIG_HOME`, else `~/.config/jcli/config.toml` with an absolute home; no system-wide path; relative values are never resolved against the CWD). Precedence per setting is process environment > exact CWD `.env` > TOML; present-but-empty or non-Unicode values are authoritative and do not fall through; the `.env` is read as absent, valid, or invalid — an invalid `.env` (present but unclassifiable, no partial entries kept) disables TOML entirely and reports a generic configuration error when process values are incomplete; a CWD `.env` supplying any jcli-relevant value also disables TOML entirely (no credential/source mixing), and duplicate `.env` keys keep the first assignment like `dotenvy::from_path`, and variable substitution is rejected to keep parsing bounded; `JCLI_ALLOW_INSECURE_HTTP` stays environment/`.env`-only; the TOML file is not read when the process environment supplies all three `JIRA_*` settings. Both the TOML file and the `.env` are read as bounded (64 KiB) regular files: on Unix they are opened with `O_NONBLOCK | O_NOFOLLOW` and regular-file status is validated on the opened handle before reading, so FIFOs and device files cannot block the read and a symlink as the final path component is rejected (documented policy); non-Unix keeps a portable open with the same handle check. Raw bytes are read before UTF-8 decoding so oversize is classified before encoding errors: `NotFound` means absent, non-regular/unreadable targets and permission errors are generic unreadable errors, invalid UTF-8 and TOML errors (including unknown keys) are generic malformed errors, and files over 64 KiB are rejected as too large. Errors never echo contents, parser details, or raw paths; `Debug` output never includes the email or API token. Blank TOML values report `Empty`, omitted keys report `Missing`. Later simplified per `AGENTS.md`: the deep file checks, substitution rejection, `JCLI_ALLOW_INSECURE_HTTP`, and the `.env`/TOML mixing policy were removed; a malformed or unreadable file now fails with a generic one-line error. |
- [x] 9. Jira request timeouts: 10 s connect and 30 s request timeout on the JSON and the attachment client, with tests for stalled headers, stalled bodies, and a slow attachment stream that outlasts the request timeout while data keeps arriving — commit scope `feat(JiraClient)`
- [x] 10. `issue read` shows the issue type and the created timestamp: request `issuetype` and `created`, render `- Type:` before Status and `- Created:` after Reporter, print the timestamp as Jira returns it, and fall back to `Unknown` for missing, null, or empty values — commit scope `feat(IssueRead)`
- [x] 11. `jcli issue search` (see the Issue search decision above): shared output helpers moved into `src/output.rs`, search client and models, then the CLI command — commit scopes `refactor(Output)`, `feat(JiraClient)`, and `feat(IssueSearch)`

## Verification

- `cargo test` green at every step (unit + httpmock client tests)
- `cargo clippy` warning-free
- `jcli issue read ABC-123` with wrong/missing env vars prints a clear error naming the variable and exits non-zero
- Manual smoke test completed against the real Jira Cloud instance after step 6: `jcli issue read` succeeded with a real key and its output matched the ticket.
- Attachment downloads remain covered by local mock-server tests only until the user chooses to run a real attachment smoke test.
- Manual smoke test against the real Jira Cloud instance is still pending for steps 9 to 11 (`issue read` with type and created, `issue search` with valid and invalid JQL). The user will run it on another machine.
