# 0003: keep the tool small and trust local input

## Status

Accepted

## Date

2026-10-26

## Context

An earlier version of the configuration loader read `.env` and `config.toml` with
size caps, `O_NOFOLLOW` and `O_NONBLOCK` opens, regular-file checks, rejection of
variable substitution, an opt-in flag for plain HTTP and a policy against mixing
`.env` and TOML values. Each check guarded against a user attacking their own
machine. Together they made the loader much larger than its job and harder to
test.

## Decision

jcli assumes good user behavior. The configuration files and directories are set
up properly, and values such as `JIRA_BASE_URL` are well formed. Do not add deep
validation or defensive checks for local input: no size caps, symlink or FIFO
hardening, permission probes or transport-scheme allowlists. Validate only what
the code needs to work. For example, the base URL must parse as a URL so API paths
can be joined onto it.

Failures must still speak. Report a short one-line error on stderr that names the
missing setting or the affected file, exit non-zero, and never echo credentials or
file contents. A malformed or unreadable configuration file fails with a generic
error. Those errors don't yet say whether `.env` or `config.toml` failed, which
falls short of naming the affected file.

The trust covers local input only. Responses from Jira and Xray are still checked:
bodies are size-bounded, attachment IDs and sizes are validated and terminal
control characters are removed from output.

## Consequences

Configuration code stays small. A misconfigured machine produces a clear error
instead of a silent fallback, but jcli does not protect a user from files they
set up wrongly on purpose. Proposals for new local-input checks need a realistic
failure, not a hypothetical attacker on the user's own machine. The `AGENTS.md`
Trust model section states the same rule for contributors.
