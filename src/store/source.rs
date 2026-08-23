use std::time::Duration;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::{ProjectId, SafeSlug, SourceId};

use super::{Doctor, IndexDb, JsonlScope, StoreError, WriterLock};

const MAX_SOURCE_LOGICAL_URI_BYTES: usize = 4 * 1024;
const MAX_SOURCE_CONFIG_BYTES: usize = 16 * 1024;
const MAX_ACTIVE_SOURCES: usize = 64;
const MAX_SOURCE_NAME_BYTES: i64 = 64;
const MAX_ERROR_CODE_BYTES: i64 = 64;
const MAX_ERROR_DETAIL_BYTES: i64 = 64 * 1024;
const MAX_TIMESTAMP_BYTES: i64 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfiguredSourceKind {
    Filesystem,
    Jsonl,
}

impl ConfiguredSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Jsonl => "jsonl",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "filesystem" => Ok(Self::Filesystem),
            "jsonl" => Ok(Self::Jsonl),
            _ => Err(StoreError::UnsupportedSourceKind),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredSource {
    pub source_id: SourceId,
    pub kind: ConfiguredSourceKind,
    pub name: SafeSlug,
    pub logical_uri: String,
    pub config_json: String,
    pub active_documents: usize,
    pub last_success_at: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_detail: Option<String>,
    pub last_error_at: Option<String>,
    pub attached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRegistration {
    pub source_id: SourceId,
    pub created: bool,
    pub attached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemSourceRegistration {
    pub source_id: SourceId,
    pub created: bool,
    pub reactivated: bool,
    pub attached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceMembershipOutcome {
    pub source_id: SourceId,
    pub source_name: SafeSlug,
    pub attached: bool,
    pub changed: bool,
    pub scope_revision: u64,
}

pub fn configure_jsonl_source_with_timeout(
    database: &mut IndexDb,
    scope: &JsonlScope,
    lock_timeout: Duration,
) -> Result<SourceRegistration, StoreError> {
    validate_source_authority(&scope.source_logical_uri, &scope.source_config_json)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    Doctor::run(database.path())?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let project_matches: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1 AND name = ?2)",
        params![
            scope.project_id.as_uuid().as_bytes().as_slice(),
            scope.project_name.as_str(),
        ],
        |row| row.get(0),
    )?;
    if !project_matches {
        return Err(StoreError::ProjectNotFound);
    }

    let mut statement = transaction.prepare(
        "SELECT id, kind, name, logical_uri, config_json, removed_at
         FROM sources
         WHERE name = ?1 OR logical_uri = ?2
         ORDER BY id
         LIMIT 2",
    )?;
    let rows = statement
        .query_map(
            params![scope.source_name.as_str(), scope.source_logical_uri],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let (source_id, created) = match rows.as_slice() {
        [] => {
            enforce_active_source_capacity(&transaction, 1)?;
            transaction.execute(
                "INSERT INTO sources(
                    id, kind, name, logical_uri, config_json, created_at
                 ) VALUES (?1, 'jsonl', ?2, ?3, ?4, ?5)",
                params![
                    scope.source_id.as_uuid().as_bytes().as_slice(),
                    scope.source_name.as_str(),
                    scope.source_logical_uri,
                    scope.source_config_json,
                    now,
                ],
            )?;
            (scope.source_id, true)
        }
        [(id, kind, name, stored_uri, stored_config, removed_at)]
            if kind == "jsonl"
                && name == scope.source_name.as_str()
                && stored_uri.as_str() == scope.source_logical_uri.as_str()
                && stored_config.as_str() == scope.source_config_json.as_str() =>
        {
            let source_id = source_id(id)?;
            if removed_at.is_some() {
                enforce_active_source_capacity(&transaction, 1)?;
                transaction.execute(
                    "UPDATE sources SET removed_at = NULL WHERE id = ?1",
                    [source_id.as_uuid().as_bytes().as_slice()],
                )?;
            }
            (source_id, false)
        }
        _ => return Err(StoreError::SourceConflict),
    };

    let prior_membership = transaction
        .query_row(
            "SELECT removed_at
             FROM project_sources
             WHERE project_id = ?1 AND source_id = ?2",
            params![
                scope.project_id.as_uuid().as_bytes().as_slice(),
                source_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    transaction.execute(
        "INSERT INTO project_sources(project_id, source_id, removed_at)
         VALUES (?1, ?2, NULL)
         ON CONFLICT(project_id, source_id) DO UPDATE SET removed_at = NULL",
        params![
            scope.project_id.as_uuid().as_bytes().as_slice(),
            source_id.as_uuid().as_bytes().as_slice(),
        ],
    )?;
    let source_reactivated = rows
        .first()
        .is_some_and(|(_, _, _, _, _, removed_at)| removed_at.is_some());
    let attached =
        prior_membership.is_none_or(|removed_at| removed_at.is_some()) || source_reactivated;
    if attached {
        transaction.execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [scope.project_id.as_uuid().as_bytes().as_slice()],
        )?;
    }
    transaction.commit()?;
    drop(writer_lock);
    Ok(SourceRegistration {
        source_id,
        created,
        attached,
    })
}

pub fn configure_filesystem_source_with_timeout(
    database: &mut IndexDb,
    requested_source_id: SourceId,
    source_name: &SafeSlug,
    logical_uri: &str,
    config_json: &str,
    selected_project_id: ProjectId,
    lock_timeout: Duration,
) -> Result<FilesystemSourceRegistration, StoreError> {
    validate_source_authority(logical_uri, config_json)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    Doctor::run(database.path())?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let project_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [selected_project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    if !project_exists {
        return Err(StoreError::ProjectNotFound);
    }

    let mut statement = transaction.prepare(
        "SELECT id, kind, name, logical_uri, config_json, removed_at
         FROM sources
         WHERE name = ?1 OR logical_uri = ?2
         ORDER BY id
         LIMIT 2",
    )?;
    let rows = statement
        .query_map(params![source_name.as_str(), logical_uri], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let (source_id, created, reactivated) = match rows.as_slice() {
        [] => {
            enforce_active_source_capacity(&transaction, 1)?;
            transaction.execute(
                "INSERT INTO sources(
                    id, kind, name, logical_uri, config_json, created_at
                 ) VALUES (?1, 'filesystem', ?2, ?3, ?4, ?5)",
                params![
                    requested_source_id.as_uuid().as_bytes().as_slice(),
                    source_name.as_str(),
                    logical_uri,
                    config_json,
                    now,
                ],
            )?;
            (requested_source_id, true, false)
        }
        [(id, kind, name, stored_uri, stored_config, removed_at)]
            if kind == "filesystem"
                && name == source_name.as_str()
                && stored_uri == logical_uri
                && stored_config == config_json =>
        {
            let source_id = source_id(id)?;
            let reactivated = removed_at.is_some();
            if reactivated {
                enforce_active_source_capacity(&transaction, 1)?;
                transaction.execute(
                    "UPDATE sources SET removed_at = NULL WHERE id = ?1",
                    [source_id.as_uuid().as_bytes().as_slice()],
                )?;
            }
            (source_id, false, reactivated)
        }
        _ => return Err(StoreError::SourceConflict),
    };
    let attached: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM project_sources
             WHERE project_id = ?1 AND source_id = ?2 AND removed_at IS NULL
         )",
        params![
            selected_project_id.as_uuid().as_bytes().as_slice(),
            source_id.as_uuid().as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;
    transaction.commit()?;
    drop(writer_lock);
    Ok(FilesystemSourceRegistration {
        source_id,
        created,
        reactivated,
        attached,
    })
}

pub fn list_project_sources(
    database: &IndexDb,
    project_id: ProjectId,
) -> Result<Vec<ConfiguredSource>, StoreError> {
    let connection = database.connection();
    validate_listable_sources(connection)?;
    let project_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    if !project_exists {
        return Err(StoreError::ProjectNotFound);
    }
    let mut statement = connection.prepare(
        "SELECT s.id, s.kind, s.name, s.logical_uri, s.config_json,
                (
                    SELECT COUNT(*)
                    FROM documents AS d
                    JOIN document_heads AS dh ON dh.document_id = d.id
                    WHERE d.source_id = s.id AND dh.state = 'active'
                ),
                s.last_success_at, s.last_error_code, s.last_error_detail,
                s.last_error_at,
                1
         FROM project_sources AS ps
         JOIN sources AS s ON s.id = ps.source_id
         WHERE ps.project_id = ?1
           AND ps.removed_at IS NULL
           AND s.removed_at IS NULL
         ORDER BY s.name, s.id",
    )?;
    let sources = statement
        .query_map([project_id.as_uuid().as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })?
        .map(|row| {
            let (
                id,
                kind,
                name,
                logical_uri,
                config_json,
                active_documents,
                last_success_at,
                last_error_code,
                last_error_detail,
                last_error_at,
                attached,
            ) = row?;
            validate_source_authority(&logical_uri, &config_json)?;
            let name =
                SafeSlug::new(name).map_err(|_| StoreError::InvalidMetadata("source name"))?;
            Ok(ConfiguredSource {
                source_id: source_id(&id)?,
                kind: ConfiguredSourceKind::parse(&kind)?,
                name,
                logical_uri,
                config_json,
                active_documents: usize::try_from(active_documents)
                    .map_err(|_| StoreError::InvalidMetadata("active document count"))?,
                last_success_at,
                last_error_code,
                last_error_detail,
                last_error_at,
                attached: attached == 1,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    if sources.len() > MAX_ACTIVE_SOURCES {
        return Err(StoreError::SourceLimitExceeded {
            maximum: MAX_ACTIVE_SOURCES,
        });
    }
    Ok(sources)
}

pub fn list_index_sources(
    database: &IndexDb,
    project_id: ProjectId,
) -> Result<Vec<ConfiguredSource>, StoreError> {
    let connection = database.connection();
    validate_listable_sources(connection)?;
    let project_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    if !project_exists {
        return Err(StoreError::ProjectNotFound);
    }
    let mut statement = connection.prepare(
        "SELECT s.id, s.kind, s.name, s.logical_uri, s.config_json,
                (
                    SELECT COUNT(*)
                    FROM documents AS d
                    JOIN document_heads AS dh ON dh.document_id = d.id
                    WHERE d.source_id = s.id AND dh.state = 'active'
                ),
                s.last_success_at, s.last_error_code, s.last_error_detail,
                s.last_error_at,
                EXISTS(
                    SELECT 1 FROM project_sources AS ps
                    WHERE ps.project_id = ?1 AND ps.source_id = s.id
                      AND ps.removed_at IS NULL
                )
         FROM sources AS s
         WHERE s.removed_at IS NULL
         ORDER BY s.name, s.id",
    )?;
    let sources = statement
        .query_map([project_id.as_uuid().as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })?
        .map(|row| {
            let (
                id,
                kind,
                name,
                logical_uri,
                config_json,
                active_documents,
                last_success_at,
                last_error_code,
                last_error_detail,
                last_error_at,
                attached,
            ) = row?;
            validate_source_authority(&logical_uri, &config_json)?;
            Ok(ConfiguredSource {
                source_id: source_id(&id)?,
                kind: ConfiguredSourceKind::parse(&kind)?,
                name: SafeSlug::new(name)
                    .map_err(|_| StoreError::InvalidMetadata("source name"))?,
                logical_uri,
                config_json,
                active_documents: usize::try_from(active_documents)
                    .map_err(|_| StoreError::InvalidMetadata("active document count"))?,
                last_success_at,
                last_error_code,
                last_error_detail,
                last_error_at,
                attached: attached == 1,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    if sources.len() > MAX_ACTIVE_SOURCES {
        return Err(StoreError::SourceLimitExceeded {
            maximum: MAX_ACTIVE_SOURCES,
        });
    }
    Ok(sources)
}

pub fn attach_jsonl_source_with_timeout(
    database: &mut IndexDb,
    project_id: ProjectId,
    source_id: SourceId,
    lock_timeout: Duration,
) -> Result<SourceMembershipOutcome, StoreError> {
    change_jsonl_membership(database, project_id, source_id, true, lock_timeout)
}

pub fn detach_jsonl_source_with_timeout(
    database: &mut IndexDb,
    project_id: ProjectId,
    source_id: SourceId,
    lock_timeout: Duration,
) -> Result<SourceMembershipOutcome, StoreError> {
    change_jsonl_membership(database, project_id, source_id, false, lock_timeout)
}

fn change_jsonl_membership(
    database: &mut IndexDb,
    project_id: ProjectId,
    source_id: SourceId,
    attach: bool,
    lock_timeout: Duration,
) -> Result<SourceMembershipOutcome, StoreError> {
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    Doctor::run(database.path())?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let project_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    if !project_exists {
        return Err(StoreError::ProjectNotFound);
    }
    let source_name = transaction
        .query_row(
            "SELECT name FROM sources
             WHERE id = ?1 AND kind = 'jsonl' AND removed_at IS NULL",
            [source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::SourceNotFound)?;
    let source_name =
        SafeSlug::new(source_name).map_err(|_| StoreError::InvalidMetadata("source name"))?;
    let prior = transaction
        .query_row(
            "SELECT removed_at FROM project_sources
             WHERE project_id = ?1 AND source_id = ?2",
            params![
                project_id.as_uuid().as_bytes().as_slice(),
                source_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    let changed = if attach {
        let changed = prior.is_none_or(|removed_at| removed_at.is_some());
        if changed {
            transaction.execute(
                "INSERT INTO project_sources(project_id, source_id, removed_at)
                 VALUES (?1, ?2, NULL)
                 ON CONFLICT(project_id, source_id) DO UPDATE SET removed_at = NULL",
                params![
                    project_id.as_uuid().as_bytes().as_slice(),
                    source_id.as_uuid().as_bytes().as_slice(),
                ],
            )?;
        }
        changed
    } else {
        match prior.as_ref() {
            Some(None) => {
                let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
                transaction.execute(
                    "UPDATE project_sources SET removed_at = ?1
                     WHERE project_id = ?2 AND source_id = ?3",
                    params![
                        now,
                        project_id.as_uuid().as_bytes().as_slice(),
                        source_id.as_uuid().as_bytes().as_slice(),
                    ],
                )?;
                true
            }
            None => return Err(StoreError::SourceNotFound),
            Some(Some(_)) => false,
        }
    };
    if changed {
        transaction.execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [project_id.as_uuid().as_bytes().as_slice()],
        )?;
    }
    let scope_revision = transaction.query_row(
        "SELECT scope_revision FROM projects WHERE id = ?1",
        [project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get::<_, i64>(0),
    )?;
    let scope_revision = u64::try_from(scope_revision).map_err(|_| StoreError::IntegerOverflow)?;
    transaction.commit()?;
    drop(writer_lock);
    Ok(SourceMembershipOutcome {
        source_id,
        source_name,
        attached: attach,
        changed,
        scope_revision,
    })
}

pub(crate) fn resolve_attached_jsonl_source(
    connection: &rusqlite::Connection,
    project_id: ProjectId,
    source_id: SourceId,
) -> Result<(SafeSlug, String, String), StoreError> {
    let row = connection
        .query_row(
            "SELECT s.kind, s.name, s.logical_uri, s.config_json
             FROM sources AS s
             JOIN project_sources AS ps ON ps.source_id = s.id
             WHERE ps.project_id = ?1 AND s.id = ?2
               AND ps.removed_at IS NULL
               AND s.removed_at IS NULL",
            params![
                project_id.as_uuid().as_bytes().as_slice(),
                source_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::SourceNotFound)?;
    if row.0 != "jsonl" {
        return Err(StoreError::UnsupportedSourceKind);
    }
    validate_source_authority(&row.2, &row.3)?;
    let name = SafeSlug::new(row.1).map_err(|_| StoreError::InvalidMetadata("source name"))?;
    Ok((name, row.2, row.3))
}

fn validate_source_authority(logical_uri: &str, config_json: &str) -> Result<(), StoreError> {
    if logical_uri.is_empty()
        || logical_uri.len() > MAX_SOURCE_LOGICAL_URI_BYTES
        || logical_uri.chars().any(char::is_control)
    {
        return Err(StoreError::InvalidPreparedDocument(
            "source logical URI is invalid",
        ));
    }
    if config_json.len() > MAX_SOURCE_CONFIG_BYTES
        || !serde_json::from_str::<serde_json::Value>(config_json)
            .is_ok_and(|value| value.is_object())
    {
        return Err(StoreError::InvalidPreparedDocument(
            "source config is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn validate_listable_sources(
    connection: &rusqlite::Connection,
) -> Result<(), StoreError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sources
             WHERE removed_at IS NULL AND (
                 typeof(id) != 'blob' OR length(id) != 16
                 OR typeof(kind) != 'text' OR length(CAST(kind AS BLOB)) > 16
                 OR typeof(name) != 'text'
                 OR length(CAST(name AS BLOB)) NOT BETWEEN 1 AND ?1
                 OR typeof(logical_uri) != 'text'
                 OR length(CAST(logical_uri AS BLOB)) NOT BETWEEN 1 AND ?2
                 OR typeof(config_json) != 'text'
                 OR length(CAST(config_json AS BLOB)) > ?3
                 OR length(CAST(COALESCE(last_success_at, '') AS BLOB)) > ?4
                 OR length(CAST(COALESCE(last_error_code, '') AS BLOB)) > ?5
                 OR length(CAST(COALESCE(last_error_detail, '') AS BLOB)) > ?6
                 OR length(CAST(COALESCE(last_error_at, '') AS BLOB)) > ?4
             )
             LIMIT 1
         )",
        params![
            MAX_SOURCE_NAME_BYTES,
            i64::try_from(MAX_SOURCE_LOGICAL_URI_BYTES).expect("URI limit fits i64"),
            i64::try_from(MAX_SOURCE_CONFIG_BYTES).expect("config limit fits i64"),
            MAX_TIMESTAMP_BYTES,
            MAX_ERROR_CODE_BYTES,
            MAX_ERROR_DETAIL_BYTES,
        ],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(StoreError::InvalidMetadata("bounded source fields"));
    }
    enforce_active_source_capacity(connection, 0)
}

fn enforce_active_source_capacity(
    connection: &rusqlite::Connection,
    additional: usize,
) -> Result<(), StoreError> {
    let active = usize::try_from(connection.query_row(
        "SELECT COUNT(*) FROM sources WHERE removed_at IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .map_err(|_| StoreError::IntegerOverflow)?;
    if active.saturating_add(additional) > MAX_ACTIVE_SOURCES {
        return Err(StoreError::SourceLimitExceeded {
            maximum: MAX_ACTIVE_SOURCES,
        });
    }
    Ok(())
}

fn source_id(bytes: &[u8]) -> Result<SourceId, StoreError> {
    uuid::Uuid::from_slice(bytes)
        .map(SourceId::from_uuid)
        .map_err(|_| StoreError::InvalidMetadata("source UUID"))
}
