use serde::{Deserialize, Serialize};

use crate::jira::search::SearchPage;
use crate::output;

/// Stable JSON output of `jcli issue search`. Issues keep Jira's order, so
/// ordering is controlled through `ORDER BY` in the JQL. Timestamps are
/// passed through as Jira returns them. `truncated` is null when Jira did
/// not say whether this is the last page.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSearchOutput {
    pub jql: String,
    pub returned: usize,
    pub truncated: Option<bool>,
    pub issues: Vec<IssueSearchEntry>,
}

/// One search hit. Missing or null fields stay null, except labels, which
/// become an empty list.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSearchEntry {
    pub key: String,
    pub issue_type: Option<String>,
    pub status: Option<String>,
    pub summary: Option<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub labels: Vec<String>,
}

impl IssueSearchOutput {
    /// Builds the CLI output from the first search page. A missing or null
    /// issue list counts as an empty result.
    pub fn from_page(jql: &str, page: SearchPage) -> Self {
        let issues: Vec<IssueSearchEntry> = page
            .issues
            .unwrap_or_default()
            .into_iter()
            .map(|issue| {
                let fields = issue.fields;
                IssueSearchEntry {
                    key: issue.key,
                    issue_type: fields.issue_type.and_then(|issue_type| issue_type.name),
                    status: fields.status.and_then(|status| status.name),
                    summary: fields.summary,
                    created: fields.created,
                    updated: fields.updated,
                    labels: fields.labels.unwrap_or_default(),
                }
            })
            .collect();
        IssueSearchOutput {
            jql: jql.to_owned(),
            returned: issues.len(),
            truncated: page.is_last.map(|is_last| !is_last),
            issues,
        }
    }

    /// Renders the output as stable pretty JSON with one trailing newline,
    /// escaping C1 controls, DEL, and bidi code points.
    pub fn to_pretty_json(&self) -> String {
        output::to_pretty_json(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(value: serde_json::Value) -> SearchPage {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn renders_the_exact_json_contract() {
        let output = IssueSearchOutput::from_page(
            "project = ABC ORDER BY created DESC",
            page(json!({
                "issues": [{
                    "id": "10001",
                    "key": "ABC-1",
                    "fields": {
                        "summary": "Fix the thing",
                        "issuetype": {"name": "Bug"},
                        "status": {"name": "Open"},
                        "created": "2026-10-17T02:13:45.123+0200",
                        "updated": "2026-10-17T03:00:00.000+0200",
                        "labels": ["backend", "urgent"]
                    }
                }],
                "isLast": true
            })),
        );

        assert_eq!(
            output.to_pretty_json(),
            r#"{
  "jql": "project = ABC ORDER BY created DESC",
  "returned": 1,
  "truncated": false,
  "issues": [
    {
      "key": "ABC-1",
      "issueType": "Bug",
      "status": "Open",
      "summary": "Fix the thing",
      "created": "2026-10-17T02:13:45.123+0200",
      "updated": "2026-10-17T03:00:00.000+0200",
      "labels": [
        "backend",
        "urgent"
      ]
    }
  ]
}
"#
        );
    }

    #[test]
    fn truncation_follows_is_last() {
        let more = IssueSearchOutput::from_page("x", page(json!({"issues": [], "isLast": false})));
        let last = IssueSearchOutput::from_page("x", page(json!({"issues": [], "isLast": true})));
        let unknown = IssueSearchOutput::from_page("x", page(json!({"issues": []})));

        assert_eq!(more.truncated, Some(true));
        assert_eq!(last.truncated, Some(false));
        assert_eq!(unknown.truncated, None);
        let parsed: serde_json::Value = serde_json::from_str(&unknown.to_pretty_json()).unwrap();
        assert!(parsed["truncated"].is_null());
    }

    #[test]
    fn missing_values_stay_null_and_labels_become_empty() {
        let output = IssueSearchOutput::from_page(
            "x",
            page(json!({
                "issues": [
                    {"key": "ABC-1", "fields": {
                        "summary": null,
                        "issuetype": {"name": null},
                        "status": null,
                        "labels": null
                    }},
                    {"key": "ABC-2"}
                ]
            })),
        );

        assert_eq!(output.returned, 2);
        for issue in &output.issues {
            assert_eq!(issue.issue_type, None);
            assert_eq!(issue.status, None);
            assert_eq!(issue.summary, None);
            assert_eq!(issue.created, None);
            assert_eq!(issue.updated, None);
            assert!(issue.labels.is_empty());
        }
        let parsed: serde_json::Value = serde_json::from_str(&output.to_pretty_json()).unwrap();
        assert!(parsed["issues"][1]["summary"].is_null());
        assert_eq!(parsed["issues"][1]["labels"], json!([]));
    }

    #[test]
    fn issues_keep_jira_order() {
        let output = IssueSearchOutput::from_page(
            "x",
            page(json!({"issues": [
                {"key": "ABC-3", "fields": {"created": "2026-01-01T00:00:00.000+0000"}},
                {"key": "ABC-1", "fields": {"created": "2026-03-01T00:00:00.000+0000"}},
                {"key": "ABC-2", "fields": {"created": "2026-02-01T00:00:00.000+0000"}}
            ]})),
        );

        let keys: Vec<&str> = output
            .issues
            .iter()
            .map(|issue| issue.key.as_str())
            .collect();
        assert_eq!(keys, ["ABC-3", "ABC-1", "ABC-2"]);
    }

    #[test]
    fn missing_or_null_issue_lists_are_empty_results() {
        for value in [
            json!({"isLast": true}),
            json!({"issues": null, "isLast": true}),
        ] {
            let output = IssueSearchOutput::from_page("x", page(value));

            assert_eq!(output.returned, 0);
            assert!(output.issues.is_empty());
        }
    }

    #[test]
    fn pretty_json_escapes_terminal_controls_and_round_trips() {
        let summary = "Fix\u{202E}gnirts\u{9B}31m\u{7F}";
        let output = IssueSearchOutput::from_page(
            "summary ~ \"\u{2066}x\"",
            page(json!({"issues": [{"key": "ABC-1", "fields": {"summary": summary}}]})),
        );

        let json = output.to_pretty_json();

        for raw in ['\u{202E}', '\u{9B}', '\u{7F}', '\u{2066}'] {
            assert!(!json.contains(raw), "raw {raw:?} leaked");
        }
        assert!(json.contains("Fix\\u202Egnirts\\u009B31m\\u007F"));
        let parsed: IssueSearchOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, output);
    }
}
