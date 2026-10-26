# 0008: hand-rolled ADF-to-Markdown renderer

## Status

Accepted

## Date

2026-10-26

## Context

Jira Cloud's REST API v3 returns issue descriptions in the Atlassian Document
Format (ADF), a JSON tree. Agents read Markdown better than ADF JSON. jcli needs a
predictable rendering that cannot inject terminal control sequences or unexpected
Markdown structure, and an unknown node type must not make an issue unreadable.

## Decision

Render ADF with a renderer written for jcli in `src/adf/markdown.rs`, without a
third-party ADF crate. It covers the nodes issue descriptions use: paragraphs,
headings, bullet and ordered lists, list items, code blocks, blockquotes and hard
breaks, plus the `strong`, `em`, `code` and `link` marks. Links render only for
absolute `http` and `https` URLs. Code block languages render only when they are
made of safe characters.

For an unknown node, render its own text and the known child content it contains
instead of failing the issue. Unit tests cover each node type.

## Consequences

jcli controls escaping and sanitizing for every node, and the output stays stable
across Jira changes to rarely used nodes. Tables, panels, mentions, media and
other unsupported nodes lose their structure and show only their text. Supporting
a new node means extending the renderer and its tests.
