use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::store::open::StoreError;

#[cfg(unix)]
#[derive(Debug)]
pub struct WriterLock {
    descriptor: std::os::fd::OwnedFd,
    lock_path: PathBuf,
}

/// A shared lease held while evidence from an index may still be delivered.
///
/// Forget and restore acquire the matching [`ReplacementLock`] exclusively
/// before publishing a replacement database. This keeps a response backed by
/// the old inode from crossing the replacement boundary.
#[cfg(unix)]
#[derive(Debug)]
pub struct ReaderLease {
    descriptor: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl ReaderLease {
    pub fn acquire(index_path: &Path, timeout: Duration) -> Result<Self, StoreError> {
        let (descriptor, lock_path) = acquire_replacement_fence(
            index_path,
            timeout,
            rustix::fs::FlockOperation::NonBlockingLockShared,
        )?;
        let _ = lock_path;
        Ok(Self { descriptor })
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        replacement_lock_path(index_path)
    }
}

#[cfg(unix)]
impl Drop for ReaderLease {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.descriptor, rustix::fs::FlockOperation::Unlock);
    }
}

/// The exclusive half of the evidence replacement fence.
#[cfg(unix)]
#[derive(Debug)]
pub struct ReplacementLock {
    descriptor: std::os::fd::OwnedFd,
    lock_path: PathBuf,
}

#[cfg(unix)]
impl ReplacementLock {
    pub fn acquire(index_path: &Path, timeout: Duration) -> Result<Self, StoreError> {
        let (descriptor, lock_path) = acquire_replacement_fence(
            index_path,
            timeout,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )?;
        Ok(Self {
            descriptor,
            lock_path,
        })
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        replacement_lock_path(index_path)
    }

    pub(crate) fn protects(&self, index_path: &Path) -> bool {
        self.lock_path == Self::sidecar_path(index_path)
    }
}

#[cfg(unix)]
impl Drop for ReplacementLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.descriptor, rustix::fs::FlockOperation::Unlock);
    }
}

#[cfg(unix)]
fn acquire_replacement_fence(
    index_path: &Path,
    timeout: Duration,
    operation: rustix::fs::FlockOperation,
) -> Result<(std::os::fd::OwnedFd, PathBuf), StoreError> {
    use rustix::fs::{FileType, Mode, OFlags, flock, fstat, open};
    use rustix::process::getuid;
    use std::thread;
    use std::time::Instant;

    const LOCK_FLAGS: OFlags = OFlags::RDWR
        .union(OFlags::CREATE)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const LOCK_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
    const RETRY_INTERVAL: Duration = Duration::from_millis(10);

    let lock_path = replacement_lock_path(index_path);
    let descriptor = open(&lock_path, LOCK_FLAGS, LOCK_MODE).map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            StoreError::UnsafeReplacementLock(lock_path.clone())
        } else {
            StoreError::Io(error.into())
        }
    })?;
    let metadata = fstat(&descriptor).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_nlink != 1
        || metadata.st_uid != getuid().as_raw()
        || metadata.st_mode & 0o077 != 0
    {
        return Err(StoreError::UnsafeReplacementLock(lock_path));
    }

    let started = Instant::now();
    loop {
        match flock(&descriptor, operation) {
            Ok(()) => return Ok((descriptor, lock_path)),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Err(StoreError::ReplacementLockBusy {
                        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                    });
                }
                thread::sleep(RETRY_INTERVAL.min(timeout.saturating_sub(elapsed)));
            }
            Err(error) => return Err(StoreError::Io(error.into())),
        }
    }
}

#[cfg(unix)]
impl WriterLock {
    pub fn acquire(index_path: &Path, timeout: Duration) -> Result<Self, StoreError> {
        use rustix::fs::{FileType, FlockOperation, Mode, OFlags, flock, fstat, open};
        use rustix::process::getuid;
        use std::thread;
        use std::time::Instant;

        const LOCK_FLAGS: OFlags = OFlags::RDWR
            .union(OFlags::CREATE)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC);
        const LOCK_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
        const RETRY_INTERVAL: Duration = Duration::from_millis(10);

        let lock_path = Self::sidecar_path(index_path);
        let descriptor = open(&lock_path, LOCK_FLAGS, LOCK_MODE).map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                StoreError::UnsafeWriterLock(lock_path.clone())
            } else {
                StoreError::Io(error.into())
            }
        })?;
        let metadata = fstat(&descriptor).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
            || metadata.st_nlink != 1
            || metadata.st_uid != getuid().as_raw()
            || metadata.st_mode & 0o077 != 0
        {
            return Err(StoreError::UnsafeWriterLock(lock_path));
        }

        let started = Instant::now();
        loop {
            match flock(&descriptor, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    return Ok(Self {
                        descriptor,
                        lock_path,
                    });
                }
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                    let elapsed = started.elapsed();
                    if elapsed >= timeout {
                        return Err(StoreError::WriterLockBusy {
                            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                        });
                    }
                    thread::sleep(RETRY_INTERVAL.min(timeout.saturating_sub(elapsed)));
                }
                Err(error) => return Err(StoreError::Io(error.into())),
            }
        }
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        writer_lock_path(index_path)
    }

    pub(crate) fn protects(&self, index_path: &Path) -> bool {
        self.lock_path == Self::sidecar_path(index_path)
    }
}

#[cfg(unix)]
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.descriptor, rustix::fs::FlockOperation::Unlock);
    }
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct WriterLock {
    lock_path: PathBuf,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct ReaderLease {
    _private: (),
}

#[cfg(not(unix))]
impl ReaderLease {
    pub fn acquire(index_path: &Path, _timeout: Duration) -> Result<Self, StoreError> {
        let _ = index_path;
        Err(StoreError::ReplacementLockUnsupported)
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        replacement_lock_path(index_path)
    }
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct ReplacementLock {
    lock_path: PathBuf,
}

#[cfg(not(unix))]
impl ReplacementLock {
    pub fn acquire(index_path: &Path, _timeout: Duration) -> Result<Self, StoreError> {
        let _ = Self {
            lock_path: Self::sidecar_path(index_path),
        };
        Err(StoreError::ReplacementLockUnsupported)
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        replacement_lock_path(index_path)
    }

    pub(crate) fn protects(&self, index_path: &Path) -> bool {
        self.lock_path == Self::sidecar_path(index_path)
    }
}

#[cfg(not(unix))]
impl WriterLock {
    pub fn acquire(index_path: &Path, _timeout: Duration) -> Result<Self, StoreError> {
        let _ = Self {
            lock_path: Self::sidecar_path(index_path),
        };
        Err(StoreError::WriterLockUnsupported)
    }

    pub fn sidecar_path(index_path: &Path) -> PathBuf {
        writer_lock_path(index_path)
    }

    pub(crate) fn protects(&self, index_path: &Path) -> bool {
        self.lock_path == Self::sidecar_path(index_path)
    }
}

fn writer_lock_path(index_path: &Path) -> PathBuf {
    let mut file_name = index_path
        .file_name()
        .map_or_else(|| OsString::from("index.sqlite"), OsString::from);
    file_name.push(".writer.lock");
    index_path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map_or_else(
            || index_path.with_file_name(&file_name),
            |parent| parent.join(&file_name),
        )
}

fn replacement_lock_path(index_path: &Path) -> PathBuf {
    let mut file_name = index_path
        .file_name()
        .map_or_else(|| OsString::from("index.sqlite"), OsString::from);
    file_name.push(".replacement.lock");
    index_path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map_or_else(
            || index_path.with_file_name(&file_name),
            |parent| parent.join(&file_name),
        )
}
