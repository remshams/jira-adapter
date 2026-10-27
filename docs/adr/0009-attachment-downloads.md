# 0009: stream attachments into verified, atomically replaced files

## Status

Accepted

## Date

2026-10-26

## Context

Issue attachments include archives, screenshots and videos of several gigabytes.
Jira controls attachment IDs, file names and sizes. A download that stops halfway
must not leave a truncated file under its final name, and a rerun should replace
earlier downloads.

## Decision

`jcli issue attachments <KEY>` downloads all attachments of an issue into
`./<KEY>/`, or directly into the directory given with `--out`. It runs on Unix
only.

- Plan every download before the first download. Reject unsafe or duplicate IDs,
  negative sizes, files over 20 GiB, more than 1,000 attachments, more than
  100 GiB in total and file names that collide case-insensitively.
- Name each file `<id>-<sanitized filename>`, at most 240 bytes, so names never
  contain path separators and two attachments with the same name do not overwrite
  each other.
- Create a missing output directory with mode `0700`.
- Download attachments one at a time. Stream each into a private temporary file
  in the output directory. The byte count must match the size in Jira's metadata
  exactly. Then atomically replace the final file name.
- If an attachment fails, keep the files already replaced, remove the temporary
  file and print nothing on stdout.

The request timeout acts as an idle timeout on the body stream, so a long download
continues while data arrives.

## Consequences

A final file name only ever holds a complete download of the expected size, and
memory use stays flat for large files. A failed run can leave a mix of new and
older files, and nothing on stdout says which were replaced. Windows is not
supported. The user owns the output directory, and jcli does not check it beyond
creating it.
