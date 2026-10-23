# AGENTS.md

Jira adapter is a cli tool that agents are using to connect to jira e.g. for reading issues, downloading attachments and the like

## General

- Always apply the `unslop` skill when writing or editing any text (responses, code comments, docs, commit messages, etc.)

## Validation

- After completing a task or feature, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo mutants`, `cargo llvm-cov --lcov --output-path lcov.info`, and `cargo crap --lcov lcov.info` before handoff.
- Use cargo-mutants 27.1.0. Install it with `cargo install --locked cargo-mutants --version 27.1.0` if it is unavailable.
- Use cargo-llvm-cov 0.9.0 and cargo-crap 0.4.3. Install them with `rustup component add llvm-tools-preview`, `cargo +stable install --locked --version 0.9.0 cargo-llvm-cov`, and `cargo +stable install --locked --version 0.4.3 cargo-crap` if they are unavailable.
- Treat formatting differences, clippy findings, missed and timed-out mutants, and CRAP scores above the configured threshold as failures. Report any that cannot be resolved.

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
