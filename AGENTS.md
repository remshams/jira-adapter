# AGENTS.md

jcli is a read-only Rust CLI that coding agents use to read Jira Cloud. It prints
issues as Markdown, searches issues with JQL, downloads attachments and lists Xray
test runs.

Install it with `cargo install --path <checkout>`, which puts the `jcli` binary in
`~/.cargo/bin`. Configure it with environment variables, a `.env` file in the
working directory (copy `.env.tpl` and uncomment the settings you use), or
`$XDG_CONFIG_HOME/jcli/config.toml` (default `~/.config/jcli/config.toml`). The
environment wins over `.env`, and `.env` wins over the TOML file. The Xray settings
are only needed for `jcli xray ...` commands. Run `jcli --help` for the commands.
The agent skill in `skills/jira/SKILL.md` describes how to use jcli.

## Architecture documentation

Start with [the architecture document](docs/architecture.md) for modules,
dependencies, runtime flow, requests, output and tests. Update it in the same
change when modules, dependencies, requests or the CLI contract change. The
[ADR index](docs/adr/README.md) records decisions and explains when to add or
supersede a record.

Keep the architecture document in two formats with the same content:
`docs/architecture.md` for agents and diffs, and a self-contained
`docs/architecture.html` for human readers. Update both in the same change. Use the
`frontend-design` skill when creating or redesigning the HTML version. Keep the HTML
self-contained so it works offline, with portable source links and no
machine-specific paths.

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

## Development

Keep the project small and simple. Handle realistic failure modes only; avoid
speculative edge-case and race-condition hardening. See the Trust model section
for local input.

Enable the pre-commit hook once per clone with `git config core.hooksPath .githooks`.
It runs `cargo fmt --check`.

## Validation

- After completing a task or feature, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo machete`, the changed-code mutation check `git diff <base>... > mutants.diff && cargo mutants --in-diff mutants.diff` (`<base>` is the branch the PR targets, usually `origin/main`), `cargo llvm-cov --lcov --output-path lcov.info`, and `cargo crap --lcov lcov.info` before handoff. A full `cargo mutants` run is optional locally; CI runs it on `main`.
- Use cargo-mutants 27.1.0. Install it with `cargo install --locked cargo-mutants --version 27.1.0` if it is unavailable.
- Use cargo-machete 0.9.2. Install it with `cargo install --locked cargo-machete --version 0.9.2` if it is unavailable.
- Use cargo-llvm-cov 0.9.0 and cargo-crap 0.4.3. Install them with `rustup component add llvm-tools-preview`, `cargo +stable install --locked --version 0.9.0 cargo-llvm-cov`, and `cargo +stable install --locked --version 0.4.3 cargo-crap` if they are unavailable.
- Treat formatting differences, clippy findings, unused dependencies, missed and timed-out mutants, and CRAP scores above the configured threshold as failures. Report any that cannot be resolved.

## End-to-end tests

- Write E2E tests as black-box tests. Drive the application only through its public interface (CLI, HTTP API, or UI) and assert only on externally observable results such as output, exit codes, responses, persisted files, or rendered UI.
- Do not import internal modules, call internal functions, mock internal components, or inspect internal state from E2E tests. Set up preconditions through the public interface where possible. Test harness code that starts real external dependencies (servers, databases, proxies) is fine.

## GitHub issues

Follow the [GitHub issue guide](docs/github-issues.md) when creating or updating an
issue. Apply the `unslop` skill to the title and body before publishing.

## Pull request descriptions

Follow the [pull request description guide](docs/pull-request-descriptions.md)
when creating or updating a PR description. Apply the `unslop` skill before
publishing it. When you open a PR, decide whether it needs the `human review required`
label, as the guide describes, and set it.

Merge a PR with the label only after the maintainer approves it, through a GitHub review
or a message. Merge a PR without the label once CI passes and its CodeRabbit review is
handled. Leave the label on after approval.

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

## Commits

- Use Conventional Commits: `<type>(<scope>): <description>`, e.g. `feat(JiraClient): ...`.
  Valid types: `feat`, `fix`, `refactor`, `chore`, `build`, `docs`, `perf`, `test`,
  `style`, `ci`, `revert`.
- Include the affected component or module name as the scope, in PascalCase.
- For e2e changes, prefix the component/module scope with `e2e-`, e.g.
  `test(e2e-IssueSearch): ...`.
- Unless instructed otherwise, commit changes in meaningful implementation chunks.
- Never mix files from different components or modules in one commit. Each commit
  must have a single component/module scope. Split changes across modules into
  separate commits. Repository moves and codebase formatting may each use one commit.
