# 0012: page issue search up to 1000 issues

## Status

Accepted

## Date

2026-10-27

## Context

`jcli issue search` sent one request to `GET rest/api/3/search/jql` and printed
that first page. `--limit` accepted 1 to 100, Jira's page size for this
endpoint. An agent that needed more matches had to split the JQL into narrower
queries, because jcli had no way to fetch the next page.

Jira's enhanced search reports no total. Each page carries `isLast` and, when more
matches exist, an opaque `nextPageToken` that requests the next page.

## Decision

- `--limit` accepts 1 to 1000. The default stays 20. There is no flag for
  fetching every match; `--limit` is the one bounded control.
- Each request asks for `min(100, remaining)` issues as `maxResults`. Every
  request after the first adds the previous page's `nextPageToken`; `jql` and
  `fields` stay the same.
- Paging stops when `--limit` issues are collected, when a page has
  `isLast: true`, when a page has no `nextPageToken`, or when a page adds no
  issues, so a server that keeps sending tokens for empty pages cannot keep
  jcli requesting forever.
- The output keeps its shape. Issues keep Jira's order across pages, and
  `returned` counts the issues of all pages. `truncated` is `!isLast` of the last
  fetched page: `true` when the limit, a missing token or an empty page stopped
  paging while Jira reports more matches, `false` on Jira's last page, and `null`
  when that page has no `isLast`.
- A failed page fails the whole command and stdout stays empty. A 400 on any page
  is reported as `invalid JQL query (400): ...`, as for the first page. Each
  page is read through the 16 MiB bounded metadata reader.
- The page loop lives in the Jira client. `nextPageToken` and the page type stay
  inside `src/jira/`; the client hands `issue_search` the collected issues and
  the last page's `isLast`.

This decision supersedes no clause of [ADR 0007](0007-output-for-agents.md).
ADR 0007 fixes the output format and the error rules, and paging keeps both. The
first-page limit was never recorded in an ADR.

## Consequences

Agents can read up to 1000 matches with one command and without rewriting the
JQL. A search with a high limit sends up to ten requests, so it takes longer and
counts more against Jira's rate limit. A failure on a later page discards the
issues already fetched. Each page is a separate request, so an issue that
changes between requests can be missed or appear twice.
