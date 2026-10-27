# 0010: list attachments and select which ones to download

## Status

Proposed

## Date

2026-10-27

## Context

Agents analysing nightly test reports need one HTML report from a ticket that
also carries logs, archives and screenshots. `jcli issue attachments <KEY>` could
only download every attachment, so agents downloaded all of them to find one file
and did the same again on every ticket they compared. Jira's issue response
already has each attachment's name, size, creation time and MIME type, so jcli can
show them without downloading anything.

## Decision

`jcli issue attachments` becomes a group of two subcommands:

- `jcli issue attachments list <KEY> --json` prints pretty JSON with `issue`,
  `returned` and `attachments`. Each attachment has `id`, `filename`, `size`,
  `created` and `mimeType` from the same `GET rest/api/3/issue/{key}?fields=attachment`
  request the download uses. Missing `created` and `mimeType` values are `null`.
  Attachments keep Jira's order, and an issue without attachments exits 0 with
  `"attachments": []`. `--json` is required, as for `issue search`. Listing works
  on every platform and creates no files.
- `jcli issue attachments download <KEY> [--out DIR]` downloads as ADR 0009
  describes.

The bare `jcli issue attachments <KEY>` form is removed and fails with clap's
usage error and exit 2. Agents learn the syntax from `jcli --help` and the jira
skill, which change with the binary, so jcli keeps no alias for the old form.

This partially supersedes ADR 0009, which made `jcli issue attachments <KEY>` the
download command, and the clause in ADR 0007 that lists `issue attachments` among
the Markdown commands: `list` prints JSON and `download` keeps the Markdown list.

## Consequences

An agent can see which attachments exist before downloading any. Callers using
the old form get a usage error until they switch to `download`. The list shows
Jira's filename, not the sanitized file name `download` writes, so agents still
take the file names on disk from the `download` output.
