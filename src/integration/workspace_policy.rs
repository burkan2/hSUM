use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::config::{AtomicSaveOutcome, BoundedReadError, read_bounded_file};

const WORKSPACE_POLICY_SCHEMA_VERSION: u32 = 1;
const WORKSPACE_POLICY_MAX_BYTES: usize = 64 * 1024;
const WORKSPACE_POLICY_MAX_ROOTS: usize = 256;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspacePolicy {
    roots: Vec<PathBuf>,
    repository_roots: Vec<PathBuf>,
}

impl WorkspacePolicy {
    pub fn load_or_empty(path: &Path) -> Result<Self, WorkspacePolicyError> {
        match read_bounded_file(path, WORKSPACE_POLICY_MAX_BYTES, 0o077) {
            Ok(bytes) => Self::parse(&bytes),
            Err(BoundedReadError::NotFound) => Ok(Self::default()),
            Err(BoundedReadError::Unsafe) => Err(WorkspacePolicyError::UnsafeFile),
            Err(BoundedReadError::TooLarge) => Err(WorkspacePolicyError::TooLarge),
            Err(BoundedReadError::Changed) => Err(WorkspacePolicyError::ChangedDuringRead),
            Err(BoundedReadError::Io(source)) => Err(WorkspacePolicyError::Read(source)),
        }
    }

    pub fn parse(contents: &[u8]) -> Result<Self, WorkspacePolicyError> {
        if contents.len() > WORKSPACE_POLICY_MAX_BYTES {
            return Err(WorkspacePolicyError::TooLarge);
        }
        let text = std::str::from_utf8(contents).map_err(|_| WorkspacePolicyError::NotUtf8)?;
        let wire: WorkspacePolicyWire =
            toml::from_str(text).map_err(WorkspacePolicyError::Malformed)?;
        if wire.schema_version != WORKSPACE_POLICY_SCHEMA_VERSION {
            return Err(WorkspacePolicyError::UnsupportedSchema {
                found: wire.schema_version,
            });
        }
        if wire.workspace_roots.len() > WORKSPACE_POLICY_MAX_ROOTS {
            return Err(WorkspacePolicyError::TooManyRoots);
        }
        if wire.repository_roots.len() > WORKSPACE_POLICY_MAX_ROOTS {
            return Err(WorkspacePolicyError::TooManyRoots);
        }
        Ok(Self {
            roots: parse_stored_roots(wire.workspace_roots)?,
            repository_roots: parse_stored_roots(wire.repository_roots)?,
        })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn authorize(&mut self, path: &Path) -> Result<(PathBuf, bool), WorkspacePolicyError> {
        let canonical = canonical_workspace_root(path)?;
        match self.roots.binary_search(&canonical) {
            Ok(_) => Ok((canonical, false)),
            Err(index) => {
                if self.roots.len() == WORKSPACE_POLICY_MAX_ROOTS {
                    return Err(WorkspacePolicyError::TooManyRoots);
                }
                self.roots.insert(index, canonical.clone());
                Ok((canonical, true))
            }
        }
    }

    pub fn revoke(&mut self, path: &Path) -> Result<(PathBuf, bool), WorkspacePolicyError> {
        let canonical =
            fs::canonicalize(path).map_err(|source| WorkspacePolicyError::Canonicalize {
                path: path.to_path_buf(),
                source,
            })?;
        match self.roots.binary_search(&canonical) {
            Ok(index) => {
                self.roots.remove(index);
                Ok((canonical, true))
            }
            Err(_) => Ok((canonical, false)),
        }
    }

    pub fn authorizes_repository(&self, canonical_repository_root: &Path) -> bool {
        self.roots.iter().any(|workspace| {
            canonical_repository_root != workspace
                && canonical_repository_root.starts_with(workspace)
        })
    }

    pub fn mark_repository(
        &mut self,
        canonical_repository_root: &Path,
    ) -> Result<bool, WorkspacePolicyError> {
        validate_stored_root(canonical_repository_root)?;
        match self
            .repository_roots
            .binary_search_by(|root| root.as_path().cmp(canonical_repository_root))
        {
            Ok(_) => Ok(false),
            Err(index) => {
                if self.repository_roots.len() == WORKSPACE_POLICY_MAX_ROOTS {
                    return Err(WorkspacePolicyError::TooManyRoots);
                }
                self.repository_roots
                    .insert(index, canonical_repository_root.to_path_buf());
                Ok(true)
            }
        }
    }

    pub fn refreshes_repository(&self, canonical_repository_root: &Path) -> bool {
        self.repository_roots
            .binary_search_by(|root| root.as_path().cmp(canonical_repository_root))
            .is_ok()
    }

    pub fn save_atomic(&self, path: &Path) -> Result<AtomicSaveOutcome, WorkspacePolicyError> {
        let contents = self.to_toml()?;
        let parent = path
            .parent()
            .ok_or_else(|| WorkspacePolicyError::MissingParent(path.to_path_buf()))?;
        fs::create_dir_all(parent).map_err(WorkspacePolicyError::Write)?;
        set_private_directory_permissions(parent)?;
        let temporary = parent.join(format!(".integration-policy.{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write_result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            set_private_file_permissions(&temporary)
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(WorkspacePolicyError::Write(error));
        }
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(WorkspacePolicyError::Write(error));
        }
        match File::open(parent).and_then(|directory| directory.sync_all()) {
            Ok(()) => Ok(AtomicSaveOutcome::Committed),
            Err(_) => Ok(AtomicSaveOutcome::DurabilityUnknown),
        }
    }

    fn to_toml(&self) -> Result<String, WorkspacePolicyError> {
        let workspace_roots = self
            .roots
            .iter()
            .map(|root| {
                root.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| WorkspacePolicyError::NonUtf8Root(root.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let repository_roots = self
            .repository_roots
            .iter()
            .map(|root| {
                root.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| WorkspacePolicyError::NonUtf8Root(root.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let contents = toml::to_string_pretty(&WorkspacePolicyWire {
            schema_version: WORKSPACE_POLICY_SCHEMA_VERSION,
            workspace_roots,
            repository_roots,
        })
        .map_err(WorkspacePolicyError::Serialize)?;
        if contents.len() > WORKSPACE_POLICY_MAX_BYTES {
            return Err(WorkspacePolicyError::TooLarge);
        }
        Ok(contents)
    }
}

fn parse_stored_roots(roots: Vec<String>) -> Result<Vec<PathBuf>, WorkspacePolicyError> {
    let mut parsed = Vec::with_capacity(roots.len());
    for root in roots {
        let path = PathBuf::from(root);
        validate_stored_root(&path)?;
        if parsed.last().is_some_and(|previous| previous >= &path) {
            return Err(WorkspacePolicyError::UnsortedOrDuplicateRoots);
        }
        parsed.push(path);
    }
    Ok(parsed)
}

fn canonical_workspace_root(path: &Path) -> Result<PathBuf, WorkspacePolicyError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| WorkspacePolicyError::Canonicalize {
            path: path.to_path_buf(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(WorkspacePolicyError::NotDirectory(canonical));
    }
    if canonical.parent().is_none() || normal_component_count(&canonical) < 2 {
        return Err(WorkspacePolicyError::DangerouslyBroad(canonical));
    }
    if BaseDirs::new()
        .and_then(|directories| fs::canonicalize(directories.home_dir()).ok())
        .is_some_and(|home| home == canonical)
    {
        return Err(WorkspacePolicyError::DangerouslyBroad(canonical));
    }
    let git_marker = canonical.join(".git");
    if fs::symlink_metadata(&git_marker).is_ok() {
        return Err(WorkspacePolicyError::RepositoryRoot(canonical));
    }
    Ok(canonical)
}

fn normal_component_count(path: &Path) -> usize {
    path.components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count()
}

fn validate_stored_root(root: &Path) -> Result<(), WorkspacePolicyError> {
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
        return Err(WorkspacePolicyError::InvalidStoredRoot(root.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), WorkspacePolicyError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(WorkspacePolicyError::Write)
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), WorkspacePolicyError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkspacePolicyWire {
    schema_version: u32,
    #[serde(default)]
    workspace_roots: Vec<String>,
    #[serde(default)]
    repository_roots: Vec<String>,
}

#[derive(Debug, Error)]
pub enum WorkspacePolicyError {
    #[error("workspace policy could not be read")]
    Read(#[source] std::io::Error),
    #[error("workspace policy could not be written")]
    Write(#[source] std::io::Error),
    #[error("workspace policy must be a stable private user-owned regular file")]
    UnsafeFile,
    #[error("workspace policy exceeds the 64 KiB limit")]
    TooLarge,
    #[error("workspace policy changed while it was being read")]
    ChangedDuringRead,
    #[error("workspace policy is not UTF-8")]
    NotUtf8,
    #[error("workspace policy is malformed")]
    Malformed(#[source] toml::de::Error),
    #[error("workspace policy schema {found} is unsupported")]
    UnsupportedSchema { found: u32 },
    #[error("workspace policy contains too many roots")]
    TooManyRoots,
    #[error("workspace policy roots must be uniquely sorted")]
    UnsortedOrDuplicateRoots,
    #[error("workspace policy root is not canonical and absolute: {0}")]
    InvalidStoredRoot(PathBuf),
    #[error("workspace root could not be canonicalized: {path}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace root is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("workspace root is dangerously broad: {0}")]
    DangerouslyBroad(PathBuf),
    #[error("workspace authorization must name a parent directory, not a Git repository: {0}")]
    RepositoryRoot(PathBuf),
    #[error("workspace policy root is not UTF-8: {0}")]
    NonUtf8Root(PathBuf),
    #[error("workspace policy path has no parent: {0}")]
    MissingParent(PathBuf),
    #[error("workspace policy could not be serialized")]
    Serialize(#[source] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn authorization_is_canonical_sorted_idempotent_and_root_bounded() {
        let root = tempdir().unwrap();
        let first = root.path().join("z-workspace");
        let second = root.path().join("a-workspace");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let repository = second.join("repo");
        fs::create_dir_all(repository.join(".git")).unwrap();

        let mut policy = WorkspacePolicy::default();
        assert!(policy.authorize(&first).unwrap().1);
        assert!(policy.authorize(&second).unwrap().1);
        assert!(!policy.authorize(&second).unwrap().1);
        assert_eq!(
            policy.roots(),
            &[
                second.canonicalize().unwrap(),
                first.canonicalize().unwrap()
            ]
        );
        assert!(policy.authorizes_repository(&repository.canonicalize().unwrap()));
        assert!(!policy.authorizes_repository(&second.canonicalize().unwrap()));
        assert!(
            policy
                .mark_repository(&repository.canonicalize().unwrap())
                .unwrap()
        );
        assert!(policy.refreshes_repository(&repository.canonicalize().unwrap()));
    }

    #[test]
    fn policy_round_trips_and_rejects_a_git_root_as_workspace() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("projects");
        fs::create_dir_all(&workspace).unwrap();
        let policy_path = root.path().join("config/integration-policy.toml");
        let mut policy = WorkspacePolicy::default();
        policy.authorize(&workspace).unwrap();
        policy.save_atomic(&policy_path).unwrap();
        assert_eq!(
            WorkspacePolicy::load_or_empty(&policy_path).unwrap(),
            policy
        );

        fs::create_dir(workspace.join(".git")).unwrap();
        assert!(matches!(
            WorkspacePolicy::default().authorize(&workspace),
            Err(WorkspacePolicyError::RepositoryRoot(_))
        ));
    }
}
