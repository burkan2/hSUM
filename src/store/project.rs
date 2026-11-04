use std::time::Duration;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::{ProjectId, SafeSlug, SourceId};

use super::{Doctor, IndexDb, StoreError, WriterLock};

const MAX_PROJECTS: usize = 4_096;
const MAX_SOURCE_LOGICAL_URI_BYTES: usize = 4 * 1024;
const MAX_SOURCE_CONFIG_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredProject {
    pub project_id: ProjectId,
    pub name: SafeSlug,
    pub scope_revision: u64,
    pub active_sources: usize,
    pub filesystem_source_id: SourceId,
    pub filesystem_source_name: SafeSlug,
    pub filesystem_root: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectRegistration {
    pub project_id: ProjectId,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemReplacementOutcome {
    pub old_source_id: SourceId,
    pub new_source_id: SourceId,
    pub new_source_name: SafeSlug,
    pub new_root: String,
    pub source_created: bool,
    pub changed: bool,
    pub scope_revision: u64,
}

pub fn create_project_with_timeout(
    database: &mut IndexDb,
    selected_project_id: ProjectId,
    filesystem_source_id: SourceId,
    project_name: &SafeSlug,
    lock_timeout: Duration,
) -> Result<ProjectRegistration, StoreError> {
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    Doctor::run(database.path())?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let authority_is_selected: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM project_sources AS ps
             JOIN sources AS s ON s.id = ps.source_id
             WHERE ps.project_id = ?1 AND s.id = ?2
               AND s.kind = 'filesystem'
               AND ps.removed_at IS NULL
               AND s.removed_at IS NULL
         )",
        params![
            selected_project_id.as_uuid().as_bytes().as_slice(),
            filesystem_source_id.as_uuid().as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;
    if !authority_is_selected {
        return Err(StoreError::ScopeConflict);
    }
    if let Some(id) = transaction
        .query_row(
            "SELECT id FROM projects WHERE name = ?1",
            [project_name.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
    {
        return Ok(ProjectRegistration {
            project_id: project_id(&id)?,
            created: false,
        });
    }
    let project_count =
        usize_from_count(
            transaction.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?,
        )?;
    if project_count >= MAX_PROJECTS {
        return Err(StoreError::ProjectLimitExceeded {
            maximum: MAX_PROJECTS,
        });
    }
    let project_id = ProjectId::new_v4();
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    transaction.execute(
        "INSERT INTO projects(id, name, scope_revision, created_at)
         VALUES (?1, ?2, 0, ?3)",
        params![
            project_id.as_uuid().as_bytes().as_slice(),
            project_name.as_str(),
            now,
        ],
    )?;
    transaction.execute(
        "INSERT INTO project_sources(project_id, source_id, removed_at)
         VALUES (?1, ?2, NULL)",
        params![
            project_id.as_uuid().as_bytes().as_slice(),
            filesystem_source_id.as_uuid().as_bytes().as_slice(),
        ],
    )?;
    transaction.commit()?;
    drop(writer_lock);
    Ok(ProjectRegistration {
        project_id,
        created: true,
    })
}

pub fn list_projects(database: &IndexDb) -> Result<Vec<ConfiguredProject>, StoreError> {
    super::source::validate_listable_sources(database.connection())?;
    let invalid_project: bool = database.connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM projects
             WHERE typeof(id) != 'blob' OR length(id) != 16
                OR typeof(name) != 'text'
                OR length(CAST(name AS BLOB)) NOT BETWEEN 1 AND 64
                OR typeof(scope_revision) != 'integer' OR scope_revision < 0
             LIMIT 1
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid_project {
        return Err(StoreError::InvalidMetadata("bounded project fields"));
    }
    let mut statement = database.connection().prepare(
        "SELECT p.id, p.name, p.scope_revision,
                (
                    SELECT COUNT(*) FROM project_sources AS aps
                    JOIN sources AS active_source ON active_source.id = aps.source_id
                    WHERE aps.project_id = p.id
                      AND aps.removed_at IS NULL
                      AND active_source.removed_at IS NULL
                ),
                (
                    SELECT COUNT(*) FROM project_sources AS fps
                    JOIN sources AS fs ON fs.id = fps.source_id
                    WHERE fps.project_id = p.id AND fs.kind = 'filesystem'
                      AND fps.removed_at IS NULL AND fs.removed_at IS NULL
                ),
                (
                    SELECT fs.id FROM project_sources AS fps
                    JOIN sources AS fs ON fs.id = fps.source_id
                    WHERE fps.project_id = p.id AND fs.kind = 'filesystem'
                      AND fps.removed_at IS NULL AND fs.removed_at IS NULL
                    ORDER BY fs.id LIMIT 1
                ),
                (
                    SELECT fs.name FROM project_sources AS fps
                    JOIN sources AS fs ON fs.id = fps.source_id
                    WHERE fps.project_id = p.id AND fs.kind = 'filesystem'
                      AND fps.removed_at IS NULL AND fs.removed_at IS NULL
                    ORDER BY fs.id LIMIT 1
                ),
                (
                    SELECT fs.logical_uri FROM project_sources AS fps
                    JOIN sources AS fs ON fs.id = fps.source_id
                    WHERE fps.project_id = p.id AND fs.kind = 'filesystem'
                      AND fps.removed_at IS NULL AND fs.removed_at IS NULL
                    ORDER BY fs.id LIMIT 1
                )
         FROM projects AS p
         ORDER BY p.name, p.id
         LIMIT ?1",
    )?;
    let rows = statement
        .query_map(
            [i64::try_from(MAX_PROJECTS + 1).expect("project cap fits i64")],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() > MAX_PROJECTS {
        return Err(StoreError::ProjectLimitExceeded {
            maximum: MAX_PROJECTS,
        });
    }
    rows.into_iter()
        .map(
            |(id, name, scope_revision, active_sources, filesystem_count, fs_id, fs_name, root)| {
                if filesystem_count != 1 {
                    return Err(StoreError::ScopeConflict);
                }
                Ok(ConfiguredProject {
                    project_id: project_id(&id)?,
                    name: SafeSlug::new(name)
                        .map_err(|_| StoreError::InvalidMetadata("project name"))?,
                    scope_revision: u64::try_from(scope_revision)
                        .map_err(|_| StoreError::IntegerOverflow)?,
                    active_sources: usize::try_from(active_sources)
                        .map_err(|_| StoreError::IntegerOverflow)?,
                    filesystem_source_id: source_id(&fs_id.ok_or(StoreError::ScopeConflict)?)?,
                    filesystem_source_name: SafeSlug::new(
                        fs_name.ok_or(StoreError::ScopeConflict)?,
                    )
                    .map_err(|_| StoreError::InvalidMetadata("source name"))?,
                    filesystem_root: root.ok_or(StoreError::ScopeConflict)?,
                })
            },
        )
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn replace_project_filesystem_source_with_timeout(
    database: &mut IndexDb,
    project_id: ProjectId,
    requested_source_id: SourceId,
    requested_source_name: &SafeSlug,
    source_logical_uri: &str,
    source_config_json: &str,
    lock_timeout: Duration,
) -> Result<FilesystemReplacementOutcome, StoreError> {
    validate_source_authority(source_logical_uri, source_config_json)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    Doctor::run(database.path())?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = active_filesystem_sources(&transaction, project_id)?;
    let [(old_source_id, _, _, _)] = current.as_slice() else {
        return Err(StoreError::ScopeConflict);
    };

    let mut statement = transaction.prepare(
        "SELECT id, kind, name, logical_uri, config_json, removed_at
         FROM sources
         WHERE name = ?1 OR logical_uri = ?2
         ORDER BY id
         LIMIT 2",
    )?;
    let matches = statement
        .query_map(
            params![requested_source_name.as_str(), source_logical_uri],
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
    let (new_source_id, new_source_name, source_created) = match matches.as_slice() {
        [] => {
            transaction.execute(
                "INSERT INTO sources(
                    id, kind, name, logical_uri, config_json, created_at
                 ) VALUES (?1, 'filesystem', ?2, ?3, ?4, ?5)",
                params![
                    requested_source_id.as_uuid().as_bytes().as_slice(),
                    requested_source_name.as_str(),
                    source_logical_uri,
                    source_config_json,
                    now,
                ],
            )?;
            (requested_source_id, requested_source_name.clone(), true)
        }
        [(id, kind, name, uri, config, removed_at)]
            if kind == "filesystem"
                && uri == source_logical_uri
                && config == source_config_json =>
        {
            let source_id = source_id(id)?;
            if removed_at.is_some() {
                transaction.execute(
                    "UPDATE sources SET removed_at = NULL WHERE id = ?1",
                    [source_id.as_uuid().as_bytes().as_slice()],
                )?;
            }
            (
                source_id,
                SafeSlug::new(name.clone())
                    .map_err(|_| StoreError::InvalidMetadata("source name"))?,
                false,
            )
        }
        _ => return Err(StoreError::SourceConflict),
    };
    let changed = *old_source_id != new_source_id;
    if changed {
        transaction.execute(
            "UPDATE project_sources SET removed_at = ?1
             WHERE project_id = ?2 AND source_id = ?3 AND removed_at IS NULL",
            params![
                now,
                project_id.as_uuid().as_bytes().as_slice(),
                old_source_id.as_uuid().as_bytes().as_slice(),
            ],
        )?;
        let old_still_attached: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM project_sources
                 WHERE source_id = ?1 AND removed_at IS NULL
             )",
            [old_source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        if !old_still_attached {
            transaction.execute(
                "UPDATE sources SET removed_at = ?1 WHERE id = ?2",
                params![now, old_source_id.as_uuid().as_bytes().as_slice()],
            )?;
        }
        transaction.execute(
            "INSERT INTO project_sources(project_id, source_id, removed_at)
             VALUES (?1, ?2, NULL)
             ON CONFLICT(project_id, source_id) DO UPDATE SET removed_at = NULL",
            params![
                project_id.as_uuid().as_bytes().as_slice(),
                new_source_id.as_uuid().as_bytes().as_slice(),
            ],
        )?;
        transaction.execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [project_id.as_uuid().as_bytes().as_slice()],
        )?;
    }
    super::source::validate_listable_sources(&transaction)?;
    let scope_revision = transaction.query_row(
        "SELECT scope_revision FROM projects WHERE id = ?1",
        [project_id.as_uuid().as_bytes().as_slice()],
        |row| row.get::<_, i64>(0),
    )?;
    let scope_revision = u64::try_from(scope_revision).map_err(|_| StoreError::IntegerOverflow)?;
    transaction.commit()?;
    drop(writer_lock);
    Ok(FilesystemReplacementOutcome {
        old_source_id: *old_source_id,
        new_source_id,
        new_source_name,
        new_root: source_logical_uri.to_owned(),
        source_created,
        changed,
        scope_revision,
    })
}

fn active_filesystem_sources(
    connection: &rusqlite::Connection,
    project_id: ProjectId,
) -> Result<Vec<(SourceId, SafeSlug, String, String)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.id, s.name, s.logical_uri, s.config_json
         FROM project_sources AS ps
         JOIN sources AS s ON s.id = ps.source_id
         WHERE ps.project_id = ?1 AND s.kind = 'filesystem'
           AND ps.removed_at IS NULL AND s.removed_at IS NULL
         ORDER BY s.id LIMIT 2",
    )?;
    let rows = statement
        .query_map([project_id.as_uuid().as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, name, uri, config)| {
            Ok((
                source_id(&id)?,
                SafeSlug::new(name).map_err(|_| StoreError::InvalidMetadata("source name"))?,
                uri,
                config,
            ))
        })
        .collect()
}

fn validate_source_authority(logical_uri: &str, config_json: &str) -> Result<(), StoreError> {
    if logical_uri.is_empty()
        || logical_uri.len() > MAX_SOURCE_LOGICAL_URI_BYTES
        || logical_uri.chars().any(char::is_control)
        || config_json.len() > MAX_SOURCE_CONFIG_BYTES
        || !serde_json::from_str::<serde_json::Value>(config_json)
            .is_ok_and(|value| value.is_object())
    {
        return Err(StoreError::InvalidPreparedDocument(
            "filesystem source authority is invalid",
        ));
    }
    Ok(())
}

fn project_id(bytes: &[u8]) -> Result<ProjectId, StoreError> {
    uuid::Uuid::from_slice(bytes)
        .map(ProjectId::from_uuid)
        .map_err(|_| StoreError::InvalidMetadata("project UUID"))
}

fn source_id(bytes: &[u8]) -> Result<SourceId, StoreError> {
    uuid::Uuid::from_slice(bytes)
        .map(SourceId::from_uuid)
        .map_err(|_| StoreError::InvalidMetadata("source UUID"))
}

fn usize_from_count(value: i64) -> Result<usize, StoreError> {
    usize::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}
