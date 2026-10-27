use serde::Deserialize;

/// One issue link as seen from the issue being read: the relation text, such
/// as `blocks` or `is blocked by`, and the linked issue's key, summary and
/// status name. Each part is `None` when Jira leaves it out.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "JiraIssueLink")]
pub struct IssueLink {
    pub relation: Option<String>,
    pub key: Option<String>,
    pub summary: Option<String>,
    pub status: Option<String>,
}

/// One remote link, such as a pull request URL, with Jira's title and URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "JiraRemoteLink")]
pub struct RemoteLink {
    pub title: Option<String>,
    pub url: Option<String>,
}

/// Jira's `issuelinks` entry. A link holds either `outwardIssue` or
/// `inwardIssue`; the link type names the relation for each direction.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraIssueLink {
    #[serde(rename = "type")]
    link_type: Option<JiraIssueLinkType>,
    outward_issue: Option<JiraLinkedIssue>,
    inward_issue: Option<JiraLinkedIssue>,
}

#[derive(Default, Deserialize)]
struct JiraIssueLinkType {
    inward: Option<String>,
    outward: Option<String>,
}

#[derive(Default, Deserialize)]
struct JiraLinkedIssue {
    key: Option<String>,
    fields: Option<JiraLinkedIssueFields>,
}

#[derive(Default, Deserialize)]
struct JiraLinkedIssueFields {
    summary: Option<String>,
    status: Option<JiraNamed>,
}

#[derive(Deserialize)]
struct JiraNamed {
    name: Option<String>,
}

impl From<JiraIssueLink> for IssueLink {
    fn from(link: JiraIssueLink) -> Self {
        let link_type = link.link_type.unwrap_or_default();
        let (relation, issue) = match link.outward_issue {
            Some(issue) => (link_type.outward, issue),
            None => (link_type.inward, link.inward_issue.unwrap_or_default()),
        };
        let fields = issue.fields.unwrap_or_default();
        Self {
            relation,
            key: issue.key,
            summary: fields.summary,
            status: fields.status.and_then(|status| status.name),
        }
    }
}

/// Jira's remote link entry. Only `object.title` and `object.url` are kept.
#[derive(Deserialize)]
struct JiraRemoteLink {
    object: Option<JiraRemoteObject>,
}

#[derive(Default, Deserialize)]
struct JiraRemoteObject {
    title: Option<String>,
    url: Option<String>,
}

impl From<JiraRemoteLink> for RemoteLink {
    fn from(link: JiraRemoteLink) -> Self {
        let object = link.object.unwrap_or_default();
        Self {
            title: object.title,
            url: object.url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue_link(value: serde_json::Value) -> IssueLink {
        serde_json::from_value(value).unwrap()
    }

    fn link(relation: &str, key: &str, summary: &str, status: &str) -> IssueLink {
        IssueLink {
            relation: Some(relation.to_string()),
            key: Some(key.to_string()),
            summary: Some(summary.to_string()),
            status: Some(status.to_string()),
        }
    }

    const LINK_TYPE: &str = r#"{"name": "Blocks", "inward": "is blocked by", "outward": "blocks"}"#;

    fn link_type() -> serde_json::Value {
        serde_json::from_str(LINK_TYPE).unwrap()
    }

    #[test]
    fn an_outward_link_uses_the_outward_relation_and_the_outward_issue() {
        let parsed = issue_link(json!({
            "id": "10001",
            "type": link_type(),
            "outwardIssue": {
                "id": "10002",
                "key": "ABC-2",
                "fields": {"summary": "Other", "status": {"name": "In Progress"}}
            }
        }));

        assert_eq!(parsed, link("blocks", "ABC-2", "Other", "In Progress"));
    }

    #[test]
    fn an_inward_link_uses_the_inward_relation_and_the_inward_issue() {
        let parsed = issue_link(json!({
            "type": link_type(),
            "inwardIssue": {
                "key": "ABC-3",
                "fields": {"summary": "Blocker", "status": {"name": "Done"}}
            }
        }));

        assert_eq!(parsed, link("is blocked by", "ABC-3", "Blocker", "Done"));
    }

    #[test]
    fn missing_link_parts_become_none() {
        let empty = IssueLink {
            relation: None,
            key: None,
            summary: None,
            status: None,
        };
        for value in [
            json!({}),
            json!({"type": null, "inwardIssue": null}),
            json!({"type": {}, "inwardIssue": {}}),
            json!({"inwardIssue": {"fields": null}}),
            json!({"outwardIssue": {"fields": {"status": null}}}),
            json!({"outwardIssue": {"fields": {"status": {}}}}),
        ] {
            assert_eq!(issue_link(value.clone()), empty, "{value}");
        }
    }

    #[test]
    fn a_link_without_an_issue_still_takes_the_inward_relation() {
        let parsed = issue_link(json!({"type": link_type()}));

        assert_eq!(parsed.relation.as_deref(), Some("is blocked by"));
        assert_eq!(parsed.key, None);
    }

    #[test]
    fn remote_links_keep_the_object_title_and_url() {
        let links: Vec<RemoteLink> = serde_json::from_value(json!([
            {
                "id": 10000,
                "self": "https://ignored.example/self",
                "application": {"name": "GitHub"},
                "object": {
                    "url": "https://github.com/acme/repo/pull/1",
                    "title": "PR #1",
                    "summary": "ignored"
                }
            },
            {"id": 10001},
            {"object": null},
            {"object": {}}
        ]))
        .unwrap();

        let empty = RemoteLink {
            title: None,
            url: None,
        };
        assert_eq!(
            links,
            [
                RemoteLink {
                    title: Some("PR #1".to_string()),
                    url: Some("https://github.com/acme/repo/pull/1".to_string()),
                },
                empty.clone(),
                empty.clone(),
                empty,
            ]
        );
    }
}
