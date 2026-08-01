use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::Sha256Digest;

use super::manifest::{
    MODEL_RECEIPT_FILE, ManifestError, ModelFile, ModelManifest, builtin_manifests,
};

const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_ARTIFACT_ENTRIES: usize = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct ModelStore<'a> {
    cache_root: PathBuf,
    manifests: &'a [ModelManifest],
}

impl ModelStore<'static> {
    #[must_use]
    pub fn new(cache_root: PathBuf) -> Self {
        Self::with_manifests(cache_root, builtin_manifests())
    }
}

impl<'a> ModelStore<'a> {
    #[must_use]
    pub fn with_manifests(cache_root: PathBuf, manifests: &'a [ModelManifest]) -> Self {
        Self {
            cache_root,
            manifests,
        }
    }

    #[must_use]
    pub fn manifests(&self) -> &'a [ModelManifest] {
        self.manifests
    }

    pub fn manifest(&self, id: &str) -> Result<&'a ModelManifest, ModelError> {
        self.manifests
            .iter()
            .find(|manifest| manifest.id == id)
            .ok_or_else(|| ModelError::UnknownModel(id.to_owned()))
    }

    pub fn installation_path(&self, manifest: &ModelManifest) -> Result<PathBuf, ModelError> {
        Ok(self
            .cache_root
            .join(&manifest.id)
            .join(manifest.fingerprint()?.to_string()))
    }

    pub fn inventory(&self) -> Vec<ModelInventoryItem> {
        self.manifests
            .iter()
            .map(|manifest| {
                let fingerprint = manifest
                    .fingerprint()
                    .expect("validated embedded manifests have fingerprints");
                let path = self
                    .installation_path(manifest)
                    .expect("validated embedded manifests have safe cache paths");
                let state = match self.inspect(manifest) {
                    Ok(()) => ModelArtifactState::Installed,
                    Err(ModelError::NotInstalled { .. }) => ModelArtifactState::Missing,
                    Err(_) => ModelArtifactState::Invalid,
                };
                ModelInventoryItem {
                    id: manifest.id.clone(),
                    kind: manifest.kind,
                    dimension: manifest.dimension,
                    license_id: manifest.license_id.clone(),
                    upstream_revision: manifest.upstream_revision.clone(),
                    fingerprint,
                    expected_bytes: manifest.files.iter().map(|file| file.bytes).sum(),
                    path,
                    state,
                }
            })
            .collect()
    }

    pub fn inspect(&self, manifest: &ModelManifest) -> Result<(), ModelError> {
        let root = self.installation_path(manifest)?;
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(ModelError::UnsafeArtifact(root)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ModelError::NotInstalled {
                    id: manifest.id.clone(),
                    path: root,
                });
            }
            Err(error) => return Err(ModelError::Io(error)),
        }
        validate_private_directory(&root)?;
        let receipt = read_receipt(&root)?;
        compare_manifests(manifest, &receipt)?;
        validate_exact_tree(&root, manifest, false)
    }

    pub fn verify(&self, id: &str) -> Result<ModelVerification, ModelError> {
        let manifest = self.manifest(id)?;
        let root = self.installation_path(manifest)?;
        self.inspect(manifest)?;
        validate_exact_tree(&root, manifest, true)?;
        Ok(ModelVerification {
            id: manifest.id.clone(),
            fingerprint: manifest.fingerprint()?,
            path: root,
            files: manifest.files.len(),
            bytes: manifest.files.iter().map(|file| file.bytes).sum(),
        })
    }

    pub fn import(&self, artifact: &Path) -> Result<ModelMutation, ModelError> {
        let source_root = resolve_import_root(artifact)?;
        let supplied = read_receipt(&source_root)?;
        supplied.validate()?;
        let manifest = self.manifest(&supplied.id)?;
        compare_manifests(manifest, &supplied)?;
        validate_exact_tree(&source_root, manifest, false)?;

        match self.inspect(manifest) {
            Ok(()) => return Ok(ModelMutation::AlreadyPresent(self.verify(&manifest.id)?)),
            Err(ModelError::NotInstalled { .. }) => {}
            Err(error) => return Err(error),
        }

        let staging = StagingArtifact::create(&self.cache_root, manifest)?;
        for file in &manifest.files {
            let source = source_root.join(&file.path);
            let destination = staging.path().join(&file.path);
            copy_verified_file(&source, &destination, file)?;
        }
        write_receipt(staging.path(), manifest)?;
        let destination = self.installation_path(manifest)?;
        match staging.commit(destination) {
            Ok(()) => Ok(ModelMutation::Installed(self.verify(&manifest.id)?)),
            Err(ModelError::DestinationOccupied(_)) => {
                Ok(ModelMutation::AlreadyPresent(self.verify(&manifest.id)?))
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn create_staging(
        &self,
        manifest: &ModelManifest,
    ) -> Result<StagingArtifact, ModelError> {
        StagingArtifact::create(&self.cache_root, manifest)
    }

    pub(crate) fn finish_staging(
        &self,
        manifest: &ModelManifest,
        staging: StagingArtifact,
    ) -> Result<ModelMutation, ModelError> {
        write_receipt(staging.path(), manifest)?;
        let destination = self.installation_path(manifest)?;
        match staging.commit(destination) {
            Ok(()) => Ok(ModelMutation::Installed(self.verify(&manifest.id)?)),
            Err(ModelError::DestinationOccupied(_)) => {
                Ok(ModelMutation::AlreadyPresent(self.verify(&manifest.id)?))
            }
            Err(error) => Err(error),
        }
    }

    pub fn remove(
        &self,
        id: &str,
        pinned_by: &[String],
        force: bool,
    ) -> Result<ModelRemoval, ModelError> {
        let manifest = self.manifest(id)?;
        let path = self.installation_path(manifest)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(ModelError::UnsafeArtifact(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ModelError::NotInstalled {
                    id: id.to_owned(),
                    path,
                });
            }
            Err(error) => return Err(ModelError::Io(error)),
        }
        if !pinned_by.is_empty() && !force {
            return Err(ModelError::Pinned {
                id: id.to_owned(),
                indexes: pinned_by.to_vec(),
            });
        }
        validate_safe_tree(&path)?;
        fs::remove_dir_all(&path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(ModelRemoval {
            id: id.to_owned(),
            path,
            forced: force && !pinned_by.is_empty(),
            affected_indexes: pinned_by.to_vec(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelArtifactState {
    Missing,
    Installed,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelInventoryItem {
    pub id: String,
    pub kind: super::manifest::ModelKind,
    pub dimension: u32,
    pub license_id: String,
    pub upstream_revision: String,
    pub fingerprint: Sha256Digest,
    pub expected_bytes: u64,
    pub path: PathBuf,
    pub state: ModelArtifactState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelVerification {
    pub id: String,
    pub fingerprint: Sha256Digest,
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelMutation {
    Installed(ModelVerification),
    AlreadyPresent(ModelVerification),
}

impl ModelMutation {
    #[must_use]
    pub fn verification(&self) -> &ModelVerification {
        match self {
            Self::Installed(verification) | Self::AlreadyPresent(verification) => verification,
        }
    }

    #[must_use]
    pub fn changed(&self) -> bool {
        matches!(self, Self::Installed(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRemoval {
    pub id: String,
    pub path: PathBuf,
    pub forced: bool,
    pub affected_indexes: Vec<String>,
}

pub fn discover_model_pins(
    data_dir: &Path,
    manifest: &ModelManifest,
) -> Result<Vec<String>, ModelError> {
    let indexes = data_dir.join("indexes");
    match fs::symlink_metadata(&indexes) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(ModelError::UnsafeArtifact(indexes)),
        Err(error) => return Err(ModelError::Io(error)),
    }

    let fingerprint = manifest.fingerprint()?.to_string();
    let mut pins = Vec::new();
    for entry in fs::read_dir(&indexes)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if !metadata.is_dir() || metadata.is_symlink() {
            continue;
        }
        let Some(index_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let database_path = entry.path().join("index.sqlite");
        match fs::symlink_metadata(&database_path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Ok(_) => return Err(ModelError::PinDiscovery(index_name)),
            Err(_) => return Err(ModelError::PinDiscovery(index_name)),
        }
        let connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| ModelError::PinDiscovery(index_name.clone()))?;
        let profile = connection
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'embedding_profile'",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(|_| ModelError::PinDiscovery(index_name.clone()))?;
        if profile
            .as_deref()
            .is_some_and(|profile| profile_pins_manifest(profile, &manifest.id, &fingerprint))
        {
            pins.push(index_name);
        }
    }
    pins.sort();
    Ok(pins)
}

fn profile_pins_manifest(profile: &[u8], id: &str, fingerprint: &str) -> bool {
    if profile == b"none" || profile.is_empty() {
        return false;
    }
    let Ok(profile) = std::str::from_utf8(profile) else {
        return true;
    };
    if profile == id || profile == fingerprint {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(profile) else {
        return true;
    };
    value
        .get("model_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == id)
        || value
            .get("manifest_sha256")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == fingerprint)
}

pub(crate) struct StagingArtifact {
    path: PathBuf,
    committed: bool,
}

impl StagingArtifact {
    fn create(cache_root: &Path, manifest: &ModelManifest) -> Result<Self, ModelError> {
        ensure_private_directory(cache_root)?;
        let model_root = cache_root.join(&manifest.id);
        ensure_private_directory(&model_root)?;
        let fingerprint = manifest.fingerprint()?;
        let path = model_root.join(format!(".{fingerprint}.{}.partial", Uuid::new_v4()));
        create_private_directory(&path)?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn commit(mut self, destination: PathBuf) -> Result<(), ModelError> {
        sync_tree_directories(&self.path)?;
        match fs::rename(&self.path, &destination) {
            Ok(()) => {
                self.committed = true;
                if let Some(parent) = destination.parent() {
                    sync_directory(parent)?;
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(ModelError::DestinationOccupied(destination))
            }
            Err(error) => Err(ModelError::Io(error)),
        }
    }
}

impl Drop for StagingArtifact {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn create_download_file(path: &Path) -> Result<File, ModelError> {
    let parent = path
        .parent()
        .ok_or_else(|| ModelError::UnsafeArtifact(path.to_path_buf()))?;
    ensure_private_directory(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

pub(crate) fn ensure_download_parent(path: &Path) -> Result<(), ModelError> {
    ensure_private_directory(path)
}

pub(crate) fn finish_download_file(
    mut file: File,
    path: &Path,
    expected: &ModelFile,
    bytes: u64,
    hasher: Sha256,
) -> Result<(), ModelError> {
    file.flush()?;
    file.sync_all()?;
    set_private_file_permissions(path)?;
    let actual = Sha256Digest::from_bytes(hasher.finalize().into());
    if bytes != expected.bytes || actual != expected.sha256 {
        return Err(ModelError::Checksum {
            path: expected.path.clone(),
            expected_bytes: expected.bytes,
            actual_bytes: bytes,
            expected_sha256: expected.sha256,
            actual_sha256: actual,
        });
    }
    Ok(())
}

fn resolve_import_root(artifact: &Path) -> Result<PathBuf, ModelError> {
    let metadata = fs::symlink_metadata(artifact)?;
    if metadata.file_type().is_dir() {
        validate_private_or_readable_directory(artifact)?;
        return Ok(artifact.to_path_buf());
    }
    if metadata.file_type().is_file()
        && artifact
            .file_name()
            .is_some_and(|name| name == MODEL_RECEIPT_FILE)
    {
        let parent = artifact
            .parent()
            .ok_or_else(|| ModelError::UnsafeArtifact(artifact.to_path_buf()))?;
        validate_private_or_readable_directory(parent)?;
        return Ok(parent.to_path_buf());
    }
    Err(ModelError::UnsupportedImport(artifact.to_path_buf()))
}

fn read_receipt(root: &Path) -> Result<ModelManifest, ModelError> {
    let path = root.join(MODEL_RECEIPT_FILE);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ModelError::ReceiptMissing(path.clone())
        } else {
            ModelError::Io(error)
        }
    })?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_RECEIPT_BYTES {
        return Err(ModelError::UnsafeArtifact(path));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| ModelError::ReceiptTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(&path)?
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| ModelError::ReceiptTooLarge)? > MAX_RECEIPT_BYTES {
        return Err(ModelError::ReceiptTooLarge);
    }
    let manifest = serde_json::from_slice(&bytes)?;
    Ok(manifest)
}

fn write_receipt(root: &Path, manifest: &ModelManifest) -> Result<(), ModelError> {
    let mut bytes = serde_json_canonicalizer::to_vec(manifest)
        .map_err(|error| ModelError::Manifest(ManifestError::Serialize(error.to_string())))?;
    bytes.push(b'\n');
    let path = root.join(MODEL_RECEIPT_FILE);
    let mut file = create_download_file(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    set_private_file_permissions(&path)
}

fn compare_manifests(expected: &ModelManifest, actual: &ModelManifest) -> Result<(), ModelError> {
    actual.validate()?;
    if expected.dimension != actual.dimension {
        return Err(ModelError::Dimension {
            expected: expected.dimension,
            actual: actual.dimension,
        });
    }
    if expected != actual {
        return Err(ModelError::ManifestMismatch {
            id: expected.id.clone(),
        });
    }
    Ok(())
}

fn validate_exact_tree(
    root: &Path,
    manifest: &ModelManifest,
    hash_files: bool,
) -> Result<(), ModelError> {
    let expected_files = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain(std::iter::once(MODEL_RECEIPT_FILE.to_owned()))
        .collect::<BTreeSet<_>>();
    let expected_directories = manifest
        .files
        .iter()
        .flat_map(|file| ancestor_paths(&file.path))
        .collect::<BTreeSet<_>>();
    let (actual_files, actual_directories) = enumerate_tree(root)?;
    if actual_files != expected_files || actual_directories != expected_directories {
        return Err(ModelError::FileListMismatch {
            expected: expected_files.into_iter().collect(),
            actual: actual_files.into_iter().collect(),
        });
    }

    for expected in &manifest.files {
        let path = root.join(&expected.path);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
            return Err(ModelError::FileSize {
                path: expected.path.clone(),
                expected: expected.bytes,
                actual: metadata.len(),
            });
        }
        if hash_files {
            let actual = hash_file(&path)?;
            if actual != expected.sha256 {
                return Err(ModelError::Checksum {
                    path: expected.path.clone(),
                    expected_bytes: expected.bytes,
                    actual_bytes: metadata.len(),
                    expected_sha256: expected.sha256,
                    actual_sha256: actual,
                });
            }
        }
    }
    Ok(())
}

fn enumerate_tree(root: &Path) -> Result<(BTreeSet<String>, BTreeSet<String>), ModelError> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries = entries.saturating_add(1);
            if entries > MAX_ARTIFACT_ENTRIES {
                return Err(ModelError::TooManyFiles);
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ModelError::UnsafeArtifact(path.clone()))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| ModelError::UnsafeArtifact(path.clone()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_dir() {
                directories.insert(relative);
                pending.push(path);
            } else if metadata.file_type().is_file() {
                files.insert(relative);
            } else {
                return Err(ModelError::UnsafeArtifact(path));
            }
        }
    }
    Ok((files, directories))
}

fn ancestor_paths(path: &str) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut current = Path::new(path).parent();
    while let Some(parent) = current {
        if parent.as_os_str().is_empty() {
            break;
        }
        ancestors.push(
            parent
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
        );
        current = parent.parent();
    }
    ancestors
}

fn validate_safe_tree(root: &Path) -> Result<(), ModelError> {
    let _ = enumerate_tree(root)?;
    validate_private_directory(root)
}

fn copy_verified_file(
    source: &Path,
    destination: &Path,
    expected: &ModelFile,
) -> Result<(), ModelError> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
        return Err(ModelError::FileSize {
            path: expected.path.clone(),
            expected: expected.bytes,
            actual: metadata.len(),
        });
    }
    if let Some(parent) = destination.parent() {
        ensure_private_directory(parent)?;
    }
    let mut input = File::open(source)?;
    let mut output = create_download_file(destination)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(u64::try_from(read).map_err(|_| ModelError::IntegerOverflow)?)
            .ok_or(ModelError::IntegerOverflow)?;
        if bytes > expected.bytes {
            break;
        }
    }
    finish_download_file(output, destination, expected, bytes, hasher)
}

fn hash_file(path: &Path) -> Result<Sha256Digest, ModelError> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn ensure_private_directory(path: &Path) -> Result<(), ModelError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            set_private_directory_permissions(path)?;
            validate_private_directory(path)
        }
        Ok(_) => Err(ModelError::UnsafeArtifact(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_directory(path),
        Err(error) => Err(ModelError::Io(error)),
    }
}

fn create_private_directory(path: &Path) -> Result<(), ModelError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(ModelError::UnsafeArtifact(parent.to_path_buf())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_private_directory(parent)?;
            }
            Err(error) => return Err(ModelError::Io(error)),
        }
    }
    fs::create_dir(path)?;
    set_private_directory_permissions(path)
}

fn validate_private_or_readable_directory(path: &Path) -> Result<(), ModelError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(ModelError::UnsafeArtifact(path.to_path_buf()));
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), ModelError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(ModelError::UnsafeArtifact(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(ModelError::UnsafeArtifact(path.to_path_buf()));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), ModelError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), ModelError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), ModelError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), ModelError> {
    Ok(())
}

fn sync_tree_directories(root: &Path) -> Result<(), ModelError> {
    let (_, directories) = enumerate_tree(root)?;
    for directory in directories.iter().rev() {
        sync_directory(&root.join(directory))?;
    }
    sync_directory(root)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ModelError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ModelError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("unknown model ID {0}")]
    UnknownModel(String),
    #[error("model {id} is not installed at {path}")]
    NotInstalled { id: String, path: PathBuf },
    #[error("model {id} is pinned by indexes: {indexes:?}")]
    Pinned { id: String, indexes: Vec<String> },
    #[error("could not prove model pin state for index {0}")]
    PinDiscovery(String),
    #[error("unsupported model import path {0}")]
    UnsupportedImport(PathBuf),
    #[error("model receipt is missing at {0}")]
    ReceiptMissing(PathBuf),
    #[error("model receipt exceeds its size limit")]
    ReceiptTooLarge,
    #[error("model manifest for {id} differs from the embedded contract")]
    ManifestMismatch { id: String },
    #[error("model dimension differs: expected {expected}, got {actual}")]
    Dimension { expected: u32, actual: u32 },
    #[error("model artifact file list differs: expected {expected:?}, got {actual:?}")]
    FileListMismatch {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("model file {path} has {actual} bytes; expected {expected}")]
    FileSize {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error(
        "model file {path} failed verification: expected {expected_bytes} bytes/{expected_sha256}, got {actual_bytes} bytes/{actual_sha256}"
    )]
    Checksum {
        path: String,
        expected_bytes: u64,
        actual_bytes: u64,
        expected_sha256: Sha256Digest,
        actual_sha256: Sha256Digest,
    },
    #[error("unsafe model artifact path {0}")]
    UnsafeArtifact(PathBuf),
    #[error("model artifact contains too many filesystem entries")]
    TooManyFiles,
    #[error("verified model destination is already occupied at {0}")]
    DestinationOccupied(PathBuf),
    #[error("integer overflow while processing model artifact")]
    IntegerOverflow,
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("invalid model receipt JSON: {0}")]
    ReceiptJson(#[from] serde_json::Error),
    #[error("model storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("model download is disabled by HSUM_OFFLINE=1; expected cache path: {path}")]
    Offline { path: PathBuf },
    #[error("model download failed after {attempts} attempts: {reason}")]
    NetworkTransient { attempts: u8, reason: String },
    #[error("model download was refused: {reason}")]
    NetworkPermanent { reason: String },
    #[error("could not configure the HTTPS client: {0}")]
    NetworkConfiguration(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_manifest(body: &[u8]) -> ModelManifest {
        ModelManifest {
            schema_version: super::super::manifest::MODEL_MANIFEST_SCHEMA.to_owned(),
            id: "fixture-model".to_owned(),
            kind: super::super::manifest::ModelKind::Embedding,
            upstream_repository: "example/model".to_owned(),
            upstream_revision: "a".repeat(40),
            dimension: 3,
            license_id: "MIT".to_owned(),
            source_url: "https://example.test/model".to_owned(),
            files: vec![ModelFile {
                path: "onnx/model.onnx".to_owned(),
                bytes: body.len() as u64,
                sha256: Sha256Digest::of_bytes(body),
            }],
        }
    }

    fn write_fixture_artifact(root: &Path, manifest: &ModelManifest, body: &[u8]) {
        fs::create_dir_all(root.join("onnx")).unwrap();
        fs::write(root.join("onnx/model.onnx"), body).unwrap();
        fs::write(
            root.join(MODEL_RECEIPT_FILE),
            serde_json_canonicalizer::to_vec(manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn import_verify_idempotency_and_remove_share_one_contract() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("airgap");
        let cache = directory.path().join("cache/models");
        let manifest = fixture_manifest(b"verified model bytes");
        write_fixture_artifact(&source, &manifest, b"verified model bytes");
        let manifests = [manifest.clone()];
        let store = ModelStore::with_manifests(cache, &manifests);

        let first = store.import(&source).unwrap();
        assert!(first.changed());
        assert_eq!(store.verify(&manifest.id).unwrap().files, 1);
        let second = store.import(&source.join(MODEL_RECEIPT_FILE)).unwrap();
        assert!(!second.changed());

        let error = store
            .remove(&manifest.id, &["pinned-index".to_owned()], false)
            .unwrap_err();
        assert!(matches!(error, ModelError::Pinned { .. }));
        let removed = store
            .remove(&manifest.id, &["pinned-index".to_owned()], true)
            .unwrap();
        assert!(removed.forced);
        assert!(matches!(
            store.verify(&manifest.id),
            Err(ModelError::NotInstalled { .. })
        ));
    }

    #[test]
    fn import_rejects_wrong_dimension_checksum_and_extra_files() {
        let directory = TempDir::new().unwrap();
        let cache = directory.path().join("cache/models");
        let manifest = fixture_manifest(b"model");
        let manifests = [manifest.clone()];
        let store = ModelStore::with_manifests(cache, &manifests);

        let wrong_dimension = directory.path().join("wrong-dimension");
        let mut supplied = manifest.clone();
        supplied.dimension = 4;
        write_fixture_artifact(&wrong_dimension, &supplied, b"model");
        assert!(matches!(
            store.import(&wrong_dimension),
            Err(ModelError::Dimension { .. })
        ));

        let wrong_checksum = directory.path().join("wrong-checksum");
        write_fixture_artifact(&wrong_checksum, &manifest, b"other");
        assert!(matches!(
            store.import(&wrong_checksum),
            Err(ModelError::Checksum { .. } | ModelError::FileSize { .. })
        ));

        let extra = directory.path().join("extra");
        write_fixture_artifact(&extra, &manifest, b"model");
        fs::write(extra.join("unexpected"), b"no").unwrap();
        assert!(matches!(
            store.import(&extra),
            Err(ModelError::FileListMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlinked_model_files() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().unwrap();
        let source = directory.path().join("airgap");
        let target = directory.path().join("target");
        let manifest = fixture_manifest(b"model");
        fs::create_dir_all(source.join("onnx")).unwrap();
        fs::write(&target, b"model").unwrap();
        symlink(&target, source.join("onnx/model.onnx")).unwrap();
        fs::write(
            source.join(MODEL_RECEIPT_FILE),
            serde_json_canonicalizer::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let manifests = [manifest];
        let store = ModelStore::with_manifests(directory.path().join("cache"), &manifests);
        assert!(matches!(
            store.import(&source),
            Err(ModelError::UnsafeArtifact(_))
        ));
    }
}
