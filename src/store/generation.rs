use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use serde_json_canonicalizer::to_string as to_canonical_json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::{ByteSpan, LineSpan, ProjectId, SafeSlug, Sha256Digest, SourceId};
use crate::ingest::{
    ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, chunk_bytes,
    extract_identifier_literals, repo_uri, revision_sha256,
};
use crate::store::WriterLock;
use crate::store::capacity::StoragePreflight;
use crate::store::doctor::Doctor;
use crate::store::open::{IndexDb, StoreError};
use crate::store::schema::{chunker_fingerprint, pipeline_fingerprint};

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
        validate_snapshot(documents, failures)?;
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
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(self.path())?;
        validate_scope(scope)?;
        validate_summary_snapshot(documents, failures)?;

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
            .collect::<BTreeSet<_>>();
        let initially_absent = existing
            .values()
            .filter(|document| document.active && !observed.contains(&document.connector_key))
            .map(|document| document.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let rename_map = detect_unambiguous_renames(&existing, &ready, &initially_absent);
        let renamed_old_keys = rename_map.values().cloned().collect::<BTreeSet<_>>();
        let absent = initially_absent
            .difference(&renamed_old_keys)
            .cloned()
            .collect::<Vec<_>>();
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
        let (requires_empty_snapshot_confirmation, requires_mass_delete_confirmation) =
            deletion_requirements(
                prior_active_documents,
                projected_active_documents,
                absent.len(),
            );
        let plan = IngestPlan {
            new_documents,
            changed_documents,
            renamed_documents: rename_map.len(),
            unchanged_documents,
            tombstoned_documents: absent.len(),
            carried_forward_documents,
            failed_documents: failures.len(),
            prior_active_documents,
            projected_active_documents,
            would_create_generation: changed_documents != 0 || !absent.is_empty(),
            requires_empty_snapshot_confirmation,
            requires_mass_delete_confirmation,
            estimated_write_bytes: estimate_write_bytes(
                &ready,
                &existing,
                &rename_map,
                absent.len(),
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
        validate_snapshot(documents, failures)?;
        let summaries = documents
            .iter()
            .map(PreparedDocumentSummary::from_document)
            .collect::<Result<Vec<_>, _>>()?;
        let documents_by_key = documents
            .iter()
            .map(|document| (document.connector_key.clone(), document))
            .collect::<BTreeMap<_, _>>();
        self.apply_filesystem_summaries_under_lock(
            writer_lock,
            scope,
            &summaries,
            failures,
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

    pub(crate) fn apply_filesystem_summaries_under_lock<F>(
        &mut self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
        documents: &[PreparedDocumentSummary],
        failures: &[SnapshotFailure],
        confirmations: DeleteConfirmations,
        mut load_document: F,
    ) -> Result<IngestOutcome, StoreError>
    where
        F: FnMut(&PreparedDocumentSummary) -> Result<PreparedDocument, StoreError>,
    {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(self.path())?;
        validate_scope(scope)?;
        validate_summary_snapshot(documents, failures)?;

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
            .collect::<BTreeSet<_>>();

        let initially_absent = existing
            .values()
            .filter(|document| document.active && !observed.contains(&document.connector_key))
            .map(|document| document.connector_key.clone())
            .collect::<BTreeSet<_>>();
        let rename_map = detect_unambiguous_renames(&existing, &ready, &initially_absent);
        let renamed_old_keys = rename_map.values().cloned().collect::<BTreeSet<_>>();
        let absent = initially_absent
            .difference(&renamed_old_keys)
            .cloned()
            .collect::<Vec<_>>();

        enforce_deletion_budget(
            existing.values().filter(|document| document.active).count(),
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
        let tombstoned_documents = absent.len();
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
            validate_document(&document)?;
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
            let (version_id, chunk_ids) =
                stage_document(&transaction, &document_id, &document, &now)?;
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

        for connector_key in &absent {
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

    pub(crate) fn record_filesystem_source_failure_under_lock(
        &mut self,
        writer_lock: &WriterLock,
        scope: &FilesystemScope,
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
                WHERE id = ?1 AND kind = 'filesystem' AND name = ?2
                  AND logical_uri = ?3 AND config_json = ?4
            )",
            params![
                uuid_bytes(scope.source_id),
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

fn validate_scope(scope: &FilesystemScope) -> Result<(), StoreError> {
    if scope.source_logical_uri.is_empty() {
        return Err(StoreError::InvalidPreparedDocument(
            "source logical URI is empty",
        ));
    }
    let config: Value = serde_json::from_str(&scope.source_config_json)
        .map_err(|_| StoreError::InvalidPreparedDocument("source config is not JSON"))?;
    if !config.is_object() {
        return Err(StoreError::InvalidPreparedDocument(
            "source config is not an object",
        ));
    }
    Ok(())
}

fn validate_snapshot(
    documents: &[PreparedDocument],
    failures: &[SnapshotFailure],
) -> Result<(), StoreError> {
    let mut keys = BTreeSet::new();
    for document in documents {
        if !keys.insert(document.connector_key.clone()) {
            return Err(StoreError::DuplicateConnectorKey);
        }
        validate_document(document)?;
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
    Ok(())
}

fn validate_summary_snapshot(
    documents: &[PreparedDocumentSummary],
    failures: &[SnapshotFailure],
) -> Result<(), StoreError> {
    let mut keys = BTreeSet::new();
    for document in documents {
        if document.connector_key.is_empty()
            || document.connector_key.len() > 4096
            || document.source_uri != repo_uri(&document.connector_key)
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
    Ok(())
}

fn validate_document(document: &PreparedDocument) -> Result<(), StoreError> {
    if document.connector_key.is_empty()
        || document.connector_key.len() > 4096
        || document.source_uri.is_empty()
        || document.source_uri != repo_uri(&document.connector_key)
        || document.title.is_empty()
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

    let chunk_kind = ChunkKind::from_path(Path::new(&document.source_uri)).ok_or(
        StoreError::InvalidPreparedDocument("source type is unsupported"),
    )?;
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

fn ensure_scope(
    transaction: &Transaction<'_>,
    scope: &FilesystemScope,
    now: &str,
) -> Result<(), StoreError> {
    let source_id = uuid_bytes(scope.source_id);
    let source_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))?;
    if source_count == 0 {
        transaction.execute(
            "INSERT INTO sources(
                id, kind, name, logical_uri, config_json, created_at
             ) VALUES (?1, 'filesystem', ?2, ?3, ?4, ?5)",
            params![
                source_id,
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
                WHERE id = ?1 AND kind = 'filesystem' AND name = ?2
                  AND logical_uri = ?3 AND config_json = ?4
            )",
            params![
                source_id,
                scope.source_name.to_string(),
                scope.source_logical_uri,
                scope.source_config_json,
            ],
            |row| row.get(0),
        )?;
        if source_count != 1 || !matches {
            return Err(StoreError::ScopeConflict);
        }
    }

    let project_id = uuid_bytes(scope.project_id);
    let project_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
    if project_count == 0 {
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
        if project_count != 1 || !matches {
            return Err(StoreError::ScopeConflict);
        }
    }

    transaction.execute(
        "INSERT OR IGNORE INTO project_sources(project_id, source_id)
         VALUES (?1, ?2)",
        params![project_id, source_id],
    )?;
    Ok(())
}

fn validate_existing_scope(
    connection: &rusqlite::Connection,
    scope: &FilesystemScope,
) -> Result<(), StoreError> {
    let scope_counts = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM sources),
             (SELECT COUNT(*) FROM projects),
             (SELECT COUNT(*) FROM project_sources)",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    if scope_counts == (0, 0, 0) {
        return Ok(());
    }

    let source_matches: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sources
            WHERE id = ?1 AND kind = 'filesystem' AND name = ?2
              AND logical_uri = ?3 AND config_json = ?4
        )",
        params![
            uuid_bytes(scope.source_id),
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
    transaction.execute(
        "INSERT INTO generations(
            state, created_at, owner_pid, lease_nonce,
            heartbeat_at, pipeline_fingerprint
         ) VALUES ('building', ?1, ?2, ?3, ?1, ?4)",
        params![
            now,
            i64::from(std::process::id()),
            lease_nonce.as_slice(),
            pipeline_fingerprint().as_bytes().as_slice(),
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn stage_document(
    transaction: &Transaction<'_>,
    document_id: &[u8],
    document: &PreparedDocument,
    now: &str,
) -> Result<(i64, Vec<i64>), StoreError> {
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
