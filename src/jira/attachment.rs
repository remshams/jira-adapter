use serde::Deserialize;

pub(crate) const MAX_ATTACHMENT_ID_BYTES: usize = 256;

/// The attachment metadata needed to list attachments and to plan and verify
/// a download. `created` and `mime_type` are only listed, never used for a
/// download, so a missing or null value stays `None`.
///
/// Jira also returns URLs for attachment resources. They are intentionally not
/// part of this model because jcli builds every request from its configured
/// Jira base URL.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub size: i64,
    pub created: Option<String>,
    pub mime_type: Option<String>,
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
    fn parses_only_the_modelled_attachment_metadata() {
        let issue: AttachmentIssue = serde_json::from_value(serde_json::json!({
            "fields": {
                "attachment": [{
                    "id": "opaque/id?value",
                    "filename": "report.pdf",
                    "size": 42,
                    "created": "2026-10-26T02:13:45.123+0100",
                    "mimeType": "application/pdf",
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
                created: Some("2026-10-26T02:13:45.123+0100".to_string()),
                mime_type: Some("application/pdf".to_string()),
            }]
        );
    }

    #[test]
    fn missing_or_null_listing_fields_stay_none() {
        for value in [
            serde_json::json!({"id": "1", "filename": "a", "size": 1}),
            serde_json::json!({
                "id": "1", "filename": "a", "size": 1, "created": null, "mimeType": null
            }),
        ] {
            let attachment: Attachment = serde_json::from_value(value).unwrap();

            assert_eq!(attachment.created, None);
            assert_eq!(attachment.mime_type, None);
        }
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
