use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json_canonicalizer::to_vec as to_canonical_vec;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{IndexId, SafeSlug, Sha256Digest};
use crate::store::{BackupReceipt, StoreError, WriterLock};

const REGISTRY_FORMAT: &str = "hsum.managed-backups.v1";
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REGISTRY_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedBackupKind {
    Manual,
    Prune,
    Migration,
    ForgetRecovery,
    RestoreSafety,
}

impl ManagedBackupKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Prune => "prune",
            Self::Migration => "migration",
            Self::ForgetRecovery => "forget-recovery",
            Self::RestoreSafety => "restore-safety",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedBackupDisposition {
    Keep,
    Purge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedBackupState {
    Verified,
    Missing,
    PendingMissing,
    PendingPresent,
    Changed,
    Unsafe,
}

impl ManagedBackupState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Missing => "missing",
            Self::PendingMissing => "pending-missing",
            Self::PendingPresent => "pending-present",
            Self::Changed => "changed",
            Self::Unsafe => "unsafe",
        }
    }

    #[must_use]
    pub const fn may_contain_evidence(self) -> bool {
        matches!(
            self,
            Self::Verified | Self::PendingPresent | Self::Changed | Self::Unsafe
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedBackupInventoryItem {
    pub index_id: IndexId,
    pub index_name: SafeSlug,
    pub kind: ManagedBackupKind,
    pub path: PathBuf,
    pub state: ManagedBackupState,
    pub file_bytes: Option<u64>,
    pub file_sha256: Option<Sha256Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedBackupDispositionOutcome {
    pub inventory: Vec<ManagedBackupInventoryItem>,
    pub purged: usize,
    pub retained: usize,
    pub missing: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupReservation {
    Created,
    Pending,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    format: String,
    registry_epoch: u64,
    entries: Vec<StoredBackup>,
}

impl Registry {
    fn empty() -> Self {
        Self {
            format: REGISTRY_FORMAT.to_owned(),
            registry_epoch: 0,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBackup {
    index_id: IndexId,
    index_name: String,
    kind: ManagedBackupKind,
    path: StoredPath,
    receipt: Option<StoredReceipt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    schema_version: u32,
    index_epoch: u64,
    file_bytes: u64,
    file_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPath {
    encoding: String,
    data: String,
}

pub struct ManagedBackupCatalog {
    registry_path: PathBuf,
    registry: Registry,
    original_bytes: Option<Vec<u8>>,
    _lock: WriterLock,
}

impl ManagedBackupCatalog {
    pub fn open(registry_path: &Path, lock_timeout: Duration) -> Result<Self, ManagedBackupError> {
        if !registry_path.is_absolute() {
            return Err(ManagedBackupError::RegistryPathInvalid(
                registry_path.to_path_buf(),
            ));
        }
        let parent = registry_path
            .parent()
            .ok_or_else(|| ManagedBackupError::RegistryPathInvalid(registry_path.to_path_buf()))?;
        let physical_parent = parent.canonicalize()?;
        validate_private_directory(&physical_parent)?;
        let file_name = registry_path
            .file_name()
            .ok_or_else(|| ManagedBackupError::RegistryPathInvalid(registry_path.to_path_buf()))?;
        let registry_path = physical_parent.join(file_name);
        let lock = WriterLock::acquire(&registry_path, lock_timeout)?;
        let original_bytes = read_registry_bytes(&registry_path)?;
        let registry = match original_bytes.as_deref() {
            Some(bytes) => serde_json::from_slice(bytes)?,
            None => Registry::empty(),
        };
        validate_registry(&registry)?;
        Ok(Self {
            registry_path,
            registry,
            original_bytes,
            _lock: lock,
        })
    }

    #[must_use]
    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    pub fn normalize_output(&self, output: &Path) -> Result<PathBuf, ManagedBackupError> {
        if !output.is_absolute() {
            return Err(ManagedBackupError::BackupPathInvalid(output.to_path_buf()));
        }
        let parent = output
            .parent()
            .ok_or_else(|| ManagedBackupError::BackupPathInvalid(output.to_path_buf()))?;
        let physical_parent = parent.canonicalize()?;
        let file_name = output
            .file_name()
            .ok_or_else(|| ManagedBackupError::BackupPathInvalid(output.to_path_buf()))?;
        let normalized = physical_parent.join(file_name);
        if normalized == self.registry_path
            || normalized == WriterLock::sidecar_path(&self.registry_path)
        {
            return Err(ManagedBackupError::BackupPathInvalid(normalized));
        }
        Ok(normalized)
    }

    pub fn reserve(
        &mut self,
        index_id: IndexId,
        index_name: &SafeSlug,
        kind: ManagedBackupKind,
        output: &Path,
    ) -> Result<BackupReservation, ManagedBackupError> {
        if let Some(reservation) = self.reservation(index_id, index_name, kind, output)? {
            return Ok(reservation);
        }
        let path = StoredPath::encode(output)?;
        if self.registry.entries.len() >= MAX_REGISTRY_ENTRIES {
            return Err(ManagedBackupError::RegistryTooManyEntries);
        }
        let mut next = self.registry.clone();
        next.entries.push(StoredBackup {
            index_id,
            index_name: index_name.as_str().to_owned(),
            kind,
            path,
            receipt: None,
        });
        sort_entries(&mut next.entries)?;
        self.persist(next)?;
        Ok(BackupReservation::Created)
    }

    pub fn reservation(
        &self,
        index_id: IndexId,
        index_name: &SafeSlug,
        kind: ManagedBackupKind,
        output: &Path,
    ) -> Result<Option<BackupReservation>, ManagedBackupError> {
        let path = StoredPath::encode(output)?;
        let Some(entry) = self
            .registry
            .entries
            .iter()
            .find(|entry| entry.path == path)
        else {
            return Ok(None);
        };
        if entry.index_id != index_id
            || entry.index_name != index_name.as_str()
            || entry.kind != kind
        {
            return Err(ManagedBackupError::BackupPathAlreadyManaged(
                output.to_path_buf(),
            ));
        }
        Ok(Some(if entry.receipt.is_some() {
            BackupReservation::Complete
        } else {
            BackupReservation::Pending
        }))
    }

    pub fn complete(
        &mut self,
        index_id: IndexId,
        output: &Path,
        receipt: &BackupReceipt,
    ) -> Result<(), ManagedBackupError> {
        if receipt.index_id != index_id || receipt.output != output {
            return Err(ManagedBackupError::ReceiptMismatch(output.to_path_buf()));
        }
        let path = StoredPath::encode(output)?;
        let stored_receipt = StoredReceipt {
            schema_version: receipt.schema_version,
            index_epoch: receipt.index_epoch,
            file_bytes: receipt.file_bytes,
            file_sha256: receipt.file_sha256,
        };
        let mut next = self.registry.clone();
        let entry = next
            .entries
            .iter_mut()
            .find(|entry| entry.path == path && entry.index_id == index_id)
            .ok_or_else(|| ManagedBackupError::ReservationMissing(output.to_path_buf()))?;
        if let Some(existing) = &entry.receipt {
            if existing != &stored_receipt {
                return Err(ManagedBackupError::ReceiptMismatch(output.to_path_buf()));
            }
            return Ok(());
        }
        entry.receipt = Some(stored_receipt);
        self.persist(next)
    }

    pub fn cancel_if_missing(
        &mut self,
        index_id: IndexId,
        output: &Path,
    ) -> Result<(), ManagedBackupError> {
        match fs::symlink_metadata(output) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let path = StoredPath::encode(output)?;
        let mut next = self.registry.clone();
        next.entries.retain(|entry| {
            !(entry.index_id == index_id && entry.path == path && entry.receipt.is_none())
        });
        if next == self.registry {
            return Ok(());
        }
        self.persist(next)
    }

    pub fn inventory(
        &self,
        index_id: Option<IndexId>,
    ) -> Result<Vec<ManagedBackupInventoryItem>, ManagedBackupError> {
        self.registry
            .entries
            .iter()
            .filter(|entry| index_id.is_none_or(|expected| entry.index_id == expected))
            .map(classify_entry)
            .collect()
    }

    pub fn preflight_purge(&self, index_id: IndexId) -> Result<(), ManagedBackupError> {
        for item in self.inventory(Some(index_id))? {
            match item.state {
                ManagedBackupState::Verified
                | ManagedBackupState::Missing
                | ManagedBackupState::PendingMissing => {}
                ManagedBackupState::PendingPresent => {
                    return Err(ManagedBackupError::PendingBackup(item.path));
                }
                ManagedBackupState::Changed => {
                    return Err(ManagedBackupError::BackupChanged(item.path));
                }
                ManagedBackupState::Unsafe => {
                    return Err(ManagedBackupError::UnsafeBackup(item.path));
                }
            }
        }
        Ok(())
    }

    pub fn apply_disposition(
        &mut self,
        index_id: IndexId,
        disposition: ManagedBackupDisposition,
    ) -> Result<ManagedBackupDispositionOutcome, ManagedBackupError> {
        let inventory = self.inventory(Some(index_id))?;
        let missing = inventory
            .iter()
            .filter(|item| {
                matches!(
                    item.state,
                    ManagedBackupState::Missing | ManagedBackupState::PendingMissing
                )
            })
            .count();
        if disposition == ManagedBackupDisposition::Keep {
            let retained = inventory
                .iter()
                .filter(|item| item.state.may_contain_evidence())
                .count();
            return Ok(ManagedBackupDispositionOutcome {
                inventory,
                purged: 0,
                retained,
                missing,
            });
        }

        self.preflight_purge(index_id)?;
        let mut purged = 0;
        for item in &inventory {
            if item.state == ManagedBackupState::Verified {
                remove_verified_backup(item)?;
                purged += 1;
            }
        }
        let mut next = self.registry.clone();
        next.entries.retain(|entry| entry.index_id != index_id);
        self.persist(next)?;
        Ok(ManagedBackupDispositionOutcome {
            inventory,
            purged,
            retained: 0,
            missing,
        })
    }

    fn persist(&mut self, mut next: Registry) -> Result<(), ManagedBackupError> {
        next.registry_epoch = self
            .registry
            .registry_epoch
            .checked_add(1)
            .ok_or(ManagedBackupError::RegistryEpochOverflow)?;
        let mut encoded = to_canonical_vec(&next)?;
        encoded.push(b'\n');
        if u64::try_from(encoded.len()).map_err(|_| ManagedBackupError::RegistryTooLarge)?
            > MAX_REGISTRY_BYTES
        {
            return Err(ManagedBackupError::RegistryTooLarge);
        }
        let parent = self
            .registry_path
            .parent()
            .ok_or_else(|| ManagedBackupError::RegistryPathInvalid(self.registry_path.clone()))?;
        let temporary = parent.join(format!(".managed-backups.tmp-{}", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write_result = (|| -> Result<(), ManagedBackupError> {
            let mut file = options.open(&temporary)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            if read_registry_bytes(&self.registry_path)? != self.original_bytes {
                return Err(ManagedBackupError::RegistryChanged);
            }
            fs::rename(&temporary, &self.registry_path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        self.registry = next;
        self.original_bytes = Some(encoded);
        Ok(())
    }
}

fn classify_entry(entry: &StoredBackup) -> Result<ManagedBackupInventoryItem, ManagedBackupError> {
    let path = entry.path.decode()?;
    let state = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if entry.receipt.is_some() {
                ManagedBackupState::Missing
            } else {
                ManagedBackupState::PendingMissing
            }
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if !safe_private_regular_file(&metadata) || backup_sidecars_exist(&path)? => {
            ManagedBackupState::Unsafe
        }
        Ok(metadata) => {
            if let Some(receipt) = &entry.receipt {
                if metadata.len() != receipt.file_bytes || hash_file(&path)? != receipt.file_sha256
                {
                    ManagedBackupState::Changed
                } else {
                    ManagedBackupState::Verified
                }
            } else {
                ManagedBackupState::PendingPresent
            }
        }
    };
    Ok(ManagedBackupInventoryItem {
        index_id: entry.index_id,
        index_name: SafeSlug::new(entry.index_name.clone())
            .map_err(|_| ManagedBackupError::RegistryIndexName)?,
        kind: entry.kind,
        path,
        state,
        file_bytes: entry.receipt.as_ref().map(|receipt| receipt.file_bytes),
        file_sha256: entry.receipt.as_ref().map(|receipt| receipt.file_sha256),
    })
}

fn remove_verified_backup(item: &ManagedBackupInventoryItem) -> Result<(), ManagedBackupError> {
    let metadata = fs::symlink_metadata(&item.path)?;
    if !safe_private_regular_file(&metadata) || backup_sidecars_exist(&item.path)? {
        return Err(ManagedBackupError::UnsafeBackup(item.path.clone()));
    }
    if Some(metadata.len()) != item.file_bytes || Some(hash_file(&item.path)?) != item.file_sha256 {
        return Err(ManagedBackupError::BackupChanged(item.path.clone()));
    }
    fs::remove_file(&item.path)?;
    let parent = item
        .path
        .parent()
        .ok_or_else(|| ManagedBackupError::BackupPathInvalid(item.path.clone()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn backup_sidecars_exist(path: &Path) -> Result<bool, io::Error> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        match fs::symlink_metadata(PathBuf::from(value)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn hash_file(path: &Path) -> Result<Sha256Digest, ManagedBackupError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn read_registry_bytes(path: &Path) -> Result<Option<Vec<u8>>, ManagedBackupError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !safe_private_regular_file(&metadata) {
        return Err(ManagedBackupError::UnsafeRegistry(path.to_path_buf()));
    }
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(ManagedBackupError::RegistryTooLarge);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| ManagedBackupError::RegistryTooLarge)?,
    );
    File::open(path)?
        .take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| ManagedBackupError::RegistryTooLarge)?
        > MAX_REGISTRY_BYTES
    {
        return Err(ManagedBackupError::RegistryTooLarge);
    }
    Ok(Some(bytes))
}

fn validate_registry(registry: &Registry) -> Result<(), ManagedBackupError> {
    if registry.format != REGISTRY_FORMAT {
        return Err(ManagedBackupError::RegistryFormat);
    }
    if registry.entries.len() > MAX_REGISTRY_ENTRIES {
        return Err(ManagedBackupError::RegistryTooManyEntries);
    }
    let mut prior: Option<Vec<u8>> = None;
    for entry in &registry.entries {
        SafeSlug::new(entry.index_name.clone())
            .map_err(|_| ManagedBackupError::RegistryIndexName)?;
        let path = entry.path.decode()?;
        if !path.is_absolute() {
            return Err(ManagedBackupError::BackupPathInvalid(path));
        }
        let key = path_sort_key(&path)?;
        if prior.as_ref().is_some_and(|prior| prior >= &key) {
            return Err(ManagedBackupError::RegistryOrder);
        }
        prior = Some(key);
    }
    Ok(())
}

fn sort_entries(entries: &mut [StoredBackup]) -> Result<(), ManagedBackupError> {
    let mut keyed = entries
        .iter()
        .cloned()
        .map(|entry| Ok((path_sort_key(&entry.path.decode()?)?, entry)))
        .collect::<Result<Vec<_>, ManagedBackupError>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    for (slot, (_, entry)) in entries.iter_mut().zip(keyed) {
        *slot = entry;
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), ManagedBackupError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(ManagedBackupError::UnsafeRegistry(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(ManagedBackupError::UnsafeRegistry(path.to_path_buf()));
        }
    }
    Ok(())
}

fn safe_private_regular_file(metadata: &fs::Metadata) -> bool {
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == rustix::process::getuid().as_raw()
            && metadata.nlink() == 1
            && metadata.mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl StoredPath {
    fn encode(path: &Path) -> Result<Self, ManagedBackupError> {
        Ok(Self {
            encoding: path_encoding().to_owned(),
            data: hex::encode(path_bytes(path)?),
        })
    }

    fn decode(&self) -> Result<PathBuf, ManagedBackupError> {
        if self.encoding != path_encoding() {
            return Err(ManagedBackupError::PathEncoding);
        }
        path_from_bytes(&hex::decode(&self.data).map_err(|_| ManagedBackupError::PathEncoding)?)
    }
}

#[cfg(unix)]
fn path_encoding() -> &'static str {
    "unix-bytes"
}

#[cfg(windows)]
fn path_encoding() -> &'static str {
    "windows-wide-le"
}

#[cfg(not(any(unix, windows)))]
fn path_encoding() -> &'static str {
    "utf8"
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, ManagedBackupError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(windows)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, ManagedBackupError> {
    use std::os::windows::ffi::OsStrExt;
    Ok(path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect())
}

#[cfg(not(any(unix, windows)))]
fn path_bytes(path: &Path) -> Result<Vec<u8>, ManagedBackupError> {
    path.to_str()
        .map(|value| value.as_bytes().to_vec())
        .ok_or(ManagedBackupError::PathEncoding)
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, ManagedBackupError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(windows)]
fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, ManagedBackupError> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        return Err(ManagedBackupError::PathEncoding);
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, ManagedBackupError> {
    String::from_utf8(bytes.to_vec())
        .map(PathBuf::from)
        .map_err(|_| ManagedBackupError::PathEncoding)
}

fn path_sort_key(path: &Path) -> Result<Vec<u8>, ManagedBackupError> {
    path_bytes(path)
}

#[derive(Debug, Error)]
pub enum ManagedBackupError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("managed-backup registry filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("managed-backup registry JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("managed-backup registry path is invalid: {0}")]
    RegistryPathInvalid(PathBuf),
    #[error("managed-backup registry is unsafe: {0}")]
    UnsafeRegistry(PathBuf),
    #[error("managed-backup registry format is unsupported")]
    RegistryFormat,
    #[error("managed-backup registry exceeds the 4 MiB bound")]
    RegistryTooLarge,
    #[error("managed-backup registry exceeds the 4096-entry bound")]
    RegistryTooManyEntries,
    #[error("managed-backup registry entries are duplicated or not canonically ordered")]
    RegistryOrder,
    #[error("managed-backup registry contains an invalid index name")]
    RegistryIndexName,
    #[error("managed-backup registry changed despite its writer lock")]
    RegistryChanged,
    #[error("managed-backup registry epoch overflowed")]
    RegistryEpochOverflow,
    #[error("managed-backup path encoding is invalid for this operating system")]
    PathEncoding,
    #[error("managed-backup path is invalid: {0}")]
    BackupPathInvalid(PathBuf),
    #[error("backup path is already managed by a different operation: {0}")]
    BackupPathAlreadyManaged(PathBuf),
    #[error("managed-backup reservation is missing: {0}")]
    ReservationMissing(PathBuf),
    #[error("managed-backup receipt does not match its reservation: {0}")]
    ReceiptMismatch(PathBuf),
    #[error("managed backup has an incomplete publication record: {0}")]
    PendingBackup(PathBuf),
    #[error("managed backup changed after hSUM verified it: {0}")]
    BackupChanged(PathBuf),
    #[error("managed backup is not a private, single-link, sidecar-free regular file: {0}")]
    UnsafeBackup(PathBuf),
}
