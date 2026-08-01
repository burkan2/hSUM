#[cfg(unix)]
use std::ffi::OsString;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::IndexId;
use crate::store::doctor::{FingerprintPolicy, InspectionDepth, inspect_connection};
use crate::store::schema::{
    APPLICATION_ID, MIGRATIONS, SCHEMA_VERSION, migration_checksum, pipeline_fingerprint,
    schema_checksum,
};

const MAX_VALUE_BYTES: i32 = 32 * 1024 * 1024;
const MAX_SQL_BYTES: i32 = 1024 * 1024;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct IndexDb {
    connection: Connection,
    path: PathBuf,
    validated_path: ValidatedIndexPath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenMode {
    ReadOnly,
    ReadWrite,
}

impl IndexDb {
    pub fn create(path: &Path, index_id: IndexId) -> Result<Self, StoreError> {
        Self::create_with_durability(path, index_id, &OsIndexDurability)
    }

    fn create_with_durability(
        path: &Path,
        index_id: IndexId,
        durability: &impl IndexDurability,
    ) -> Result<Self, StoreError> {
        Self::create_with_durability_and_observer(path, index_id, durability, &mut |_| {})
    }

    #[doc(hidden)]
    pub fn create_with_observer(
        path: &Path,
        index_id: IndexId,
        mut observer: impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        Self::create_with_durability_and_observer(path, index_id, &OsIndexDurability, &mut observer)
    }

    fn create_with_durability_and_observer(
        path: &Path,
        index_id: IndexId,
        durability: &impl IndexDurability,
        observer: &mut impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        let parent = ValidatedParent::acquire(path)?;
        parent.require_sidecars_absent()?;
        let mut reservation = reserve_new_file(parent)?;
        reservation.require_sidecars_absent()?;
        reservation.revalidate_namespace()?;
        let sqlite_path = reservation.path().to_path_buf();

        observer("before_sqlite_open");
        let connection_result = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        );
        observer("after_sqlite_open");
        let mut connection = connection_result?;
        reservation.observe_sidecars()?;
        reservation.verify_connection(&connection)?;

        let configure_result = configure_connection(&connection);
        reservation.observe_sidecars()?;
        configure_result?;

        let journal_result =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0));
        reservation.observe_sidecars()?;
        let journal_mode: String = journal_result?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::WalUnavailable(journal_mode));
        }
        let synchronous_result = connection.pragma_update(None, "synchronous", "FULL");
        reservation.observe_sidecars()?;
        synchronous_result?;

        let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let index_uuid = *index_id.as_uuid().as_bytes();
        let schema_digest = schema_checksum();
        let pipeline_digest = pipeline_fingerprint();

        connection.pragma_update(None, "foreign_keys", false)?;
        let transaction_result =
            connection.transaction_with_behavior(TransactionBehavior::Immediate);
        reservation.observe_sidecars()?;
        let transaction = transaction_result?;
        for (_, migration) in MIGRATIONS {
            let migration_result = transaction.execute_batch(migration);
            reservation.observe_sidecars()?;
            migration_result?;
        }
        let application_id_result =
            transaction.pragma_update(None, "application_id", APPLICATION_ID);
        reservation.observe_sidecars()?;
        application_id_result?;
        let schema_version_result = transaction.pragma_update(None, "user_version", SCHEMA_VERSION);
        reservation.observe_sidecars()?;
        schema_version_result?;
        for (version, _) in MIGRATIONS {
            let checksum = migration_checksum(version)
                .expect("compiled migration list has a checksum for every version");
            let migration_row_result = transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at, checksum)
                 VALUES (?1, ?2, ?3)",
                params![
                    i64::from(version),
                    applied_at,
                    checksum.as_bytes().as_slice(),
                ],
            );
            reservation.observe_sidecars()?;
            migration_row_result?;
        }

        let metadata_result = (|| -> Result<(), rusqlite::Error> {
            let metadata: [(&str, &[u8]); 10] = [
                ("index_uuid", index_uuid.as_slice()),
                ("api_version", b"hsum.api.v1"),
                ("schema_version", b"3"),
                ("schema_checksum", schema_digest.as_bytes().as_slice()),
                ("embedding_profile", b"none"),
                (
                    "pipeline_fingerprint",
                    pipeline_digest.as_bytes().as_slice(),
                ),
                ("index_epoch", b"0"),
                ("active_generation", b""),
                ("history_floor_epoch", b"1"),
                ("replacement_epoch", b"0"),
            ];
            let mut insert =
                transaction.prepare("INSERT INTO index_meta(key, value) VALUES (?1, ?2)")?;
            for (key, value) in metadata {
                insert.execute(params![key, value])?;
            }
            Ok(())
        })();
        reservation.observe_sidecars()?;
        metadata_result?;
        let commit_result = transaction.commit();
        reservation.observe_sidecars()?;
        commit_result?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let foreign_key_violation = connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()?
            .is_some();
        if foreign_key_violation {
            return Err(StoreError::ForeignKeyCheckFailed);
        }
        observer("after_creation_transaction");
        reservation.verify_connection(&connection)?;
        let durability_result = durability.sync_created_index(reservation.validated());
        reservation.observe_sidecars()?;
        durability_result?;
        reservation.verify_connection(&connection)?;
        let validated_path = reservation.persist();

        Ok(Self {
            connection,
            path: sqlite_path,
            validated_path,
        })
    }

    pub fn is_read_only(&self) -> Result<bool, StoreError> {
        self.connection.is_readonly("main").map_err(Into::into)
    }

    pub fn open_existing(path: &Path, mode: OpenMode) -> Result<Self, StoreError> {
        Self::open_existing_with_observer(path, mode, |_| {})
    }

    pub(crate) fn open_existing_with_policy(
        path: &Path,
        mode: OpenMode,
        policy: FingerprintPolicy,
    ) -> Result<Self, StoreError> {
        Self::open_existing_with_policy_and_observer(path, mode, policy, |_| {})
    }

    #[doc(hidden)]
    pub fn open_existing_with_observer(
        path: &Path,
        mode: OpenMode,
        observer: impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        Self::open_existing_with_policy_and_observer(
            path,
            mode,
            FingerprintPolicy::Reject,
            observer,
        )
    }

    fn open_existing_with_policy_and_observer(
        path: &Path,
        mode: OpenMode,
        policy: FingerprintPolicy,
        mut observer: impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        let database = match mode {
            OpenMode::ReadOnly => Self::open_read_only_with_observer(path, &mut observer)?,
            OpenMode::ReadWrite => Self::open_read_write_with_observer(path, &mut observer)?,
        };
        inspect_connection(
            database.connection(),
            mode == OpenMode::ReadOnly,
            InspectionDepth::Open,
            policy,
        )?;
        database.verify_live_identity()?;
        Ok(database)
    }

    pub(crate) fn open_existing_for_maintenance(
        path: &Path,
        mode: OpenMode,
    ) -> Result<Self, StoreError> {
        let database = match mode {
            OpenMode::ReadOnly => Self::open_read_only(path)?,
            OpenMode::ReadWrite => Self::open_read_write_with_observer(path, &mut |_| {})?,
        };
        database.verify_live_identity()?;
        Ok(database)
    }

    pub(crate) fn remove(self) -> Result<(), StoreError> {
        self.validated_path.verify_connection(&self.connection)?;
        let Self {
            connection,
            validated_path,
            ..
        } = self;
        drop(connection);
        validated_path.remove()
    }

    pub(crate) fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        Self::open_read_only_with_observer(path, &mut |_| {})
    }

    fn open_read_only_with_observer(
        path: &Path,
        observer: &mut impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        let identity = validate_index_path(path, FileAccess::ReadOnly)?;
        identity.validate_sidecars()?;
        identity.revalidate_namespace()?;
        let sqlite_path = identity.path().to_path_buf();

        observer("before_sqlite_open");
        let connection_result = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        );
        observer("after_sqlite_open");
        let connection = connection_result?;
        identity.verify_connection(&connection)?;
        configure_connection(&connection)?;
        connection.pragma_update(None, "query_only", true)?;
        identity.verify_connection(&connection)?;

        Ok(Self {
            connection,
            path: sqlite_path,
            validated_path: identity,
        })
    }

    fn open_read_write_with_observer(
        path: &Path,
        observer: &mut impl FnMut(&'static str),
    ) -> Result<Self, StoreError> {
        let identity = validate_index_path(path, FileAccess::ReadWrite)?;
        identity.validate_sidecars()?;
        identity.revalidate_namespace()?;
        let sqlite_path = identity.path().to_path_buf();

        observer("before_sqlite_open");
        let connection_result = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        );
        observer("after_sqlite_open");
        let connection = connection_result?;
        identity.verify_connection(&connection)?;
        configure_connection(&connection)?;
        connection.pragma_update(None, "query_only", false)?;
        identity.verify_connection(&connection)?;

        Ok(Self {
            connection,
            path: sqlite_path,
            validated_path: identity,
        })
    }

    pub(crate) const fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[doc(hidden)]
    pub fn verify_live_identity(&self) -> Result<(), StoreError> {
        self.validated_path.verify_connection(&self.connection)
    }
}

trait IndexDurability {
    fn sync_created_index(&self, path: &ValidatedIndexPath) -> Result<(), StoreError>;
}

struct OsIndexDurability;

impl IndexDurability for OsIndexDurability {
    fn sync_created_index(&self, path: &ValidatedIndexPath) -> Result<(), StoreError> {
        sync_created_index(path)
    }
}

const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

#[cfg(unix)]
fn sync_created_index(path: &ValidatedIndexPath) -> Result<(), StoreError> {
    sync_file_descriptor(&path.descriptor)?;
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        if let Some((descriptor, _)) = path.parent.open_validated_sidecar(suffix)? {
            sync_file_descriptor(&descriptor)?;
        }
    }
    rustix::fs::fsync(&path.parent.descriptor).map_err(io::Error::from)?;
    Ok(())
}

#[cfg(all(unix, target_os = "macos"))]
fn sync_file_descriptor(descriptor: &std::os::fd::OwnedFd) -> io::Result<()> {
    rustix::fs::fcntl_fullfsync(descriptor).map_err(Into::into)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn sync_file_descriptor(descriptor: &std::os::fd::OwnedFd) -> io::Result<()> {
    rustix::fs::fsync(descriptor).map_err(Into::into)
}

#[cfg(not(unix))]
fn sync_created_index(path: &ValidatedIndexPath) -> Result<(), StoreError> {
    OpenOptions::new()
        .read(true)
        .open(path.path())?
        .sync_all()?;
    let parent = path
        .path()
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "index path has no parent"))?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum FileAccess {
    ReadOnly,
    ReadWrite,
}

#[cfg(not(unix))]
#[derive(Clone, Copy)]
enum FileAccess {
    ReadOnly,
    ReadWrite,
}

#[cfg(unix)]
struct ValidatedParent {
    descriptor: std::os::fd::OwnedFd,
    identity: FileIdentity,
    physical_path: PathBuf,
    file_name: OsString,
    requested_path: PathBuf,
}

#[cfg(unix)]
impl ValidatedParent {
    fn acquire(path: &Path) -> Result<Self, StoreError> {
        use std::path::Component;

        use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
        use rustix::process::getuid;

        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let absolute = normalize_fixed_system_alias(absolute);
        let file_name = match absolute.components().next_back() {
            Some(Component::Normal(name)) => name.to_os_string(),
            _ => return Err(StoreError::UnsafeIndexPath(path.to_path_buf())),
        };
        let parent_path = absolute
            .parent()
            .ok_or_else(|| StoreError::UnsafeIndexPath(path.to_path_buf()))?;

        const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::CLOEXEC)
            .union(OFlags::NOFOLLOW);
        let mut descriptor = open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let mut physical_path = PathBuf::from("/");

        for component in parent_path.components() {
            match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(name) => {
                    let before = statat(&descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|error| unsafe_component_error(path, error))?;
                    if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
                        return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
                    }
                    let next = openat(&descriptor, name, DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|error| unsafe_component_error(path, error))?;
                    let after = fstat(&next).map_err(io::Error::from)?;
                    if !same_stat_identity(&before, &after)
                        || FileType::from_raw_mode(after.st_mode) != FileType::Directory
                    {
                        return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
                    }
                    descriptor = next;
                    physical_path.push(name);
                }
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
                }
            }
        }

        let metadata = fstat(&descriptor).map_err(io::Error::from)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
            || metadata.st_uid != getuid().as_raw()
            || metadata.st_mode & 0o777 != 0o700
        {
            return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
        }

        Ok(Self {
            descriptor,
            identity: stat_identity(&metadata),
            physical_path,
            file_name,
            requested_path: path.to_path_buf(),
        })
    }

    fn path(&self) -> PathBuf {
        self.physical_path.join(&self.file_name)
    }

    fn sidecar_name(&self, suffix: &str) -> OsString {
        let mut name = self.file_name.clone();
        name.push(suffix);
        name
    }

    fn require_sidecars_absent(&self) -> Result<(), StoreError> {
        use rustix::fs::{AtFlags, statat};

        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let name = self.sidecar_name(suffix);
            match statat(&self.descriptor, &name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => {}
                Ok(_) | Err(_) => {
                    return Err(StoreError::UnsafeIndexPath(sqlite_sidecar_path(
                        &self.requested_path,
                        suffix,
                    )));
                }
            }
        }
        Ok(())
    }

    fn open_validated_sidecar(
        &self,
        suffix: &str,
    ) -> Result<Option<(std::os::fd::OwnedFd, FileIdentity)>, StoreError> {
        let name = self.sidecar_name(suffix);
        open_validated_regular_at(
            &self.descriptor,
            &name,
            &sqlite_sidecar_path(&self.requested_path, suffix),
            FileAccess::ReadOnly,
            true,
        )
    }

    fn validate_sidecars(&self) -> Result<[Option<FileIdentity>; 3], StoreError> {
        let mut identities = [None; 3];
        for (slot, suffix) in identities.iter_mut().zip(SQLITE_SIDECAR_SUFFIXES) {
            *slot = self
                .open_validated_sidecar(suffix)?
                .map(|(_, identity)| identity);
        }
        Ok(identities)
    }

    fn revalidate_identity(&self) -> Result<(), StoreError> {
        use rustix::fs::fstat;

        let current = fstat(&self.descriptor).map_err(io::Error::from)?;
        if stat_identity(&current) != self.identity {
            return Err(StoreError::UnsafeIndexPath(self.requested_path.clone()));
        }
        Ok(())
    }
}

#[cfg(unix)]
struct ValidatedIndexPath {
    parent: ValidatedParent,
    descriptor: std::os::fd::OwnedFd,
    identity: FileIdentity,
    access: FileAccess,
    path: PathBuf,
}

#[cfg(unix)]
impl ValidatedIndexPath {
    fn path(&self) -> &Path {
        &self.path
    }

    fn validate_sidecars(&self) -> Result<[Option<FileIdentity>; 3], StoreError> {
        self.parent.validate_sidecars()
    }

    fn revalidate_namespace(&self) -> Result<(), StoreError> {
        use rustix::fs::fstat;

        self.parent.revalidate_identity()?;
        let held = fstat(&self.descriptor).map_err(io::Error::from)?;
        validate_private_regular_stat(&held, &self.parent.requested_path)?;
        if stat_identity(&held) != self.identity {
            return Err(StoreError::UnsafeIndexPath(
                self.parent.requested_path.clone(),
            ));
        }

        let current_parent = ValidatedParent::acquire(&self.path)?;
        if current_parent.identity != self.parent.identity {
            return Err(StoreError::UnsafeIndexPath(
                self.parent.requested_path.clone(),
            ));
        }
        let current = open_validated_regular_at(
            &current_parent.descriptor,
            &current_parent.file_name,
            &self.parent.requested_path,
            self.access,
            false,
        )?
        .ok_or_else(|| StoreError::MissingPath(self.parent.requested_path.clone()))?;
        let held = fstat(&self.descriptor).map_err(std::io::Error::from)?;
        if stat_identity(&held) != current.1 || current.1 != self.identity {
            return Err(StoreError::UnsafeIndexPath(
                self.parent.requested_path.clone(),
            ));
        }
        Ok(())
    }

    fn verify_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        // The pathname proof binds the held inode to the canonical namespace;
        // HAS_MOVED then binds SQLite's live VFS handle to that same entry. The
        // repeated pathname proof detects replacement on either side of the
        // file-control call.
        self.revalidate_namespace()?;
        self.validate_sidecars()?;
        if sqlite_main_handle_has_moved(connection)? {
            return Err(StoreError::UnsafeIndexPath(
                self.parent.requested_path.clone(),
            ));
        }
        self.revalidate_namespace()?;
        self.validate_sidecars()?;
        Ok(())
    }

    fn remove(self) -> Result<(), StoreError> {
        use rustix::fs::fsync;

        self.revalidate_namespace()?;
        let sidecars = self.validate_sidecars()?;
        for (identity, suffix) in sidecars.into_iter().zip(SQLITE_SIDECAR_SUFFIXES) {
            let Some(identity) = identity else {
                continue;
            };
            let name = self.parent.sidecar_name(suffix);
            unlink_validated_regular_at(
                &self.parent,
                &name,
                &sqlite_sidecar_path(&self.parent.requested_path, suffix),
                identity,
                true,
            )?;
        }
        self.revalidate_namespace()?;
        unlink_validated_regular_at(
            &self.parent,
            &self.parent.file_name,
            &self.parent.requested_path,
            self.identity,
            false,
        )?;
        fsync(&self.parent.descriptor).map_err(io::Error::from)?;
        Ok(())
    }
}

#[cfg(unix)]
fn unlink_validated_regular_at(
    parent: &ValidatedParent,
    name: &std::ffi::OsStr,
    reported_path: &Path,
    expected: FileIdentity,
    optional: bool,
) -> Result<(), StoreError> {
    use rustix::fs::{AtFlags, statat, unlinkat};

    let current = match statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(rustix::io::Errno::NOENT) if optional => return Ok(()),
        Err(error) => return Err(unsafe_component_error(reported_path, error)),
    };
    validate_private_regular_stat(&current, reported_path)?;
    if stat_identity(&current) != expected {
        return Err(StoreError::UnsafeIndexPath(reported_path.to_path_buf()));
    }
    unlinkat(&parent.descriptor, name, AtFlags::empty())
        .map_err(|error| unsafe_component_error(reported_path, error))?;
    Ok(())
}

#[cfg(unix)]
fn validate_index_path(path: &Path, access: FileAccess) -> Result<ValidatedIndexPath, StoreError> {
    let parent = ValidatedParent::acquire(path)?;
    let (descriptor, identity) =
        open_validated_regular_at(&parent.descriptor, &parent.file_name, path, access, false)?
            .ok_or_else(|| StoreError::MissingPath(path.to_path_buf()))?;
    let physical_path = parent.path();
    Ok(ValidatedIndexPath {
        parent,
        descriptor,
        identity,
        access,
        path: physical_path,
    })
}

#[cfg(unix)]
fn open_validated_regular_at(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    reported_path: &Path,
    access: FileAccess,
    optional: bool,
) -> Result<Option<(std::os::fd::OwnedFd, FileIdentity)>, StoreError> {
    use rustix::fs::{AtFlags, Mode, OFlags, fstat, openat, statat};

    let before = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) if optional => return Ok(None),
        Err(rustix::io::Errno::NOENT) => {
            return Err(StoreError::MissingPath(reported_path.to_path_buf()));
        }
        Err(error) => return Err(unsafe_component_error(reported_path, error)),
    };
    validate_private_regular_stat(&before, reported_path)?;

    let access_flag = match access {
        FileAccess::ReadOnly => OFlags::RDONLY,
        FileAccess::ReadWrite => OFlags::RDWR,
    };
    let flags = access_flag
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK);
    let descriptor = openat(parent, name, flags, Mode::empty())
        .map_err(|error| unsafe_component_error(reported_path, error))?;
    let after = fstat(&descriptor).map_err(io::Error::from)?;
    validate_private_regular_stat(&after, reported_path)?;
    if !same_stat_identity(&before, &after) {
        return Err(StoreError::UnsafeIndexPath(reported_path.to_path_buf()));
    }
    Ok(Some((descriptor, stat_identity(&after))))
}

#[cfg(unix)]
fn validate_private_regular_stat(
    metadata: &rustix::fs::Stat,
    path: &Path,
) -> Result<(), StoreError> {
    use rustix::fs::FileType;
    use rustix::process::getuid;

    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_nlink != 1
        || metadata.st_uid != getuid().as_raw()
        || metadata.st_mode & 0o777 != 0o600
    {
        return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn stat_identity(metadata: &rustix::fs::Stat) -> FileIdentity {
    // `st_dev` is `i32` on Apple targets and `u64` on Linux, so this cast is
    // required on macOS and redundant on Linux.
    #[allow(clippy::unnecessary_cast)]
    let device = metadata.st_dev as u64;
    FileIdentity {
        device,
        inode: metadata.st_ino,
    }
}

#[cfg(unix)]
fn same_stat_identity(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    stat_identity(left) == stat_identity(right)
}

#[cfg(unix)]
fn unsafe_component_error(path: &Path, error: rustix::io::Errno) -> StoreError {
    if matches!(
        error,
        rustix::io::Errno::LOOP
            | rustix::io::Errno::NOTDIR
            | rustix::io::Errno::NXIO
            | rustix::io::Errno::INVAL
    ) {
        StoreError::UnsafeIndexPath(path.to_path_buf())
    } else if error == rustix::io::Errno::NOENT {
        StoreError::MissingPath(path.to_path_buf())
    } else {
        StoreError::Io(error.into())
    }
}

#[cfg(target_os = "macos")]
fn normalize_fixed_system_alias(path: PathBuf) -> PathBuf {
    for (alias, physical) in [("/var", "/private/var"), ("/tmp", "/private/tmp")] {
        let alias = Path::new(alias);
        let physical = Path::new(physical);
        if let Ok(rest) = path.strip_prefix(alias)
            && std::fs::canonicalize(alias).is_ok_and(|actual| actual == physical)
        {
            return physical.join(rest);
        }
    }
    path
}

#[cfg(all(unix, not(target_os = "macos")))]
fn normalize_fixed_system_alias(path: PathBuf) -> PathBuf {
    path
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn sqlite_main_handle_has_moved(connection: &Connection) -> Result<bool, StoreError> {
    let mut moved: std::ffi::c_int = 0;
    // SAFETY: `connection` owns a live sqlite3 handle for the duration of this
    // call, "main" is a static NUL-terminated schema name, and SQLite documents
    // SQLITE_FCNTL_HAS_MOVED as writing exactly one C `int` to its argument.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            std::ptr::from_mut(&mut moved).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(result),
            None,
        )));
    }
    Ok(moved != 0)
}

#[cfg(unix)]
fn reserve_new_file(parent: ValidatedParent) -> Result<NewFileReservation, StoreError> {
    use rustix::fs::{AtFlags, Mode, OFlags, fstat, openat, statat};

    let flags = OFlags::RDWR
        .union(OFlags::CREATE)
        .union(OFlags::EXCL)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK);
    let mode = Mode::RUSR.union(Mode::WUSR);
    let descriptor = match openat(&parent.descriptor, &parent.file_name, flags, mode) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::EXIST) => {
            return Err(StoreError::AlreadyExists(parent.requested_path.clone()));
        }
        Err(error) => return Err(unsafe_component_error(&parent.requested_path, error)),
    };
    let held = fstat(&descriptor).map_err(io::Error::from)?;
    validate_private_regular_stat(&held, &parent.requested_path)?;
    let named = statat(
        &parent.descriptor,
        &parent.file_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|error| unsafe_component_error(&parent.requested_path, error))?;
    if !same_stat_identity(&held, &named) {
        return Err(StoreError::UnsafeIndexPath(parent.requested_path.clone()));
    }
    let identity = stat_identity(&held);
    let physical_path = parent.path();
    Ok(NewFileReservation {
        validated: Some(ValidatedIndexPath {
            parent,
            descriptor,
            identity,
            access: FileAccess::ReadWrite,
            path: physical_path,
        }),
        sidecars: [None; 3],
        persisted: false,
    })
}

#[cfg(unix)]
struct NewFileReservation {
    validated: Option<ValidatedIndexPath>,
    sidecars: [Option<FileIdentity>; 3],
    persisted: bool,
}

#[cfg(unix)]
impl NewFileReservation {
    fn validated(&self) -> &ValidatedIndexPath {
        self.validated
            .as_ref()
            .expect("reservation retains its validated path until persistence")
    }

    fn path(&self) -> &Path {
        self.validated().path()
    }

    fn require_sidecars_absent(&self) -> Result<(), StoreError> {
        self.validated().parent.require_sidecars_absent()
    }

    fn observe_sidecars(&mut self) -> Result<(), StoreError> {
        let current = self.validated().validate_sidecars()?;
        for (known, observed) in self.sidecars.iter_mut().zip(current) {
            match (*known, observed) {
                (None, Some(identity)) => *known = Some(identity),
                (Some(expected), Some(actual)) if expected != actual => {
                    return Err(StoreError::UnsafeIndexPath(
                        self.validated().parent.requested_path.clone(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn revalidate_namespace(&self) -> Result<(), StoreError> {
        self.validated().revalidate_namespace()
    }

    fn verify_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        self.validated().verify_connection(connection)
    }

    fn persist(mut self) -> ValidatedIndexPath {
        self.persisted = true;
        self.validated
            .take()
            .expect("validated path exists before reservation persistence")
    }
}

#[cfg(unix)]
impl Drop for NewFileReservation {
    fn drop(&mut self) {
        use rustix::fs::{AtFlags, statat, unlinkat};

        if self.persisted {
            return;
        }
        let Some(validated) = self.validated.as_ref() else {
            return;
        };

        for (known, suffix) in self.sidecars.iter().zip(SQLITE_SIDECAR_SUFFIXES) {
            let Some(expected) = known else {
                continue;
            };
            let name = validated.parent.sidecar_name(suffix);
            let Ok(current) = statat(
                &validated.parent.descriptor,
                &name,
                AtFlags::SYMLINK_NOFOLLOW,
            ) else {
                continue;
            };
            if stat_identity(&current) == *expected
                && validate_private_regular_stat(
                    &current,
                    &sqlite_sidecar_path(&validated.parent.requested_path, suffix),
                )
                .is_ok()
            {
                let _ = unlinkat(&validated.parent.descriptor, &name, AtFlags::empty());
            }
        }

        let Ok(current) = statat(
            &validated.parent.descriptor,
            &validated.parent.file_name,
            AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            return;
        };
        if stat_identity(&current) == validated.identity {
            let _ = unlinkat(
                &validated.parent.descriptor,
                &validated.parent.file_name,
                AtFlags::empty(),
            );
        }
    }
}

#[cfg(not(unix))]
struct ValidatedParent {
    path: PathBuf,
}

#[cfg(not(unix))]
impl ValidatedParent {
    fn acquire(path: &Path) -> Result<Self, StoreError> {
        let parent = path
            .parent()
            .ok_or_else(|| StoreError::UnsafeIndexPath(path.to_path_buf()))?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn require_sidecars_absent(&self) -> Result<(), StoreError> {
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            if sqlite_sidecar_path(&self.path, suffix).try_exists()? {
                return Err(StoreError::UnsafeIndexPath(sqlite_sidecar_path(
                    &self.path, suffix,
                )));
            }
        }
        Ok(())
    }
}

#[cfg(not(unix))]
struct ValidatedIndexPath {
    path: PathBuf,
    length: u64,
}

#[cfg(not(unix))]
impl ValidatedIndexPath {
    fn path(&self) -> &Path {
        &self.path
    }

    fn revalidate_namespace(&self) -> Result<(), StoreError> {
        let current = validate_index_path(&self.path, FileAccess::ReadOnly)?;
        if self.length != current.length {
            return Err(StoreError::UnsafeIndexPath(self.path.clone()));
        }
        Ok(())
    }

    fn validate_sidecars(&self) -> Result<(), StoreError> {
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let path = sqlite_sidecar_path(&self.path, suffix);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Ok(_) => return Err(StoreError::UnsafeIndexPath(path)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
        Ok(())
    }

    fn verify_connection(&self, _connection: &Connection) -> Result<(), StoreError> {
        self.revalidate_namespace()?;
        self.validate_sidecars()
    }

    fn remove(self) -> Result<(), StoreError> {
        self.revalidate_namespace()?;
        self.validate_sidecars()?;
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let sidecar = sqlite_sidecar_path(&self.path, suffix);
            match std::fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
        std::fs::remove_file(&self.path)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| StoreError::UnsafeIndexPath(self.path.clone()))?;
        OpenOptions::new().read(true).open(parent)?.sync_all()?;
        Ok(())
    }
}

#[cfg(not(unix))]
fn validate_index_path(path: &Path, _access: FileAccess) -> Result<ValidatedIndexPath, StoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            StoreError::MissingPath(path.to_path_buf())
        } else {
            StoreError::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StoreError::UnsafeIndexPath(path.to_path_buf()));
    }
    Ok(ValidatedIndexPath {
        path: path.to_path_buf(),
        length: metadata.len(),
    })
}

#[cfg(not(unix))]
fn reserve_new_file(parent: ValidatedParent) -> Result<NewFileReservation, StoreError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    match options.open(&parent.path) {
        Ok(_) => {
            let validated = validate_index_path(&parent.path, FileAccess::ReadWrite)?;
            Ok(NewFileReservation {
                validated: Some(validated),
                persisted: false,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(StoreError::AlreadyExists(parent.path))
        }
        Err(error) => Err(StoreError::Io(error)),
    }
}

#[cfg(not(unix))]
struct NewFileReservation {
    validated: Option<ValidatedIndexPath>,
    persisted: bool,
}

#[cfg(not(unix))]
impl NewFileReservation {
    fn validated(&self) -> &ValidatedIndexPath {
        self.validated
            .as_ref()
            .expect("reservation retains its validated path until persistence")
    }

    fn path(&self) -> &Path {
        self.validated().path()
    }

    fn require_sidecars_absent(&self) -> Result<(), StoreError> {
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            if sqlite_sidecar_path(self.path(), suffix).try_exists()? {
                return Err(StoreError::UnsafeIndexPath(sqlite_sidecar_path(
                    self.path(),
                    suffix,
                )));
            }
        }
        Ok(())
    }

    fn observe_sidecars(&mut self) -> Result<(), StoreError> {
        self.validated().validate_sidecars()
    }

    fn revalidate_namespace(&self) -> Result<(), StoreError> {
        self.validated().revalidate_namespace()
    }

    fn verify_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        self.validated().verify_connection(connection)
    }

    fn persist(mut self) -> ValidatedIndexPath {
        self.persisted = true;
        self.validated
            .take()
            .expect("validated path exists before reservation persistence")
    }
}

#[cfg(not(unix))]
impl Drop for NewFileReservation {
    fn drop(&mut self) {
        if self.persisted {
            return;
        }
        let Some(validated) = self.validated.as_ref() else {
            return;
        };
        let _ = std::fs::remove_file(validated.path());
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let _ = std::fs::remove_file(sqlite_sidecar_path(validated.path(), suffix));
        }
    }
}

pub(crate) fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;

    for (config, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true),
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, true),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
        (DbConfig::SQLITE_DBCONFIG_WRITABLE_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_COMMENTS, false),
    ] {
        connection.set_db_config(config, enabled)?;
    }

    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES),
        (Limit::SQLITE_LIMIT_COLUMN, 256),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 32),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 100),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 4096),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 1024),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection.set_limit(limit, value)?;
    }

    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("time formatting failed")]
    Time(#[from] time::error::Format),
    #[error("index path already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("SQLite refused WAL journal mode and selected {0}")]
    WalUnavailable(String),
    #[error("index path does not exist: {0}")]
    MissingPath(PathBuf),
    #[error(
        "index must be a mode-0600, user-owned single-link file in a mode-0700 user-owned directory: {0}"
    )]
    UnsafeIndexPath(PathBuf),
    #[error("SQLite application ID mismatch: expected {expected:#010x}, found {actual:#010x}")]
    InvalidApplicationId { expected: i32, actual: i32 },
    #[error("SQLite schema version is outside the supported unsigned range: {0}")]
    InvalidSchemaVersion(i64),
    #[error("unsupported schema version: this binary supports {current}, index uses {found}")]
    UnsupportedSchemaVersion { current: u32, found: u32 },
    #[error("required schema object is missing: {0}")]
    MissingSchemaObject(String),
    #[error("views and triggers are forbidden in the hSUM index schema")]
    UnexpectedExecutableSchema,
    #[error("invalid index metadata: {0}")]
    InvalidMetadata(&'static str),
    #[error("schema checksum does not match this binary")]
    SchemaChecksumMismatch,
    #[error("live SQLite schema does not match the compiled migration")]
    SchemaManifestMismatch,
    #[error("pipeline fingerprint does not match this binary")]
    PipelineFingerprintMismatch,
    #[error("schema migration chain is invalid")]
    MigrationChainInvalid,
    #[error("SQLite integrity check failed: {0}")]
    IntegrityCheckFailed(String),
    #[error("SQLite foreign-key check found a violation")]
    ForeignKeyCheckFailed,
    #[error("doctor requires a read-only, query-only connection")]
    ReadOnlyRequired,
    #[error("mutation requires a read-write connection")]
    ReadWriteRequired,
    #[error("prepared document is invalid: {0}")]
    InvalidPreparedDocument(&'static str),
    #[error("snapshot contains a duplicate connector key")]
    DuplicateConnectorKey,
    #[error("the filesystem source or project conflicts with the index")]
    ScopeConflict,
    #[error("the configured source name or logical URI conflicts with the index")]
    SourceConflict,
    #[error("the configured source was not found")]
    SourceNotFound,
    #[error("the managed index already contains the maximum of {maximum} active sources")]
    SourceLimitExceeded { maximum: usize },
    #[error("the selected project was not found")]
    ProjectNotFound,
    #[error("the managed index already contains the maximum of {maximum} projects")]
    ProjectLimitExceeded { maximum: usize },
    #[error("the source kind is not supported by this operation")]
    UnsupportedSourceKind,
    #[error("a SHA-256 collision was detected")]
    HashCollision,
    #[error("stored chunk layout does not match the prepared layout")]
    ChunkLayoutMismatch,
    #[error("the prepared revision is protected by a body-free forget tombstone")]
    ForgetTombstone,
    #[error("the body-free forget ledger does not match stored evidence")]
    ForgetLedgerMismatch,
    #[error("empty snapshot requires --allow-empty-snapshot")]
    EmptySnapshotConfirmationRequired,
    #[error("deleting {absent} of {eligible_prior} documents requires --allow-mass-delete")]
    MassDeleteConfirmationRequired {
        absent: usize,
        eligible_prior: usize,
    },
    #[error("integer value exceeds the supported range")]
    IntegerOverflow,
    #[error("active derived index is out of parity: {0}")]
    ActiveIndexParity(&'static str),
    #[error("immutable evidence is out of parity: {0}")]
    ImmutableEvidenceMismatch(&'static str),
    #[error("generation invariant failed: {0}")]
    GenerationInvariant(&'static str),
    #[error("writer lock remained busy for {timeout_ms} ms")]
    WriterLockBusy { timeout_ms: u64 },
    #[error("writer lock does not protect this index")]
    WriterLockMismatch,
    #[error("writer lock sidecar is not a private, user-owned regular file: {0}")]
    UnsafeWriterLock(PathBuf),
    #[error("writer locks are unsupported on this platform")]
    WriterLockUnsupported,
    #[error("evidence replacement lock remained busy for {timeout_ms} ms")]
    ReplacementLockBusy { timeout_ms: u64 },
    #[error("evidence replacement lock does not protect this index")]
    ReplacementLockMismatch,
    #[error("replacement lock sidecar is not a private, user-owned regular file: {0}")]
    UnsafeReplacementLock(PathBuf),
    #[error("evidence replacement locks are unsupported on this platform")]
    ReplacementLockUnsupported,
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::{TempDir, tempdir};

    use super::*;

    fn private_tempdir() -> TempDir {
        let directory = tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        directory
    }

    struct InspectCommittedSchema<'a> {
        called: &'a Cell<bool>,
        fail_after_inspection: bool,
    }

    impl IndexDurability for InspectCommittedSchema<'_> {
        fn sync_created_index(&self, validated: &ValidatedIndexPath) -> Result<(), StoreError> {
            let path = validated.path();
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StoreError::from)?;
            let migration_count: i64 = connection
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })
                .map_err(StoreError::from)?;
            assert_eq!(
                migration_count,
                i64::from(SCHEMA_VERSION),
                "durability runs only after the complete migration chain commits"
            );
            self.called.set(true);
            if self.fail_after_inspection {
                return Err(StoreError::Io(io::Error::other(
                    "injected durability failure",
                )));
            }
            Ok(())
        }
    }

    #[test]
    fn creation_crosses_the_durability_boundary_after_commit() {
        let directory = private_tempdir();
        let path = directory.path().join("index.sqlite");
        let called = Cell::new(false);

        let database = IndexDb::create_with_durability(
            &path,
            IndexId::new_v4(),
            &InspectCommittedSchema {
                called: &called,
                fail_after_inspection: false,
            },
        )
        .unwrap();

        assert!(called.get());
        assert_eq!(database.path(), std::fs::canonicalize(path).unwrap());
    }

    #[test]
    fn failed_durability_does_not_publish_the_reserved_index() {
        let directory = private_tempdir();
        let path = directory.path().join("index.sqlite");
        let called = Cell::new(false);

        let error = IndexDb::create_with_durability(
            &path,
            IndexId::new_v4(),
            &InspectCommittedSchema {
                called: &called,
                fail_after_inspection: true,
            },
        )
        .err()
        .expect("injected durability failure must abort creation");

        assert!(called.get());
        assert!(matches!(error, StoreError::Io(_)));
        assert!(!path.exists());
        assert!(!sqlite_sidecar_path(&path, "-wal").exists());
        assert!(!sqlite_sidecar_path(&path, "-shm").exists());
        assert!(!sqlite_sidecar_path(&path, "-journal").exists());
    }
}
