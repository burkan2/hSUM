use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::config::{
    AtomicSaveOutcome, BoundedReadError, CONFIG_SCHEMA_VERSION, LogicalSelection, ManagedPaths,
    TrustError, TrustRegistry, USER_CONFIG_MAX_BYTES, read_bounded_file,
};
use crate::domain::{IndexId, SafeSlug};
use crate::store::{Doctor, IndexDb, OpenMode, ReplacementLock, StoreError, WriterLock};

#[derive(Clone, Debug)]
pub struct DeleteIndexRequest {
    pub managed_paths: ManagedPaths,
    pub config_file: PathBuf,
    pub config_file_explicit: bool,
    pub index_name: SafeSlug,
    pub lock_timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteIndexOutcome {
    pub index_id: IndexId,
    pub index_name: SafeSlug,
    pub removed_bindings: usize,
    pub cleared_configured_default: bool,
    pub resumed_quarantine: bool,
    pub durability_unknown: bool,
}

pub fn delete_index(
    request: &DeleteIndexRequest,
) -> Result<DeleteIndexOutcome, IndexManagementError> {
    let database_path = request.managed_paths.index_database(&request.index_name);
    let target_directory = database_path
        .parent()
        .ok_or_else(|| IndexManagementError::InvalidManagedPath(database_path.clone()))?;
    let indexes_directory = target_directory
        .parent()
        .ok_or_else(|| IndexManagementError::InvalidManagedPath(database_path.clone()))?;
    let quarantine_directory =
        indexes_directory.join(format!(".{}.deleting", request.index_name.as_str()));
    if request.config_file == request.managed_paths.trust_registry_file()
        || request.config_file.starts_with(target_directory)
        || request.config_file.starts_with(&quarantine_directory)
    {
        return Err(IndexManagementError::ConfigPathOverlap);
    }
    let target_exists = require_directory_or_absent(target_directory)?;
    let quarantine_exists = require_directory_or_absent(&quarantine_directory)?;
    let (active_directory, resumed_quarantine) = match (target_exists, quarantine_exists) {
        (true, false) => (target_directory.to_path_buf(), false),
        (false, true) => (quarantine_directory.clone(), true),
        (false, false) => {
            return Err(IndexManagementError::IndexNotFound(
                request.index_name.clone(),
            ));
        }
        (true, true) => return Err(IndexManagementError::DeletionRecoveryConflict),
    };
    let active_database_path = active_directory.join("index.sqlite");
    let writer_lock = WriterLock::acquire(&active_database_path, request.lock_timeout)?;
    let replacement_lock = ReplacementLock::acquire(&active_database_path, request.lock_timeout)?;
    if !writer_lock.protects(&active_database_path) {
        return Err(StoreError::WriterLockMismatch.into());
    }
    if !replacement_lock.protects(&active_database_path) {
        return Err(StoreError::ReplacementLockMismatch.into());
    }
    let database = IndexDb::open_existing(&active_database_path, OpenMode::ReadWrite)?;
    let report = Doctor::run(&active_database_path)?;
    database.verify_live_identity()?;

    ensure_private_config_directory(request.managed_paths.config_dir())?;
    let trust_file = request.managed_paths.trust_registry_file();
    let _config_locks =
        acquire_config_locks(&request.config_file, &trust_file, request.lock_timeout)?;
    let mut user_config = load_user_config(&request.config_file, request.config_file_explicit)?;
    let mut registry = load_registry(&trust_file)?;
    let removed_bindings = registry.remove_index_bindings(&request.index_name, report.index_id)?;
    let cleared_configured_default = match user_config.as_mut() {
        Some(config) => config.clear_index(&request.index_name)?,
        None => false,
    };

    let mut durability_unknown = false;
    if let Some(config) = &user_config
        && cleared_configured_default
    {
        durability_unknown |=
            config.save_atomic(&request.config_file)? == AtomicSaveOutcome::DurabilityUnknown;
    }
    if !removed_bindings.is_empty() {
        durability_unknown |=
            registry.save_atomic(&trust_file)? == AtomicSaveOutcome::DurabilityUnknown;
    }

    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(StoreError::from)?;
    database.verify_live_identity()?;
    drop(database);

    if !resumed_quarantine {
        match fs::rename(target_directory, &quarantine_directory) {
            Ok(()) => {}
            Err(error)
                if !target_directory.exists()
                    && quarantine_directory
                        .symlink_metadata()
                        .is_ok_and(|metadata| metadata.is_dir()) =>
            {
                durability_unknown = true;
                let _ = error;
            }
            Err(error) => return Err(IndexManagementError::Io(error)),
        }
        durability_unknown |= sync_directory(indexes_directory).is_err();
    }
    if let Err(source) = fs::remove_dir_all(&quarantine_directory) {
        return Err(IndexManagementError::QuarantineCleanup {
            path: quarantine_directory,
            source,
        });
    }
    durability_unknown |= sync_directory(indexes_directory).is_err();
    drop(replacement_lock);
    drop(writer_lock);

    Ok(DeleteIndexOutcome {
        index_id: report.index_id,
        index_name: request.index_name.clone(),
        removed_bindings: removed_bindings.len(),
        cleared_configured_default,
        resumed_quarantine,
        durability_unknown,
    })
}

fn acquire_config_locks(
    config_file: &Path,
    trust_file: &Path,
    lock_timeout: Duration,
) -> Result<Vec<WriterLock>, IndexManagementError> {
    let mut paths = [config_file, trust_file];
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path.parent().is_some_and(Path::is_dir))
        .map(|path| WriterLock::acquire(path, lock_timeout).map_err(Into::into))
        .collect()
}

fn require_directory_or_absent(path: &Path) -> Result<bool, IndexManagementError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(IndexManagementError::UnsafeManagedDirectory(
            path.to_path_buf(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ensure_private_config_directory(path: &Path) -> Result<(), IndexManagementError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(IndexManagementError::UnsafeManagedDirectory(
                path.to_path_buf(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            if let Some(parent) = path.parent() {
                let _ = sync_directory(parent);
            }
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(IndexManagementError::UnsafeManagedDirectory(
                path.to_path_buf(),
            ));
        }
    }
    Ok(())
}

fn load_registry(path: &Path) -> Result<TrustRegistry, IndexManagementError> {
    match TrustRegistry::load(path) {
        Ok(registry) => Ok(registry),
        Err(TrustError::Read(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok(TrustRegistry::new())
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Debug)]
struct UserConfigSnapshot {
    wire: UserConfigWire,
    original: Vec<u8>,
}

impl UserConfigSnapshot {
    fn clear_index(&mut self, index_name: &SafeSlug) -> Result<bool, IndexManagementError> {
        let Some(selection) = self.wire.selection()? else {
            return Ok(false);
        };
        if selection.index_name() != index_name {
            return Ok(false);
        }
        self.wire.config_epoch = self
            .wire
            .config_epoch
            .checked_add(1)
            .ok_or(IndexManagementError::ConfigEpochOverflow)?;
        self.wire.default_index = None;
        self.wire.default_project = None;
        Ok(true)
    }

    fn save_atomic(&self, path: &Path) -> Result<AtomicSaveOutcome, IndexManagementError> {
        let contents = toml::to_string_pretty(&self.wire)?;
        if contents.len() > USER_CONFIG_MAX_BYTES {
            return Err(IndexManagementError::ConfigTooLarge);
        }
        let parent = path
            .parent()
            .ok_or_else(|| IndexManagementError::InvalidManagedPath(path.to_path_buf()))?;
        let temporary = parent.join(format!(".config.toml.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            drop(file);
            let live = read_bounded_file(path, USER_CONFIG_MAX_BYTES, 0o077)
                .map_err(IndexManagementError::from_bounded_read)?;
            if live != self.original {
                return Err(IndexManagementError::ConfigChanged);
            }
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::remove_file(&temporary);
                if read_bounded_file(path, USER_CONFIG_MAX_BYTES, 0o077)
                    .is_ok_and(|stored| stored == contents.as_bytes())
                {
                    return Ok(AtomicSaveOutcome::DurabilityUnknown);
                }
                return Err(error.into());
            }
            Ok(if sync_directory(parent).is_ok() {
                AtomicSaveOutcome::Committed
            } else {
                AtomicSaveOutcome::DurabilityUnknown
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn load_user_config(
    path: &Path,
    explicitly_requested: bool,
) -> Result<Option<UserConfigSnapshot>, IndexManagementError> {
    let bytes = match read_bounded_file(path, USER_CONFIG_MAX_BYTES, 0o077) {
        Ok(bytes) => bytes,
        Err(BoundedReadError::NotFound) if !explicitly_requested => return Ok(None),
        Err(error) => return Err(IndexManagementError::from_bounded_read(error)),
    };
    let contents = std::str::from_utf8(&bytes).map_err(|_| IndexManagementError::ConfigNotUtf8)?;
    let version: UserConfigVersion = toml::from_str(contents)?;
    if version.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(IndexManagementError::ConfigSchema {
            found: version.schema_version,
        });
    }
    let wire: UserConfigWire = toml::from_str(contents)?;
    if wire.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(IndexManagementError::ConfigSchema {
            found: wire.schema_version,
        });
    }
    if wire.config_epoch == 0 {
        return Err(IndexManagementError::InvalidConfigEpoch);
    }
    wire.selection()?;
    Ok(Some(UserConfigSnapshot {
        wire,
        original: bytes,
    }))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UserConfigWire {
    schema_version: u32,
    config_epoch: u64,
    default_index: Option<String>,
    default_project: Option<String>,
}

impl UserConfigWire {
    fn selection(&self) -> Result<Option<LogicalSelection>, IndexManagementError> {
        match (&self.default_index, &self.default_project) {
            (None, None) => Ok(None),
            (Some(index), Some(project)) => LogicalSelection::parse(index, project)
                .map(Some)
                .map_err(Into::into),
            _ => Err(IndexManagementError::IncompleteConfiguredDefault),
        }
    }
}

#[derive(Deserialize)]
struct UserConfigVersion {
    schema_version: u32,
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum IndexManagementError {
    #[error("managed index {0} was not found")]
    IndexNotFound(SafeSlug),
    #[error("managed index deletion found both the live path and its recovery quarantine")]
    DeletionRecoveryConflict,
    #[error("managed index directory is not a real directory: {0}")]
    UnsafeManagedDirectory(PathBuf),
    #[error("the user configuration path overlaps the index deletion target")]
    ConfigPathOverlap,
    #[error("managed index path is invalid: {0}")]
    InvalidManagedPath(PathBuf),
    #[error("configured default index and project must both be present")]
    IncompleteConfiguredDefault,
    #[error("user configuration schema {found} requires explicit migration")]
    ConfigSchema { found: u32 },
    #[error("user configuration epoch is invalid")]
    InvalidConfigEpoch,
    #[error("user configuration epoch overflowed")]
    ConfigEpochOverflow,
    #[error("user configuration is unsafe")]
    ConfigUnsafe,
    #[error("user configuration is too large")]
    ConfigTooLarge,
    #[error("user configuration changed during deletion")]
    ConfigChanged,
    #[error("user configuration is not UTF-8")]
    ConfigNotUtf8,
    #[error("user configuration TOML is malformed")]
    ConfigToml(#[from] toml::de::Error),
    #[error("user configuration TOML could not be serialized")]
    ConfigSerialize(#[from] toml::ser::Error),
    #[error("index removal was committed, but quarantine cleanup failed at {path}")]
    QuarantineCleanup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    LogicalSelection(#[from] crate::config::LogicalSelectionError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("index deletion I/O failed")]
    Io(#[from] io::Error),
}

impl IndexManagementError {
    fn from_bounded_read(error: BoundedReadError) -> Self {
        match error {
            BoundedReadError::NotFound => Self::Io(io::Error::from(io::ErrorKind::NotFound)),
            BoundedReadError::Unsafe => Self::ConfigUnsafe,
            BoundedReadError::TooLarge => Self::ConfigTooLarge,
            BoundedReadError::Changed => Self::ConfigChanged,
            BoundedReadError::Io(error) => Self::Io(error),
        }
    }
}
