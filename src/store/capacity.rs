use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

pub const MINIMUM_STORAGE_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemLocality {
    Local,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncRootKind {
    ICloudDrive,
    Dropbox,
    OneDrive,
    GoogleDrive,
    BoxDrive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FilesystemAssessment {
    pub locality: FilesystemLocality,
    pub filesystem_type: Option<String>,
    pub sync_root: Option<SyncRootKind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoragePreflight {
    pub filesystem: FilesystemAssessment,
    pub available_bytes: u64,
    pub managed_index_bytes: u64,
    pub estimated_write_bytes: u64,
    pub reserve_bytes: u64,
    pub required_free_bytes: u64,
    pub quota_bytes: Option<u64>,
    pub required_quota_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StorageInspection {
    pub filesystem: FilesystemAssessment,
    pub available_bytes: u64,
    pub managed_index_bytes: u64,
    pub reserve_bytes: u64,
    pub quota_bytes: Option<u64>,
}

impl StoragePreflight {
    pub fn run(
        index_path: &Path,
        estimated_write_bytes: u64,
        quota_bytes: Option<u64>,
    ) -> Result<Self, StoragePreflightError> {
        let inspection = StorageInspection::run(index_path, quota_bytes)?;
        evaluate_preflight(
            estimated_write_bytes,
            inspection.managed_index_bytes,
            inspection.quota_bytes,
            StorageProbe {
                filesystem: inspection.filesystem,
                available_bytes: inspection.available_bytes,
            },
        )
    }

    pub(crate) fn run_staging(
        staging_directory: &Path,
        estimated_staging_bytes: u64,
    ) -> Result<Self, StoragePreflightError> {
        evaluate_preflight(
            estimated_staging_bytes,
            0,
            None,
            probe_storage(staging_directory)?,
        )
    }
}

impl StorageInspection {
    pub fn run(index_path: &Path, quota_bytes: Option<u64>) -> Result<Self, StoragePreflightError> {
        let parent = index_path
            .parent()
            .ok_or_else(|| StoragePreflightError::PathHasNoParent {
                path: index_path.to_path_buf(),
            })?;
        let managed_index_bytes = managed_index_bytes(index_path)?;
        let probe = probe_storage(parent)?;
        Ok(Self {
            filesystem: probe.filesystem,
            available_bytes: probe.available_bytes,
            managed_index_bytes,
            reserve_bytes: storage_reserve_bytes(managed_index_bytes),
            quota_bytes,
        })
    }
}

#[derive(Debug, Error)]
pub enum StoragePreflightError {
    #[error("index path has no parent directory: {path}")]
    PathHasNoParent { path: PathBuf },
    #[error("managed index file is not a regular file: {path}")]
    UnsafeManagedFile { path: PathBuf },
    #[error("storage filesystem could not be inspected for {path}")]
    FilesystemInspectionUnavailable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("known network filesystem {filesystem_type} is unsupported for index storage")]
    UnsupportedNetworkFilesystem { filesystem_type: String },
    #[error("available storage capacity could not be determined for {path}")]
    CapacityUnavailable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("storage arithmetic overflowed while calculating {calculation}")]
    ArithmeticOverflow { calculation: &'static str },
    #[error(
        "insufficient disk capacity: {available_bytes} bytes available, {required_bytes} required"
    )]
    InsufficientCapacity {
        available_bytes: u64,
        required_bytes: u64,
        reserve_bytes: u64,
    },
    #[error(
        "index quota exceeded: quota is {quota_bytes} bytes, {required_bytes} bytes are required"
    )]
    QuotaExceeded {
        quota_bytes: u64,
        required_bytes: u64,
        reserve_bytes: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StorageProbe {
    filesystem: FilesystemAssessment,
    available_bytes: u64,
}

fn evaluate_preflight(
    estimated_write_bytes: u64,
    managed_index_bytes: u64,
    quota_bytes: Option<u64>,
    probe: StorageProbe,
) -> Result<StoragePreflight, StoragePreflightError> {
    let reserve_bytes = storage_reserve_bytes(managed_index_bytes);
    let required_free_bytes = estimated_write_bytes.checked_add(reserve_bytes).ok_or(
        StoragePreflightError::ArithmeticOverflow {
            calculation: "estimated write plus recovery reserve",
        },
    )?;
    if probe.available_bytes < required_free_bytes {
        return Err(StoragePreflightError::InsufficientCapacity {
            available_bytes: probe.available_bytes,
            required_bytes: required_free_bytes,
            reserve_bytes,
        });
    }

    let required_quota_bytes = quota_bytes
        .map(|quota| {
            let required = managed_index_bytes.checked_add(required_free_bytes).ok_or(
                StoragePreflightError::ArithmeticOverflow {
                    calculation: "managed bytes plus estimated write and recovery reserve",
                },
            )?;
            if quota < required {
                return Err(StoragePreflightError::QuotaExceeded {
                    quota_bytes: quota,
                    required_bytes: required,
                    reserve_bytes,
                });
            }
            Ok(required)
        })
        .transpose()?;

    Ok(StoragePreflight {
        filesystem: probe.filesystem,
        available_bytes: probe.available_bytes,
        managed_index_bytes,
        estimated_write_bytes,
        reserve_bytes,
        required_free_bytes,
        quota_bytes,
        required_quota_bytes,
    })
}

pub const fn storage_reserve_bytes(managed_index_bytes: u64) -> u64 {
    let ten_percent = managed_index_bytes / 10 + (!managed_index_bytes.is_multiple_of(10)) as u64;
    if ten_percent > MINIMUM_STORAGE_RESERVE_BYTES {
        ten_percent
    } else {
        MINIMUM_STORAGE_RESERVE_BYTES
    }
}

fn managed_index_bytes(index_path: &Path) -> Result<u64, StoragePreflightError> {
    sqlite_managed_paths(index_path).try_fold(0_u64, |total, path| {
        let Some(length) = regular_file_length(&path)? else {
            return Ok(total);
        };
        total
            .checked_add(length)
            .ok_or(StoragePreflightError::ArithmeticOverflow {
                calculation: "managed index bytes",
            })
    })
}

fn sqlite_managed_paths(index_path: &Path) -> impl Iterator<Item = PathBuf> {
    ["", "-wal", "-shm"].into_iter().map(|suffix| {
        if suffix.is_empty() {
            return index_path.to_path_buf();
        }
        let mut value = index_path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    })
}

#[cfg(unix)]
fn regular_file_length(path: &Path) -> Result<Option<u64>, StoragePreflightError> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, open};

    const FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK);
    let descriptor = match open(path, FLAGS, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP) => {
            return Err(StoragePreflightError::UnsafeManagedFile {
                path: path.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(StoragePreflightError::FilesystemInspectionUnavailable {
                path: path.to_path_buf(),
                source: error.into(),
            });
        }
    };
    let metadata = fstat(&descriptor).map_err(|error| {
        StoragePreflightError::FilesystemInspectionUnavailable {
            path: path.to_path_buf(),
            source: error.into(),
        }
    })?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(StoragePreflightError::UnsafeManagedFile {
            path: path.to_path_buf(),
        });
    }
    let length =
        u64::try_from(metadata.st_size).map_err(|_| StoragePreflightError::UnsafeManagedFile {
            path: path.to_path_buf(),
        })?;
    Ok(Some(length))
}

#[cfg(not(unix))]
fn regular_file_length(path: &Path) -> Result<Option<u64>, StoragePreflightError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(StoragePreflightError::UnsafeManagedFile {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoragePreflightError::FilesystemInspectionUnavailable {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe_storage(path: &Path) -> Result<StorageProbe, StoragePreflightError> {
    use rustix::fs::{Mode, OFlags, fstatfs, fstatvfs, open};

    const FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW);
    let descriptor = open(path, FLAGS, Mode::empty()).map_err(|error| {
        StoragePreflightError::FilesystemInspectionUnavailable {
            path: path.to_path_buf(),
            source: error.into(),
        }
    })?;

    let filesystem = match fstatfs(&descriptor) {
        Ok(statistics) => filesystem_assessment(&statistics, path)?,
        Err(_) => FilesystemAssessment {
            locality: FilesystemLocality::Unknown,
            filesystem_type: None,
            sync_root: classify_sync_root(path),
        },
    };
    let capacity =
        fstatvfs(&descriptor).map_err(|error| StoragePreflightError::CapacityUnavailable {
            path: path.to_path_buf(),
            source: error.into(),
        })?;
    let available_bytes = available_bytes(&capacity, path)?;
    Ok(StorageProbe {
        filesystem,
        available_bytes,
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn probe_storage(path: &Path) -> Result<StorageProbe, StoragePreflightError> {
    use rustix::fs::{Mode, OFlags, fstatvfs, open};

    const FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW);
    let descriptor = open(path, FLAGS, Mode::empty()).map_err(|error| {
        StoragePreflightError::FilesystemInspectionUnavailable {
            path: path.to_path_buf(),
            source: error.into(),
        }
    })?;
    let capacity =
        fstatvfs(&descriptor).map_err(|error| StoragePreflightError::CapacityUnavailable {
            path: path.to_path_buf(),
            source: error.into(),
        })?;
    let available_bytes = available_bytes(&capacity, path)?;
    Ok(StorageProbe {
        filesystem: FilesystemAssessment {
            locality: FilesystemLocality::Unknown,
            filesystem_type: None,
            sync_root: classify_sync_root(path),
        },
        available_bytes,
    })
}

#[cfg(unix)]
fn available_bytes(
    capacity: &rustix::fs::StatVfs,
    path: &Path,
) -> Result<u64, StoragePreflightError> {
    let fragment_size = if capacity.f_frsize == 0 {
        capacity.f_bsize
    } else {
        capacity.f_frsize
    };
    if fragment_size == 0 {
        return Err(StoragePreflightError::CapacityUnavailable {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "filesystem reported a zero block size",
            ),
        });
    }
    capacity
        .f_bavail
        .checked_mul(fragment_size)
        .ok_or(StoragePreflightError::ArithmeticOverflow {
            calculation: "available filesystem capacity",
        })
}

#[cfg(not(unix))]
fn probe_storage(path: &Path) -> Result<StorageProbe, StoragePreflightError> {
    Err(StoragePreflightError::FilesystemInspectionUnavailable {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem inspection is unsupported on this platform",
        ),
    })
}

#[cfg(target_os = "macos")]
fn filesystem_assessment(
    statistics: &rustix::fs::StatFs,
    path: &Path,
) -> Result<FilesystemAssessment, StoragePreflightError> {
    const MNT_LOCAL: u32 = 0x0000_1000;

    let filesystem_type = c_char_array_to_string(&statistics.f_fstypename);
    if is_known_network_name(&filesystem_type) {
        return Err(StoragePreflightError::UnsupportedNetworkFilesystem { filesystem_type });
    }
    let locality =
        if statistics.f_flags & MNT_LOCAL != 0 && is_known_local_macos_name(&filesystem_type) {
            FilesystemLocality::Local
        } else {
            FilesystemLocality::Unknown
        };
    Ok(FilesystemAssessment {
        locality,
        filesystem_type: (!filesystem_type.is_empty()).then_some(filesystem_type),
        sync_root: classify_sync_root(path),
    })
}

#[cfg(target_os = "macos")]
fn c_char_array_to_string(value: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = value
        .iter()
        .take_while(|character| **character != 0)
        .map(|character| character.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "linux")]
fn filesystem_assessment(
    statistics: &rustix::fs::StatFs,
    path: &Path,
) -> Result<FilesystemAssessment, StoragePreflightError> {
    let magic = (statistics.f_type as u64) & u64::from(u32::MAX);
    let filesystem_type = format!("magic:0x{magic:08x}");
    match classify_linux_magic(magic) {
        FilesystemMagicClass::Network => {
            Err(StoragePreflightError::UnsupportedNetworkFilesystem { filesystem_type })
        }
        FilesystemMagicClass::Local => Ok(FilesystemAssessment {
            locality: FilesystemLocality::Local,
            filesystem_type: Some(filesystem_type),
            sync_root: classify_sync_root(path),
        }),
        FilesystemMagicClass::Unknown => Ok(FilesystemAssessment {
            locality: FilesystemLocality::Unknown,
            filesystem_type: Some(filesystem_type),
            sync_root: classify_sync_root(path),
        }),
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilesystemMagicClass {
    Local,
    Network,
    Unknown,
}

#[cfg(any(target_os = "linux", test))]
fn classify_linux_magic(magic: u64) -> FilesystemMagicClass {
    match magic {
        // NFS, SMB/CIFS, SMB2, AFS, Coda, Ceph, NCP, and 9P.
        0x0000_6969 | 0x0000_517b | 0xff53_4d42 | 0xfe53_4d42 | 0x5346_414f | 0x7375_7245
        | 0x00c3_6400 | 0x0000_564c | 0x0102_1997 => FilesystemMagicClass::Network,
        // ext2/3/4, XFS, Btrfs, and ZFS.
        0x0000_ef53 | 0x5846_5342 | 0x9123_683e | 0x2fc1_2fc1 => FilesystemMagicClass::Local,
        _ => FilesystemMagicClass::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn is_known_local_macos_name(filesystem_type: &str) -> bool {
    matches!(
        filesystem_type.to_ascii_lowercase().as_str(),
        "apfs" | "hfs" | "ufs"
    )
}

#[cfg(any(target_os = "macos", test))]
fn is_known_network_name(filesystem_type: &str) -> bool {
    matches!(
        filesystem_type.to_ascii_lowercase().as_str(),
        "nfs" | "nfs4" | "smb" | "smbfs" | "cifs" | "afp" | "afpfs" | "webdav" | "webdavfs"
    )
}

pub fn classify_sync_root(path: &Path) -> Option<SyncRootKind> {
    let components: Vec<&OsStr> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();

    for (index, component) in components.iter().enumerate() {
        let text = component.to_string_lossy();
        if text == "Dropbox" {
            return Some(SyncRootKind::Dropbox);
        }
        if text == "Mobile Documents"
            && components
                .get(index + 1)
                .is_some_and(|next| *next == OsStr::new("com~apple~CloudDocs"))
        {
            return Some(SyncRootKind::ICloudDrive);
        }
        if text == "OneDrive" || text.starts_with("OneDrive-") {
            return Some(SyncRootKind::OneDrive);
        }
        if text == "Google Drive" || text == "GoogleDrive" || text.starts_with("GoogleDrive-") {
            return Some(SyncRootKind::GoogleDrive);
        }
        if text == "Box" || text == "Box-Box" {
            return Some(SyncRootKind::BoxDrive);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn probe(available_bytes: u64) -> StorageProbe {
        StorageProbe {
            filesystem: FilesystemAssessment {
                locality: FilesystemLocality::Local,
                filesystem_type: Some("testfs".to_owned()),
                sync_root: None,
            },
            available_bytes,
        }
    }

    #[test]
    fn reserve_is_64_mib_or_rounded_up_ten_percent() {
        assert_eq!(storage_reserve_bytes(0), MINIMUM_STORAGE_RESERVE_BYTES);
        assert_eq!(
            storage_reserve_bytes(MINIMUM_STORAGE_RESERVE_BYTES * 10),
            MINIMUM_STORAGE_RESERVE_BYTES
        );
        assert_eq!(
            storage_reserve_bytes(MINIMUM_STORAGE_RESERVE_BYTES * 10 + 1),
            MINIMUM_STORAGE_RESERVE_BYTES + 1
        );
        assert_eq!(storage_reserve_bytes(u64::MAX), u64::MAX / 10 + 1);
    }

    #[test]
    fn checked_preflight_preserves_reserve_and_quota_headroom() {
        let estimate = 20 * 1024 * 1024;
        let managed = 10 * 1024 * 1024;
        let reserve = MINIMUM_STORAGE_RESERVE_BYTES;
        let required_free = estimate + reserve;
        let required_quota = managed + required_free;
        let report = evaluate_preflight(
            estimate,
            managed,
            Some(required_quota),
            probe(required_free),
        )
        .unwrap();

        assert_eq!(report.reserve_bytes, reserve);
        assert_eq!(report.required_free_bytes, required_free);
        assert_eq!(report.required_quota_bytes, Some(required_quota));
    }

    #[test]
    fn insufficient_capacity_and_quota_are_distinct() {
        let disk_error =
            evaluate_preflight(1, 0, None, probe(MINIMUM_STORAGE_RESERVE_BYTES)).unwrap_err();
        assert!(matches!(
            disk_error,
            StoragePreflightError::InsufficientCapacity { .. }
        ));

        let quota_error =
            evaluate_preflight(0, 1, Some(MINIMUM_STORAGE_RESERVE_BYTES), probe(u64::MAX))
                .unwrap_err();
        assert!(matches!(
            quota_error,
            StoragePreflightError::QuotaExceeded { .. }
        ));
    }

    #[test]
    fn arithmetic_overflow_refuses_preflight() {
        let error = evaluate_preflight(u64::MAX, 0, None, probe(u64::MAX)).unwrap_err();
        assert!(matches!(
            error,
            StoragePreflightError::ArithmeticOverflow { .. }
        ));
    }

    #[test]
    fn known_network_magic_and_names_are_classified_conservatively() {
        assert_eq!(
            classify_linux_magic(0x0000_6969),
            FilesystemMagicClass::Network
        );
        assert_eq!(
            classify_linux_magic(0xff53_4d42),
            FilesystemMagicClass::Network
        );
        assert_eq!(
            classify_linux_magic(0x0000_ef53),
            FilesystemMagicClass::Local
        );
        assert_eq!(
            classify_linux_magic(0x0102_1994),
            FilesystemMagicClass::Unknown
        );
        assert_eq!(
            classify_linux_magic(0x6573_5546),
            FilesystemMagicClass::Unknown
        );
        assert!(is_known_network_name("nfs"));
        assert!(is_known_network_name("SMBFS"));
        assert!(!is_known_network_name("apfs"));
    }

    #[test]
    fn common_sync_roots_are_warnings() {
        assert_eq!(
            classify_sync_root(Path::new(
                "/Users/me/Library/Mobile Documents/com~apple~CloudDocs/project"
            )),
            Some(SyncRootKind::ICloudDrive)
        );
        assert_eq!(
            classify_sync_root(Path::new(
                "/Users/me/Library/CloudStorage/OneDrive-Example/project"
            )),
            Some(SyncRootKind::OneDrive)
        );
        assert_eq!(
            classify_sync_root(Path::new("/Users/me/Dropbox/project")),
            Some(SyncRootKind::Dropbox)
        );
        assert_eq!(classify_sync_root(Path::new("/var/lib/hsum")), None);
    }

    #[test]
    fn managed_bytes_include_sqlite_wal_and_shared_memory() {
        let directory = tempdir().unwrap();
        let index_path = directory.path().join("index.sqlite");
        std::fs::write(&index_path, [0_u8; 11]).unwrap();
        std::fs::write(
            sqlite_managed_paths(&index_path).nth(1).unwrap(),
            [0_u8; 13],
        )
        .unwrap();
        std::fs::write(
            sqlite_managed_paths(&index_path).nth(2).unwrap(),
            [0_u8; 17],
        )
        .unwrap();

        assert_eq!(managed_index_bytes(&index_path).unwrap(), 41);
    }

    #[cfg(unix)]
    #[test]
    fn managed_bytes_refuse_symlinked_sqlite_files() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let target = directory.path().join("target");
        let index_path = directory.path().join("index.sqlite");
        std::fs::write(&target, b"not an index").unwrap();
        symlink(&target, &index_path).unwrap();

        assert!(matches!(
            managed_index_bytes(&index_path),
            Err(StoragePreflightError::UnsafeManagedFile { .. })
        ));
    }
}
