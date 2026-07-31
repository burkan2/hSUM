#[cfg(unix)]
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::io::Read;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum BoundedReadError {
    NotFound,
    Unsafe,
    TooLarge,
    Changed,
    Io(io::Error),
}

#[cfg(unix)]
pub(crate) fn read_bounded_file(
    path: &Path,
    max_bytes: usize,
    forbidden_mode_bits: u32,
) -> Result<Vec<u8>, BoundedReadError> {
    use rustix::fs::{Mode, open};

    let descriptor = match open(path, file_flags(), Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(error) if error == rustix::io::Errno::NOENT => {
            return Err(BoundedReadError::NotFound);
        }
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NXIO) => {
            return Err(BoundedReadError::Unsafe);
        }
        Err(error) => return Err(BoundedReadError::Io(error.into())),
    };
    read_descriptor(descriptor, max_bytes, forbidden_mode_bits)
}

#[cfg(unix)]
pub(crate) fn read_bounded_at(
    directory_path: &Path,
    file_name: &Path,
    max_bytes: usize,
    forbidden_mode_bits: u32,
) -> Result<Vec<u8>, BoundedReadError> {
    use rustix::fs::{Mode, OFlags, open, openat};

    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    let directory = open(directory_path, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            BoundedReadError::Unsafe
        } else {
            BoundedReadError::Io(error.into())
        }
    })?;
    let descriptor = match openat(&directory, file_name, file_flags(), Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(error) if error == rustix::io::Errno::NOENT => {
            return Err(BoundedReadError::NotFound);
        }
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NXIO) => {
            return Err(BoundedReadError::Unsafe);
        }
        Err(error) => return Err(BoundedReadError::Io(error.into())),
    };
    read_descriptor(descriptor, max_bytes, forbidden_mode_bits)
}

#[cfg(unix)]
fn file_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;

    OFlags::RDONLY
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC)
}

#[cfg(unix)]
fn read_descriptor(
    descriptor: std::os::fd::OwnedFd,
    max_bytes: usize,
    forbidden_mode_bits: u32,
) -> Result<Vec<u8>, BoundedReadError> {
    use rustix::fs::{FileType, fstat};
    use rustix::process::getuid;
    use std::os::fd::AsFd;

    let before = fstat(&descriptor).map_err(|error| BoundedReadError::Io(error.into()))?;
    // `st_mode` is `u16` on Apple targets and `u32` on Linux, so this conversion
    // is required on macOS and redundant on Linux.
    #[allow(clippy::useless_conversion)]
    let mode = u32::from(before.st_mode);
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_nlink != 1
        || before.st_uid != getuid().as_raw()
        || mode & forbidden_mode_bits != 0
    {
        return Err(BoundedReadError::Unsafe);
    }
    let expected_size = usize::try_from(before.st_size).map_err(|_| BoundedReadError::TooLarge)?;
    if expected_size > max_bytes {
        return Err(BoundedReadError::TooLarge);
    }

    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(expected_size);
    (&mut file)
        .take(
            u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(BoundedReadError::Io)?;
    if bytes.len() > max_bytes {
        return Err(BoundedReadError::TooLarge);
    }
    let after = fstat(file.as_fd()).map_err(|error| BoundedReadError::Io(error.into()))?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_mode != after.st_mode
        || before.st_nlink != after.st_nlink
        || before.st_uid != after.st_uid
        || before.st_size != after.st_size
        || before.st_mtime != after.st_mtime
        || before.st_mtime_nsec != after.st_mtime_nsec
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
        || bytes.len() != expected_size
    {
        return Err(BoundedReadError::Changed);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
pub(crate) fn read_bounded_file(
    path: &Path,
    max_bytes: usize,
    _forbidden_mode_bits: u32,
) -> Result<Vec<u8>, BoundedReadError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            BoundedReadError::NotFound
        } else {
            BoundedReadError::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(BoundedReadError::Unsafe);
    }
    let length = usize::try_from(metadata.len()).map_err(|_| BoundedReadError::TooLarge)?;
    if length > max_bytes {
        return Err(BoundedReadError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(BoundedReadError::Io)?;
    if bytes.len() != length {
        return Err(BoundedReadError::Changed);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
pub(crate) fn read_bounded_at(
    directory_path: &Path,
    file_name: &Path,
    max_bytes: usize,
    forbidden_mode_bits: u32,
) -> Result<Vec<u8>, BoundedReadError> {
    read_bounded_file(
        &directory_path.join(file_name),
        max_bytes,
        forbidden_mode_bits,
    )
}
