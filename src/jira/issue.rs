use serde::Deserialize;

use crate::jira::attachment::Attachment;
use crate::jira::link::IssueLink;

/// A Jira Cloud account, as returned by `/rest/api/3/myself` and embedded in
/// issue fields such as assignee and reporter.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub account_id: String,
    pub display_name: Option<String>,
    pub email_address: Option<String>,
}

/// The numeric ID and the issue type name of one issue. Xray's GraphQL API
/// references issues by this ID, and the type name decides which Xray
/// filter applies. `issue_type` is `None` when Jira sends no type name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueReference {
    pub id: String,
    pub issue_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub key: String,
    pub fields: IssueFields,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueFields {
    pub summary: String,
    #[serde(rename = "issuetype")]
    pub issue_type: Option<IssueType>,
    pub status: Status,
    pub assignee: Option<Account>,
    pub reporter: Option<Account>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Jira's creation timestamp, kept exactly as returned.
    pub created: Option<String>,
    pub description: Option<AdfDocument>,
    #[serde(default)]
    pub attachment: Vec<Attachment>,
    #[serde(default, rename = "issuelinks")]
    pub issue_links: Vec<IssueLink>,
}

/// Jira can return an issue type object without a usable name, which
/// renders as unknown.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueType {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdfDocument {
    #[allow(
        dead_code,
        reason = "Jira's ADF version is validated by serde but not rendered"
    )]
    pub version: u32,
    #[allow(
        dead_code,
        reason = "Jira's ADF root type is parsed but only its content is rendered"
    )]
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
                "issuetype": null,
                "status": {"name": "Open"},
                "assignee": null,
                "reporter": null,
                "labels": [],
                "created": null,
                "description": null,
                "attachment": []
            }
        }))
        .unwrap();

        assert!(issue.fields.issue_type.is_none());
        assert!(issue.fields.reporter.is_none());
        assert!(issue.fields.created.is_none());
        assert!(issue.fields.description.is_none());
    }

    #[test]
    fn parses_issue_type_and_keeps_the_created_timestamp_verbatim() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "issuetype": {"name": "Bug", "subtask": false, "iconUrl": "https://ignored"},
                "status": {"name": "Open"},
                "created": "2026-10-17T02:13:45.123+0200"
            }
        }))
        .unwrap();

        assert_eq!(
            issue.fields.issue_type.unwrap().name.as_deref(),
            Some("Bug")
        );
        assert_eq!(
            issue.fields.created.as_deref(),
            Some("2026-10-17T02:13:45.123+0200")
        );
    }

    #[test]
    fn parses_issue_types_with_a_missing_or_null_name() {
        for issue_type in [serde_json::json!({}), serde_json::json!({"name": null})] {
            let issue: Issue = serde_json::from_value(serde_json::json!({
                "key": "ABC-123",
                "fields": {
                    "summary": "Summary",
                    "issuetype": issue_type,
                    "status": {"name": "Open"}
                }
            }))
            .unwrap();

            assert!(issue.fields.issue_type.unwrap().name.is_none());
        }
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
        assert!(issue.fields.issue_links.is_empty());
        assert!(issue.fields.issue_type.is_none());
        assert!(issue.fields.created.is_none());
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

    #[test]
    fn parses_issue_links_in_jira_order() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "issuelinks": [
                    {
                        "type": {"name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
                        "outwardIssue": {"key": "ABC-2"}
                    },
                    {
                        "type": {"name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
                        "inwardIssue": {"key": "ABC-3"}
                    }
                ]
            }
        }))
        .unwrap();

        let links = &issue.fields.issue_links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].relation.as_deref(), Some("blocks"));
        assert_eq!(links[0].key.as_deref(), Some("ABC-2"));
        assert_eq!(links[1].relation.as_deref(), Some("is blocked by"));
        assert_eq!(links[1].key.as_deref(), Some("ABC-3"));
    }
}
