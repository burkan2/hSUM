use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

use crate::config::{
    BindingId, BoundedReadError, ExplicitSelection, LogicalSelection, LogicalSelectionError,
    ManagedPaths, RepositoryPointer, SelectedContext, SelectionError, SelectionMode,
    SelectionRequest, SelectionSource, TrustError, TrustRegistry, canonicalize_repository_root,
    read_bounded_file,
};
use crate::domain::{IdParseError, IndexId, ProjectId, SafeSlug, SlugError, SourceId};
use crate::ingest::DiscoveryOptions;
use crate::store::{IndexDb, OpenMode, StoreError};

use super::{
    FilesystemSourceConfig, MAX_FILESYSTEM_SOURCE_CONFIG_BYTES, SourceConfigError, TrustTarget,
};

const CONFIG_SCHEMA_VERSION: u32 = 1;
const USER_CONFIG_MAX_BYTES: usize = 64 * 1024;
const MAX_FILESYSTEM_SOURCE_NAME_BYTES: i64 = 64;
const MAX_FILESYSTEM_SOURCE_ROOT_BYTES: i64 = MAX_FILESYSTEM_SOURCE_CONFIG_BYTES as i64;
const MAX_FILESYSTEM_SOURCE_CONFIG_BYTES_SQL: i64 = MAX_FILESYSTEM_SOURCE_CONFIG_BYTES as i64;

#[derive(Clone, Debug)]
pub struct ContextRequest {
    pub current_dir: PathBuf,
    pub managed_paths: ManagedPaths,
    pub mode: SelectionMode,
    pub binding: Option<BindingId>,
    pub project: Option<SafeSlug>,
    pub config_file: Option<PathBuf>,
}

impl ContextRequest {
    pub fn direct(current_dir: PathBuf, managed_paths: ManagedPaths) -> Self {
        Self {
            current_dir,
            managed_paths,
            mode: SelectionMode::DirectCli,
            binding: None,
            project: None,
            config_file: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EffectiveContext {
    pub index_id: IndexId,
    pub index_name: SafeSlug,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
    pub scope_revision: u64,
    pub source_id: SourceId,
    pub source_name: SafeSlug,
    pub source_root: PathBuf,
    pub source_config_json: String,
    pub source_discovery_options: DiscoveryOptions,
    pub index_quota_bytes: Option<u64>,
    pub binding_id: Option<BindingId>,
    pub canonical_root: Option<PathBuf>,
    pub database_path: PathBuf,
    pub selection_source: SelectionSource,
    pub managed_paths: ManagedPaths,
}

pub fn resolve_context(request: &ContextRequest) -> Result<EffectiveContext, ContextError> {
    if request.binding.is_some() && request.project.is_some() {
        return Err(ContextError::ConflictingMcpScope);
    }

    if let Some(binding_id) = request.binding {
        let registry = load_registry(&request.managed_paths.trust_registry_file())?;
        let selected = registry.select(SelectionRequest {
            mode: request.mode,
            explicit: Some(ExplicitSelection::Binding(binding_id)),
            environment: None,
            canonical_root: None,
            configured_default: None,
            pointer: None,
        })?;
        return materialize_context(request, &selected, request.project.as_ref());
    }

    if request.mode == SelectionMode::DirectCli
        && let Some(environment) = environment_selection()?
    {
        let selected = TrustRegistry::new().select(SelectionRequest {
            mode: request.mode,
            explicit: None,
            environment: Some(environment),
            canonical_root: None,
            configured_default: None,
            pointer: None,
        })?;
        return materialize_context(request, &selected, request.project.as_ref());
    }

    let root = repository_root_for_current_dir(&request.current_dir)?;
    let registry = load_registry(&request.managed_paths.trust_registry_file())?;
    match registry.select(SelectionRequest {
        mode: request.mode,
        explicit: None,
        environment: None,
        canonical_root: Some(root.clone()),
        configured_default: None,
        pointer: None,
    }) {
        Ok(selected) => {
            return materialize_context(request, &selected, request.project.as_ref());
        }
        Err(SelectionError::NotConfigured | SelectionError::TrustRequired) => {}
        Err(error) => return Err(error.into()),
    }

    if request.mode == SelectionMode::DirectCli
        && let Some(configured_default) = load_user_config(
            request
                .config_file
                .as_deref()
                .unwrap_or(&request.managed_paths.config_file()),
            request.config_file.is_some(),
        )?
    {
        let selected = registry.select(SelectionRequest {
            mode: request.mode,
            explicit: None,
            environment: None,
            canonical_root: None,
            configured_default: Some(configured_default),
            pointer: None,
        })?;
        return materialize_context(request, &selected, request.project.as_ref());
    }

    let pointer = RepositoryPointer::load(&root)?;
    let selected = registry.select(SelectionRequest {
        mode: request.mode,
        explicit: None,
        environment: None,
        canonical_root: None,
        configured_default: None,
        pointer,
    })?;
    if request.mode == SelectionMode::Mcp
        && selected.binding_id().is_none()
        && selected.canonical_root().is_none()
    {
        return Err(ContextError::McpTrustRequired);
    }

    materialize_context(request, &selected, request.project.as_ref())
}

pub fn resolve_trust_target(
    root: &Path,
    managed_paths: &ManagedPaths,
    index_name: Option<SafeSlug>,
    project_name: Option<SafeSlug>,
) -> Result<(PathBuf, TrustTarget), ContextError> {
    let canonical_root = canonicalize_repository_root(root)?;
    let (index_name, project_name) = match (index_name, project_name) {
        (Some(index_name), Some(project_name)) => (index_name, project_name),
        (index_name, project_name) => {
            let pointer = RepositoryPointer::load(&canonical_root)?;
            let index_name = index_name
                .or_else(|| pointer.as_ref().map(|value| value.index_name().clone()))
                .ok_or(ContextError::TrustTargetIncomplete)?;
            let project_name = project_name
                .or_else(|| pointer.as_ref().map(|value| value.project_name().clone()))
                .ok_or(ContextError::TrustTargetIncomplete)?;
            (index_name, project_name)
        }
    };
    let database_path = managed_paths.index_database(&index_name);
    let database = IndexDb::open_existing(&database_path, OpenMode::ReadOnly)?;
    let index_id = read_index_id(&database)?;
    let project_id = read_project_id(&database, &project_name)?;
    let source = read_filesystem_source(database.connection(), project_id)?;
    if source.source_root.as_os_str() != canonical_root.as_os_str() {
        return Err(ContextError::TrustedSourceRootMismatch);
    }

    Ok((
        canonical_root,
        TrustTarget {
            index_id,
            index_name,
            project_id,
            project_name,
        },
    ))
}

fn materialize_context(
    request: &ContextRequest,
    selected: &SelectedContext,
    project_override: Option<&SafeSlug>,
) -> Result<EffectiveContext, ContextError> {
    let index_name = selected.index_name().clone();
    let project_name = project_override
        .cloned()
        .unwrap_or_else(|| selected.project_name().clone());
    let database_path = request.managed_paths.index_database(&index_name);
    let database = IndexDb::open_existing(&database_path, OpenMode::ReadOnly)?;
    let index_id = read_index_id(&database)?;
    if selected
        .index_id()
        .is_some_and(|expected| expected != index_id)
    {
        return Err(ContextError::TrustedIndexIdentityMismatch);
    }

    let connection = database.connection();
    let project_row = connection
        .query_row(
            "SELECT id, scope_revision FROM projects WHERE name = ?1",
            [project_name.as_str()],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or(ContextError::ProjectNotFound)?;
    let project_id = project_id(&project_row.0)?;
    if selected
        .project_id()
        .is_some_and(|expected| expected != project_id)
    {
        return Err(ContextError::TrustedProjectIdentityMismatch);
    }
    let scope_revision = u64::try_from(project_row.1)
        .map_err(|_| ContextError::InvalidDatabaseValue("scope revision"))?;

    let source = read_filesystem_source(connection, project_id)?;
    if let Some(trusted_root) = selected.canonical_root()
        && source.source_root.as_os_str() != trusted_root.as_os_str()
    {
        return Err(ContextError::TrustedSourceRootMismatch);
    }
    let canonical_root = selected
        .canonical_root()
        .map(Path::to_path_buf)
        .or_else(|| Some(source.source_root.clone()));

    Ok(EffectiveContext {
        index_id,
        index_name,
        project_id,
        project_name,
        scope_revision,
        source_id: source.source_id,
        source_name: source.source_name,
        source_root: source.source_root,
        source_config_json: source.source_config_json,
        source_discovery_options: source.source_discovery_options,
        index_quota_bytes: source.index_quota_bytes,
        binding_id: selected.binding_id(),
        canonical_root,
        database_path,
        selection_source: selected.source(),
        managed_paths: request.managed_paths.clone(),
    })
}

struct FilesystemSourceAuthority {
    source_id: SourceId,
    source_name: SafeSlug,
    source_root: PathBuf,
    source_config_json: String,
    source_discovery_options: DiscoveryOptions,
    index_quota_bytes: Option<u64>,
}

fn read_filesystem_source(
    connection: &rusqlite::Connection,
    project_id: ProjectId,
) -> Result<FilesystemSourceAuthority, ContextError> {
    let transaction = connection.unchecked_transaction()?;
    let source = read_filesystem_source_snapshot(&transaction, project_id)?;
    transaction.rollback()?;
    Ok(source)
}

fn read_filesystem_source_snapshot(
    connection: &rusqlite::Connection,
    project_id: ProjectId,
) -> Result<FilesystemSourceAuthority, ContextError> {
    let project_id_bytes = *project_id.as_uuid().as_bytes();
    let (has_source, has_second_source): (bool, bool) = connection.query_row(
        "SELECT
             EXISTS(
                 SELECT 1
                 FROM project_sources
                 WHERE project_id = ?1
                 LIMIT 1
             ),
             EXISTS(
                 SELECT 1
                 FROM project_sources
                 WHERE project_id = ?1
                 LIMIT 1 OFFSET 1
             )",
        [project_id_bytes.as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if !has_source || has_second_source {
        return Err(ContextError::AlphaSourceCardinality {
            found: usize::from(has_source) + usize::from(has_second_source),
        });
    }

    let invalid_kind: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM project_sources AS ps
             LEFT JOIN sources AS s ON s.id = ps.source_id
             WHERE ps.project_id = ?1
               AND (
                   s.id IS NULL
                   OR typeof(s.kind) != 'text'
                   OR s.kind != 'filesystem'
               )
             LIMIT 1
         )",
        [project_id_bytes.as_slice()],
        |row| row.get(0),
    )?;
    if invalid_kind {
        return Err(ContextError::AlphaSourceMustBeFilesystem);
    }

    let invalid_fields: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM project_sources AS ps
             JOIN sources AS s ON s.id = ps.source_id
             WHERE ps.project_id = ?1
               AND (
                   typeof(ps.project_id) != 'blob'
                   OR length(ps.project_id) != 16
                   OR typeof(ps.source_id) != 'blob'
                   OR length(ps.source_id) != 16
                   OR typeof(s.id) != 'blob'
                   OR length(s.id) != 16
                   OR typeof(s.name) != 'text'
                   OR length(CAST(s.name AS BLOB)) NOT BETWEEN 1 AND ?2
                   OR typeof(s.logical_uri) != 'text'
                   OR length(CAST(s.logical_uri AS BLOB)) NOT BETWEEN 1 AND ?3
                   OR typeof(s.config_json) != 'text'
                   OR length(CAST(s.config_json AS BLOB)) > ?4
               )
             LIMIT 1
         )",
        params![
            project_id_bytes.as_slice(),
            MAX_FILESYSTEM_SOURCE_NAME_BYTES,
            MAX_FILESYSTEM_SOURCE_ROOT_BYTES,
            MAX_FILESYSTEM_SOURCE_CONFIG_BYTES_SQL,
        ],
        |row| row.get(0),
    )?;
    if invalid_fields {
        return Err(ContextError::InvalidDatabaseValue(
            "bounded filesystem source fields",
        ));
    }

    let (source_id_bytes, source_kind, source_name, source_root, source_config_json) = connection
        .query_row(
        "SELECT s.id, s.kind, s.name, s.logical_uri, s.config_json
         FROM sources AS s
         JOIN project_sources AS ps ON ps.source_id = s.id
         WHERE ps.project_id = ?1
         ORDER BY s.id
         LIMIT 1",
        [project_id_bytes.as_slice()],
        |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    )?;
    if source_kind != "filesystem" {
        return Err(ContextError::AlphaSourceMustBeFilesystem);
    }
    let source_id = source_id(&source_id_bytes)?;
    let source_name = SafeSlug::new(source_name).map_err(ContextError::InvalidSourceName)?;
    let source_root = PathBuf::from(source_root);
    let source_config = FilesystemSourceConfig::parse(&source_config_json)
        .map_err(ContextError::InvalidFilesystemSourceConfig)?;
    if source_config.root().as_os_str() != source_root.as_os_str() {
        return Err(ContextError::SourceConfigurationRootMismatch);
    }
    let source_discovery_options = source_config.discovery_options().clone();
    let index_quota_bytes = source_config.index_quota_bytes();

    Ok(FilesystemSourceAuthority {
        source_id,
        source_name,
        source_root,
        source_config_json,
        source_discovery_options,
        index_quota_bytes,
    })
}

pub fn repository_root_for_current_dir(current_dir: &Path) -> Result<PathBuf, ContextError> {
    let canonical = canonicalize_repository_root(current_dir)?;
    for candidate in canonical.ancestors() {
        let marker = candidate.join(".git");
        match fs::symlink_metadata(marker) {
            Ok(metadata)
                if !metadata.file_type().is_symlink()
                    && (metadata.file_type().is_dir() || metadata.file_type().is_file()) =>
            {
                return Ok(candidate.to_path_buf());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(canonical)
}

fn environment_selection() -> Result<Option<LogicalSelection>, ContextError> {
    let index = env::var_os("HSUM_INDEX");
    let project = env::var_os("HSUM_PROJECT");
    match (index, project) {
        (None, None) => Ok(None),
        (Some(index), Some(project)) => {
            let index = index
                .into_string()
                .map_err(|_| ContextError::NonUtf8Environment)?;
            let project = project
                .into_string()
                .map_err(|_| ContextError::NonUtf8Environment)?;
            LogicalSelection::parse(&index, &project)
                .map(Some)
                .map_err(Into::into)
        }
        _ => Err(ContextError::IncompleteEnvironmentSelection),
    }
}

fn load_registry(path: &Path) -> Result<TrustRegistry, ContextError> {
    match TrustRegistry::load(path) {
        Ok(registry) => Ok(registry),
        Err(TrustError::Read(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(TrustRegistry::new())
        }
        Err(error) => Err(error.into()),
    }
}

fn load_user_config(
    path: &Path,
    explicitly_requested: bool,
) -> Result<Option<LogicalSelection>, ContextError> {
    let bytes = match read_bounded_file(path, USER_CONFIG_MAX_BYTES, 0o077) {
        Ok(bytes) => bytes,
        Err(BoundedReadError::NotFound) if !explicitly_requested => {
            return Ok(None);
        }
        Err(BoundedReadError::NotFound) => {
            return Err(ContextError::ConfigRead(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )));
        }
        Err(BoundedReadError::Unsafe) => return Err(ContextError::ConfigUnsafe),
        Err(BoundedReadError::TooLarge) => return Err(ContextError::ConfigTooLarge),
        Err(BoundedReadError::Changed) => return Err(ContextError::ConfigChangedDuringRead),
        Err(BoundedReadError::Io(error)) => return Err(ContextError::ConfigRead(error)),
    };
    let contents = std::str::from_utf8(&bytes).map_err(|_| ContextError::ConfigNotUtf8)?;
    let config: UserConfig = toml::from_str(contents).map_err(ContextError::ConfigMalformed)?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(ContextError::ConfigSchema {
            found: config.schema_version,
        });
    }
    match (config.default_index, config.default_project) {
        (None, None) => Ok(None),
        (Some(index), Some(project)) => LogicalSelection::parse(&index, &project)
            .map(Some)
            .map_err(Into::into),
        _ => Err(ContextError::IncompleteConfiguredDefault),
    }
}

fn read_index_id(database: &IndexDb) -> Result<IndexId, ContextError> {
    let bytes: Vec<u8> = database.connection().query_row(
        "SELECT value FROM index_meta WHERE key = 'index_uuid'",
        [],
        |row| row.get(0),
    )?;
    Ok(IndexId::from_uuid(uuid(&bytes, "index UUID")?))
}

fn read_project_id(database: &IndexDb, project_name: &SafeSlug) -> Result<ProjectId, ContextError> {
    let bytes = database
        .connection()
        .query_row(
            "SELECT id FROM projects WHERE name = ?1",
            [project_name.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(ContextError::ProjectNotFound)?;
    project_id(&bytes)
}

fn project_id(bytes: &[u8]) -> Result<ProjectId, ContextError> {
    Ok(ProjectId::from_uuid(uuid(bytes, "project UUID")?))
}

fn source_id(bytes: &[u8]) -> Result<SourceId, ContextError> {
    Ok(SourceId::from_uuid(uuid(bytes, "source UUID")?))
}

fn uuid(bytes: &[u8], field: &'static str) -> Result<Uuid, ContextError> {
    Uuid::from_slice(bytes).map_err(|_| ContextError::InvalidDatabaseValue(field))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    schema_version: u32,
    default_index: Option<String>,
    default_project: Option<String>,
}

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("MCP binding and project options conflict")]
    ConflictingMcpScope,
    #[error("MCP context requires a user-side trust binding")]
    McpTrustRequired,
    #[error("HSUM_INDEX and HSUM_PROJECT must be set together")]
    IncompleteEnvironmentSelection,
    #[error("HSUM_INDEX or HSUM_PROJECT is not UTF-8")]
    NonUtf8Environment,
    #[error("configured default index and project must both be present")]
    IncompleteConfiguredDefault,
    #[error("trust target requires index and project names or a pointer hint")]
    TrustTargetIncomplete,
    #[error("trusted index identity does not match the database")]
    TrustedIndexIdentityMismatch,
    #[error("trusted project identity does not match the database")]
    TrustedProjectIdentityMismatch,
    #[error("selected project does not exist")]
    ProjectNotFound,
    #[error("alpha.3 requires exactly one project source, found {found}")]
    AlphaSourceCardinality { found: usize },
    #[error("alpha.3 requires the sole project source to be filesystem-backed")]
    AlphaSourceMustBeFilesystem,
    #[error("filesystem source configuration is invalid")]
    InvalidFilesystemSourceConfig(#[source] SourceConfigError),
    #[error("filesystem source configuration root does not match its logical URI")]
    SourceConfigurationRootMismatch,
    #[error("trusted repository root does not match the source authority recorded in the index")]
    TrustedSourceRootMismatch,
    #[error("index contains invalid {0}")]
    InvalidDatabaseValue(&'static str),
    #[error("source name is invalid")]
    InvalidSourceName(#[source] SlugError),
    #[error("configuration file could not be read")]
    ConfigRead(#[source] std::io::Error),
    #[error("configuration file must be a stable private user-owned regular file")]
    ConfigUnsafe,
    #[error("configuration file exceeds the 64 KiB limit")]
    ConfigTooLarge,
    #[error("configuration file changed while it was read")]
    ConfigChangedDuringRead,
    #[error("configuration file is not UTF-8")]
    ConfigNotUtf8,
    #[error("configuration file is malformed")]
    ConfigMalformed(#[source] toml::de::Error),
    #[error("configuration schema {found} is unsupported")]
    ConfigSchema { found: u32 },
    #[error(transparent)]
    LogicalSelection(#[from] LogicalSelectionError),
    #[error(transparent)]
    Selection(#[from] SelectionError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Pointer(#[from] crate::config::PointerError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("identity could not be parsed")]
    Identity(#[from] IdParseError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_bounds_connection() -> (rusqlite::Connection, ProjectId) {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sources (
                     id,
                     kind,
                     name,
                     logical_uri,
                     config_json
                 );
                 CREATE TABLE project_sources (
                     project_id,
                     source_id
                 );",
            )
            .unwrap();
        let project_id = ProjectId::new_v4();
        let source_id = SourceId::new_v4();
        let root = PathBuf::from("/tmp/hsum-context-source");
        let config_json = FilesystemSourceConfig::new(root.clone(), DiscoveryOptions::default())
            .unwrap()
            .to_canonical_json()
            .unwrap();
        connection
            .execute(
                "INSERT INTO sources (id, kind, name, logical_uri, config_json)
                 VALUES (?1, 'filesystem', 'workspace', ?2, ?3)",
                params![
                    source_id.as_uuid().as_bytes().as_slice(),
                    root.to_str().unwrap(),
                    config_json,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_sources (project_id, source_id) VALUES (?1, ?2)",
                params![
                    project_id.as_uuid().as_bytes().as_slice(),
                    source_id.as_uuid().as_bytes().as_slice(),
                ],
            )
            .unwrap();
        (connection, project_id)
    }

    #[test]
    fn copied_source_storage_classes_are_checked_before_row_decoding() {
        let corruptions = [
            "UPDATE sources SET kind = CAST('filesystem' AS BLOB)",
            "UPDATE sources SET name = 7",
            "UPDATE sources SET logical_uri = zeroblob(1)",
            "UPDATE sources SET config_json = zeroblob(2)",
            "UPDATE project_sources SET source_id = zeroblob(17);
             UPDATE sources SET id = zeroblob(17);",
        ];

        for corruption in corruptions {
            let (connection, project_id) = source_bounds_connection();
            connection.execute_batch(corruption).unwrap();

            assert!(
                read_filesystem_source_snapshot(&connection, project_id).is_err(),
                "corruption must be rejected before row decoding: {corruption}"
            );
        }
    }
}
