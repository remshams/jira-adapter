#![allow(
    dead_code,
    reason = "step 6 will consume these issue and ADF models in CLI output"
)]

use serde::Deserialize;

/// A Jira Cloud account, as returned by `/rest/api/3/myself` and embedded in
/// issue fields such as assignee and reporter.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub account_id: String,
    pub display_name: Option<String>,
    pub email_address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub key: String,
    pub fields: IssueFields,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueFields {
    pub summary: String,
    pub status: Status,
    pub assignee: Option<Account>,
    pub reporter: Option<Account>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub description: Option<AdfDocument>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdfDocument {
    pub version: u32,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub content: Vec<AdfNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdfNode {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub content: Vec<AdfNode>,
    pub text: Option<String>,
    #[serde(default)]
    pub marks: Vec<AdfMark>,
    pub attrs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdfMark {
    #[serde(rename = "type")]
    pub mark_type: String,
    pub attrs: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_null_reporter_and_description() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "assignee": null,
                "reporter": null,
                "labels": [],
                "description": null
            }
        }))
        .unwrap();

        assert!(issue.fields.reporter.is_none());
        assert!(issue.fields.description.is_none());
    }

    #[test]
    fn preserves_unknown_adf_nodes_children_marks_and_attrs() {
        let node: AdfNode = serde_json::from_value(serde_json::json!({
            "type": "custom-node",
            "attrs": {"layout": {"width": 42}},
            "futureField": true,
            "content": [{
                "type": "text",
                "text": "Preserved child",
                "marks": [{
                    "type": "link",
                    "attrs": {"href": "https://example.com"},
                    "futureMarkField": true
                }]
            }]
        }))
        .unwrap();

        assert_eq!(node.node_type, "custom-node");
        assert_eq!(node.attrs.as_ref().unwrap()["layout"]["width"], 42);
        assert_eq!(node.content[0].text.as_deref(), Some("Preserved child"));
        assert_eq!(node.content[0].marks[0].mark_type, "link");
        assert_eq!(
            node.content[0].marks[0].attrs.as_ref().unwrap()["href"],
            "https://example.com"
        );
    }

    #[test]
    fn defaults_missing_labels_account_email_and_adf_collections() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "ABC-123",
            "futureIssueField": true,
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open", "futureStatusField": true},
                "assignee": {
                    "accountId": "assignee-id",
                    "displayName": "Assignee",
                    "futureAccountField": true
                },
                "reporter": null,
                "description": {
                    "version": 1,
                    "type": "doc",
                    "futureDocumentField": true
                },
                "futureFieldsValue": true
            }
        }))
        .unwrap();

        assert!(issue.fields.labels.is_empty());
        assert!(
            issue
                .fields
                .assignee
                .as_ref()
                .unwrap()
                .email_address
                .is_none()
        );
        assert!(issue.fields.description.unwrap().content.is_empty());
    }
}
