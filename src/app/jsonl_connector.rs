use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use thiserror::Error;

use crate::domain::SourceId;
use crate::ingest::{
    ChunkError, ChunkKind, ChunkSettings, JsonlRecord, JsonlSnapshotError, QuoteBloom,
    SnapshotRevision, body_sha256, chunk_bytes, parse_jsonl_snapshot, revision_sha256,
};
use crate::store::{
    DeleteConfirmations, IndexDb, IngestOutcome, JsonlBatchSource, JsonlScope, PreparedChunk,
    PreparedDocument, StoragePreflight, StoragePreflightError, StoreError, WriterLock,
    chunker_fingerprint, prepare_passage_literals,
};

use super::{JsonlSourceConfig, JsonlSourceConfigError};

pub const MAX_JSONL_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const SOURCE_FAILURE_CODE: &str = "SOURCE_JSONL_INVALID";
const MAX_SOURCE_FAILURE_DETAIL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedJsonlSnapshot {
    pub documents: Vec<PreparedDocument>,
    pub explicit_deletions: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub struct JsonlIngestTarget<'a> {
    pub scope: &'a JsonlScope,
    pub config: &'a JsonlSourceConfig,
}

impl<'a> JsonlIngestTarget<'a> {
    pub const fn new(scope: &'a JsonlScope, config: &'a JsonlSourceConfig) -> Self {
        Self { scope, config }
    }
}

pub fn prepare_jsonl_snapshot(input: &[u8]) -> Result<PreparedJsonlSnapshot, JsonlPrepareError> {
    let parsed = parse_jsonl_snapshot(input)?;
    let mut documents = parsed
        .records()
        .iter()
        .map(prepare_record)
        .collect::<Result<Vec<_>, _>>()?;
    documents.sort_by(|left, right| left.connector_key.cmp(&right.connector_key));
    let mut explicit_deletions = parsed
        .deletions()
        .iter()
        .map(|deletion| deletion.id().as_bytes().to_vec())
        .collect::<Vec<_>>();
    explicit_deletions.sort();
    Ok(PreparedJsonlSnapshot {
        documents,
        explicit_deletions,
    })
}

pub fn ingest_jsonl_with_timeout(
    database: &mut IndexDb,
    scope: &JsonlScope,
    config: &JsonlSourceConfig,
    strict: bool,
    confirmations: DeleteConfirmations,
    lock_timeout: Duration,
) -> Result<IngestOutcome, JsonlFileIngestError> {
    validate_authority(scope, config)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    StoragePreflight::run(
        database.path(),
        super::FAILURE_RECORD_ESTIMATED_WRITE_BYTES,
        config.index_quota_bytes(),
    )?;
    let snapshot = match read_prepared_snapshot(config) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let detail = bounded_failure_detail(&error);
            record_default_failure(database, &writer_lock, scope, strict, &detail)?;
            return Err(error);
        }
    };
    let estimated_write_bytes = conservative_snapshot_write_bytes(&snapshot)?;
    let storage_preflight = StoragePreflight::run(
        database.path(),
        estimated_write_bytes,
        config.index_quota_bytes(),
    )?;
    let mut outcome = database
        .apply_jsonl_snapshot_under_lock(
            &writer_lock,
            scope,
            &snapshot.documents,
            &snapshot.explicit_deletions,
            confirmations,
        )
        .map_err(JsonlFileIngestError::from)?;
    outcome.storage_preflight = Some(storage_preflight);
    Ok(outcome)
}

pub fn ingest_jsonl_sources_with_timeout(
    database: &mut IndexDb,
    targets: &[JsonlIngestTarget<'_>],
    strict: bool,
    confirmations: DeleteConfirmations,
    lock_timeout: Duration,
) -> Result<IngestOutcome, JsonlBatchIngestError> {
    let mut ordered = targets.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|target| *target.scope.source_id.as_uuid().as_bytes());
    let index_quota_bytes = ordered
        .first()
        .and_then(|target| target.config.index_quota_bytes());
    for target in &ordered {
        validate_authority(target.scope, target.config).map_err(|source| {
            JsonlBatchIngestError::Source {
                source_id: target.scope.source_id,
                source,
            }
        })?;
        if target.config.index_quota_bytes() != index_quota_bytes {
            return Err(JsonlBatchIngestError::InconsistentIndexQuota);
        }
    }

    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    let failure_budget = super::FAILURE_RECORD_ESTIMATED_WRITE_BYTES
        .checked_mul(u64::try_from(targets.len()).map_err(|_| StoreError::IntegerOverflow)?)
        .ok_or(StoreError::IntegerOverflow)?;
    StoragePreflight::run(database.path(), failure_budget, index_quota_bytes)?;
    let prepared = ordered
        .into_iter()
        .map(|target| (target, read_prepared_snapshot(target.config)))
        .collect::<Vec<_>>();

    let mut prepared = prepared;
    if strict && let Some(index) = prepared.iter().position(|(_, result)| result.is_err()) {
        let (target, result) = prepared.remove(index);
        return Err(JsonlBatchIngestError::StrictSource {
            source_id: target.scope.source_id,
            source: result.expect_err("the strict failure position contains an error"),
        });
    }

    let mut estimated_write_bytes = 0_u64;
    for (target, snapshot) in &prepared {
        if let Ok(snapshot) = snapshot {
            let plan = database.plan_jsonl_snapshot_under_lock(
                &writer_lock,
                target.scope,
                &snapshot.documents,
                &snapshot.explicit_deletions,
            )?;
            estimated_write_bytes = estimated_write_bytes
                .checked_add(plan.estimated_write_bytes)
                .ok_or(StoreError::IntegerOverflow)?;
        }
    }
    estimated_write_bytes = estimated_write_bytes
        .checked_add(failure_budget)
        .ok_or(StoreError::IntegerOverflow)?;
    let storage_preflight =
        StoragePreflight::run(database.path(), estimated_write_bytes, index_quota_bytes)?;

    let failure_details = prepared
        .iter()
        .map(|(_, snapshot)| snapshot.as_ref().err().map(bounded_failure_detail))
        .collect::<Vec<_>>();
    let batch = prepared
        .iter()
        .zip(&failure_details)
        .map(|((target, snapshot), detail)| match snapshot {
            Ok(snapshot) => JsonlBatchSource::snapshot(
                target.scope,
                &snapshot.documents,
                &snapshot.explicit_deletions,
            ),
            Err(_) => JsonlBatchSource::failed(
                target.scope,
                SOURCE_FAILURE_CODE,
                detail
                    .as_deref()
                    .expect("a failed JSONL snapshot has a rendered diagnostic"),
            ),
        })
        .collect::<Vec<_>>();
    let mut outcome = database
        .apply_jsonl_batch_under_lock(&writer_lock, &batch, confirmations)
        .map_err(JsonlBatchIngestError::from)?;
    outcome.storage_preflight = Some(storage_preflight);
    Ok(outcome)
}

pub(super) fn read_prepared_snapshot(
    config: &JsonlSourceConfig,
) -> Result<PreparedJsonlSnapshot, JsonlFileIngestError> {
    let bytes = read_complete_snapshot(config.path())?;
    prepare_jsonl_snapshot(&bytes).map_err(Into::into)
}

pub(super) fn bounded_failure_detail(error: &JsonlFileIngestError) -> String {
    let mut detail = error.to_string();
    if detail.len() <= MAX_SOURCE_FAILURE_DETAIL_BYTES {
        return detail;
    }
    let mut end = MAX_SOURCE_FAILURE_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail.truncate(end);
    detail
}

fn conservative_snapshot_write_bytes(
    snapshot: &PreparedJsonlSnapshot,
) -> Result<u64, JsonlFileIngestError> {
    let document_bytes = snapshot
        .documents
        .iter()
        .try_fold(0_u64, |total, document| {
            let body_bytes = u64::try_from(document.body.len())
                .map_err(|_| StoreError::IntegerOverflow)?
                .checked_mul(6)
                .ok_or(StoreError::IntegerOverflow)?;
            total
                .checked_add(body_bytes.max(super::FAILURE_RECORD_ESTIMATED_WRITE_BYTES))
                .ok_or(StoreError::IntegerOverflow)
        })?;
    let deletion_bytes = u64::try_from(snapshot.explicit_deletions.len())
        .map_err(|_| StoreError::IntegerOverflow)?
        .checked_mul(super::FAILURE_RECORD_ESTIMATED_WRITE_BYTES)
        .ok_or(StoreError::IntegerOverflow)?;
    document_bytes
        .checked_add(deletion_bytes)
        .ok_or(StoreError::IntegerOverflow)
        .map_err(Into::into)
}

fn record_default_failure(
    database: &mut IndexDb,
    writer_lock: &WriterLock,
    scope: &JsonlScope,
    strict: bool,
    detail: &str,
) -> Result<(), JsonlFileIngestError> {
    if strict {
        return Ok(());
    }
    database.record_jsonl_source_failure_under_lock(
        writer_lock,
        scope,
        SOURCE_FAILURE_CODE,
        detail,
    )?;
    Ok(())
}

fn validate_authority(
    scope: &JsonlScope,
    config: &JsonlSourceConfig,
) -> Result<(), JsonlFileIngestError> {
    let canonical = config.to_canonical_json()?;
    let logical_path = config
        .path()
        .to_str()
        .ok_or(JsonlFileIngestError::SourceAuthorityMismatch)?;
    if canonical != scope.source_config_json || logical_path != scope.source_logical_uri {
        return Err(JsonlFileIngestError::SourceAuthorityMismatch);
    }
    Ok(())
}

fn prepare_record(record: &JsonlRecord) -> Result<PreparedDocument, JsonlPrepareError> {
    let body = record.content().as_bytes().to_vec();
    let title = record.title().unwrap_or_default().to_owned();
    let revision = revision_sha256(&SnapshotRevision {
        body: &body,
        source_uri: record.source_uri(),
        title: &title,
        metadata: record.metadata(),
        source_updated_at: record.updated_at(),
    })?;
    let chunks = chunk_bytes(&body, ChunkKind::PlainText, ChunkSettings::default())?
        .into_iter()
        .map(|chunk| prepared_chunk(&title, record.source_uri(), chunk))
        .collect();
    Ok(PreparedDocument {
        connector_key: record.id().as_bytes().to_vec(),
        source_uri: record.source_uri().to_owned(),
        title,
        metadata_json: record.metadata_json().to_owned(),
        source_updated_at: record.updated_at().map(str::to_owned),
        body_sha256: body_sha256(&body),
        revision_sha256: revision,
        chunker_fingerprint: chunker_fingerprint(ChunkKind::PlainText),
        chunks,
        body,
    })
}

fn prepared_chunk(title: &str, source_uri: &str, chunk: crate::ingest::Chunk) -> PreparedChunk {
    let content = chunk.text().as_bytes();
    PreparedChunk {
        ordinal: chunk.ordinal(),
        byte_span: chunk.span(),
        line_span: chunk.line_span(),
        body_text: chunk.text().to_owned(),
        content_sha256: body_sha256(content),
        quote_bloom: QuoteBloom::from_content(content).into_bytes(),
        literals: prepare_passage_literals(title, source_uri, content),
    }
}

fn read_complete_snapshot(path: &Path) -> Result<Vec<u8>, JsonlFileIngestError> {
    for attempt in 0..2 {
        match read_snapshot_once(path) {
            Err(JsonlFileIngestError::ChangedDuringRead) if attempt == 0 => {}
            result => return result,
        }
    }
    unreachable!("the second JSONL read attempt always returns")
}

#[cfg(unix)]
fn read_snapshot_once(path: &Path) -> Result<Vec<u8>, JsonlFileIngestError> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, open};

    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| JsonlFileIngestError::Open(io::Error::from(source)))?;
    let before =
        fstat(&descriptor).map_err(|source| JsonlFileIngestError::Read(io::Error::from(source)))?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(JsonlFileIngestError::NotRegularFile);
    }
    let declared_size =
        u64::try_from(before.st_size).map_err(|_| JsonlFileIngestError::TooLarge)?;
    if declared_size > MAX_JSONL_SNAPSHOT_BYTES {
        return Err(JsonlFileIngestError::TooLarge);
    }

    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(
        usize::try_from(declared_size.min(16 * 1024 * 1024))
            .expect("the bounded initial JSONL allocation fits usize"),
    );
    file.by_ref()
        .take(MAX_JSONL_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(JsonlFileIngestError::Read)?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|size| size > MAX_JSONL_SNAPSHOT_BYTES)
    {
        return Err(JsonlFileIngestError::TooLarge);
    }
    let after =
        fstat(&file).map_err(|source| JsonlFileIngestError::Read(io::Error::from(source)))?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
        || before.st_mtime != after.st_mtime
        || before.st_mtime_nsec != after.st_mtime_nsec
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
    {
        return Err(JsonlFileIngestError::ChangedDuringRead);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_snapshot_once(_path: &Path) -> Result<Vec<u8>, JsonlFileIngestError> {
    Err(JsonlFileIngestError::UnsupportedPlatform)
}

#[derive(Debug, Error)]
pub enum JsonlFileIngestError {
    #[error(transparent)]
    Prepare(#[from] JsonlPrepareError),
    #[error(transparent)]
    Config(#[from] JsonlSourceConfigError),
    #[error("JSONL source authority does not match its stored configuration")]
    SourceAuthorityMismatch,
    #[error("JSONL snapshot is supported only on macOS and Linux")]
    UnsupportedPlatform,
    #[error("JSONL snapshot could not be opened")]
    Open(#[source] io::Error),
    #[error("JSONL snapshot could not be read")]
    Read(#[source] io::Error),
    #[error("JSONL snapshot path is not a regular file")]
    NotRegularFile,
    #[error("JSONL snapshot exceeds the 2 GiB source ceiling")]
    TooLarge,
    #[error("JSONL snapshot changed while it was read")]
    ChangedDuringRead,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
}

#[derive(Debug, Error)]
pub enum JsonlBatchIngestError {
    #[error("strict JSONL ingest rejected source {source_id} before applying any source")]
    StrictSource {
        source_id: SourceId,
        #[source]
        source: JsonlFileIngestError,
    },
    #[error("JSONL source {source_id} has invalid configured authority")]
    Source {
        source_id: SourceId,
        #[source]
        source: JsonlFileIngestError,
    },
    #[error("JSONL sources in one index must use the same index quota")]
    InconsistentIndexQuota,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
    #[error("SQLite JSONL batch accounting failed")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum JsonlPrepareError {
    #[error(transparent)]
    Snapshot(#[from] JsonlSnapshotError),
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    #[error(transparent)]
    Revision(#[from] crate::ingest::RevisionHashError),
}
