# GitHub issues

Follow this guide when creating or updating a GitHub issue for jcli. Apply the `unslop` skill to the title and body before publishing.

## Opening summary

Start every issue body with a short paragraph summarizing the problem or requested feature. State what goes wrong or what the user should be able to do. Keep the title short and specific too.

For example, "`jcli issue read` drops the content of ADF tables and prints only their cell text. Render tables as Markdown tables so agents can read the rows and columns."

Add supporting details after the summary as needed. For a bug, include the command, reproduction steps, actual behavior, expected behavior and the exit code. For a feature, describe the intended behavior and how to verify it. Distinguish confirmed facts from assumptions, especially about the shape of Jira or Xray responses.

## Labels

Assign at least one relevant GitHub label to every issue. Check the repository's existing labels and select those that match the work, such as `bug`, `enhancement` or `documentation`. Apply labels through GitHub's issue metadata; mentioning them in the body does not assign them.

## Affected modules

Include an "Affected modules" section. Use the module names in the [architecture document](architecture.md) and list each affected module when you can identify it. Briefly explain its connection to the issue. Include the affected part of the CLI contract, such as a command, flag, output field or exit code, where relevant.

For example, an ADF table issue might list `src/adf/` for the rendering and `src/jira/issue.rs` if the model needs table attributes. Check the code before treating either as a confirmed cause.

If the impact is uncertain, say "Unknown" and describe what needs investigation. If no module is affected, say "None" and state the scope, such as documentation. Do not invent an architectural impact to fill the section.

## Before publishing

- Confirm that the body starts with a concise problem or feature summary.
- Assign at least one relevant repository label.
- List affected modules, or explain why they are unknown or none apply.
- Leave credentials, tokens and private issue content out of the body.
- Apply the `unslop` skill to the title and body, removing filler and keeping concrete details.
