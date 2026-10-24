use std::env;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use thiserror::Error;

use crate::domain::SafeSlug;

const APPLICATION_NAME: &str = "hsum";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl ManagedPaths {
    pub fn resolve(hsum_home: Option<&Path>) -> Result<Self, ManagedPathsError> {
        match hsum_home {
            Some(root) => Self::from_hsum_home(root),
            None => Self::from_project_dirs(),
        }
    }

    pub fn from_environment() -> Result<Self, ManagedPathsError> {
        match env::var_os("HSUM_HOME") {
            Some(root) => Self::from_hsum_home(Path::new(&root)),
            None => Self::from_project_dirs(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn trust_registry_file(&self) -> PathBuf {
        self.config_dir.join("trusted-projects.toml")
    }

    pub fn managed_backup_registry_file(&self) -> PathBuf {
        self.data_dir.join("managed-backups.json")
    }

    pub fn integration_policy_file(&self) -> PathBuf {
        self.config_dir.join("integration-policy.toml")
    }

    pub fn index_database(&self, index_name: &SafeSlug) -> PathBuf {
        self.data_dir
            .join("indexes")
            .join(index_name.as_str())
            .join("index.sqlite")
    }

    pub fn model_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("models")
    }

    pub fn with_overrides(
        mut self,
        data_dir: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
    ) -> Result<Self, ManagedPathsError> {
        if let Some(data_dir) = data_dir {
            if !data_dir.is_absolute() {
                return Err(ManagedPathsError::RelativeDirectoryOverride {
                    kind: "data",
                    path: data_dir,
                });
            }
            self.data_dir = data_dir;
        }
        if let Some(cache_dir) = cache_dir {
            if !cache_dir.is_absolute() {
                return Err(ManagedPathsError::RelativeDirectoryOverride {
                    kind: "cache",
                    path: cache_dir,
                });
            }
            self.cache_dir = cache_dir;
        }
        Ok(self)
    }

    fn from_hsum_home(root: &Path) -> Result<Self, ManagedPathsError> {
        if root.as_os_str().is_empty() {
            return Err(ManagedPathsError::EmptyHomeOverride);
        }
        if !root.is_absolute() {
            return Err(ManagedPathsError::RelativeHomeOverride {
                path: root.to_path_buf(),
            });
        }

        Ok(Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        })
    }

    fn from_project_dirs() -> Result<Self, ManagedPathsError> {
        let project_dirs =
            ProjectDirs::from("", "", APPLICATION_NAME).ok_or(ManagedPathsError::Unavailable)?;
        Ok(Self {
            config_dir: project_dirs.config_dir().to_path_buf(),
            data_dir: project_dirs.data_dir().to_path_buf(),
            cache_dir: project_dirs.cache_dir().to_path_buf(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ManagedPathsError {
    #[error("HSUM_HOME must not be empty")]
    EmptyHomeOverride,
    #[error("HSUM_HOME must be absolute, got {path}")]
    RelativeHomeOverride { path: PathBuf },
    #[error("the operating system did not provide project directories")]
    Unavailable,
    #[error("{kind} directory override must be absolute, got {path}")]
    RelativeDirectoryOverride { kind: &'static str, path: PathBuf },
}
