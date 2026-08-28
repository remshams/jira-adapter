# AGENTS.md

Jira adapter is a cli tool that agents are using to connect to jira e.g. for reading issues, downloading attachments and the like

## General

- Always apply the `unslop` skill when writing or editing any text (responses, code comments, docs, commit messages, etc.)

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
