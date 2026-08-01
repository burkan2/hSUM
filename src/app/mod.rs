use std::collections::BTreeMap;
use std::io;
use std::path::Path;
#[cfg(test)]
use std::time::Duration;

use serde_json::json;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::ingest::{
    ChunkError, ChunkKind, ChunkSettings, DiscoveryError, DiscoveryOptions, FileIssueKind,
    FilesystemDiscoveryEstimate, FilesystemSpool, QuoteBloom, RevisionHashError, SnapshotRevision,
    SourceTimestamp, body_sha256, chunk_bytes, discover_files, discover_files_spooled_bounded,
    estimate_filesystem_discovery, repo_uri, revision_sha256,
};
#[cfg(test)]
use crate::store::{
    DEFAULT_WRITER_LOCK_TIMEOUT, DeleteConfirmations, FilesystemScope, IngestOutcome, IngestPlan,
    WriterLock,
};
use crate::store::{
    IndexDb, PreparedChunk, PreparedDocument, PreparedDocumentSummary, SnapshotFailure,
    StoragePreflight, StoragePreflightError, StoreError, chunker_fingerprint,
    prepare_passage_literals,
};

mod context;
mod evidence;
mod index_management;
mod init;
mod jsonl_connector;
mod project_ingest;
mod project_management;
mod search_evidence;
mod source_config;
mod source_management;
mod status_evidence;
mod stored_source;
#[cfg(test)]
mod tests;

const FAILURE_RECORD_ESTIMATED_WRITE_BYTES: u64 = 64 * 1024;

pub use context::{
    ContextError, ContextRequest, EffectiveContext, repository_root_for_current_dir,
    resolve_context, resolve_trust_target,
};
pub use evidence::{
    EvidenceSourceState, GetEvidence, GetEvidenceError, GetEvidenceFieldLimits, GetEvidenceOutcome,
    GetEvidenceRequest, SourceHashVerification,
};
pub use index_management::{
    DeleteIndexOutcome, DeleteIndexRequest, IndexManagementError, delete_index,
};
pub use init::{
    BroadRootReason, InitError, InitNextStep, InitOutcome, InitRequest, PointerOutcome,
    SourceEstimate, TrustOutcome, TrustRequest, TrustTarget, initialize, trust_repository,
};
pub use jsonl_connector::{
    JsonlBatchIngestError, JsonlFileIngestError, JsonlIngestTarget, JsonlPrepareError,
    MAX_JSONL_SNAPSHOT_BYTES, PreparedJsonlSnapshot, ingest_jsonl_sources_with_timeout,
    ingest_jsonl_with_timeout, prepare_jsonl_snapshot,
};
pub use project_ingest::{
    ProjectIngestError, ProjectIngestPlan, ingest_project_sources_with_timeout,
    plan_project_sources_with_timeout,
};
pub use project_management::{
    ProjectManagementError, ProjectUseOutcome, SetProjectRootRequest, create_project,
    list_projects, resolve_project_selector, set_project_root, use_project,
};
pub use search_evidence::{
    SearchEvidence, SearchEvidenceError, SearchEvidenceFieldLimits, SearchEvidenceOutcome,
    SearchEvidencePage, SearchEvidenceRequest, SearchEvidenceSnapshot,
};
pub use source_config::{
    FILESYSTEM_SOURCE_CONFIG_SCHEMA_VERSION, FilesystemSourceConfig,
    JSONL_SOURCE_CONFIG_SCHEMA_VERSION, JsonlSourceConfig, JsonlSourceConfigError,
    MAX_FILESYSTEM_SOURCE_CONFIG_BYTES, MAX_JSONL_SOURCE_CONFIG_BYTES, SourceConfigError,
};
pub use source_management::{
    AddFilesystemSourceRequest, AddJsonlSourceRequest, SourceManagementError,
    add_filesystem_source, add_jsonl_source, attach_jsonl_source, detach_jsonl_source,
    list_sources, list_sources_in_scope, remove_jsonl_source, resolve_source_selector,
};
pub use status_evidence::{
    StatusEvidence, StatusEvidenceError, StatusEvidenceFieldLimits, StatusEvidenceOutcome,
    StatusEvidenceRequest, StatusHealthIssue,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedFilesystemSnapshot {
    pub documents: Vec<PreparedDocument>,
    pub failures: Vec<SnapshotFailure>,
}

pub(crate) struct PreparedFilesystemSpool {
    spool: FilesystemSpool,
    pub(crate) summaries: Vec<PreparedDocumentSummary>,
    pub(crate) failures: Vec<SnapshotFailure>,
}

impl PreparedFilesystemSpool {
    fn load_document(
        &mut self,
        summary: &PreparedDocumentSummary,
        entry_by_key: &BTreeMap<Vec<u8>, usize>,
    ) -> Result<PreparedDocument, StoreError> {
        let index = *entry_by_key.get(&summary.connector_key).ok_or(
            StoreError::InvalidPreparedDocument("spooled document entry is absent"),
        )?;
        let entry = self.spool.entries()[index].clone();
        let body = self
            .spool
            .read_body(index)
            .map_err(|_| StoreError::InvalidPreparedDocument("private staging read failed"))?;
        prepare_document(
            entry.connector_key(),
            entry.relative_path(),
            entry.source_timestamp(),
            body,
        )
        .map_err(|_| StoreError::InvalidPreparedDocument("spooled document preparation failed"))
    }

    fn entry_index(&self) -> BTreeMap<Vec<u8>, usize> {
        self.spool
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.connector_key().to_vec(), index))
            .collect()
    }
}

pub fn prepare_filesystem_snapshot(
    root: &Path,
    discovery_options: &DiscoveryOptions,
) -> Result<PreparedFilesystemSnapshot, FilesystemIngestError> {
    let discovered = discover_files(root, discovery_options)?;
    let mut documents = Vec::with_capacity(discovered.files().len());

    for file in discovered.files() {
        documents.push(prepare_document(
            file.connector_key(),
            file.relative_path(),
            file.source_timestamp(),
            file.original_bytes().to_vec(),
        )?);
    }

    let failures = snapshot_failures(discovered.issues());

    Ok(PreparedFilesystemSnapshot {
        documents,
        failures,
    })
}

#[cfg(test)]
pub(crate) fn prepare_filesystem_spool(
    root: &Path,
    discovery_options: &DiscoveryOptions,
    staging_directory: &Path,
) -> Result<PreparedFilesystemSpool, FilesystemIngestError> {
    let estimate = estimate_filesystem_spool(root, discovery_options)?;
    prepare_filesystem_spool_with_estimate(root, discovery_options, staging_directory, estimate)
}

pub(crate) fn estimate_filesystem_spool(
    root: &Path,
    discovery_options: &DiscoveryOptions,
) -> Result<FilesystemDiscoveryEstimate, FilesystemIngestError> {
    estimate_filesystem_discovery(root, discovery_options).map_err(Into::into)
}

pub(crate) fn prepare_filesystem_spool_with_estimate(
    root: &Path,
    discovery_options: &DiscoveryOptions,
    staging_directory: &Path,
    estimate: FilesystemDiscoveryEstimate,
) -> Result<PreparedFilesystemSpool, FilesystemIngestError> {
    StoragePreflight::run_staging(staging_directory, estimate.eligible_bytes)?;
    let mut spool = discover_files_spooled_bounded(
        root,
        discovery_options,
        staging_directory,
        estimate.eligible_bytes,
    )?;
    let mut summaries = Vec::with_capacity(spool.entries().len());
    for index in 0..spool.entries().len() {
        let entry = spool.entries()[index].clone();
        let body = spool
            .read_body(index)
            .map_err(FilesystemIngestError::StagingRead)?;
        summaries.push(summarize_document(
            entry.connector_key(),
            entry.source_timestamp(),
            &body,
        )?);
    }
    let failures = snapshot_failures(spool.issues());
    Ok(PreparedFilesystemSpool {
        spool,
        summaries,
        failures,
    })
}

fn summarize_document(
    connector_key: &[u8],
    source_timestamp: Option<SourceTimestamp>,
    body: &[u8],
) -> Result<PreparedDocumentSummary, FilesystemIngestError> {
    let source_uri = repo_uri(connector_key);
    let title = display_title(connector_key);
    let source_updated_at = source_timestamp.and_then(format_source_timestamp);
    let metadata = json!({});
    let body_sha256 = body_sha256(body);
    let revision_sha256 = revision_sha256(&SnapshotRevision {
        body,
        source_uri: &source_uri,
        title: &title,
        metadata: &metadata,
        source_updated_at: source_updated_at.as_deref(),
    })?;
    Ok(PreparedDocumentSummary {
        connector_key: connector_key.to_vec(),
        source_uri,
        body_sha256,
        revision_sha256,
        body_len: u64::try_from(body.len())
            .map_err(|_| FilesystemIngestError::SourceEstimateOverflow)?,
    })
}

fn prepare_document(
    connector_key: &[u8],
    relative_path: &Path,
    source_timestamp: Option<SourceTimestamp>,
    body: Vec<u8>,
) -> Result<PreparedDocument, FilesystemIngestError> {
    let source_uri = repo_uri(connector_key);
    let title = display_title(connector_key);
    let source_updated_at = source_timestamp.and_then(format_source_timestamp);
    let kind = ChunkKind::from_path(relative_path).ok_or_else(|| {
        FilesystemIngestError::UnsupportedDiscoveredPath {
            path: display_connector_key(connector_key),
        }
    })?;
    let metadata = json!({});
    let body_digest = body_sha256(&body);
    let revision_digest = revision_sha256(&SnapshotRevision {
        body: &body,
        source_uri: &source_uri,
        title: &title,
        metadata: &metadata,
        source_updated_at: source_updated_at.as_deref(),
    })?;
    let chunks = chunk_bytes(&body, kind, ChunkSettings::default())
        .map_err(|source| FilesystemIngestError::Chunk {
            path: display_connector_key(connector_key),
            source,
        })?
        .into_iter()
        .map(|chunk| {
            let content = chunk.text().as_bytes();
            PreparedChunk {
                ordinal: chunk.ordinal(),
                byte_span: chunk.span(),
                line_span: chunk.line_span(),
                body_text: chunk.text().to_owned(),
                content_sha256: body_sha256(content),
                quote_bloom: QuoteBloom::from_content(content).into_bytes(),
                literals: prepare_passage_literals(&title, &source_uri, content),
            }
        })
        .collect();
    Ok(PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri,
        title,
        metadata_json: "{}".to_owned(),
        source_updated_at,
        body,
        body_sha256: body_digest,
        revision_sha256: revision_digest,
        chunker_fingerprint: chunker_fingerprint(kind),
        chunks,
    })
}

fn snapshot_failures(issues: &[crate::ingest::FileIssue]) -> Vec<SnapshotFailure> {
    issues
        .iter()
        .map(|issue| SnapshotFailure {
            connector_key: issue.connector_key().to_vec(),
            code: issue_code(issue.kind()).to_owned(),
            detail: format!(
                "{}: {}",
                issue_code(issue.kind()),
                display_connector_key(issue.connector_key())
            ),
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn ingest_filesystem(
    database: &mut IndexDb,
    scope: &FilesystemScope,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    strict: bool,
    confirmations: DeleteConfirmations,
) -> Result<IngestOutcome, FilesystemIngestError> {
    ingest_filesystem_with_timeout(
        database,
        scope,
        root,
        discovery_options,
        strict,
        confirmations,
        DEFAULT_WRITER_LOCK_TIMEOUT,
    )
}

#[cfg(test)]
pub(crate) fn plan_filesystem_ingest_with_timeout(
    database: &IndexDb,
    scope: &FilesystemScope,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    lock_timeout: Duration,
) -> Result<IngestPlan, FilesystemIngestError> {
    validate_filesystem_authority(scope, root, discovery_options, None)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    let staging_directory = database
        .path()
        .parent()
        .ok_or(FilesystemIngestError::StagingDirectoryMissing)?;
    let snapshot = prepare_filesystem_spool(root, discovery_options, staging_directory)?;
    database
        .plan_filesystem_summaries_under_lock(
            &writer_lock,
            scope,
            &snapshot.summaries,
            &snapshot.failures,
        )
        .map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn ingest_filesystem_with_timeout(
    database: &mut IndexDb,
    scope: &FilesystemScope,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    strict: bool,
    confirmations: DeleteConfirmations,
    lock_timeout: Duration,
) -> Result<IngestOutcome, FilesystemIngestError> {
    ingest_filesystem_with_policy(
        database,
        scope,
        root,
        discovery_options,
        strict,
        confirmations,
        FilesystemIngestPolicy {
            lock_timeout,
            index_quota_bytes: None,
        },
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct FilesystemIngestPolicy {
    pub lock_timeout: Duration,
    pub index_quota_bytes: Option<u64>,
}

#[cfg(test)]
pub(crate) fn ingest_filesystem_with_policy(
    database: &mut IndexDb,
    scope: &FilesystemScope,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    strict: bool,
    confirmations: DeleteConfirmations,
    policy: FilesystemIngestPolicy,
) -> Result<IngestOutcome, FilesystemIngestError> {
    validate_filesystem_authority(
        scope,
        root,
        discovery_options,
        Some(policy.index_quota_bytes),
    )?;
    let writer_lock = WriterLock::acquire(database.path(), policy.lock_timeout)?;
    StoragePreflight::run(
        database.path(),
        FAILURE_RECORD_ESTIMATED_WRITE_BYTES,
        policy.index_quota_bytes,
    )?;
    let staging_directory = database
        .path()
        .parent()
        .ok_or(FilesystemIngestError::StagingDirectoryMissing)?;
    let mut snapshot = match prepare_filesystem_spool_for_ingest(
        database,
        root,
        discovery_options,
        staging_directory,
        policy.index_quota_bytes,
    ) {
        Ok(snapshot) => snapshot,
        Err(FilesystemIngestError::Discovery(error @ DiscoveryError::Staging { .. })) => {
            return Err(FilesystemIngestError::Discovery(error));
        }
        Err(FilesystemIngestError::Discovery(error)) => {
            if !strict {
                database.record_filesystem_source_failure_under_lock(
                    &writer_lock,
                    scope,
                    discovery_error_code(&error),
                    &error.to_string(),
                )?;
            }
            return Err(FilesystemIngestError::Discovery(error));
        }
        Err(error) => return Err(error),
    };
    if strict && !snapshot.failures.is_empty() {
        return Err(FilesystemIngestError::StrictSourceFailures {
            failures: snapshot.failures,
        });
    }

    let summaries = snapshot.summaries.clone();
    let failures = snapshot.failures.clone();
    let entry_by_key = snapshot.entry_index();
    let plan = database.plan_filesystem_summaries_under_lock(
        &writer_lock,
        scope,
        &summaries,
        &failures,
    )?;
    let storage_preflight = StoragePreflight::run(
        database.path(),
        plan.estimated_write_bytes,
        policy.index_quota_bytes,
    )?;
    let mut outcome = database.apply_filesystem_summaries_under_lock(
        &writer_lock,
        scope,
        &summaries,
        &failures,
        confirmations,
        |summary| snapshot.load_document(summary, &entry_by_key),
    )?;
    outcome.storage_preflight = Some(storage_preflight);
    Ok(outcome)
}

#[cfg(test)]
fn validate_filesystem_authority(
    scope: &FilesystemScope,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    requested_quota: Option<Option<u64>>,
) -> Result<(), FilesystemIngestError> {
    let config = FilesystemSourceConfig::parse(&scope.source_config_json)
        .map_err(|_| FilesystemIngestError::SourceAuthorityMismatch)?;
    let canonical_config = config
        .to_canonical_json()
        .map_err(|_| FilesystemIngestError::SourceAuthorityMismatch)?;
    let logical_root = config
        .root()
        .to_str()
        .ok_or(FilesystemIngestError::SourceAuthorityMismatch)?;
    if canonical_config != scope.source_config_json
        || config.root() != root
        || config.discovery_options() != discovery_options
        || scope.source_logical_uri != logical_root
        || requested_quota.is_some_and(|quota| quota != config.index_quota_bytes())
    {
        return Err(FilesystemIngestError::SourceAuthorityMismatch);
    }
    Ok(())
}

fn prepare_filesystem_spool_for_ingest(
    database: &IndexDb,
    root: &Path,
    discovery_options: &DiscoveryOptions,
    staging_directory: &Path,
    index_quota_bytes: Option<u64>,
) -> Result<PreparedFilesystemSpool, FilesystemIngestError> {
    let estimate = estimate_filesystem_spool(root, discovery_options)?;
    let conservative_peak_bytes = estimate
        .eligible_bytes
        .checked_mul(7)
        .ok_or(FilesystemIngestError::SourceEstimateOverflow)?;
    StoragePreflight::run(database.path(), conservative_peak_bytes, index_quota_bytes)?;
    prepare_filesystem_spool_with_estimate(root, discovery_options, staging_directory, estimate)
}

fn discovery_error_code(error: &DiscoveryError) -> &'static str {
    match error {
        DiscoveryError::UnsupportedPlatform => "SOURCE_PLATFORM_UNSUPPORTED",
        DiscoveryError::InvalidIgnoreRule { .. }
        | DiscoveryError::InvalidIgnoreFile { .. }
        | DiscoveryError::InvalidPattern { .. } => "SOURCE_CONFIG_INVALID",
        DiscoveryError::SourceLimitExceeded { .. } => "SOURCE_LIMIT_EXCEEDED",
        DiscoveryError::TraversalLimitExceeded { .. } => "SOURCE_TRAVERSAL_LIMIT",
        DiscoveryError::StagingEstimateExceeded { .. } => "SOURCE_UNAVAILABLE",
        DiscoveryError::Staging { .. } => "SOURCE_STAGING_FAILED",
        DiscoveryError::RootMissing { .. }
        | DiscoveryError::RootIsSymlink { .. }
        | DiscoveryError::RootNotDirectory { .. }
        | DiscoveryError::RootOpen { .. }
        | DiscoveryError::DirectoryUnreadable { .. }
        | DiscoveryError::DirectoryChanged { .. } => "SOURCE_UNAVAILABLE",
    }
}

fn format_source_timestamp(timestamp: crate::ingest::SourceTimestamp) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(timestamp.unix_seconds())
        .ok()?
        .replace_nanosecond(timestamp.nanoseconds())
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn issue_code(kind: FileIssueKind) -> &'static str {
    match kind {
        FileIssueKind::InvalidUtf8 => "SOURCE_INVALID_UTF8",
        FileIssueKind::NulContent => "SOURCE_NUL_CONTENT",
        FileIssueKind::FileTooLarge => "SOURCE_FILE_TOO_LARGE",
        FileIssueKind::PermissionDenied => "SOURCE_PERMISSION_DENIED",
        FileIssueKind::ReadFailed => "SOURCE_READ_FAILED",
        FileIssueKind::ChangedDuringRead => "SOURCE_CHANGED_DURING_READ",
    }
}

fn display_title(connector_key: &[u8]) -> String {
    display_connector_key(
        connector_key
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or(connector_key),
    )
}

fn display_connector_key(connector_key: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut display = String::with_capacity(connector_key.len());
    for &byte in connector_key {
        if matches!(byte, 0x20..=0x7e) && byte != b'\\' {
            display.push(char::from(byte));
        } else if byte == b'\\' {
            display.push_str("\\\\");
        } else {
            display.push_str("\\x");
            display.push(char::from(HEX[usize::from(byte >> 4)]));
            display.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    display
}

#[derive(Debug, Error)]
pub enum FilesystemIngestError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error("filesystem discovery returned a path with an unsupported extension: {path}")]
    UnsupportedDiscoveredPath { path: String },
    #[error("failed to chunk {path}")]
    Chunk {
        path: String,
        #[source]
        source: ChunkError,
    },
    #[error(transparent)]
    Revision(#[from] RevisionHashError),
    #[error("private ingest staging could not be read")]
    StagingRead(#[source] io::Error),
    #[error("managed index path has no staging directory")]
    StagingDirectoryMissing,
    #[error("source estimate overflowed")]
    SourceEstimateOverflow,
    #[error("filesystem source root, options, or quota do not match the bound scope authority")]
    SourceAuthorityMismatch,
    #[error("strict ingest refused a snapshot containing source failures")]
    StrictSourceFailures { failures: Vec<SnapshotFailure> },
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
    #[error(transparent)]
    Store(#[from] StoreError),
}
