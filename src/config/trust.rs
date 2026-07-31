use std::collections::{HashMap, HashSet};
use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::{Uuid, Version};

use crate::config::safe_file::{BoundedReadError, read_bounded_file};
use crate::domain::{IdParseError, IndexId, ProjectId, SafeSlug, SlugError};

const TRUST_SCHEMA_VERSION: u32 = 1;
const TRUST_REGISTRY_MAX_BYTES: usize = 1024 * 1024;
const TRUST_REGISTRY_MAX_BINDINGS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BindingId(Uuid);

impl BindingId {
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }

    fn is_random_v4(self) -> bool {
        self.0.get_version() == Some(Version::Random)
    }
}

impl fmt::Display for BindingId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for BindingId {
    type Err = BindingIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = Uuid::parse_str(value).map_err(BindingIdParseError::InvalidUuid)?;
        if parsed.hyphenated().to_string() != value {
            return Err(BindingIdParseError::NonCanonical);
        }
        let binding_id = Self(parsed);
        if !binding_id.is_random_v4() {
            return Err(BindingIdParseError::NotRandomV4);
        }
        Ok(binding_id)
    }
}

impl Serialize for BindingId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BindingId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum BindingIdParseError {
    #[error("binding UUID is invalid")]
    InvalidUuid(#[source] uuid::Error),
    #[error("binding UUID must use canonical lowercase hyphenated text")]
    NonCanonical,
    #[error("binding UUID must be a random UUIDv4 value")]
    NotRandomV4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRegistration {
    pub canonical_root: PathBuf,
    pub index_id: IndexId,
    pub index_name: SafeSlug,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustBinding {
    binding_id: BindingId,
    canonical_root: PathBuf,
    index_id: IndexId,
    index_name: SafeSlug,
    project_id: ProjectId,
    project_name: SafeSlug,
}

impl TrustBinding {
    pub fn from_registration(binding_id: BindingId, registration: TrustRegistration) -> Self {
        Self {
            binding_id,
            canonical_root: registration.canonical_root,
            index_id: registration.index_id,
            index_name: registration.index_name,
            project_id: registration.project_id,
            project_name: registration.project_name,
        }
    }

    pub const fn binding_id(&self) -> BindingId {
        self.binding_id
    }

    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub const fn index_id(&self) -> IndexId {
        self.index_id
    }

    pub fn index_name(&self) -> &SafeSlug {
        &self.index_name
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub fn project_name(&self) -> &SafeSlug {
        &self.project_name
    }

    fn matches_registration(&self, registration: &TrustRegistration) -> bool {
        self.canonical_root == registration.canonical_root
            && self.index_id == registration.index_id
            && self.index_name == registration.index_name
            && self.project_id == registration.project_id
            && self.project_name == registration.project_name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRegistry {
    bindings: Vec<TrustBinding>,
}

impl Default for TrustRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TrustRegistry {
    pub const fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn from_bindings(bindings: Vec<TrustBinding>) -> Result<Self, TrustError> {
        let registry = Self { bindings };
        registry.validate()?;
        Ok(registry)
    }

    pub fn bindings(&self) -> &[TrustBinding] {
        &self.bindings
    }

    pub fn parse(contents: &str) -> Result<Self, TrustError> {
        validate_registry_size(contents.as_bytes())?;
        let wire: TrustRegistryWire = toml::from_str(contents).map_err(TrustError::Malformed)?;
        if wire.schema_version != TRUST_SCHEMA_VERSION {
            return Err(TrustError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }
        if wire.bindings.len() > TRUST_REGISTRY_MAX_BINDINGS {
            return Err(TrustError::TooManyBindings {
                found: wire.bindings.len(),
                maximum: TRUST_REGISTRY_MAX_BINDINGS,
            });
        }

        let mut bindings = Vec::with_capacity(wire.bindings.len());
        for binding in wire.bindings {
            bindings.push(TrustBinding {
                binding_id: binding.binding_id,
                canonical_root: PathBuf::from(binding.root),
                index_id: binding.index_id,
                index_name: SafeSlug::new(binding.index_name)
                    .map_err(TrustError::InvalidIndexName)?,
                project_id: binding.project_id,
                project_name: SafeSlug::new(binding.project_name)
                    .map_err(TrustError::InvalidProjectName)?,
            });
        }
        Self::from_bindings(bindings)
    }

    pub fn load(path: &Path) -> Result<Self, TrustError> {
        let bytes = match read_bounded_file(path, TRUST_REGISTRY_MAX_BYTES, 0o077) {
            Ok(bytes) => bytes,
            Err(BoundedReadError::NotFound) => {
                return Err(TrustError::Read(std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )));
            }
            Err(BoundedReadError::Unsafe) => return Err(TrustError::UnsafeFile),
            Err(BoundedReadError::TooLarge) => return Err(TrustError::TooLarge),
            Err(BoundedReadError::Changed) => return Err(TrustError::ChangedDuringRead),
            Err(BoundedReadError::Io(error)) => return Err(TrustError::Read(error)),
        };
        let contents = std::str::from_utf8(&bytes).map_err(|_| TrustError::NotUtf8)?;
        Self::parse(contents)
    }

    pub fn register(
        &mut self,
        registration: TrustRegistration,
    ) -> Result<RegistrationOutcome, TrustError> {
        validate_canonical_root(&registration.canonical_root)?;
        let root_matches = self
            .bindings
            .iter()
            .filter(|binding| binding.canonical_root == registration.canonical_root)
            .collect::<Vec<_>>();

        match root_matches.as_slice() {
            [] => {}
            [binding] if binding.matches_registration(&registration) => {
                return Ok(RegistrationOutcome::Existing((*binding).clone()));
            }
            _ => {
                return Err(TrustError::ConflictingRoot {
                    root: registration.canonical_root,
                });
            }
        }

        if self.bindings.iter().any(|binding| {
            (binding.index_id == registration.index_id
                && binding.index_name != registration.index_name)
                || (binding.index_name == registration.index_name
                    && binding.index_id != registration.index_id)
                || (binding.index_id == registration.index_id
                    && binding.project_id == registration.project_id
                    && binding.project_name != registration.project_name)
                || (binding.index_id == registration.index_id
                    && binding.project_name == registration.project_name
                    && binding.project_id != registration.project_id)
        }) {
            return Err(TrustError::ConflictingIdentity);
        }
        if self.bindings.len() == TRUST_REGISTRY_MAX_BINDINGS {
            return Err(TrustError::TooManyBindings {
                found: self.bindings.len() + 1,
                maximum: TRUST_REGISTRY_MAX_BINDINGS,
            });
        }

        let binding_id = loop {
            let candidate = BindingId::new_v4();
            if self
                .bindings
                .iter()
                .all(|binding| binding.binding_id != candidate)
            {
                break candidate;
            }
        };
        let binding = TrustBinding::from_registration(binding_id, registration);
        self.bindings.push(binding.clone());
        Ok(RegistrationOutcome::Created(binding))
    }

    pub fn to_toml(&self) -> Result<String, TrustError> {
        self.validate()?;
        let mut sorted = self.bindings.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            left.canonical_root
                .cmp(&right.canonical_root)
                .then(left.binding_id.cmp(&right.binding_id))
        });

        let mut bindings = Vec::with_capacity(sorted.len());
        for binding in sorted {
            let root = binding
                .canonical_root
                .to_str()
                .ok_or_else(|| TrustError::NonUtf8Root {
                    root: binding.canonical_root.clone(),
                })?;
            bindings.push(TrustBindingWireRef {
                root,
                binding_id: binding.binding_id,
                index_id: binding.index_id,
                index_name: binding.index_name.as_str(),
                project_id: binding.project_id,
                project_name: binding.project_name.as_str(),
            });
        }

        let contents = toml::to_string_pretty(&TrustRegistryWireRef {
            schema_version: TRUST_SCHEMA_VERSION,
            bindings,
        })
        .map_err(TrustError::Serialize)?;
        validate_registry_size(contents.as_bytes())?;
        Ok(contents)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<AtomicSaveOutcome, TrustError> {
        let contents = self.to_toml()?;
        let parent = path.parent().ok_or_else(|| TrustError::MissingParent {
            path: path.to_path_buf(),
        })?;
        fs::create_dir_all(parent).map_err(TrustError::Write)?;
        set_user_only_directory_permissions(parent)?;

        let temporary_path = parent.join(format!(
            ".trusted-projects.toml.{}.tmp",
            BindingId::new_v4()
        ));
        let write_result = write_user_only_file(&temporary_path, contents.as_bytes());
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }

        if let Err(error) = fs::rename(&temporary_path, path) {
            let _ = fs::remove_file(&temporary_path);
            if read_bounded_file(path, TRUST_REGISTRY_MAX_BYTES, 0o077)
                .is_ok_and(|stored| stored == contents.as_bytes())
            {
                return Ok(AtomicSaveOutcome::DurabilityUnknown);
            }
            return Err(TrustError::Write(error));
        }
        match sync_parent_directory(parent) {
            Ok(()) => Ok(AtomicSaveOutcome::Committed),
            Err(_) => Ok(AtomicSaveOutcome::DurabilityUnknown),
        }
    }

    fn validate(&self) -> Result<(), TrustError> {
        if self.bindings.len() > TRUST_REGISTRY_MAX_BINDINGS {
            return Err(TrustError::TooManyBindings {
                found: self.bindings.len(),
                maximum: TRUST_REGISTRY_MAX_BINDINGS,
            });
        }
        let mut binding_ids = HashSet::with_capacity(self.bindings.len());
        let mut roots = HashSet::with_capacity(self.bindings.len());
        let mut index_names_by_id = HashMap::with_capacity(self.bindings.len());
        let mut index_ids_by_name = HashMap::with_capacity(self.bindings.len());
        let mut project_names_by_id = HashMap::with_capacity(self.bindings.len());
        let mut project_ids_by_name = HashMap::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            validate_stored_root(&binding.canonical_root)?;
            if !roots.insert(binding.canonical_root.clone()) {
                return Err(TrustError::ConflictingRoot {
                    root: binding.canonical_root.clone(),
                });
            }
            if !binding.binding_id.is_random_v4() {
                return Err(TrustError::NonRandomBinding {
                    binding_id: binding.binding_id,
                });
            }
            if !binding_ids.insert(binding.binding_id) {
                return Err(TrustError::DuplicateBinding {
                    binding_id: binding.binding_id,
                });
            }
            if index_names_by_id
                .insert(binding.index_id, binding.index_name.clone())
                .is_some_and(|existing| existing != binding.index_name)
                || index_ids_by_name
                    .insert(binding.index_name.clone(), binding.index_id)
                    .is_some_and(|existing| existing != binding.index_id)
                || project_names_by_id
                    .insert(
                        (binding.index_id, binding.project_id),
                        binding.project_name.clone(),
                    )
                    .is_some_and(|existing| existing != binding.project_name)
                || project_ids_by_name
                    .insert(
                        (binding.index_id, binding.project_name.clone()),
                        binding.project_id,
                    )
                    .is_some_and(|existing| existing != binding.project_id)
            {
                return Err(TrustError::ConflictingIdentity);
            }
        }
        Ok(())
    }
}

fn validate_registry_size(contents: &[u8]) -> Result<(), TrustError> {
    if contents.len() > TRUST_REGISTRY_MAX_BYTES {
        Err(TrustError::TooLarge)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicSaveOutcome {
    Committed,
    DurabilityUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationOutcome {
    Created(TrustBinding),
    Existing(TrustBinding),
}

pub fn canonicalize_repository_root(root: &Path) -> Result<PathBuf, TrustError> {
    let canonical = fs::canonicalize(root).map_err(|source| TrustError::CanonicalizeRoot {
        root: root.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(TrustError::RootNotDirectory { root: canonical });
    }
    Ok(canonical)
}

fn validate_canonical_root(root: &Path) -> Result<(), TrustError> {
    let canonical = canonicalize_repository_root(root)?;
    if canonical != root {
        return Err(TrustError::NonCanonicalRoot {
            stored: root.to_path_buf(),
            canonical,
        });
    }
    Ok(())
}

fn validate_stored_root(root: &Path) -> Result<(), TrustError> {
    let normalized = root.components().collect::<PathBuf>();
    if !root.is_absolute()
        || root.components().any(|component| {
            !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
        || normalized.as_os_str() != root.as_os_str()
    {
        return Err(TrustError::InvalidStoredRoot {
            root: root.to_path_buf(),
        });
    }
    Ok(())
}

fn write_user_only_file(path: &Path, contents: &[u8]) -> Result<(), TrustError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(TrustError::Write)?;
    file.write_all(contents).map_err(TrustError::Write)?;
    file.sync_all().map_err(TrustError::Write)?;
    set_user_only_file_permissions(path)
}

#[cfg(unix)]
fn set_user_only_directory_permissions(path: &Path) -> Result<(), TrustError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(TrustError::Write)
}

// No user-only permission primitive is implemented for this target. The trust
// registry is strict private user configuration: `README.md` states its
// directory is user-only and its file is user-only, and the loader enforces
// that on read. Returning `Ok(())` here would claim the directory had been
// made private while leaving whatever permissions it inherited, so a registry
// written on such a target would be readable by other accounts while hSUM
// reported success. Refuse instead; a target gains support by implementing the
// primitive, never by skipping it.
#[cfg(not(unix))]
fn set_user_only_directory_permissions(path: &Path) -> Result<(), TrustError> {
    Err(TrustError::PrivatePermissionsUnsupported {
        path: path.to_path_buf(),
    })
}

#[cfg(unix)]
fn set_user_only_file_permissions(path: &Path) -> Result<(), TrustError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(TrustError::Write)
}

// Same contract as the directory case above. Note that `write_user_only_file`
// can only apply its `0o600` creation mode under `#[cfg(unix)]`, so on other
// targets the file is created with inherited permissions and this call is the
// only remaining opportunity to make it private. Refusing here is what keeps
// the written bytes and the reported outcome consistent.
#[cfg(not(unix))]
fn set_user_only_file_permissions(path: &Path) -> Result<(), TrustError> {
    Err(TrustError::PrivatePermissionsUnsupported {
        path: path.to_path_buf(),
    })
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), TrustError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(TrustError::Write)
}

// Unlike the two permission cases, this one is not a refusal to write: the
// bytes are already durable at this point because `write_user_only_file` called
// `sync_all` on the file itself. What cannot be established on a target without
// a directory-sync primitive is that the *rename* survives power loss. The sole
// caller maps any error here to `AtomicSaveOutcome::DurabilityUnknown`, which
// is precisely the truthful outcome, so reporting the missing primitive is
// enough. `Ok(())` would instead claim `Committed` durability nobody verified.
#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), TrustError> {
    Err(TrustError::Write(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory sync is unsupported on this platform",
    )))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustRegistryWire {
    schema_version: u32,
    #[serde(default)]
    bindings: Vec<TrustBindingWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustBindingWire {
    root: String,
    binding_id: BindingId,
    index_id: IndexId,
    index_name: String,
    project_id: ProjectId,
    project_name: String,
}

#[derive(Serialize)]
struct TrustRegistryWireRef<'a> {
    schema_version: u32,
    bindings: Vec<TrustBindingWireRef<'a>>,
}

#[derive(Serialize)]
struct TrustBindingWireRef<'a> {
    root: &'a str,
    binding_id: BindingId,
    index_id: IndexId,
    index_name: &'a str,
    project_id: ProjectId,
    project_name: &'a str,
}

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("trust registry is not strict TOML")]
    Malformed(#[source] toml::de::Error),
    #[error("trust registry schema {found} is unsupported")]
    UnsupportedSchema { found: u32 },
    #[error("trust registry index name is invalid")]
    InvalidIndexName(#[source] SlugError),
    #[error("trust registry project name is invalid")]
    InvalidProjectName(#[source] SlugError),
    #[error("repository root {root} could not be canonicalized: {source}")]
    CanonicalizeRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("repository root {root} is not a directory")]
    RootNotDirectory { root: PathBuf },
    #[error("stored repository root {stored} is not canonical; canonical root is {canonical}")]
    NonCanonicalRoot { stored: PathBuf, canonical: PathBuf },
    #[error("stored repository root is not an absolute normalized path: {root}")]
    InvalidStoredRoot { root: PathBuf },
    #[error("repository root {root} is already bound to different index or project context")]
    ConflictingRoot { root: PathBuf },
    #[error("index or project identity is aliased under inconsistent logical names")]
    ConflictingIdentity,
    #[error("binding {binding_id} appears more than once")]
    DuplicateBinding { binding_id: BindingId },
    #[error("binding {binding_id} is not a random UUIDv4 value")]
    NonRandomBinding { binding_id: BindingId },
    #[error("trust registry has {found} bindings; maximum is {maximum}")]
    TooManyBindings { found: usize, maximum: usize },
    #[error("repository root cannot be represented as UTF-8: {root:?}")]
    NonUtf8Root { root: PathBuf },
    #[error("trust registry could not be read")]
    Read(#[source] std::io::Error),
    #[error("trust registry must be a stable private user-owned regular file")]
    UnsafeFile,
    #[error("trust registry exceeds the 1 MiB limit")]
    TooLarge,
    #[error("trust registry changed while it was read")]
    ChangedDuringRead,
    #[error("trust registry is not UTF-8")]
    NotUtf8,
    #[error("trust registry could not be serialized: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error("trust registry path has no parent: {path}")]
    MissingParent { path: PathBuf },
    #[error("trust registry could not be written atomically: {0}")]
    Write(#[source] std::io::Error),
    #[error(
        "this platform has no user-only permission primitive, so {path} cannot be made private; \
         hSUM refuses to store a trust binding it cannot protect"
    )]
    PrivatePermissionsUnsupported { path: PathBuf },
    #[error("identity field is invalid: {0}")]
    InvalidIdentity(#[from] IdParseError),
}

#[cfg(test)]
mod tests {
    use super::{TRUST_REGISTRY_MAX_BYTES, TrustError, validate_registry_size};

    #[test]
    fn registry_size_accepts_the_exact_limit_and_rejects_one_byte_more() {
        assert!(validate_registry_size(&vec![0; TRUST_REGISTRY_MAX_BYTES]).is_ok());
        assert!(matches!(
            validate_registry_size(&vec![0; TRUST_REGISTRY_MAX_BYTES + 1]),
            Err(TrustError::TooLarge)
        ));
    }
}
