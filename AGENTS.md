# AGENTS.md

Jira adapter is a cli tool that agents are using to connect to jira e.g. for reading issues, downloading attachments and the like

## General

- Always apply the `unslop` skill when writing or editing any text (responses, code comments, docs, commit messages, etc.)

## Domain language

Describe architecture in Domain-Driven Design terms. This applies to docs,
issues, PR descriptions and explanations of the code.

- Use the project's ubiquitous language: the words the domain uses, such as
  `issue` and `attachment`. Use the same word in code, docs and conversation,
  and don't switch between synonyms for one concept.
- Name the parts of the model by their DDD role. An entity keeps its identity as
  its data changes. A value object is defined by its values and is immutable. An
  aggregate is a cluster of objects that changes only through its root.
- Call stateless domain logic that belongs to no single type a domain service,
  and code that runs one use case an application service.
- Call the boundary within which one model and its language stay consistent a
  bounded context. Code that maps the Jira REST API to that model is an
  anti-corruption layer; keep Jira response types out of the rest of the code.
- Use these terms to describe the code that exists. Don't add layers, interfaces
  or folders just to match a DDD pattern; keeping the project small still wins.

## Validation

- After completing a task or feature, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo mutants`, `cargo llvm-cov --lcov --output-path lcov.info`, and `cargo crap --lcov lcov.info` before handoff.
- Use cargo-mutants 27.1.0. Install it with `cargo install --locked cargo-mutants --version 27.1.0` if it is unavailable.
- Use cargo-llvm-cov 0.9.0 and cargo-crap 0.4.3. Install them with `rustup component add llvm-tools-preview`, `cargo +stable install --locked --version 0.9.0 cargo-llvm-cov`, and `cargo +stable install --locked --version 0.4.3 cargo-crap` if they are unavailable.
- Treat formatting differences, clippy findings, missed and timed-out mutants, and CRAP scores above the configured threshold as failures. Report any that cannot be resolved.

## End-to-end tests

- Write E2E tests as black-box tests. Drive the application only through its public interface (CLI, HTTP API, or UI) and assert only on externally observable results such as output, exit codes, responses, persisted files, or rendered UI.
- Do not import internal modules, call internal functions, mock internal components, or inspect internal state from E2E tests. Set up preconditions through the public interface where possible. Test harness code that starts real external dependencies (servers, databases, proxies) is fine.

## CodeRabbit

- CodeRabbit does not review pull requests automatically. Each PR gets one CodeRabbit review.
- Request it only when the PR is finished: your own review rounds are done, CI passes, and the branch is rebased onto its base. For a stack, wait until every layer is finished, then request one review per layer. Use `gh pr comment <n> --body "@coderabbitai review"`.
- Fix all actionable findings in one push and add a regression test for every fix that changes behavior. Reply on each thread with the commit that fixes it, or the reason for declining.
- Before replying, review the fix commits against the commit CodeRabbit reviewed, and check that each "Fixed in <sha>" reply points to a commit that addresses that finding.
- Do not request another review, except when a fix changes production behavior for a Major or Critical finding. Then request one incremental review with `@coderabbitai review`.
- If CodeRabbit replies that the rate limit is exceeded, wait the stated time, then trigger the request again. Count from when its message was posted; the number in it does not count down.

## Trust model

jcli assumes good user behavior:

- Configuration files and directories (`~/.config/jcli/config.toml`, the CWD `.env`) are set up properly.
- Values such as `JIRA_BASE_URL` are properly formatted.

Do not add in-depth validation or defensive checks for local input: no
size caps, symlink or FIFO hardening, permission probes, or
transport-scheme allowlists. Validate only what the code needs in order to
function (for example, the base URL must parse as a URL so relative API
paths can be joined onto it).

Failures must still speak: report a short one-line error on stderr that
names the missing setting or the affected file, exit non-zero, and never
echo credentials or file contents.
