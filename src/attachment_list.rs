use serde::{Deserialize, Serialize};

use crate::issue_key::IssueKey;
use crate::jira::attachment::Attachment;
use crate::output;

/// Stable JSON output of `jcli issue attachments list`. Attachments keep
/// Jira's order. Timestamps and sizes are passed through as Jira returns
/// them.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentListOutput {
    pub issue: String,
    pub returned: usize,
    pub attachments: Vec<AttachmentListEntry>,
}

/// One attachment. Missing or null `created` and `mimeType` stay null.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentListEntry {
    pub id: String,
    pub filename: String,
    pub size: i64,
    pub created: Option<String>,
    pub mime_type: Option<String>,
}

impl AttachmentListOutput {
    pub fn from_attachments(issue_key: &IssueKey, attachments: Vec<Attachment>) -> Self {
        let attachments: Vec<AttachmentListEntry> = attachments
            .into_iter()
            .map(|attachment| AttachmentListEntry {
                id: attachment.id,
                filename: attachment.filename,
                size: attachment.size,
                created: attachment.created,
                mime_type: attachment.mime_type,
            })
            .collect();
        AttachmentListOutput {
            issue: issue_key.as_str().to_owned(),
            returned: attachments.len(),
            attachments,
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

    fn key() -> IssueKey {
        "ABC-123".parse().unwrap()
    }

    fn attachment(id: &str, filename: &str, size: i64) -> Attachment {
        Attachment {
            id: id.to_string(),
            filename: filename.to_string(),
            size,
            created: None,
            mime_type: None,
        }
    }

    #[test]
    fn renders_the_exact_json_contract_in_jira_order() {
        let attachments = vec![
            Attachment {
                created: Some("2026-10-26T02:13:45.123+0100".to_string()),
                mime_type: Some("text/html".to_string()),
                ..attachment("10002", "report.html", 2048)
            },
            attachment("10001", "log.txt", 0),
        ];

        let output = AttachmentListOutput::from_attachments(&key(), attachments);

        assert_eq!(
            output.to_pretty_json(),
            r#"{
  "issue": "ABC-123",
  "returned": 2,
  "attachments": [
    {
      "id": "10002",
      "filename": "report.html",
      "size": 2048,
      "created": "2026-10-26T02:13:45.123+0100",
      "mimeType": "text/html"
    },
    {
      "id": "10001",
      "filename": "log.txt",
      "size": 0,
      "created": null,
      "mimeType": null
    }
  ]
}
"#
        );
    }

    #[test]
    fn an_issue_without_attachments_renders_an_empty_list() {
        let output = AttachmentListOutput::from_attachments(&key(), Vec::new());

        assert_eq!(
            output.to_pretty_json(),
            "{\n  \"issue\": \"ABC-123\",\n  \"returned\": 0,\n  \"attachments\": []\n}\n"
        );
    }

    #[test]
    fn filenames_escape_terminal_controls_and_round_trip() {
        let filename = "report\u{9B}31m\u{202E}lmth.exe";
        let output =
            AttachmentListOutput::from_attachments(&key(), vec![attachment("1", filename, 1)]);

        let json = output.to_pretty_json();

        assert!(json.contains("report\\u009B31m\\u202Elmth.exe"));
        let parsed: AttachmentListOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, output);
    }
}
