use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirBuilder, File, OpenOptions};
use same_file::Handle;
use sha2::{Digest, Sha256};
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

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    #[error("an attachment already exists in the output directory")]
    ExistingFile,
    #[error("attachment content could not be read")]
    SourceRead,
    #[error("downloaded attachment size does not match Jira metadata")]
    SizeMismatch,
    #[error("attachment downloads are not supported on this platform")]
    UnsupportedPlatform,
    #[error("the attachment output path is unsafe")]
    UnsafeOutputPath,
    #[error("failed to access the attachment output directory")]
    Filesystem {
        #[source]
        source: io::Error,
    },
    #[error("failed to publish attachments without overwriting files")]
    Publish {
        #[source]
        source: io::Error,
    },
    #[error("failed to clean up attachment output")]
    Cleanup {
        #[source]
        source: io::Error,
    },
}

impl AttachmentError {
    pub fn is_filesystem(&self) -> bool {
        matches!(
            self,
            Self::ExistingFile
                | Self::UnsupportedPlatform
                | Self::UnsafeOutputPath
                | Self::Filesystem { .. }
                | Self::Publish { .. }
                | Self::Cleanup { .. }
        )
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

struct StagedAttachment {
    plan: PlannedAttachment,
    temporary_name: String,
    file: File,
    published: bool,
}

/// Downloads every attachment, verifies every staged file, and only then
/// publishes final names. The returned manifest remains in Jira response order.
pub fn download_all(
    source: &impl ContentSource,
    metadata: &[Attachment],
    output_directory: &Path,
) -> Result<DownloadManifest, AttachmentError> {
    if !cfg!(unix) {
        return Err(AttachmentError::UnsupportedPlatform);
    }

    let plans = plan_downloads(metadata)?;
    if plans.is_empty() {
        return Ok(DownloadManifest { names: Vec::new() });
    }

    let mut directory = OutputDirectory::prepare(output_directory)?;
    let result = stage_and_publish(source, plans, directory.dir());

    match result {
        Ok(names) => {
            directory.keep();
            Ok(DownloadManifest { names })
        }
        Err(error) => match directory.remove_created() {
            Ok(()) => Err(error),
            Err(source) => Err(AttachmentError::Cleanup { source }),
        },
    }
}

fn stage_and_publish(
    source: &impl ContentSource,
    plans: Vec<PlannedAttachment>,
    directory: &Dir,
) -> Result<Vec<String>, AttachmentError> {
    preflight_output(directory, &plans)?;
    let mut staging_directory = StagingDirectory::create(directory)?;
    let staging = staging_directory
        .dir
        .as_ref()
        .expect("staging directory is available");
    let mut staged = Vec::with_capacity(plans.len());

    let result = (|| {
        for plan in plans {
            match stage_one(source, plan, staging) {
                Ok(attachment) => staged.push(attachment),
                Err(error) => {
                    cleanup_staged(staging, &staged)
                        .map_err(|source| AttachmentError::Cleanup { source })?;
                    return Err(error);
                }
            }
        }

        publish_all(directory, staging, &mut staged)?;
        Ok(staged
            .iter()
            .map(|attachment| attachment.plan.name.clone())
            .collect())
    })();

    match result {
        Ok(names) => {
            // Publication is the commit point. Cleanup failures after every
            // final hard link exists must not turn a successful download into
            // a reported failure with files already visible.
            let _ = cleanup_staged(staging, &staged);
            let _ = staging_directory.remove();
            Ok(names)
        }
        Err(error) => {
            let staged_cleanup = cleanup_staged(staging, &staged);
            let directory_cleanup = staging_directory.remove();
            if let Some(source) = staged_cleanup.err().or_else(|| directory_cleanup.err()) {
                Err(AttachmentError::Cleanup { source })
            } else {
                Err(error)
            }
        }
    }
}

fn plan_downloads(metadata: &[Attachment]) -> Result<Vec<PlannedAttachment>, AttachmentError> {
    if metadata.len() > MAX_ATTACHMENT_COUNT {
        return Err(AttachmentError::AttachmentCountLimit);
    }

    let mut ids = HashSet::with_capacity(metadata.len());
    let mut plans = Vec::with_capacity(metadata.len());
    let mut total_size = 0_u64;

    for attachment in metadata {
        if attachment.id.is_empty()
            || attachment.id.len() > MAX_ATTACHMENT_ID_BYTES
            || matches!(attachment.id.as_str(), "." | "..")
        {
            return Err(AttachmentError::InvalidId);
        }
        if !ids.insert(attachment.id.as_str()) {
            return Err(AttachmentError::DuplicateId);
        }
        let size = u64::try_from(attachment.size).map_err(|_| AttachmentError::InvalidSize)?;
        if size > MAX_ATTACHMENT_SIZE_BYTES {
            return Err(AttachmentError::AttachmentSizeLimit);
        }
        total_size = total_size
            .checked_add(size)
            .ok_or(AttachmentError::AggregateSizeLimit)?;
        if total_size > MAX_TOTAL_ATTACHMENT_SIZE_BYTES {
            return Err(AttachmentError::AggregateSizeLimit);
        }
        plans.push(PlannedAttachment {
            id: attachment.id.clone(),
            size,
            name: generated_name(&attachment.id, &attachment.filename),
        });
    }

    detect_name_collisions(&plans)?;
    Ok(plans)
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

fn preflight_output(directory: &Dir, plans: &[PlannedAttachment]) -> Result<(), AttachmentError> {
    for plan in plans {
        match directory.symlink_metadata(&plan.name) {
            Ok(_) => return Err(AttachmentError::ExistingFile),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AttachmentError::filesystem(error)),
        }
    }
    Ok(())
}

fn stage_one(
    source: &impl ContentSource,
    plan: PlannedAttachment,
    directory: &Dir,
) -> Result<StagedAttachment, AttachmentError> {
    let (temporary_name, mut file) = create_temporary_file(directory)?;
    let stage_result = (|| {
        let mut content = source.content(&plan.id)?;
        copy_exact_size(&mut content, &mut file, plan.size)?;
        file.flush().map_err(AttachmentError::filesystem)
    })();

    match stage_result {
        Ok(()) => Ok(StagedAttachment {
            plan,
            temporary_name,
            file,
            published: false,
        }),
        Err(error) => match remove_if_present(directory, &temporary_name) {
            Ok(()) => Err(error),
            Err(source) => Err(AttachmentError::Cleanup { source }),
        },
    }
}

fn create_temporary_file(directory: &Dir) -> Result<(String, File), AttachmentError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    for _ in 0..128 {
        let name = unique_internal_name("attachment");
        match directory.open_with(&name, &options) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(AttachmentError::filesystem(error)),
        }
    }

    Err(AttachmentError::filesystem(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a staging file",
    )))
}

fn unique_internal_name(kind: &str) -> String {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(".jcli-{kind}-{}-{sequence:016x}", std::process::id())
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
        if count == 0 {
            break;
        }

        copied = copied
            .checked_add(count as u64)
            .ok_or(AttachmentError::SizeMismatch)?;
        if copied > expected {
            return Err(AttachmentError::SizeMismatch);
        }
        destination
            .write_all(&buffer[..count])
            .map_err(AttachmentError::filesystem)?;
    }

    if copied != expected {
        return Err(AttachmentError::SizeMismatch);
    }
    Ok(())
}

fn publish_all(
    directory: &Dir,
    staging: &Dir,
    staged: &mut [StagedAttachment],
) -> Result<(), AttachmentError> {
    for index in 0..staged.len() {
        let attachment = &staged[index];
        if let Err(publish_error) =
            staging.hard_link(&attachment.temporary_name, directory, &attachment.plan.name)
        {
            let cleanup = rollback_and_cleanup(directory, staging, staged);
            if let Err(source) = cleanup {
                return Err(AttachmentError::Cleanup { source });
            }
            return Err(AttachmentError::Publish {
                source: publish_error,
            });
        }
        staged[index].published = true;
    }

    Ok(())
}

fn rollback_and_cleanup(
    directory: &Dir,
    staging: &Dir,
    staged: &mut [StagedAttachment],
) -> io::Result<()> {
    let rollback = rollback_published(directory, staging, staged);
    let cleanup = cleanup_staged(staging, staged);
    rollback.and(cleanup)
}

fn cleanup_staged(directory: &Dir, staged: &[StagedAttachment]) -> io::Result<()> {
    let mut first_error = None;
    for attachment in staged {
        if let Err(error) = remove_if_present(directory, &attachment.temporary_name)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn rollback_published(
    directory: &Dir,
    staging: &Dir,
    staged: &mut [StagedAttachment],
) -> io::Result<()> {
    let mut first_error = None;
    for attachment in staged.iter_mut().rev().filter(|item| item.published) {
        let quarantine_name = unique_internal_name("rollback");
        match directory.rename(&attachment.plan.name, staging, &quarantine_name) {
            Ok(()) => match quarantined_entry_is_same(staging, &quarantine_name, attachment) {
                Ok(true) => {
                    if let Err(error) = remove_if_present(staging, &quarantine_name) {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    } else {
                        attachment.published = false;
                    }
                }
                Ok(false) => {
                    let restore =
                        staging.hard_link(&quarantine_name, directory, &attachment.plan.name);
                    if restore.is_ok() {
                        let _ = remove_if_present(staging, &quarantine_name);
                    }
                    attachment.published = false;
                    if first_error.is_none() {
                        first_error = Some(restore.err().unwrap_or_else(|| {
                            io::Error::other(
                                "published attachment identity changed during rollback",
                            )
                        }));
                    }
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                attachment.published = false;
            }
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn quarantined_entry_is_same(
    staging: &Dir,
    quarantine_name: &str,
    attachment: &StagedAttachment,
) -> io::Result<bool> {
    let metadata = staging.symlink_metadata(quarantine_name)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let quarantined = staging.open(quarantine_name)?;
    same_cap_file(&quarantined, &attachment.file)
}

fn same_cap_file(first: &File, second: &File) -> io::Result<bool> {
    let first = Handle::from_file(first.try_clone()?.into_std())?;
    let second = Handle::from_file(second.try_clone()?.into_std())?;
    Ok(first == second)
}

fn remove_if_present(directory: &Dir, name: &str) -> io::Result<()> {
    match directory.remove_file(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct StagingDirectory {
    dir: Option<Dir>,
}

impl StagingDirectory {
    fn create(parent: &Dir) -> Result<Self, AttachmentError> {
        for _ in 0..128 {
            let name = OsString::from(unique_internal_name("stage"));
            match create_private_directory(parent, &name)? {
                true => {
                    let dir = match open_verified_directory(parent, &name) {
                        Ok(dir) => dir,
                        Err(error) => {
                            let _ = parent.remove_dir(&name);
                            return Err(error);
                        }
                    };
                    return Ok(Self { dir: Some(dir) });
                }
                false => continue,
            }
        }
        Err(AttachmentError::filesystem(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a staging directory",
        )))
    }

    fn remove(&mut self) -> io::Result<()> {
        match self.dir.take() {
            Some(directory) => directory.remove_open_dir(),
            None => Ok(()),
        }
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[derive(Default)]
struct CreatedDirectories {
    directories: Vec<Dir>,
}

impl CreatedDirectories {
    fn take(&mut self) -> Vec<Dir> {
        std::mem::take(&mut self.directories)
    }
}

impl Drop for CreatedDirectories {
    fn drop(&mut self) {
        for directory in self.directories.drain(..).rev() {
            let _ = directory.remove_open_dir();
        }
    }
}

struct OutputDirectory {
    dir: Option<Dir>,
    created: Vec<Dir>,
}

impl OutputDirectory {
    fn prepare(path: &Path) -> Result<Self, AttachmentError> {
        let (anchor, components) = split_output_path(path)?;
        let mut created = CreatedDirectories::default();
        let mut current = Dir::open_ambient_dir(anchor, ambient_authority())
            .map_err(AttachmentError::filesystem)?;
        let mut parent_trust = verify_output_directory_trust(&current)?;
        if !directory_has_trusted_owner(&current)? {
            return Err(AttachmentError::UnsafeOutputPath);
        }

        for component in components {
            // Every parent stays open and is checked before resolving the next
            // name. On Unix, that means no other local account can substitute
            // a child between creation and the no-follow open below.
            let was_created = create_private_directory(&current, &component)?;
            let child = open_verified_directory(&current, &component)?;
            if was_created {
                created
                    .directories
                    .push(child.try_clone().map_err(AttachmentError::filesystem)?);
            }
            let child_trust = verify_output_directory_trust(&child)?;
            if !directory_has_trusted_owner(&child)?
                || (parent_trust == DirectoryTrust::StickyShared
                    && !directory_is_owned_by_current_user(&child)?)
            {
                return Err(AttachmentError::UnsafeOutputPath);
            }
            current = child;
            parent_trust = child_trust;
        }

        // A sticky shared directory can safely contain a private child, but
        // cannot be the output directory itself because other users may add
        // colliding names while files are being published.
        if parent_trust == DirectoryTrust::StickyShared
            || !directory_is_owned_by_current_user(&current)?
        {
            return Err(AttachmentError::UnsafeOutputPath);
        }

        Ok(Self {
            dir: Some(current),
            created: created.take(),
        })
    }

    fn dir(&self) -> &Dir {
        self.dir.as_ref().expect("output directory is available")
    }

    fn keep(&mut self) {
        self.created.clear();
    }

    fn remove_created(&mut self) -> io::Result<()> {
        self.dir.take();
        let mut first_error = None;
        for directory in self.created.drain(..).rev() {
            if let Err(error) = directory.remove_open_dir()
                && error.kind() != io::ErrorKind::NotFound
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for OutputDirectory {
    fn drop(&mut self) {
        let _ = self.remove_created();
    }
}

fn split_output_path(path: &Path) -> Result<(PathBuf, Vec<OsString>), AttachmentError> {
    let mut anchor = PathBuf::new();
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(AttachmentError::UnsafeOutputPath),
            Component::Normal(value) => components.push(value.to_os_string()),
        }
    }

    if anchor.as_os_str().is_empty() {
        anchor.push(".");
    }
    Ok((anchor, components))
}

fn create_private_directory(parent: &Dir, name: &OsString) -> Result<bool, AttachmentError> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }

    match parent.create_dir_with(name, &builder) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(AttachmentError::filesystem(error)),
    }
}

fn open_verified_directory(parent: &Dir, name: &OsString) -> Result<Dir, AttachmentError> {
    let metadata = parent
        .symlink_metadata(name)
        .map_err(AttachmentError::filesystem)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AttachmentError::UnsafeOutputPath);
    }

    // `Dir::open_dir` follows a final symlink. Opening with
    // `FollowSymlinks::No` makes the handle itself, rather than a preceding
    // path check, the proof that this component is a directory and not a
    // symlink or Windows reparse point.
    let mut options = OpenOptions::new();
    options
        .read(true)
        .maybe_dir(true)
        .follow(FollowSymlinks::No);
    let opened = Dir::from_std_file(
        parent
            .open_with(name, &options)
            .map_err(AttachmentError::filesystem)?
            .into_std(),
    );
    let metadata = opened.dir_metadata().map_err(AttachmentError::filesystem)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AttachmentError::UnsafeOutputPath);
    }
    Ok(opened)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectoryTrust {
    Private,
    StickyShared,
}

#[cfg(unix)]
fn verify_output_directory_trust(directory: &Dir) -> Result<DirectoryTrust, AttachmentError> {
    use cap_std::fs::MetadataExt;

    let mode = directory
        .dir_metadata()
        .map_err(AttachmentError::filesystem)?
        .mode();
    if mode & 0o022 == 0 {
        Ok(DirectoryTrust::Private)
    } else if mode & 0o1000 != 0 {
        // Sticky directories such as /tmp may be traversed only to reach an
        // owner-controlled child. The caller never publishes directly into
        // one because it is shared with other local accounts.
        Ok(DirectoryTrust::StickyShared)
    } else {
        Err(AttachmentError::UnsafeOutputPath)
    }
}

#[cfg(unix)]
fn directory_is_owned_by_current_user(directory: &Dir) -> Result<bool, AttachmentError> {
    Ok(directory_owner(directory)? == effective_user_id())
}

#[cfg(unix)]
fn directory_has_trusted_owner(directory: &Dir) -> Result<bool, AttachmentError> {
    Ok(owner_is_trusted(
        directory_owner(directory)?,
        effective_user_id(),
    ))
}

#[cfg(unix)]
fn owner_is_trusted(owner: u32, effective_user: u32) -> bool {
    owner == 0 || owner == effective_user
}

#[cfg(unix)]
fn directory_owner(directory: &Dir) -> Result<u32, AttachmentError> {
    use cap_std::fs::MetadataExt;

    Ok(directory
        .dir_metadata()
        .map_err(AttachmentError::filesystem)?
        .uid())
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    // SAFETY: geteuid reads the effective user ID and has no memory or
    // process-state preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn verify_output_directory_trust(_directory: &Dir) -> Result<DirectoryTrust, AttachmentError> {
    Err(AttachmentError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn directory_is_owned_by_current_user(_directory: &Dir) -> Result<bool, AttachmentError> {
    Ok(false)
}

#[cfg(not(unix))]
fn directory_has_trusted_owner(_directory: &Dir) -> Result<bool, AttachmentError> {
    Ok(false)
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
    let mut sanitized = String::with_capacity(filename.len().min(max_bytes));
    let mut pending_separator = false;

    for character in filename.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-') {
            if pending_separator && !sanitized.is_empty() && sanitized.len() < max_bytes {
                sanitized.push('-');
            }
            pending_separator = false;
            if sanitized.len() == max_bytes {
                break;
            }
            sanitized.push(character);
        } else {
            pending_separator = true;
        }
    }

    while sanitized.ends_with(['.', '-']) {
        sanitized.pop();
    }
    while sanitized.starts_with(['.', '-']) {
        sanitized.remove(0);
    }

    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        return FALLBACK_FILENAME[..FALLBACK_FILENAME.len().min(max_bytes)].to_string();
    }

    if is_windows_reserved_name(&sanitized) {
        let prefix = "file-";
        if sanitized.len() + prefix.len() <= max_bytes {
            sanitized.insert_str(0, prefix);
        } else {
            sanitized.replace_range(..prefix.len().min(sanitized.len()), prefix);
        }
    }
    sanitized
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
        }
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
    }

    #[test]
    fn plans_reject_unsafe_duplicate_ids_and_invalid_sizes() {
        for id in ["", ".", ".."] {
            assert!(matches!(
                plan_downloads(&[attachment(id, "a", 0)]),
                Err(AttachmentError::InvalidId)
            ));
        }
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
        let too_many = (0..=MAX_ATTACHMENT_COUNT)
            .map(|index| attachment(&index.to_string(), "a", 0))
            .collect::<Vec<_>>();
        assert!(matches!(
            plan_downloads(&too_many),
            Err(AttachmentError::AttachmentCountLimit)
        ));
        assert!(matches!(
            plan_downloads(&[attachment("1", "a", (MAX_ATTACHMENT_SIZE_BYTES + 1) as i64)]),
            Err(AttachmentError::AttachmentSizeLimit)
        ));

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

    #[test]
    fn existing_final_files_fail_before_content_is_requested() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        fs::create_dir(&output).unwrap();
        let item = attachment("id", "report.pdf", 4);
        let name = generated_name(&item.id, &item.filename);
        fs::write(output.join(name), b"old").unwrap();
        let source = FakeSource::bytes(&[b"data"]);

        let error = download_all(&source, &[item], &output).unwrap_err();

        assert!(matches!(error, AttachmentError::ExistingFile));
        assert_eq!(source.calls.get(), 0);
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
        assert!(!fs::read_dir(&output).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".jcli-attachment-")
        }));
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
    fn size_mismatch_removes_temps_and_a_created_empty_directory() {
        for bytes in [&b"short"[..], &b"too long"[..]] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("out");
            let source = FakeSource::bytes(&[bytes]);

            let error =
                download_all(&source, &[attachment("id", "file.bin", 7)], &output).unwrap_err();

            assert!(matches!(error, AttachmentError::SizeMismatch));
            assert!(!output.exists());
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
    fn source_read_failures_clean_partial_staging() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        let source = FakeSource::new(vec![Ok(Box::new(FailingReader { yielded: false }))]);

        let error = download_all(&source, &[attachment("id", "file.bin", 6)], &output).unwrap_err();

        assert!(matches!(error, AttachmentError::SourceRead));
        assert!(!output.exists());
    }

    #[test]
    fn ordinary_failure_removes_every_nested_directory_it_created() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("one/two/three");
        let source = FakeSource::bytes(&[b"short"]);

        let error = download_all(&source, &[attachment("1", "file.bin", 7)], &output).unwrap_err();

        assert!(matches!(error, AttachmentError::SizeMismatch));
        assert!(!root.path().join("one").exists());
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

    fn staged_attachment(directory: &Dir, id: &str, name: &str, bytes: &[u8]) -> StagedAttachment {
        let (temporary_name, mut file) = create_temporary_file(directory).unwrap();
        file.write_all(bytes).unwrap();
        StagedAttachment {
            plan: PlannedAttachment {
                id: id.to_string(),
                size: bytes.len() as u64,
                name: name.to_string(),
            },
            temporary_name,
            file,
            published: false,
        }
    }

    #[test]
    fn publication_is_no_clobber_and_rolls_back_earlier_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut staged = vec![
            staged_attachment(&directory, "one", "first", b"first"),
            staged_attachment(&directory, "two", "second", b"second"),
        ];
        fs::write(root.path().join("second"), b"racer").unwrap();

        let error = publish_all(&directory, &directory, &mut staged).unwrap_err();

        assert!(matches!(error, AttachmentError::Publish { .. }));
        assert!(!root.path().join("first").exists());
        assert_eq!(fs::read(root.path().join("second")).unwrap(), b"racer");
    }

    #[test]
    fn rollback_does_not_delete_a_replaced_published_entry() {
        let root = tempfile::tempdir().unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut staged = vec![staged_attachment(&directory, "one", "first", b"first")];
        directory
            .hard_link(&staged[0].temporary_name, &directory, "first")
            .unwrap();
        staged[0].published = true;
        directory.remove_file("first").unwrap();
        fs::write(root.path().join("first"), b"replacement").unwrap();

        let error = rollback_published(&directory, &directory, &mut staged).unwrap_err();

        assert_eq!(fs::read(root.path().join("first")).unwrap(), b"replacement");
        assert_eq!(
            error.to_string(),
            "published attachment identity changed during rollback"
        );
        cleanup_staged(&directory, &staged).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rollback_does_not_follow_a_replaced_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"replacement").unwrap();
        let mut staged = vec![staged_attachment(&directory, "one", "first", b"first")];
        directory
            .hard_link(&staged[0].temporary_name, &directory, "first")
            .unwrap();
        staged[0].published = true;
        directory.remove_file("first").unwrap();
        symlink(&outside, root.path().join("first")).unwrap();

        assert!(rollback_published(&directory, &directory, &mut staged).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"replacement");
        assert!(
            fs::symlink_metadata(root.path().join("first"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        cleanup_staged(&directory, &staged).unwrap();
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
            AttachmentError::ExistingFile,
            AttachmentError::SourceRead,
            AttachmentError::SizeMismatch,
            AttachmentError::UnsafeOutputPath,
        ];

        for error in errors {
            let message = error.to_string();
            assert!(!message.contains(secret_id));
            assert!(!message.contains(secret_filename));
            assert!(!message.contains(secret_path));
            assert!(!message.chars().any(char::is_control));
        }
    }

    #[test]
    fn created_directory_guard_removes_only_directories_it_created() {
        let root = tempfile::tempdir().unwrap();
        let created = root.path().join("created");
        {
            let _guard = OutputDirectory::prepare(&created).unwrap();
            assert!(created.is_dir());
        }
        assert!(!created.exists());

        let existing = root.path().join("existing");
        fs::create_dir(&existing).unwrap();
        {
            let _guard = OutputDirectory::prepare(&existing).unwrap();
        }
        assert!(existing.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn new_output_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("nested/out");
        let mut guard = OutputDirectory::prepare(&output).unwrap();

        assert_eq!(
            fs::metadata(root.path().join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o700
        );
        guard.keep();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outputs_and_publicly_writable_existing_outputs_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            OutputDirectory::prepare(&link),
            Err(AttachmentError::UnsafeOutputPath)
        ));

        let nested_link = root.path().join("nested-link");
        symlink(&target, &nested_link).unwrap();
        assert!(matches!(
            OutputDirectory::prepare(&nested_link.join("output")),
            Err(AttachmentError::UnsafeOutputPath)
        ));

        let writable = root.path().join("writable");
        fs::create_dir(&writable).unwrap();
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            OutputDirectory::prepare(&writable),
            Err(AttachmentError::UnsafeOutputPath)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn only_root_or_the_effective_user_is_a_trusted_directory_owner() {
        assert!(owner_is_trusted(0, 1000));
        assert!(owner_is_trusted(1000, 1000));
        assert!(!owner_is_trusted(1001, 1000));
    }

    #[cfg(unix)]
    #[test]
    fn sticky_shared_parents_require_an_owner_controlled_output_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        let owned = shared.join("owned");
        fs::create_dir(&owned).unwrap();

        assert!(matches!(
            OutputDirectory::prepare(&shared),
            Err(AttachmentError::UnsafeOutputPath)
        ));
        let mut output = OutputDirectory::prepare(&owned).unwrap();
        output.keep();
    }

    #[test]
    fn root_and_dot_paths_have_no_untrusted_components() {
        let (root_anchor, root_components) = split_output_path(Path::new("/")).unwrap();
        assert_eq!(root_anchor, PathBuf::from("/"));
        assert!(root_components.is_empty());

        let (dot_anchor, dot_components) = split_output_path(Path::new(".")).unwrap();
        assert_eq!(dot_anchor, PathBuf::from("."));
        assert!(dot_components.is_empty());
    }

    #[test]
    fn parent_traversal_is_rejected() {
        assert!(matches!(
            OutputDirectory::prepare(Path::new("safe/../unsafe")),
            Err(AttachmentError::UnsafeOutputPath)
        ));
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
