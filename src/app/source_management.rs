use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::config::{TrustError, canonicalize_repository_root};
use crate::domain::{ProjectId, SafeSlug, SourceId};
use crate::ingest::DiscoveryOptions;
use crate::store::{
    ConfiguredSource, ConfiguredSourceKind, FilesystemSourceRegistration, IndexDb, JsonlScope,
    OpenMode, SourceMembershipOutcome, SourceRegistration, SourceRemovalOutcome, StoragePreflight,
    StoragePreflightError, StoreError, attach_jsonl_source_with_timeout,
    configure_filesystem_source_with_timeout, configure_jsonl_source_with_timeout,
    detach_jsonl_source_with_timeout, list_index_sources, list_project_sources,
};

use super::{
    FilesystemSourceConfig, InitError, JsonlSourceConfig, JsonlSourceConfigError, SourceConfigError,
};

#[derive(Clone, Debug)]
pub struct AddFilesystemSourceRequest {
    pub database_path: PathBuf,
    pub project_id: ProjectId,
    pub path: PathBuf,
    pub source_name: SafeSlug,
    pub current_dir: PathBuf,
    pub environment_home: Option<PathBuf>,
    pub allow_broad_root: bool,
    pub index_quota_bytes: Option<u64>,
    pub lock_timeout: Duration,
}

pub fn add_filesystem_source(
    request: &AddFilesystemSourceRequest,
) -> Result<(FilesystemSourceRegistration, FilesystemSourceConfig), SourceManagementError> {
    let canonical_current = canonicalize_repository_root(&request.current_dir)?;
    let canonical_root = canonicalize_repository_root(&request.path)?;
    super::init::enforce_safe_root(
        &canonical_root,
        &canonical_current,
        request.environment_home.as_deref(),
        request.allow_broad_root,
    )?;
    let logical_uri = canonical_root
        .to_str()
        .ok_or(SourceManagementError::NonUtf8FilesystemRoot)?
        .to_owned();
    let config = FilesystemSourceConfig::new(canonical_root, DiscoveryOptions::default())?
        .with_index_quota_bytes(request.index_quota_bytes)?;
    let config_json = config.to_canonical_json()?;
    StoragePreflight::run(&request.database_path, 64 * 1024, request.index_quota_bytes)?;
    let mut database = IndexDb::open_existing(&request.database_path, OpenMode::ReadWrite)?;
    let registration = configure_filesystem_source_with_timeout(
        &mut database,
        SourceId::new_v4(),
        &request.source_name,
        &logical_uri,
        &config_json,
        request.project_id,
        request.lock_timeout,
    )?;
    Ok((registration, config))
}

#[derive(Clone, Debug)]
pub struct AddJsonlSourceRequest {
    pub database_path: PathBuf,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
    pub path: PathBuf,
    pub source_name: SafeSlug,
    pub index_quota_bytes: Option<u64>,
    pub lock_timeout: Duration,
}

pub fn add_jsonl_source(
    request: &AddJsonlSourceRequest,
) -> Result<(SourceRegistration, JsonlSourceConfig), SourceManagementError> {
    let path = canonical_jsonl_path(&request.path)?;
    let config =
        JsonlSourceConfig::new(path.clone())?.with_index_quota_bytes(request.index_quota_bytes)?;
    let config_json = config.to_canonical_json()?;
    let logical_uri = path.to_str().ok_or(SourceManagementError::NonUtf8Path)?;
    StoragePreflight::run(&request.database_path, 64 * 1024, request.index_quota_bytes)?;
    let mut database = IndexDb::open_existing(&request.database_path, OpenMode::ReadWrite)?;
    let scope = JsonlScope {
        source_id: SourceId::new_v4(),
        source_name: request.source_name.clone(),
        source_logical_uri: logical_uri.to_owned(),
        source_config_json: config_json,
        project_id: request.project_id,
        project_name: request.project_name.clone(),
    };
    let registration =
        configure_jsonl_source_with_timeout(&mut database, &scope, request.lock_timeout)?;
    Ok((registration, config))
}

pub fn list_sources(
    database_path: &Path,
    project_id: ProjectId,
) -> Result<Vec<ConfiguredSource>, SourceManagementError> {
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    list_project_sources(&database, project_id).map_err(Into::into)
}

pub fn list_sources_in_scope(
    database_path: &Path,
    project_id: ProjectId,
    include_unattached: bool,
) -> Result<Vec<ConfiguredSource>, SourceManagementError> {
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    if include_unattached {
        list_index_sources(&database, project_id).map_err(Into::into)
    } else {
        list_project_sources(&database, project_id).map_err(Into::into)
    }
}

pub fn attach_jsonl_source(
    database_path: &Path,
    project_id: ProjectId,
    selector: &str,
    index_quota_bytes: Option<u64>,
    lock_timeout: Duration,
) -> Result<SourceMembershipOutcome, SourceManagementError> {
    let sources = list_sources_in_scope(database_path, project_id, true)?;
    let source_id = resolve_source_selector(&sources, selector)?;
    let source = sources
        .iter()
        .find(|source| source.source_id == source_id)
        .expect("a resolved source selector identifies one listed source");
    if source.kind != ConfiguredSourceKind::Jsonl {
        return Err(SourceManagementError::MembershipRequiresJsonl);
    }
    StoragePreflight::run(database_path, 64 * 1024, index_quota_bytes)?;
    let mut database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    attach_jsonl_source_with_timeout(&mut database, project_id, source_id, lock_timeout)
        .map_err(Into::into)
}

pub fn detach_jsonl_source(
    database_path: &Path,
    project_id: ProjectId,
    selector: &str,
    index_quota_bytes: Option<u64>,
    lock_timeout: Duration,
) -> Result<SourceMembershipOutcome, SourceManagementError> {
    let sources = list_sources_in_scope(database_path, project_id, true)?;
    let source_id = resolve_source_selector(&sources, selector)?;
    let source = sources
        .iter()
        .find(|source| source.source_id == source_id)
        .expect("a resolved source selector identifies one listed source");
    if source.kind != ConfiguredSourceKind::Jsonl {
        return Err(SourceManagementError::MembershipRequiresJsonl);
    }
    StoragePreflight::run(database_path, 64 * 1024, index_quota_bytes)?;
    let mut database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    detach_jsonl_source_with_timeout(&mut database, project_id, source_id, lock_timeout)
        .map_err(Into::into)
}

pub fn resolve_source_selector(
    sources: &[ConfiguredSource],
    selector: &str,
) -> Result<SourceId, SourceManagementError> {
    if let Ok(source_id) = selector.parse::<SourceId>() {
        return sources
            .iter()
            .any(|source| source.source_id == source_id)
            .then_some(source_id)
            .ok_or_else(|| SourceManagementError::SourceNotFound(selector.to_owned()));
    }
    sources
        .iter()
        .find(|source| source.name.as_str() == selector)
        .map(|source| source.source_id)
        .ok_or_else(|| SourceManagementError::SourceNotFound(selector.to_owned()))
}

pub fn remove_jsonl_source(
    database_path: &Path,
    project_id: ProjectId,
    selector: &str,
    index_quota_bytes: Option<u64>,
    lock_timeout: Duration,
) -> Result<SourceRemovalOutcome, SourceManagementError> {
    let sources = list_sources(database_path, project_id)?;
    let source_id = resolve_source_selector(&sources, selector)?;
    let source = sources
        .iter()
        .find(|source| source.source_id == source_id)
        .expect("a resolved source selector identifies one listed source");
    if source.kind != ConfiguredSourceKind::Jsonl {
        return Err(SourceManagementError::RemovalRequiresJsonl);
    }
    StoragePreflight::run(database_path, 64 * 1024, index_quota_bytes)?;
    let mut database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    database
        .remove_jsonl_source_with_timeout(project_id, source_id, lock_timeout)
        .map_err(Into::into)
}

fn canonical_jsonl_path(path: &Path) -> Result<PathBuf, SourceManagementError> {
    let canonical = fs::canonicalize(path).map_err(SourceManagementError::Canonicalize)?;
    let metadata = fs::symlink_metadata(&canonical).map_err(SourceManagementError::Canonicalize)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(SourceManagementError::NotRegularFile);
    }
    if canonical.to_str().is_none() {
        return Err(SourceManagementError::NonUtf8Path);
    }
    Ok(canonical)
}

#[derive(Debug, Error)]
pub enum SourceManagementError {
    #[error("JSONL source path could not be canonicalized")]
    Canonicalize(#[source] std::io::Error),
    #[error("JSONL source path is not a regular file")]
    NotRegularFile,
    #[error("JSONL source path is not UTF-8")]
    NonUtf8Path,
    #[error("filesystem source root is not UTF-8")]
    NonUtf8FilesystemRoot,
    #[error("JSONL source configuration is invalid")]
    Config(#[from] JsonlSourceConfigError),
    #[error("configured source {0} was not found in the selected project")]
    SourceNotFound(String),
    #[error("this slice removes JSONL sources only; the filesystem authority is retained")]
    RemovalRequiresJsonl,
    #[error("only JSONL sources can be attached or detached explicitly")]
    MembershipRequiresJsonl,
    #[error(transparent)]
    Init(#[from] InitError),
    #[error(transparent)]
    FilesystemConfig(#[from] SourceConfigError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
}
