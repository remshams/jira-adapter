# 0010: list attachments and select which ones to download

## Status

Accepted

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

Both subcommands accept `--name <GLOB>` and `--id <ID>`, each repeatable:

- An attachment is selected when its ID equals any `--id` or its filename matches
  any `--name`. Without filters every attachment is selected. Selected
  attachments keep Jira's order.
- `--name` matches Jira's original filename, not the sanitized file name on disk.
  Matching is case-sensitive. The `globset` crate, without default features,
  parses and matches the globs. Patterns come from the caller and are trusted
  (ADR 0003); filenames come from Jira and are matched in linear time.
- clap parses the globs, so an invalid glob is a usage error with exit 2 before
  jcli loads configuration or sends a request.
- Filters that match nothing exit 0: `list` prints `"attachments": []` and
  `download` prints `No attachments.` without creating a directory.
- `download` plans, checks and downloads only the selected attachments. The ADR
  0009 limits on count, size and file name collisions apply to the selected set.

This partially supersedes [ADR 0009](0009-attachment-downloads.md), which made
`jcli issue attachments <KEY>` the download command for every attachment and
applied its limits to all of them. It also partially supersedes two clauses of
[ADR 0007](0007-output-for-agents.md): the one that lists `issue attachments`
among the Markdown commands, because `list` prints JSON and `download` keeps the
Markdown list, and the one that names the commands that require `--json`, which
now include `issue attachments list`.

## Consequences

An agent can see which attachments exist before downloading any, then download
only the files it needs. Callers using the old form get a usage error until they
switch to `download`. The list shows Jira's filename, not the sanitized file name
`download` writes, so agents still take the file names on disk from the
`download` output. An issue whose full attachment set exceeds a limit can still be
downloaded in parts with filters. jcli gains one dependency, `globset`, and its
dependency `bstr`; its other dependencies were already in the build.
