use serde::Deserialize;

pub(crate) const MAX_ATTACHMENT_ID_BYTES: usize = 256;

/// The attachment metadata needed to plan and verify a download.
///
/// Jira also returns URLs for attachment resources. They are intentionally not
/// part of this model because jcli builds every request from its configured
/// Jira base URL.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub size: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttachmentIssue {
    pub fields: AttachmentFields,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttachmentFields {
    #[serde(default)]
    pub attachment: Vec<Attachment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_required_attachment_metadata() {
        let issue: AttachmentIssue = serde_json::from_value(serde_json::json!({
            "fields": {
                "attachment": [{
                    "id": "opaque/id?value",
                    "filename": "report.pdf",
                    "size": 42,
                    "content": "https://untrusted.example/content",
                    "self": "https://untrusted.example/metadata",
                    "thumbnail": "https://untrusted.example/thumbnail",
                    "author": {"displayName": "Ignored"}
                }]
            }
        }))
        .unwrap();

        assert_eq!(
            issue.fields.attachment,
            [Attachment {
                id: "opaque/id?value".to_string(),
                filename: "report.pdf".to_string(),
                size: 42,
            }]
        );
    }

    #[test]
    fn defaults_a_missing_attachment_field_to_empty() {
        let issue: AttachmentIssue =
            serde_json::from_value(serde_json::json!({"fields": {}})).unwrap();

        assert!(issue.fields.attachment.is_empty());
    }

    #[test]
    fn rejects_sizes_outside_the_signed_int64_model() {
        let error = serde_json::from_value::<Attachment>(serde_json::json!({
            "id": "id",
            "filename": "file",
            "size": 9_223_372_036_854_775_808_u64
        }));

        assert!(error.is_err());
    }
}
