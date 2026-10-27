use std::collections::HashSet;
use std::fs::DirBuilder;
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile};
use thiserror::Error;

use crate::issue_key::IssueKey;
use crate::jira::attachment::{Attachment, MAX_ATTACHMENT_ID_BYTES};
use crate::jira::client::{JiraClient, JiraClientError};

const COPY_BUFFER_SIZE: usize = 64 * 1024;
const MAX_GENERATED_NAME_BYTES: usize = 240;
const MAX_INLINE_ID_BYTES: usize = 48;
const FALLBACK_FILENAME: &str = "attachment";
pub(crate) const MAX_ATTACHMENT_COUNT: usize = 1_000;
pub(crate) const MAX_ATTACHMENT_SIZE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub(crate) const MAX_TOTAL_ATTACHMENT_SIZE_BYTES: u64 = 100 * 1024 * 1024 * 1024;

pub(crate) trait ContentSource {
    fn content(&self, id: &str) -> Result<Box<dyn Read>, JiraClientError>;
}

impl ContentSource for JiraClient {
    fn content(&self, id: &str) -> Result<Box<dyn Read>, JiraClientError> {
        Ok(Box::new(self.attachment_content(id)?))
    }
}

#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error(transparent)]
    Jira(#[from] JiraClientError),
    #[error("Jira returned an unsafe attachment ID")]
    InvalidId,
    #[error("Jira returned duplicate attachment IDs")]
    DuplicateId,
    #[error("Jira returned an invalid attachment size")]
    InvalidSize,
    #[error("Jira returned too many attachments")]
    AttachmentCountLimit,
    #[error("a Jira attachment exceeds the download size limit")]
    AttachmentSizeLimit,
    #[error("Jira attachments exceed the aggregate download size limit")]
    AggregateSizeLimit,
    #[error("generated attachment filenames collide")]
    NameCollision,
    #[error("attachment content could not be read")]
    SourceRead,
    #[error("downloaded attachment size does not match Jira metadata")]
    SizeMismatch,
    #[error("attachment downloads are not supported on this platform")]
    UnsupportedPlatform,
    #[error("failed to access attachment files")]
    Filesystem {
        #[source]
        source: io::Error,
    },
}

impl AttachmentError {
    pub fn is_filesystem(&self) -> bool {
        matches!(self, Self::UnsupportedPlatform | Self::Filesystem { .. })
    }

    fn filesystem(source: io::Error) -> Self {
        Self::Filesystem { source }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DownloadManifest {
    names: Vec<String>,
}

impl DownloadManifest {
    pub fn stdout(&self, issue_key: &IssueKey) -> String {
        let mut output = format!("# {issue_key} attachments\n\n");
        if self.names.is_empty() {
            output.push_str("No attachments.\n");
            return output;
        }

        for name in &self.names {
            output.push_str("- ");
            output.push_str(name);
            output.push('\n');
        }
        output
    }
}

#[derive(Debug, Clone)]
struct PlannedAttachment {
    id: String,
    size: u64,
    name: String,
}

/// Downloads attachments in Jira response order. Each verified temporary file
/// replaces its final path before the next download starts.
pub fn download_all(
    source: &impl ContentSource,
    metadata: &[Attachment],
    output_directory: &Path,
) -> Result<DownloadManifest, AttachmentError> {
    if !cfg!(unix) {
        return Err(AttachmentError::UnsupportedPlatform);
    }

    let plans = plan_downloads(metadata)?;
    if !plans.is_empty() {
        prepare_output_directory(output_directory)?;
    }
    download_planned(source, plans, output_directory)
}

fn download_planned(
    source: &impl ContentSource,
    plans: Vec<PlannedAttachment>,
    output_directory: &Path,
) -> Result<DownloadManifest, AttachmentError> {
    let mut names = Vec::with_capacity(plans.len());
    for plan in plans {
        download_one(source, &plan, output_directory)?;
        names.push(plan.name);
    }
    Ok(DownloadManifest { names })
}

fn prepare_output_directory(path: &Path) -> Result<(), AttachmentError> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(AttachmentError::filesystem)
}

fn download_one(
    source: &impl ContentSource,
    plan: &PlannedAttachment,
    output_directory: &Path,
) -> Result<(), AttachmentError> {
    let mut temporary = create_temporary_file(output_directory)?;
    let download = (|| {
        let mut content = source.content(&plan.id)?;
        copy_exact_size(&mut content, &mut temporary, plan.size)?;
        temporary.flush().map_err(AttachmentError::filesystem)
    })();
    if let Err(error) = download {
        return fail_after_cleanup(temporary, error);
    }

    match temporary.persist(output_directory.join(&plan.name)) {
        Ok(_) => Ok(()),
        Err(error) => {
            let tempfile::PersistError { error, file } = error;
            fail_after_cleanup(file, AttachmentError::filesystem(error))
        }
    }
}

fn fail_after_cleanup(
    temporary: NamedTempFile,
    error: AttachmentError,
) -> Result<(), AttachmentError> {
    temporary.close().map_err(AttachmentError::filesystem)?;
    Err(error)
}

fn create_temporary_file(directory: &Path) -> Result<NamedTempFile, AttachmentError> {
    Builder::new()
        .prefix(".jcli-attachment-")
        .tempfile_in(directory)
        .map_err(AttachmentError::filesystem)
}

fn plan_downloads(metadata: &[Attachment]) -> Result<Vec<PlannedAttachment>, AttachmentError> {
    if metadata.len() > MAX_ATTACHMENT_COUNT {
        return Err(AttachmentError::AttachmentCountLimit);
    }

    let mut ids = HashSet::with_capacity(metadata.len());
    let mut plans = Vec::with_capacity(metadata.len());
    let mut total_size = 0_u64;

    for attachment in metadata {
        let plan = plan_download(attachment, &mut ids)?;
        total_size = add_to_total_size(total_size, plan.size)?;
        plans.push(plan);
    }

    detect_name_collisions(&plans)?;
    Ok(plans)
}

/// Validates one attachment and records its ID in `ids`, which rejects IDs
/// already seen earlier in the response.
fn plan_download<'a>(
    attachment: &'a Attachment,
    ids: &mut HashSet<&'a str>,
) -> Result<PlannedAttachment, AttachmentError> {
    validate_id(&attachment.id)?;
    if !ids.insert(attachment.id.as_str()) {
        return Err(AttachmentError::DuplicateId);
    }
    Ok(PlannedAttachment {
        id: attachment.id.clone(),
        size: attachment_size(attachment.size)?,
        name: generated_name(&attachment.id, &attachment.filename),
    })
}

fn validate_id(id: &str) -> Result<(), AttachmentError> {
    if id.is_empty() || id.len() > MAX_ATTACHMENT_ID_BYTES || matches!(id, "." | "..") {
        Err(AttachmentError::InvalidId)
    } else {
        Ok(())
    }
}

fn attachment_size(size: i64) -> Result<u64, AttachmentError> {
    let size = u64::try_from(size).map_err(|_| AttachmentError::InvalidSize)?;
    if size > MAX_ATTACHMENT_SIZE_BYTES {
        Err(AttachmentError::AttachmentSizeLimit)
    } else {
        Ok(size)
    }
}

fn add_to_total_size(total_size: u64, size: u64) -> Result<u64, AttachmentError> {
    let total_size = total_size
        .checked_add(size)
        .ok_or(AttachmentError::AggregateSizeLimit)?;
    if total_size > MAX_TOTAL_ATTACHMENT_SIZE_BYTES {
        Err(AttachmentError::AggregateSizeLimit)
    } else {
        Ok(total_size)
    }
}

fn detect_name_collisions(plans: &[PlannedAttachment]) -> Result<(), AttachmentError> {
    let mut names = HashSet::with_capacity(plans.len());
    if plans
        .iter()
        .any(|plan| !names.insert(plan.name.to_ascii_lowercase()))
    {
        Err(AttachmentError::NameCollision)
    } else {
        Ok(())
    }
}

fn copy_exact_size(
    source: &mut dyn Read,
    destination: &mut dyn Write,
    expected: u64,
) -> Result<(), AttachmentError> {
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    let mut copied = 0_u64;

    loop {
        let remaining_with_sentinel = expected.saturating_sub(copied).saturating_add(1);
        let read_limit = usize::try_from(remaining_with_sentinel)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = source
            .read(&mut buffer[..read_limit])
            .map_err(|_| AttachmentError::SourceRead)?;
        let Some(count) = std::num::NonZeroUsize::new(count) else {
            return finish_copy(copied, expected);
        };
        let count = count.get();

        copied = add_copied(copied, count, expected)?;
        destination
            .write_all(&buffer[..count])
            .map_err(AttachmentError::filesystem)?;
    }
}

fn finish_copy(copied: u64, expected: u64) -> Result<(), AttachmentError> {
    if copied == expected {
        Ok(())
    } else {
        Err(AttachmentError::SizeMismatch)
    }
}

fn add_copied(copied: u64, count: usize, expected: u64) -> Result<u64, AttachmentError> {
    let copied = copied
        .checked_add(count as u64)
        .ok_or(AttachmentError::SizeMismatch)?;
    if copied > expected {
        Err(AttachmentError::SizeMismatch)
    } else {
        Ok(copied)
    }
}

fn generated_name(id: &str, filename: &str) -> String {
    let id = safe_id(id);
    let filename_budget = MAX_GENERATED_NAME_BYTES - id.len() - 1;
    let filename = sanitized_filename(filename, filename_budget);
    format!("{id}-{filename}")
}

fn safe_id(id: &str) -> String {
    if id.len() <= MAX_INLINE_ID_BYTES && id.bytes().all(|byte| byte.is_ascii_digit()) {
        return id.to_string();
    }

    if id.len() <= MAX_INLINE_ID_BYTES {
        let mut encoded = String::with_capacity(3 + id.len() * 2);
        encoded.push_str("id-");
        append_hex(&mut encoded, id.as_bytes());
        encoded
    } else {
        let digest = Sha256::digest(id.as_bytes());
        let mut encoded = String::with_capacity(10 + digest.len() * 2);
        encoded.push_str("id-sha256-");
        append_hex(&mut encoded, &digest);
        encoded
    }
}

fn append_hex(output: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
}

fn sanitized_filename(filename: &str, max_bytes: usize) -> String {
    let collapsed = collapse_filename(filename, max_bytes);
    let sanitized = collapsed.trim_matches(['.', '-']);

    if sanitized.is_empty() {
        return FALLBACK_FILENAME[..FALLBACK_FILENAME.len().min(max_bytes)].to_string();
    }
    if is_windows_reserved_name(sanitized) {
        return prefix_reserved_name(sanitized, max_bytes);
    }
    sanitized.to_string()
}

/// Keeps ASCII letters, digits, `.`, and `-`, collapsing every run of other
/// characters between kept ones into a single `-`, within `max_bytes`.
fn collapse_filename(filename: &str, max_bytes: usize) -> String {
    let mut collapsed = String::with_capacity(filename.len().min(max_bytes));
    let mut pending_separator = false;

    for character in filename.chars() {
        if !is_kept_filename_character(character) {
            pending_separator = true;
            continue;
        }
        if !push_within_limit(&mut collapsed, character, pending_separator, max_bytes) {
            break;
        }
        pending_separator = false;
    }
    collapsed
}

fn is_kept_filename_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '-')
}

/// Pushes `character`, preceded by a `-` when `separate` is set and the
/// output is not empty. Returns `false` once the output is full.
fn push_within_limit(
    output: &mut String,
    character: char,
    separate: bool,
    max_bytes: usize,
) -> bool {
    if separate && !output.is_empty() && output.len() < max_bytes {
        output.push('-');
    }
    if output.len() == max_bytes {
        return false;
    }
    output.push(character);
    true
}

fn prefix_reserved_name(name: &str, max_bytes: usize) -> String {
    let prefix = "file-";
    let Some(remaining) = max_bytes.checked_sub(prefix.len()) else {
        return prefix[..max_bytes].to_string();
    };
    let mut prefixed = name.to_string();
    if prefixed.len() <= remaining {
        prefixed.insert_str(0, prefix);
    } else {
        prefixed.replace_range(..prefix.len().min(prefixed.len()), prefix);
    }
    prefixed
}

fn is_windows_reserved_name(filename: &str) -> bool {
    let stem = filename.split('.').next().unwrap_or_default();
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}
#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io::Cursor;
    use std::rc::Rc;
    use std::sync::{Mutex, mpsc};
    use std::thread;

    use httpmock::Method::GET;
    use httpmock::MockServer;
    use url::Url;

    use super::*;
    use crate::config::Config;

    struct FakeSource {
        responses: RefCell<Vec<Result<Box<dyn Read>, JiraClientError>>>,
        calls: Cell<usize>,
    }

    impl FakeSource {
        fn new(responses: Vec<Result<Box<dyn Read>, JiraClientError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                calls: Cell::new(0),
            }
        }

        fn bytes(contents: &[&[u8]]) -> Self {
            Self::new(
                contents
                    .iter()
                    .map(|content| Ok(Box::new(Cursor::new(content.to_vec())) as Box<dyn Read>))
                    .collect(),
            )
        }
    }

    impl ContentSource for FakeSource {
        fn content(&self, _id: &str) -> Result<Box<dyn Read>, JiraClientError> {
            self.calls.set(self.calls.get() + 1);
            self.responses.borrow_mut().remove(0)
        }
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
    fn resource_limits_match_the_download_contract() {
        assert_eq!(COPY_BUFFER_SIZE, 65_536);
        assert_eq!(MAX_ATTACHMENT_COUNT, 1_000);
        assert_eq!(MAX_ATTACHMENT_SIZE_BYTES, 21_474_836_480);
        assert_eq!(MAX_TOTAL_ATTACHMENT_SIZE_BYTES, 107_374_182_400);
    }

    #[test]
    fn generated_names_are_safe_deterministic_and_id_prefixed() {
        let first = generated_name("opaque/id?x", "../../a [report]\u{1b}.pdf");
        let second = generated_name("opaque/id?x", "../../a [report]\u{1b}.pdf");

        assert_eq!(first, second);
        assert!(first.starts_with("id-6f70617175652f69643f78-"));
        assert!(first.ends_with("a-report-.pdf"));
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        );
        assert!(!first.contains(".."));
        assert!(!first.contains('/') && !first.contains('\\'));
        assert!(!first.contains('\u{1b}'));
    }

    #[test]
    fn numeric_ids_stay_readable_and_case_varying_opaque_ids_stay_distinct() {
        assert_eq!(generated_name("10001", "file.pdf"), "10001-file.pdf");

        let upper = generated_name("Opaque-ID", "report?.pdf");
        let lower = generated_name("opaque-id", "report*.pdf");
        assert_ne!(upper.to_ascii_lowercase(), lower.to_ascii_lowercase());
        assert!(upper.starts_with("id-4f70617175652d4944-"));
        assert!(lower.starts_with("id-6f70617175652d6964-"));
    }

    #[test]
    fn sanitizes_empty_dot_bidi_and_reserved_names() {
        for unsafe_name in ["", ".", "..", "/\\", "\u{202e}\u{1b}"] {
            assert!(generated_name("id", unsafe_name).ends_with("-attachment"));
        }
        assert!(generated_name("id", "CON").ends_with("-file-CON"));
        assert!(generated_name("id", "nul.txt").ends_with("-file-nul.txt"));
    }

    #[test]
    fn long_utf8_names_and_ids_fit_the_byte_limit() {
        let name = generated_name(&"opaque".repeat(100), &"é文📎.pdf".repeat(200));

        assert!(name.starts_with("id-sha256-"));
        assert!(name.len() <= MAX_GENERATED_NAME_BYTES);
        assert!(name.is_ascii());

        let full_budget = generated_name("1", &"a".repeat(MAX_GENERATED_NAME_BYTES));
        assert_eq!(full_budget.len(), 240);
        assert_eq!(full_budget, format!("1-{}", "a".repeat(238)));
    }

    #[test]
    fn filename_sanitization_respects_exact_budgets() {
        for budget in 0..=8 {
            let sanitized = sanitized_filename("CON", budget);
            assert!(sanitized.len() <= budget);
            assert!(!is_windows_reserved_name(&sanitized));
        }
        assert_eq!(sanitized_filename("CON", 8), "file-CON");

        let full = "a".repeat(8);
        assert_eq!(sanitized_filename(&format!("{full} x"), 8), full);
    }

    #[test]
    fn plans_reject_unsafe_duplicate_ids_and_invalid_sizes() {
        for id in ["", ".", ".."] {
            assert!(matches!(
                plan_downloads(&[attachment(id, "a", 0)]),
                Err(AttachmentError::InvalidId)
            ));
        }
        let maximum_id = "a".repeat(MAX_ATTACHMENT_ID_BYTES);
        assert_eq!(
            plan_downloads(&[attachment(&maximum_id, "a", 0)])
                .unwrap()
                .len(),
            1
        );
        let oversized_id = "a".repeat(MAX_ATTACHMENT_ID_BYTES + 1);
        assert!(matches!(
            plan_downloads(&[attachment(&oversized_id, "a", 0)]),
            Err(AttachmentError::InvalidId)
        ));
        assert!(matches!(
            plan_downloads(&[attachment("same", "a", 0), attachment("same", "b", 0)]),
            Err(AttachmentError::DuplicateId)
        ));
        assert!(matches!(
            plan_downloads(&[attachment("id", "a", -1)]),
            Err(AttachmentError::InvalidSize)
        ));
    }

    #[test]
    fn plans_enforce_count_file_and_aggregate_limits() {
        let maximum_count = (0..MAX_ATTACHMENT_COUNT)
            .map(|index| attachment(&index.to_string(), "a", 0))
            .collect::<Vec<_>>();
        assert_eq!(plan_downloads(&maximum_count).unwrap().len(), 1_000);

        let too_many = (0..=MAX_ATTACHMENT_COUNT)
            .map(|index| attachment(&index.to_string(), "a", 0))
            .collect::<Vec<_>>();
        assert!(matches!(
            plan_downloads(&too_many),
            Err(AttachmentError::AttachmentCountLimit)
        ));

        let maximum_file =
            plan_downloads(&[attachment("1", "a", MAX_ATTACHMENT_SIZE_BYTES as i64)]).unwrap();
        assert_eq!(maximum_file[0].size, 21_474_836_480);
        assert!(matches!(
            plan_downloads(&[attachment("1", "a", (MAX_ATTACHMENT_SIZE_BYTES + 1) as i64)]),
            Err(AttachmentError::AttachmentSizeLimit)
        ));

        let maximum_aggregate = (0..5)
            .map(|index| attachment(&index.to_string(), "a", MAX_ATTACHMENT_SIZE_BYTES as i64))
            .collect::<Vec<_>>();
        assert_eq!(plan_downloads(&maximum_aggregate).unwrap().len(), 5);

        let aggregate = (0..6)
            .map(|index| attachment(&index.to_string(), "a", MAX_ATTACHMENT_SIZE_BYTES as i64))
            .collect::<Vec<_>>();
        assert!(matches!(
            plan_downloads(&aggregate),
            Err(AttachmentError::AggregateSizeLimit)
        ));
    }

    #[test]
    fn collision_detection_is_case_insensitive() {
        let plans = vec![
            PlannedAttachment {
                id: "one".to_string(),
                size: 0,
                name: "Same.PDF".to_string(),
            },
            PlannedAttachment {
                id: "two".to_string(),
                size: 0,
                name: "same.pdf".to_string(),
            },
        ];

        assert!(matches!(
            detect_name_collisions(&plans),
            Err(AttachmentError::NameCollision)
        ));
    }

    #[test]
    fn empty_metadata_does_not_create_the_output_directory() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("ABC-123");
        let source = FakeSource::bytes(&[]);

        let manifest = download_all(&source, &[], &output).unwrap();

        assert_eq!(
            manifest.stdout(&"ABC-123".parse().unwrap()),
            "# ABC-123 attachments\n\nNo attachments.\n"
        );
        assert!(!output.exists());
        assert_eq!(source.calls.get(), 0);
    }

    struct FinalPathCheckingReader {
        content: Cursor<Vec<u8>>,
        final_path: std::path::PathBuf,
        expected_old_content: Vec<u8>,
    }

    impl Read for FinalPathCheckingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert_eq!(
                fs::read(&self.final_path).unwrap(),
                self.expected_old_content
            );
            self.content.read(buffer)
        }
    }

    fn assert_no_temporary_files(directory: &Path) {
        assert!(!fs::read_dir(directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".jcli-attachment-")
        }));
    }

    #[test]
    fn existing_file_is_atomically_replaced_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        fs::create_dir(&output).unwrap();
        let item = attachment("id", "report.pdf", 8);
        let name = generated_name(&item.id, &item.filename);
        let final_path = output.join(&name);
        fs::write(&final_path, b"old content").unwrap();
        let source = FakeSource::new(vec![Ok(Box::new(FinalPathCheckingReader {
            content: Cursor::new(b"new data".to_vec()),
            final_path: final_path.clone(),
            expected_old_content: b"old content".to_vec(),
        }))]);

        let manifest = download_all(&source, &[item], &output).unwrap();

        assert_eq!(manifest.names, [name]);
        assert_eq!(source.calls.get(), 1);
        assert_eq!(fs::read(final_path).unwrap(), b"new data");
        assert_no_temporary_files(&output);
    }

    #[test]
    fn failed_replacement_keeps_the_old_file_and_removes_its_temp() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        fs::create_dir(&output).unwrap();
        let item = attachment("id", "report.pdf", 7);
        let name = generated_name(&item.id, &item.filename);
        let final_path = output.join(name);
        fs::write(&final_path, b"old content").unwrap();
        let source = FakeSource::bytes(&[b"short"]);

        let error = download_all(&source, &[item], &output).unwrap_err();

        assert!(matches!(error, AttachmentError::SizeMismatch));
        assert_eq!(fs::read(final_path).unwrap(), b"old content");
        assert_no_temporary_files(&output);
    }

    struct BlockingAtEofSource {
        content: Vec<u8>,
        ready: mpsc::Sender<()>,
        release: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl ContentSource for BlockingAtEofSource {
        fn content(&self, _id: &str) -> Result<Box<dyn Read>, JiraClientError> {
            Ok(Box::new(BlockingAtEofReader {
                content: Cursor::new(self.content.clone()),
                ready: self.ready.clone(),
                release: self.release.lock().unwrap().take().unwrap(),
                waited: false,
            }))
        }
    }

    struct BlockingAtEofReader {
        content: Cursor<Vec<u8>>,
        ready: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        waited: bool,
    }

    impl Read for BlockingAtEofReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.waited && self.content.position() == self.content.get_ref().len() as u64 {
                self.ready.send(()).unwrap();
                self.release
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                self.waited = true;
            }
            self.content.read(buffer)
        }
    }

    #[test]
    fn concurrent_successful_runs_are_last_writer_wins() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let item = attachment("id", "report.pdf", 5);
        let first_item = item.clone();
        let first_output = output.clone();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let first_run = thread::spawn(move || {
            let source = BlockingAtEofSource {
                content: b"first".to_vec(),
                ready: ready_sender,
                release: Mutex::new(Some(release_receiver)),
            };
            download_all(&source, &[first_item], &first_output)
        });

        ready_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let second_result = download_all(&FakeSource::bytes(&[b"later"]), &[item], &output);
        release_sender.send(()).unwrap();
        let first_result = first_run.join().unwrap();

        second_result.unwrap();
        first_result.unwrap();
        assert_eq!(
            fs::read(output.join(generated_name("id", "report.pdf"))).unwrap(),
            b"first"
        );
        assert_no_temporary_files(&output);
    }

    #[test]
    fn earlier_replacement_remains_when_a_later_download_fails() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        fs::create_dir(&output).unwrap();
        let metadata = [
            attachment("one", "first.txt", 5),
            attachment("two", "second.txt", 6),
        ];
        let first_path = output.join(generated_name("one", "first.txt"));
        let second_path = output.join(generated_name("two", "second.txt"));
        fs::write(&first_path, b"old first").unwrap();
        fs::write(&second_path, b"old second").unwrap();
        let source = FakeSource::bytes(&[b"first", b"short"]);

        let error = download_all(&source, &metadata, &output).unwrap_err();

        assert!(matches!(error, AttachmentError::SizeMismatch));
        assert_eq!(source.calls.get(), 2);
        assert_eq!(fs::read(first_path).unwrap(), b"first");
        assert_eq!(fs::read(second_path).unwrap(), b"old second");
        assert_no_temporary_files(&output);
    }

    #[test]
    fn streams_and_publishes_all_files_in_response_order() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let metadata = [
            attachment("one", "first.txt", 5),
            attachment("two", "second.bin", 6),
        ];
        let source = FakeSource::bytes(&[b"first", b"second"]);

        let manifest = download_all(&source, &metadata, &output).unwrap();
        let expected_names = metadata
            .iter()
            .map(|item| generated_name(&item.id, &item.filename))
            .collect::<Vec<_>>();

        assert_eq!(
            manifest.stdout(&"ABC-123".parse().unwrap()),
            format!(
                "# ABC-123 attachments\n\n- {}\n- {}\n",
                expected_names[0], expected_names[1]
            )
        );
        assert_eq!(fs::read(output.join(&expected_names[0])).unwrap(), b"first");
        assert_eq!(
            fs::read(output.join(&expected_names[1])).unwrap(),
            b"second"
        );
        assert_no_temporary_files(&output);
    }

    #[test]
    fn streams_an_http_response_without_using_content_length() {
        let server = MockServer::start();
        let content = vec![9_u8; COPY_BUFFER_SIZE + 17];
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/attachment/content/opaque-id")
                .query_param("redirect", "false")
                .header(
                    "Authorization",
                    "Basic amFuZUBleGFtcGxlLmNvbTpzZWNyZXQtdG9rZW4=",
                )
                .header_missing("Range");
            then.status(200).body(content.clone());
        });
        let client = JiraClient::new(&Config {
            base_url: Url::parse(&server.base_url()).unwrap(),
            email: "jane@example.com".to_string(),
            api_token: "secret-token".to_string(),
        })
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let metadata = [attachment(
            "opaque-id",
            "payload.bin",
            (COPY_BUFFER_SIZE + 17) as i64,
        )];

        let manifest = download_all(&client, &metadata, &output).unwrap();
        let name = generated_name("opaque-id", "payload.bin");

        mock.assert_calls(1);
        assert_eq!(
            manifest.stdout(&"ABC-123".parse().unwrap()),
            format!("# ABC-123 attachments\n\n- {name}\n")
        );
        assert_eq!(fs::read(output.join(name)).unwrap(), content);
    }

    #[test]
    fn size_mismatch_removes_the_temporary_file() {
        for bytes in [&b"short"[..], &b"too long"[..]] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("out");
            let source = FakeSource::bytes(&[bytes]);

            let error =
                download_all(&source, &[attachment("id", "file.bin", 7)], &output).unwrap_err();

            assert!(matches!(error, AttachmentError::SizeMismatch));
            assert!(output.is_dir());
            assert_no_temporary_files(&output);
            assert!(fs::read_dir(&output).unwrap().next().is_none());
        }
    }

    struct FailingReader {
        yielded: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.yielded {
                return Err(io::Error::other("source failed"));
            }
            self.yielded = true;
            buffer[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }

    #[test]
    fn source_read_failures_remove_the_partial_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let source = FakeSource::new(vec![Ok(Box::new(FailingReader { yielded: false }))]);

        let error = download_all(&source, &[attachment("id", "file.bin", 6)], &output).unwrap_err();

        assert!(matches!(error, AttachmentError::SourceRead));
        assert_no_temporary_files(&output);
        assert!(fs::read_dir(&output).unwrap().next().is_none());
    }

    #[test]
    fn unreplaceable_final_directory_is_a_filesystem_error() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        fs::create_dir(&output).unwrap();
        let item = attachment("1", "file.bin", 4);
        let final_path = output.join(generated_name(&item.id, &item.filename));
        fs::create_dir(&final_path).unwrap();
        let source = FakeSource::bytes(&[b"data"]);

        let error = download_all(&source, &[item], &output).unwrap_err();

        assert!(error.is_filesystem());
        assert!(final_path.is_dir());
        assert_no_temporary_files(&output);
    }

    #[test]
    fn zero_byte_download_publishes_an_empty_file() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let source = FakeSource::bytes(&[b""]);

        let manifest =
            download_all(&source, &[attachment("10001", "empty.bin", 0)], &output).unwrap();

        assert_eq!(
            manifest.stdout(&"ABC-123".parse().unwrap()),
            "# ABC-123 attachments\n\n- 10001-empty.bin\n"
        );
        assert_eq!(
            fs::metadata(output.join("10001-empty.bin")).unwrap().len(),
            0
        );
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("destination failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn destination_errors_are_classified_as_filesystem_failures() {
        let mut source = Cursor::new(b"data");
        let error = copy_exact_size(&mut source, &mut FailingWriter, 4).unwrap_err();

        assert!(error.is_filesystem());
    }

    #[test]
    fn error_messages_do_not_echo_metadata_or_output_paths() {
        let secret_id = "secret/id";
        let secret_filename = "secret-file.txt";
        let secret_path = "/secret/output/path";
        let errors = [
            AttachmentError::InvalidId,
            AttachmentError::DuplicateId,
            AttachmentError::InvalidSize,
            AttachmentError::AttachmentCountLimit,
            AttachmentError::AttachmentSizeLimit,
            AttachmentError::AggregateSizeLimit,
            AttachmentError::NameCollision,
            AttachmentError::SourceRead,
            AttachmentError::SizeMismatch,
            AttachmentError::filesystem(io::Error::other(secret_path.to_string())),
        ];

        for error in errors {
            let message = error.to_string();
            assert!(!message.contains(secret_id));
            assert!(!message.contains(secret_filename));
            assert!(!message.contains(secret_path));
            assert!(!message.chars().any(char::is_control));
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_output_directories_and_temporary_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("nested/out");
        prepare_output_directory(&output).unwrap();
        let temporary = create_temporary_file(&output).unwrap();
        let temporary_path = temporary.path().to_owned();

        for path in [root.path().join("nested"), output.clone(), temporary_path] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        }

        let final_path = output.join("final");
        temporary.persist(&final_path).unwrap();
        assert_eq!(
            fs::metadata(final_path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn shared_reader_state_can_model_bounded_streaming() {
        struct CountingReader {
            data: Cursor<Vec<u8>>,
            largest_request: Rc<Cell<usize>>,
        }
        impl Read for CountingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.largest_request
                    .set(self.largest_request.get().max(buffer.len()));
                self.data.read(buffer)
            }
        }

        let largest = Rc::new(Cell::new(0));
        let mut reader = CountingReader {
            data: Cursor::new(vec![7; COPY_BUFFER_SIZE * 2 + 1]),
            largest_request: Rc::clone(&largest),
        };
        let mut destination = Vec::new();
        copy_exact_size(
            &mut reader,
            &mut destination,
            (COPY_BUFFER_SIZE * 2 + 1) as u64,
        )
        .unwrap();

        assert_eq!(destination.len(), COPY_BUFFER_SIZE * 2 + 1);
        assert!(largest.get() <= COPY_BUFFER_SIZE);
    }
}
