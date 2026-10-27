# 0013: issue links and comments in issue read

## Status

Accepted

## Date

2026-10-27

## Context

This record covers the links in `issue read`. The comments decision is added to
it with #20 before it is accepted.

`jcli issue read` printed the metadata and the description of an issue, but
none of its links. An agent couldn't see which issues block a ticket or which
pull requests are already linked to it, so it linked pull requests by hand in
PR descriptions.

Jira keeps two kinds of links. Issue links are part of the issue and come back
in its `issuelinks` field. Each one has a link type with an `inward` and an
`outward` relation text, such as `is blocked by` and `blocks`, and either an
`outwardIssue` or an `inwardIssue` with the linked issue's key, summary and
status. Remote links, such as pull request URLs, are not an issue field. They
come from `GET rest/api/3/issue/{key}/remotelink`, which is not paged, and each
one holds the target in `object.title` and `object.url`.

Comments were left out as well, so the discussion on a ticket never reached an
agent. They come from `GET rest/api/3/issue/{key}/comment`, which is paged with
`startAt` and `maxResults` and can be sorted oldest first with
`orderBy=created`. Each page holds `comments`, `startAt`, `maxResults` and
`total`. Each comment has `author.displayName`, a `created` timestamp and a
`body` in ADF, the format the description uses.

These response shapes come from Jira's REST API v3 documentation. They have not
been checked against a real Jira instance yet.

## Decision

- `issue read` adds `issuelinks` to the fields it requests and sends a second
  request, `GET rest/api/3/issue/{key}/remotelink`, after the issue request.
  Both are GET requests ([ADR 0005](0005-read-only-get-only-http-layer.md)),
  and the remote link response is read through the 16 MiB bounded metadata
  reader.
- The Jira client maps both responses to value objects in `src/jira/link.rs`.
  `IssueLink` holds the relation text, key, summary and status name of the
  linked issue. An outward link takes `type.outward` as its relation, an inward
  link `type.inward`. `RemoteLink` holds the title and URL. Jira's link shapes
  stay in that file.
- The output gets a `## Links` section after `## Description`. It is always
  shown. Issue links come first, then remote links, each in Jira's order, as one
  Markdown list:

  ```text
  - blocks ABC-2: <summary> (<status>)
  - is blocked by ABC-3: <summary> (<status>)
  - Remote: <title> <https://...>
  ```

  An issue without links shows `None`.
- Values go through the same single-line sanitizing and Markdown escaping as
  the other metadata. A missing or blank value shows as `Unknown`. A remote link
  URL shows as an autolink only when it is an absolute `http` or `https` URL,
  the rule ADF links follow ([ADR 0008](0008-hand-rolled-adf-to-markdown-renderer.md));
  otherwise only the title shows. jcli never requests a remote link URL.
- A failed remote link request fails the whole command with exit code 3, and
  stdout stays empty.
- Comments are opt-in with `issue read <KEY> --comments`. Without the flag
  jcli sends no comment request and the output stays as it is, because a long
  discussion can be much larger than the issue itself.
- With the flag, jcli sends `GET rest/api/3/issue/{key}/comment` with
  `startAt`, `maxResults` and `orderBy=created` after the remote link request.
  Each request asks for `min(100, 1000 - fetched)` comments from the number
  fetched so far. Paging stops at 1000 comments, when `startAt` plus the
  comments on the page reach `total`, when a page has no `total`, or when a
  page has no comments; the last rule ends a server that reports a higher
  total than it sends. Each page is a GET request read through the 16 MiB
  bounded metadata reader.
- `src/jira/comment.rs` maps each comment to the value object `Comment`, with
  the author's display name, `created` and the ADF body, and hands the
  renderer `Comments`: the fetched comments and the last page's `total`. The
  page shape and the author object stay in that file.
- The output gets a `## Comments` section after `## Links`, oldest first:

  ```text
  ### <author> · <created>

  <body rendered from ADF>
  ```

  An issue without comments shows `None`. When Jira's `total` is higher than
  the number of comments shown, a last line says
  `Showing the first <shown> of <total> comments.` Without a `total` no such
  line is printed.
- The author and the timestamp go through the same single-line sanitizing,
  Markdown escaping and timestamp rule as the issue's `Created` value. A
  missing or blank author or timestamp shows as `Unknown`, and a missing or
  empty body as `No content.`
- ADF headings in a comment body move three levels down, to at most level 6:
  a level 1 heading becomes `####`. A comment's own heading is `###`, so a
  heading in its body can no longer start a section that looks like
  `## Links` or another comment. Levels 3 to 6 all become `######`, which
  loses their order but keeps the page structure. Description headings keep
  their level.
- A failed comment request, on any page, fails the whole command with exit
  code 3, and stdout stays empty.

This decision supersedes no clause of [ADR 0007](0007-output-for-agents.md).
ADR 0007 says `issue read` prints Markdown, that stdout stays empty on failure,
that Jira values are reduced to safe text and that missing values show as
`Unknown` or `None`. The links and comments sections follow all four rules. The
list of sections in the `issue read` output was never recorded in an ADR.

## Consequences

Agents see an issue's links and linked pull requests without a JQL search or a
browser. Every `issue read` now sends two requests instead of one, so it takes
a little longer and counts twice against Jira's rate limit. A failure of the
remote link request, for example a permission problem, hides an issue that
could otherwise be read. Linked issues show only key, summary and status;
reading one needs another `issue read`. If Jira's real responses differ from the
documented shapes, a missing field shows as `Unknown`, but a field of another
JSON type fails the command with `Jira metadata response is invalid`.

With `--comments`, agents read the discussion on a ticket in the same page.
The flag adds one request per 100 comments, up to ten. A failed comment page,
for example a permission problem or a rate limit on a later page, hides the
whole issue. Comments added or deleted between page requests can be missed or
shown twice. An issue with more than 1000 comments shows only the oldest 1000,
so the latest discussion is the part that is cut. The comment's `updated`
timestamp, edit author and visibility restrictions are not shown.
