use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const HARD_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_SOURCE_FILES: usize = 10_000;
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const HARD_MAX_SOURCE_FILES: usize = 50_000;
pub const HARD_MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const HARD_MAX_VISITED_ENTRIES: usize = 100_000;
pub const HARD_MAX_VISITED_DIRECTORIES: usize = 10_000;
pub const HARD_MAX_DIRECTORY_DEPTH: usize = 128;
pub const HARD_MAX_ENTRIES_PER_DIRECTORY: usize = 50_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryOptions {
    max_file_bytes: u64,
    max_source_files: usize,
    max_source_bytes: u64,
    includes: Vec<String>,
    excludes: Vec<String>,
    allow_sensitive: bool,
    max_visited_entries: usize,
    max_visited_directories: usize,
    max_directory_depth: usize,
    max_entries_per_directory: usize,
}

impl DiscoveryOptions {
    pub fn with_max_file_bytes(
        mut self,
        max_file_bytes: u64,
    ) -> Result<Self, DiscoveryConfigError> {
        if max_file_bytes == 0 || max_file_bytes > HARD_MAX_FILE_BYTES {
            return Err(DiscoveryConfigError::InvalidFileLimit {
                requested: max_file_bytes,
            });
        }
        self.max_file_bytes = max_file_bytes;
        Ok(self)
    }

    pub fn with_source_limits(
        mut self,
        max_source_files: usize,
        max_source_bytes: u64,
    ) -> Result<Self, DiscoveryConfigError> {
        if max_source_files == 0
            || max_source_files > HARD_MAX_SOURCE_FILES
            || max_source_bytes == 0
            || max_source_bytes > HARD_MAX_SOURCE_BYTES
        {
            return Err(DiscoveryConfigError::InvalidSourceLimit {
                max_files: max_source_files,
                max_bytes: max_source_bytes,
            });
        }
        self.max_source_files = max_source_files;
        self.max_source_bytes = max_source_bytes;
        Ok(self)
    }

    pub const fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    pub const fn max_source_files(&self) -> usize {
        self.max_source_files
    }

    pub const fn max_source_bytes(&self) -> u64 {
        self.max_source_bytes
    }

    pub fn includes(&self) -> &[String] {
        &self.includes
    }

    pub fn excludes(&self) -> &[String] {
        &self.excludes
    }

    pub const fn allows_sensitive(&self) -> bool {
        self.allow_sensitive
    }

    pub const fn max_visited_entries(&self) -> usize {
        self.max_visited_entries
    }

    pub const fn max_visited_directories(&self) -> usize {
        self.max_visited_directories
    }

    pub const fn max_directory_depth(&self) -> usize {
        self.max_directory_depth
    }

    pub const fn max_entries_per_directory(&self) -> usize {
        self.max_entries_per_directory
    }

    pub fn include(mut self, pattern: impl Into<String>) -> Self {
        self.includes.push(pattern.into());
        self
    }

    pub fn exclude(mut self, pattern: impl Into<String>) -> Self {
        self.excludes.push(pattern.into());
        self
    }

    pub fn allow_sensitive(mut self, allow: bool) -> Self {
        self.allow_sensitive = allow;
        self
    }

    pub fn with_traversal_limits(
        mut self,
        max_visited_entries: usize,
        max_visited_directories: usize,
        max_directory_depth: usize,
        max_entries_per_directory: usize,
    ) -> Result<Self, DiscoveryConfigError> {
        if max_visited_entries == 0
            || max_visited_entries > HARD_MAX_VISITED_ENTRIES
            || max_visited_directories == 0
            || max_visited_directories > HARD_MAX_VISITED_DIRECTORIES
            || max_directory_depth == 0
            || max_directory_depth > HARD_MAX_DIRECTORY_DEPTH
            || max_entries_per_directory == 0
            || max_entries_per_directory > HARD_MAX_ENTRIES_PER_DIRECTORY
        {
            return Err(DiscoveryConfigError::InvalidTraversalLimit);
        }
        self.max_visited_entries = max_visited_entries;
        self.max_visited_directories = max_visited_directories;
        self.max_directory_depth = max_directory_depth;
        self.max_entries_per_directory = max_entries_per_directory;
        Ok(self)
    }
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_source_files: DEFAULT_MAX_SOURCE_FILES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            includes: Vec::new(),
            excludes: Vec::new(),
            allow_sensitive: false,
            max_visited_entries: HARD_MAX_VISITED_ENTRIES,
            max_visited_directories: HARD_MAX_VISITED_DIRECTORIES,
            max_directory_depth: HARD_MAX_DIRECTORY_DEPTH,
            max_entries_per_directory: HARD_MAX_ENTRIES_PER_DIRECTORY,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemFile {
    connector_key: Vec<u8>,
    relative_path: PathBuf,
    original_bytes: Vec<u8>,
    source_timestamp: Option<SourceTimestamp>,
}

impl FilesystemFile {
    pub fn connector_key(&self) -> &[u8] {
        &self.connector_key
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn original_bytes(&self) -> &[u8] {
        &self.original_bytes
    }

    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.original_bytes)
            .expect("filesystem snapshots are validated as UTF-8 before construction")
    }

    pub const fn source_timestamp(&self) -> Option<SourceTimestamp> {
        self.source_timestamp
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceTimestamp {
    unix_seconds: i64,
    nanoseconds: u32,
}

impl SourceTimestamp {
    pub const fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }

    pub const fn nanoseconds(self) -> u32 {
        self.nanoseconds
    }
}

/// Builds the stable ASCII source URI for a raw filesystem connector key.
///
/// RFC 3986 unreserved bytes and path separators are preserved. Every other
/// byte is percent-encoded with uppercase hexadecimal digits.
pub fn repo_uri(connector_key: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut uri = String::with_capacity(7 + connector_key.len());
    uri.push_str("repo://");
    for &byte in connector_key {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    uri
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileIssueKind {
    InvalidUtf8,
    NulContent,
    FileTooLarge,
    PermissionDenied,
    ReadFailed,
    ChangedDuringRead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIssue {
    connector_key: Vec<u8>,
    relative_path: PathBuf,
    kind: FileIssueKind,
}

impl FileIssue {
    pub fn connector_key(&self) -> &[u8] {
        &self.connector_key
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub const fn kind(&self) -> FileIssueKind {
        self.kind
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemSnapshot {
    files: Vec<FilesystemFile>,
    issues: Vec<FileIssue>,
}

impl FilesystemSnapshot {
    pub fn files(&self) -> &[FilesystemFile] {
        &self.files
    }

    pub fn issues(&self) -> &[FileIssue] {
        &self.issues
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FilesystemDiscoveryEstimate {
    pub(crate) eligible_files: usize,
    pub(crate) eligible_bytes: u64,
    pub(crate) skipped_files: usize,
}

#[derive(Debug)]
pub struct FilesystemSpool {
    file: File,
    entries: Vec<SpooledFilesystemFile>,
    issues: Vec<FileIssue>,
}

impl FilesystemSpool {
    pub fn entries(&self) -> &[SpooledFilesystemFile] {
        &self.entries
    }

    pub fn issues(&self) -> &[FileIssue] {
        &self.issues
    }

    pub fn read_body(&mut self, index: usize) -> std::io::Result<Vec<u8>> {
        let entry = self.entries.get(index).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "spool entry is absent")
        })?;
        let length = usize::try_from(entry.body_len).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "spooled body length is not representable",
            )
        })?;
        self.file.seek(SeekFrom::Start(entry.body_offset))?;
        let mut bytes = vec![0_u8; length];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpooledFilesystemFile {
    connector_key: Vec<u8>,
    relative_path: PathBuf,
    source_timestamp: Option<SourceTimestamp>,
    body_offset: u64,
    body_len: u64,
}

impl SpooledFilesystemFile {
    pub fn connector_key(&self) -> &[u8] {
        &self.connector_key
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub const fn source_timestamp(&self) -> Option<SourceTimestamp> {
        self.source_timestamp
    }

    pub const fn body_len(&self) -> u64 {
        self.body_len
    }
}

#[cfg(unix)]
pub fn discover_files(
    root: &Path,
    options: &DiscoveryOptions,
) -> Result<FilesystemSnapshot, DiscoveryError> {
    unix::discover_files(root, options)
}

#[cfg(unix)]
pub fn discover_files_spooled(
    root: &Path,
    options: &DiscoveryOptions,
    staging_directory: &Path,
) -> Result<FilesystemSpool, DiscoveryError> {
    unix::discover_files_spooled(root, options, staging_directory, options.max_source_bytes())
}

#[cfg(unix)]
pub(crate) fn discover_files_spooled_bounded(
    root: &Path,
    options: &DiscoveryOptions,
    staging_directory: &Path,
    max_spool_bytes: u64,
) -> Result<FilesystemSpool, DiscoveryError> {
    unix::discover_files_spooled(root, options, staging_directory, max_spool_bytes)
}

#[cfg(unix)]
pub(crate) fn estimate_filesystem_discovery(
    root: &Path,
    options: &DiscoveryOptions,
) -> Result<FilesystemDiscoveryEstimate, DiscoveryError> {
    unix::estimate_filesystem_discovery(root, options)
}

#[cfg(unix)]
pub(crate) use unix::open_source_root;

#[cfg(not(unix))]
pub fn discover_files_spooled(
    _root: &Path,
    _options: &DiscoveryOptions,
    _staging_directory: &Path,
) -> Result<FilesystemSpool, DiscoveryError> {
    Err(DiscoveryError::UnsupportedPlatform)
}

#[cfg(not(unix))]
pub(crate) fn discover_files_spooled_bounded(
    _root: &Path,
    _options: &DiscoveryOptions,
    _staging_directory: &Path,
    _max_spool_bytes: u64,
) -> Result<FilesystemSpool, DiscoveryError> {
    Err(DiscoveryError::UnsupportedPlatform)
}

#[cfg(not(unix))]
pub(crate) fn estimate_filesystem_discovery(
    _root: &Path,
    _options: &DiscoveryOptions,
) -> Result<FilesystemDiscoveryEstimate, DiscoveryError> {
    Err(DiscoveryError::UnsupportedPlatform)
}

#[cfg(not(unix))]
pub fn discover_files(
    _root: &Path,
    _options: &DiscoveryOptions,
) -> Result<FilesystemSnapshot, DiscoveryError> {
    Err(DiscoveryError::UnsupportedPlatform)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DiscoveryConfigError {
    #[error(
        "maximum file size must be between 1 byte and the 16 MiB hard ceiling; requested {requested}"
    )]
    InvalidFileLimit { requested: u64 },
    #[error(
        "source limits must be nonzero and no greater than the hard ceilings \
         ({max_files} files, {max_bytes} bytes requested)"
    )]
    InvalidSourceLimit { max_files: usize, max_bytes: u64 },
    #[error("filesystem traversal limits must be nonzero and no greater than the hard ceilings")]
    InvalidTraversalLimit,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DiscoveryError {
    #[error("filesystem discovery is supported only on macOS and Linux")]
    UnsupportedPlatform,
    #[error("source root does not exist")]
    RootMissing { path: PathBuf },
    #[error("source root is a symlink and will not be followed")]
    RootIsSymlink { path: PathBuf },
    #[error("source root is not a directory")]
    RootNotDirectory { path: PathBuf },
    #[error("source root could not be opened as a directory capability")]
    RootOpen { path: PathBuf, detail: String },
    #[error("directory enumeration was incomplete")]
    DirectoryUnreadable { path: PathBuf, detail: String },
    #[error("directory identity changed while it was opened")]
    DirectoryChanged { path: PathBuf },
    #[error("invalid gitignore rule")]
    InvalidIgnoreRule { path: PathBuf, detail: String },
    #[error("gitignore file is invalid or changed during discovery")]
    InvalidIgnoreFile { path: PathBuf, detail: String },
    #[error("invalid explicit {kind} pattern")]
    InvalidPattern {
        kind: &'static str,
        pattern: String,
        detail: String,
    },
    #[error(
        "source exceeds the configured discovery budget ({files} files, {bytes} bytes; limits are {max_files} files and {max_bytes} bytes)"
    )]
    SourceLimitExceeded {
        files: usize,
        bytes: u64,
        max_files: usize,
        max_bytes: u64,
    },
    #[error("filesystem traversal exceeded the {kind} limit of {limit}")]
    TraversalLimitExceeded { kind: &'static str, limit: usize },
    #[error(
        "source changed after storage preflight ({observed_bytes} staging bytes observed, \
         {estimated_bytes} bytes estimated)"
    )]
    StagingEstimateExceeded {
        estimated_bytes: u64,
        observed_bytes: u64,
    },
    #[error("private ingest staging failed")]
    Staging {
        kind: std::io::ErrorKind,
        detail: String,
    },
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, open, openat, statat};
    use std::ffi::{CStr, CString, OsString};
    use std::fs;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;
    use uuid::Uuid;

    const IGNORE_FILE_MAX_BYTES: u64 = 1024 * 1024;
    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const FILE_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    pub(super) fn discover_files(
        root: &Path,
        options: &DiscoveryOptions,
    ) -> Result<FilesystemSnapshot, DiscoveryError> {
        let mut sink = MemorySink::default();
        let issues = walk_files(root, options, &mut sink)?;
        sink.files
            .sort_by(|left, right| left.connector_key.cmp(&right.connector_key));
        Ok(FilesystemSnapshot {
            files: sink.files,
            issues,
        })
    }

    pub(super) fn discover_files_spooled(
        root: &Path,
        options: &DiscoveryOptions,
        staging_directory: &Path,
        max_spool_bytes: u64,
    ) -> Result<FilesystemSpool, DiscoveryError> {
        let temporary = staging_directory.join(format!(".hsum-ingest-{}.spool", Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(staging_error)?;
        if let Err(error) = fs::remove_file(&temporary) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(staging_error(error));
        }
        let mut entries = Vec::new();
        let mut sink = SpoolSink {
            file: &mut file,
            entries: &mut entries,
            accepted_bytes: 0,
            max_spool_bytes,
        };
        let issues = walk_files(root, options, &mut sink)?;
        file.flush().map_err(staging_error)?;
        entries.sort_by(|left, right| left.connector_key.cmp(&right.connector_key));
        Ok(FilesystemSpool {
            file,
            entries,
            issues,
        })
    }

    pub(super) fn estimate_filesystem_discovery(
        root: &Path,
        options: &DiscoveryOptions,
    ) -> Result<FilesystemDiscoveryEstimate, DiscoveryError> {
        let mut sink = CountingSink::default();
        let issues = walk_files(root, options, &mut sink)?;
        Ok(FilesystemDiscoveryEstimate {
            eligible_files: sink.eligible_files,
            eligible_bytes: sink.eligible_bytes,
            skipped_files: issues.len(),
        })
    }

    trait AcceptedFileSink {
        fn accept(&mut self, file: FilesystemFile) -> Result<(), DiscoveryError>;
    }

    #[derive(Default)]
    struct MemorySink {
        files: Vec<FilesystemFile>,
    }

    impl AcceptedFileSink for MemorySink {
        fn accept(&mut self, file: FilesystemFile) -> Result<(), DiscoveryError> {
            self.files.push(file);
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingSink {
        eligible_files: usize,
        eligible_bytes: u64,
    }

    impl AcceptedFileSink for CountingSink {
        fn accept(&mut self, file: FilesystemFile) -> Result<(), DiscoveryError> {
            let body_len =
                u64::try_from(file.original_bytes.len()).map_err(|_| DiscoveryError::Staging {
                    kind: std::io::ErrorKind::InvalidData,
                    detail: "accepted body length overflowed".to_owned(),
                })?;
            self.eligible_files =
                self.eligible_files
                    .checked_add(1)
                    .ok_or(DiscoveryError::Staging {
                        kind: std::io::ErrorKind::InvalidData,
                        detail: "accepted file count overflowed".to_owned(),
                    })?;
            self.eligible_bytes =
                self.eligible_bytes
                    .checked_add(body_len)
                    .ok_or(DiscoveryError::Staging {
                        kind: std::io::ErrorKind::InvalidData,
                        detail: "accepted byte count overflowed".to_owned(),
                    })?;
            Ok(())
        }
    }

    struct SpoolSink<'a> {
        file: &'a mut File,
        entries: &'a mut Vec<SpooledFilesystemFile>,
        accepted_bytes: u64,
        max_spool_bytes: u64,
    }

    impl AcceptedFileSink for SpoolSink<'_> {
        fn accept(&mut self, file: FilesystemFile) -> Result<(), DiscoveryError> {
            let body_offset = self.file.stream_position().map_err(staging_error)?;
            let body_len =
                u64::try_from(file.original_bytes.len()).map_err(|_| DiscoveryError::Staging {
                    kind: std::io::ErrorKind::InvalidData,
                    detail: "spooled body length overflowed".to_owned(),
                })?;
            let observed_bytes =
                self.accepted_bytes
                    .checked_add(body_len)
                    .ok_or(DiscoveryError::Staging {
                        kind: std::io::ErrorKind::InvalidData,
                        detail: "spooled byte count overflowed".to_owned(),
                    })?;
            if observed_bytes > self.max_spool_bytes {
                return Err(DiscoveryError::StagingEstimateExceeded {
                    estimated_bytes: self.max_spool_bytes,
                    observed_bytes,
                });
            }
            self.file
                .write_all(&file.original_bytes)
                .map_err(staging_error)?;
            self.accepted_bytes = observed_bytes;
            self.entries.push(SpooledFilesystemFile {
                connector_key: file.connector_key,
                relative_path: file.relative_path,
                source_timestamp: file.source_timestamp,
                body_offset,
                body_len,
            });
            Ok(())
        }
    }

    fn staging_error(error: std::io::Error) -> DiscoveryError {
        DiscoveryError::Staging {
            kind: error.kind(),
            detail: error.to_string(),
        }
    }

    fn walk_files(
        root: &Path,
        options: &DiscoveryOptions,
        sink: &mut dyn AcceptedFileSink,
    ) -> Result<Vec<FileIssue>, DiscoveryError> {
        let mut observer = NoopWalkObserver;
        walk_files_with_observer(root, options, sink, &mut observer)
    }

    trait WalkObserver {
        fn after_root_component_statted(&mut self, _absolute_component: &Path) {}
        fn after_directory_enumerated(&mut self, _relative_directory: &Path) {}
        fn after_ignore_loaded(&mut self, _relative_directory: &Path) {}
    }

    struct NoopWalkObserver;

    impl WalkObserver for NoopWalkObserver {}

    fn walk_files_with_observer(
        root: &Path,
        options: &DiscoveryOptions,
        sink: &mut dyn AcceptedFileSink,
        observer: &mut dyn WalkObserver,
    ) -> Result<Vec<FileIssue>, DiscoveryError> {
        let root_fd = open_source_root_with_observer(root, observer)?;

        let includes = build_explicit_matcher(&options.includes, true, "include")?;
        let excludes = build_explicit_matcher(&options.excludes, false, "exclude")?;
        let mut state = WalkState {
            options,
            includes,
            excludes,
            ignore_stack: Vec::new(),
            sink,
            issues: Vec::new(),
            eligible_files: 0,
            eligible_bytes: 0,
            visited_entries: 0,
            visited_directories: 1,
            observer,
        };
        walk_directory(&root_fd, Path::new(""), 0, &mut state)?;
        state
            .issues
            .sort_by(|left, right| left.connector_key.cmp(&right.connector_key));
        Ok(state.issues)
    }

    pub(crate) fn open_source_root(root: &Path) -> Result<OwnedFd, DiscoveryError> {
        let mut observer = NoopWalkObserver;
        open_source_root_with_observer(root, &mut observer)
    }

    fn open_source_root_with_observer(
        root: &Path,
        observer: &mut dyn WalkObserver,
    ) -> Result<OwnedFd, DiscoveryError> {
        if !root.is_absolute() {
            return Err(DiscoveryError::RootOpen {
                path: root.to_path_buf(),
                detail: "source root is not absolute".to_owned(),
            });
        }
        let normalized_root = normalize_system_root_alias(root);
        let mut directory =
            open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
                DiscoveryError::RootOpen {
                    path: PathBuf::from("/"),
                    detail: error.to_string(),
                }
            })?;
        let mut opened_path = PathBuf::from("/");
        for component in normalized_root.components() {
            let Component::Normal(component) = component else {
                if matches!(component, Component::RootDir | Component::CurDir) {
                    continue;
                }
                return Err(DiscoveryError::RootOpen {
                    path: root.to_path_buf(),
                    detail: "source root contains an unsupported path component".to_owned(),
                });
            };
            let name =
                CString::new(component.as_bytes()).map_err(|_| DiscoveryError::RootOpen {
                    path: root.to_path_buf(),
                    detail: "source root contains a NUL byte".to_owned(),
                })?;
            opened_path.push(component);
            let before = statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
                if error == rustix::io::Errno::NOENT {
                    DiscoveryError::RootMissing {
                        path: opened_path.clone(),
                    }
                } else {
                    DiscoveryError::RootOpen {
                        path: opened_path.clone(),
                        detail: error.to_string(),
                    }
                }
            })?;
            match FileType::from_raw_mode(before.st_mode) {
                FileType::Symlink => {
                    return Err(DiscoveryError::RootIsSymlink { path: opened_path });
                }
                FileType::Directory => {}
                _ => {
                    return Err(DiscoveryError::RootNotDirectory { path: opened_path });
                }
            }
            observer.after_root_component_statted(&opened_path);
            let child =
                openat(&directory, &name, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
                    DiscoveryError::RootOpen {
                        path: opened_path.clone(),
                        detail: error.to_string(),
                    }
                })?;
            let opened = fstat(&child).map_err(|error| DiscoveryError::RootOpen {
                path: opened_path.clone(),
                detail: error.to_string(),
            })?;
            if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
                || !same_identity(&before, &opened)
            {
                return Err(DiscoveryError::DirectoryChanged { path: opened_path });
            }
            directory = child;
        }
        Ok(directory)
    }

    #[cfg(target_os = "macos")]
    fn normalize_system_root_alias(root: &Path) -> PathBuf {
        for (alias, physical) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
        ] {
            if let Ok(remainder) = root.strip_prefix(alias) {
                return physical.join(remainder);
            }
        }
        root.to_path_buf()
    }

    #[cfg(not(target_os = "macos"))]
    fn normalize_system_root_alias(root: &Path) -> PathBuf {
        root.to_path_buf()
    }

    struct WalkState<'a> {
        options: &'a DiscoveryOptions,
        includes: Gitignore,
        excludes: Gitignore,
        ignore_stack: Vec<Gitignore>,
        sink: &'a mut dyn AcceptedFileSink,
        issues: Vec<FileIssue>,
        eligible_files: usize,
        eligible_bytes: u64,
        visited_entries: usize,
        visited_directories: usize,
        observer: &'a mut dyn WalkObserver,
    }

    fn walk_directory(
        directory_fd: &OwnedFd,
        relative_directory: &Path,
        depth: usize,
        state: &mut WalkState<'_>,
    ) -> Result<(), DiscoveryError> {
        let before_directory =
            fstat(directory_fd).map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
        if depth > state.options.max_directory_depth {
            return Err(DiscoveryError::TraversalLimitExceeded {
                kind: "directory depth",
                limit: state.options.max_directory_depth,
            });
        }
        let mut directory =
            Dir::read_from(directory_fd).map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
        let mut names = Vec::new();
        for entry in &mut directory {
            let entry = entry.map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
            let name = entry.file_name();
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                state.visited_entries = state.visited_entries.saturating_add(1);
                if state.visited_entries > state.options.max_visited_entries {
                    return Err(DiscoveryError::TraversalLimitExceeded {
                        kind: "visited entries",
                        limit: state.options.max_visited_entries,
                    });
                }
                names.push(name.to_owned());
                if names.len() > state.options.max_entries_per_directory {
                    return Err(DiscoveryError::TraversalLimitExceeded {
                        kind: "entries per directory",
                        limit: state.options.max_entries_per_directory,
                    });
                }
            }
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        state
            .observer
            .after_directory_enumerated(relative_directory);

        let mut entry_snapshots = Vec::with_capacity(names.len());
        for name in &names {
            let component = OsString::from_vec(name.as_bytes().to_vec());
            let relative_path = relative_directory.join(component);
            let stat = statat(directory_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
                DiscoveryError::DirectoryUnreadable {
                    path: relative_path,
                    detail: error.to_string(),
                }
            })?;
            entry_snapshots.push(stat);
        }

        if let Some(ignore_matcher) =
            load_local_ignore(directory_fd, relative_directory, &names, &entry_snapshots)?
        {
            state.ignore_stack.push(ignore_matcher);
        } else {
            state.ignore_stack.push(Gitignore::empty());
        }
        state.observer.after_ignore_loaded(relative_directory);

        for (name, stat) in names.iter().zip(&entry_snapshots) {
            if name.as_bytes() == b".gitignore" {
                continue;
            }
            let component = OsString::from_vec(name.as_bytes().to_vec());
            let relative_path = relative_directory.join(component);
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Symlink => {}
                FileType::Directory => {
                    if path_is_excluded(&relative_path, true, state) {
                        continue;
                    }
                    let child_fd = openat(directory_fd, name, DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|error| DiscoveryError::DirectoryUnreadable {
                            path: relative_path.clone(),
                            detail: error.to_string(),
                        })?;
                    let opened =
                        fstat(&child_fd).map_err(|error| DiscoveryError::DirectoryUnreadable {
                            path: relative_path.clone(),
                            detail: error.to_string(),
                        })?;
                    if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
                        || !same_identity(stat, &opened)
                    {
                        return Err(DiscoveryError::DirectoryChanged {
                            path: relative_path,
                        });
                    }
                    state.visited_directories = state.visited_directories.saturating_add(1);
                    if state.visited_directories > state.options.max_visited_directories {
                        return Err(DiscoveryError::TraversalLimitExceeded {
                            kind: "visited directories",
                            limit: state.options.max_visited_directories,
                        });
                    }
                    walk_directory(&child_fd, &relative_path, depth + 1, state)?;
                }
                FileType::RegularFile => {
                    if !is_supported_path(&relative_path)
                        || path_is_excluded(&relative_path, false, state)
                    {
                        continue;
                    }
                    record_eligible(stat, state)?;
                    let connector_key = relative_path.as_os_str().as_bytes().to_vec();
                    if stat_size(stat).is_none_or(|size| size > state.options.max_file_bytes) {
                        state.issues.push(FileIssue {
                            connector_key,
                            relative_path,
                            kind: FileIssueKind::FileTooLarge,
                        });
                        continue;
                    }
                    match read_document(directory_fd, name, stat, state.options.max_file_bytes) {
                        ReadOutcome::Accepted {
                            bytes: original_bytes,
                            source_timestamp,
                        } => {
                            state.sink.accept(FilesystemFile {
                                connector_key,
                                relative_path,
                                original_bytes,
                                source_timestamp,
                            })?;
                        }
                        ReadOutcome::Issue(kind) => {
                            state.issues.push(FileIssue {
                                connector_key,
                                relative_path,
                                kind,
                            });
                        }
                    }
                }
                FileType::Fifo
                | FileType::Socket
                | FileType::CharacterDevice
                | FileType::BlockDevice
                | FileType::Unknown => {}
            }
        }

        state.ignore_stack.pop();
        let mut after_names = Vec::new();
        let mut after_enumeration =
            Dir::read_from(directory_fd).map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
        for entry in &mut after_enumeration {
            let entry = entry.map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
            let name = entry.file_name();
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                after_names.push(name.to_owned());
                if after_names.len() > state.options.max_entries_per_directory {
                    return Err(DiscoveryError::DirectoryChanged {
                        path: relative_directory.to_path_buf(),
                    });
                }
            }
        }
        after_names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let after_directory =
            fstat(directory_fd).map_err(|error| DiscoveryError::DirectoryUnreadable {
                path: relative_directory.to_path_buf(),
                detail: error.to_string(),
            })?;
        if !same_directory_snapshot(&before_directory, &names, &after_directory, &after_names) {
            return Err(DiscoveryError::DirectoryChanged {
                path: relative_directory.to_path_buf(),
            });
        }
        for (name, before_entry) in names.iter().zip(&entry_snapshots) {
            let Ok(after_entry) = statat(directory_fd, name, AtFlags::SYMLINK_NOFOLLOW) else {
                return Err(DiscoveryError::DirectoryChanged {
                    path: relative_directory.to_path_buf(),
                });
            };
            if !same_entry_snapshot(before_entry, &after_entry) {
                return Err(DiscoveryError::DirectoryChanged {
                    path: relative_directory.to_path_buf(),
                });
            }
        }
        Ok(())
    }

    fn same_directory_snapshot(
        before: &Stat,
        before_names: &[CString],
        after: &Stat,
        after_names: &[CString],
    ) -> bool {
        same_identity(before, after)
            && before.st_size == after.st_size
            && before.st_mtime == after.st_mtime
            && before.st_mtime_nsec == after.st_mtime_nsec
            && before.st_ctime == after.st_ctime
            && before.st_ctime_nsec == after.st_ctime_nsec
            && before_names == after_names
    }

    fn same_entry_snapshot(before: &Stat, after: &Stat) -> bool {
        same_identity(before, after)
            && before.st_mode == after.st_mode
            && before.st_size == after.st_size
            && before.st_mtime == after.st_mtime
            && before.st_mtime_nsec == after.st_mtime_nsec
            && before.st_ctime == after.st_ctime
            && before.st_ctime_nsec == after.st_ctime_nsec
    }

    fn load_local_ignore(
        directory_fd: &OwnedFd,
        relative_directory: &Path,
        names: &[CString],
        entry_snapshots: &[Stat],
    ) -> Result<Option<Gitignore>, DiscoveryError> {
        let Some((index, name)) = names
            .iter()
            .enumerate()
            .find(|(_, name)| name.as_bytes() == b".gitignore")
        else {
            return Ok(None);
        };
        let ignore_path = relative_directory.join(".gitignore");
        let stat = &entry_snapshots[index];
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(None);
        }
        if stat_size(stat).is_none_or(|size| size > IGNORE_FILE_MAX_BYTES) {
            return Err(DiscoveryError::InvalidIgnoreFile {
                path: ignore_path,
                detail: "ignore file exceeds the 1 MiB parser limit".to_owned(),
            });
        }
        let bytes = match read_regular_file(directory_fd, name, stat, IGNORE_FILE_MAX_BYTES) {
            ReadOutcome::Accepted { bytes, .. } => bytes,
            ReadOutcome::Issue(kind) => {
                return Err(DiscoveryError::InvalidIgnoreFile {
                    path: ignore_path,
                    detail: format!("{kind:?}"),
                });
            }
        };
        let contents =
            std::str::from_utf8(&bytes).map_err(|error| DiscoveryError::InvalidIgnoreFile {
                path: ignore_path.clone(),
                detail: error.to_string(),
            })?;
        let mut builder = GitignoreBuilder::new(relative_directory);
        for line in contents.lines() {
            builder
                .add_line(Some(ignore_path.clone()), line)
                .map_err(|error| DiscoveryError::InvalidIgnoreRule {
                    path: ignore_path.clone(),
                    detail: error.to_string(),
                })?;
        }
        builder
            .build()
            .map(Some)
            .map_err(|error| DiscoveryError::InvalidIgnoreRule {
                path: ignore_path,
                detail: error.to_string(),
            })
    }

    fn build_explicit_matcher(
        patterns: &[String],
        whitelist: bool,
        kind: &'static str,
    ) -> Result<Gitignore, DiscoveryError> {
        let mut builder = GitignoreBuilder::new("");
        for pattern in patterns {
            let line = if whitelist {
                format!("!{pattern}")
            } else {
                pattern.clone()
            };
            builder
                .add_line(None, &line)
                .map_err(|error| DiscoveryError::InvalidPattern {
                    kind,
                    pattern: pattern.clone(),
                    detail: error.to_string(),
                })?;
        }
        builder
            .build()
            .map_err(|error| DiscoveryError::InvalidPattern {
                kind,
                pattern: "<matcher>".to_owned(),
                detail: error.to_string(),
            })
    }

    fn path_is_excluded(path: &Path, is_directory: bool, state: &WalkState<'_>) -> bool {
        if state.excludes.matched(path, is_directory).is_ignore() {
            return true;
        }

        let directly_included = state.includes.matched(path, is_directory).is_whitelist();
        let may_contain_included =
            is_directory && include_may_descend(path, &state.options.includes);
        if !state.options.includes.is_empty() && !directly_included && !may_contain_included {
            return true;
        }

        let default_excluded = default_excluded(path, is_directory);
        let sensitive = sensitive_path(path);
        if sensitive
            && !((directly_included || may_contain_included) && state.options.allow_sensitive)
        {
            return true;
        }
        if default_excluded && !directly_included && !may_contain_included {
            return true;
        }

        if directly_included || may_contain_included {
            return false;
        }
        state
            .ignore_stack
            .iter()
            .rev()
            .find_map(|matcher| match matcher.matched(path, is_directory) {
                Match::Ignore(_) => Some(true),
                Match::Whitelist(_) => Some(false),
                Match::None => None,
            })
            .unwrap_or(false)
    }

    fn include_may_descend(path: &Path, includes: &[String]) -> bool {
        let Some(path) = path.to_str() else {
            return false;
        };
        let prefix = format!("{path}/");
        includes.iter().any(|pattern| {
            let pattern = pattern.trim_start_matches('/');
            pattern.starts_with(&prefix)
                || pattern.starts_with("**/")
                || pattern == "**"
                || pattern == "**/*"
        })
    }

    fn default_excluded(path: &Path, is_directory: bool) -> bool {
        let name = path
            .file_name()
            .map(|name| name.as_bytes())
            .unwrap_or_default();
        if name.starts_with(b".") {
            return true;
        }
        is_directory
            && matches!(
                name,
                b"target"
                    | b"node_modules"
                    | b"build"
                    | b"dist"
                    | b"out"
                    | b"vendor"
                    | b"__pycache__"
                    | b"venv"
            )
    }

    fn sensitive_path(path: &Path) -> bool {
        let components: Vec<_> = path
            .components()
            .map(|component| component.as_os_str().as_bytes())
            .collect();
        if components.iter().any(|component| {
            matches!(
                *component,
                b".git"
                    | b".ssh"
                    | b".aws"
                    | b".gnupg"
                    | b".config"
                    | b".cache"
                    | b".idea"
                    | b".vscode"
            )
        }) {
            return true;
        }
        let Some(name) = components.last().copied() else {
            return false;
        };
        name == b".env"
            || name.starts_with(b".env.")
            || matches!(
                name,
                b"id_rsa"
                    | b"id_dsa"
                    | b"id_ecdsa"
                    | b"id_ed25519"
                    | b"credentials"
                    | b"credentials.json"
            )
            || matches!(
                path.extension().map(|extension| extension.as_bytes()),
                Some(b"pem" | b"key" | b"p12" | b"pfx")
            )
    }

    fn is_supported_path(path: &Path) -> bool {
        matches!(
            path.extension().map(|extension| extension.as_bytes()),
            Some(
                b"md"
                    | b"markdown"
                    | b"txt"
                    | b"rs"
                    | b"py"
                    | b"ts"
                    | b"tsx"
                    | b"js"
                    | b"jsx"
                    | b"go"
            )
        )
    }

    fn record_eligible(stat: &Stat, state: &mut WalkState<'_>) -> Result<(), DiscoveryError> {
        state.eligible_files = state.eligible_files.saturating_add(1);
        state.eligible_bytes = state
            .eligible_bytes
            .saturating_add(stat_size(stat).unwrap_or(u64::MAX));
        if state.eligible_files > state.options.max_source_files
            || state.eligible_bytes > state.options.max_source_bytes
        {
            return Err(DiscoveryError::SourceLimitExceeded {
                files: state.eligible_files,
                bytes: state.eligible_bytes,
                max_files: state.options.max_source_files,
                max_bytes: state.options.max_source_bytes,
            });
        }
        Ok(())
    }

    enum ReadOutcome {
        Accepted {
            bytes: Vec<u8>,
            source_timestamp: Option<SourceTimestamp>,
        },
        Issue(FileIssueKind),
    }

    fn read_document(
        directory_fd: &OwnedFd,
        name: &CStr,
        enumerated: &Stat,
        max_bytes: u64,
    ) -> ReadOutcome {
        let outcome = read_regular_file(directory_fd, name, enumerated, max_bytes);
        match outcome {
            ReadOutcome::Accepted { bytes, .. } if bytes.contains(&0) => {
                ReadOutcome::Issue(FileIssueKind::NulContent)
            }
            ReadOutcome::Accepted { bytes, .. } if std::str::from_utf8(&bytes).is_err() => {
                ReadOutcome::Issue(FileIssueKind::InvalidUtf8)
            }
            other => other,
        }
    }

    fn read_regular_file(
        directory_fd: &OwnedFd,
        name: &CStr,
        enumerated: &Stat,
        max_bytes: u64,
    ) -> ReadOutcome {
        for attempt in 0..2 {
            let descriptor = match openat(directory_fd, name, FILE_FLAGS, Mode::empty()) {
                Ok(descriptor) => descriptor,
                Err(error) if error == rustix::io::Errno::LOOP => {
                    return ReadOutcome::Issue(FileIssueKind::ChangedDuringRead);
                }
                Err(error) if error == rustix::io::Errno::ACCESS => {
                    return ReadOutcome::Issue(FileIssueKind::PermissionDenied);
                }
                Err(_) => return ReadOutcome::Issue(FileIssueKind::ReadFailed),
            };
            let before = match fstat(&descriptor) {
                Ok(stat) => stat,
                Err(_) => return ReadOutcome::Issue(FileIssueKind::ReadFailed),
            };
            if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
                return ReadOutcome::Issue(FileIssueKind::ChangedDuringRead);
            }
            if !same_identity(enumerated, &before) {
                if attempt == 0 {
                    continue;
                }
                return ReadOutcome::Issue(FileIssueKind::ChangedDuringRead);
            }
            if stat_size(&before).is_none_or(|size| size > max_bytes) {
                return ReadOutcome::Issue(FileIssueKind::FileTooLarge);
            }

            let mut file = fs::File::from(descriptor);
            let mut bytes = Vec::with_capacity(
                usize::try_from(stat_size(&before).unwrap_or_default()).unwrap_or_default(),
            );
            let read_result = (&mut file)
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes);
            if read_result.is_err() {
                return ReadOutcome::Issue(FileIssueKind::ReadFailed);
            }
            if bytes.len() as u64 > max_bytes {
                return ReadOutcome::Issue(FileIssueKind::FileTooLarge);
            }
            let after = match fstat(file.as_fd()) {
                Ok(stat) => stat,
                Err(_) => return ReadOutcome::Issue(FileIssueKind::ReadFailed),
            };
            if !same_snapshot(&before, &after) || stat_size(&after) != Some(bytes.len() as u64) {
                if attempt == 0 {
                    continue;
                }
                return ReadOutcome::Issue(FileIssueKind::ChangedDuringRead);
            }
            return ReadOutcome::Accepted {
                bytes,
                source_timestamp: source_timestamp(&after),
            };
        }
        ReadOutcome::Issue(FileIssueKind::ChangedDuringRead)
    }

    fn same_identity(left: &Stat, right: &Stat) -> bool {
        left.st_dev == right.st_dev && left.st_ino == right.st_ino
    }

    fn same_snapshot(left: &Stat, right: &Stat) -> bool {
        same_identity(left, right)
            && left.st_size == right.st_size
            && left.st_mtime == right.st_mtime
            && left.st_mtime_nsec == right.st_mtime_nsec
    }

    fn stat_size(stat: &Stat) -> Option<u64> {
        u64::try_from(stat.st_size).ok()
    }

    fn source_timestamp(stat: &Stat) -> Option<SourceTimestamp> {
        Some(SourceTimestamp {
            unix_seconds: stat.st_mtime,
            nanoseconds: u32::try_from(stat.st_mtime_nsec).ok()?,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[cfg(not(target_os = "linux"))]
        use std::process::Command;
        #[cfg(not(target_os = "linux"))]
        use std::thread;
        #[cfg(not(target_os = "linux"))]
        use std::time::{Duration, Instant};
        use tempfile::tempdir;

        struct DirectoryMutationObserver {
            root: PathBuf,
            mutated: bool,
        }

        impl WalkObserver for DirectoryMutationObserver {
            fn after_directory_enumerated(&mut self, relative_directory: &Path) {
                if relative_directory == Path::new("nested") && !self.mutated {
                    fs::write(self.root.join("appeared-during-walk.md"), b"late\n").unwrap();
                    self.mutated = true;
                }
            }
        }

        struct IgnoreMutationObserver {
            root: PathBuf,
            mutated: bool,
        }

        impl WalkObserver for IgnoreMutationObserver {
            fn after_ignore_loaded(&mut self, relative_directory: &Path) {
                if relative_directory.as_os_str().is_empty() && !self.mutated {
                    fs::write(self.root.join(".gitignore"), b"").unwrap();
                    self.mutated = true;
                }
            }
        }

        struct RootComponentSwapObserver {
            target: PathBuf,
            saved: PathBuf,
            outside: PathBuf,
            mutated: bool,
        }

        impl WalkObserver for RootComponentSwapObserver {
            fn after_root_component_statted(&mut self, absolute_component: &Path) {
                if absolute_component == self.target && !self.mutated {
                    fs::rename(&self.target, &self.saved).unwrap();
                    std::os::unix::fs::symlink(&self.outside, &self.target).unwrap();
                    self.mutated = true;
                }
            }
        }

        #[test]
        fn recursive_walk_rejects_a_parent_directory_mutated_while_visiting_a_child() {
            let root = tempdir().unwrap();
            fs::create_dir(root.path().join("nested")).unwrap();
            fs::write(root.path().join("nested/stable.md"), b"stable\n").unwrap();
            let mut observer = DirectoryMutationObserver {
                root: root.path().to_path_buf(),
                mutated: false,
            };
            let mut sink = MemorySink::default();

            let error = walk_files_with_observer(
                root.path(),
                &DiscoveryOptions::default(),
                &mut sink,
                &mut observer,
            )
            .unwrap_err();

            assert!(observer.mutated);
            assert!(matches!(
                error,
                DiscoveryError::DirectoryChanged { ref path } if path.as_os_str().is_empty()
            ));
        }

        #[test]
        fn directory_snapshot_rejects_name_changes_even_when_metadata_is_identical() {
            let root = tempdir().unwrap();
            let root_fd = open(root.path(), DIRECTORY_FLAGS, Mode::empty()).unwrap();
            let unchanged_metadata = fstat(&root_fd).unwrap();
            let before = vec![CString::new("stable.md").unwrap()];
            let after = vec![
                CString::new("appeared.md").unwrap(),
                CString::new("stable.md").unwrap(),
            ];

            assert!(!same_directory_snapshot(
                &unchanged_metadata,
                &before,
                &unchanged_metadata,
                &after,
            ));
        }

        #[test]
        fn ignore_file_changed_after_matcher_load_invalidates_the_walk() {
            let root = tempdir().unwrap();
            fs::write(root.path().join(".gitignore"), b"ignored.md\n").unwrap();
            fs::write(root.path().join("ignored.md"), b"ignored\n").unwrap();
            fs::write(root.path().join("kept.md"), b"kept\n").unwrap();
            let mut observer = IgnoreMutationObserver {
                root: root.path().to_path_buf(),
                mutated: false,
            };
            let mut sink = MemorySink::default();

            let error = walk_files_with_observer(
                root.path(),
                &DiscoveryOptions::default(),
                &mut sink,
                &mut observer,
            )
            .unwrap_err();

            assert!(observer.mutated);
            assert!(matches!(
                error,
                DiscoveryError::DirectoryChanged { ref path } if path.as_os_str().is_empty()
            ));
        }

        #[test]
        fn source_root_component_swap_cannot_redirect_the_walk() {
            let fixture = tempdir().unwrap();
            let base = fs::canonicalize(fixture.path()).unwrap();
            let trusted = base.join("trusted");
            let saved = base.join("trusted-saved");
            let outside = base.join("outside");
            fs::create_dir_all(trusted.join("source")).unwrap();
            fs::create_dir_all(outside.join("source")).unwrap();
            fs::write(trusted.join("source/inside.md"), b"inside\n").unwrap();
            fs::write(outside.join("source/outside.md"), b"outside\n").unwrap();
            let mut observer = RootComponentSwapObserver {
                target: trusted.clone(),
                saved: saved.clone(),
                outside,
                mutated: false,
            };
            let mut sink = MemorySink::default();

            let error = walk_files_with_observer(
                &trusted.join("source"),
                &DiscoveryOptions::default(),
                &mut sink,
                &mut observer,
            )
            .unwrap_err();

            assert!(observer.mutated);
            assert!(sink.files.is_empty());
            assert!(matches!(
                error,
                DiscoveryError::RootOpen { .. } | DiscoveryError::DirectoryChanged { .. }
            ));
            fs::remove_file(&trusted).unwrap();
            fs::rename(saved, trusted).unwrap();
        }

        #[derive(Clone, Copy)]
        enum CandidateReplacement {
            Fifo,
            Symlink,
            Directory,
        }

        fn create_fifo(_root_fd: &OwnedFd, _path: &Path) {
            #[cfg(target_os = "linux")]
            rustix::fs::mkfifoat(_root_fd, c"candidate.md", Mode::RUSR.union(Mode::WUSR)).unwrap();

            #[cfg(not(target_os = "linux"))]
            {
                let mut child = Command::new("/usr/bin/mkfifo").arg(_path).spawn().unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match child.try_wait().unwrap() {
                        Some(status) => {
                            assert!(status.success());
                            break;
                        }
                        None if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        None => {
                            child.kill().unwrap();
                            child.wait().unwrap();
                            panic!("mkfifo did not complete within the bounded test deadline");
                        }
                    }
                }
            }
        }

        fn assert_candidate_replacement_is_rejected(replacement: CandidateReplacement) {
            assert!(
                FILE_FLAGS.contains(OFlags::NONBLOCK),
                "a replaced FIFO must never turn discovery into a blocking open"
            );
            assert!(
                FILE_FLAGS.contains(OFlags::NOFOLLOW),
                "a replaced symlink must never be followed"
            );

            let root = tempdir().unwrap();
            let path = root.path().join("candidate.md");
            fs::write(&path, b"original\n").unwrap();
            let root_fd = open(root.path(), DIRECTORY_FLAGS, Mode::empty()).unwrap();
            let enumerated = statat(&root_fd, c"candidate.md", AtFlags::SYMLINK_NOFOLLOW).unwrap();
            fs::remove_file(&path).unwrap();
            match replacement {
                CandidateReplacement::Fifo => create_fifo(&root_fd, &path),
                CandidateReplacement::Symlink => {
                    std::os::unix::fs::symlink("replacement-target.md", &path).unwrap();
                }
                CandidateReplacement::Directory => fs::create_dir(&path).unwrap(),
            }

            assert!(matches!(
                read_document(
                    &root_fd,
                    c"candidate.md",
                    &enumerated,
                    DEFAULT_MAX_FILE_BYTES,
                ),
                ReadOutcome::Issue(FileIssueKind::ChangedDuringRead)
            ));
        }

        #[test]
        fn regular_candidate_replaced_by_fifo_is_rejected_without_blocking() {
            assert_candidate_replacement_is_rejected(CandidateReplacement::Fifo);
        }

        #[test]
        fn regular_candidate_replaced_by_symlink_is_rejected_without_following_it() {
            assert_candidate_replacement_is_rejected(CandidateReplacement::Symlink);
        }

        #[test]
        fn regular_candidate_replaced_by_non_regular_entry_is_rejected() {
            assert_candidate_replacement_is_rejected(CandidateReplacement::Directory);
        }

        #[test]
        fn staging_fault_preserves_storage_full_for_public_error_mapping() {
            let error = staging_error(std::io::Error::from(std::io::ErrorKind::StorageFull));

            assert!(matches!(
                error,
                DiscoveryError::Staging {
                    kind: std::io::ErrorKind::StorageFull,
                    ..
                }
            ));
        }
    }
}
