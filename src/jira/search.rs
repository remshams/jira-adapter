use serde::Deserialize;

/// First page of `GET /rest/api/3/search/jql`. Jira's enhanced search
/// reports no total; `isLast` says whether more matches exist beyond this
/// page. The page token and unknown fields are ignored because only the
/// first page is ever fetched.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub issues: Option<Vec<SearchIssue>>,
    pub is_last: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchIssue {
    pub key: String,
    #[serde(default)]
    pub fields: SearchFields,
}

/// The fields requested by the search. Every one may be missing or null,
/// for example when a field is hidden from the account.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchFields {
    pub summary: Option<String>,
    #[serde(rename = "issuetype")]
    pub issue_type: Option<NamedValue>,
    pub status: Option<NamedValue>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub labels: Option<Vec<String>>,
}

/// A Jira object of which only the display name is used, such as a status
/// or an issue type.
#[derive(Debug, Clone, Deserialize)]
pub struct NamedValue {
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_page_and_ignores_unknown_fields() {
        let page: SearchPage = serde_json::from_value(serde_json::json!({
            "issues": [{
                "id": "10001",
                "key": "ABC-1",
                "self": "https://ignored.example/issue/10001",
                "fields": {
                    "summary": "Fix the thing",
                    "issuetype": {"name": "Bug", "subtask": false},
                    "status": {"name": "Open", "statusCategory": {"key": "new"}},
                    "created": "2026-10-17T02:13:45.123+0200",
                    "updated": "2026-10-17T03:00:00.000+0200",
                    "labels": ["backend"],
                    "futureField": true
                }
            }],
            "nextPageToken": "opaque-token",
            "isLast": false,
            "futureTopLevel": 1
        }))
        .unwrap();

        assert_eq!(page.is_last, Some(false));
        let issues = page.issues.unwrap();
        assert_eq!(issues.len(), 1);
        let issue = &issues[0];
        assert_eq!(issue.key, "ABC-1");
        assert_eq!(issue.fields.summary.as_deref(), Some("Fix the thing"));
        assert_eq!(
            issue.fields.issue_type.as_ref().unwrap().name.as_deref(),
            Some("Bug")
        );
        assert_eq!(
            issue.fields.status.as_ref().unwrap().name.as_deref(),
            Some("Open")
        );
        assert_eq!(
            issue.fields.created.as_deref(),
            Some("2026-10-17T02:13:45.123+0200")
        );
        assert_eq!(
            issue.fields.updated.as_deref(),
            Some("2026-10-17T03:00:00.000+0200")
        );
        assert_eq!(
            issue.fields.labels.as_deref(),
            Some(&["backend".to_owned()][..])
        );
    }

    #[test]
    fn tolerates_null_and_missing_optional_values() {
        let page: SearchPage = serde_json::from_value(serde_json::json!({
            "issues": [
                {
                    "key": "ABC-1",
                    "fields": {
                        "summary": null,
                        "issuetype": null,
                        "status": {"name": null},
                        "created": null,
                        "updated": null,
                        "labels": null
                    }
                },
                {"key": "ABC-2", "fields": {}},
                {"key": "ABC-3"}
            ]
        }))
        .unwrap();

        assert_eq!(page.is_last, None);
        let issues = page.issues.unwrap();
        assert_eq!(issues.len(), 3);
        for issue in &issues {
            assert!(issue.fields.summary.is_none());
            assert!(issue.fields.issue_type.is_none());
            assert!(issue.fields.created.is_none());
            assert!(issue.fields.updated.is_none());
            assert!(issue.fields.labels.is_none());
        }
        assert!(issues[0].fields.status.as_ref().unwrap().name.is_none());
        assert!(issues[1].fields.status.is_none());
    }

    #[test]
    fn missing_and_null_issue_lists_parse_as_absent() {
        let missing: SearchPage =
            serde_json::from_value(serde_json::json!({"isLast": true})).unwrap();
        let null: SearchPage = serde_json::from_value(serde_json::json!({"issues": null})).unwrap();

        assert!(missing.issues.is_none());
        assert_eq!(missing.is_last, Some(true));
        assert!(null.issues.is_none());
    }
}
