# 0006: configuration sources and precedence

## Status

Accepted

## Date

2026-10-26

## Context

jcli needs a Jira base URL, email and API token, and for Xray commands a base URL,
client ID and client secret. Agents run it in CI-like shells with exported
variables, in project checkouts that keep a `.env`, and on workstations where a
per-user file is easier. A wrong value must not silently mix with a value from
another source.

## Decision

Read each setting from three sources, in this order:

1. The process environment: `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN`,
   `XRAY_BASE_URL`, `XRAY_CLIENT_ID`, `XRAY_CLIENT_SECRET`.
2. The `.env` file in the current working directory only, parsed with dotenvy
   without changing the process environment. A duplicate key keeps its first
   value, like `dotenvy::from_path`.
3. A flat per-user TOML file at `$XDG_CONFIG_HOME/jcli/config.toml` when
   `XDG_CONFIG_HOME` is set, non-empty and absolute, otherwise
   `~/.config/jcli/config.toml`. Keys are the lowercase setting names without the
   `JIRA_` prefix (`base_url`, `email`, `api_token`, `xray_base_url`,
   `xray_client_id`, `xray_client_secret`). Unknown keys are an error. There is no
   system-wide file.

Precedence applies per setting. A value present in a higher source is
authoritative even when it is blank or, in the environment, not valid Unicode: it
fails as empty instead of falling through. A `.env` or TOML file that is not valid
UTF-8 fails as a whole. Only an absent value falls through. Errors name the
setting, never its value. Jira-only commands do not load the Xray settings.

A base URL only has to parse as a URL; jcli adds a trailing slash so API paths
append to it.

## Consequences

Exported variables always win, which suits CI and one-off overrides. A blank
export cannot quietly combine with a token from a file. Both files are read on
every run, so a broken file fails a command even when the environment holds every
value. Users with a token in a file should make it readable only by themselves;
jcli does not check this ([ADR 0003](0003-keep-the-tool-small-and-trust-local-input.md)).
