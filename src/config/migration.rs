use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json_canonicalizer::to_vec as to_canonical_vec;
use thiserror::Error;
use uuid::Uuid;

use super::safe_file::{BoundedReadError, read_bounded_file};
use super::trust::{
    TRUST_PREVIOUS_SCHEMA_VERSION, TRUST_REGISTRY_MAX_BYTES, TRUST_SCHEMA_VERSION, TrustError,
    TrustRegistry,
};
use crate::config::LogicalSelection;
use crate::domain::Sha256Digest;
use crate::store::{
    MaintenanceError, PlanEnvelope, StoragePreflight, StoreError, WriterLock, create_plan_envelope,
    validate_plan_envelope,
};

pub const CONFIG_SCHEMA_VERSION: u32 = 2;
pub const PREVIOUS_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const USER_CONFIG_MAX_BYTES: usize = 64 * 1024;
const PLAN_FORMAT: &str = "hsum.config-migration-plan.v1";
const MANIFEST_FILE: &str = "migration-plan.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigArtifactKind {
    UserConfig,
    TrustRegistry,
}

impl ConfigArtifactKind {
    const fn backup_name(self) -> &'static str {
        match self {
            Self::UserConfig => "config.toml.bak",
            Self::TrustRegistry => "trusted-projects.toml.bak",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigMigrationArtifact {
    pub kind: ConfigArtifactKind,
    pub path: PathBuf,
    pub backup_name: String,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
    pub from_config_epoch: u64,
    pub to_config_epoch: u64,
    pub source_bytes: u64,
    pub target_bytes: u64,
    pub source_sha256: Sha256Digest,
    pub target_sha256: Sha256Digest,
    pub migration_required: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigMigrationPlan {
    pub config_file: PathBuf,
    pub trust_registry_file: PathBuf,
    pub backup_directory: PathBuf,
    pub estimated_backup_bytes: u64,
    pub estimated_peak_bytes: u64,
    pub artifacts: Vec<ConfigMigrationArtifact>,
}

impl ConfigMigrationPlan {
    pub fn migrations_required(&self) -> usize {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.migration_required)
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigMigrationOutcome {
    pub plan_hash: Sha256Digest,
    pub backup_directory: PathBuf,
    pub migrated_artifacts: u64,
    pub already_migrated_artifacts: u64,
}

pub fn plan_config_migration(
    config_file: &Path,
    trust_registry_file: &Path,
    backup_directory: &Path,
    lock_timeout: Duration,
) -> Result<PlanEnvelope<ConfigMigrationPlan>, ConfigMigrationError> {
    validate_paths(config_file, trust_registry_file, backup_directory)?;
    let _locks = acquire_locks(config_file, trust_registry_file, lock_timeout)?;
    build_plan(config_file, trust_registry_file, backup_directory)
}

pub fn apply_config_migration(
    config_file: &Path,
    trust_registry_file: &Path,
    plan: &PlanEnvelope<ConfigMigrationPlan>,
    confirmed_hash: Sha256Digest,
    lock_timeout: Duration,
) -> Result<ConfigMigrationOutcome, ConfigMigrationError> {
    validate_plan_envelope(PLAN_FORMAT, plan, confirmed_hash)?;
    validate_paths(
        &plan.plan.config_file,
        &plan.plan.trust_registry_file,
        &plan.plan.backup_directory,
    )?;
    validate_plan_structure(&plan.plan)?;
    if plan.plan.config_file != config_file || plan.plan.trust_registry_file != trust_registry_file
    {
        return Err(ConfigMigrationError::PlanPathMismatch);
    }
    if plan.plan.migrations_required() == 0 {
        return Err(MaintenanceError::NoMaintenanceWork.into());
    }

    let _locks = acquire_locks(config_file, trust_registry_file, lock_timeout)?;
    let live = validate_live_states(&plan.plan)?;
    StoragePreflight::run_staging(
        plan.plan.backup_directory.parent().ok_or_else(|| {
            ConfigMigrationError::PathHasNoParent(plan.plan.backup_directory.clone())
        })?,
        plan.plan.estimated_peak_bytes,
    )?;
    prepare_backup_bundle(plan, &live)?;

    let mut migrated = 0_u64;
    let mut already_migrated = 0_u64;
    for (artifact, state) in plan.plan.artifacts.iter().zip(&live) {
        if !artifact.migration_required {
            continue;
        }
        match state {
            LiveArtifactState::Source(bytes) => {
                let target = target_bytes(artifact.kind, bytes)?;
                if Sha256Digest::of_bytes(&target) != artifact.target_sha256
                    || u64::try_from(target.len()).map_err(|_| StoreError::IntegerOverflow)?
                        != artifact.target_bytes
                {
                    return Err(ConfigMigrationError::PlanStale);
                }
                atomic_replace_private(&artifact.path, bytes, &target)?;
                migrated = migrated.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
            }
            LiveArtifactState::Target => {
                already_migrated = already_migrated
                    .checked_add(1)
                    .ok_or(StoreError::IntegerOverflow)?;
            }
        }
    }

    let final_states = validate_live_states(&plan.plan)?;
    if final_states
        .iter()
        .zip(&plan.plan.artifacts)
        .any(|(state, artifact)| artifact.migration_required && !state.is_target())
    {
        return Err(ConfigMigrationError::PlanStale);
    }
    validate_current_files(&plan.plan)?;
    Ok(ConfigMigrationOutcome {
        plan_hash: plan.plan_hash,
        backup_directory: plan.plan.backup_directory.clone(),
        migrated_artifacts: migrated,
        already_migrated_artifacts: already_migrated,
    })
}

fn build_plan(
    config_file: &Path,
    trust_registry_file: &Path,
    backup_directory: &Path,
) -> Result<PlanEnvelope<ConfigMigrationPlan>, ConfigMigrationError> {
    let mut artifacts = Vec::new();
    if let Some(bytes) = read_optional(config_file, USER_CONFIG_MAX_BYTES)? {
        artifacts.push(inspect_artifact(
            ConfigArtifactKind::UserConfig,
            config_file,
            &bytes,
        )?);
    }
    if let Some(bytes) = read_optional(trust_registry_file, TRUST_REGISTRY_MAX_BYTES)? {
        artifacts.push(inspect_artifact(
            ConfigArtifactKind::TrustRegistry,
            trust_registry_file,
            &bytes,
        )?);
    }
    artifacts.sort_by_key(|artifact| artifact.kind);
    let estimated_backup_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
        total
            .checked_add(artifact.source_bytes)
            .ok_or(StoreError::IntegerOverflow)
    })?;
    let estimated_target_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
        total
            .checked_add(artifact.target_bytes)
            .ok_or(StoreError::IntegerOverflow)
    })?;
    let estimated_peak_bytes = estimated_backup_bytes
        .checked_add(estimated_target_bytes)
        .and_then(|bytes| bytes.checked_add(64 * 1024))
        .ok_or(StoreError::IntegerOverflow)?;
    Ok(create_plan_envelope(
        PLAN_FORMAT,
        ConfigMigrationPlan {
            config_file: config_file.to_path_buf(),
            trust_registry_file: trust_registry_file.to_path_buf(),
            backup_directory: backup_directory.to_path_buf(),
            estimated_backup_bytes,
            estimated_peak_bytes,
            artifacts,
        },
    )?)
}

fn inspect_artifact(
    kind: ConfigArtifactKind,
    path: &Path,
    source: &[u8],
) -> Result<ConfigMigrationArtifact, ConfigMigrationError> {
    let (from_schema_version, from_config_epoch, target) = match kind {
        ConfigArtifactKind::UserConfig => inspect_user_config(source)?,
        ConfigArtifactKind::TrustRegistry => inspect_trust_registry(source)?,
    };
    let to_schema_version = match kind {
        ConfigArtifactKind::UserConfig => CONFIG_SCHEMA_VERSION,
        ConfigArtifactKind::TrustRegistry => TRUST_SCHEMA_VERSION,
    };
    let to_config_epoch = if from_schema_version == to_schema_version {
        from_config_epoch
    } else {
        1
    };
    Ok(ConfigMigrationArtifact {
        kind,
        path: path.to_path_buf(),
        backup_name: kind.backup_name().to_owned(),
        from_schema_version,
        to_schema_version,
        from_config_epoch,
        to_config_epoch,
        source_bytes: u64::try_from(source.len()).map_err(|_| StoreError::IntegerOverflow)?,
        target_bytes: u64::try_from(target.len()).map_err(|_| StoreError::IntegerOverflow)?,
        source_sha256: Sha256Digest::of_bytes(source),
        target_sha256: Sha256Digest::of_bytes(&target),
        migration_required: from_schema_version != to_schema_version,
    })
}

fn inspect_user_config(source: &[u8]) -> Result<(u32, u64, Vec<u8>), ConfigMigrationError> {
    let text = std::str::from_utf8(source)
        .map_err(|_| ConfigMigrationError::NotUtf8(ConfigArtifactKind::UserConfig))?;
    let version: SchemaVersion = toml::from_str(text)?;
    match version.schema_version {
        PREVIOUS_CONFIG_SCHEMA_VERSION => {
            let previous: UserConfigV1 = toml::from_str(text)?;
            if previous.schema_version != PREVIOUS_CONFIG_SCHEMA_VERSION {
                return Err(ConfigMigrationError::UnsupportedSchema {
                    kind: ConfigArtifactKind::UserConfig,
                    found: previous.schema_version,
                    current: CONFIG_SCHEMA_VERSION,
                });
            }
            validate_selection(
                previous.default_index.as_deref(),
                previous.default_project.as_deref(),
            )?;
            let target = toml::to_string_pretty(&UserConfigV2 {
                schema_version: CONFIG_SCHEMA_VERSION,
                config_epoch: 1,
                default_index: previous.default_index,
                default_project: previous.default_project,
            })?;
            Ok((PREVIOUS_CONFIG_SCHEMA_VERSION, 0, target.into_bytes()))
        }
        CONFIG_SCHEMA_VERSION => {
            let current: UserConfigV2 = toml::from_str(text)?;
            if current.config_epoch == 0 {
                return Err(ConfigMigrationError::InvalidConfigEpoch(
                    ConfigArtifactKind::UserConfig,
                ));
            }
            validate_selection(
                current.default_index.as_deref(),
                current.default_project.as_deref(),
            )?;
            Ok((CONFIG_SCHEMA_VERSION, current.config_epoch, source.to_vec()))
        }
        found => Err(ConfigMigrationError::UnsupportedSchema {
            kind: ConfigArtifactKind::UserConfig,
            found,
            current: CONFIG_SCHEMA_VERSION,
        }),
    }
}

fn inspect_trust_registry(source: &[u8]) -> Result<(u32, u64, Vec<u8>), ConfigMigrationError> {
    let text = std::str::from_utf8(source)
        .map_err(|_| ConfigMigrationError::NotUtf8(ConfigArtifactKind::TrustRegistry))?;
    let version: SchemaVersion = toml::from_str(text)?;
    match version.schema_version {
        TRUST_PREVIOUS_SCHEMA_VERSION => {
            let registry = TrustRegistry::parse_previous_for_migration(text)?;
            let target = registry.to_toml()?;
            Ok((TRUST_PREVIOUS_SCHEMA_VERSION, 0, target.into_bytes()))
        }
        TRUST_SCHEMA_VERSION => {
            let registry = TrustRegistry::parse(text)?;
            Ok((
                TRUST_SCHEMA_VERSION,
                registry.config_epoch(),
                source.to_vec(),
            ))
        }
        found => Err(ConfigMigrationError::UnsupportedSchema {
            kind: ConfigArtifactKind::TrustRegistry,
            found,
            current: TRUST_SCHEMA_VERSION,
        }),
    }
}

fn target_bytes(kind: ConfigArtifactKind, source: &[u8]) -> Result<Vec<u8>, ConfigMigrationError> {
    Ok(match kind {
        ConfigArtifactKind::UserConfig => inspect_user_config(source)?.2,
        ConfigArtifactKind::TrustRegistry => inspect_trust_registry(source)?.2,
    })
}

fn validate_selection(
    index: Option<&str>,
    project: Option<&str>,
) -> Result<(), ConfigMigrationError> {
    match (index, project) {
        (None, None) => Ok(()),
        (Some(index), Some(project)) => {
            LogicalSelection::parse(index, project)?;
            Ok(())
        }
        _ => Err(ConfigMigrationError::IncompleteConfiguredDefault),
    }
}

#[derive(Debug)]
enum LiveArtifactState {
    Source(Vec<u8>),
    Target,
}

impl LiveArtifactState {
    const fn is_target(&self) -> bool {
        matches!(self, Self::Target)
    }
}

fn validate_live_states(
    plan: &ConfigMigrationPlan,
) -> Result<Vec<LiveArtifactState>, ConfigMigrationError> {
    for (kind, path) in [
        (ConfigArtifactKind::UserConfig, plan.config_file.as_path()),
        (
            ConfigArtifactKind::TrustRegistry,
            plan.trust_registry_file.as_path(),
        ),
    ] {
        if !plan.artifacts.iter().any(|artifact| artifact.kind == kind)
            && read_optional(path, max_bytes(kind))?.is_some()
        {
            return Err(ConfigMigrationError::PlanStale);
        }
    }

    let mut states = Vec::with_capacity(plan.artifacts.len());
    for artifact in &plan.artifacts {
        let bytes = read_optional(&artifact.path, max_bytes(artifact.kind))?
            .ok_or(ConfigMigrationError::PlanStale)?;
        let digest = Sha256Digest::of_bytes(&bytes);
        let byte_count = u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)?;
        if digest == artifact.source_sha256 && byte_count == artifact.source_bytes {
            states.push(LiveArtifactState::Source(bytes));
        } else if digest == artifact.target_sha256 && byte_count == artifact.target_bytes {
            states.push(LiveArtifactState::Target);
        } else {
            return Err(ConfigMigrationError::PlanStale);
        }
    }
    Ok(states)
}

fn prepare_backup_bundle(
    plan: &PlanEnvelope<ConfigMigrationPlan>,
    live: &[LiveArtifactState],
) -> Result<(), ConfigMigrationError> {
    ensure_backup_directory(&plan.plan.backup_directory)?;
    let allowed = plan
        .plan
        .artifacts
        .iter()
        .map(|artifact| artifact.backup_name.as_str())
        .chain(std::iter::once(MANIFEST_FILE))
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(&plan.plan.backup_directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(ConfigMigrationError::BackupMismatch);
        };
        if !allowed.contains(name) {
            return Err(ConfigMigrationError::BackupMismatch);
        }
    }

    for (artifact, state) in plan.plan.artifacts.iter().zip(live) {
        let backup = plan.plan.backup_directory.join(&artifact.backup_name);
        match read_optional(&backup, max_bytes(artifact.kind))? {
            Some(bytes)
                if Sha256Digest::of_bytes(&bytes) == artifact.source_sha256
                    && u64::try_from(bytes.len())
                        .is_ok_and(|length| length == artifact.source_bytes) => {}
            Some(_) => return Err(ConfigMigrationError::BackupMismatch),
            None => match state {
                LiveArtifactState::Source(bytes) => write_new_private(&backup, bytes)?,
                LiveArtifactState::Target => return Err(ConfigMigrationError::BackupMismatch),
            },
        }
    }

    let manifest_path = plan.plan.backup_directory.join(MANIFEST_FILE);
    let mut manifest = to_canonical_vec(plan)?;
    manifest.push(b'\n');
    match read_optional(&manifest_path, 1024 * 1024)? {
        Some(bytes) if bytes == manifest => {}
        Some(_) => return Err(ConfigMigrationError::BackupMismatch),
        None => write_new_private(&manifest_path, &manifest)?,
    }
    sync_parent(&plan.plan.backup_directory)?;
    Ok(())
}

fn validate_current_files(plan: &ConfigMigrationPlan) -> Result<(), ConfigMigrationError> {
    for artifact in &plan.artifacts {
        let bytes = read_optional(&artifact.path, max_bytes(artifact.kind))?
            .ok_or(ConfigMigrationError::PlanStale)?;
        if Sha256Digest::of_bytes(&bytes) != artifact.target_sha256
            || u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)?
                != artifact.target_bytes
        {
            return Err(ConfigMigrationError::PlanStale);
        }
        match artifact.kind {
            ConfigArtifactKind::UserConfig => {
                let _ = inspect_user_config(&bytes)?;
            }
            ConfigArtifactKind::TrustRegistry => {
                let _ = inspect_trust_registry(&bytes)?;
            }
        }
    }
    Ok(())
}

fn validate_paths(
    config_file: &Path,
    trust_registry_file: &Path,
    backup_directory: &Path,
) -> Result<(), ConfigMigrationError> {
    if !config_file.is_absolute()
        || !trust_registry_file.is_absolute()
        || !backup_directory.is_absolute()
    {
        return Err(ConfigMigrationError::PathsMustBeAbsolute);
    }
    if paths_overlap(config_file, trust_registry_file)
        || paths_overlap(backup_directory, config_file)
        || paths_overlap(backup_directory, trust_registry_file)
    {
        return Err(ConfigMigrationError::PathsOverlap);
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn validate_plan_structure(plan: &ConfigMigrationPlan) -> Result<(), ConfigMigrationError> {
    if plan.artifacts.len() > 2 {
        return Err(ConfigMigrationError::PlanInvalid);
    }
    let mut kinds = BTreeSet::new();
    let mut backup_bytes = 0_u64;
    let mut target_bytes = 0_u64;
    for artifact in &plan.artifacts {
        if !kinds.insert(artifact.kind) {
            return Err(ConfigMigrationError::PlanInvalid);
        }
        let (expected_path, current_schema, previous_schema) = match artifact.kind {
            ConfigArtifactKind::UserConfig => (
                plan.config_file.as_path(),
                CONFIG_SCHEMA_VERSION,
                PREVIOUS_CONFIG_SCHEMA_VERSION,
            ),
            ConfigArtifactKind::TrustRegistry => (
                plan.trust_registry_file.as_path(),
                TRUST_SCHEMA_VERSION,
                TRUST_PREVIOUS_SCHEMA_VERSION,
            ),
        };
        if artifact.path != expected_path
            || artifact.backup_name != artifact.kind.backup_name()
            || artifact.to_schema_version != current_schema
            || artifact.source_bytes > u64::try_from(max_bytes(artifact.kind)).unwrap_or(u64::MAX)
            || artifact.target_bytes > u64::try_from(max_bytes(artifact.kind)).unwrap_or(u64::MAX)
        {
            return Err(ConfigMigrationError::PlanInvalid);
        }
        if artifact.migration_required {
            if artifact.from_schema_version != previous_schema
                || artifact.from_config_epoch != 0
                || artifact.to_config_epoch != 1
            {
                return Err(ConfigMigrationError::PlanInvalid);
            }
        } else if artifact.from_schema_version != current_schema
            || artifact.from_config_epoch == 0
            || artifact.from_config_epoch != artifact.to_config_epoch
            || artifact.source_bytes != artifact.target_bytes
            || artifact.source_sha256 != artifact.target_sha256
        {
            return Err(ConfigMigrationError::PlanInvalid);
        }
        backup_bytes = backup_bytes
            .checked_add(artifact.source_bytes)
            .ok_or(StoreError::IntegerOverflow)?;
        target_bytes = target_bytes
            .checked_add(artifact.target_bytes)
            .ok_or(StoreError::IntegerOverflow)?;
    }
    let peak_bytes = backup_bytes
        .checked_add(target_bytes)
        .and_then(|bytes| bytes.checked_add(64 * 1024))
        .ok_or(StoreError::IntegerOverflow)?;
    if plan.estimated_backup_bytes != backup_bytes || plan.estimated_peak_bytes != peak_bytes {
        return Err(ConfigMigrationError::PlanInvalid);
    }
    Ok(())
}

fn acquire_locks(
    config_file: &Path,
    trust_registry_file: &Path,
    timeout: Duration,
) -> Result<Vec<WriterLock>, ConfigMigrationError> {
    let mut paths = [config_file, trust_registry_file];
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path.parent().is_some_and(Path::is_dir))
        .map(|path| WriterLock::acquire(path, timeout).map_err(Into::into))
        .collect()
}

fn read_optional(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, ConfigMigrationError> {
    match read_bounded_file(path, maximum, 0o077) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(BoundedReadError::NotFound) => Ok(None),
        Err(BoundedReadError::Unsafe) => Err(ConfigMigrationError::UnsafeFile(path.to_path_buf())),
        Err(BoundedReadError::TooLarge) => {
            Err(ConfigMigrationError::FileTooLarge(path.to_path_buf()))
        }
        Err(BoundedReadError::Changed) => {
            Err(ConfigMigrationError::FileChanged(path.to_path_buf()))
        }
        Err(BoundedReadError::Io(error)) => Err(error.into()),
    }
}

const fn max_bytes(kind: ConfigArtifactKind) -> usize {
    match kind {
        ConfigArtifactKind::UserConfig => USER_CONFIG_MAX_BYTES,
        ConfigArtifactKind::TrustRegistry => TRUST_REGISTRY_MAX_BYTES,
    }
}

fn ensure_backup_directory(path: &Path) -> Result<(), ConfigMigrationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| ConfigMigrationError::PathHasNoParent(path.to_path_buf()))?;
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            sync_parent(parent)?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_directory(path, &metadata)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn validate_private_directory(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ConfigMigrationError> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.file_type().is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(ConfigMigrationError::UnsafeBackupDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_directory(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ConfigMigrationError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ConfigMigrationError::UnsafeBackupDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), ConfigMigrationError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    drop(file);
    let parent = path
        .parent()
        .ok_or_else(|| ConfigMigrationError::PathHasNoParent(path.to_path_buf()))?;
    sync_parent(parent)
}

fn atomic_replace_private(
    path: &Path,
    expected_source: &[u8],
    target: &[u8],
) -> Result<(), ConfigMigrationError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigMigrationError::PathHasNoParent(path.to_path_buf()))?;
    let temporary = parent.join(format!(".config-migrate.{}.tmp", Uuid::new_v4()));
    if let Err(error) = write_new_private(&temporary, target) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let live = match read_optional(path, expected_source.len().max(target.len())) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            let _ = fs::remove_file(&temporary);
            return Err(ConfigMigrationError::PlanStale);
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
    };
    if live != expected_source {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigMigrationError::PlanStale);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_parent(parent)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), ConfigMigrationError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), ConfigMigrationError> {
    Ok(())
}

#[derive(Deserialize)]
struct SchemaVersion {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfigV1 {
    schema_version: u32,
    default_index: Option<String>,
    default_project: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UserConfigV2 {
    schema_version: u32,
    config_epoch: u64,
    default_index: Option<String>,
    default_project: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigMigrationError {
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] crate::store::StoragePreflightError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error("configuration migration TOML is malformed")]
    Toml(#[from] toml::de::Error),
    #[error("configuration migration TOML could not be serialized")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("configuration migration JSON could not be serialized")]
    Json(#[from] serde_json::Error),
    #[error("configuration migration filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("configuration migration paths must be absolute")]
    PathsMustBeAbsolute,
    #[error("configuration migration paths overlap")]
    PathsOverlap,
    #[error("configuration migration path has no parent: {0}")]
    PathHasNoParent(PathBuf),
    #[error("configuration migration plan targets different files")]
    PlanPathMismatch,
    #[error("configuration migration plan structure is invalid")]
    PlanInvalid,
    #[error("configuration migration plan is stale")]
    PlanStale,
    #[error("configuration backup does not match the reviewed plan")]
    BackupMismatch,
    #[error("configuration file is unsafe: {0}")]
    UnsafeFile(PathBuf),
    #[error("configuration file is too large: {0}")]
    FileTooLarge(PathBuf),
    #[error("configuration file changed while it was read: {0}")]
    FileChanged(PathBuf),
    #[error("configuration backup directory is unsafe: {0}")]
    UnsafeBackupDirectory(PathBuf),
    #[error("{0:?} is not UTF-8")]
    NotUtf8(ConfigArtifactKind),
    #[error("{kind:?} schema {found} cannot be migrated by schema {current}")]
    UnsupportedSchema {
        kind: ConfigArtifactKind,
        found: u32,
        current: u32,
    },
    #[error("{0:?} config epoch must be at least one")]
    InvalidConfigEpoch(ConfigArtifactKind),
    #[error("configured default index and project must both be present")]
    IncompleteConfiguredDefault,
    #[error(transparent)]
    LogicalSelection(#[from] crate::config::LogicalSelectionError),
}
