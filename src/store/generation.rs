use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use serde_json_canonicalizer::to_string as to_canonical_json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::{ByteSpan, IndexId, LineSpan, ProjectId, SafeSlug, Sha256Digest, SourceId};
use crate::ingest::{
    ChunkKind, ChunkSettings, MAX_JSONL_CONTENT_BYTES, MAX_JSONL_ID_BYTES,
    MAX_JSONL_METADATA_BYTES, MAX_JSONL_SOURCE_URI_BYTES, MAX_JSONL_TITLE_BYTES, QuoteBloom,
    SnapshotRevision, chunk_bytes, extract_identifier_literals, repo_uri, revision_sha256,
};
use crate::store::capacity::StoragePreflight;
use crate::store::doctor::Doctor;
use crate::store::open::{IndexDb, StoreError};
use crate::store::schema::{chunker_fingerprint, pipeline_fingerprint_for};
use crate::store::vector::{
    IndexEmbeddingProfile, clear_active_vector_membership, read_embedding_profile,
};
use crate::store::{ForgetLedger, WriterLock};

pub const DEFAULT_WRITER_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemScope {
    pub source_id: SourceId,
    pub source_name: SafeSlug,
    pub source_logical_uri: String,
    pub source_config_json: String,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlScope {
    pub source_id: SourceId,
    pub source_name: SafeSlug,
    pub source_logical_uri: String,
    pub source_config_json: String,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
}

/// One fully enumerated JSONL source, or one source-level failure whose prior
/// heads must be carried forward by the surrounding project ingest.
pub(crate) enum JsonlBatchSource<'a> {
    Snapshot {
        scope: &'a JsonlScope,
        documents: &'a [PreparedDocument],
        explicit_deletions: &'a [Vec<u8>],
    },
    Failed {
        scope: &'a JsonlScope,
        code: &'a str,
        detail: &'a str,
    },
}

pub(crate) struct PreparedSourceBatch<'a> {
    scope: SourceScopeRef<'a>,
    state: PreparedSourceBatchState<'a>,
}

enum PreparedSourceBatchState<'a> {
    Snapshot {
        documents: &'a [PreparedDocumentSummary],
        failures: &'a [SnapshotFailure],
        explicit_deletions: &'a [Vec<u8>],
    },
    Failed {
        code: &'a str,
        detail: &'a str,
    },
}

impl<'a> PreparedSourceBatch<'a> {
    pub(crate) fn filesystem_snapshot(
        scope: &'a FilesystemScope,
        documents: &'a [PreparedDocumentSummary],
        failures: &'a [SnapshotFailure],
    ) -> Self {
        Self {
            scope: scope.as_source_scope(),
            state: PreparedSourceBatchState::Snapshot {
                documents,
                failures,
                explicit_deletions: &[],
            },
        }
    }

    pub(crate) fn jsonl_snapshot(
        scope: &'a JsonlScope,
        documents: &'a [PreparedDocumentSummary],
        explicit_deletions: &'a [Vec<u8>],
    ) -> Self {
        Self {
            scope: scope.as_source_scope(),
            state: PreparedSourceBatchState::Snapshot {
                documents,
                failures: &[],
                explicit_deletions,
            },
        }
    }

    pub(crate) fn filesystem_failed(
        scope: &'a FilesystemScope,
        code: &'a str,
        detail: &'a str,
    ) -> Self {
        Self {
            scope: scope.as_source_scope(),
            state: PreparedSourceBatchState::Failed { code, detail },
        }
    }

    pub(crate) fn jsonl_failed(scope: &'a JsonlScope, code: &'a str, detail: &'a str) -> Self {
        Self {
            scope: scope.as_source_scope(),
            state: PreparedSourceBatchState::Failed { code, detail },
        }
    }
}

impl<'a> JsonlBatchSource<'a> {
    pub(crate) const fn snapshot(
        scope: &'a JsonlScope,
        documents: &'a [PreparedDocument],
        explicit_deletions: &'a [Vec<u8>],
    ) -> Self {
        Self::Snapshot {
            scope,
            documents,
            explicit_deletions,
        }
    }

    pub(crate) const fn failed(scope: &'a JsonlScope, code: &'a str, detail: &'a str) -> Self {
        Self::Failed {
            scope,
            code,
            detail,
        }
    }

    const fn scope(&self) -> &JsonlScope {
        match self {
            Self::Snapshot { scope, .. } | Self::Failed { scope, .. } => scope,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceKind {
    Filesystem,
    Jsonl,
}

impl SourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Jsonl => "jsonl",
        }
    }

    const fn detects_renames(self) -> bool {
        matches!(self, Self::Filesystem)
    }
}

#[derive(Clone, Copy)]
struct SourceScopeRef<'a> {
    kind: SourceKind,
    source_id: SourceId,
    source_name: &'a SafeSlug,
    source_logical_uri: &'a str,
    source_config_json: &'a str,
    project_id: ProjectId,
    project_name: &'a SafeSlug,
}

struct PreparedSourceSummarySnapshot<'a> {
    scope: SourceScopeRef<'a>,
    documents: &'a [PreparedDocumentSummary],
    failures: &'a [SnapshotFailure],
    explicit_deletions: &'a [Vec<u8>],
}

impl FilesystemScope {
    fn as_source_scope(&self) -> SourceScopeRef<'_> {
        SourceScopeRef {
            kind: SourceKind::Filesystem,
            source_id: self.source_id,
            source_name: &self.source_name,
            source_logical_uri: &self.source_logical_uri,
            source_config_json: &self.source_config_json,
            project_id: self.project_id,
            project_name: &self.project_name,
        }
    }
}

impl JsonlScope {
    fn as_source_scope(&self) -> SourceScopeRef<'_> {
        SourceScopeRef {
            kind: SourceKind::Jsonl,
            source_id: self.source_id,
            source_name: &self.source_name,
            source_logical_uri: &self.source_logical_uri,
            source_config_json: &self.source_config_json,
            project_id: self.project_id,
            project_name: &self.project_name,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedDocument {
    pub connector_key: Vec<u8>,
    pub source_uri: String,
    pub title: String,
    pub metadata_json: String,
    pub source_updated_at: Option<String>,
    pub body: Vec<u8>,
    pub body_sha256: Sha256Digest,
    pub revision_sha256: Sha256Digest,
    pub chunker_fingerprint: Sha256Digest,
    pub chunks: Vec<PreparedChunk>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedDocumentSummary {
    pub(crate) connector_key: Vec<u8>,
    pub(crate) source_uri: String,
    pub(crate) body_sha256: Sha256Digest,
    pub(crate) revision_sha256: Sha256Digest,
    pub(crate) body_len: u64,
}

impl PreparedDocumentSummary {
    pub(crate) fn from_document(document: &PreparedDocument) -> Result<Self, StoreError> {
        Ok(Self {
            connector_key: document.connector_key.clone(),
            source_uri: document.source_uri.clone(),
            body_sha256: document.body_sha256,
            revision_sha256: document.revision_sha256,
            body_len: u64::try_from(document.body.len())
                .map_err(|_| StoreError::IntegerOverflow)?,
        })
    }

    fn matches(&self, document: &PreparedDocument) -> Result<bool, StoreError> {
        Ok(self == &Self::from_document(document)?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedChunk {
    pub ordinal: u32,
    pub byte_span: ByteSpan,
    pub line_span: LineSpan,
    pub body_text: String,
    pub content_sha256: Sha256Digest,
    pub quote_bloom: [u8; 512],
    pub literals: Vec<(Vec<u8>, LiteralField)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiteralField {
    Title,
    SourceUri,
    Body,
}

impl LiteralField {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::SourceUri => "source_uri",
            Self::Body => "body",
        }
    }
}

pub fn prepare_passage_literals(
    title: &str,
    source_uri: &str,
    body: &[u8],
) -> Vec<(Vec<u8>, LiteralField)> {
    let mut postings = Vec::with_capacity(64);
    for (content, field) in [
        (title.as_bytes(), LiteralField::Title),
        (source_uri.as_bytes(), LiteralField::SourceUri),
        (body, LiteralField::Body),
    ] {
        for literal in extract_identifier_literals(content) {
            if postings.len() == 64 {
                return postings;
            }
            postings.push((literal, field));
        }
    }
    postings
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotFailure {
    pub connector_key: Vec<u8>,
    pub code: String,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeleteConfirmations {
    pub allow_empty_snapshot: bool,
    pub allow_mass_delete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestOutcome {
    pub generation_id: Option<i64>,
    pub changed_documents: usize,
    pub unchanged_documents: usize,
    pub tombstoned_documents: usize,
    pub carried_forward_documents: usize,
    pub failed_documents: usize,
    pub active_documents: usize,
    pub active_passages: usize,
    pub index_epoch: u64,
    pub source_outcomes: Vec<SourceIngestOutcome>,
    pub storage_preflight: Option<StoragePreflight>,
}

impl IngestOutcome {
    pub fn is_partial(&self) -> bool {
        self.failed_documents != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngestState {
    Success,
    Partial,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceIngestOutcome {
    pub source_id: SourceId,
    pub state: SourceIngestState,
    pub accepted_documents: usize,
    pub failed_documents: usize,
    pub carried_forward_documents: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRemovalOutcome {
    pub source_id: SourceId,
    pub source_name: SafeSlug,
    pub generation_id: Option<i64>,
    pub tombstoned_documents: usize,
    pub detached_projects: usize,
    pub index_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestPlan {
    pub new_documents: usize,
    pub changed_documents: usize,
    pub renamed_documents: usize,
    pub unchanged_documents: usize,
    pub tombstoned_documents: usize,
    pub carried_forward_documents: usize,
    pub failed_documents: usize,
    pub prior_active_documents: usize,
    pub projected_active_documents: usize,
    pub would_create_generation: bool,
    pub requires_empty_snapshot_confirmation: bool,
    pub requires_mass_delete_confirmation: bool,
    pub estimated_write_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OutcomeCounts {
    changed_documents: usize,
    unchanged_documents: usize,
    tombstoned_documents: usize,
    carried_forward_documents: usize,
    accepted_documents: usize,
    failed_documents: usize,
}

impl IndexDb {
    pub fn remove_jsonl_source_with_timeout(
        &mut self,
        project_id: ProjectId,
        source_id: SourceId,
        lock_timeout: Duration,
    ) -> Result<SourceRemovalOutcome, StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), lock_timeout)?;
        Doctor::run(self.path())?;
        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (source_name, _, _) = crate::store::source::resolve_attached_jsonl_source(
            &transaction,
            project_id,
            source_id,
        )?;
        let existing = load_existing_documents(&transaction, source_id)?;
        let active = existing
            .values()
            .filter(|document| document.active)
            .collect::<Vec<_>>();
        let generation_id = (!active.is_empty())
            .then(|| create_generation(&transaction, &now))
            .transpose()?;

        if let Some(generation_id) = generation_id {
            for current in &active {
                delete_active_passages(&transaction, &current.id)?;
                transaction.execute(
                    "UPDATE documents SET tombstoned_at = ?1 WHERE id = ?2",
                    params![now, current.id],
                )?;
                transaction.execute(
                    "INSERT INTO document_heads(
                        document_id, document_version_id, state, generation_id
                     ) VALUES (?1, NULL, 'tombstoned', ?2)
                     ON CONFLICT(document_id) DO UPDATE SET
                        document_version_id = NULL,
                        state = 'tombstoned',
                        generation_id = excluded.generation_id",
                    params![current.id, generation_id],
                )?;
                transaction.execute(
                    "INSERT INTO generation_changes(
                        generation_id, document_id, prior_version_id,
                        next_version_id, next_state
                     ) VALUES (?1, ?2, ?3, NULL, 'tombstoned')",
                    params![generation_id, current.id, current.document_version_id],
                )?;
            }
            transaction.execute(
                "UPDATE generations
                 SET state = 'committed', committed_at = ?1
                 WHERE id = ?2 AND state = 'building'",
                params![now, generation_id],
            )?;
            let index_epoch = metadata_u64(&transaction, "index_epoch")?
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?;
            set_metadata(
                &transaction,
                "index_epoch",
                index_epoch.to_string().as_bytes(),
            )?;
            set_metadata(
                &transaction,
                "active_generation",
                generation_id.to_string().as_bytes(),
            )?;
        }

        let detached_projects = usize_from_count(transaction.query_row(
            "SELECT COUNT(*) FROM project_sources
             WHERE source_id = ?1 AND removed_at IS NULL",
            [source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )?)?;
        transaction.execute(
            "UPDATE projects
             SET scope_revision = scope_revision + 1
             WHERE id IN (
                 SELECT project_id FROM project_sources
                 WHERE source_id = ?1 AND removed_at IS NULL
             )",
            [source_id.as_uuid().as_bytes().as_slice()],
        )?;
        transaction.execute(
            "UPDATE sources SET removed_at = ?1 WHERE id = ?2",
            params![now, source_id.as_uuid().as_bytes().as_slice()],
        )?;
        transaction.execute(
            "UPDATE project_sources SET removed_at = ?1 WHERE source_id = ?2",
            params![now, source_id.as_uuid().as_bytes().as_slice()],
        )?;
        let index_epoch = metadata_u64(&transaction, "index_epoch")?;
        transaction.commit()?;
        drop(writer_lock);
        Ok(SourceRemovalOutcome {
            source_id,
            source_name,
            generation_id,
            tombstoned_documents: active.len(),
            detached_projects,
            index_epoch,
        })
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_cap_sqlite_pages_at_current_size(&self) -> Result<i64, StoreError> {
        self.connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let page_count: i64 = self
            .connection()
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        Ok(self.connection().pragma_update_and_check(
            None,
            "max_page_count",
            page_count,
            |row| row.get(0),
        )?)
    }

    /// Test-support synthetic writer: atomically flips the index epoch, one
    /// project's scope revision, and one source's outcome fields inside a
    /// single immediate transaction under the writer lock, so WAL
    /// snapshot-consistency tests can detect torn reads. Not a supported
    /// public API; it must become crate-private before any library ABI ships.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_set_synthetic_snapshot_state(
        &mut self,
        project_id: ProjectId,
        source_id: SourceId,
        index_epoch: u64,
        scope_revision: u64,
        last_success_at: &str,
        last_error: Option<(&str, &str)>,
    ) -> Result<(), StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), DEFAULT_WRITER_LOCK_TIMEOUT)?;
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        let scope_revision = i64::try_from(scope_revision)
            .map_err(|_| StoreError::InvalidMetadata("scope revision"))?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'index_epoch'",
            [index_epoch.to_string().into_bytes()],
        )?;
        transaction.execute(
            "UPDATE projects SET scope_revision = ?1 WHERE id = ?2",
            params![scope_revision, uuid_bytes(project_id)],
        )?;
        if let Some((code, detail)) = last_error {
            transaction.execute(
                "UPDATE sources
                 SET last_success_at = ?1,
                     last_error_code = ?2,
                     last_error_detail = ?3,
                     last_error_at = ?1
                 WHERE id = ?4",
                params![last_success_at, code, detail, uuid_bytes(source_id)],
            )?;
        } else {
            transaction.execute(
                "UPDATE sources
                 SET last_success_at = ?1,
                     last_error_code = NULL,
                     last_error_detail = NULL,
                     last_error_at = NULL
                 WHERE id = ?2",
                params![last_success_at, uuid_bytes(source_id)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn configure_filesystem_scope(
        &mut self,
        scope: &FilesystemScope,
    ) -> Result<(), StoreError> {
        self.configure_source_scope(scope.as_source_scope())
    }

    fn configure_source_scope(&mut self, scope: SourceScopeRef<'_>) -> Result<(), StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), DEFAULT_WRITER_LOCK_TIMEOUT)?;
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(self.path())?;
        validate_scope(scope)?;
        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_scope(&transaction, scope, &now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn plan_filesystem_snapshot_with_timeout(
        &self,
        scope: &FilesystemScope,
        documents: &[PreparedDocument],
        failures: &[SnapshotFailure],
        lock_timeout: Duration,
    ) -> Result<IngestPlan, StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), lock_timeout)?;
        self.plan_filesystem_snapshot_under_lock(&writer_lock, scope, documents, failures)
    }

    pub(crate) fn plan_filesystem_snapshot_under_lock(
        &self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        documents: &[PreparedDocument],
        failures: &[SnapshotFailure],
    ) -> Result<IngestPlan, StoreError> {
        validate_snapshot(SourceKind::Filesystem, documents, failures, &[])?;
        let summaries = documents
            .iter()
            .map(PreparedDocumentSummary::from_document)
            .collect::<Result<Vec<_>, _>>()?;
        self.plan_filesystem_summaries_under_lock(writer_lock, scope, &summaries, failures)
    }

    pub(crate) fn plan_filesystem_summaries_under_lock(
        &self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        documents: &[PreparedDocumentSummary],
        failures: &[SnapshotFailure],
    ) -> Result<IngestPlan, StoreError> {
        self.plan_source_summaries_under_lock(
            writer_lock,
            scope.as_source_scope(),
            documents,
            failures,
            &[],
        )
    }

    pub fn plan_jsonl_snapshot_with_timeout(
        &self,
        scope: &JsonlScope,
        documents: &[PreparedDocument],
        explicit_deletions: &[Vec<u8>],
        lock_timeout: Duration,
    ) -> Result<IngestPlan, StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), lock_timeout)?;
        self.plan_jsonl_snapshot_under_lock(&writer_lock, scope, documents, explicit_deletions)
    }

    pub(crate) fn plan_jsonl_snapshot_under_lock(
        &self,
        writer_lock: &WriterLock,
        scope: &JsonlScope,
        documents: &[PreparedDocument],
        explicit_deletions: &[Vec<u8>],
    ) -> Result<IngestPlan, StoreError> {
        validate_snapshot(SourceKind::Jsonl, documents, &[], explicit_deletions)?;
        let summaries = documents
            .iter()
            .map(PreparedDocumentSummary::from_document)
            .collect::<Result<Vec<_>, _>>()?;
        self.plan_source_summaries_under_lock(
            writer_lock,
            scope.as_source_scope(),
            &summaries,
            &[],
            explicit_deletions,
        )
    }

    fn plan_source_summaries_under_lock(
        &self,
        writer_lock: &WriterLock,
        scope: SourceScopeRef<'_>,
        documents: &[PreparedDocumentSummary],
        failures: &[SnapshotFailure],
        explicit_deletions: &[Vec<u8>],
    ) -> Result<IngestPlan, StoreError> {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(self.path())?;
        validate_scope(scope)?;
        validate_summary_snapshot(scope.kind, documents, failures, explicit_deletions)?;

        let transaction = self.connection().unchecked_transaction()?;
        validate_existing_scope(&transaction, scope)?;
        let existing = load_existing_documents(&transaction, scope.source_id)?;
        let ready = documents
            .iter()
            .map(|document| (document.connector_key.clone(), document))
            .collect::<BTreeMap<_, _>>();
        let failed_keys = failures
            .iter()
            .map(|failure| failure.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let observed = ready
            .keys()
            .cloned()
            .chain(failed_keys.iter().cloned())
            .chain(explicit_deletions.iter().cloned())
            .collect::<BTreeSet<_>>();
        let initially_absent = existing
            .values()
            .filter(|document| document.active && !observed.contains(&document.connector_key))
            .map(|document| document.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let rename_map = if scope.kind.detects_renames() {
            detect_unambiguous_renames(&existing, &ready, &initially_absent)
        } else {
            BTreeMap::new()
        };
        let renamed_old_keys = rename_map.values().cloned().collect::<BTreeSet<_>>();
        let absent = initially_absent
            .difference(&renamed_old_keys)
            .cloned()
            .collect::<Vec<_>>();
        let explicit_tombstones = explicit_deletions
            .iter()
            .filter(|key| existing.get(*key).is_some_and(|document| document.active))
            .count();
        let unchanged_documents = ready
            .iter()
            .filter(|(connector_key, document)| {
                !rename_map.contains_key(*connector_key)
                    && existing.get(*connector_key).is_some_and(|current| {
                        current.active && current.revision_sha256 == Some(document.revision_sha256)
                    })
            })
            .count();
        let changed_documents = ready.len() - unchanged_documents;
        let new_documents = ready
            .keys()
            .filter(|key| !existing.contains_key(*key) && !rename_map.contains_key(*key))
            .count();
        let carried_forward_documents = failed_keys
            .iter()
            .filter(|key| existing.get(*key).is_some_and(|document| document.active))
            .count();
        let prior_active_documents = existing.values().filter(|document| document.active).count();
        let projected_active_documents = ready.len() + carried_forward_documents;
        let deletion_budget_prior = prior_active_documents
            .checked_sub(explicit_tombstones)
            .ok_or(StoreError::IntegerOverflow)?;
        let (requires_empty_snapshot_confirmation, requires_mass_delete_confirmation) =
            deletion_requirements(
                deletion_budget_prior,
                projected_active_documents,
                absent.len(),
            );
        let plan = IngestPlan {
            new_documents,
            changed_documents,
            renamed_documents: rename_map.len(),
            unchanged_documents,
            tombstoned_documents: absent.len() + explicit_tombstones,
            carried_forward_documents,
            failed_documents: failures.len(),
            prior_active_documents,
            projected_active_documents,
            would_create_generation: changed_documents != 0
                || !absent.is_empty()
                || explicit_tombstones != 0,
            requires_empty_snapshot_confirmation,
            requires_mass_delete_confirmation,
            estimated_write_bytes: estimate_write_bytes(
                &ready,
                &existing,
                &rename_map,
                absent.len() + explicit_tombstones,
                failures,
            )?,
        };
        transaction.rollback()?;
        Ok(plan)
    }

    pub fn apply_filesystem_snapshot(
        &mut self,
        scope: &FilesystemScope,
        documents: &[PreparedDocument],
        failures: &[SnapshotFailure],
        confirmations: DeleteConfirmations,
    ) -> Result<IngestOutcome, StoreError> {
        self.apply_filesystem_snapshot_with_timeout(
            scope,
            documents,
            failures,
            confirmations,
            DEFAULT_WRITER_LOCK_TIMEOUT,
        )
    }

    pub fn apply_filesystem_snapshot_with_timeout(
        &mut self,
        scope: &FilesystemScope,
        documents: &[PreparedDocument],
        failures: &[SnapshotFailure],
        confirmations: DeleteConfirmations,
        lock_timeout: Duration,
    ) -> Result<IngestOutcome, StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), lock_timeout)?;
        self.apply_filesystem_snapshot_under_lock(
            &writer_lock,
            scope,
            documents,
            failures,
            confirmations,
        )
    }

    pub(crate) fn apply_filesystem_snapshot_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        documents: &[PreparedDocument],
        failures: &[SnapshotFailure],
        confirmations: DeleteConfirmations,
    ) -> Result<IngestOutcome, StoreError> {
        validate_snapshot(SourceKind::Filesystem, documents, failures, &[])?;
        let summaries = documents
            .iter()
            .map(PreparedDocumentSummary::from_document)
            .collect::<Result<Vec<_>, _>>()?;
        let documents_by_key = documents
            .iter()
            .map(|document| (document.connector_key.clone(), document))
            .collect::<BTreeMap<_, _>>();
        self.apply_source_summaries_under_lock(
            writer_lock,
            PreparedSourceSummarySnapshot {
                scope: scope.as_source_scope(),
                documents: &summaries,
                failures,
                explicit_deletions: &[],
            },
            confirmations,
            |summary| {
                documents_by_key
                    .get(&summary.connector_key)
                    .map(|document| (*document).clone())
                    .ok_or(StoreError::InvalidPreparedDocument(
                        "prepared document summary has no body",
                    ))
            },
        )
    }

    pub fn apply_jsonl_snapshot(
        &mut self,
        scope: &JsonlScope,
        documents: &[PreparedDocument],
        explicit_deletions: &[Vec<u8>],
        confirmations: DeleteConfirmations,
    ) -> Result<IngestOutcome, StoreError> {
        self.apply_jsonl_snapshot_with_timeout(
            scope,
            documents,
            explicit_deletions,
            confirmations,
            DEFAULT_WRITER_LOCK_TIMEOUT,
        )
    }

    pub fn apply_jsonl_snapshot_with_timeout(
        &mut self,
        scope: &JsonlScope,
        documents: &[PreparedDocument],
        explicit_deletions: &[Vec<u8>],
        confirmations: DeleteConfirmations,
        lock_timeout: Duration,
    ) -> Result<IngestOutcome, StoreError> {
        let writer_lock = WriterLock::acquire(self.path(), lock_timeout)?;
        self.apply_jsonl_snapshot_under_lock(
            &writer_lock,
            scope,
            documents,
            explicit_deletions,
            confirmations,
        )
    }

    pub(crate) fn apply_jsonl_snapshot_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: &JsonlScope,
        documents: &[PreparedDocument],
        explicit_deletions: &[Vec<u8>],
        confirmations: DeleteConfirmations,
    ) -> Result<IngestOutcome, StoreError> {
        validate_snapshot(SourceKind::Jsonl, documents, &[], explicit_deletions)?;
        let summaries = documents
            .iter()
            .map(PreparedDocumentSummary::from_document)
            .collect::<Result<Vec<_>, _>>()?;
        let documents_by_key = documents
            .iter()
            .map(|document| (document.connector_key.clone(), document))
            .collect::<BTreeMap<_, _>>();
        self.apply_source_summaries_under_lock(
            writer_lock,
            PreparedSourceSummarySnapshot {
                scope: scope.as_source_scope(),
                documents: &summaries,
                failures: &[],
                explicit_deletions,
            },
            confirmations,
            |summary| {
                documents_by_key
                    .get(&summary.connector_key)
                    .map(|document| (*document).clone())
                    .ok_or(StoreError::InvalidPreparedDocument(
                        "prepared document summary has no body",
                    ))
            },
        )
    }

    pub(crate) fn apply_jsonl_batch_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        sources: &[JsonlBatchSource<'_>],
        confirmations: DeleteConfirmations,
    ) -> Result<IngestOutcome, StoreError> {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        if sources.is_empty() {
            return Err(StoreError::InvalidPreparedDocument(
                "JSONL source batch is empty",
            ));
        }
        Doctor::run(self.path())?;
        let forget_ledger = load_forget_ledger(self)?;

        let mut source_ids = BTreeSet::new();
        for source in sources {
            let scope = source.scope();
            if !source_ids.insert(scope.source_id) {
                return Err(StoreError::ScopeConflict);
            }
            validate_scope(scope.as_source_scope())?;
            match source {
                JsonlBatchSource::Snapshot {
                    documents,
                    explicit_deletions,
                    ..
                } => validate_snapshot(SourceKind::Jsonl, documents, &[], explicit_deletions)?,
                JsonlBatchSource::Failed { code, detail, .. }
                    if code.is_empty() || detail.is_empty() =>
                {
                    return Err(StoreError::InvalidPreparedDocument(
                        "source failure is incomplete",
                    ));
                }
                JsonlBatchSource::Failed { .. } => {}
            }
        }

        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deltas = Vec::new();
        let mut failed_sources = Vec::new();

        for source in sources {
            match source {
                JsonlBatchSource::Snapshot {
                    scope,
                    documents,
                    explicit_deletions,
                } => {
                    ensure_scope(&transaction, scope.as_source_scope(), &now)?;
                    let existing = load_existing_documents(&transaction, scope.source_id)?;
                    let ready = documents
                        .iter()
                        .map(|document| (document.connector_key.clone(), document))
                        .collect::<BTreeMap<_, _>>();
                    let observed = ready
                        .keys()
                        .cloned()
                        .chain(explicit_deletions.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    let absent = existing
                        .values()
                        .filter(|document| {
                            document.active && !observed.contains(&document.connector_key)
                        })
                        .map(|document| document.connector_key.clone())
                        .collect::<Vec<_>>();
                    let explicit_tombstones = explicit_deletions
                        .iter()
                        .filter(|key| existing.get(*key).is_some_and(|document| document.active))
                        .cloned()
                        .collect::<Vec<_>>();
                    let prior_active_documents =
                        existing.values().filter(|document| document.active).count();
                    let deletion_budget_prior = prior_active_documents
                        .checked_sub(explicit_tombstones.len())
                        .ok_or(StoreError::IntegerOverflow)?;
                    enforce_deletion_budget(
                        deletion_budget_prior,
                        ready.len(),
                        absent.len(),
                        confirmations,
                    )?;
                    let unchanged_documents = ready
                        .iter()
                        .filter(|(connector_key, document)| {
                            existing.get(*connector_key).is_some_and(|current| {
                                current.active
                                    && current.revision_sha256 == Some(document.revision_sha256)
                            })
                        })
                        .count();
                    let changed_documents = ready.len() - unchanged_documents;
                    deltas.push(JsonlBatchDelta {
                        scope,
                        ready,
                        existing,
                        absent,
                        explicit_tombstones,
                        changed_documents,
                        unchanged_documents,
                    });
                }
                JsonlBatchSource::Failed {
                    scope,
                    code,
                    detail,
                } => {
                    validate_existing_scope(&transaction, scope.as_source_scope())?;
                    let carried_forward_documents =
                        load_existing_documents(&transaction, scope.source_id)?
                            .values()
                            .filter(|document| document.active)
                            .count();
                    failed_sources.push(JsonlBatchFailure {
                        scope,
                        code,
                        detail,
                        carried_forward_documents,
                    });
                }
            }
        }

        let needs_generation = deltas.iter().any(JsonlBatchDelta::needs_generation);
        let generation_id = needs_generation
            .then(|| create_generation(&transaction, &now))
            .transpose()?;
        let mut counts = OutcomeCounts {
            changed_documents: 0,
            unchanged_documents: 0,
            tombstoned_documents: 0,
            carried_forward_documents: 0,
            accepted_documents: 0,
            failed_documents: 0,
        };
        let mut source_outcomes = Vec::with_capacity(sources.len());

        for delta in deltas {
            if let Some(generation_id) = generation_id {
                for (connector_key, document) in &delta.ready {
                    let current = delta.existing.get(connector_key);
                    if current.is_some_and(|value| {
                        value.active && value.revision_sha256 == Some(document.revision_sha256)
                    }) {
                        continue;
                    }
                    validate_document(SourceKind::Jsonl, document)?;
                    let document_id = match current {
                        Some(value) => {
                            transaction.execute(
                                "UPDATE documents
                                 SET current_source_uri = ?1, tombstoned_at = NULL
                                 WHERE id = ?2",
                                params![document.source_uri, value.id],
                            )?;
                            value.id.clone()
                        }
                        None => {
                            let id = Uuid::new_v4().as_bytes().to_vec();
                            transaction.execute(
                                "INSERT INTO documents(
                                    id, source_id, connector_key,
                                    current_source_uri, tombstoned_at
                                 ) VALUES (?1, ?2, ?3, ?4, NULL)",
                                params![
                                    id,
                                    uuid_bytes(delta.scope.source_id),
                                    connector_key,
                                    document.source_uri,
                                ],
                            )?;
                            id
                        }
                    };
                    let prior_version_id = current.and_then(|value| value.document_version_id);
                    let (version_id, chunk_ids) = stage_document(
                        &transaction,
                        &forget_ledger,
                        delta.scope.source_id,
                        &document_id,
                        document,
                        &now,
                    )?;
                    replace_active_passages(
                        &transaction,
                        delta.scope.source_id,
                        &document_id,
                        version_id,
                        document,
                        &chunk_ids,
                    )?;
                    transaction.execute(
                        "INSERT INTO document_heads(
                            document_id, document_version_id, state, generation_id
                         ) VALUES (?1, ?2, 'active', ?3)
                         ON CONFLICT(document_id) DO UPDATE SET
                            document_version_id = excluded.document_version_id,
                            state = excluded.state,
                            generation_id = excluded.generation_id",
                        params![document_id, version_id, generation_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO generation_changes(
                            generation_id, document_id, prior_version_id,
                            next_version_id, next_state
                         ) VALUES (?1, ?2, ?3, ?4, 'active')",
                        params![generation_id, document_id, prior_version_id, version_id],
                    )?;
                }

                for connector_key in delta.absent.iter().chain(&delta.explicit_tombstones) {
                    let current = delta
                        .existing
                        .get(connector_key)
                        .expect("tombstone keys originate from existing JSONL documents");
                    delete_active_passages(&transaction, &current.id)?;
                    transaction.execute(
                        "UPDATE documents SET tombstoned_at = ?1 WHERE id = ?2",
                        params![now, current.id],
                    )?;
                    transaction.execute(
                        "INSERT INTO document_heads(
                            document_id, document_version_id, state, generation_id
                         ) VALUES (?1, NULL, 'tombstoned', ?2)
                         ON CONFLICT(document_id) DO UPDATE SET
                            document_version_id = NULL,
                            state = 'tombstoned',
                            generation_id = excluded.generation_id",
                        params![current.id, generation_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO generation_changes(
                            generation_id, document_id, prior_version_id,
                            next_version_id, next_state
                         ) VALUES (?1, ?2, ?3, NULL, 'tombstoned')",
                        params![generation_id, current.id, current.document_version_id],
                    )?;
                }
            }

            update_source_status(
                &transaction,
                delta.scope.source_id,
                &now,
                delta.ready.len(),
                None,
            )?;
            let tombstoned_documents = delta
                .absent
                .len()
                .checked_add(delta.explicit_tombstones.len())
                .ok_or(StoreError::IntegerOverflow)?;
            counts.changed_documents =
                checked_count_add(counts.changed_documents, delta.changed_documents)?;
            counts.unchanged_documents =
                checked_count_add(counts.unchanged_documents, delta.unchanged_documents)?;
            counts.tombstoned_documents =
                checked_count_add(counts.tombstoned_documents, tombstoned_documents)?;
            counts.accepted_documents =
                checked_count_add(counts.accepted_documents, delta.ready.len())?;
            source_outcomes.push(SourceIngestOutcome {
                source_id: delta.scope.source_id,
                state: SourceIngestState::Success,
                accepted_documents: delta.ready.len(),
                failed_documents: 0,
                carried_forward_documents: 0,
            });
        }

        for failure in failed_sources {
            transaction.execute(
                "UPDATE sources
                 SET last_error_code = ?1, last_error_detail = ?2, last_error_at = ?3
                 WHERE id = ?4",
                params![
                    failure.code,
                    failure.detail,
                    now,
                    uuid_bytes(failure.scope.source_id),
                ],
            )?;
            if let Some(generation_id) = generation_id {
                transaction.execute(
                    "INSERT INTO source_sync_errors(
                        generation_id, source_id, connector_key,
                        code, detail, created_at
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
                    params![
                        generation_id,
                        uuid_bytes(failure.scope.source_id),
                        failure.code,
                        failure.detail,
                        now,
                    ],
                )?;
            }
            counts.carried_forward_documents = checked_count_add(
                counts.carried_forward_documents,
                failure.carried_forward_documents,
            )?;
            counts.failed_documents = checked_count_add(counts.failed_documents, 1)?;
            source_outcomes.push(SourceIngestOutcome {
                source_id: failure.scope.source_id,
                state: SourceIngestState::Failed,
                accepted_documents: 0,
                failed_documents: 1,
                carried_forward_documents: failure.carried_forward_documents,
            });
        }

        if let Some(generation_id) = generation_id {
            transaction.execute(
                "UPDATE generations
                 SET state = 'committed', committed_at = ?1
                 WHERE id = ?2 AND state = 'building'",
                params![now, generation_id],
            )?;
            let index_epoch = metadata_u64(&transaction, "index_epoch")?
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?;
            set_metadata(
                &transaction,
                "index_epoch",
                index_epoch.to_string().as_bytes(),
            )?;
            set_metadata(
                &transaction,
                "active_generation",
                generation_id.to_string().as_bytes(),
            )?;
        }

        let active_documents = usize_from_count(transaction.query_row(
            "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
            [],
            |row| row.get(0),
        )?)?;
        let active_passages = usize_from_count(transaction.query_row(
            "SELECT COUNT(*) FROM active_passages",
            [],
            |row| row.get(0),
        )?)?;
        let index_epoch = metadata_u64(&transaction, "index_epoch")?;
        transaction.commit()?;
        Ok(IngestOutcome {
            generation_id,
            changed_documents: counts.changed_documents,
            unchanged_documents: counts.unchanged_documents,
            tombstoned_documents: counts.tombstoned_documents,
            carried_forward_documents: counts.carried_forward_documents,
            failed_documents: counts.failed_documents,
            active_documents,
            active_passages,
            index_epoch,
            source_outcomes,
            storage_preflight: None,
        })
    }

    pub(crate) fn apply_prepared_source_batch_under_lock<F>(
        &mut self,
        writer_lock: &WriterLock,
        sources: &[PreparedSourceBatch<'_>],
        confirmations: DeleteConfirmations,
        mut load_document: F,
    ) -> Result<IngestOutcome, StoreError>
    where
        F: FnMut(SourceId, &PreparedDocumentSummary) -> Result<PreparedDocument, StoreError>,
    {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        if sources.is_empty() {
            return Err(StoreError::InvalidPreparedDocument(
                "prepared source batch is empty",
            ));
        }
        Doctor::run(self.path())?;
        let forget_ledger = load_forget_ledger(self)?;

        let mut source_ids = BTreeSet::new();
        for source in sources {
            if !source_ids.insert(source.scope.source_id) {
                return Err(StoreError::ScopeConflict);
            }
            validate_scope(source.scope)?;
            match &source.state {
                PreparedSourceBatchState::Snapshot {
                    documents,
                    failures,
                    explicit_deletions,
                } => validate_summary_snapshot(
                    source.scope.kind,
                    documents,
                    failures,
                    explicit_deletions,
                )?,
                PreparedSourceBatchState::Failed { code, detail }
                    if code.is_empty() || detail.is_empty() =>
                {
                    return Err(StoreError::InvalidPreparedDocument(
                        "source failure is incomplete",
                    ));
                }
                PreparedSourceBatchState::Failed { .. } => {}
            }
        }

        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deltas = Vec::new();
        let mut failed_sources = Vec::new();

        for source in sources {
            match &source.state {
                PreparedSourceBatchState::Snapshot {
                    documents,
                    failures,
                    explicit_deletions,
                } => {
                    ensure_scope(&transaction, source.scope, &now)?;
                    let existing = load_existing_documents(&transaction, source.scope.source_id)?;
                    let ready = documents
                        .iter()
                        .map(|document| (document.connector_key.clone(), document))
                        .collect::<BTreeMap<_, _>>();
                    let failed_keys = failures
                        .iter()
                        .map(|failure| failure.connector_key.clone())
                        .collect::<BTreeSet<_>>();
                    let carried_forward_documents = failed_keys
                        .iter()
                        .filter(|connector_key| {
                            existing
                                .get(*connector_key)
                                .is_some_and(|document| document.active)
                        })
                        .count();
                    let observed = ready
                        .keys()
                        .cloned()
                        .chain(failed_keys.iter().cloned())
                        .chain(explicit_deletions.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    let initially_absent = existing
                        .values()
                        .filter(|document| {
                            document.active && !observed.contains(&document.connector_key)
                        })
                        .map(|document| document.connector_key.clone())
                        .collect::<BTreeSet<_>>();
                    let rename_map = if source.scope.kind.detects_renames() {
                        detect_unambiguous_renames(&existing, &ready, &initially_absent)
                    } else {
                        BTreeMap::new()
                    };
                    let renamed_old_keys = rename_map.values().cloned().collect::<BTreeSet<_>>();
                    let absent = initially_absent
                        .difference(&renamed_old_keys)
                        .cloned()
                        .collect::<Vec<_>>();
                    let explicit_tombstones = explicit_deletions
                        .iter()
                        .filter(|key| existing.get(*key).is_some_and(|document| document.active))
                        .cloned()
                        .collect::<Vec<_>>();
                    let deletion_budget_prior = existing
                        .values()
                        .filter(|document| document.active)
                        .count()
                        .checked_sub(explicit_tombstones.len())
                        .ok_or(StoreError::IntegerOverflow)?;
                    enforce_deletion_budget(
                        deletion_budget_prior,
                        ready.len() + carried_forward_documents,
                        absent.len(),
                        confirmations,
                    )?;
                    let unchanged_documents = ready
                        .iter()
                        .filter(|(connector_key, document)| {
                            !rename_map.contains_key(*connector_key)
                                && existing.get(*connector_key).is_some_and(|current| {
                                    current.active
                                        && current.revision_sha256 == Some(document.revision_sha256)
                                })
                        })
                        .count();
                    let changed_documents = ready.len() - unchanged_documents;
                    deltas.push(PreparedBatchDelta {
                        scope: source.scope,
                        ready,
                        failures,
                        existing,
                        rename_map,
                        absent,
                        explicit_tombstones,
                        changed_documents,
                        unchanged_documents,
                        carried_forward_documents,
                    });
                }
                PreparedSourceBatchState::Failed { code, detail } => {
                    validate_existing_scope(&transaction, source.scope)?;
                    let carried_forward_documents =
                        load_existing_documents(&transaction, source.scope.source_id)?
                            .values()
                            .filter(|document| document.active)
                            .count();
                    failed_sources.push(PreparedBatchFailure {
                        scope: source.scope,
                        code,
                        detail,
                        carried_forward_documents,
                    });
                }
            }
        }

        let needs_generation = deltas.iter().any(PreparedBatchDelta::needs_generation);
        let generation_id = needs_generation
            .then(|| create_generation(&transaction, &now))
            .transpose()?;
        let mut counts = OutcomeCounts {
            changed_documents: 0,
            unchanged_documents: 0,
            tombstoned_documents: 0,
            carried_forward_documents: 0,
            accepted_documents: 0,
            failed_documents: 0,
        };
        let mut source_outcomes = Vec::with_capacity(sources.len());

        for delta in deltas {
            if let Some(generation_id) = generation_id {
                for (connector_key, document_summary) in &delta.ready {
                    let renamed_from = delta.rename_map.get(connector_key);
                    let current = renamed_from
                        .and_then(|old_key| delta.existing.get(old_key))
                        .or_else(|| delta.existing.get(connector_key));
                    let is_unchanged = renamed_from.is_none()
                        && current.is_some_and(|value| {
                            value.active
                                && value.revision_sha256 == Some(document_summary.revision_sha256)
                        });
                    if is_unchanged {
                        continue;
                    }
                    let document = load_document(delta.scope.source_id, document_summary)?;
                    validate_document(delta.scope.kind, &document)?;
                    if !document_summary.matches(&document)? {
                        return Err(StoreError::InvalidPreparedDocument(
                            "prepared document does not match its scan summary",
                        ));
                    }

                    let document_id = match current {
                        Some(value) => {
                            transaction.execute(
                                "UPDATE documents
                                 SET connector_key = ?1,
                                     current_source_uri = ?2,
                                     tombstoned_at = NULL
                                 WHERE id = ?3",
                                params![connector_key, &document.source_uri, value.id],
                            )?;
                            value.id.clone()
                        }
                        None => {
                            let id = Uuid::new_v4().as_bytes().to_vec();
                            transaction.execute(
                                "INSERT INTO documents(
                                    id, source_id, connector_key,
                                    current_source_uri, tombstoned_at
                                 ) VALUES (?1, ?2, ?3, ?4, NULL)",
                                params![
                                    id,
                                    uuid_bytes(delta.scope.source_id),
                                    connector_key,
                                    &document.source_uri,
                                ],
                            )?;
                            id
                        }
                    };
                    let prior_version_id = current.and_then(|value| value.document_version_id);
                    let (version_id, chunk_ids) = stage_document(
                        &transaction,
                        &forget_ledger,
                        delta.scope.source_id,
                        &document_id,
                        &document,
                        &now,
                    )?;
                    replace_active_passages(
                        &transaction,
                        delta.scope.source_id,
                        &document_id,
                        version_id,
                        &document,
                        &chunk_ids,
                    )?;
                    transaction.execute(
                        "INSERT INTO document_heads(
                            document_id, document_version_id, state, generation_id
                         ) VALUES (?1, ?2, 'active', ?3)
                         ON CONFLICT(document_id) DO UPDATE SET
                            document_version_id = excluded.document_version_id,
                            state = excluded.state,
                            generation_id = excluded.generation_id",
                        params![document_id, version_id, generation_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO generation_changes(
                            generation_id, document_id, prior_version_id,
                            next_version_id, next_state
                         ) VALUES (?1, ?2, ?3, ?4, 'active')",
                        params![generation_id, document_id, prior_version_id, version_id],
                    )?;
                }

                for connector_key in delta.absent.iter().chain(&delta.explicit_tombstones) {
                    let current = delta
                        .existing
                        .get(connector_key)
                        .expect("tombstone keys originate from existing documents");
                    delete_active_passages(&transaction, &current.id)?;
                    transaction.execute(
                        "UPDATE documents SET tombstoned_at = ?1 WHERE id = ?2",
                        params![now, current.id],
                    )?;
                    transaction.execute(
                        "INSERT INTO document_heads(
                            document_id, document_version_id, state, generation_id
                         ) VALUES (?1, NULL, 'tombstoned', ?2)
                         ON CONFLICT(document_id) DO UPDATE SET
                            document_version_id = NULL,
                            state = 'tombstoned',
                            generation_id = excluded.generation_id",
                        params![current.id, generation_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO generation_changes(
                            generation_id, document_id, prior_version_id,
                            next_version_id, next_state
                         ) VALUES (?1, ?2, ?3, NULL, 'tombstoned')",
                        params![generation_id, current.id, current.document_version_id],
                    )?;
                }

                for failure in delta.failures {
                    transaction.execute(
                        "INSERT INTO source_sync_errors(
                            generation_id, source_id, connector_key,
                            code, detail, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            generation_id,
                            uuid_bytes(delta.scope.source_id),
                            failure.connector_key,
                            failure.code,
                            failure.detail,
                            now,
                        ],
                    )?;
                }
            }

            update_source_status(
                &transaction,
                delta.scope.source_id,
                &now,
                delta.ready.len(),
                delta.failures.first(),
            )?;
            let tombstoned_documents = delta
                .absent
                .len()
                .checked_add(delta.explicit_tombstones.len())
                .ok_or(StoreError::IntegerOverflow)?;
            counts.changed_documents =
                checked_count_add(counts.changed_documents, delta.changed_documents)?;
            counts.unchanged_documents =
                checked_count_add(counts.unchanged_documents, delta.unchanged_documents)?;
            counts.tombstoned_documents =
                checked_count_add(counts.tombstoned_documents, tombstoned_documents)?;
            counts.carried_forward_documents = checked_count_add(
                counts.carried_forward_documents,
                delta.carried_forward_documents,
            )?;
            counts.accepted_documents =
                checked_count_add(counts.accepted_documents, delta.ready.len())?;
            counts.failed_documents =
                checked_count_add(counts.failed_documents, delta.failures.len())?;
            source_outcomes.push(SourceIngestOutcome {
                source_id: delta.scope.source_id,
                state: if delta.failures.is_empty() {
                    SourceIngestState::Success
                } else if delta.ready.is_empty() {
                    SourceIngestState::Failed
                } else {
                    SourceIngestState::Partial
                },
                accepted_documents: delta.ready.len(),
                failed_documents: delta.failures.len(),
                carried_forward_documents: delta.carried_forward_documents,
            });
        }

        for failure in failed_sources {
            transaction.execute(
                "UPDATE sources
                 SET last_error_code = ?1, last_error_detail = ?2, last_error_at = ?3
                 WHERE id = ?4",
                params![
                    failure.code,
                    failure.detail,
                    now,
                    uuid_bytes(failure.scope.source_id),
                ],
            )?;
            if let Some(generation_id) = generation_id {
                transaction.execute(
                    "INSERT INTO source_sync_errors(
                        generation_id, source_id, connector_key,
                        code, detail, created_at
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
                    params![
                        generation_id,
                        uuid_bytes(failure.scope.source_id),
                        failure.code,
                        failure.detail,
                        now,
                    ],
                )?;
            }
            counts.carried_forward_documents = checked_count_add(
                counts.carried_forward_documents,
                failure.carried_forward_documents,
            )?;
            counts.failed_documents = checked_count_add(counts.failed_documents, 1)?;
            source_outcomes.push(SourceIngestOutcome {
                source_id: failure.scope.source_id,
                state: SourceIngestState::Failed,
                accepted_documents: 0,
                failed_documents: 1,
                carried_forward_documents: failure.carried_forward_documents,
            });
        }

        if let Some(generation_id) = generation_id {
            transaction.execute(
                "UPDATE generations
                 SET state = 'committed', committed_at = ?1
                 WHERE id = ?2 AND state = 'building'",
                params![now, generation_id],
            )?;
            let index_epoch = metadata_u64(&transaction, "index_epoch")?
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?;
            set_metadata(
                &transaction,
                "index_epoch",
                index_epoch.to_string().as_bytes(),
            )?;
            set_metadata(
                &transaction,
                "active_generation",
                generation_id.to_string().as_bytes(),
            )?;
        }

        let active_documents = usize_from_count(transaction.query_row(
            "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
            [],
            |row| row.get(0),
        )?)?;
        let active_passages = usize_from_count(transaction.query_row(
            "SELECT COUNT(*) FROM active_passages",
            [],
            |row| row.get(0),
        )?)?;
        let index_epoch = metadata_u64(&transaction, "index_epoch")?;
        source_outcomes.sort_by_key(|source| *source.source_id.as_uuid().as_bytes());
        transaction.commit()?;
        Ok(IngestOutcome {
            generation_id,
            changed_documents: counts.changed_documents,
            unchanged_documents: counts.unchanged_documents,
            tombstoned_documents: counts.tombstoned_documents,
            carried_forward_documents: counts.carried_forward_documents,
            failed_documents: counts.failed_documents,
            active_documents,
            active_passages,
            index_epoch,
            source_outcomes,
            storage_preflight: None,
        })
    }

    pub(crate) fn apply_filesystem_summaries_under_lock<F>(
        &mut self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        documents: &[PreparedDocumentSummary],
        failures: &[SnapshotFailure],
        confirmations: DeleteConfirmations,
        load_document: F,
    ) -> Result<IngestOutcome, StoreError>
    where
        F: FnMut(&PreparedDocumentSummary) -> Result<PreparedDocument, StoreError>,
    {
        self.apply_source_summaries_under_lock(
            writer_lock,
            PreparedSourceSummarySnapshot {
                scope: scope.as_source_scope(),
                documents,
                failures,
                explicit_deletions: &[],
            },
            confirmations,
            load_document,
        )
    }

    fn apply_source_summaries_under_lock<F>(
        &mut self,
        writer_lock: &WriterLock,
        snapshot: PreparedSourceSummarySnapshot<'_>,
        confirmations: DeleteConfirmations,
        mut load_document: F,
    ) -> Result<IngestOutcome, StoreError>
    where
        F: FnMut(&PreparedDocumentSummary) -> Result<PreparedDocument, StoreError>,
    {
        let PreparedSourceSummarySnapshot {
            scope,
            documents,
            failures,
            explicit_deletions,
        } = snapshot;
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(self.path())?;
        let forget_ledger = load_forget_ledger(self)?;
        validate_scope(scope)?;
        validate_summary_snapshot(scope.kind, documents, failures, explicit_deletions)?;

        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_scope(&transaction, scope, &now)?;

        let existing = load_existing_documents(&transaction, scope.source_id)?;
        let ready = documents
            .iter()
            .map(|document| (document.connector_key.clone(), document))
            .collect::<BTreeMap<_, _>>();
        let failed_keys = failures
            .iter()
            .map(|failure| failure.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let carried_forward_documents = failed_keys
            .iter()
            .filter(|connector_key| {
                existing
                    .get(*connector_key)
                    .is_some_and(|document| document.active)
            })
            .count();
        let observed = ready
            .keys()
            .cloned()
            .chain(failed_keys.iter().cloned())
            .chain(explicit_deletions.iter().cloned())
            .collect::<BTreeSet<_>>();

        let initially_absent = existing
            .values()
            .filter(|document| document.active && !observed.contains(&document.connector_key))
            .map(|document| document.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let rename_map = if scope.kind.detects_renames() {
            detect_unambiguous_renames(&existing, &ready, &initially_absent)
        } else {
            BTreeMap::new()
        };
        let renamed_old_keys = rename_map.values().cloned().collect::<BTreeSet<_>>();
        let absent = initially_absent
            .difference(&renamed_old_keys)
            .cloned()
            .collect::<Vec<_>>();
        let explicit_tombstones = explicit_deletions
            .iter()
            .filter(|key| existing.get(*key).is_some_and(|document| document.active))
            .cloned()
            .collect::<Vec<_>>();

        let deletion_budget_prior = existing
            .values()
            .filter(|document| document.active)
            .count()
            .checked_sub(explicit_tombstones.len())
            .ok_or(StoreError::IntegerOverflow)?;
        enforce_deletion_budget(
            deletion_budget_prior,
            ready.len() + carried_forward_documents,
            absent.len(),
            confirmations,
        )?;

        let unchanged_documents = ready
            .iter()
            .filter(|(connector_key, document)| {
                if rename_map.contains_key(*connector_key) {
                    return false;
                }
                existing.get(*connector_key).is_some_and(|current| {
                    current.active && current.revision_sha256 == Some(document.revision_sha256)
                })
            })
            .count();
        let changed_documents = ready.len() - unchanged_documents;
        let tombstoned_documents = absent.len() + explicit_tombstones.len();
        let needs_generation = changed_documents != 0 || tombstoned_documents != 0;

        if !needs_generation {
            update_source_status(
                &transaction,
                scope.source_id,
                &now,
                documents.len(),
                failures.first(),
            )?;
            let outcome = current_outcome(
                &transaction,
                scope.source_id,
                None,
                OutcomeCounts {
                    changed_documents: 0,
                    unchanged_documents,
                    tombstoned_documents: 0,
                    carried_forward_documents,
                    accepted_documents: documents.len(),
                    failed_documents: failures.len(),
                },
            )?;
            transaction.commit()?;
            return Ok(outcome);
        }

        let generation_id = create_generation(&transaction, &now)?;

        for (connector_key, document_summary) in &ready {
            let renamed_from = rename_map.get(connector_key);
            let current = renamed_from
                .and_then(|old_key| existing.get(old_key))
                .or_else(|| existing.get(connector_key));
            let is_unchanged = renamed_from.is_none()
                && current.is_some_and(|value| {
                    value.active && value.revision_sha256 == Some(document_summary.revision_sha256)
                });
            if is_unchanged {
                continue;
            }
            let document = load_document(document_summary)?;
            validate_document(scope.kind, &document)?;
            if !document_summary.matches(&document)? {
                return Err(StoreError::InvalidPreparedDocument(
                    "prepared document does not match its scan summary",
                ));
            }

            let document_id = match current {
                Some(value) => {
                    transaction.execute(
                        "UPDATE documents
                         SET connector_key = ?1,
                             current_source_uri = ?2,
                             tombstoned_at = NULL
                         WHERE id = ?3",
                        params![connector_key, &document.source_uri, value.id,],
                    )?;
                    value.id.clone()
                }
                None => {
                    let id = Uuid::new_v4().as_bytes().to_vec();
                    transaction.execute(
                        "INSERT INTO documents(
                            id, source_id, connector_key,
                            current_source_uri, tombstoned_at
                         ) VALUES (?1, ?2, ?3, ?4, NULL)",
                        params![
                            id,
                            uuid_bytes(scope.source_id),
                            connector_key,
                            &document.source_uri,
                        ],
                    )?;
                    id
                }
            };

            let prior_version_id = current.and_then(|value| value.document_version_id);
            let (version_id, chunk_ids) = stage_document(
                &transaction,
                &forget_ledger,
                scope.source_id,
                &document_id,
                &document,
                &now,
            )?;
            replace_active_passages(
                &transaction,
                scope.source_id,
                &document_id,
                version_id,
                &document,
                &chunk_ids,
            )?;
            transaction.execute(
                "INSERT INTO document_heads(
                    document_id, document_version_id, state, generation_id
                 ) VALUES (?1, ?2, 'active', ?3)
                 ON CONFLICT(document_id) DO UPDATE SET
                    document_version_id = excluded.document_version_id,
                    state = excluded.state,
                    generation_id = excluded.generation_id",
                params![document_id, version_id, generation_id],
            )?;
            transaction.execute(
                "INSERT INTO generation_changes(
                    generation_id, document_id, prior_version_id,
                    next_version_id, next_state
                 ) VALUES (?1, ?2, ?3, ?4, 'active')",
                params![generation_id, document_id, prior_version_id, version_id,],
            )?;
        }

        for connector_key in absent.iter().chain(&explicit_tombstones) {
            let current = existing
                .get(connector_key)
                .expect("absent keys originate from existing documents");
            delete_active_passages(&transaction, &current.id)?;
            transaction.execute(
                "UPDATE documents SET tombstoned_at = ?1 WHERE id = ?2",
                params![now, current.id],
            )?;
            transaction.execute(
                "INSERT INTO document_heads(
                    document_id, document_version_id, state, generation_id
                 ) VALUES (?1, NULL, 'tombstoned', ?2)
                 ON CONFLICT(document_id) DO UPDATE SET
                    document_version_id = NULL,
                    state = 'tombstoned',
                    generation_id = excluded.generation_id",
                params![current.id, generation_id],
            )?;
            transaction.execute(
                "INSERT INTO generation_changes(
                    generation_id, document_id, prior_version_id,
                    next_version_id, next_state
                 ) VALUES (?1, ?2, ?3, NULL, 'tombstoned')",
                params![generation_id, current.id, current.document_version_id,],
            )?;
        }

        for failure in failures {
            transaction.execute(
                "INSERT INTO source_sync_errors(
                    generation_id, source_id, connector_key,
                    code, detail, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    generation_id,
                    uuid_bytes(scope.source_id),
                    failure.connector_key,
                    failure.code,
                    failure.detail,
                    now,
                ],
            )?;
        }

        update_source_status(
            &transaction,
            scope.source_id,
            &now,
            documents.len(),
            failures.first(),
        )?;
        transaction.execute(
            "UPDATE generations
             SET state = 'committed', committed_at = ?1
             WHERE id = ?2 AND state = 'building'",
            params![now, generation_id],
        )?;
        let previous_epoch = metadata_u64(&transaction, "index_epoch")?;
        let index_epoch = previous_epoch
            .checked_add(1)
            .ok_or(StoreError::IntegerOverflow)?;
        set_metadata(
            &transaction,
            "index_epoch",
            index_epoch.to_string().as_bytes(),
        )?;
        set_metadata(
            &transaction,
            "active_generation",
            generation_id.to_string().as_bytes(),
        )?;

        let outcome = current_outcome(
            &transaction,
            scope.source_id,
            Some(generation_id),
            OutcomeCounts {
                changed_documents,
                unchanged_documents,
                tombstoned_documents,
                carried_forward_documents,
                accepted_documents: documents.len(),
                failed_documents: failures.len(),
            },
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    #[cfg(test)]
    pub(crate) fn record_filesystem_source_failure_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        code: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.record_source_failure_under_lock(writer_lock, scope.as_source_scope(), code, detail)
    }

    pub(crate) fn record_jsonl_source_failure_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: &JsonlScope,
        code: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.record_source_failure_under_lock(writer_lock, scope.as_source_scope(), code, detail)
    }

    fn record_source_failure_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: SourceScopeRef<'_>,
        code: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        validate_scope(scope)?;
        if code.is_empty() || detail.is_empty() {
            return Err(StoreError::InvalidPreparedDocument(
                "source failure is incomplete",
            ));
        }

        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sources
                WHERE id = ?1 AND kind = ?2 AND name = ?3
                  AND logical_uri = ?4 AND config_json = ?5
                  AND removed_at IS NULL
            )",
            params![
                uuid_bytes(scope.source_id),
                scope.kind.as_str(),
                scope.source_name.to_string(),
                scope.source_logical_uri,
                scope.source_config_json,
            ],
            |row| row.get(0),
        )?;
        if !matches {
            return Err(StoreError::ScopeConflict);
        }
        transaction.execute(
            "UPDATE sources
             SET last_error_code = ?1, last_error_detail = ?2, last_error_at = ?3
             WHERE id = ?4",
            params![code, detail, now, uuid_bytes(scope.source_id)],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ExistingDocument {
    id: Vec<u8>,
    connector_key: Vec<u8>,
    active: bool,
    document_version_id: Option<i64>,
    body_sha256: Option<Sha256Digest>,
    revision_sha256: Option<Sha256Digest>,
}

struct JsonlBatchDelta<'a> {
    scope: &'a JsonlScope,
    ready: BTreeMap<Vec<u8>, &'a PreparedDocument>,
    existing: BTreeMap<Vec<u8>, ExistingDocument>,
    absent: Vec<Vec<u8>>,
    explicit_tombstones: Vec<Vec<u8>>,
    changed_documents: usize,
    unchanged_documents: usize,
}

impl JsonlBatchDelta<'_> {
    fn needs_generation(&self) -> bool {
        self.changed_documents != 0
            || !self.absent.is_empty()
            || !self.explicit_tombstones.is_empty()
    }
}

struct JsonlBatchFailure<'a> {
    scope: &'a JsonlScope,
    code: &'a str,
    detail: &'a str,
    carried_forward_documents: usize,
}

struct PreparedBatchDelta<'a> {
    scope: SourceScopeRef<'a>,
    ready: BTreeMap<Vec<u8>, &'a PreparedDocumentSummary>,
    failures: &'a [SnapshotFailure],
    existing: BTreeMap<Vec<u8>, ExistingDocument>,
    rename_map: BTreeMap<Vec<u8>, Vec<u8>>,
    absent: Vec<Vec<u8>>,
    explicit_tombstones: Vec<Vec<u8>>,
    changed_documents: usize,
    unchanged_documents: usize,
    carried_forward_documents: usize,
}

impl PreparedBatchDelta<'_> {
    fn needs_generation(&self) -> bool {
        self.changed_documents != 0
            || !self.absent.is_empty()
            || !self.explicit_tombstones.is_empty()
    }
}

struct PreparedBatchFailure<'a> {
    scope: SourceScopeRef<'a>,
    code: &'a str,
    detail: &'a str,
    carried_forward_documents: usize,
}

fn checked_count_add(left: usize, right: usize) -> Result<usize, StoreError> {
    left.checked_add(right).ok_or(StoreError::IntegerOverflow)
}

fn validate_scope(scope: SourceScopeRef<'_>) -> Result<(), StoreError> {
    if scope.source_logical_uri.is_empty() {
        return Err(StoreError::InvalidPreparedDocument(
            "source logical URI is empty",
        ));
    }
    let config: Value = serde_json::from_str(scope.source_config_json)
        .map_err(|_| StoreError::InvalidPreparedDocument("source config is not JSON"))?;
    if !config.is_object() {
        return Err(StoreError::InvalidPreparedDocument(
            "source config is not an object",
        ));
    }
    Ok(())
}

fn validate_snapshot(
    kind: SourceKind,
    documents: &[PreparedDocument],
    failures: &[SnapshotFailure],
    explicit_deletions: &[Vec<u8>],
) -> Result<(), StoreError> {
    let mut keys = BTreeSet::new();
    for document in documents {
        if !keys.insert(document.connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
        validate_document(kind, document)?;
    }
    for failure in failures {
        if failure.connector_key.is_empty() || !keys.insert(failure.connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
        if failure.code.is_empty() || failure.detail.is_empty() {
            return Err(StoreError::InvalidPreparedDocument(
                "source failure is incomplete",
            ));
        }
    }
    for connector_key in explicit_deletions {
        validate_connector_key(kind, connector_key)?;
        if !keys.insert(connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
    }
    Ok(())
}

fn validate_summary_snapshot(
    kind: SourceKind,
    documents: &[PreparedDocumentSummary],
    failures: &[SnapshotFailure],
    explicit_deletions: &[Vec<u8>],
) -> Result<(), StoreError> {
    let mut keys = BTreeSet::new();
    for document in documents {
        if validate_connector_key(kind, &document.connector_key).is_err()
            || !valid_source_uri(kind, &document.connector_key, &document.source_uri)
            || !keys.insert(document.connector_key.clone())
        {
            return Err(StoreError::InvalidPreparedDocument(
                "document scan summary is invalid",
            ));
        }
    }
    for failure in failures {
        if failure.connector_key.is_empty() || !keys.insert(failure.connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
        if failure.code.is_empty() || failure.detail.is_empty() {
            return Err(StoreError::InvalidPreparedDocument(
                "source failure is incomplete",
            ));
        }
    }
    for connector_key in explicit_deletions {
        validate_connector_key(kind, connector_key)?;
        if !keys.insert(connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
    }
    Ok(())
}

fn validate_document(kind: SourceKind, document: &PreparedDocument) -> Result<(), StoreError> {
    validate_connector_key(kind, &document.connector_key)?;
    if !valid_source_uri(kind, &document.connector_key, &document.source_uri)
        || (kind == SourceKind::Filesystem && document.title.is_empty())
        || (kind == SourceKind::Jsonl
            && (document.title.len() > MAX_JSONL_TITLE_BYTES
                || document.body.is_empty()
                || document.body.len() > MAX_JSONL_CONTENT_BYTES))
        || document.body_sha256 != Sha256Digest::of_bytes(&document.body)
    {
        return Err(StoreError::InvalidPreparedDocument(
            "document identity or body hash is invalid",
        ));
    }
    let metadata: Value = serde_json::from_str(&document.metadata_json)
        .map_err(|_| StoreError::InvalidPreparedDocument("metadata is not JSON"))?;
    if !metadata.is_object() {
        return Err(StoreError::InvalidPreparedDocument(
            "metadata is not an object",
        ));
    }
    let canonical_metadata = to_canonical_json(&metadata)
        .map_err(|_| StoreError::InvalidPreparedDocument("metadata is not canonical JSON"))?;
    if canonical_metadata != document.metadata_json {
        return Err(StoreError::InvalidPreparedDocument(
            "metadata is not canonical JSON",
        ));
    }
    if kind == SourceKind::Jsonl && document.metadata_json.len() > MAX_JSONL_METADATA_BYTES {
        return Err(StoreError::InvalidPreparedDocument(
            "metadata exceeds the JSONL limit",
        ));
    }
    if let Some(updated_at) = document.source_updated_at.as_deref() {
        let timestamp = OffsetDateTime::parse(updated_at, &Rfc3339)
            .map_err(|_| StoreError::InvalidPreparedDocument("source timestamp is invalid"))?;
        let normalized = timestamp
            .format(&Rfc3339)
            .map_err(|_| StoreError::InvalidPreparedDocument("source timestamp is invalid"))?;
        if normalized != updated_at {
            return Err(StoreError::InvalidPreparedDocument(
                "source timestamp is not normalized",
            ));
        }
    }
    let expected_revision = revision_sha256(&SnapshotRevision {
        body: &document.body,
        source_uri: &document.source_uri,
        title: &document.title,
        metadata: &metadata,
        source_updated_at: document.source_updated_at.as_deref(),
    })
    .map_err(|_| StoreError::InvalidPreparedDocument("snapshot revision is invalid"))?;
    if expected_revision != document.revision_sha256 {
        return Err(StoreError::InvalidPreparedDocument(
            "snapshot revision hash does not match",
        ));
    }

    let chunk_kind = match kind {
        SourceKind::Filesystem => ChunkKind::from_path(Path::new(&document.source_uri)).ok_or(
            StoreError::InvalidPreparedDocument("source type is unsupported"),
        )?,
        SourceKind::Jsonl => ChunkKind::PlainText,
    };
    if document.chunker_fingerprint != chunker_fingerprint(chunk_kind) {
        return Err(StoreError::InvalidPreparedDocument(
            "chunker fingerprint does not match the source type",
        ));
    }
    let expected_chunks = chunk_bytes(&document.body, chunk_kind, ChunkSettings::default())
        .map_err(|_| StoreError::InvalidPreparedDocument("deterministic chunking failed"))?;
    if document.chunks.is_empty()
        && !document.body.is_empty()
        && document.body.as_slice() != b"\xef\xbb\xbf"
    {
        return Err(StoreError::InvalidPreparedDocument(
            "nonempty document has no chunks",
        ));
    }
    if expected_chunks.len() != document.chunks.len() {
        return Err(StoreError::InvalidPreparedDocument(
            "chunk layout does not match the frozen pipeline",
        ));
    }

    for (expected_ordinal, (chunk, expected)) in
        document.chunks.iter().zip(&expected_chunks).enumerate()
    {
        if usize::try_from(chunk.ordinal).ok() != Some(expected_ordinal) {
            return Err(StoreError::InvalidPreparedDocument(
                "chunk ordinals are not contiguous",
            ));
        }
        if chunk.ordinal != expected.ordinal()
            || chunk.byte_span != expected.span()
            || chunk.line_span != expected.line_span()
            || chunk.body_text != expected.text()
        {
            return Err(StoreError::InvalidPreparedDocument(
                "chunk layout does not match the frozen pipeline",
            ));
        }
        let bytes = chunk
            .byte_span
            .slice_bytes(&document.body)
            .map_err(|_| StoreError::InvalidPreparedDocument("chunk span is invalid"))?;
        if bytes != chunk.body_text.as_bytes()
            || chunk.content_sha256 != Sha256Digest::of_bytes(bytes)
            || chunk.quote_bloom != QuoteBloom::from_content(bytes).into_bytes()
        {
            return Err(StoreError::InvalidPreparedDocument(
                "chunk bytes, hash, or quote filter do not match",
            ));
        }
        if chunk.literals != prepare_passage_literals(&document.title, &document.source_uri, bytes)
        {
            return Err(StoreError::InvalidPreparedDocument(
                "literal postings do not match the passage",
            ));
        }
    }
    Ok(())
}

fn validate_connector_key(kind: SourceKind, connector_key: &[u8]) -> Result<(), StoreError> {
    let maximum = match kind {
        SourceKind::Filesystem => 4096,
        SourceKind::Jsonl => MAX_JSONL_ID_BYTES,
    };
    if connector_key.is_empty()
        || connector_key.len() > maximum
        || (kind == SourceKind::Jsonl
            && match std::str::from_utf8(connector_key) {
                Ok(value) => value.chars().any(char::is_control),
                Err(_) => true,
            })
    {
        return Err(StoreError::InvalidPreparedDocument(
            "connector key is invalid for the source kind",
        ));
    }
    Ok(())
}

fn valid_source_uri(kind: SourceKind, connector_key: &[u8], source_uri: &str) -> bool {
    !source_uri.is_empty()
        && source_uri.len() <= MAX_JSONL_SOURCE_URI_BYTES
        && !source_uri.chars().any(char::is_control)
        && (kind != SourceKind::Filesystem || source_uri == repo_uri(connector_key))
}

fn ensure_scope(
    transaction: &Transaction<'_>,
    scope: SourceScopeRef<'_>,
    now: &str,
) -> Result<(), StoreError> {
    let source_id = uuid_bytes(scope.source_id);
    let source_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sources WHERE id = ?1)",
        [source_id],
        |row| row.get(0),
    )?;
    if !source_exists {
        transaction.execute(
            "INSERT INTO sources(
                id, kind, name, logical_uri, config_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source_id,
                scope.kind.as_str(),
                scope.source_name.to_string(),
                scope.source_logical_uri,
                scope.source_config_json,
                now,
            ],
        )?;
    } else {
        let matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sources
                WHERE id = ?1 AND kind = ?2 AND name = ?3
                  AND logical_uri = ?4 AND config_json = ?5
            )",
            params![
                source_id,
                scope.kind.as_str(),
                scope.source_name.to_string(),
                scope.source_logical_uri,
                scope.source_config_json,
            ],
            |row| row.get(0),
        )?;
        if !matches {
            return Err(StoreError::ScopeConflict);
        }
    }

    let project_id = uuid_bytes(scope.project_id);
    let project_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if !project_exists {
        transaction.execute(
            "INSERT INTO projects(id, name, scope_revision, created_at)
             VALUES (?1, ?2, 0, ?3)",
            params![project_id, scope.project_name.to_string(), now],
        )?;
    } else {
        let matches: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM projects WHERE id = ?1 AND name = ?2
            )",
            params![project_id, scope.project_name.to_string()],
            |row| row.get(0),
        )?;
        if !matches {
            return Err(StoreError::ScopeConflict);
        }
    }

    let added = transaction.execute(
        "INSERT OR IGNORE INTO project_sources(project_id, source_id)
         VALUES (?1, ?2)",
        params![project_id, source_id],
    )?;
    if added != 0 && project_exists {
        transaction.execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [project_id],
        )?;
    }
    let membership_active: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM project_sources
            WHERE project_id = ?1 AND source_id = ?2 AND removed_at IS NULL
        )",
        params![project_id, source_id],
        |row| row.get(0),
    )?;
    if !membership_active {
        return Err(StoreError::ScopeConflict);
    }
    Ok(())
}

fn validate_existing_scope(
    connection: &rusqlite::Connection,
    scope: SourceScopeRef<'_>,
) -> Result<(), StoreError> {
    let is_empty: bool = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sources)
             AND NOT EXISTS(SELECT 1 FROM projects)
             AND NOT EXISTS(SELECT 1 FROM project_sources)",
        [],
        |row| row.get(0),
    )?;
    if is_empty {
        return Ok(());
    }

    let source_matches: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sources
            WHERE id = ?1 AND kind = ?2 AND name = ?3
              AND logical_uri = ?4 AND config_json = ?5
              AND removed_at IS NULL
        )",
        params![
            uuid_bytes(scope.source_id),
            scope.kind.as_str(),
            scope.source_name.to_string(),
            scope.source_logical_uri,
            scope.source_config_json,
        ],
        |row| row.get(0),
    )?;
    let project_matches: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM projects AS p
            JOIN project_sources AS ps ON ps.project_id = p.id
            WHERE p.id = ?1 AND p.name = ?2 AND ps.source_id = ?3
              AND ps.removed_at IS NULL
        )",
        params![
            uuid_bytes(scope.project_id),
            scope.project_name.to_string(),
            uuid_bytes(scope.source_id),
        ],
        |row| row.get(0),
    )?;
    if !source_matches || !project_matches {
        return Err(StoreError::ScopeConflict);
    }
    Ok(())
}

fn load_existing_documents(
    transaction: &Transaction<'_>,
    source_id: SourceId,
) -> Result<BTreeMap<Vec<u8>, ExistingDocument>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT d.id, d.connector_key, dh.state,
                dh.document_version_id, cb.body_sha256,
                dv.revision_sha256
         FROM documents AS d
         LEFT JOIN document_heads AS dh ON dh.document_id = d.id
         LEFT JOIN document_versions AS dv
           ON dv.id = dh.document_version_id
         LEFT JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
         WHERE d.source_id = ?1
         ORDER BY d.connector_key",
    )?;
    let rows = statement.query_map([uuid_bytes(source_id)], |row| {
        let state = row.get::<_, Option<String>>(2)?;
        let body = row.get::<_, Option<Vec<u8>>>(4)?;
        let revision = row.get::<_, Option<Vec<u8>>>(5)?;
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            state.as_deref() == Some("active"),
            row.get::<_, Option<i64>>(3)?,
            body,
            revision,
        ))
    })?;

    let mut documents = BTreeMap::new();
    for row in rows {
        let (id, connector_key, active, version, body, revision) = row?;
        documents.insert(
            connector_key.clone(),
            ExistingDocument {
                id,
                connector_key,
                active,
                document_version_id: version,
                body_sha256: body.map(|bytes| digest_from_blob(&bytes)).transpose()?,
                revision_sha256: revision.map(|bytes| digest_from_blob(&bytes)).transpose()?,
            },
        );
    }
    Ok(documents)
}

fn detect_unambiguous_renames(
    existing: &BTreeMap<Vec<u8>, ExistingDocument>,
    ready: &BTreeMap<Vec<u8>, &PreparedDocumentSummary>,
    absent: &BTreeSet<Vec<u8>>,
) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut new_by_hash = BTreeMap::<Sha256Digest, Vec<Vec<u8>>>::new();
    for (key, document) in ready {
        if !existing.contains_key(key) {
            new_by_hash
                .entry(document.body_sha256)
                .or_default()
                .push(key.clone());
        }
    }

    let mut absent_by_hash = BTreeMap::<Sha256Digest, Vec<Vec<u8>>>::new();
    for key in absent {
        if let Some(hash) = existing.get(key).and_then(|value| value.body_sha256) {
            absent_by_hash.entry(hash).or_default().push(key.clone());
        }
    }

    new_by_hash
        .into_iter()
        .filter_map(|(hash, new_keys)| {
            let old_keys = absent_by_hash.get(&hash)?;
            if new_keys.len() == 1 && old_keys.len() == 1 {
                Some((new_keys[0].clone(), old_keys[0].clone()))
            } else {
                None
            }
        })
        .collect()
}

fn enforce_deletion_budget(
    prior_active: usize,
    projected_active: usize,
    absent: usize,
    confirmations: DeleteConfirmations,
) -> Result<(), StoreError> {
    let (requires_empty, requires_mass_delete) =
        deletion_requirements(prior_active, projected_active, absent);
    if requires_empty && !confirmations.allow_empty_snapshot {
        return Err(StoreError::EmptySnapshotConfirmationRequired);
    }
    if requires_mass_delete && !confirmations.allow_mass_delete {
        return Err(StoreError::MassDeleteConfirmationRequired {
            absent,
            eligible_prior: prior_active,
        });
    }
    Ok(())
}

fn deletion_requirements(
    prior_active: usize,
    projected_active: usize,
    absent: usize,
) -> (bool, bool) {
    let requires_empty = prior_active != 0 && projected_active == 0;
    let over_fraction = absent
        .checked_mul(4)
        .is_none_or(|scaled| scaled > prior_active);
    let requires_mass_delete = absent != 0 && (absent > 1_000 || over_fraction);
    (requires_empty, requires_mass_delete)
}

fn estimate_write_bytes(
    ready: &BTreeMap<Vec<u8>, &PreparedDocumentSummary>,
    existing: &BTreeMap<Vec<u8>, ExistingDocument>,
    rename_map: &BTreeMap<Vec<u8>, Vec<u8>>,
    tombstoned_documents: usize,
    failures: &[SnapshotFailure],
) -> Result<u64, StoreError> {
    let mut estimate = 64 * 1024_u64;
    for document in ready.values() {
        let unchanged = !rename_map.contains_key(&document.connector_key)
            && existing
                .get(&document.connector_key)
                .is_some_and(|current| {
                    current.active && current.revision_sha256 == Some(document.revision_sha256)
                });
        if unchanged {
            continue;
        }
        estimate = estimate
            .checked_add(
                document
                    .body_len
                    .checked_mul(6)
                    .ok_or(StoreError::IntegerOverflow)?,
            )
            .and_then(|value| value.checked_add(8 * 1024))
            .ok_or(StoreError::IntegerOverflow)?;
    }
    let tombstone_bytes = u64::try_from(tombstoned_documents)
        .map_err(|_| StoreError::IntegerOverflow)?
        .checked_mul(8 * 1024)
        .ok_or(StoreError::IntegerOverflow)?;
    estimate = estimate
        .checked_add(tombstone_bytes)
        .ok_or(StoreError::IntegerOverflow)?;
    for failure in failures {
        let row_bytes = failure
            .connector_key
            .len()
            .checked_add(failure.code.len())
            .and_then(|value| value.checked_add(failure.detail.len()))
            .and_then(|value| value.checked_add(1024))
            .ok_or(StoreError::IntegerOverflow)?;
        estimate = estimate
            .checked_add(u64::try_from(row_bytes).map_err(|_| StoreError::IntegerOverflow)?)
            .ok_or(StoreError::IntegerOverflow)?;
    }
    Ok(estimate)
}

fn create_generation(transaction: &Transaction<'_>, now: &str) -> Result<i64, StoreError> {
    let lease_nonce = *Uuid::new_v4().as_bytes();
    let profile = read_embedding_profile(transaction)?;
    if matches!(profile, IndexEmbeddingProfile::Pinned(_)) {
        clear_active_vector_membership(transaction)?;
    }
    let pipeline_fingerprint = pipeline_fingerprint_for(&profile);
    let (model_id, revision, model_fingerprint, dimension) = match &profile {
        IndexEmbeddingProfile::LexicalOnly => (None, None, None, None),
        IndexEmbeddingProfile::Pinned(pin) => (
            Some(pin.model_id()),
            Some(pin.upstream_revision()),
            Some(pin.model_fingerprint()),
            Some(i64::from(pin.dimension())),
        ),
    };
    transaction.execute(
        "INSERT INTO generations(
            state, created_at, owner_pid, lease_nonce,
            heartbeat_at, pipeline_fingerprint, embedding_model_id,
            embedding_revision, embedding_model_fingerprint, embedding_dimension,
            vector_state
         ) VALUES ('building', ?1, ?2, ?3, ?1, ?4, ?5, ?6, ?7, ?8, 'absent')",
        params![
            now,
            i64::from(std::process::id()),
            lease_nonce.as_slice(),
            pipeline_fingerprint.as_bytes().as_slice(),
            model_id,
            revision,
            model_fingerprint.map(|value| *value.as_bytes()),
            dimension,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn stage_document(
    transaction: &Transaction<'_>,
    forget_ledger: &ForgetLedger,
    source_id: SourceId,
    document_id: &[u8],
    document: &PreparedDocument,
    now: &str,
) -> Result<(i64, Vec<i64>), StoreError> {
    let connector_key_sha256 = Sha256Digest::of_bytes(&document.connector_key);
    if forget_ledger.suppresses(source_id, connector_key_sha256) {
        return Err(StoreError::ForgetTombstone);
    }
    let forgotten: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM forgotten_documents
             WHERE source_id = ?1
               AND connector_key_sha256 = ?2
         )",
        params![
            source_id.as_uuid().as_bytes().as_slice(),
            connector_key_sha256.as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;
    if forgotten {
        return Err(StoreError::ForgetTombstone);
    }

    transaction.execute(
        "INSERT INTO content_blobs(body_sha256, original_bytes)
         VALUES (?1, ?2)
         ON CONFLICT(body_sha256) DO NOTHING",
        params![document.body_sha256.as_bytes().as_slice(), document.body,],
    )?;
    let (content_blob_id, stored_body): (i64, Vec<u8>) = transaction.query_row(
        "SELECT id, original_bytes FROM content_blobs
             WHERE body_sha256 = ?1",
        [document.body_sha256.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if stored_body != document.body {
        return Err(StoreError::HashCollision);
    }

    transaction.execute(
        "INSERT INTO chunk_layouts(content_blob_id, chunker_fingerprint)
         VALUES (?1, ?2)
         ON CONFLICT(content_blob_id, chunker_fingerprint) DO NOTHING",
        params![
            content_blob_id,
            document.chunker_fingerprint.as_bytes().as_slice(),
        ],
    )?;
    let layout_id: i64 = transaction.query_row(
        "SELECT id FROM chunk_layouts
         WHERE content_blob_id = ?1 AND chunker_fingerprint = ?2",
        params![
            content_blob_id,
            document.chunker_fingerprint.as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;

    let existing_chunk_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM chunks WHERE chunk_layout_id = ?1",
        [layout_id],
        |row| row.get(0),
    )?;
    if existing_chunk_count == 0 {
        for chunk in &document.chunks {
            transaction.execute(
                "INSERT INTO chunks(
                    chunk_layout_id, ordinal, start_byte, end_byte,
                    start_line, end_line, body_text, content_sha256,
                    quote_bloom
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    layout_id,
                    i64::from(chunk.ordinal),
                    integer_from_u64(chunk.byte_span.start())?,
                    integer_from_u64(chunk.byte_span.end())?,
                    integer_from_u64(chunk.line_span.start())?,
                    integer_from_u64(chunk.line_span.end())?,
                    chunk.body_text,
                    chunk.content_sha256.as_bytes().as_slice(),
                    chunk.quote_bloom.as_slice(),
                ],
            )?;
        }
    } else if usize::try_from(existing_chunk_count).ok() != Some(document.chunks.len()) {
        return Err(StoreError::ChunkLayoutMismatch);
    }

    let mut statement = transaction.prepare(
        "SELECT id, ordinal, start_byte, end_byte, start_line, end_line,
                body_text, content_sha256, quote_bloom
         FROM chunks
         WHERE chunk_layout_id = ?1 ORDER BY ordinal",
    )?;
    let stored_chunks = statement
        .query_map([layout_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Vec<u8>>(7)?,
                row.get::<_, Vec<u8>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if stored_chunks.len() != document.chunks.len() {
        return Err(StoreError::ChunkLayoutMismatch);
    }
    for (stored, prepared) in stored_chunks.iter().zip(&document.chunks) {
        if stored.1 != i64::from(prepared.ordinal)
            || stored.2 != integer_from_u64(prepared.byte_span.start())?
            || stored.3 != integer_from_u64(prepared.byte_span.end())?
            || stored.4 != integer_from_u64(prepared.line_span.start())?
            || stored.5 != integer_from_u64(prepared.line_span.end())?
            || stored.6 != prepared.body_text
            || stored.7.as_slice() != prepared.content_sha256.as_bytes()
            || stored.8.as_slice() != prepared.quote_bloom
        {
            return Err(StoreError::ChunkLayoutMismatch);
        }
    }
    let chunk_ids = stored_chunks
        .iter()
        .map(|stored| stored.0)
        .collect::<Vec<_>>();

    let existing_version = transaction
        .query_row(
            "SELECT id, content_blob_id, source_uri, title,
                    metadata_json, source_updated_at
             FROM document_versions
             WHERE document_id = ?1 AND revision_sha256 = ?2",
            params![document_id, document.revision_sha256.as_bytes().as_slice(),],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    let version_id = match existing_version {
        Some(existing) => {
            if existing.1 != content_blob_id
                || existing.2 != document.source_uri
                || existing.3 != document.title
                || existing.4 != document.metadata_json
                || existing.5 != document.source_updated_at
            {
                return Err(StoreError::HashCollision);
            }
            existing.0
        }
        None => {
            transaction.execute(
                "INSERT INTO document_versions(
                    document_id, content_blob_id, revision_sha256,
                    source_uri, title, metadata_json,
                    source_updated_at, indexed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    document_id,
                    content_blob_id,
                    document.revision_sha256.as_bytes().as_slice(),
                    document.source_uri,
                    document.title,
                    document.metadata_json,
                    document.source_updated_at,
                    now,
                ],
            )?;
            transaction.last_insert_rowid()
        }
    };
    Ok((version_id, chunk_ids))
}

fn load_forget_ledger(database: &IndexDb) -> Result<ForgetLedger, StoreError> {
    let bytes: Vec<u8> = database.connection().query_row(
        "SELECT value FROM index_meta WHERE key = 'index_uuid'",
        [],
        |row| row.get(0),
    )?;
    let index_id = IndexId::from_uuid(
        Uuid::from_slice(&bytes).map_err(|_| StoreError::InvalidMetadata("index_uuid"))?,
    );
    ForgetLedger::read(database.path(), index_id)
}

fn replace_active_passages(
    transaction: &Transaction<'_>,
    source_id: SourceId,
    document_id: &[u8],
    version_id: i64,
    document: &PreparedDocument,
    chunk_ids: &[i64],
) -> Result<(), StoreError> {
    delete_active_passages(transaction, document_id)?;
    for (chunk, chunk_id) in document.chunks.iter().zip(chunk_ids) {
        transaction.execute(
            "INSERT INTO active_passages(
                document_id, document_version_id, chunk_id, source_id
             ) VALUES (?1, ?2, ?3, ?4)",
            params![document_id, version_id, chunk_id, uuid_bytes(source_id),],
        )?;
        let passage_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO passages_fts(rowid, title, source_uri, body)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                passage_id,
                document.title,
                document.source_uri,
                chunk.body_text,
            ],
        )?;
        for (literal, field) in &chunk.literals {
            transaction.execute(
                "INSERT INTO passage_literals(passage_id, literal, field)
                 VALUES (?1, ?2, ?3)",
                params![passage_id, literal, field.as_str()],
            )?;
        }
    }
    Ok(())
}

fn delete_active_passages(
    transaction: &Transaction<'_>,
    document_id: &[u8],
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM passages_fts
         WHERE rowid IN (
             SELECT id FROM active_passages WHERE document_id = ?1
         )",
        [document_id],
    )?;
    transaction.execute(
        "DELETE FROM active_passages WHERE document_id = ?1",
        [document_id],
    )?;
    Ok(())
}

fn update_source_status(
    transaction: &Transaction<'_>,
    source_id: SourceId,
    now: &str,
    accepted_documents: usize,
    failure: Option<&SnapshotFailure>,
) -> Result<(), StoreError> {
    match failure {
        Some(failure) if accepted_documents == 0 => {
            transaction.execute(
                "UPDATE sources
                 SET last_error_code = ?1, last_error_detail = ?2,
                     last_error_at = ?3
                 WHERE id = ?4",
                params![failure.code, failure.detail, now, uuid_bytes(source_id),],
            )?;
        }
        Some(failure) => {
            transaction.execute(
                "UPDATE sources
                 SET last_success_at = ?1, last_error_code = ?2,
                     last_error_detail = ?3, last_error_at = ?1
                 WHERE id = ?4",
                params![now, failure.code, failure.detail, uuid_bytes(source_id),],
            )?;
        }
        None => {
            transaction.execute(
                "UPDATE sources
                 SET last_success_at = ?1, last_error_code = NULL,
                     last_error_detail = NULL, last_error_at = NULL
                 WHERE id = ?2",
                params![now, uuid_bytes(source_id)],
            )?;
        }
    }
    Ok(())
}

fn current_outcome(
    transaction: &Transaction<'_>,
    source_id: SourceId,
    generation_id: Option<i64>,
    counts: OutcomeCounts,
) -> Result<IngestOutcome, StoreError> {
    let active_documents = usize_from_count(transaction.query_row(
        "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
        [],
        |row| row.get(0),
    )?)?;
    let active_passages = usize_from_count(transaction.query_row(
        "SELECT COUNT(*) FROM active_passages",
        [],
        |row| row.get(0),
    )?)?;
    Ok(IngestOutcome {
        generation_id,
        changed_documents: counts.changed_documents,
        unchanged_documents: counts.unchanged_documents,
        tombstoned_documents: counts.tombstoned_documents,
        carried_forward_documents: counts.carried_forward_documents,
        failed_documents: counts.failed_documents,
        active_documents,
        active_passages,
        index_epoch: metadata_u64(transaction, "index_epoch")?,
        source_outcomes: vec![SourceIngestOutcome {
            source_id,
            state: match (
                generation_id.is_some(),
                counts.accepted_documents == 0,
                counts.failed_documents == 0,
            ) {
                (_, _, true) => SourceIngestState::Success,
                (false, true, false) => SourceIngestState::Failed,
                (_, _, false) => SourceIngestState::Partial,
            },
            accepted_documents: counts.accepted_documents,
            failed_documents: counts.failed_documents,
            carried_forward_documents: counts.carried_forward_documents,
        }],
        storage_preflight: None,
    })
}

fn set_metadata(transaction: &Transaction<'_>, key: &str, value: &[u8]) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE index_meta SET value = ?1 WHERE key = ?2",
        params![value, key],
    )?;
    Ok(())
}

fn metadata_u64(transaction: &Transaction<'_>, key: &'static str) -> Result<u64, StoreError> {
    let bytes: Vec<u8> = transaction.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(StoreError::InvalidMetadata(key))
}

fn digest_from_blob(bytes: &[u8]) -> Result<Sha256Digest, StoreError> {
    let value: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::InvalidMetadata("digest blob"))?;
    Ok(Sha256Digest::from_bytes(value))
}

fn uuid_bytes<T>(value: T) -> [u8; 16]
where
    T: IntoUuid,
{
    *value.into_uuid().as_bytes()
}

trait IntoUuid {
    fn into_uuid(self) -> Uuid;
}

impl IntoUuid for SourceId {
    fn into_uuid(self) -> Uuid {
        self.as_uuid()
    }
}

impl IntoUuid for ProjectId {
    fn into_uuid(self) -> Uuid {
        self.as_uuid()
    }
}

fn integer_from_u64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}

fn usize_from_count(value: i64) -> Result<usize, StoreError> {
    usize::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}
