use serde::Deserialize;

use crate::jira::issue::AdfDocument;

/// One issue comment: the author's display name, Jira's creation timestamp
/// as returned, and the ADF body. Each part is `None` when Jira leaves it out.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "JiraComment")]
pub struct Comment {
    pub author: Option<String>,
    pub created: Option<String>,
    pub body: Option<AdfDocument>,
}

/// The comments of all fetched pages, oldest first, and the `total` Jira
/// reported on the last fetched page, or `None` when Jira left it out.
#[derive(Debug, Clone)]
pub struct Comments {
    pub comments: Vec<Comment>,
    pub total: Option<usize>,
}

/// One page of `GET rest/api/3/issue/{key}/comment`. Unknown fields such as
/// `startAt` and `maxResults` are ignored; the client tracks its own offset.
#[derive(Debug, Deserialize)]
pub struct CommentPage {
    pub comments: Option<Vec<Comment>>,
    pub total: Option<usize>,
}

/// Jira's comment entry. Only the author's display name, `created` and the
/// body are kept.
#[derive(Deserialize)]
struct JiraComment {
    author: Option<JiraCommentAuthor>,
    created: Option<String>,
    body: Option<AdfDocument>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraCommentAuthor {
    display_name: Option<String>,
}

impl From<JiraComment> for Comment {
    fn from(comment: JiraComment) -> Self {
        Self {
            author: comment.author.and_then(|author| author.display_name),
            created: comment.created,
            body: comment.body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(value: serde_json::Value) -> CommentPage {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_page_keeps_author_name_created_body_and_total() {
        let page = page(json!({
            "startAt": 0,
            "maxResults": 100,
            "total": 1,
            "comments": [{
                "id": "10000",
                "self": "https://ignored.example/comment/10000",
                "author": {
                    "accountId": "private-id",
                    "displayName": "Jane Doe",
                    "emailAddress": "jane@example.com"
                },
                "updateAuthor": {"displayName": "Someone Else"},
                "created": "2026-10-17T02:13:45.123+0200",
                "updated": "2026-10-18T02:13:45.123+0200",
                "body": {
                    "version": 1,
                    "type": "doc",
                    "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Hi"}]}]
                }
            }]
        }));

        assert_eq!(page.total, Some(1));
        let comments = page.comments.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author.as_deref(), Some("Jane Doe"));
        assert_eq!(
            comments[0].created.as_deref(),
            Some("2026-10-17T02:13:45.123+0200")
        );
        let body = comments[0].body.as_ref().unwrap();
        assert_eq!(body.content[0].content[0].text.as_deref(), Some("Hi"));
    }

    #[test]
    fn missing_comment_parts_become_none() {
        for value in [
            json!({}),
            json!({"author": null, "created": null, "body": null}),
            json!({"author": {}}),
            json!({"author": {"displayName": null}}),
        ] {
            let comment: Comment = serde_json::from_value(value.clone()).unwrap();

            assert!(comment.author.is_none(), "{value}");
            assert!(comment.created.is_none(), "{value}");
            assert!(comment.body.is_none(), "{value}");
        }
    }

    #[test]
    fn missing_comments_and_total_become_none() {
        for value in [json!({}), json!({"comments": null, "total": null})] {
            let page = page(value.clone());

            assert!(page.comments.is_none(), "{value}");
            assert!(page.total.is_none(), "{value}");
        }
    }
}
