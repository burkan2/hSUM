use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json_canonicalizer::to_string as to_canonical_json;
use thiserror::Error;

use crate::ingest::{
    DiscoveryConfigError, DiscoveryOptions, HARD_MAX_DIRECTORY_DEPTH,
    HARD_MAX_ENTRIES_PER_DIRECTORY, HARD_MAX_VISITED_DIRECTORIES, HARD_MAX_VISITED_ENTRIES,
};

pub const FILESYSTEM_SOURCE_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const MAX_FILESYSTEM_SOURCE_CONFIG_BYTES: usize = 16 * 1024;
pub const JSONL_SOURCE_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const MAX_JSONL_SOURCE_CONFIG_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemSourceConfig {
    root: PathBuf,
    options: DiscoveryOptions,
    index_quota_bytes: Option<u64>,
}

impl FilesystemSourceConfig {
    pub fn new(root: PathBuf, options: DiscoveryOptions) -> Result<Self, SourceConfigError> {
        validate_root(&root)?;
        Ok(Self {
            root,
            options,
            index_quota_bytes: None,
        })
    }

    pub fn with_index_quota_bytes(
        mut self,
        index_quota_bytes: Option<u64>,
    ) -> Result<Self, SourceConfigError> {
        if index_quota_bytes == Some(0) {
            return Err(SourceConfigError::InvalidIndexQuota);
        }
        self.index_quota_bytes = index_quota_bytes;
        Ok(self)
    }

    pub fn parse(input: &str) -> Result<Self, SourceConfigError> {
        if input.len() > MAX_FILESYSTEM_SOURCE_CONFIG_BYTES {
            return Err(SourceConfigError::TooLarge);
        }
        let wire: FilesystemSourceConfigWire = serde_json::from_str(input)?;
        if wire.schema_version != FILESYSTEM_SOURCE_CONFIG_SCHEMA_VERSION {
            return Err(SourceConfigError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }
        let root = PathBuf::from(&wire.root);
        validate_root(&root)?;
        let mut options = DiscoveryOptions::default()
            .with_max_file_bytes(wire.max_file_bytes)?
            .with_source_limits(wire.max_source_files, wire.max_source_bytes)?
            .with_traversal_limits(
                wire.max_visited_entries,
                wire.max_visited_directories,
                wire.max_directory_depth,
                wire.max_entries_per_directory,
            )?
            .allow_sensitive(wire.allow_sensitive);
        for include in wire.includes {
            options = options.include(include);
        }
        for exclude in wire.excludes {
            options = options.exclude(exclude);
        }
        if wire.index_quota_bytes == Some(0) {
            return Err(SourceConfigError::InvalidIndexQuota);
        }
        Ok(Self {
            root,
            options,
            index_quota_bytes: wire.index_quota_bytes,
        })
    }

    /// Reads only the stable root field needed to verify immutable evidence.
    ///
    /// Early alpha indexes stored a minimal `{ "root": ... }` object, while
    /// current indexes persist the complete versioned discovery policy. Both
    /// shapes remain readable because retrieval does not depend on that policy.
    pub(crate) fn parse_stored_root(input: &str) -> Result<PathBuf, SourceConfigError> {
        if input.len() > MAX_FILESYSTEM_SOURCE_CONFIG_BYTES {
            return Err(SourceConfigError::TooLarge);
        }
        let wire: StoredFilesystemRootWire = serde_json::from_str(input)?;
        let root = PathBuf::from(wire.root);
        validate_root(&root)?;
        Ok(root)
    }

    pub fn to_canonical_json(&self) -> Result<String, SourceConfigError> {
        let root = self
            .root
            .to_str()
            .ok_or(SourceConfigError::NonUtf8Root)?
            .to_owned();
        let wire = FilesystemSourceConfigWire {
            schema_version: FILESYSTEM_SOURCE_CONFIG_SCHEMA_VERSION,
            root,
            max_file_bytes: self.options.max_file_bytes(),
            max_source_files: self.options.max_source_files(),
            max_source_bytes: self.options.max_source_bytes(),
            includes: self.options.includes().to_vec(),
            excludes: self.options.excludes().to_vec(),
            allow_sensitive: self.options.allows_sensitive(),
            max_visited_entries: self.options.max_visited_entries(),
            max_visited_directories: self.options.max_visited_directories(),
            max_directory_depth: self.options.max_directory_depth(),
            max_entries_per_directory: self.options.max_entries_per_directory(),
            index_quota_bytes: self.index_quota_bytes,
        };
        to_canonical_json(&wire).map_err(SourceConfigError::Serialize)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn discovery_options(&self) -> &DiscoveryOptions {
        &self.options
    }

    pub const fn index_quota_bytes(&self) -> Option<u64> {
        self.index_quota_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlSourceConfig {
    path: PathBuf,
    index_quota_bytes: Option<u64>,
}

impl JsonlSourceConfig {
    pub fn new(path: PathBuf) -> Result<Self, JsonlSourceConfigError> {
        validate_root(&path).map_err(JsonlSourceConfigError::Path)?;
        Ok(Self {
            path,
            index_quota_bytes: None,
        })
    }

    pub fn with_index_quota_bytes(
        mut self,
        index_quota_bytes: Option<u64>,
    ) -> Result<Self, JsonlSourceConfigError> {
        if index_quota_bytes == Some(0) {
            return Err(JsonlSourceConfigError::InvalidIndexQuota);
        }
        self.index_quota_bytes = index_quota_bytes;
        Ok(self)
    }

    pub fn parse(input: &str) -> Result<Self, JsonlSourceConfigError> {
        if input.len() > MAX_JSONL_SOURCE_CONFIG_BYTES {
            return Err(JsonlSourceConfigError::TooLarge);
        }
        let wire: JsonlSourceConfigWire = serde_json::from_str(input)?;
        if wire.schema_version != JSONL_SOURCE_CONFIG_SCHEMA_VERSION {
            return Err(JsonlSourceConfigError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }
        let path = PathBuf::from(wire.path);
        validate_root(&path).map_err(JsonlSourceConfigError::Path)?;
        if wire.index_quota_bytes == Some(0) {
            return Err(JsonlSourceConfigError::InvalidIndexQuota);
        }
        Ok(Self {
            path,
            index_quota_bytes: wire.index_quota_bytes,
        })
    }

    pub fn to_canonical_json(&self) -> Result<String, JsonlSourceConfigError> {
        let path = self
            .path
            .to_str()
            .ok_or(JsonlSourceConfigError::NonUtf8Path)?
            .to_owned();
        let wire = JsonlSourceConfigWire {
            schema_version: JSONL_SOURCE_CONFIG_SCHEMA_VERSION,
            path,
            index_quota_bytes: self.index_quota_bytes,
        };
        to_canonical_json(&wire).map_err(JsonlSourceConfigError::Serialize)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn index_quota_bytes(&self) -> Option<u64> {
        self.index_quota_bytes
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FilesystemSourceConfigWire {
    schema_version: u32,
    root: String,
    max_file_bytes: u64,
    max_source_files: usize,
    max_source_bytes: u64,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    allow_sensitive: bool,
    #[serde(default = "default_max_visited_entries")]
    max_visited_entries: usize,
    #[serde(default = "default_max_visited_directories")]
    max_visited_directories: usize,
    #[serde(default = "default_max_directory_depth")]
    max_directory_depth: usize,
    #[serde(default = "default_max_entries_per_directory")]
    max_entries_per_directory: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_quota_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StoredFilesystemRootWire {
    root: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JsonlSourceConfigWire {
    schema_version: u32,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_quota_bytes: Option<u64>,
}

const fn default_max_visited_entries() -> usize {
    HARD_MAX_VISITED_ENTRIES
}

const fn default_max_visited_directories() -> usize {
    HARD_MAX_VISITED_DIRECTORIES
}

const fn default_max_directory_depth() -> usize {
    HARD_MAX_DIRECTORY_DEPTH
}

const fn default_max_entries_per_directory() -> usize {
    HARD_MAX_ENTRIES_PER_DIRECTORY
}

fn validate_root(root: &Path) -> Result<(), SourceConfigError> {
    if !root.is_absolute() {
        return Err(SourceConfigError::RootNotAbsolute);
    }
    for component in root.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(SourceConfigError::RootNotLexicallyCanonical);
        }
    }
    let rendered = root.to_str().ok_or(SourceConfigError::NonUtf8Root)?;
    if rendered.as_bytes().contains(&0)
        || rendered.contains("//")
        || rendered.contains("/./")
        || rendered.ends_with("/.")
        || rendered.contains("/../")
        || rendered.ends_with("/..")
        || (rendered.len() > 1 && rendered.ends_with('/'))
    {
        return Err(SourceConfigError::RootNotLexicallyCanonical);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SourceConfigError {
    #[error("filesystem source configuration exceeds 16 KiB")]
    TooLarge,
    #[error("filesystem source configuration schema {found} is unsupported")]
    UnsupportedSchema { found: u32 },
    #[error("filesystem source root must be absolute")]
    RootNotAbsolute,
    #[error("filesystem source root is not lexically canonical")]
    RootNotLexicallyCanonical,
    #[error("filesystem source root is not UTF-8")]
    NonUtf8Root,
    #[error("filesystem source configuration is invalid")]
    Parse(#[from] serde_json::Error),
    #[error("filesystem source limits are invalid")]
    Discovery(#[from] DiscoveryConfigError),
    #[error("filesystem source index quota must be greater than zero")]
    InvalidIndexQuota,
    #[error("filesystem source configuration could not be serialized")]
    Serialize(serde_json::Error),
}

#[derive(Debug, Error)]
pub enum JsonlSourceConfigError {
    #[error("JSONL source configuration exceeds 16 KiB")]
    TooLarge,
    #[error("JSONL source configuration schema {found} is unsupported")]
    UnsupportedSchema { found: u32 },
    #[error("JSONL source path is invalid")]
    Path(#[source] SourceConfigError),
    #[error("JSONL source path is not UTF-8")]
    NonUtf8Path,
    #[error("JSONL source configuration is invalid")]
    Parse(#[from] serde_json::Error),
    #[error("JSONL source index quota must be greater than zero")]
    InvalidIndexQuota,
    #[error("JSONL source configuration could not be serialized")]
    Serialize(serde_json::Error),
}
