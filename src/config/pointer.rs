use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::safe_file::{BoundedReadError, read_bounded_at};
use crate::domain::{SafeSlug, SlugError};

pub const POINTER_FILE_NAME: &str = ".hsum.toml";
const POINTER_SCHEMA_VERSION: u32 = 1;
const POINTER_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryPointer {
    index_name: SafeSlug,
    project_name: SafeSlug,
}

impl RepositoryPointer {
    pub fn new(index_name: SafeSlug, project_name: SafeSlug) -> Self {
        Self {
            index_name,
            project_name,
        }
    }

    pub fn parse(contents: &str) -> Result<Self, PointerError> {
        let wire: PointerWire = toml::from_str(contents).map_err(PointerError::Malformed)?;
        if wire.schema_version != POINTER_SCHEMA_VERSION {
            return Err(PointerError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }

        let index_name = SafeSlug::new(wire.index).map_err(PointerError::InvalidIndexName)?;
        let project_name = SafeSlug::new(wire.project).map_err(PointerError::InvalidProjectName)?;
        Ok(Self::new(index_name, project_name))
    }

    pub fn load(repository_root: &Path) -> Result<Option<Self>, PointerError> {
        let Some(bytes) = Self::read_bytes(repository_root)? else {
            return Ok(None);
        };
        let contents = std::str::from_utf8(&bytes).map_err(|_| PointerError::NotUtf8)?;
        Self::parse(contents).map(Some)
    }

    pub(crate) fn read_bytes(repository_root: &Path) -> Result<Option<Vec<u8>>, PointerError> {
        match read_bounded_at(
            repository_root,
            Path::new(POINTER_FILE_NAME),
            POINTER_MAX_BYTES,
            0o022,
        ) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(BoundedReadError::NotFound) => Ok(None),
            Err(BoundedReadError::Unsafe) => Err(PointerError::Unsafe),
            Err(BoundedReadError::TooLarge) => Err(PointerError::TooLarge),
            Err(BoundedReadError::Changed) => Err(PointerError::Changed),
            Err(BoundedReadError::Io(error)) => Err(PointerError::Read(error)),
        }
    }

    pub fn to_toml(&self) -> Result<String, PointerError> {
        toml::to_string_pretty(&PointerWireRef {
            schema_version: POINTER_SCHEMA_VERSION,
            index: self.index_name.as_str(),
            project: self.project_name.as_str(),
        })
        .map_err(PointerError::Serialize)
    }

    pub const fn schema_version(&self) -> u32 {
        POINTER_SCHEMA_VERSION
    }

    pub fn index_name(&self) -> &SafeSlug {
        &self.index_name
    }

    pub fn project_name(&self) -> &SafeSlug {
        &self.project_name
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointerWire {
    schema_version: u32,
    index: String,
    project: String,
}

#[derive(Serialize)]
struct PointerWireRef<'a> {
    schema_version: u32,
    index: &'a str,
    project: &'a str,
}

#[derive(Debug, Error)]
pub enum PointerError {
    #[error("repository pointer is not strict TOML")]
    Malformed(#[source] toml::de::Error),
    #[error("repository pointer schema {found} is unsupported")]
    UnsupportedSchema { found: u32 },
    #[error("repository pointer index name is invalid")]
    InvalidIndexName(#[source] SlugError),
    #[error("repository pointer project name is invalid")]
    InvalidProjectName(#[source] SlugError),
    #[error("repository pointer could not be read")]
    Read(#[source] std::io::Error),
    #[error("repository pointer must be a stable, user-owned regular file")]
    Unsafe,
    #[error("repository pointer exceeds the 16 KiB limit")]
    TooLarge,
    #[error("repository pointer changed while it was read")]
    Changed,
    #[error("repository pointer is not UTF-8")]
    NotUtf8,
    #[error("repository pointer could not be serialized: {0}")]
    Serialize(#[source] toml::ser::Error),
}
