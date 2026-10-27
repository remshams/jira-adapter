use globset::{Glob, GlobMatcher};

use crate::jira::attachment::Attachment;

/// Parses one `--name` glob. Matching is case-sensitive.
pub fn parse_name_pattern(value: &str) -> Result<GlobMatcher, globset::Error> {
    Glob::new(value).map(|glob| glob.compile_matcher())
}

/// The attachments a caller asked for: an attachment is selected when its
/// original Jira filename matches any name pattern or its ID equals any given
/// ID. Without patterns and IDs, every attachment is selected.
#[derive(Debug, Clone, Default)]
pub struct AttachmentSelection {
    names: Vec<GlobMatcher>,
    ids: Vec<String>,
}

impl AttachmentSelection {
    pub fn new(names: Vec<GlobMatcher>, ids: Vec<String>) -> Self {
        AttachmentSelection { names, ids }
    }

    /// Keeps the selected attachments in Jira's order.
    pub fn select(&self, attachments: Vec<Attachment>) -> Vec<Attachment> {
        if self.names.is_empty() && self.ids.is_empty() {
            return attachments;
        }
        attachments
            .into_iter()
            .filter(|attachment| self.is_selected(attachment))
            .collect()
    }

    fn is_selected(&self, attachment: &Attachment) -> bool {
        self.ids.contains(&attachment.id)
            || self
                .names
                .iter()
                .any(|name| name.is_match(&attachment.filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(id: &str, filename: &str) -> Attachment {
        Attachment {
            id: id.to_string(),
            filename: filename.to_string(),
            size: 1,
            created: None,
            mime_type: None,
        }
    }

    fn attachments() -> Vec<Attachment> {
        vec![
            attachment("10001", "nightly-report.html"),
            attachment("10002", "server.log"),
            attachment("10003", "screenshot.png"),
            attachment("10004", "Report.HTML"),
        ]
    }

    fn selection(names: &[&str], ids: &[&str]) -> AttachmentSelection {
        AttachmentSelection::new(
            names
                .iter()
                .map(|name| parse_name_pattern(name).unwrap())
                .collect(),
            ids.iter().map(|id| id.to_string()).collect(),
        )
    }

    fn selected_ids(selection: &AttachmentSelection) -> Vec<String> {
        selection
            .select(attachments())
            .into_iter()
            .map(|attachment| attachment.id)
            .collect()
    }

    #[test]
    fn no_filter_selects_every_attachment() {
        assert_eq!(
            AttachmentSelection::default().select(attachments()),
            attachments()
        );
    }

    #[test]
    fn a_name_pattern_selects_matching_filenames() {
        assert_eq!(selected_ids(&selection(&["*.html"], &[])), ["10001"]);
    }

    #[test]
    fn an_id_selects_the_attachment_with_that_id() {
        assert_eq!(selected_ids(&selection(&[], &["10002"])), ["10002"]);
    }

    #[test]
    fn names_and_ids_together_select_either_match_in_jira_order() {
        assert_eq!(
            selected_ids(&selection(&["*.png"], &["10001"])),
            ["10001", "10003"]
        );
    }

    #[test]
    fn repeated_filters_select_any_match() {
        assert_eq!(
            selected_ids(&selection(&["*.log", "*.png"], &[])),
            ["10002", "10003"]
        );
        assert_eq!(
            selected_ids(&selection(&[], &["10004", "10001"])),
            ["10001", "10004"]
        );
    }

    #[test]
    fn an_attachment_matching_several_filters_is_selected_once() {
        assert_eq!(
            selected_ids(&selection(&["*.html", "nightly-*"], &["10001"])),
            ["10001"]
        );
    }

    #[test]
    fn filters_matching_nothing_select_nothing() {
        assert!(selected_ids(&selection(&["*.pdf"], &["99999"])).is_empty());
    }

    #[test]
    fn name_patterns_are_case_sensitive() {
        assert_eq!(selected_ids(&selection(&["*.HTML"], &[])), ["10004"]);
        assert_eq!(
            selected_ids(&selection(&["report*"], &[])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_invalid_pattern_is_rejected() {
        assert!(parse_name_pattern("report[").is_err());
    }
}
