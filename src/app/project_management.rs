use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::config::{AtomicSaveOutcome, TrustError, TrustRegistry, canonicalize_repository_root};
use crate::domain::{ProjectId, SafeSlug, SourceId};
use crate::store::{
    ConfiguredProject, DEFAULT_WRITER_LOCK_TIMEOUT, FilesystemReplacementOutcome, IndexDb,
    OpenMode, ProjectRegistration, StoragePreflight, StoragePreflightError, StoreError, WriterLock,
    create_project_with_timeout, list_index_sources, list_projects as store_list_projects,
    replace_project_filesystem_source_with_timeout,
};

use super::{EffectiveContext, FilesystemSourceConfig, InitError, SourceConfigError};

#[derive(Clone, Debug)]
pub struct ProjectUseOutcome {
    pub project: ConfiguredProject,
    pub changed: bool,
    pub durability: Option<AtomicSaveOutcome>,
}

#[derive(Clone, Debug)]
pub struct SetProjectRootRequest<'a> {
    pub context: &'a EffectiveContext,
    pub path: PathBuf,
    pub source_name: Option<SafeSlug>,
    pub current_dir: PathBuf,
    pub environment_home: Option<PathBuf>,
    pub allow_broad_root: bool,
    pub lock_timeout: Duration,
}

pub fn create_project(
    context: &EffectiveContext,
    name: &SafeSlug,
    lock_timeout: Duration,
) -> Result<ProjectRegistration, ProjectManagementError> {
    StoragePreflight::run(&context.database_path, 64 * 1024, context.index_quota_bytes)?;
    let mut database = IndexDb::open_existing(&context.database_path, OpenMode::ReadWrite)?;
    create_project_with_timeout(
        &mut database,
        context.project_id,
        context.source_id,
        name,
        lock_timeout,
    )
    .map_err(Into::into)
}

pub fn list_projects(
    database_path: &Path,
) -> Result<Vec<ConfiguredProject>, ProjectManagementError> {
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    store_list_projects(&database).map_err(Into::into)
}

pub fn resolve_project_selector<'a>(
    projects: &'a [ConfiguredProject],
    selector: &str,
) -> Result<&'a ConfiguredProject, ProjectManagementError> {
    if let Ok(project_id) = selector.parse::<ProjectId>() {
        return projects
            .iter()
            .find(|project| project.project_id == project_id)
            .ok_or_else(|| ProjectManagementError::ProjectNotFound(selector.to_owned()));
    }
    projects
        .iter()
        .find(|project| project.name.as_str() == selector)
        .ok_or_else(|| ProjectManagementError::ProjectNotFound(selector.to_owned()))
}

pub fn use_project(
    context: &EffectiveContext,
    selector: &str,
) -> Result<ProjectUseOutcome, ProjectManagementError> {
    let binding_id = context
        .binding_id
        .ok_or(ProjectManagementError::PersistentBindingRequired)?;
    let projects = list_projects(&context.database_path)?;
    let project = resolve_project_selector(&projects, selector)?.clone();
    let trust_path = context.managed_paths.trust_registry_file();
    let _lock = WriterLock::acquire(&trust_path, DEFAULT_WRITER_LOCK_TIMEOUT)?;
    let mut registry = TrustRegistry::load(&trust_path)?;
    let retarget = registry.retarget_binding(
        binding_id,
        PathBuf::from(&project.filesystem_root),
        project.project_id,
        project.name.clone(),
    )?;
    let durability = if retarget.changed {
        Some(registry.save_atomic(&trust_path)?)
    } else {
        None
    };
    Ok(ProjectUseOutcome {
        project,
        changed: retarget.changed,
        durability,
    })
}

pub fn set_project_root(
    request: &SetProjectRootRequest<'_>,
) -> Result<FilesystemReplacementOutcome, ProjectManagementError> {
    let canonical_current = canonicalize_repository_root(&request.current_dir)?;
    let canonical_root = canonicalize_repository_root(&request.path)?;
    super::init::enforce_safe_root(
        &canonical_root,
        &canonical_current,
        request.environment_home.as_deref(),
        request.allow_broad_root,
    )?;
    let root_text = canonical_root
        .to_str()
        .ok_or(ProjectManagementError::NonUtf8Root)?
        .to_owned();
    let source_id = SourceId::new_v4();
    let source_name = match &request.source_name {
        Some(name) => name.clone(),
        None => available_filesystem_name(request.context, source_id)?,
    };
    let config = FilesystemSourceConfig::new(
        canonical_root.clone(),
        request.context.source_discovery_options.clone(),
    )?
    .with_index_quota_bytes(request.context.index_quota_bytes)?;
    let config_json = config.to_canonical_json()?;

    StoragePreflight::run(
        &request.context.database_path,
        64 * 1024,
        request.context.index_quota_bytes,
    )?;
    let mut database = IndexDb::open_existing(&request.context.database_path, OpenMode::ReadWrite)?;
    let outcome = replace_project_filesystem_source_with_timeout(
        &mut database,
        request.context.project_id,
        source_id,
        &source_name,
        &root_text,
        &config_json,
        request.lock_timeout,
    )?;
    let trust_update = if let Some(binding_id) = request.context.binding_id
        && outcome.changed
    {
        let trust_path = request.context.managed_paths.trust_registry_file();
        let result = (|| {
            let _lock = WriterLock::acquire(&trust_path, request.lock_timeout)?;
            let mut registry = TrustRegistry::load(&trust_path)?;
            registry.retarget_binding(
                binding_id,
                canonical_root,
                request.context.project_id,
                request.context.project_name.clone(),
            )?;
            registry.save_atomic(&trust_path)?;
            Ok::<(), ProjectManagementError>(())
        })();
        Some(result)
    } else {
        None
    };
    if let Some(Err(trust_error)) = trust_update {
        let old_root = request
            .context
            .source_root
            .to_str()
            .ok_or(ProjectManagementError::NonUtf8Root)?;
        let rollback = replace_project_filesystem_source_with_timeout(
            &mut database,
            request.context.project_id,
            request.context.source_id,
            &request.context.source_name,
            old_root,
            &request.context.source_config_json,
            request.lock_timeout,
        );
        if let Err(rollback_error) = rollback {
            return Err(ProjectManagementError::TrustRollback {
                trust: Box::new(trust_error),
                rollback: Box::new(rollback_error),
            });
        }
        return Err(trust_error);
    }
    Ok(outcome)
}

fn available_filesystem_name(
    context: &EffectiveContext,
    source_id: SourceId,
) -> Result<SafeSlug, ProjectManagementError> {
    let database = IndexDb::open_existing(&context.database_path, OpenMode::ReadOnly)?;
    let sources = list_index_sources(&database, context.project_id)?;
    let suffix = &source_id.to_string()[..8];
    let suffix = format!("-fs-{suffix}");
    let keep = 64_usize.saturating_sub(suffix.len());
    let mut base = context.project_name.as_str()[..context.project_name.as_str().len().min(keep)]
        .trim_end_matches('-')
        .to_owned();
    base.push_str(&suffix);
    let candidate = SafeSlug::new(base).expect("derived filesystem source name is safe");
    if sources.iter().any(|source| source.name == candidate) {
        return Err(ProjectManagementError::SourceNameCollision);
    }
    Ok(candidate)
}

#[derive(Debug, Error)]
pub enum ProjectManagementError {
    #[error("project {0} was not found in the selected index")]
    ProjectNotFound(String),
    #[error("persistent project selection requires a trusted root binding")]
    PersistentBindingRequired,
    #[error("filesystem source root is not UTF-8")]
    NonUtf8Root,
    #[error("the generated filesystem source name unexpectedly collided")]
    SourceNameCollision,
    #[error("the trust update failed and restoring the prior filesystem scope also failed")]
    TrustRollback {
        trust: Box<ProjectManagementError>,
        rollback: Box<StoreError>,
    },
    #[error(transparent)]
    Init(#[from] InitError),
    #[error(transparent)]
    Config(#[from] SourceConfigError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
}
