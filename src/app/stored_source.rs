use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::app::{
    FilesystemSourceConfig, JsonlSourceConfig, JsonlSourceConfigError,
    MAX_FILESYSTEM_SOURCE_CONFIG_BYTES, MAX_JSONL_SOURCE_CONFIG_BYTES, SourceConfigError,
};
use crate::domain::{ProjectId, SourceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum StoredSource {
    FilesystemRoot(PathBuf),
    SnapshotOnly,
}

#[derive(Debug, Error)]
pub(super) enum StoredSourceRootError {
    #[error("SQLite source-root lookup failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored filesystem source configuration is invalid")]
    SourceConfig(#[from] SourceConfigError),
    #[error("stored JSONL source configuration is invalid")]
    JsonlSourceConfig(#[from] JsonlSourceConfigError),
    #[error("stored source configuration exceeds the transport field limits")]
    FieldLimit,
    #[error("the cited evidence has no stored source")]
    Unavailable,
}

pub(super) fn stored_source(
    connection: &Connection,
    project_id: ProjectId,
    source_id: SourceId,
    root_bytes: usize,
) -> Result<StoredSource, StoredSourceRootError> {
    let config_limit = MAX_FILESYSTEM_SOURCE_CONFIG_BYTES.max(MAX_JSONL_SOURCE_CONFIG_BYTES);
    let (kind, config_len, config): (String, i64, Option<String>) = connection
        .query_row(
            "SELECT s.kind,
                    length(CAST(s.config_json AS BLOB)),
                    CASE
                        WHEN length(CAST(s.config_json AS BLOB)) <= ?3
                        THEN s.config_json
                    END
             FROM sources AS s
             JOIN project_sources AS ps ON ps.source_id = s.id
             WHERE ps.project_id = ?1 AND s.id = ?2",
            params![
                project_id.as_uuid().as_bytes().as_slice(),
                source_id.as_uuid().as_bytes().as_slice(),
                sqlite_limit(config_limit)?,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(StoredSourceRootError::Unavailable)?;
    if usize::try_from(config_len)
        .ok()
        .is_none_or(|len| len > config_limit)
    {
        return Err(StoredSourceRootError::FieldLimit);
    }
    let config = config.ok_or(StoredSourceRootError::Unavailable)?;
    match kind.as_str() {
        "filesystem" => {
            let root = FilesystemSourceConfig::parse_stored_root(&config)?;
            if root
                .to_str()
                .is_none_or(|rendered| rendered.len() > root_bytes)
            {
                return Err(StoredSourceRootError::FieldLimit);
            }
            Ok(StoredSource::FilesystemRoot(root))
        }
        "jsonl" => {
            JsonlSourceConfig::parse(&config)?;
            Ok(StoredSource::SnapshotOnly)
        }
        _ => Err(StoredSourceRootError::Unavailable),
    }
}

fn sqlite_limit(value: usize) -> Result<i64, StoredSourceRootError> {
    i64::try_from(value).map_err(|_| StoredSourceRootError::FieldLimit)
}
