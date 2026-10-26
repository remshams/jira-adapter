# 0005: read-only, GET-only Jira client

## Status

Accepted

## Date

2026-10-26

## Context

Agents use jcli to pull issue content into their context. A read-only tool cannot
change a ticket by mistake, whatever an agent passes to it. Write operations may
come later, but they should be a conscious extension, not a parameter away.

## Decision

`JiraClient` sends only GET requests. It has no method for another HTTP verb, so
making jcli write to Jira means extending that type on purpose. Requests use Basic
authentication with the account email and an API token.

Credentials stay on the configured origin. Every request URL is built from the
configured base URL; resource URLs in Jira's responses are ignored. JSON requests
follow redirects only to the same scheme, host and port. Attachment content
requests follow no redirects.

Xray Cloud authenticates and answers GraphQL queries only over POST, so
`XrayClient` sends two POST requests: the API key exchange and the `getTestRuns`
query. Neither changes Xray data, and the client has no other request. It follows
no redirects because both requests carry credentials.

Both clients use a 10 second connect timeout and a 30 second request timeout.

## Consequences

Agents can call jcli without risk to Jira or Xray data. The Jira client's type
shows the read-only rule, and a review sees any new verb. For Xray the rule is
semantic: a review has to check that a new query or mutation does not write.
Adding write operations needs a new record and a new client method.
