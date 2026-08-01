use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json_canonicalizer::{to_string as to_canonical_json, to_vec as to_canonical_vec};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::domain::{ByteSpan, Citation, DocumentId, IndexId, ProjectId, Sha256Digest, SourceId};
use crate::store::capacity::{StoragePreflight, StoragePreflightError};
use crate::store::doctor::{Doctor, inspect_migration_source};
use crate::store::open::{IndexDb, OpenMode, StoreError};
use crate::store::schema::{
    MIGRATIONS, SCHEMA_VERSION, migration_checksum, schema_checksum, schema_checksum_through,
};
use crate::store::{
    ForgetLedger, ForgetLedgerTarget, ForgetOperationState, ReplacementEpoch, ReplacementLock,
    WriterLock,
};

const MAX_PLAN_BYTES: u64 = 64 * 1024 * 1024;
const BACKUP_PAGES_PER_STEP: i32 = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackupReceipt {
    pub format: &'static str,
    pub index_id: IndexId,
    pub schema_version: u32,
    pub index_epoch: u64,
    pub output: PathBuf,
    pub file_bytes: u64,
    pub file_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEnvelope<T> {
    pub format: String,
    pub plan_hash: Sha256Digest,
    pub plan: T,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrunePlan {
    pub index_id: IndexId,
    pub schema_version: u32,
    pub schema_checksum: Sha256Digest,
    pub pipeline_fingerprint: Sha256Digest,
    pub index_epoch: u64,
    pub active_generation: Option<u64>,
    pub history_floor_epoch: u64,
    pub before: String,
    pub keep_latest: u64,
    pub database_file_bytes: u64,
    pub estimated_transaction_bytes: u64,
    pub logical_reclaimable_bytes: u64,
    pub generations_removed: u64,
    pub affected_generations: Vec<PrunedGeneration>,
    pub affected_citation_count: u64,
    pub affected_revisions: Vec<PrunedRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrunedRevision {
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub revision_sha256: Sha256Digest,
    pub source_uri: String,
    pub indexed_at: String,
    pub canonical_stored_chunk_citations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrunedGeneration {
    pub generation_id: u64,
    pub state: String,
    pub created_at: String,
    pub committed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationPlan {
    pub index_name: String,
    pub index_id: IndexId,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
    pub from_schema_checksum: Sha256Digest,
    pub to_schema_checksum: Sha256Digest,
    pub pipeline_fingerprint: Sha256Digest,
    pub database_file_bytes: u64,
    pub estimated_transaction_bytes: u64,
    pub steps: Vec<MigrationStep>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationStep {
    pub version: u32,
    pub checksum: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetPlan {
    pub index_id: IndexId,
    pub project_id: ProjectId,
    pub schema_version: u32,
    pub schema_checksum: Sha256Digest,
    pub pipeline_fingerprint: Sha256Digest,
    pub index_epoch: u64,
    pub history_floor_epoch: u64,
    pub replacement_epoch: u64,
    pub active_generation: u64,
    pub database_file_bytes: u64,
    pub estimated_rewrite_bytes: u64,
    pub affected_revisions: Vec<ForgottenRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgottenRevision {
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub connector_key_sha256: Sha256Digest,
    pub revision_sha256: Sha256Digest,
    pub source_uri: String,
    pub original_body_bytes: u64,
    pub canonical_stored_chunk_citations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestorePlan {
    pub index_id: IndexId,
    pub schema_version: u32,
    pub schema_checksum: Sha256Digest,
    pub pipeline_fingerprint: Sha256Digest,
    pub forget_plan_hash: Sha256Digest,
    pub pre_forget_index_epoch: u64,
    pub post_forget_index_epoch: u64,
    pub post_restore_index_epoch: u64,
    pub pre_forget_replacement_epoch: u64,
    pub post_forget_replacement_epoch: u64,
    pub post_restore_replacement_epoch: u64,
    pub recovery_backup_sha256: Sha256Digest,
    pub post_forget_database_sha256: Sha256Digest,
    pub post_forget_database_bytes: u64,
    pub affected_revisions: Vec<ForgottenRevision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PruneOutcome {
    pub plan_hash: Sha256Digest,
    pub backup: BackupReceipt,
    pub history_floor_epoch: u64,
    pub affected_revisions: u64,
    pub affected_citations: u64,
    pub logical_reclaimable_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MigrationOutcome {
    pub plan_hash: Sha256Digest,
    pub backup: BackupReceipt,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForgetOutcome {
    pub plan_hash: Sha256Digest,
    pub recovery_backup: BackupReceipt,
    pub restore_plan_hash: Sha256Digest,
    pub restore_plan: PathBuf,
    pub replacement_epoch: u64,
    pub affected_revisions: u64,
    pub physically_rewritten_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RestoreOutcome {
    pub plan_hash: Sha256Digest,
    pub safety_backup: BackupReceipt,
    pub replacement_epoch: u64,
    pub restored_revisions: u64,
}

pub fn create_backup(
    database_path: &Path,
    output: &Path,
    lock_timeout: Duration,
) -> Result<BackupReceipt, MaintenanceError> {
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    let report = Doctor::run(database_path)?;
    backup_under_lock(
        &database,
        output,
        &writer_lock,
        report.index_id,
        report.schema_version,
        read_metadata_u64(database.connection(), "index_epoch")?,
        BackupVerifier::Current,
    )
}

/// Reconstruct a receipt for an already-published current-schema backup.
///
/// This is used only to resume registry publication after a process exits
/// between the durable backup link and its managed-inventory update.
pub fn inspect_backup(
    path: &Path,
    expected_index_id: IndexId,
) -> Result<BackupReceipt, MaintenanceError> {
    remove_closed_backup_sidecars(path)?;
    ensure_sidecars_absent(path)?;
    let report = Doctor::run(path)?;
    if report.index_id != expected_index_id {
        return Err(MaintenanceError::BackupIdentityMismatch);
    }
    let database = IndexDb::open_existing(path, OpenMode::ReadOnly)?;
    let index_epoch = read_metadata_u64(database.connection(), "index_epoch")?;
    drop(database);
    remove_closed_backup_sidecars(path)?;
    ensure_sidecars_absent(path)?;
    Ok(BackupReceipt {
        format: "hsum.backup.v1",
        index_id: expected_index_id,
        schema_version: report.schema_version,
        index_epoch,
        output: path.to_path_buf(),
        file_bytes: fs::metadata(path)?.len(),
        file_sha256: hash_file(path)?,
    })
}

pub fn plan_prune(
    database_path: &Path,
    before: OffsetDateTime,
    keep_latest: u64,
    lock_timeout: Duration,
) -> Result<PlanEnvelope<PrunePlan>, MaintenanceError> {
    if keep_latest == 0 {
        return Err(MaintenanceError::InvalidPruneSelector);
    }
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    Doctor::run(database_path)?;
    if !writer_lock.protects(database.path()) {
        return Err(StoreError::WriterLockMismatch.into());
    }
    build_prune_plan(&database, before, keep_latest)
}

pub fn apply_prune(
    database_path: &Path,
    plan: &PlanEnvelope<PrunePlan>,
    confirmed_hash: Sha256Digest,
    backup_output: &Path,
    lock_timeout: Duration,
) -> Result<PruneOutcome, MaintenanceError> {
    validate_plan_envelope("hsum.prune-plan.v1", plan, confirmed_hash)?;
    if plan.plan.generations_removed == 0 && plan.plan.affected_revisions.is_empty() {
        return Err(MaintenanceError::NoMaintenanceWork);
    }

    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let mut database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    if database.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM prune_runs WHERE plan_hash = ?1)",
        [plan.plan_hash.as_bytes().as_slice()],
        |row| row.get::<_, bool>(0),
    )? {
        validate_completed_prune(&database, plan)?;
        let backup = existing_backup_receipt(
            backup_output,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
        )?;
        Doctor::run(database_path)?;
        return prune_outcome(plan, backup);
    }
    let before = OffsetDateTime::parse(&plan.plan.before, &Rfc3339)
        .map_err(|_| MaintenanceError::InvalidPruneSelector)?;
    let live_plan = build_prune_plan(&database, before, plan.plan.keep_latest)?;
    if &live_plan != plan {
        return Err(MaintenanceError::PlanStale);
    }
    StoragePreflight::run(database.path(), plan.plan.estimated_transaction_bytes, None)?;
    let backup = if backup_output.exists() {
        existing_backup_receipt(
            backup_output,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
        )?
    } else {
        backup_under_lock(
            &database,
            backup_output,
            &writer_lock,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
            BackupVerifier::Current,
        )?
    };
    let manifest_json = to_canonical_json(plan)?;
    let active_generation = plan
        .plan
        .active_generation
        .map(integer_from_u64)
        .transpose()?;
    let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let new_history_floor_epoch = if plan.plan.index_epoch == 0 {
        plan.plan.history_floor_epoch
    } else {
        plan.plan.index_epoch
    };
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;

    transaction.execute(
        "INSERT INTO prune_runs(
             plan_hash, applied_at, history_floor_epoch,
             affected_revision_count, affected_citation_count,
             logical_reclaimable_bytes, manifest_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            plan.plan_hash.as_bytes().as_slice(),
            applied_at,
            integer_from_u64(new_history_floor_epoch)?,
            integer_from_usize(plan.plan.affected_revisions.len())?,
            integer_from_u64(plan.plan.affected_citation_count)?,
            integer_from_u64(plan.plan.logical_reclaimable_bytes)?,
            manifest_json,
        ],
    )?;
    for revision in &plan.plan.affected_revisions {
        transaction.execute(
            "INSERT INTO pruned_revision_namespaces(
                 plan_hash, source_id, document_id, revision_sha256
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                plan.plan_hash.as_bytes().as_slice(),
                revision.source_id.as_uuid().as_bytes().as_slice(),
                revision.document_id.as_uuid().as_bytes().as_slice(),
                revision.revision_sha256.as_bytes().as_slice(),
            ],
        )?;
    }

    if let Some(active_generation) = active_generation {
        transaction.execute(
            "UPDATE document_heads SET generation_id = ?1",
            [active_generation],
        )?;
        transaction.execute("DELETE FROM generation_changes", [])?;
        transaction.execute(
            "DELETE FROM generations WHERE id != ?1",
            [active_generation],
        )?;
        transaction.execute(
            "INSERT INTO generation_changes(
                 generation_id, document_id, prior_version_id, next_version_id, next_state
             )
             SELECT ?1, document_id, NULL, document_version_id, state
             FROM document_heads
             ORDER BY document_id",
            [active_generation],
        )?;
    } else {
        transaction.execute("DELETE FROM generation_changes", [])?;
        transaction.execute("DELETE FROM generations", [])?;
    }
    for revision in &plan.plan.affected_revisions {
        if transaction.execute(
            "DELETE FROM document_versions
             WHERE document_id = ?1
               AND revision_sha256 = ?2
               AND NOT EXISTS (
                   SELECT 1 FROM document_heads
                   WHERE document_version_id = document_versions.id
               )",
            params![
                revision.document_id.as_uuid().as_bytes().as_slice(),
                revision.revision_sha256.as_bytes().as_slice(),
            ],
        )? != 1
        {
            return Err(MaintenanceError::PlanStale);
        }
    }
    transaction.execute(
        "DELETE FROM chunks
         WHERE chunk_layout_id IN (
             SELECT layout.id
             FROM chunk_layouts AS layout
             WHERE NOT EXISTS (
                 SELECT 1 FROM document_versions AS version
                 WHERE version.content_blob_id = layout.content_blob_id
             )
         )",
        [],
    )?;
    transaction.execute(
        "DELETE FROM chunk_layouts
         WHERE NOT EXISTS (
             SELECT 1 FROM chunks WHERE chunks.chunk_layout_id = chunk_layouts.id
         )",
        [],
    )?;
    transaction.execute(
        "DELETE FROM content_blobs
         WHERE NOT EXISTS (
             SELECT 1 FROM document_versions
             WHERE document_versions.content_blob_id = content_blobs.id
         )",
        [],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'history_floor_epoch'",
        [new_history_floor_epoch.to_string()],
    )?;
    transaction.commit()?;
    database.verify_live_identity()?;
    drop(database);
    Doctor::run(database_path)?;

    prune_outcome(plan, backup)
}

pub fn plan_forget(
    database_path: &Path,
    project_id: ProjectId,
    citations: &[Citation],
    lock_timeout: Duration,
) -> Result<PlanEnvelope<ForgetPlan>, MaintenanceError> {
    if citations.is_empty() {
        return Err(MaintenanceError::NoMaintenanceWork);
    }
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadOnly)?;
    Doctor::run(database_path)?;
    if !writer_lock.protects(database.path()) {
        return Err(StoreError::WriterLockMismatch.into());
    }
    let requested = citations
        .iter()
        .map(|citation| RequestedRevision {
            index_id: citation.index_id,
            source_id: citation.source_id,
            document_id: citation.document_id,
            revision_sha256: citation.revision,
        })
        .collect::<Vec<_>>();
    build_forget_plan(&database, project_id, &requested)
}

pub fn apply_forget(
    database_path: &Path,
    plan: &PlanEnvelope<ForgetPlan>,
    confirmed_hash: Sha256Digest,
    recovery_backup_output: &Path,
    restore_plan_output: &Path,
    lock_timeout: Duration,
) -> Result<ForgetOutcome, MaintenanceError> {
    apply_forget_with_observer(
        database_path,
        plan,
        confirmed_hash,
        recovery_backup_output,
        restore_plan_output,
        lock_timeout,
        |_| {},
    )
}

#[doc(hidden)]
pub fn apply_forget_with_observer(
    database_path: &Path,
    plan: &PlanEnvelope<ForgetPlan>,
    confirmed_hash: Sha256Digest,
    recovery_backup_output: &Path,
    restore_plan_output: &Path,
    lock_timeout: Duration,
    mut observer: impl FnMut(&'static str),
) -> Result<ForgetOutcome, MaintenanceError> {
    validate_plan_envelope("hsum.forget-plan.v1", plan, confirmed_hash)?;
    if plan.plan.affected_revisions.is_empty() {
        return Err(MaintenanceError::NoMaintenanceWork);
    }
    if recovery_backup_output == restore_plan_output {
        return Err(MaintenanceError::OutputPathsOverlap);
    }

    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    let operation_id = operation_id_from_plan_hash(plan.plan_hash);
    let post_replacement_epoch = plan
        .plan
        .replacement_epoch
        .checked_add(1)
        .ok_or(StoreError::IntegerOverflow)?;

    if database_has_forget_run(&database, plan.plan_hash)? {
        let recovery_backup = existing_backup_receipt(
            recovery_backup_output,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
        )?;
        let restore_plan = expected_restore_plan(
            plan,
            &recovery_backup,
            database_path,
            post_replacement_epoch,
        )?;
        let stored_restore_plan: PlanEnvelope<RestorePlan> = read_plan(restore_plan_output)?;
        validate_plan_envelope(
            "hsum.restore-plan.v1",
            &stored_restore_plan,
            stored_restore_plan.plan_hash,
        )?;
        if stored_restore_plan != restore_plan {
            return Err(MaintenanceError::RestoreStateMismatch);
        }
        validate_restore_live_state(&database, &restore_plan)?;

        let replacement_lock = ReplacementLock::acquire(database_path, lock_timeout)?;
        if !replacement_lock.protects(database_path) {
            return Err(StoreError::ReplacementLockMismatch.into());
        }
        database.verify_live_identity()?;
        checkpoint_and_close(database)?;
        if ReplacementEpoch::read(database_path, plan.plan.index_id)?
            != Some(post_replacement_epoch)
        {
            ReplacementEpoch::publish(
                database_path,
                plan.plan.index_id,
                plan.plan.replacement_epoch,
                post_replacement_epoch,
            )?;
        }
        append_forget_state(
            database_path,
            &writer_lock,
            plan,
            operation_id,
            ForgetOperationState::ReplacementActivated,
        )?;
        Doctor::run(database_path)?;
        append_forget_state(
            database_path,
            &writer_lock,
            plan,
            operation_id,
            ForgetOperationState::Committed,
        )?;
        observer("forget-recovered");
        return forget_outcome(plan, recovery_backup, &restore_plan, restore_plan_output);
    }

    let requested = plan
        .plan
        .affected_revisions
        .iter()
        .map(|revision| RequestedRevision {
            index_id: plan.plan.index_id,
            source_id: revision.source_id,
            document_id: revision.document_id,
            revision_sha256: revision.revision_sha256,
        })
        .collect::<Vec<_>>();
    let live_plan = build_forget_plan(&database, plan.plan.project_id, &requested)?;
    if &live_plan != plan {
        return Err(MaintenanceError::PlanStale);
    }
    StoragePreflight::run(database.path(), plan.plan.estimated_rewrite_bytes, None)?;
    append_forget_state(
        database_path,
        &writer_lock,
        plan,
        operation_id,
        ForgetOperationState::LedgerPrepared,
    )?;
    observer("ledger-prepared");
    let recovery_backup = if recovery_backup_output.exists() {
        existing_backup_receipt(
            recovery_backup_output,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
        )?
    } else {
        backup_under_lock(
            &database,
            recovery_backup_output,
            &writer_lock,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
            BackupVerifier::Current,
        )?
    };
    observer("recovery-backup-created");

    let staging = forget_staging_path(database.path(), plan.plan_hash)?;
    if !staging.exists() {
        let scratch = replacement_staging_path(database.path(), "forget-scratch")?;
        let scratch_cleanup = PartialBackup::new(scratch.clone());
        let _staging_receipt = backup_under_lock(
            &database,
            &scratch,
            &writer_lock,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.index_epoch,
            BackupVerifier::Current,
        )?;
        apply_forget_to_staging(
            &scratch,
            plan,
            operation_id,
            recovery_backup.file_sha256,
            post_replacement_epoch,
        )?;
        compact_database(&scratch)?;
        publish_staging(&scratch, &staging)?;
        scratch_cleanup.disarm();
    }
    let restore_plan =
        expected_restore_plan(plan, &recovery_backup, &staging, post_replacement_epoch)?;
    let staged_database = IndexDb::open_existing(&staging, OpenMode::ReadWrite)?;
    validate_restore_live_state(&staged_database, &restore_plan)?;
    drop(staged_database);
    if restore_plan_output.exists() {
        let stored: PlanEnvelope<RestorePlan> = read_plan(restore_plan_output)?;
        validate_plan_envelope("hsum.restore-plan.v1", &stored, stored.plan_hash)?;
        if stored != restore_plan {
            return Err(MaintenanceError::RestoreStateMismatch);
        }
    } else {
        write_plan(restore_plan_output, &restore_plan)?;
    }
    append_forget_state(
        database_path,
        &writer_lock,
        plan,
        operation_id,
        ForgetOperationState::ReplacementBuilt,
    )?;
    observer("replacement-prepared");

    let replacement_lock = ReplacementLock::acquire(database_path, lock_timeout)?;
    if !replacement_lock.protects(database_path) {
        return Err(StoreError::ReplacementLockMismatch.into());
    }
    append_forget_state(
        database_path,
        &writer_lock,
        plan,
        operation_id,
        ForgetOperationState::OldReadersFenced,
    )?;
    observer("old-readers-fenced");
    database.verify_live_identity()?;
    checkpoint_and_close(database)?;
    publish_replacement(&staging, database_path)?;
    ReplacementEpoch::publish(
        database_path,
        plan.plan.index_id,
        plan.plan.replacement_epoch,
        post_replacement_epoch,
    )?;
    append_forget_state(
        database_path,
        &writer_lock,
        plan,
        operation_id,
        ForgetOperationState::ReplacementActivated,
    )?;
    observer("replacement-published");
    let report = Doctor::run(database_path)?;
    if report.index_id != plan.plan.index_id {
        return Err(MaintenanceError::BackupIdentityMismatch);
    }
    remove_closed_backup_sidecars(database_path)?;
    ensure_sidecars_absent(database_path)?;
    append_forget_state(
        database_path,
        &writer_lock,
        plan,
        operation_id,
        ForgetOperationState::Committed,
    )?;
    observer("forget-committed");

    forget_outcome(plan, recovery_backup, &restore_plan, restore_plan_output)
}

pub fn apply_restore(
    database_path: &Path,
    plan: &PlanEnvelope<RestorePlan>,
    confirmed_hash: Sha256Digest,
    recovery_backup: &Path,
    safety_backup_output: &Path,
    lock_timeout: Duration,
) -> Result<RestoreOutcome, MaintenanceError> {
    validate_plan_envelope("hsum.restore-plan.v1", plan, confirmed_hash)?;
    if recovery_backup == safety_backup_output {
        return Err(MaintenanceError::OutputPathsOverlap);
    }
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    if database_has_restore_run(&database, plan.plan_hash)? {
        validate_recovery_backup(recovery_backup, plan)?;
        let safety_backup = existing_backup_receipt(
            safety_backup_output,
            plan.plan.index_id,
            plan.plan.schema_version,
            plan.plan.post_forget_index_epoch,
        )?;
        validate_completed_restore_state(&database, plan, safety_backup.file_sha256)?;
        let replacement_lock = ReplacementLock::acquire(database_path, lock_timeout)?;
        if !replacement_lock.protects(database_path) {
            return Err(StoreError::ReplacementLockMismatch.into());
        }
        database.verify_live_identity()?;
        checkpoint_and_close(database)?;
        if ReplacementEpoch::read(database_path, plan.plan.index_id)?
            != Some(plan.plan.post_restore_replacement_epoch)
        {
            ReplacementEpoch::publish(
                database_path,
                plan.plan.index_id,
                plan.plan.post_forget_replacement_epoch,
                plan.plan.post_restore_replacement_epoch,
            )?;
        }
        append_restore_state(database_path, &writer_lock, plan)?;
        Doctor::run(database_path)?;
        return Ok(RestoreOutcome {
            plan_hash: plan.plan_hash,
            safety_backup,
            replacement_epoch: plan.plan.post_restore_replacement_epoch,
            restored_revisions: u64::try_from(plan.plan.affected_revisions.len())
                .map_err(|_| StoreError::IntegerOverflow)?,
        });
    }
    validate_restore_live_state(&database, plan)?;
    checkpoint_and_close(database)?;
    if hash_file(database_path)? != plan.plan.post_forget_database_sha256
        || fs::metadata(database_path)?.len() != plan.plan.post_forget_database_bytes
    {
        return Err(MaintenanceError::RestoreStateMismatch);
    }

    validate_recovery_backup(recovery_backup, plan)?;
    let database = IndexDb::open_existing(database_path, OpenMode::ReadWrite)?;
    let estimated_rewrite_bytes = fs::metadata(recovery_backup)?
        .len()
        .checked_mul(2)
        .ok_or(StoreError::IntegerOverflow)?;
    StoragePreflight::run(database.path(), estimated_rewrite_bytes, None)?;
    let safety_backup = backup_under_lock(
        &database,
        safety_backup_output,
        &writer_lock,
        plan.plan.index_id,
        plan.plan.schema_version,
        plan.plan.post_forget_index_epoch,
        BackupVerifier::Current,
    )?;
    let staging = replacement_staging_path(database.path(), "restore")?;
    let staging_cleanup = PartialBackup::new(staging.clone());
    clone_closed_database(recovery_backup, &staging)?;
    apply_restore_to_staging(&staging, plan, safety_backup.file_sha256)?;
    compact_database(&staging)?;

    let replacement_lock = ReplacementLock::acquire(database_path, lock_timeout)?;
    if !replacement_lock.protects(database_path) {
        return Err(StoreError::ReplacementLockMismatch.into());
    }
    database.verify_live_identity()?;
    checkpoint_and_close(database)?;
    if hash_file(database_path)? != plan.plan.post_forget_database_sha256 {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    publish_replacement(&staging, database_path)?;
    staging_cleanup.disarm();
    ReplacementEpoch::publish(
        database_path,
        plan.plan.index_id,
        plan.plan.post_forget_replacement_epoch,
        plan.plan.post_restore_replacement_epoch,
    )?;
    append_restore_state(database_path, &writer_lock, plan)?;
    let report = Doctor::run(database_path)?;
    if report.index_id != plan.plan.index_id {
        return Err(MaintenanceError::BackupIdentityMismatch);
    }
    remove_closed_backup_sidecars(database_path)?;
    ensure_sidecars_absent(database_path)?;

    Ok(RestoreOutcome {
        plan_hash: plan.plan_hash,
        safety_backup,
        replacement_epoch: plan.plan.post_restore_replacement_epoch,
        restored_revisions: u64::try_from(plan.plan.affected_revisions.len())
            .map_err(|_| StoreError::IntegerOverflow)?,
    })
}

fn operation_id_from_plan_hash(plan_hash: Sha256Digest) -> Uuid {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&plan_hash.as_bytes()[..16]);
    Uuid::from_bytes(bytes)
}

fn database_has_forget_run(
    database: &IndexDb,
    plan_hash: Sha256Digest,
) -> Result<bool, MaintenanceError> {
    Ok(database.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM forget_runs WHERE plan_hash = ?1)",
        [plan_hash.as_bytes().as_slice()],
        |row| row.get(0),
    )?)
}

fn database_has_restore_run(
    database: &IndexDb,
    plan_hash: Sha256Digest,
) -> Result<bool, MaintenanceError> {
    Ok(database.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM restore_runs WHERE plan_hash = ?1)",
        [plan_hash.as_bytes().as_slice()],
        |row| row.get(0),
    )?)
}

fn validate_completed_prune(
    database: &IndexDb,
    plan: &PlanEnvelope<PrunePlan>,
) -> Result<(), MaintenanceError> {
    let stored = database
        .connection()
        .query_row(
            "SELECT history_floor_epoch, affected_revision_count,
                    affected_citation_count, logical_reclaimable_bytes, manifest_json
             FROM prune_runs WHERE plan_hash = ?1",
            [plan.plan_hash.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(MaintenanceError::PlanStale)?;
    let expected_floor = if plan.plan.index_epoch == 0 {
        plan.plan.history_floor_epoch
    } else {
        plan.plan.index_epoch
    };
    if u64::try_from(stored.0).ok() != Some(expected_floor)
        || usize::try_from(stored.1).ok() != Some(plan.plan.affected_revisions.len())
        || u64::try_from(stored.2).ok() != Some(plan.plan.affected_citation_count)
        || u64::try_from(stored.3).ok() != Some(plan.plan.logical_reclaimable_bytes)
        || stored.4 != to_canonical_json(plan)?
        || read_metadata_u64(database.connection(), "history_floor_epoch")? != expected_floor
    {
        return Err(MaintenanceError::PlanStale);
    }
    for revision in &plan.plan.affected_revisions {
        let present: bool = database.connection().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM pruned_revision_namespaces
                 WHERE plan_hash = ?1
                   AND source_id = ?2
                   AND document_id = ?3
                   AND revision_sha256 = ?4
             )",
            params![
                plan.plan_hash.as_bytes().as_slice(),
                revision.source_id.as_uuid().as_bytes().as_slice(),
                revision.document_id.as_uuid().as_bytes().as_slice(),
                revision.revision_sha256.as_bytes().as_slice(),
            ],
            |row| row.get(0),
        )?;
        if !present {
            return Err(MaintenanceError::PlanStale);
        }
    }
    Ok(())
}

fn prune_outcome(
    plan: &PlanEnvelope<PrunePlan>,
    backup: BackupReceipt,
) -> Result<PruneOutcome, MaintenanceError> {
    Ok(PruneOutcome {
        plan_hash: plan.plan_hash,
        backup,
        history_floor_epoch: if plan.plan.index_epoch == 0 {
            plan.plan.history_floor_epoch
        } else {
            plan.plan.index_epoch
        },
        affected_revisions: u64::try_from(plan.plan.affected_revisions.len())
            .map_err(|_| StoreError::IntegerOverflow)?,
        affected_citations: plan.plan.affected_citation_count,
        logical_reclaimable_bytes: plan.plan.logical_reclaimable_bytes,
    })
}

fn validate_completed_restore_state(
    database: &IndexDb,
    plan: &PlanEnvelope<RestorePlan>,
    safety_backup_sha256: Sha256Digest,
) -> Result<(), MaintenanceError> {
    let report = crate::store::doctor::inspect_connection(
        database.connection(),
        false,
        crate::store::doctor::InspectionDepth::Full,
        crate::store::doctor::FingerprintPolicy::Reject,
    )?;
    if report.index_id != plan.plan.index_id
        || report.schema_version != plan.plan.schema_version
        || report.schema_checksum != plan.plan.schema_checksum
        || report.pipeline_fingerprint != plan.plan.pipeline_fingerprint
        || read_metadata_u64(database.connection(), "index_epoch")?
            != plan.plan.post_restore_index_epoch
        || read_metadata_u64(database.connection(), "replacement_epoch")?
            != plan.plan.post_restore_replacement_epoch
    {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    let hashes = database
        .connection()
        .query_row(
            "SELECT recovery_backup_sha256, safety_backup_sha256
             FROM restore_runs
             WHERE plan_hash = ?1 AND forget_plan_hash = ?2",
            params![
                plan.plan_hash.as_bytes().as_slice(),
                plan.plan.forget_plan_hash.as_bytes().as_slice(),
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
        .ok_or(MaintenanceError::RestoreStateMismatch)?;
    if hashes.0.as_slice() != plan.plan.recovery_backup_sha256.as_bytes()
        || hashes.1.as_slice() != safety_backup_sha256.as_bytes()
    {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    let forgotten: bool = database.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM forgotten_documents)",
        [],
        |row| row.get(0),
    )?;
    if forgotten {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    Ok(())
}

fn existing_backup_receipt(
    path: &Path,
    index_id: IndexId,
    schema_version: u32,
    index_epoch: u64,
) -> Result<BackupReceipt, MaintenanceError> {
    verify_backup(path, BackupVerifier::Current, index_id)?;
    let report = Doctor::run(path)?;
    let database = IndexDb::open_existing(path, OpenMode::ReadOnly)?;
    if report.schema_version != schema_version
        || read_metadata_u64(database.connection(), "index_epoch")? != index_epoch
    {
        return Err(MaintenanceError::RecoveryBackupMismatch);
    }
    database.verify_live_identity()?;
    Ok(BackupReceipt {
        format: "hsum.backup.v1",
        index_id,
        schema_version,
        index_epoch,
        output: path.to_path_buf(),
        file_bytes: fs::metadata(path)?.len(),
        file_sha256: hash_file(path)?,
    })
}

fn expected_restore_plan(
    forget_plan: &PlanEnvelope<ForgetPlan>,
    recovery_backup: &BackupReceipt,
    post_forget_database: &Path,
    post_replacement_epoch: u64,
) -> Result<PlanEnvelope<RestorePlan>, MaintenanceError> {
    create_plan_envelope(
        "hsum.restore-plan.v1",
        RestorePlan {
            index_id: forget_plan.plan.index_id,
            schema_version: forget_plan.plan.schema_version,
            schema_checksum: forget_plan.plan.schema_checksum,
            pipeline_fingerprint: forget_plan.plan.pipeline_fingerprint,
            forget_plan_hash: forget_plan.plan_hash,
            pre_forget_index_epoch: forget_plan.plan.index_epoch,
            post_forget_index_epoch: forget_plan
                .plan
                .index_epoch
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?,
            post_restore_index_epoch: forget_plan
                .plan
                .index_epoch
                .checked_add(2)
                .ok_or(StoreError::IntegerOverflow)?,
            pre_forget_replacement_epoch: forget_plan.plan.replacement_epoch,
            post_forget_replacement_epoch: post_replacement_epoch,
            post_restore_replacement_epoch: post_replacement_epoch
                .checked_add(1)
                .ok_or(StoreError::IntegerOverflow)?,
            recovery_backup_sha256: recovery_backup.file_sha256,
            post_forget_database_sha256: hash_file(post_forget_database)?,
            post_forget_database_bytes: fs::metadata(post_forget_database)?.len(),
            affected_revisions: forget_plan.plan.affected_revisions.clone(),
        },
    )
}

fn forget_outcome(
    plan: &PlanEnvelope<ForgetPlan>,
    recovery_backup: BackupReceipt,
    restore_plan: &PlanEnvelope<RestorePlan>,
    restore_plan_path: &Path,
) -> Result<ForgetOutcome, MaintenanceError> {
    Ok(ForgetOutcome {
        plan_hash: plan.plan_hash,
        recovery_backup,
        restore_plan_hash: restore_plan.plan_hash,
        restore_plan: restore_plan_path.to_path_buf(),
        replacement_epoch: restore_plan.plan.post_forget_replacement_epoch,
        affected_revisions: u64::try_from(plan.plan.affected_revisions.len())
            .map_err(|_| StoreError::IntegerOverflow)?,
        physically_rewritten_bytes: restore_plan.plan.post_forget_database_bytes,
    })
}

fn append_forget_state(
    database_path: &Path,
    writer_lock: &WriterLock,
    plan: &PlanEnvelope<ForgetPlan>,
    operation_id: Uuid,
    state: ForgetOperationState,
) -> Result<(), MaintenanceError> {
    for revision in &plan.plan.affected_revisions {
        let ledger = ForgetLedger::read(database_path, plan.plan.index_id)?;
        if ledger
            .state_for(
                operation_id,
                revision.source_id,
                revision.document_id,
                revision.connector_key_sha256,
            )
            .is_some_and(|current| current >= state)
        {
            continue;
        }
        ForgetLedger::append(
            database_path,
            writer_lock,
            plan.plan.index_id,
            operation_id,
            state,
            ForgetLedgerTarget {
                source_id: revision.source_id,
                document_id: revision.document_id,
                connector_key_sha256: revision.connector_key_sha256,
            },
        )?;
    }
    Ok(())
}

fn append_restore_state(
    database_path: &Path,
    writer_lock: &WriterLock,
    plan: &PlanEnvelope<RestorePlan>,
) -> Result<(), MaintenanceError> {
    let operation_id = operation_id_from_plan_hash(plan.plan.forget_plan_hash);
    for revision in &plan.plan.affected_revisions {
        let ledger = ForgetLedger::read(database_path, plan.plan.index_id)?;
        if ledger
            .state_for(
                operation_id,
                revision.source_id,
                revision.document_id,
                revision.connector_key_sha256,
            )
            .is_some_and(|state| state >= ForgetOperationState::Restored)
        {
            continue;
        }
        ForgetLedger::append(
            database_path,
            writer_lock,
            plan.plan.index_id,
            operation_id,
            ForgetOperationState::Restored,
            ForgetLedgerTarget {
                source_id: revision.source_id,
                document_id: revision.document_id,
                connector_key_sha256: revision.connector_key_sha256,
            },
        )?;
    }
    Ok(())
}

pub fn plan_migration(
    database_path: &Path,
    index_name: &str,
    lock_timeout: Duration,
) -> Result<PlanEnvelope<MigrationPlan>, MaintenanceError> {
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let database = IndexDb::open_existing_for_maintenance(database_path, OpenMode::ReadOnly)?;
    if !writer_lock.protects(database.path()) {
        return Err(StoreError::WriterLockMismatch.into());
    }
    build_migration_plan(&database, index_name)
}

pub fn apply_migration(
    database_path: &Path,
    index_name: &str,
    plan: &PlanEnvelope<MigrationPlan>,
    confirmed_hash: Sha256Digest,
    backup_output: &Path,
    lock_timeout: Duration,
) -> Result<MigrationOutcome, MaintenanceError> {
    validate_plan_envelope("hsum.migration-plan.v1", plan, confirmed_hash)?;
    if plan.plan.index_name != index_name {
        return Err(MaintenanceError::PlanIndexMismatch);
    }
    if plan.plan.steps.is_empty() {
        return Err(MaintenanceError::NoMaintenanceWork);
    }
    let writer_lock = WriterLock::acquire(database_path, lock_timeout)?;
    let mut database = IndexDb::open_existing_for_maintenance(database_path, OpenMode::ReadWrite)?;
    let live_plan = build_migration_plan(&database, index_name)?;
    if &live_plan != plan {
        return Err(MaintenanceError::PlanStale);
    }
    StoragePreflight::run(database.path(), plan.plan.estimated_transaction_bytes, None)?;
    let backup = backup_under_lock(
        &database,
        backup_output,
        &writer_lock,
        plan.plan.index_id,
        plan.plan.from_schema_version,
        read_metadata_u64(database.connection(), "index_epoch")?,
        BackupVerifier::MigrationSource(plan.plan.from_schema_version),
    )?;

    database
        .connection()
        .pragma_update(None, "foreign_keys", false)?;
    let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    for step in &plan.plan.steps {
        let sql = MIGRATIONS
            .iter()
            .find_map(|(version, sql)| (*version == step.version).then_some(*sql))
            .ok_or(MaintenanceError::UnsupportedMigrationPath)?;
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at, checksum)
             VALUES (?1, ?2, ?3)",
            params![
                i64::from(step.version),
                applied_at,
                step.checksum.as_bytes().as_slice(),
            ],
        )?;
        if step.version == 3 {
            transaction.execute(
                "INSERT INTO index_meta(key, value)
                 VALUES ('history_floor_epoch', CAST('1' AS BLOB))",
                [],
            )?;
            transaction.execute(
                "INSERT INTO index_meta(key, value)
                 VALUES ('replacement_epoch', CAST('0' AS BLOB))",
                [],
            )?;
        }
    }
    transaction.pragma_update(None, "user_version", plan.plan.to_schema_version)?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB) WHERE key = 'schema_version'",
        [plan.plan.to_schema_version.to_string()],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = ?1 WHERE key = 'schema_checksum'",
        [plan.plan.to_schema_checksum.as_bytes().as_slice()],
    )?;
    transaction.commit()?;
    database
        .connection()
        .pragma_update(None, "foreign_keys", true)?;
    database.verify_live_identity()?;
    drop(database);
    Doctor::run(database_path)?;

    Ok(MigrationOutcome {
        plan_hash: plan.plan_hash,
        backup,
        from_schema_version: plan.plan.from_schema_version,
        to_schema_version: plan.plan.to_schema_version,
    })
}

pub fn write_plan<T: Serialize>(
    output: &Path,
    plan: &PlanEnvelope<T>,
) -> Result<(), MaintenanceError> {
    write_private_json(output, plan)
}

pub fn write_private_json<T: Serialize>(output: &Path, value: &T) -> Result<(), MaintenanceError> {
    if output.exists() {
        return Err(MaintenanceError::OutputExists(output.to_path_buf()));
    }
    let parent = output
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(output.to_path_buf()))?;
    let mut bytes = to_canonical_vec(value)?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)? > MAX_PLAN_BYTES {
        return Err(MaintenanceError::PlanTooLarge);
    }
    StoragePreflight::run_staging(
        parent,
        u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)?,
    )?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output)?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_parent(parent)?;
    Ok(())
}

pub fn read_plan<T: DeserializeOwned>(path: &Path) -> Result<PlanEnvelope<T>, MaintenanceError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PLAN_BYTES {
        return Err(MaintenanceError::UnsafePlanFile(path.to_path_buf()));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| StoreError::IntegerOverflow)?,
    );
    File::open(path)?
        .take(MAX_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)? > MAX_PLAN_BYTES {
        return Err(MaintenanceError::PlanTooLarge);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn build_prune_plan(
    database: &IndexDb,
    before: OffsetDateTime,
    keep_latest: u64,
) -> Result<PlanEnvelope<PrunePlan>, MaintenanceError> {
    if keep_latest == 0 {
        return Err(MaintenanceError::InvalidPruneSelector);
    }
    let connection = database.connection();
    let report = crate::store::doctor::inspect_connection(
        connection,
        database.is_read_only()?,
        crate::store::doctor::InspectionDepth::Full,
        crate::store::doctor::FingerprintPolicy::Reject,
    )?;
    let index_epoch = read_metadata_u64(connection, "index_epoch")?;
    let active_generation = read_optional_metadata_u64(connection, "active_generation")?;
    let history_floor_epoch = read_metadata_u64(connection, "history_floor_epoch")?;
    let database_file_bytes = fs::metadata(database.path())?.len();
    let before = before.to_offset(UtcOffset::UTC).format(&Rfc3339)?;

    let mut generation_statement = connection.prepare(
        "SELECT id, state, created_at, committed_at
         FROM generations
         WHERE (?1 IS NULL OR id != ?1)
         ORDER BY id",
    )?;
    let mut generation_rows =
        generation_statement.query([active_generation.map(integer_from_u64).transpose()?])?;
    let mut affected_generations = Vec::new();
    while let Some(row) = generation_rows.next()? {
        affected_generations.push(PrunedGeneration {
            generation_id: u64::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| StoreError::IntegerOverflow)?,
            state: row.get(1)?,
            created_at: row.get(2)?,
            committed_at: row.get(3)?,
        });
    }
    let mut version_statement = connection.prepare(
        "WITH ranked AS (
             SELECT dv.id, d.source_id, dv.document_id, dv.revision_sha256,
                    dv.source_uri, dv.content_blob_id, dv.indexed_at,
                    ROW_NUMBER() OVER (
                        PARTITION BY dv.document_id
                        ORDER BY dv.indexed_at DESC, dv.id DESC
                    ) AS retention_rank
             FROM document_versions AS dv
             JOIN documents AS d ON d.id = dv.document_id
         )
         SELECT source_id, document_id, revision_sha256,
                source_uri, content_blob_id, indexed_at
         FROM ranked
         WHERE indexed_at < ?1
           AND retention_rank > ?2
           AND NOT EXISTS (
             SELECT 1 FROM document_heads AS dh
             WHERE dh.document_version_id = ranked.id
         )
         ORDER BY source_id, document_id, revision_sha256",
    )?;
    let mut version_rows = version_statement.query(params![
        before,
        i64::try_from(keep_latest).map_err(|_| StoreError::IntegerOverflow)?,
    ])?;
    let mut affected_revisions = Vec::new();
    let mut affected_citation_count = 0_u64;
    let mut candidate_blob_counts = BTreeMap::<i64, u64>::new();
    while let Some(row) = version_rows.next()? {
        let source_id = source_id_from_blob(&row.get::<_, Vec<u8>>(0)?)?;
        let document_id = document_id_from_blob(&row.get::<_, Vec<u8>>(1)?)?;
        let revision_sha256 = digest_from_blob(&row.get::<_, Vec<u8>>(2)?)?;
        let source_uri = row.get::<_, String>(3)?;
        let content_blob_id = row.get::<_, i64>(4)?;
        let indexed_at = row.get::<_, String>(5)?;
        let count = candidate_blob_counts.entry(content_blob_id).or_default();
        *count = count.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
        let mut chunk_statement = connection.prepare(
            "SELECT DISTINCT c.start_byte, c.end_byte
             FROM chunks AS c
             JOIN chunk_layouts AS cl ON cl.id = c.chunk_layout_id
             WHERE cl.content_blob_id = ?1
             ORDER BY c.start_byte, c.end_byte",
        )?;
        let mut chunk_rows = chunk_statement.query([content_blob_id])?;
        let mut citations = Vec::new();
        while let Some(chunk_row) = chunk_rows.next()? {
            let start = u64::try_from(chunk_row.get::<_, i64>(0)?)
                .map_err(|_| StoreError::IntegerOverflow)?;
            let end = u64::try_from(chunk_row.get::<_, i64>(1)?)
                .map_err(|_| StoreError::IntegerOverflow)?;
            citations.push(
                Citation {
                    index_id: report.index_id,
                    source_id,
                    document_id,
                    revision: revision_sha256,
                    span: ByteSpan::new(start, end)
                        .map_err(|_| MaintenanceError::InvalidStoredSpan)?,
                }
                .to_string(),
            );
        }
        affected_citation_count = affected_citation_count
            .checked_add(u64::try_from(citations.len()).map_err(|_| StoreError::IntegerOverflow)?)
            .ok_or(StoreError::IntegerOverflow)?;
        affected_revisions.push(PrunedRevision {
            source_id,
            document_id,
            revision_sha256,
            source_uri,
            indexed_at,
            canonical_stored_chunk_citations: citations,
        });
    }
    let mut logical_reclaimable_bytes = 0_u64;
    for (content_blob_id, candidate_count) in candidate_blob_counts {
        let (version_count, body_bytes): (i64, i64) = connection.query_row(
            "SELECT COUNT(*), length(original_bytes)
             FROM document_versions
             JOIN content_blobs ON content_blobs.id = document_versions.content_blob_id
             WHERE content_blob_id = ?1",
            [content_blob_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if u64::try_from(version_count).ok() == Some(candidate_count) {
            logical_reclaimable_bytes = logical_reclaimable_bytes
                .checked_add(u64::try_from(body_bytes).map_err(|_| StoreError::IntegerOverflow)?)
                .ok_or(StoreError::IntegerOverflow)?;
        }
    }
    if affected_revisions.is_empty()
        && affected_generations
            .iter()
            .any(|generation| generation.created_at.as_str() >= before.as_str())
    {
        affected_generations.clear();
    }
    let generations_removed =
        u64::try_from(affected_generations.len()).map_err(|_| StoreError::IntegerOverflow)?;
    let body = PrunePlan {
        index_id: report.index_id,
        schema_version: report.schema_version,
        schema_checksum: report.schema_checksum,
        pipeline_fingerprint: report.pipeline_fingerprint,
        index_epoch,
        active_generation,
        history_floor_epoch,
        before,
        keep_latest,
        database_file_bytes,
        estimated_transaction_bytes: database_file_bytes,
        logical_reclaimable_bytes,
        generations_removed,
        affected_generations,
        affected_citation_count,
        affected_revisions,
    };
    create_plan_envelope("hsum.prune-plan.v1", body)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequestedRevision {
    index_id: IndexId,
    source_id: SourceId,
    document_id: DocumentId,
    revision_sha256: Sha256Digest,
}

fn build_forget_plan(
    database: &IndexDb,
    project_id: ProjectId,
    requested: &[RequestedRevision],
) -> Result<PlanEnvelope<ForgetPlan>, MaintenanceError> {
    let connection = database.connection();
    let report = crate::store::doctor::inspect_connection(
        connection,
        database.is_read_only()?,
        crate::store::doctor::InspectionDepth::Full,
        crate::store::doctor::FingerprintPolicy::Reject,
    )?;
    let index_epoch = read_metadata_u64(connection, "index_epoch")?;
    let history_floor_epoch = read_metadata_u64(connection, "history_floor_epoch")?;
    let replacement_epoch = read_metadata_u64(connection, "replacement_epoch")?;
    let active_generation = read_optional_metadata_u64(connection, "active_generation")?
        .ok_or(MaintenanceError::HistoryNotPruned)?;
    let generation_count = count_u64(connection, "SELECT COUNT(*) FROM generations")?;
    let non_head_versions = count_u64(
        connection,
        "SELECT COUNT(*)
         FROM document_versions AS version
         WHERE NOT EXISTS (
             SELECT 1 FROM document_heads AS head
             WHERE head.document_version_id = version.id
         )",
    )?;
    if index_epoch == 0
        || history_floor_epoch != index_epoch
        || generation_count != 1
        || non_head_versions != 0
    {
        return Err(MaintenanceError::HistoryNotPruned);
    }

    let mut affected_revisions = Vec::new();
    let unique = requested.iter().copied().collect::<BTreeSet<_>>();
    for target in unique {
        if target.index_id != report.index_id {
            return Err(MaintenanceError::PlanIndexMismatch);
        }
        let stored = connection
            .query_row(
                "SELECT version.source_uri, version.content_blob_id,
                        length(blob.original_bytes), document.connector_key
                 FROM document_versions AS version
                 JOIN content_blobs AS blob ON blob.id = version.content_blob_id
                 JOIN documents AS document ON document.id = version.document_id
                 JOIN document_heads AS head
                   ON head.document_id = document.id
                  AND head.document_version_id = version.id
                  AND head.state = 'active'
                 JOIN project_sources AS membership
                   ON membership.source_id = document.source_id
                  AND membership.project_id = ?1
                  AND membership.removed_at IS NULL
                 WHERE document.source_id = ?2
                   AND document.id = ?3
                   AND version.revision_sha256 = ?4",
                params![
                    project_id.as_uuid().as_bytes().as_slice(),
                    target.source_id.as_uuid().as_bytes().as_slice(),
                    target.document_id.as_uuid().as_bytes().as_slice(),
                    target.revision_sha256.as_bytes().as_slice(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(MaintenanceError::ForgetTargetUnavailable)?;
        let original_body_bytes =
            u64::try_from(stored.2).map_err(|_| StoreError::IntegerOverflow)?;
        let mut statement = connection.prepare(
            "SELECT chunk.start_byte, chunk.end_byte
             FROM chunks AS chunk
             JOIN chunk_layouts AS layout ON layout.id = chunk.chunk_layout_id
             WHERE layout.content_blob_id = ?1
             ORDER BY chunk.start_byte, chunk.end_byte",
        )?;
        let citations = statement
            .query_map([stored.1], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .map(|row| {
                let (start, end) = row?;
                Ok(Citation {
                    index_id: report.index_id,
                    source_id: target.source_id,
                    document_id: target.document_id,
                    revision: target.revision_sha256,
                    span: ByteSpan::new(
                        u64::try_from(start).map_err(|_| StoreError::IntegerOverflow)?,
                        u64::try_from(end).map_err(|_| StoreError::IntegerOverflow)?,
                    )
                    .map_err(|_| MaintenanceError::InvalidStoredSpan)?,
                }
                .to_string())
            })
            .collect::<Result<Vec<_>, MaintenanceError>>()?;
        if citations.is_empty() {
            return Err(MaintenanceError::ForgetTargetUnavailable);
        }
        affected_revisions.push(ForgottenRevision {
            source_id: target.source_id,
            document_id: target.document_id,
            connector_key_sha256: Sha256Digest::of_bytes(&stored.3),
            revision_sha256: target.revision_sha256,
            source_uri: stored.0,
            original_body_bytes,
            canonical_stored_chunk_citations: citations,
        });
    }
    if affected_revisions.is_empty() {
        return Err(MaintenanceError::NoMaintenanceWork);
    }
    let database_file_bytes = fs::metadata(database.path())?.len();
    let estimated_rewrite_bytes = database_file_bytes
        .checked_mul(2)
        .ok_or(StoreError::IntegerOverflow)?;
    create_plan_envelope(
        "hsum.forget-plan.v1",
        ForgetPlan {
            index_id: report.index_id,
            project_id,
            schema_version: report.schema_version,
            schema_checksum: report.schema_checksum,
            pipeline_fingerprint: report.pipeline_fingerprint,
            index_epoch,
            history_floor_epoch,
            replacement_epoch,
            active_generation,
            database_file_bytes,
            estimated_rewrite_bytes,
            affected_revisions,
        },
    )
}

fn apply_forget_to_staging(
    staging: &Path,
    plan: &PlanEnvelope<ForgetPlan>,
    operation_id: Uuid,
    recovery_backup_sha256: Sha256Digest,
    post_replacement_epoch: u64,
) -> Result<(), MaintenanceError> {
    let mut database = IndexDb::open_existing(staging, OpenMode::ReadWrite)?;
    let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let post_index_epoch = plan
        .plan
        .index_epoch
        .checked_add(1)
        .ok_or(StoreError::IntegerOverflow)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO forget_runs(
             plan_hash, applied_at, pre_index_epoch, post_index_epoch,
             pre_replacement_epoch, post_replacement_epoch,
             recovery_backup_sha256, affected_revision_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            plan.plan_hash.as_bytes().as_slice(),
            applied_at,
            integer_from_u64(plan.plan.index_epoch)?,
            integer_from_u64(post_index_epoch)?,
            integer_from_u64(plan.plan.replacement_epoch)?,
            integer_from_u64(post_replacement_epoch)?,
            recovery_backup_sha256.as_bytes().as_slice(),
            integer_from_usize(plan.plan.affected_revisions.len())?,
        ],
    )?;
    for revision in &plan.plan.affected_revisions {
        let document_uuid = revision.document_id.as_uuid();
        let document_id = document_uuid.as_bytes();
        transaction.execute(
            "DELETE FROM passages_fts
             WHERE rowid IN (
                 SELECT id FROM active_passages WHERE document_id = ?1
             )",
            [document_id.as_slice()],
        )?;
        transaction.execute(
            "DELETE FROM active_passages WHERE document_id = ?1",
            [document_id.as_slice()],
        )?;
        if transaction.execute(
            "UPDATE documents SET tombstoned_at = ?1 WHERE id = ?2",
            params![applied_at, document_id.as_slice()],
        )? != 1
        {
            return Err(MaintenanceError::PlanStale);
        }
        if transaction.execute(
            "UPDATE document_heads
             SET document_version_id = NULL, state = 'tombstoned'
             WHERE document_id = ?1
               AND generation_id = ?2
               AND state = 'active'",
            params![
                document_id.as_slice(),
                integer_from_u64(plan.plan.active_generation)?,
            ],
        )? != 1
        {
            return Err(MaintenanceError::PlanStale);
        }
        if transaction.execute(
            "UPDATE generation_changes
             SET prior_version_id = NULL,
                 next_version_id = NULL,
                 next_state = 'tombstoned'
             WHERE generation_id = ?1
               AND document_id = ?2
               AND prior_version_id IS NULL
               AND next_state = 'active'",
            params![
                integer_from_u64(plan.plan.active_generation)?,
                document_id.as_slice(),
            ],
        )? != 1
        {
            return Err(MaintenanceError::HistoryNotPruned);
        }
        if transaction.execute(
            "DELETE FROM document_versions
             WHERE document_id = ?1 AND revision_sha256 = ?2",
            params![
                document_id.as_slice(),
                revision.revision_sha256.as_bytes().as_slice(),
            ],
        )? != 1
        {
            return Err(MaintenanceError::PlanStale);
        }
        transaction.execute(
            "INSERT INTO forgotten_documents(
                 source_id, document_id, connector_key_sha256,
                 forget_plan_hash, forgotten_at, operation_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision.source_id.as_uuid().as_bytes().as_slice(),
                document_id.as_slice(),
                revision.connector_key_sha256.as_bytes().as_slice(),
                plan.plan_hash.as_bytes().as_slice(),
                applied_at,
                operation_id.as_bytes().as_slice(),
            ],
        )?;
    }
    delete_unreferenced_content(&transaction)?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'index_epoch'",
        [post_index_epoch.to_string()],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'history_floor_epoch'",
        [post_index_epoch.to_string()],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'replacement_epoch'",
        [post_replacement_epoch.to_string()],
    )?;
    transaction.commit()?;
    database.verify_live_identity()?;
    drop(database);
    Doctor::run(staging)?;
    Ok(())
}

fn delete_unreferenced_content(transaction: &Connection) -> Result<(), MaintenanceError> {
    transaction.execute(
        "DELETE FROM chunks
         WHERE id NOT IN (SELECT chunk_id FROM active_passages)",
        [],
    )?;
    transaction.execute(
        "DELETE FROM chunk_layouts
         WHERE NOT EXISTS (
             SELECT 1 FROM chunks WHERE chunks.chunk_layout_id = chunk_layouts.id
         )",
        [],
    )?;
    transaction.execute(
        "DELETE FROM content_blobs
         WHERE NOT EXISTS (
             SELECT 1 FROM document_versions
             WHERE document_versions.content_blob_id = content_blobs.id
         )",
        [],
    )?;
    Ok(())
}

fn validate_restore_live_state(
    database: &IndexDb,
    plan: &PlanEnvelope<RestorePlan>,
) -> Result<(), MaintenanceError> {
    let report = crate::store::doctor::inspect_connection(
        database.connection(),
        false,
        crate::store::doctor::InspectionDepth::Full,
        crate::store::doctor::FingerprintPolicy::Reject,
    )?;
    if report.index_id != plan.plan.index_id
        || report.schema_version != plan.plan.schema_version
        || report.schema_checksum != plan.plan.schema_checksum
        || report.pipeline_fingerprint != plan.plan.pipeline_fingerprint
        || read_metadata_u64(database.connection(), "index_epoch")?
            != plan.plan.post_forget_index_epoch
        || read_metadata_u64(database.connection(), "replacement_epoch")?
            != plan.plan.post_forget_replacement_epoch
    {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    let stored_run = database
        .connection()
        .query_row(
            "SELECT pre_index_epoch, post_index_epoch,
                    pre_replacement_epoch, post_replacement_epoch,
                    recovery_backup_sha256, affected_revision_count
             FROM forget_runs WHERE plan_hash = ?1",
            [plan.plan.forget_plan_hash.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(MaintenanceError::RestoreStateMismatch)?;
    if u64::try_from(stored_run.0).ok() != Some(plan.plan.pre_forget_index_epoch)
        || u64::try_from(stored_run.1).ok() != Some(plan.plan.post_forget_index_epoch)
        || u64::try_from(stored_run.2).ok() != Some(plan.plan.pre_forget_replacement_epoch)
        || u64::try_from(stored_run.3).ok() != Some(plan.plan.post_forget_replacement_epoch)
        || stored_run.4.as_slice() != plan.plan.recovery_backup_sha256.as_bytes()
        || usize::try_from(stored_run.5).ok() != Some(plan.plan.affected_revisions.len())
    {
        return Err(MaintenanceError::RestoreStateMismatch);
    }
    for revision in &plan.plan.affected_revisions {
        let present: bool = database.connection().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM forgotten_documents
                 WHERE source_id = ?1
                   AND document_id = ?2
                   AND connector_key_sha256 = ?3
                   AND forget_plan_hash = ?4
             )",
            params![
                revision.source_id.as_uuid().as_bytes().as_slice(),
                revision.document_id.as_uuid().as_bytes().as_slice(),
                revision.connector_key_sha256.as_bytes().as_slice(),
                plan.plan.forget_plan_hash.as_bytes().as_slice(),
            ],
            |row| row.get(0),
        )?;
        if !present {
            return Err(MaintenanceError::RestoreStateMismatch);
        }
    }
    Ok(())
}

fn validate_recovery_backup(
    recovery_backup: &Path,
    plan: &PlanEnvelope<RestorePlan>,
) -> Result<(), MaintenanceError> {
    if hash_file(recovery_backup)? != plan.plan.recovery_backup_sha256 {
        return Err(MaintenanceError::RecoveryBackupMismatch);
    }
    remove_closed_backup_sidecars(recovery_backup)?;
    ensure_sidecars_absent(recovery_backup)?;
    let report = Doctor::run(recovery_backup)?;
    if report.index_id != plan.plan.index_id
        || report.schema_version != plan.plan.schema_version
        || report.schema_checksum != plan.plan.schema_checksum
        || report.pipeline_fingerprint != plan.plan.pipeline_fingerprint
    {
        return Err(MaintenanceError::RecoveryBackupMismatch);
    }
    let database = IndexDb::open_existing(recovery_backup, OpenMode::ReadOnly)?;
    if read_metadata_u64(database.connection(), "index_epoch")? != plan.plan.pre_forget_index_epoch
        || read_metadata_u64(database.connection(), "replacement_epoch")?
            != plan.plan.pre_forget_replacement_epoch
    {
        return Err(MaintenanceError::RecoveryBackupMismatch);
    }
    for revision in &plan.plan.affected_revisions {
        let present: bool = database.connection().query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM document_versions AS version
                 JOIN documents AS document ON document.id = version.document_id
                 WHERE document.source_id = ?1
                   AND document.id = ?2
                   AND version.revision_sha256 = ?3
             )",
            params![
                revision.source_id.as_uuid().as_bytes().as_slice(),
                revision.document_id.as_uuid().as_bytes().as_slice(),
                revision.revision_sha256.as_bytes().as_slice(),
            ],
            |row| row.get(0),
        )?;
        if !present {
            return Err(MaintenanceError::RecoveryBackupMismatch);
        }
    }
    database.verify_live_identity()?;
    drop(database);
    remove_closed_backup_sidecars(recovery_backup)?;
    ensure_sidecars_absent(recovery_backup)?;
    Ok(())
}

fn apply_restore_to_staging(
    staging: &Path,
    plan: &PlanEnvelope<RestorePlan>,
    safety_backup_sha256: Sha256Digest,
) -> Result<(), MaintenanceError> {
    let mut database = IndexDb::open_existing(staging, OpenMode::ReadWrite)?;
    let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let transaction = database
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO restore_runs(
             plan_hash, forget_plan_hash, applied_at,
             pre_index_epoch, post_index_epoch,
             pre_replacement_epoch, post_replacement_epoch,
             recovery_backup_sha256, safety_backup_sha256
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            plan.plan_hash.as_bytes().as_slice(),
            plan.plan.forget_plan_hash.as_bytes().as_slice(),
            applied_at,
            integer_from_u64(plan.plan.post_forget_index_epoch)?,
            integer_from_u64(plan.plan.post_restore_index_epoch)?,
            integer_from_u64(plan.plan.post_forget_replacement_epoch)?,
            integer_from_u64(plan.plan.post_restore_replacement_epoch)?,
            plan.plan.recovery_backup_sha256.as_bytes().as_slice(),
            safety_backup_sha256.as_bytes().as_slice(),
        ],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'index_epoch'",
        [plan.plan.post_restore_index_epoch.to_string()],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'history_floor_epoch'",
        [plan.plan.post_restore_index_epoch.to_string()],
    )?;
    transaction.execute(
        "UPDATE index_meta SET value = CAST(?1 AS BLOB)
         WHERE key = 'replacement_epoch'",
        [plan.plan.post_restore_replacement_epoch.to_string()],
    )?;
    transaction.commit()?;
    database.verify_live_identity()?;
    drop(database);
    Doctor::run(staging)?;
    Ok(())
}

fn require_output_absent(path: &Path) -> Result<(), MaintenanceError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(MaintenanceError::OutputExists(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn replacement_staging_path(
    database_path: &Path,
    operation: &str,
) -> Result<PathBuf, MaintenanceError> {
    let parent = database_path
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?
        .canonicalize()?;
    let file_name = database_path
        .file_name()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?;
    Ok(parent.join(format!(
        ".{}.{}-replacement-{}",
        file_name.to_string_lossy(),
        operation,
        Uuid::new_v4()
    )))
}

fn forget_staging_path(
    database_path: &Path,
    plan_hash: Sha256Digest,
) -> Result<PathBuf, MaintenanceError> {
    let parent = database_path
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?
        .canonicalize()?;
    let file_name = database_path
        .file_name()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?;
    Ok(parent.join(format!(
        ".{}.forget-replacement-{plan_hash}",
        file_name.to_string_lossy(),
    )))
}

fn publish_staging(scratch: &Path, staging: &Path) -> Result<(), MaintenanceError> {
    require_output_absent(staging)?;
    let scratch_parent = scratch
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(scratch.to_path_buf()))?
        .canonicalize()?;
    let staging_parent = staging
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(staging.to_path_buf()))?
        .canonicalize()?;
    if scratch_parent != staging_parent {
        return Err(MaintenanceError::ReplacementPathMismatch);
    }
    fs::rename(scratch, staging)?;
    sync_parent(&staging_parent)?;
    Ok(())
}

fn clone_closed_database(source: &Path, destination: &Path) -> Result<(), MaintenanceError> {
    require_output_absent(destination)?;
    ensure_sidecars_absent(source)?;
    let expected_hash = hash_file(source)?;
    let mut input = File::open(source)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    if hash_file(source)? != expected_hash || hash_file(destination)? != expected_hash {
        return Err(MaintenanceError::RecoveryBackupMismatch);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(destination.to_path_buf()))?;
    sync_parent(parent)?;
    Doctor::run(destination)?;
    Ok(())
}

fn compact_database(path: &Path) -> Result<(), MaintenanceError> {
    Doctor::run(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let journal_mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(StoreError::WalUnavailable(journal_mode).into());
    }
    connection.pragma_update(None, "secure_delete", true)?;
    connection.execute_batch("VACUUM")?;
    let journal_mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable(journal_mode).into());
    }
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(connection);
    remove_closed_backup_sidecars(path)?;
    ensure_sidecars_absent(path)?;
    File::open(path)?.sync_all()?;
    let parent = path
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(path.to_path_buf()))?;
    sync_parent(parent)?;
    Doctor::run(path)?;
    remove_closed_backup_sidecars(path)?;
    ensure_sidecars_absent(path)?;
    Ok(())
}

fn checkpoint_and_close(database: IndexDb) -> Result<(), MaintenanceError> {
    let path = database.path().to_path_buf();
    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    database.verify_live_identity()?;
    drop(database);
    remove_closed_backup_sidecars(&path)?;
    ensure_sidecars_absent(&path)?;
    File::open(&path)?.sync_all()?;
    Ok(())
}

fn publish_replacement(staging: &Path, database_path: &Path) -> Result<(), MaintenanceError> {
    let staging_parent = staging
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(staging.to_path_buf()))?
        .canonicalize()?;
    let database_parent = database_path
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?
        .canonicalize()?;
    if staging_parent != database_parent {
        return Err(MaintenanceError::ReplacementPathMismatch);
    }
    fs::rename(staging, database_path)?;
    let parent = database_path
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(database_path.to_path_buf()))?;
    sync_parent(parent)?;
    Ok(())
}

fn build_migration_plan(
    database: &IndexDb,
    index_name: &str,
) -> Result<PlanEnvelope<MigrationPlan>, MaintenanceError> {
    let raw_version: i64 =
        database
            .connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
    let found =
        u32::try_from(raw_version).map_err(|_| StoreError::InvalidSchemaVersion(raw_version))?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            current: SCHEMA_VERSION,
            found,
        }
        .into());
    }
    let require_read_only = database.is_read_only()?;
    let report = if found == SCHEMA_VERSION {
        crate::store::doctor::inspect_connection(
            database.connection(),
            require_read_only,
            crate::store::doctor::InspectionDepth::Full,
            crate::store::doctor::FingerprintPolicy::Reject,
        )?
    } else {
        if found + 1 != SCHEMA_VERSION {
            return Err(MaintenanceError::UnsupportedMigrationPath);
        }
        inspect_migration_source(database.connection(), require_read_only, found)?
    };
    let steps = ((found + 1)..=SCHEMA_VERSION)
        .map(|version| {
            Ok(MigrationStep {
                version,
                checksum: migration_checksum(version)
                    .ok_or(MaintenanceError::UnsupportedMigrationPath)?,
            })
        })
        .collect::<Result<Vec<_>, MaintenanceError>>()?;
    let database_file_bytes = fs::metadata(database.path())?.len();
    create_plan_envelope(
        "hsum.migration-plan.v1",
        MigrationPlan {
            index_name: index_name.to_owned(),
            index_id: report.index_id,
            from_schema_version: found,
            to_schema_version: SCHEMA_VERSION,
            from_schema_checksum: schema_checksum_through(found),
            to_schema_checksum: schema_checksum(),
            pipeline_fingerprint: report.pipeline_fingerprint,
            database_file_bytes,
            estimated_transaction_bytes: database_file_bytes,
            steps,
        },
    )
}

pub(crate) fn create_plan_envelope<T: Serialize>(
    format: &str,
    plan: T,
) -> Result<PlanEnvelope<T>, MaintenanceError> {
    let plan_hash = Sha256Digest::of_bytes(&to_canonical_vec(&plan)?);
    Ok(PlanEnvelope {
        format: format.to_owned(),
        plan_hash,
        plan,
    })
}

pub(crate) fn validate_plan_envelope<T: Serialize>(
    expected_format: &str,
    envelope: &PlanEnvelope<T>,
    confirmed_hash: Sha256Digest,
) -> Result<(), MaintenanceError> {
    if envelope.format != expected_format {
        return Err(MaintenanceError::PlanFormat);
    }
    let computed = Sha256Digest::of_bytes(&to_canonical_vec(&envelope.plan)?);
    if computed != envelope.plan_hash {
        return Err(MaintenanceError::PlanHashInvalid);
    }
    if confirmed_hash != envelope.plan_hash {
        return Err(MaintenanceError::ConfirmationMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum BackupVerifier {
    Current,
    MigrationSource(u32),
}

#[allow(clippy::too_many_arguments)]
fn backup_under_lock(
    source: &IndexDb,
    output: &Path,
    writer_lock: &WriterLock,
    index_id: IndexId,
    schema_version: u32,
    index_epoch: u64,
    verifier: BackupVerifier,
) -> Result<BackupReceipt, MaintenanceError> {
    if !writer_lock.protects(source.path()) {
        return Err(StoreError::WriterLockMismatch.into());
    }
    if output == source.path() {
        return Err(MaintenanceError::BackupOverlapsIndex);
    }
    match fs::symlink_metadata(output) {
        Ok(_) => return Err(MaintenanceError::OutputExists(output.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = output
        .parent()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(output.to_path_buf()))?;
    let physical_parent = parent.canonicalize()?;
    let estimated_bytes = fs::metadata(source.path())?.len();
    StoragePreflight::run_staging(parent, estimated_bytes)?;
    let file_name = output
        .file_name()
        .ok_or_else(|| MaintenanceError::OutputHasNoParent(output.to_path_buf()))?;
    let temporary = physical_parent.join(format!(
        ".{}.partial-{}",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let cleanup = PartialBackup::new(temporary.clone());
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(&temporary)?);
    let mut destination = Connection::open_with_flags(
        &temporary,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    {
        let backup = Backup::new(source.connection(), &mut destination)?;
        backup.run_to_completion(BACKUP_PAGES_PER_STEP, Duration::from_millis(5), None)?;
    }
    let journal_mode: String =
        destination.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(StoreError::WalUnavailable(journal_mode).into());
    }
    let journal_mode: String =
        destination.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable(journal_mode).into());
    }
    destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(destination);
    remove_closed_backup_sidecars(&temporary)?;
    File::open(&temporary)?.sync_all()?;
    verify_backup(&temporary, verifier, index_id)?;
    remove_closed_backup_sidecars(&temporary)?;
    ensure_sidecars_absent(&temporary)?;
    fs::hard_link(&temporary, output)?;
    fs::remove_file(&temporary)?;
    cleanup.disarm();
    sync_parent(parent)?;
    verify_backup(output, verifier, index_id)?;
    remove_closed_backup_sidecars(output)?;
    ensure_sidecars_absent(output)?;
    let file_bytes = fs::metadata(output)?.len();
    let file_sha256 = hash_file(output)?;
    Ok(BackupReceipt {
        format: "hsum.backup.v1",
        index_id,
        schema_version,
        index_epoch,
        output: output.to_path_buf(),
        file_bytes,
        file_sha256,
    })
}

fn verify_backup(
    path: &Path,
    verifier: BackupVerifier,
    expected_index_id: IndexId,
) -> Result<(), MaintenanceError> {
    let report = match verifier {
        BackupVerifier::Current => Doctor::run(path)?,
        BackupVerifier::MigrationSource(version) => {
            let database = IndexDb::open_existing_for_maintenance(path, OpenMode::ReadOnly)?;
            inspect_migration_source(database.connection(), true, version)?
        }
    };
    if report.index_id != expected_index_id {
        return Err(MaintenanceError::BackupIdentityMismatch);
    }
    Ok(())
}

fn ensure_sidecars_absent(path: &Path) -> Result<(), MaintenanceError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        if PathBuf::from(value).exists() {
            return Err(MaintenanceError::BackupSidecarPresent);
        }
    }
    Ok(())
}

fn remove_closed_backup_sidecars(path: &Path) -> Result<(), MaintenanceError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        let sidecar = PathBuf::from(value);
        let metadata = match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file()
            || ((suffix == "-wal" || suffix == "-journal") && metadata.len() != 0)
        {
            return Err(MaintenanceError::BackupSidecarPresent);
        }
        fs::remove_file(&sidecar)?;
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<Sha256Digest, MaintenanceError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn sync_parent(parent: &Path) -> Result<(), MaintenanceError> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct PartialBackup {
    path: PathBuf,
    armed: std::cell::Cell<bool>,
}

impl PartialBackup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            armed: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for PartialBackup {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = fs::remove_file(&self.path);
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut value = self.path.as_os_str().to_os_string();
                value.push(suffix);
                let _ = fs::remove_file(PathBuf::from(value));
            }
        }
    }
}

fn read_metadata_u64(connection: &Connection, key: &'static str) -> Result<u64, MaintenanceError> {
    let bytes = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(StoreError::InvalidMetadata(key))?;
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StoreError::InvalidMetadata(key).into())
}

fn read_optional_metadata_u64(
    connection: &Connection,
    key: &'static str,
) -> Result<Option<u64>, MaintenanceError> {
    let bytes = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(StoreError::InvalidMetadata(key))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(StoreError::InvalidMetadata(key))?,
    ))
}

fn count_u64(connection: &Connection, sql: &str) -> Result<u64, MaintenanceError> {
    u64::try_from(connection.query_row(sql, [], |row| row.get::<_, i64>(0))?)
        .map_err(|_| StoreError::IntegerOverflow.into())
}

fn integer_from_u64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}

fn integer_from_usize(value: usize) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}

fn source_id_from_blob(bytes: &[u8]) -> Result<SourceId, MaintenanceError> {
    Ok(SourceId::from_uuid(
        Uuid::from_slice(bytes).map_err(|_| MaintenanceError::InvalidStoredIdentity)?,
    ))
}

fn document_id_from_blob(bytes: &[u8]) -> Result<DocumentId, MaintenanceError> {
    Ok(DocumentId::from_uuid(
        Uuid::from_slice(bytes).map_err(|_| MaintenanceError::InvalidStoredIdentity)?,
    ))
}

fn digest_from_blob(bytes: &[u8]) -> Result<Sha256Digest, MaintenanceError> {
    Ok(Sha256Digest::from_bytes(
        bytes
            .try_into()
            .map_err(|_| MaintenanceError::InvalidStoredDigest)?,
    ))
}

#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
    #[error("maintenance JSON serialization failed")]
    Json(#[from] serde_json::Error),
    #[error("maintenance time formatting failed")]
    Time(#[from] time::error::Format),
    #[error("maintenance filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("output path has no parent: {0}")]
    OutputHasNoParent(PathBuf),
    #[error("backup output must not overlap the live index")]
    BackupOverlapsIndex,
    #[error("verified backup index identity does not match the source")]
    BackupIdentityMismatch,
    #[error("backup retained a SQLite sidecar and cannot be published")]
    BackupSidecarPresent,
    #[error("maintenance plan exceeds the 64 MiB bound")]
    PlanTooLarge,
    #[error("maintenance plan is not a safe bounded regular file: {0}")]
    UnsafePlanFile(PathBuf),
    #[error("maintenance plan format is unsupported")]
    PlanFormat,
    #[error("maintenance plan hash does not match its contents")]
    PlanHashInvalid,
    #[error("--confirm does not match the maintenance plan hash")]
    ConfirmationMismatch,
    #[error("maintenance plan is stale; generate and review a new plan")]
    PlanStale,
    #[error("maintenance plan targets a different managed index")]
    PlanIndexMismatch,
    #[error("the index has no work for this maintenance operation")]
    NoMaintenanceWork,
    #[error("prune requires an RFC3339 --before value and --keep-latest of at least one")]
    InvalidPruneSelector,
    #[error("forget requires a one-generation baseline; apply prune first")]
    HistoryNotPruned,
    #[error("a requested forget citation is not an active revision in the selected project")]
    ForgetTargetUnavailable,
    #[error("the live index no longer matches the exact post-forget restore state")]
    RestoreStateMismatch,
    #[error("the recovery backup does not match the backup bound into the restore plan")]
    RecoveryBackupMismatch,
    #[error("the physical replacement staging file is not beside the live index")]
    ReplacementPathMismatch,
    #[error("maintenance output paths must be distinct")]
    OutputPathsOverlap,
    #[error("only the released N-1 schema can be migrated by this binary")]
    UnsupportedMigrationPath,
    #[error("stored maintenance identity is invalid")]
    InvalidStoredIdentity,
    #[error("stored maintenance digest is invalid")]
    InvalidStoredDigest,
    #[error("stored chunk span is invalid")]
    InvalidStoredSpan,
}

impl From<rusqlite::Error> for MaintenanceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sqlite(error))
    }
}
