# Pull request descriptions

Write for a reviewer who has not read the conversation or issue history. Apply the `unslop` skill when writing or editing a PR description. Use plain English, concrete behavior, and links that help the reviewer check your claims. Keep the description current as the scope changes.

Use the sections below, with the opening description first. Scale the detail to the change. A documentation fix may need only a sentence per section; a CLI contract change needs enough detail to assess compatibility and rollout. Replace examples with facts about the PR.

## Opening description

Start with a short paragraph explaining the problem, what motivated the change, and the resulting behavior. Name the command or failure that prompted the work when relevant. Link the originating issue if there is one, but include enough context to understand the PR without opening it.

For example, explain that `jcli issue search` reported every rejected JQL query as a bare HTTP 400, and that the change surfaces Jira's error messages on one line so an agent can fix the query. Avoid opening with a list of files or a chronology of implementation work.

## Visual summary

Follow the opening description with a visual when it shows the change faster than prose: pseudocode, a call tree, a file tree, a Mermaid diagram, or a `diff` sketch of one of these. Pick the smallest view that makes the key point clear. Skip this section when a sentence says enough, such as for a documentation fix.

Read the [pull request visuals guide](pull-request-visuals.md) for an example of each type and when to use it.

## Architecture impact

Use the module names in the [architecture document](architecture.md). Include a table with one row for each affected module. Describe changes to its responsibilities, dependencies, contracts, or communication with other modules. Include affected parts of the CLI contract, such as output fields or exit codes, where relevant.

The following rows illustrate a hypothetical search output change.

| Module | What changed | Effect on callers or boundaries |
| --- | --- | --- |
| `src/jira/client.rs` | Requests the `assignee` field in the search. | The search request asks Jira for one more field. |
| `src/issue_search.rs` | Adds `assignee` to each issue in the JSON output. | Agents receive the expanded `issue search --json` contract. |

If no module changes, keep the table and use a single row stating that, with the actual scope, such as documentation or tooling. Link any updated architecture documentation or ADRs. Do not claim an ownership or dependency change when the PR only updates behavior inside an existing module.

## Important decisions

Explain choices that a reviewer could reasonably question. State the chosen approach, the constraint it addresses, and the tradeoff. Mention a rejected alternative only when it explains the decision. Link relevant ADRs rather than repeating them. If the PR introduces no such decisions, say so briefly.

## After merge

State the actions needed after merging, who should perform them, and their order when it matters. Include reinstalling jcli, configuration changes, new settings, API token or Xray API key scope changes, or follow-up work that the PR requires.

For a CLI contract change, spell out which agent scripts or skill instructions need updating and whether update order matters. For example, if a flag is renamed, explain whether callers must update together with the reinstalled binary or whether the old flag still works. State any rollback constraints. If merging requires no further action, write "No post-merge action required."

## Risk

Choose `small`, `medium`, or `high` and explain the rating in terms of what could fail and who would be affected. Diff size alone does not determine risk.

- `small` fits a contained change with limited consequences and a straightforward revert, such as a documentation correction.
- `medium` fits a behavior change that can affect an agent workflow or several modules but has a practical recovery path.
- `high` fits a change that could overwrite or lose downloaded files, leak credentials, break agent scripts, or change data in Jira or Xray.

Name the main failure case, the validation that reduces the risk, and any remaining uncertainty. Explain recovery steps when reverting the commit alone would not restore the previous behavior.

End the section with the human review decision, for example "Human review: required, because the JSON output of `issue search` changes." See [Human review label](#human-review-label).

## Evidence

Show concrete before and after evidence that the change works.

Show execution output: the test that failed before the change and passes after it, or the command output before and after, such as the JSON result, the stderr line and the exit code. Pseudocode is fine when the full test is long. Never paste credentials or private issue content; use the test fixtures or redact.

For a change with no observable behavior, such as documentation or a refactor, say so and point to the validation below.

## Validation and CI

List the CI jobs executed for the current PR revision and their results: Lint, Test, Coverage and CRAP, Unused dependencies, Mutants (changed code) and `ci-success`. Link the CI runs so reviewers can inspect failures or warnings. Report local checks separately, with the commands and results, so reviewers can tell which checks CI has confirmed. The local checks are listed in the Validation section of `AGENTS.md`.

For each skipped step, name it and explain why it is unnecessary for this PR's scope. For example, mutation checks find no mutants in a documentation-only change because it touches no Rust code. If a required check could not run because of a missing tool, platform, or another blocker, state that limitation and what remains unverified. Say whether new behavior was checked against a real Jira or Xray instance or only against `httpmock`. Mark pending checks as pending and update the description when they finish.

## Review focus

Direct the reviewer to the parts that need judgment. Link the relevant files, tests, or ADRs and explain what to verify, such as credential redaction, response size limits, exit codes or JSON output compatibility. Mention test gaps and assumptions that need confirmation.

Keep this specific to the PR. "Check that the API token is redacted when Jira quotes it in a field name" gives the reviewer a useful question; "review the code carefully" does not.

## Human review label

When you open a PR, decide whether a maintainer has to review it before it merges. If so, add the `human review required` label, for example with `gh pr create --label "human review required"` or `gh pr edit <n> --add-label "human review required"`. The label is not a section of the description. Record the decision and its reason at the end of the Risk section.

Add the label when any of these apply:

- The risk is `medium` or `high`.
- jcli sends a request it did not send before that could change data: a non-GET request to Jira, or an Xray request other than the API key exchange and read-only GraphQL queries. This ends the read-only design in [ADR 0005](adr/0005-read-only-get-only-http-layer.md).
- The PR changes the CLI contract: commands, flags, the Markdown or JSON output shape, or exit codes.
- The PR touches authentication, tokens, configuration sources or the redaction of credentials.
- The PR changes dependencies, CI, the quality gates or the pre-commit hook.
- The PR adds an architecture decision record or changes module boundaries.
- New behavior rests on Jira or Xray responses nobody has checked against a real instance.

Leave the label off for `small` changes with no behavior change, such as documentation, test-only changes, formatting or refactors that keep the CLI contract. If you are unsure, add it.

Decide again when the scope changes, and add or remove the label to match.
