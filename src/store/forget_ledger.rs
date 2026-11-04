use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json_canonicalizer::to_vec as to_canonical_vec;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::{DocumentId, IndexId, Sha256Digest, SourceId};
use crate::store::{StoreError, WriterLock};

const LEDGER_FORMAT: &str = "hsum.forget.v1";
const RECORD_HASH_DOMAIN: &[u8] = b"hsum.forget-ledger-record.v1\0";
const MAX_LEDGER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 4 * 1024;
const REPLACEMENT_EPOCH_FORMAT: &str = "hsum.replacement-epoch.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ForgetOperationState {
    Requested,
    LedgerPrepared,
    ReplacementBuilt,
    OldReadersFenced,
    ReplacementActivated,
    Committed,
    Restored,
}

impl ForgetOperationState {
    fn can_follow(self, prior: Option<Self>) -> bool {
        matches!(
            (prior, self),
            (None, Self::Requested)
                | (None, Self::LedgerPrepared)
                | (Some(Self::Requested), Self::LedgerPrepared)
                | (Some(Self::LedgerPrepared), Self::ReplacementBuilt)
                | (Some(Self::ReplacementBuilt), Self::OldReadersFenced)
                | (Some(Self::OldReadersFenced), Self::ReplacementActivated)
                | (Some(Self::ReplacementActivated), Self::Committed)
                | (Some(Self::Committed), Self::Restored)
        )
    }

    fn suppresses_ingest(self) -> bool {
        self >= Self::LedgerPrepared && self != Self::Restored
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForgetLedgerRecord {
    pub sequence: u64,
    pub operation_id: Uuid,
    pub state: ForgetOperationState,
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub connector_key_sha256: Sha256Digest,
    pub recorded_at: String,
    pub previous_record_sha256: Option<Sha256Digest>,
    pub record_sha256: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ForgetLedgerTarget {
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub connector_key_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgetLedger {
    index_id: IndexId,
    records: Vec<ForgetLedgerRecord>,
}

impl ForgetLedger {
    pub(crate) fn read(index_path: &Path, index_id: IndexId) -> Result<Self, StoreError> {
        let path = Self::sidecar_path(index_path);
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(Self {
                index_id,
                records: Vec::new(),
            });
        };
        parse_ledger(&bytes, index_id)
    }

    pub(crate) fn append(
        index_path: &Path,
        writer_lock: &WriterLock,
        index_id: IndexId,
        operation_id: Uuid,
        state: ForgetOperationState,
        target: ForgetLedgerTarget,
    ) -> Result<ForgetLedgerRecord, StoreError> {
        if !writer_lock.protects(index_path) {
            return Err(StoreError::WriterLockMismatch);
        }

        let ledger = Self::read(index_path, index_id)?;
        let prior_for_operation = ledger.records.iter().rev().find(|record| {
            record.operation_id == operation_id
                && record.source_id == target.source_id
                && record.document_id == target.document_id
                && record.connector_key_sha256 == target.connector_key_sha256
        });
        if !state.can_follow(prior_for_operation.map(|record| record.state)) {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        let sequence = u64::try_from(ledger.records.len())
            .map_err(|_| StoreError::IntegerOverflow)?
            .checked_add(1)
            .ok_or(StoreError::IntegerOverflow)?;
        let recorded_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let previous_record_sha256 = ledger.records.last().map(|record| record.record_sha256);
        let record_sha256 = hash_record(&UnsignedForgetLedgerRecord {
            sequence,
            operation_id,
            state,
            source_id: target.source_id,
            document_id: target.document_id,
            connector_key_sha256: target.connector_key_sha256,
            recorded_at: &recorded_at,
            previous_record_sha256,
        })?;
        let record = ForgetLedgerRecord {
            sequence,
            operation_id,
            state,
            source_id: target.source_id,
            document_id: target.document_id,
            connector_key_sha256: target.connector_key_sha256,
            recorded_at,
            previous_record_sha256,
            record_sha256,
        };

        append_record(index_path, index_id, ledger.records.is_empty(), &record)?;
        Ok(record)
    }

    pub(crate) fn suppresses(
        &self,
        source_id: SourceId,
        connector_key_sha256: Sha256Digest,
    ) -> bool {
        let mut latest =
            BTreeMap::<(Uuid, SourceId, DocumentId, Sha256Digest), &ForgetLedgerRecord>::new();
        for record in &self.records {
            latest.insert(
                (
                    record.operation_id,
                    record.source_id,
                    record.document_id,
                    record.connector_key_sha256,
                ),
                record,
            );
        }
        latest.values().any(|record| {
            record.state.suppresses_ingest()
                && record.source_id == source_id
                && record.connector_key_sha256 == connector_key_sha256
        })
    }

    pub(crate) fn suppresses_document(&self, source_id: SourceId, document_id: DocumentId) -> bool {
        let mut latest =
            BTreeMap::<(Uuid, SourceId, DocumentId, Sha256Digest), &ForgetLedgerRecord>::new();
        for record in &self.records {
            latest.insert(
                (
                    record.operation_id,
                    record.source_id,
                    record.document_id,
                    record.connector_key_sha256,
                ),
                record,
            );
        }
        latest.values().any(|record| {
            record.state.suppresses_ingest()
                && record.source_id == source_id
                && record.document_id == document_id
        })
    }

    pub(crate) fn state_for(
        &self,
        operation_id: Uuid,
        source_id: SourceId,
        document_id: DocumentId,
        connector_key_sha256: Sha256Digest,
    ) -> Option<ForgetOperationState> {
        self.records
            .iter()
            .rev()
            .find(|record| {
                record.operation_id == operation_id
                    && record.source_id == source_id
                    && record.document_id == document_id
                    && record.connector_key_sha256 == connector_key_sha256
            })
            .map(|record| record.state)
    }

    pub(crate) fn records(&self) -> &[ForgetLedgerRecord] {
        &self.records
    }

    pub(crate) fn sidecar_path(index_path: &Path) -> PathBuf {
        sidecar_path(index_path, ".forget.jsonl")
    }
}

pub(crate) struct ReplacementEpoch;

impl ReplacementEpoch {
    pub(crate) fn read(
        index_path: &Path,
        expected_index_id: IndexId,
    ) -> Result<Option<u64>, StoreError> {
        let path = Self::sidecar_path(index_path);
        let Some(bytes) = read_optional_private_file_bounded(&path, MAX_RECORD_BYTES as u64)?
        else {
            return Ok(None);
        };
        let value: ReplacementEpochFile =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::ForgetLedgerMismatch)?;
        if value.format != REPLACEMENT_EPOCH_FORMAT || value.index_id != expected_index_id {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        Ok(Some(value.epoch))
    }

    pub(crate) fn publish(
        index_path: &Path,
        index_id: IndexId,
        expected_epoch: u64,
        next_epoch: u64,
    ) -> Result<(), StoreError> {
        if next_epoch
            != expected_epoch
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?
        {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        match Self::read(index_path, index_id)? {
            Some(epoch) if epoch != expected_epoch => {
                return Err(StoreError::ForgetLedgerMismatch);
            }
            None if expected_epoch != 0 => return Err(StoreError::ForgetLedgerMismatch),
            _ => {}
        }

        let path = Self::sidecar_path(index_path);
        let temporary = sidecar_path(
            index_path,
            &format!(".replacement-epoch.tmp-{}", Uuid::new_v4()),
        );
        let encoded = to_canonical_vec(&ReplacementEpochFile {
            format: REPLACEMENT_EPOCH_FORMAT.to_owned(),
            index_id,
            epoch: next_epoch,
        })
        .map_err(|_| StoreError::ForgetLedgerMismatch)?;
        let mut file = create_private_file(&temporary)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(StoreError::Io(error));
        }
        sync_parent(&path)?;
        Ok(())
    }

    pub(crate) fn sidecar_path(index_path: &Path) -> PathBuf {
        sidecar_path(index_path, ".replacement-epoch.json")
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ForgetLedgerHeader {
    format: String,
    index_id: IndexId,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplacementEpochFile {
    format: String,
    index_id: IndexId,
    epoch: u64,
}

#[derive(Serialize)]
struct UnsignedForgetLedgerRecord<'a> {
    sequence: u64,
    operation_id: Uuid,
    state: ForgetOperationState,
    source_id: SourceId,
    document_id: DocumentId,
    connector_key_sha256: Sha256Digest,
    recorded_at: &'a str,
    previous_record_sha256: Option<Sha256Digest>,
}

fn parse_ledger(bytes: &[u8], expected_index_id: IndexId) -> Result<ForgetLedger, StoreError> {
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    let complete = match bytes.last() {
        Some(b'\n') => bytes,
        Some(_) => bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&[][..], |position| &bytes[..position]),
        None => return Err(StoreError::ForgetLedgerMismatch),
    };
    let mut lines = complete.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.iter().any(|line| line.is_empty()) {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    let header_line = lines.first().ok_or(StoreError::ForgetLedgerMismatch)?;
    if header_line.is_empty() || header_line.len() > MAX_RECORD_BYTES {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    let header: ForgetLedgerHeader =
        serde_json::from_slice(header_line).map_err(|_| StoreError::ForgetLedgerMismatch)?;
    if header.format != LEDGER_FORMAT || header.index_id != expected_index_id {
        return Err(StoreError::ForgetLedgerMismatch);
    }

    let mut records = Vec::new();
    let mut operation_states =
        BTreeMap::<(Uuid, SourceId, DocumentId, Sha256Digest), ForgetOperationState>::new();
    let mut previous_hash = None;
    for line in lines.into_iter().skip(1) {
        if line.len() > MAX_RECORD_BYTES {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        let record: ForgetLedgerRecord =
            serde_json::from_slice(line).map_err(|_| StoreError::ForgetLedgerMismatch)?;
        let expected_sequence = u64::try_from(records.len())
            .map_err(|_| StoreError::IntegerOverflow)?
            .checked_add(1)
            .ok_or(StoreError::IntegerOverflow)?;
        if record.sequence != expected_sequence || record.previous_record_sha256 != previous_hash {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        let expected_hash = hash_record(&UnsignedForgetLedgerRecord {
            sequence: record.sequence,
            operation_id: record.operation_id,
            state: record.state,
            source_id: record.source_id,
            document_id: record.document_id,
            connector_key_sha256: record.connector_key_sha256,
            recorded_at: &record.recorded_at,
            previous_record_sha256: record.previous_record_sha256,
        })?;
        let target = (
            record.operation_id,
            record.source_id,
            record.document_id,
            record.connector_key_sha256,
        );
        if record.record_sha256 != expected_hash
            || !record
                .state
                .can_follow(operation_states.get(&target).copied())
        {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        operation_states.insert(target, record.state);
        previous_hash = Some(record.record_sha256);
        records.push(record);
    }

    Ok(ForgetLedger {
        index_id: header.index_id,
        records,
    })
}

fn append_record(
    index_path: &Path,
    index_id: IndexId,
    initialize: bool,
    record: &ForgetLedgerRecord,
) -> Result<(), StoreError> {
    let path = ForgetLedger::sidecar_path(index_path);
    let mut file = open_private_append_file(&path)?;
    let current_len = file.metadata()?.len();
    if initialize {
        if current_len != 0 {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        let header = to_canonical_vec(&ForgetLedgerHeader {
            format: LEDGER_FORMAT.to_owned(),
            index_id,
        })
        .map_err(|_| StoreError::ForgetLedgerMismatch)?;
        write_bounded_line(&mut file, &header)?;
    } else if current_len == 0 {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    let encoded = to_canonical_vec(record).map_err(|_| StoreError::ForgetLedgerMismatch)?;
    write_bounded_line(&mut file, &encoded)?;
    if file.metadata()?.len() > MAX_LEDGER_BYTES {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    file.sync_all()?;
    sync_parent(&path)?;
    Ok(())
}

fn write_bounded_line(file: &mut File, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn hash_record(record: &UnsignedForgetLedgerRecord<'_>) -> Result<Sha256Digest, StoreError> {
    let canonical = to_canonical_vec(record).map_err(|_| StoreError::ForgetLedgerMismatch)?;
    let mut hasher = Sha256::new();
    hasher.update(RECORD_HASH_DOMAIN);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn read_optional_private_file(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    read_optional_private_file_bounded(path, MAX_LEDGER_BYTES)
}

fn read_optional_private_file_bounded(
    path: &Path,
    maximum_bytes: u64,
) -> Result<Option<Vec<u8>>, StoreError> {
    #[cfg(unix)]
    {
        use rustix::fs::{FileType, OFlags, flock, fstat, open};
        use rustix::process::getuid;

        let descriptor = match open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP) => return Err(StoreError::ForgetLedgerMismatch),
            Err(error) => return Err(StoreError::Io(error.into())),
        };
        let metadata = fstat(&descriptor).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
            || metadata.st_nlink != 1
            || metadata.st_uid != getuid().as_raw()
            || metadata.st_mode & 0o077 != 0
            || metadata.st_size < 0
            || metadata.st_size as u64 > maximum_bytes
        {
            return Err(StoreError::ForgetLedgerMismatch);
        }
        flock(&descriptor, rustix::fs::FlockOperation::LockShared).map_err(std::io::Error::from)?;
        let mut file = File::from(descriptor);
        let mut bytes = Vec::with_capacity(metadata.st_size as usize);
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(StoreError::ReplacementLockUnsupported)
    }
}

fn create_private_file(path: &Path) -> Result<File, StoreError> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open};

        let descriptor = open(
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| StoreError::Io(error.into()))?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(StoreError::ReplacementLockUnsupported)
    }
}

fn open_private_append_file(path: &Path) -> Result<File, StoreError> {
    #[cfg(unix)]
    {
        use rustix::fs::{FileType, Mode, OFlags, flock, fstat, open};
        use rustix::process::getuid;

        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let descriptor = open(path, flags, Mode::RUSR | Mode::WUSR).map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                StoreError::ForgetLedgerMismatch
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
            return Err(StoreError::ForgetLedgerMismatch);
        }
        flock(&descriptor, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::from)?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(StoreError::ReplacementLockUnsupported)
    }
}

fn sync_parent(path: &Path) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Io(std::io::Error::other("forget ledger has no parent")))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn sidecar_path(index_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = index_path
        .file_name()
        .map_or_else(|| OsString::from("index.sqlite"), OsString::from);
    file_name.push(suffix);
    index_path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map_or_else(
            || index_path.with_file_name(&file_name),
            |parent| parent.join(&file_name),
        )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, IndexId, WriterLock) {
        let directory = tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let index_path = directory.path().join("index.sqlite");
        let index_id = IndexId::new_v4();
        let writer_lock = WriterLock::acquire(&index_path, Duration::ZERO).unwrap();
        (directory, index_path, index_id, writer_lock)
    }

    #[test]
    fn prepared_record_suppresses_by_source_and_connector_hash() {
        let (_directory, index_path, index_id, writer_lock) = fixture();
        let source_id = SourceId::new_v4();
        let connector_hash = Sha256Digest::of_bytes(b"notes.md");

        ForgetLedger::append(
            &index_path,
            &writer_lock,
            index_id,
            Uuid::new_v4(),
            ForgetOperationState::LedgerPrepared,
            ForgetLedgerTarget {
                source_id,
                document_id: DocumentId::new_v4(),
                connector_key_sha256: connector_hash,
            },
        )
        .unwrap();

        let ledger = ForgetLedger::read(&index_path, index_id).unwrap();
        assert!(ledger.suppresses(source_id, connector_hash));
        assert!(!ledger.suppresses(source_id, Sha256Digest::of_bytes(b"other.md")));
    }

    #[test]
    fn missing_final_newline_discards_only_the_trailing_record() {
        let (_directory, index_path, index_id, writer_lock) = fixture();
        let operation_id = Uuid::new_v4();
        let source_id = SourceId::new_v4();
        let document_id = DocumentId::new_v4();
        let connector_hash = Sha256Digest::of_bytes(b"notes.md");
        ForgetLedger::append(
            &index_path,
            &writer_lock,
            index_id,
            operation_id,
            ForgetOperationState::LedgerPrepared,
            ForgetLedgerTarget {
                source_id,
                document_id,
                connector_key_sha256: connector_hash,
            },
        )
        .unwrap();
        let path = ForgetLedger::sidecar_path(&index_path);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(br#"{"sequence":2"#);
        fs::write(&path, bytes).unwrap();

        let ledger = ForgetLedger::read(&index_path, index_id).unwrap();
        assert_eq!(ledger.records().len(), 1);
        assert!(ledger.suppresses(source_id, connector_hash));
    }

    #[test]
    fn interior_hash_corruption_fails_closed() {
        let (_directory, index_path, index_id, writer_lock) = fixture();
        ForgetLedger::append(
            &index_path,
            &writer_lock,
            index_id,
            Uuid::new_v4(),
            ForgetOperationState::LedgerPrepared,
            ForgetLedgerTarget {
                source_id: SourceId::new_v4(),
                document_id: DocumentId::new_v4(),
                connector_key_sha256: Sha256Digest::of_bytes(b"notes.md"),
            },
        )
        .unwrap();
        let path = ForgetLedger::sidecar_path(&index_path);
        let mut bytes = fs::read(&path).unwrap();
        let position = bytes
            .windows(b"ledger_prepared".len())
            .position(|window| window == b"ledger_prepared")
            .unwrap();
        bytes[position] = b'X';
        fs::write(path, bytes).unwrap();

        assert!(matches!(
            ForgetLedger::read(&index_path, index_id),
            Err(StoreError::ForgetLedgerMismatch)
        ));
    }
}
