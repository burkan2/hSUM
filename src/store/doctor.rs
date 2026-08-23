use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::str;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;
use serde_json_canonicalizer::to_string as to_canonical_json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::{IndexId, Sha256Digest};
use crate::ingest::{
    Chunk, ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, chunk_bytes, repo_uri,
    revision_sha256,
};
use crate::store::generation::prepare_passage_literals;
use crate::store::lock::WriterLock;
use crate::store::open::{IndexDb, OpenMode, StoreError, configure_connection};
use crate::store::schema::{
    APPLICATION_ID, MIGRATIONS, SCHEMA_VERSION, chunk_kind_for_fingerprint, chunker_fingerprint,
    migration_checksum, pipeline_fingerprint, pipeline_fingerprint_for, schema_checksum,
    schema_checksum_through,
};
use crate::store::vector::{
    IndexEmbeddingProfile, read_embedding_profile, validate_active_vector_membership,
    validate_stored_provenance, validate_stored_vector_blob,
};
use crate::store::{ForgetLedger, ForgetOperationState, ReplacementEpoch};

const REQUIRED_TABLES: &[&str] = &[
    "schema_migrations",
    "index_meta",
    "generations",
    "sources",
    "projects",
    "project_sources",
    "documents",
    "content_blobs",
    "chunk_layouts",
    "chunks",
    "document_versions",
    "document_heads",
    "generation_changes",
    "active_passages",
    "passages_fts",
    "passage_literals",
    "source_sync_errors",
    "prune_runs",
    "pruned_revision_namespaces",
    "forget_runs",
    "forgotten_documents",
    "restore_runs",
    "embedding_provenance",
    "chunk_embeddings",
    "passages_vec_a",
    "passages_vec_b",
];

const REQUIRED_INDEXES: &[&str] = &[
    "document_versions_content_blob_idx",
    "document_heads_generation_idx",
    "generation_changes_document_idx",
    "active_passages_source_idx",
    "active_passages_version_idx",
    "passage_literals_lookup_idx",
    "source_sync_errors_generation_idx",
    "chunk_embeddings_chunk_idx",
];

const MAX_SCHEMA_OBJECT_TYPE_BYTES: i64 = 16;
const MAX_SCHEMA_OBJECT_NAME_BYTES: i64 = 128;
const MAX_SCHEMA_TABLE_NAME_BYTES: i64 = 128;
const MAX_SCHEMA_SQL_BYTES: i64 = 64 * 1024;

#[derive(Debug)]
pub struct Doctor;

impl Doctor {
    pub fn run(path: &std::path::Path) -> Result<DoctorReport, StoreError> {
        Self::run_with_policy(path, FingerprintPolicy::Reject)
    }

    pub(crate) fn run_with_policy(
        path: &std::path::Path,
        policy: FingerprintPolicy,
    ) -> Result<DoctorReport, StoreError> {
        let database = IndexDb::open_read_only(path)?;
        let report =
            inspect_connection(database.connection(), true, InspectionDepth::Full, policy)?;
        if std::fs::symlink_metadata(ForgetLedger::sidecar_path(path)).is_ok() {
            let ledger = ForgetLedger::read(path, report.index_id)?;
            validate_external_forget_ledger(database.connection(), &ledger)?;
        }
        if let Some(epoch) = ReplacementEpoch::read(path, report.index_id)? {
            let stored =
                parse_metadata_u64(&metadata_value(database.connection(), "replacement_epoch")?)
                    .ok_or(StoreError::InvalidMetadata("replacement_epoch"))?;
            if epoch != stored {
                return Err(StoreError::ForgetLedgerMismatch);
            }
        }
        Ok(report)
    }

    /// Remove only generation rows already classified as abandoned.
    ///
    /// Full Doctor validation runs on both sides of the transaction. Committed
    /// history, active heads, immutable evidence, and index epochs are never
    /// rewritten by this repair.
    pub fn repair_abandoned(
        path: &Path,
        timeout: Duration,
    ) -> Result<DoctorRepairOutcome, StoreError> {
        let _writer_lock = WriterLock::acquire(path, timeout)?;
        let before = Self::run(path)?;
        let mut database = IndexDb::open_existing_for_maintenance(path, OpenMode::ReadWrite)?;
        let transaction = database
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM generations
             WHERE state = 'abandoned'",
            [],
        )?;
        transaction.commit()?;
        drop(database);

        let report = Self::run(path)?;
        if report.index_id != before.index_id {
            return Err(StoreError::GenerationInvariant(
                "doctor repair changed the index identity",
            ));
        }
        Ok(DoctorRepairOutcome {
            removed_abandoned_generations: u64::try_from(removed)
                .map_err(|_| StoreError::IntegerOverflow)?,
            report,
        })
    }

    /// Capture a support artifact that contains no source or evidence content.
    pub fn support_report(path: &Path) -> Result<DoctorSupportReport, StoreError> {
        Ok(DoctorSupportReport {
            format: "hsum.doctor-report.v1",
            created_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
            hsum_version: env!("CARGO_PKG_VERSION"),
            body_free: true,
            query_free: true,
            diagnosis: Self::run(path)?,
        })
    }
}

fn validate_external_forget_ledger(
    connection: &Connection,
    ledger: &ForgetLedger,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT source_id, document_id, connector_key_sha256
         FROM forgotten_documents
         ORDER BY source_id, connector_key_sha256",
    )?;
    let database_records = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for (source, document, connector_hash) in &database_records {
        let source_id = crate::domain::SourceId::from_uuid(
            Uuid::from_slice(source).map_err(|_| StoreError::ForgetLedgerMismatch)?,
        );
        let document_id = crate::domain::DocumentId::from_uuid(
            Uuid::from_slice(document).map_err(|_| StoreError::ForgetLedgerMismatch)?,
        );
        let connector_key_sha256 = Sha256Digest::from_bytes(
            connector_hash
                .as_slice()
                .try_into()
                .map_err(|_| StoreError::ForgetLedgerMismatch)?,
        );
        if !ledger.suppresses(source_id, connector_key_sha256)
            || !ledger.suppresses_document(source_id, document_id)
        {
            return Err(StoreError::ForgetLedgerMismatch);
        }
    }

    let mut latest = BTreeMap::new();
    for record in ledger.records() {
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
    for record in latest.values() {
        if matches!(
            record.state,
            ForgetOperationState::ReplacementActivated | ForgetOperationState::Committed
        ) {
            let present = database_records
                .iter()
                .any(|(source, document, connector_hash)| {
                    source.as_slice() == record.source_id.as_uuid().as_bytes()
                        && document.as_slice() == record.document_id.as_uuid().as_bytes()
                        && connector_hash.as_slice() == record.connector_key_sha256.as_bytes()
                });
            if !present {
                return Err(StoreError::ForgetLedgerMismatch);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InspectionDepth {
    Open,
    Full,
}

/// Whether a pipeline-fingerprint mismatch is fatal.
///
/// `Reject` is the default for every public read path: an index built by a
/// different indexing pipeline must not be silently searched, because its
/// stored chunks were produced under different rules. `Tolerate` is
/// crate-private and exists only so `init --rebuild` can validate that an old
/// index is otherwise coherent before replacing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintPolicy {
    Reject,
    Tolerate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub application_id: i32,
    pub schema_version: u32,
    pub index_id: IndexId,
    pub schema_checksum: Sha256Digest,
    pub pipeline_fingerprint: Sha256Digest,
    pub journal_mode: String,
    pub read_only: bool,
    pub abandoned_generations: u64,
    pub scan: DoctorScanStats,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DoctorScanStats {
    pub content_blobs: u64,
    pub chunk_layouts: u64,
    pub chunks: u64,
    pub document_versions: u64,
    pub active_passages: u64,
    pub original_body_bytes: u64,
    pub max_single_body_bytes: u64,
    pub max_body_rows_in_flight: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorRepairOutcome {
    pub removed_abandoned_generations: u64,
    pub report: DoctorReport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorSupportReport {
    pub format: &'static str,
    pub created_at: String,
    pub hsum_version: &'static str,
    pub body_free: bool,
    pub query_free: bool,
    pub diagnosis: DoctorReport,
}

impl DoctorSupportReport {
    pub const INCLUDED_FIELDS: &'static [&'static str] = &[
        "format",
        "created_at",
        "hsum_version",
        "body_free",
        "query_free",
        "diagnosis.application_id",
        "diagnosis.schema_version",
        "diagnosis.index_id",
        "diagnosis.schema_checksum",
        "diagnosis.pipeline_fingerprint",
        "diagnosis.journal_mode",
        "diagnosis.read_only",
        "diagnosis.abandoned_generations",
        "diagnosis.scan.content_blobs",
        "diagnosis.scan.chunk_layouts",
        "diagnosis.scan.chunks",
        "diagnosis.scan.document_versions",
        "diagnosis.scan.active_passages",
        "diagnosis.scan.original_body_bytes",
        "diagnosis.scan.max_single_body_bytes",
        "diagnosis.scan.max_body_rows_in_flight",
    ];
}

impl DoctorScanStats {
    fn observe_original_body(&mut self, bytes: usize) -> Result<(), StoreError> {
        checked_increment(&mut self.content_blobs)?;
        let bytes = u64::try_from(bytes).map_err(|_| StoreError::IntegerOverflow)?;
        self.original_body_bytes = self
            .original_body_bytes
            .checked_add(bytes)
            .ok_or(StoreError::IntegerOverflow)?;
        self.max_single_body_bytes = self.max_single_body_bytes.max(bytes);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct BodyScanTracker {
    live: Cell<u64>,
    max: Cell<u64>,
}

impl BodyScanTracker {
    fn acquire(&self) -> Result<BodyScanLease<'_>, StoreError> {
        let live = self
            .live
            .get()
            .checked_add(1)
            .ok_or(StoreError::IntegerOverflow)?;
        self.live.set(live);
        self.max.set(self.max.get().max(live));
        Ok(BodyScanLease { tracker: self })
    }

    fn finish(&self) -> u64 {
        debug_assert_eq!(self.live.get(), 0);
        self.max.get()
    }

    #[cfg(test)]
    fn live(&self) -> u64 {
        self.live.get()
    }

    #[cfg(test)]
    fn max(&self) -> u64 {
        self.max.get()
    }
}

struct BodyScanLease<'tracker> {
    tracker: &'tracker BodyScanTracker,
}

impl Drop for BodyScanLease<'_> {
    fn drop(&mut self) {
        let live = self
            .tracker
            .live
            .get()
            .checked_sub(1)
            .expect("a body scan lease cannot be dropped when none is live");
        self.tracker.live.set(live);
    }
}

pub(crate) fn inspect_connection(
    connection: &Connection,
    require_read_only: bool,
    depth: InspectionDepth,
    policy: FingerprintPolicy,
) -> Result<DoctorReport, StoreError> {
    let transaction = connection.unchecked_transaction()?;
    let report = inspect_snapshot(&transaction, require_read_only, depth, policy)?;
    transaction.rollback()?;
    Ok(report)
}

pub(crate) fn inspect_migration_source(
    connection: &Connection,
    require_read_only: bool,
    version: u32,
) -> Result<DoctorReport, StoreError> {
    inspect_migration_source_with_policy(
        connection,
        require_read_only,
        version,
        FingerprintPolicy::Reject,
    )
}

pub(crate) fn inspect_migration_source_with_policy(
    connection: &Connection,
    require_read_only: bool,
    version: u32,
    policy: FingerprintPolicy,
) -> Result<DoctorReport, StoreError> {
    let transaction = connection.unchecked_transaction()?;
    let report = inspect_migration_snapshot(&transaction, require_read_only, version, policy)?;
    transaction.rollback()?;
    Ok(report)
}

fn inspect_migration_snapshot(
    connection: &Connection,
    require_read_only: bool,
    version: u32,
    policy: FingerprintPolicy,
) -> Result<DoctorReport, StoreError> {
    if version == 0 || version >= SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            current: SCHEMA_VERSION,
            found: version,
        });
    }
    let application_id: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(StoreError::InvalidApplicationId {
            expected: APPLICATION_ID,
            actual: application_id,
        });
    }
    let raw_schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if raw_schema_version != i64::from(version) {
        return Err(StoreError::UnsupportedSchemaVersion {
            current: SCHEMA_VERSION,
            found: u32::try_from(raw_schema_version)
                .map_err(|_| StoreError::InvalidSchemaVersion(raw_schema_version))?,
        });
    }

    validate_schema_manifest_through(connection, version)?;
    let (index_id, stored_pipeline_fingerprint) =
        validate_metadata_through(connection, version, policy)?;
    validate_migration_chain_through(connection, version)?;
    let mut scan = DoctorScanStats::default();
    let body_scan = BodyScanTracker::default();
    validate_integrity(connection)?;
    validate_generation_invariants(connection, stored_pipeline_fingerprint, 1, false)?;
    validate_immutable_evidence(connection, &mut scan, &body_scan)?;
    validate_active_indexes(connection, &mut scan)?;
    scan.max_body_rows_in_flight = body_scan.finish();

    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable(journal_mode));
    }
    let read_only = connection.is_readonly("main")?;
    let query_only: bool = connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
    if require_read_only && (!read_only || !query_only) {
        return Err(StoreError::ReadOnlyRequired);
    }
    if !require_read_only && (read_only || query_only) {
        return Err(StoreError::ReadWriteRequired);
    }

    Ok(DoctorReport {
        application_id,
        schema_version: version,
        index_id,
        schema_checksum: schema_checksum_through(version),
        pipeline_fingerprint: stored_pipeline_fingerprint,
        journal_mode: journal_mode.to_ascii_lowercase(),
        read_only,
        abandoned_generations: count_abandoned_generations(connection)?,
        scan,
    })
}

fn inspect_snapshot(
    connection: &Connection,
    require_read_only: bool,
    depth: InspectionDepth,
    policy: FingerprintPolicy,
) -> Result<DoctorReport, StoreError> {
    let application_id: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(StoreError::InvalidApplicationId {
            expected: APPLICATION_ID,
            actual: application_id,
        });
    }

    let raw_schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let found = u32::try_from(raw_schema_version)
        .map_err(|_| StoreError::InvalidSchemaVersion(raw_schema_version))?;
    if found != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            current: SCHEMA_VERSION,
            found,
        });
    }

    validate_schema_manifest(connection)?;
    let (index_id, stored_pipeline_fingerprint) = validate_metadata(connection, policy)?;
    validate_migration_chain(connection)?;
    let mut scan = DoctorScanStats::default();
    if depth == InspectionDepth::Full {
        let body_scan = BodyScanTracker::default();
        validate_integrity(connection)?;
        let history_floor_epoch = history_floor_epoch(connection)?;
        validate_generation_invariants(
            connection,
            stored_pipeline_fingerprint,
            history_floor_epoch,
            true,
        )?;
        validate_generation_embedding_profile(connection)?;
        validate_immutable_evidence(connection, &mut scan, &body_scan)?;
        validate_embedding_cache(connection)?;
        validate_active_indexes(connection, &mut scan)?;
        validate_forget_ledger(connection)?;
        scan.max_body_rows_in_flight = body_scan.finish();
    }

    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable(journal_mode));
    }
    let read_only = connection.is_readonly("main")?;
    let query_only: bool = connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
    if require_read_only && (!read_only || !query_only) {
        return Err(StoreError::ReadOnlyRequired);
    }
    if !require_read_only && (read_only || query_only) {
        return Err(StoreError::ReadWriteRequired);
    }

    Ok(DoctorReport {
        application_id,
        schema_version: found,
        index_id,
        schema_checksum: schema_checksum(),
        pipeline_fingerprint: stored_pipeline_fingerprint,
        journal_mode: journal_mode.to_ascii_lowercase(),
        read_only,
        abandoned_generations: count_abandoned_generations(connection)?,
        scan,
    })
}

fn count_abandoned_generations(connection: &Connection) -> Result<u64, StoreError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM generations WHERE state = 'abandoned'",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|_| StoreError::IntegerOverflow)
}

fn validate_schema_manifest(connection: &Connection) -> Result<(), StoreError> {
    for (object_type, names) in [("table", REQUIRED_TABLES), ("index", REQUIRED_INDEXES)] {
        for name in names {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = ?1 AND name = ?2
                )",
                params![object_type, name],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StoreError::MissingSchemaObject((*name).to_owned()));
            }
        }
    }

    let executable_schema_objects: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE type IN ('trigger', 'view')",
        [],
        |row| row.get(0),
    )?;
    if executable_schema_objects != 0 {
        return Err(StoreError::UnexpectedExecutableSchema);
    }

    validate_schema_manifest_through(connection, SCHEMA_VERSION)
}

fn validate_schema_manifest_through(
    connection: &Connection,
    version: u32,
) -> Result<(), StoreError> {
    let actual = schema_manifest_fingerprint(connection, version == SCHEMA_VERSION)?;
    let expected_connection = Connection::open_in_memory()?;
    configure_connection(&expected_connection)?;
    expected_connection.pragma_update(None, "foreign_keys", false)?;
    for (migration_version, migration) in MIGRATIONS {
        if migration_version > version {
            break;
        }
        expected_connection.execute_batch(migration)?;
    }
    expected_connection.pragma_update(None, "foreign_keys", true)?;
    let expected = schema_manifest_fingerprint(&expected_connection, version == SCHEMA_VERSION)?;
    if actual != expected {
        return Err(StoreError::SchemaManifestMismatch);
    }

    Ok(())
}

fn schema_manifest_fingerprint(
    connection: &Connection,
    _require_current_count: bool,
) -> Result<Sha256Digest, StoreError> {
    let invalid_field: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM sqlite_schema
             WHERE typeof(type) != 'text'
                OR length(CAST(type AS BLOB)) NOT BETWEEN 1 AND ?1
                OR typeof(name) != 'text'
                OR length(CAST(name AS BLOB)) NOT BETWEEN 1 AND ?2
                OR typeof(tbl_name) != 'text'
                OR length(CAST(tbl_name AS BLOB)) NOT BETWEEN 1 AND ?3
                OR (
                    sql IS NOT NULL
                    AND (
                        typeof(sql) != 'text'
                        OR length(CAST(sql AS BLOB)) > ?4
                    )
                )
             LIMIT 1
         )",
        params![
            MAX_SCHEMA_OBJECT_TYPE_BYTES,
            MAX_SCHEMA_OBJECT_NAME_BYTES,
            MAX_SCHEMA_TABLE_NAME_BYTES,
            MAX_SCHEMA_SQL_BYTES,
        ],
        |row| row.get(0),
    )?;
    if invalid_field {
        return Err(StoreError::SchemaManifestMismatch);
    }

    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema
         ORDER BY type, name, tbl_name",
    )?;
    let mut rows = statement.query([])?;
    let mut hasher = Sha256::new();
    hasher.update(b"hsum.schema.manifest.v1\0");
    while let Some(row) = rows.next()? {
        for column in 0..4 {
            let field = row
                .get_ref(column)?
                .as_str()
                .map_err(|_| StoreError::SchemaManifestMismatch)?;
            let bytes = field.as_bytes();
            hasher.update(
                u64::try_from(bytes.len())
                    .expect("bounded SQLite schema text length fits in u64")
                    .to_be_bytes(),
            );
            hasher.update(bytes);
        }
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn validate_metadata(
    connection: &Connection,
    policy: FingerprintPolicy,
) -> Result<(IndexId, Sha256Digest), StoreError> {
    let metadata_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM index_meta", [], |row| row.get(0))?;
    if metadata_count != 15 {
        return Err(StoreError::InvalidMetadata("key set"));
    }

    expect_metadata(connection, "api_version", b"hsum.api.v1")?;
    expect_metadata(connection, "schema_version", b"4")?;
    let embedding_profile = read_embedding_profile(connection)?;
    expect_active_vector_slot(connection)?;
    expect_metadata(connection, "schema_checksum", schema_checksum().as_bytes()).map_err(
        |error| match error {
            StoreError::InvalidMetadata(_) => StoreError::SchemaChecksumMismatch,
            other => other,
        },
    )?;
    let raw_pipeline_fingerprint = metadata_value(connection, "pipeline_fingerprint")?;
    let stored_pipeline_fingerprint =
        digest_metadata(&raw_pipeline_fingerprint, "pipeline_fingerprint")?;
    if policy == FingerprintPolicy::Reject
        && stored_pipeline_fingerprint != pipeline_fingerprint_for(&embedding_profile)
    {
        return Err(StoreError::PipelineFingerprintMismatch);
    }

    let epoch = metadata_value(connection, "index_epoch")?;
    str::from_utf8(&epoch)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(StoreError::InvalidMetadata("index_epoch"))?;

    let active_generation = metadata_value(connection, "active_generation")?;
    if !active_generation.is_empty()
        && str::from_utf8(&active_generation)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .is_none()
    {
        return Err(StoreError::InvalidMetadata("active_generation"));
    }

    let history_floor_epoch = metadata_value(connection, "history_floor_epoch")?;
    if str::from_utf8(&history_floor_epoch)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .is_none_or(|value| value == 0)
    {
        return Err(StoreError::InvalidMetadata("history_floor_epoch"));
    }

    let replacement_epoch = metadata_value(connection, "replacement_epoch")?;
    if parse_metadata_u64(&replacement_epoch).is_none() {
        return Err(StoreError::InvalidMetadata("replacement_epoch"));
    }

    let raw_index_id = metadata_value(connection, "index_uuid")?;
    let uuid =
        Uuid::from_slice(&raw_index_id).map_err(|_| StoreError::InvalidMetadata("index_uuid"))?;
    Ok((IndexId::from_uuid(uuid), stored_pipeline_fingerprint))
}

fn validate_metadata_through(
    connection: &Connection,
    version: u32,
    policy: FingerprintPolicy,
) -> Result<(IndexId, Sha256Digest), StoreError> {
    let metadata_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM index_meta", [], |row| row.get(0))?;
    let expected_count = if version >= 4 {
        15
    } else if version >= 3 {
        10
    } else {
        8
    };
    if metadata_count != expected_count {
        return Err(StoreError::InvalidMetadata("key set"));
    }
    expect_metadata(connection, "api_version", b"hsum.api.v1")?;
    expect_metadata(connection, "schema_version", version.to_string().as_bytes())?;
    let expected_pipeline_fingerprint = if version >= 4 {
        let profile = read_embedding_profile(connection)?;
        expect_active_vector_slot(connection)?;
        pipeline_fingerprint_for(&profile)
    } else {
        expect_metadata(connection, "embedding_profile", b"none")?;
        pipeline_fingerprint()
    };
    expect_metadata(
        connection,
        "schema_checksum",
        schema_checksum_through(version).as_bytes(),
    )
    .map_err(|error| match error {
        StoreError::InvalidMetadata(_) => StoreError::SchemaChecksumMismatch,
        other => other,
    })?;
    let raw_pipeline_fingerprint = metadata_value(connection, "pipeline_fingerprint")?;
    let stored_pipeline_fingerprint =
        digest_metadata(&raw_pipeline_fingerprint, "pipeline_fingerprint")?;
    if policy == FingerprintPolicy::Reject
        && stored_pipeline_fingerprint != expected_pipeline_fingerprint
    {
        return Err(StoreError::PipelineFingerprintMismatch);
    }
    let index_epoch = metadata_value(connection, "index_epoch")?;
    parse_metadata_u64(&index_epoch).ok_or(StoreError::InvalidMetadata("index_epoch"))?;
    let active_generation = metadata_value(connection, "active_generation")?;
    if !active_generation.is_empty() && parse_metadata_u64(&active_generation).is_none() {
        return Err(StoreError::InvalidMetadata("active_generation"));
    }
    if version >= 3 {
        history_floor_epoch(connection)?;
        parse_metadata_u64(&metadata_value(connection, "replacement_epoch")?)
            .ok_or(StoreError::InvalidMetadata("replacement_epoch"))?;
    }
    let raw_index_id = metadata_value(connection, "index_uuid")?;
    let uuid =
        Uuid::from_slice(&raw_index_id).map_err(|_| StoreError::InvalidMetadata("index_uuid"))?;
    Ok((IndexId::from_uuid(uuid), stored_pipeline_fingerprint))
}

fn expect_active_vector_slot(connection: &Connection) -> Result<(), StoreError> {
    match metadata_value(connection, "active_vector_slot")?.as_slice() {
        b"0" | b"1" => Ok(()),
        _ => Err(StoreError::InvalidMetadata("active_vector_slot")),
    }
}

fn validate_migration_chain(connection: &Connection) -> Result<(), StoreError> {
    validate_migration_chain_through(connection, SCHEMA_VERSION)
}

fn validate_migration_chain_through(
    connection: &Connection,
    through_version: u32,
) -> Result<(), StoreError> {
    let row_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    if row_count != i64::from(through_version) {
        return Err(StoreError::MigrationChainInvalid);
    }

    for version in 1..=through_version {
        let row = connection.query_row(
            "SELECT version, applied_at, checksum
             FROM schema_migrations WHERE version = ?1",
            [i64::from(version)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        let expected = migration_checksum(version).ok_or(StoreError::MigrationChainInvalid)?;
        if row.0 != i64::from(version)
            || row.1.is_empty()
            || row.2.as_slice() != expected.as_bytes()
        {
            return Err(StoreError::MigrationChainInvalid);
        }
    }

    Ok(())
}

fn validate_integrity(connection: &Connection) -> Result<(), StoreError> {
    let quick_check: String =
        connection.pragma_query_value(None, "quick_check", |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(StoreError::IntegrityCheckFailed(quick_check));
    }

    let foreign_key_violation = connection
        .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
        .optional()?
        .is_some();
    if foreign_key_violation {
        return Err(StoreError::ForeignKeyCheckFailed);
    }

    Ok(())
}

fn validate_forget_ledger(connection: &Connection) -> Result<(), StoreError> {
    let live_forgotten_revision: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM forgotten_documents AS forgotten
             JOIN document_versions AS version
               ON version.document_id = forgotten.document_id
         )",
        [],
        |row| row.get(0),
    )?;
    if live_forgotten_revision {
        return Err(StoreError::ForgetLedgerMismatch);
    }

    let run_count_mismatch: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM forget_runs AS run
             WHERE run.affected_revision_count != (
                 SELECT COUNT(*)
                 FROM forgotten_documents AS forgotten
                 WHERE forgotten.forget_plan_hash = run.plan_hash
             )
         )",
        [],
        |row| row.get(0),
    )?;
    if run_count_mismatch {
        return Err(StoreError::ForgetLedgerMismatch);
    }

    let replacement_epoch = parse_metadata_u64(&metadata_value(connection, "replacement_epoch")?)
        .ok_or(StoreError::InvalidMetadata("replacement_epoch"))?;
    let replacement_epoch =
        i64::try_from(replacement_epoch).map_err(|_| StoreError::IntegerOverflow)?;
    let future_run: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM forget_runs WHERE post_replacement_epoch > ?1
             UNION ALL
             SELECT 1 FROM restore_runs WHERE post_replacement_epoch > ?1
         )",
        [replacement_epoch],
        |row| row.get(0),
    )?;
    if future_run {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    let index_epoch = parse_metadata_u64(&metadata_value(connection, "index_epoch")?)
        .ok_or(StoreError::InvalidMetadata("index_epoch"))?;
    let index_epoch = i64::try_from(index_epoch).map_err(|_| StoreError::IntegerOverflow)?;
    let future_evidence_run: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM forget_runs WHERE post_index_epoch > ?1
             UNION ALL
             SELECT 1 FROM restore_runs WHERE post_index_epoch > ?1
         )",
        [index_epoch],
        |row| row.get(0),
    )?;
    if future_evidence_run {
        return Err(StoreError::ForgetLedgerMismatch);
    }
    Ok(())
}

fn validate_generation_invariants(
    connection: &Connection,
    stored_pipeline_fingerprint: Sha256Digest,
    history_floor_epoch: u64,
    vector_schema: bool,
) -> Result<(), StoreError> {
    let unexpected_pipeline_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM generations WHERE pipeline_fingerprint != ?1",
        [stored_pipeline_fingerprint.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    if unexpected_pipeline_count != 0 {
        return Err(StoreError::GenerationInvariant(
            "generation pipeline fingerprint",
        ));
    }

    let replay = replay_committed_generations(connection, history_floor_epoch, vector_schema)?;
    let raw_active_generation = metadata_value(connection, "active_generation")?;
    let active_generation = if raw_active_generation.is_empty() {
        None
    } else {
        Some(
            str::from_utf8(&raw_active_generation)
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value > 0)
                .ok_or(StoreError::GenerationInvariant(
                    "active generation identifier",
                ))?,
        )
    };
    if active_generation != replay.latest_committed {
        return Err(StoreError::GenerationInvariant(
            "active generation is not the latest committed generation",
        ));
    }
    let index_epoch = metadata_value(connection, "index_epoch")?;
    let index_epoch = str::from_utf8(&index_epoch)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(StoreError::GenerationInvariant("index epoch"))?;
    let expected_epoch = if replay.committed_count == 0 {
        0
    } else {
        history_floor_epoch
            .checked_sub(1)
            .and_then(|floor| floor.checked_add(replay.committed_count))
            .ok_or(StoreError::IntegerOverflow)?
    };
    if index_epoch != expected_epoch {
        return Err(StoreError::GenerationInvariant(
            "index epoch does not equal retained committed activation history",
        ));
    }

    let document_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))?;
    let head_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM document_heads", [], |row| row.get(0))?;
    if document_count != head_count {
        return Err(StoreError::GenerationInvariant(
            "every document must have exactly one head",
        ));
    }

    let invalid_head_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM document_heads AS dh
         JOIN documents AS d ON d.id = dh.document_id
         JOIN generations AS g ON g.id = dh.generation_id
         WHERE g.state != 'committed'
            OR (dh.state = 'active' AND d.tombstoned_at IS NOT NULL)
            OR (dh.state = 'tombstoned' AND d.tombstoned_at IS NULL)
            OR NOT EXISTS (
                SELECT 1
                FROM generation_changes AS gc
                WHERE gc.generation_id = dh.generation_id
                  AND gc.document_id = dh.document_id
                  AND gc.next_state = dh.state
                  AND (
                      gc.next_version_id = dh.document_version_id
                      OR (
                          gc.next_version_id IS NULL
                          AND dh.document_version_id IS NULL
                      )
                  )
            )",
        [],
        |row| row.get(0),
    )?;
    if invalid_head_count != 0 {
        return Err(StoreError::GenerationInvariant(
            "document head does not match a committed generation change",
        ));
    }

    let mut statement = connection.prepare(
        "SELECT d.connector_key, d.current_source_uri, dv.source_uri, s.kind
         FROM document_heads AS dh
         JOIN documents AS d ON d.id = dh.document_id
         JOIN sources AS s ON s.id = d.source_id
         JOIN document_versions AS dv ON dv.id = dh.document_version_id
         WHERE dh.state = 'active'
         ORDER BY dh.document_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let connector_key = row.get::<_, Vec<u8>>(0)?;
        let current_source_uri = row.get::<_, String>(1)?;
        let version_source_uri = row.get::<_, String>(2)?;
        let source_kind = row.get::<_, String>(3)?;
        let invalid = match source_kind.as_str() {
            "filesystem" => {
                let expected = repo_uri(&connector_key);
                current_source_uri != expected || version_source_uri != expected
            }
            "jsonl" => current_source_uri != version_source_uri,
            _ => true,
        };
        if invalid {
            return Err(StoreError::GenerationInvariant(
                "active document identity does not match its connector key",
            ));
        }
    }

    Ok(())
}

fn validate_generation_embedding_profile(connection: &Connection) -> Result<(), StoreError> {
    let profile = read_embedding_profile(connection)?;
    let invalid: bool = match profile {
        IndexEmbeddingProfile::LexicalOnly => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM generations
                 WHERE embedding_model_id IS NOT NULL
                    OR embedding_revision IS NOT NULL
                    OR embedding_model_fingerprint IS NOT NULL
                    OR embedding_dimension IS NOT NULL
                    OR vector_state != 'absent'
             )",
            [],
            |row| row.get(0),
        )?,
        IndexEmbeddingProfile::Pinned(pin) => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM generations
                 WHERE embedding_model_id IS NULL
                    OR embedding_model_id != ?1
                    OR embedding_revision IS NULL
                    OR embedding_revision != ?2
                    OR embedding_model_fingerprint IS NULL
                    OR embedding_model_fingerprint != ?3
                    OR embedding_dimension IS NULL
                    OR embedding_dimension != ?4
                    OR vector_state NOT IN ('absent', 'complete')
             )",
            params![
                pin.model_id(),
                pin.upstream_revision(),
                pin.model_fingerprint().as_bytes().as_slice(),
                i64::from(pin.dimension()),
            ],
            |row| row.get(0),
        )?,
    };
    if invalid {
        return Err(StoreError::GenerationInvariant(
            "generation embedding profile",
        ));
    }
    Ok(())
}

fn validate_embedding_cache(connection: &Connection) -> Result<(), StoreError> {
    let profile = read_embedding_profile(connection)?;
    if profile == IndexEmbeddingProfile::LexicalOnly {
        let unexpected: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM embedding_provenance)
                 OR EXISTS(SELECT 1 FROM chunk_embeddings)
                 OR EXISTS(SELECT 1 FROM passages_vec_a)
                 OR EXISTS(SELECT 1 FROM passages_vec_b)",
            [],
            |row| row.get(0),
        )?;
        if unexpected {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "lexical-only embedding storage",
            ));
        }
        return Ok(());
    }
    let IndexEmbeddingProfile::Pinned(pin) = profile else {
        unreachable!("lexical-only profile returned above")
    };

    let mut provenance = BTreeMap::new();
    let mut provenance_statement = connection.prepare(
        "SELECT fingerprint, compatibility_fingerprint, schema_version, canonical_json
         FROM embedding_provenance ORDER BY fingerprint",
    )?;
    let mut rows = provenance_statement.query([])?;
    while let Some(row) = rows.next()? {
        if provenance.len() >= 64 {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "embedding provenance cardinality",
            ));
        }
        let raw_fingerprint = row.get::<_, Vec<u8>>(0)?;
        let raw_compatibility = row.get::<_, Vec<u8>>(1)?;
        let schema_version = row.get::<_, String>(2)?;
        let canonical_json = row.get::<_, String>(3)?;
        let record = validate_stored_provenance(
            &canonical_json,
            &raw_fingerprint,
            &raw_compatibility,
            &schema_version,
        )?;
        if record.model_id() != pin.model_id()
            || record.upstream_revision() != pin.upstream_revision()
            || record.manifest_fingerprint() != pin.model_fingerprint()
            || record.dimension() != pin.dimension()
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "embedding provenance model pin",
            ));
        }
        let fingerprint = digest_from_blob(&raw_fingerprint)?;
        if provenance.insert(fingerprint, record).is_some() {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "embedding provenance identity",
            ));
        }
    }

    let mut embedding_statement = connection.prepare(
        "SELECT model_fingerprint, vector_dimension, vector_blob, provenance_fingerprint
         FROM chunk_embeddings ORDER BY id",
    )?;
    let mut rows = embedding_statement.query([])?;
    while let Some(row) = rows.next()? {
        let model_fingerprint = digest_from_blob(&row.get::<_, Vec<u8>>(0)?)?;
        let dimension = row.get::<_, i64>(1)?;
        let vector_blob = row.get::<_, Vec<u8>>(2)?;
        let provenance_fingerprint = digest_from_blob(&row.get::<_, Vec<u8>>(3)?)?;
        if model_fingerprint != pin.model_fingerprint()
            || dimension != i64::from(pin.dimension())
            || !provenance.contains_key(&provenance_fingerprint)
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "chunk embedding model provenance",
            ));
        }
        validate_stored_vector_blob(&vector_blob)?;
    }
    validate_active_vector_membership(connection)?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReplayedHead {
    document_version_id: Option<i64>,
    state: String,
    generation_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GenerationReplay {
    latest_committed: Option<i64>,
    committed_count: u64,
}

fn replay_committed_generations(
    connection: &Connection,
    history_floor_epoch: u64,
    vector_schema: bool,
) -> Result<GenerationReplay, StoreError> {
    let noncommitted_change_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM generation_changes AS gc
         JOIN generations AS g ON g.id = gc.generation_id
         WHERE g.state != 'committed'",
        [],
        |row| row.get(0),
    )?;
    if noncommitted_change_count != 0 {
        return Err(StoreError::GenerationInvariant(
            "noncommitted generation contains document changes",
        ));
    }

    let (earliest_committed, latest_committed, raw_committed_count) = connection.query_row(
        "SELECT MIN(id), MAX(id), COUNT(*)
         FROM generations
         WHERE state = 'committed'",
        [],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let committed_count =
        u64::try_from(raw_committed_count).map_err(|_| StoreError::IntegerOverflow)?;

    let vector_generation_clause = if vector_schema {
        "AND g.vector_state != 'complete'"
    } else {
        ""
    };
    let empty_committed_generation = connection
        .query_row(
            &format!(
                "SELECT g.id
             FROM generations AS g
             WHERE g.state = 'committed'
               {vector_generation_clause}
               AND NOT EXISTS (
                   SELECT 1
                   FROM generation_changes AS gc
                   WHERE gc.generation_id = g.id
               )
             ORDER BY g.id
             LIMIT 1"
            ),
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if empty_committed_generation.is_some() {
        return Err(StoreError::GenerationInvariant(
            "committed generation has no document changes",
        ));
    }

    // The required index supplies replay order directly. CROSS JOIN fixes
    // generation_changes as the outer loop so SQLite cannot trade it for a
    // corpus-sized temporary sort while filtering committed generations.
    let mut change_statement = connection.prepare(
        "SELECT gc.document_id, gc.generation_id, gc.prior_version_id,
                gc.next_version_id, gc.next_state
         FROM generation_changes AS gc
              INDEXED BY generation_changes_document_idx
         CROSS JOIN generations AS g
         WHERE g.id = gc.generation_id
           AND g.state = 'committed'
         ORDER BY gc.document_id, gc.generation_id",
    )?;
    let mut change_rows = change_statement.query([])?;
    let mut head_statement = connection.prepare(
        "SELECT document_id, document_version_id, state, generation_id
         FROM document_heads
         ORDER BY document_id",
    )?;
    let mut head_rows = head_statement.query([])?;
    let mut next_head = next_stored_head(&mut head_rows)?;
    let mut current_document_id = None::<Vec<u8>>;
    let mut replayed_head = None::<ReplayedHead>;

    while let Some(row) = change_rows.next()? {
        let document_id = row.get::<_, Vec<u8>>(0)?;
        if current_document_id
            .as_deref()
            .is_some_and(|current| current != document_id.as_slice())
        {
            compare_replayed_head(
                current_document_id
                    .as_deref()
                    .expect("a replayed head always has a document identifier"),
                replayed_head
                    .as_ref()
                    .expect("a replayed document always has a final head"),
                &mut next_head,
                &mut head_rows,
            )?;
            replayed_head = None;
        }
        if current_document_id.as_deref() != Some(document_id.as_slice()) {
            current_document_id = Some(document_id);
        }

        let generation_id = row.get::<_, i64>(1)?;
        let prior_version_id = row.get::<_, Option<i64>>(2)?;
        let next_version_id = row.get::<_, Option<i64>>(3)?;
        let next_state =
            row.get::<_, Option<String>>(4)?
                .ok_or(StoreError::GenerationInvariant(
                    "generation change has no next state",
                ))?;
        let previous = replayed_head.as_ref();
        let replayed_prior = previous.and_then(|head| head.document_version_id);
        if prior_version_id != replayed_prior {
            return Err(StoreError::GenerationInvariant(
                "generation change prior version does not match replayed head",
            ));
        }

        match next_state.as_str() {
            "active" if next_version_id.is_some() && next_version_id != prior_version_id => {}
            "tombstoned"
                if next_version_id.is_none()
                    && (previous.is_some_and(|head| head.state == "active")
                        || (previous.is_none()
                            && history_floor_epoch > 1
                            && Some(generation_id) == earliest_committed)) => {}
            "active" | "tombstoned" => {
                return Err(StoreError::GenerationInvariant(
                    "generation change next state and version are incoherent",
                ));
            }
            _ => {
                return Err(StoreError::GenerationInvariant(
                    "generation change has an unknown next state",
                ));
            }
        }

        replayed_head = Some(ReplayedHead {
            document_version_id: next_version_id,
            state: next_state,
            generation_id,
        });
    }
    if let (Some(document_id), Some(head)) =
        (current_document_id.as_deref(), replayed_head.as_ref())
    {
        compare_replayed_head(document_id, head, &mut next_head, &mut head_rows)?;
    }
    if next_head.is_some() {
        return Err(StoreError::GenerationInvariant(
            "document heads do not equal replayed generation history",
        ));
    }

    Ok(GenerationReplay {
        latest_committed,
        committed_count,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct StoredHead {
    document_id: Vec<u8>,
    head: ReplayedHead,
}

fn next_stored_head(rows: &mut rusqlite::Rows<'_>) -> Result<Option<StoredHead>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(StoredHead {
        document_id: row.get(0)?,
        head: ReplayedHead {
            document_version_id: row.get(1)?,
            state: row.get(2)?,
            generation_id: row.get(3)?,
        },
    }))
}

fn compare_replayed_head(
    document_id: &[u8],
    replayed: &ReplayedHead,
    next_head: &mut Option<StoredHead>,
    head_rows: &mut rusqlite::Rows<'_>,
) -> Result<(), StoreError> {
    if next_head.as_ref().is_none_or(|stored| {
        stored.document_id.as_slice() != document_id || &stored.head != replayed
    }) {
        return Err(StoreError::GenerationInvariant(
            "document heads do not equal replayed generation history",
        ));
    }
    *next_head = next_stored_head(head_rows)?;
    Ok(())
}

fn validate_immutable_evidence(
    connection: &Connection,
    scan: &mut DoctorScanStats,
    body_scan: &BodyScanTracker,
) -> Result<(), StoreError> {
    validate_content_blobs(connection, scan, body_scan)?;
    validate_chunk_layouts(connection, scan, body_scan)?;
    validate_document_versions(connection, scan, body_scan)
}

fn validate_content_blobs(
    connection: &Connection,
    scan: &mut DoctorScanStats,
    body_scan: &BodyScanTracker,
) -> Result<(), StoreError> {
    let mut statement = connection
        .prepare("SELECT id, body_sha256, original_bytes FROM content_blobs ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let stored_digest = row.get::<_, Vec<u8>>(1)?;
        let body = blob_column(row, 2, "content blob body")?;
        let _body_lease = body_scan.acquire()?;
        if digest_from_blob(&stored_digest)? != Sha256Digest::of_bytes(body) {
            return Err(StoreError::ImmutableEvidenceMismatch("content blob digest"));
        }
        scan.observe_original_body(body.len())?;
    }
    Ok(())
}

struct StoredChunk {
    layout_id: i64,
    ordinal: i64,
    start_byte: i64,
    end_byte: i64,
    start_line: i64,
    end_line: i64,
    body_text: String,
    content_sha256: Vec<u8>,
    quote_bloom: Vec<u8>,
}

fn validate_chunk_layouts(
    connection: &Connection,
    scan: &mut DoctorScanStats,
    body_scan: &BodyScanTracker,
) -> Result<(), StoreError> {
    let mut layout_statement = connection.prepare(
        "SELECT cl.id, cl.chunker_fingerprint, cb.original_bytes
         FROM chunk_layouts AS cl
         LEFT JOIN content_blobs AS cb ON cb.id = cl.content_blob_id
         ORDER BY cl.id",
    )?;
    let mut layout_rows = layout_statement.query([])?;
    let mut chunk_statement = connection.prepare(
        "SELECT chunk_layout_id, ordinal, start_byte, end_byte,
                start_line, end_line, body_text, content_sha256, quote_bloom
         FROM chunks
         ORDER BY chunk_layout_id, ordinal",
    )?;
    let mut chunk_rows = chunk_statement.query([])?;
    let mut actual = next_stored_chunk(&mut chunk_rows)?;

    while let Some(row) = layout_rows.next()? {
        let layout_id = row.get::<_, i64>(0)?;
        let fingerprint = digest_from_blob(&row.get::<_, Vec<u8>>(1)?)?;
        let kind = chunk_kind_for_fingerprint(fingerprint).ok_or(
            StoreError::ImmutableEvidenceMismatch("unknown chunk layout fingerprint"),
        )?;
        let expected = {
            let body = blob_column(row, 2, "chunk layout content blob")?;
            let _body_lease = body_scan.acquire()?;
            chunk_bytes(body, kind, ChunkSettings::default()).map_err(|_| {
                StoreError::ImmutableEvidenceMismatch(
                    "stored body cannot be deterministically chunked",
                )
            })?
        };
        checked_increment(&mut scan.chunk_layouts)?;

        if actual
            .as_ref()
            .is_some_and(|chunk| chunk.layout_id < layout_id)
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "chunk layout cardinality",
            ));
        }
        for expected_chunk in &expected {
            let stored = actual.take().ok_or(StoreError::ImmutableEvidenceMismatch(
                "chunk layout cardinality",
            ))?;
            if stored.layout_id != layout_id {
                return Err(StoreError::ImmutableEvidenceMismatch(
                    "chunk layout cardinality",
                ));
            }
            validate_stored_chunk(&stored, expected_chunk)?;
            checked_increment(&mut scan.chunks)?;
            actual = next_stored_chunk(&mut chunk_rows)?;
        }
        if actual
            .as_ref()
            .is_some_and(|chunk| chunk.layout_id == layout_id)
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "chunk layout cardinality",
            ));
        }
    }
    if actual.is_some() {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "chunk layout cardinality",
        ));
    }
    Ok(())
}

fn next_stored_chunk(rows: &mut rusqlite::Rows<'_>) -> Result<Option<StoredChunk>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(StoredChunk {
        layout_id: row.get(0)?,
        ordinal: row.get(1)?,
        start_byte: row.get(2)?,
        end_byte: row.get(3)?,
        start_line: row.get(4)?,
        end_line: row.get(5)?,
        body_text: row.get(6)?,
        content_sha256: row.get(7)?,
        quote_bloom: row.get(8)?,
    }))
}

fn validate_stored_chunk(stored: &StoredChunk, expected: &Chunk) -> Result<(), StoreError> {
    if stored.ordinal != i64::from(expected.ordinal())
        || stored.start_byte != integer_from_u64(expected.span().start())?
        || stored.end_byte != integer_from_u64(expected.span().end())?
        || stored.start_line != integer_from_u64(expected.line_span().start())?
        || stored.end_line != integer_from_u64(expected.line_span().end())?
        || stored.body_text != expected.text()
        || digest_from_blob(&stored.content_sha256)?
            != Sha256Digest::of_bytes(expected.text().as_bytes())
        || stored.quote_bloom.as_slice()
            != QuoteBloom::from_content(expected.text().as_bytes()).as_bytes()
    {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "deterministic chunk content",
        ));
    }
    Ok(())
}

struct VersionEvidence {
    content_blob_id: i64,
    raw_revision: Vec<u8>,
    source_uri: String,
    title: Option<String>,
    metadata_json: String,
    source_updated_at: Option<String>,
    source_kind: String,
}

fn validate_document_versions(
    connection: &Connection,
    scan: &mut DoctorScanStats,
    body_scan: &BodyScanTracker,
) -> Result<(), StoreError> {
    let mut body_statement =
        connection.prepare("SELECT id, original_bytes FROM content_blobs ORDER BY id")?;
    let mut body_rows = body_statement.query([])?;
    let mut version_statement = connection.prepare(
        "SELECT dv.content_blob_id, dv.revision_sha256, dv.source_uri, dv.title,
                dv.metadata_json, dv.source_updated_at, s.kind
         FROM document_versions AS dv
         JOIN documents AS d ON d.id = dv.document_id
         JOIN sources AS s ON s.id = d.source_id
         ORDER BY dv.content_blob_id, dv.id",
    )?;
    let mut version_rows = version_statement.query([])?;
    let mut layout_statement = connection.prepare(
        "SELECT content_blob_id, chunker_fingerprint
         FROM chunk_layouts
         ORDER BY content_blob_id, chunker_fingerprint",
    )?;
    let mut layout_rows = layout_statement.query([])?;
    let mut version = next_version_evidence(&mut version_rows)?;
    let mut layout = next_layout_fingerprint(&mut layout_rows)?;

    while let Some(row) = body_rows.next()? {
        let content_blob_id = row.get::<_, i64>(0)?;
        let body = blob_column(row, 1, "document version content blob")?;
        let _body_lease = body_scan.acquire()?;
        let mut available_layouts = [false; ChunkKind::ALL.len()];
        while layout
            .as_ref()
            .is_some_and(|(layout_blob_id, _)| *layout_blob_id == content_blob_id)
        {
            let (_, fingerprint) = layout
                .take()
                .expect("a matching layout was present before it was consumed");
            let kind = chunk_kind_for_fingerprint(fingerprint).ok_or(
                StoreError::ImmutableEvidenceMismatch("unknown chunk layout fingerprint"),
            )?;
            let kind_index = ChunkKind::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .expect("every recognized chunk kind is in ChunkKind::ALL");
            available_layouts[kind_index] = true;
            layout = next_layout_fingerprint(&mut layout_rows)?;
        }
        if layout
            .as_ref()
            .is_some_and(|(layout_blob_id, _)| *layout_blob_id < content_blob_id)
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "chunk layout content blob",
            ));
        }

        while version
            .as_ref()
            .is_some_and(|evidence| evidence.content_blob_id == content_blob_id)
        {
            let evidence = version
                .take()
                .expect("a matching version was present before it was consumed");
            validate_version_evidence(&evidence, body, &available_layouts)?;
            checked_increment(&mut scan.document_versions)?;
            version = next_version_evidence(&mut version_rows)?;
        }
        if version
            .as_ref()
            .is_some_and(|evidence| evidence.content_blob_id < content_blob_id)
        {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "document version content blob",
            ));
        }
    }
    if layout.is_some() {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "chunk layout content blob",
        ));
    }
    if version.is_some() {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "document version content blob",
        ));
    }
    Ok(())
}

fn next_version_evidence(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<VersionEvidence>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(VersionEvidence {
        content_blob_id: row.get(0)?,
        raw_revision: row.get(1)?,
        source_uri: row.get(2)?,
        title: row.get(3)?,
        metadata_json: row.get(4)?,
        source_updated_at: row.get(5)?,
        source_kind: row.get(6)?,
    }))
}

fn next_layout_fingerprint(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<(i64, Sha256Digest)>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((
        row.get(0)?,
        digest_from_blob(&row.get::<_, Vec<u8>>(1)?)?,
    )))
}

fn validate_version_evidence(
    evidence: &VersionEvidence,
    body: &[u8],
    available_layouts: &[bool; ChunkKind::ALL.len()],
) -> Result<(), StoreError> {
    let title = evidence
        .title
        .as_deref()
        .ok_or(StoreError::ImmutableEvidenceMismatch(
            "document version title",
        ))?;
    let metadata: Value = serde_json::from_str(&evidence.metadata_json)
        .map_err(|_| StoreError::ImmutableEvidenceMismatch("document version metadata"))?;
    if !metadata.is_object()
        || to_canonical_json(&metadata)
            .map_err(|_| StoreError::ImmutableEvidenceMismatch("document version metadata"))?
            != evidence.metadata_json
    {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "canonical document version metadata",
        ));
    }
    let expected_revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri: &evidence.source_uri,
        title,
        metadata: &metadata,
        source_updated_at: evidence.source_updated_at.as_deref(),
    })
    .map_err(|_| StoreError::ImmutableEvidenceMismatch("document version revision"))?;
    if digest_from_blob(&evidence.raw_revision)? != expected_revision {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "document version revision",
        ));
    }
    let kind = match evidence.source_kind.as_str() {
        "filesystem" => ChunkKind::from_path(Path::new(&evidence.source_uri)).ok_or(
            StoreError::ImmutableEvidenceMismatch("document version source kind"),
        )?,
        "jsonl" => ChunkKind::PlainText,
        _ => {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "document version source kind",
            ));
        }
    };
    let kind_index = ChunkKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .expect("a parsed chunk kind is in ChunkKind::ALL");
    if !available_layouts[kind_index] {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "document version chunk layout",
        ));
    }
    Ok(())
}

fn validate_active_indexes(
    connection: &Connection,
    scan: &mut DoctorScanStats,
) -> Result<(), StoreError> {
    validate_active_membership(connection, scan)?;
    validate_fts_content(connection)?;
    validate_literal_content(connection)
}

type ActiveMembership = (Vec<u8>, i64, i64, Vec<u8>);

fn validate_active_membership(
    connection: &Connection,
    scan: &mut DoctorScanStats,
) -> Result<(), StoreError> {
    let mut expected_statement = connection.prepare(
        "SELECT dh.document_id, dv.id, c.id, d.source_id,
                dv.source_uri, cl.chunker_fingerprint, s.kind
         FROM document_heads AS dh
         JOIN documents AS d ON d.id = dh.document_id
         JOIN sources AS s ON s.id = d.source_id
         JOIN document_versions AS dv ON dv.id = dh.document_version_id
         JOIN chunk_layouts AS cl ON cl.content_blob_id = dv.content_blob_id
         JOIN chunks AS c ON c.chunk_layout_id = cl.id
         WHERE dh.state = 'active'
         ORDER BY dh.document_id, dv.id, c.id, d.source_id,
                  cl.chunker_fingerprint",
    )?;
    let mut expected_rows = expected_statement.query([])?;
    let mut actual_statement = connection.prepare(
        "SELECT document_id, document_version_id, chunk_id, source_id
         FROM active_passages
         ORDER BY document_id, document_version_id, chunk_id, source_id",
    )?;
    let mut actual_rows = actual_statement.query([])?;

    loop {
        let expected = next_expected_membership(&mut expected_rows)?;
        let actual = next_actual_membership(&mut actual_rows)?;
        if expected != actual {
            return Err(StoreError::ActiveIndexParity("active_passages"));
        }
        if expected.is_none() {
            return Ok(());
        }
        checked_increment(&mut scan.active_passages)?;
    }
}

fn next_expected_membership(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<ActiveMembership>, StoreError> {
    while let Some(row) = rows.next()? {
        let source_uri = row.get::<_, String>(4)?;
        let kind = match row.get::<_, String>(6)?.as_str() {
            "filesystem" => ChunkKind::from_path(Path::new(&source_uri))
                .ok_or(StoreError::ActiveIndexParity("active source kind"))?,
            "jsonl" => ChunkKind::PlainText,
            _ => return Err(StoreError::ActiveIndexParity("active source kind")),
        };
        if digest_from_blob(&row.get::<_, Vec<u8>>(5)?)? == chunker_fingerprint(kind) {
            return Ok(Some((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            )));
        }
    }
    Ok(None)
}

fn next_actual_membership(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<ActiveMembership>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((
        row.get::<_, Vec<u8>>(0)?,
        row.get::<_, i64>(1)?,
        row.get::<_, i64>(2)?,
        row.get::<_, Vec<u8>>(3)?,
    )))
}

type PassageContent = (i64, String, String, String);

fn validate_fts_content(connection: &Connection) -> Result<(), StoreError> {
    let mut expected_statement = connection.prepare(
        "SELECT ap.id, COALESCE(dv.title, ''), dv.source_uri, c.body_text
         FROM active_passages AS ap
         JOIN document_versions AS dv
           ON dv.id = ap.document_version_id
         JOIN chunks AS c ON c.id = ap.chunk_id
         ORDER BY ap.id",
    )?;
    let mut expected_rows = expected_statement.query([])?;
    let mut actual_statement = connection.prepare(
        "SELECT rowid, title, source_uri, body
         FROM passages_fts
         ORDER BY rowid",
    )?;
    let mut actual_rows = actual_statement.query([])?;

    loop {
        let expected = next_passage_content(&mut expected_rows)?;
        let actual = next_passage_content(&mut actual_rows)?;
        if expected != actual {
            return Err(StoreError::ActiveIndexParity("passages_fts"));
        }
        if expected.is_none() {
            return Ok(());
        }
    }
}

fn next_passage_content(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<PassageContent>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
    )))
}

type LiteralContent = (i64, Vec<u8>, String);

fn validate_literal_content(connection: &Connection) -> Result<(), StoreError> {
    let mut passage_statement = connection.prepare(
        "SELECT ap.id, COALESCE(dv.title, ''), dv.source_uri, c.body_text
         FROM active_passages AS ap
         JOIN document_versions AS dv ON dv.id = ap.document_version_id
         JOIN chunks AS c ON c.id = ap.chunk_id
         ORDER BY ap.id",
    )?;
    let mut passage_rows = passage_statement.query([])?;
    let mut actual_statement = connection.prepare(
        "SELECT passage_id, literal, field
         FROM passage_literals
         ORDER BY passage_id, literal, field",
    )?;
    let mut actual_rows = actual_statement.query([])?;
    let mut actual = next_literal_content(&mut actual_rows)?;

    while let Some(row) = passage_rows.next()? {
        let passage_id = row.get::<_, i64>(0)?;
        let title = row.get::<_, String>(1)?;
        let source_uri = row.get::<_, String>(2)?;
        let body = row.get::<_, String>(3)?;
        let mut expected = prepare_passage_literals(&title, &source_uri, body.as_bytes())
            .into_iter()
            .map(|(literal, field)| (passage_id, literal, field.as_str().to_owned()))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        for expected_literal in expected {
            if actual.as_ref() != Some(&expected_literal) {
                return Err(StoreError::ActiveIndexParity("passage_literals"));
            }
            actual = next_literal_content(&mut actual_rows)?;
        }
    }
    if actual.is_some() {
        return Err(StoreError::ActiveIndexParity("passage_literals"));
    }
    Ok(())
}

fn next_literal_content(
    rows: &mut rusqlite::Rows<'_>,
) -> Result<Option<LiteralContent>, StoreError> {
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((
        row.get::<_, i64>(0)?,
        row.get::<_, Vec<u8>>(1)?,
        row.get::<_, String>(2)?,
    )))
}

fn expect_metadata(
    connection: &Connection,
    key: &'static str,
    expected: &[u8],
) -> Result<(), StoreError> {
    if metadata_value(connection, key)? != expected {
        return Err(StoreError::InvalidMetadata(key));
    }
    Ok(())
}

fn metadata_value(connection: &Connection, key: &'static str) -> Result<Vec<u8>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::InvalidMetadata(key))
}

fn parse_metadata_u64(bytes: &[u8]) -> Option<u64> {
    str::from_utf8(bytes).ok()?.parse().ok()
}

fn history_floor_epoch(connection: &Connection) -> Result<u64, StoreError> {
    parse_metadata_u64(&metadata_value(connection, "history_floor_epoch")?)
        .filter(|value| *value > 0)
        .ok_or(StoreError::InvalidMetadata("history_floor_epoch"))
}

fn digest_metadata(bytes: &[u8], key: &'static str) -> Result<Sha256Digest, StoreError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::InvalidMetadata(key))?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn digest_from_blob(bytes: &[u8]) -> Result<Sha256Digest, StoreError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::ImmutableEvidenceMismatch("digest length"))?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn blob_column<'row>(
    row: &'row rusqlite::Row<'_>,
    index: usize,
    mismatch: &'static str,
) -> Result<&'row [u8], StoreError> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Blob(value) => Ok(value),
        _ => Err(StoreError::ImmutableEvidenceMismatch(mismatch)),
    }
}

fn checked_increment(value: &mut u64) -> Result<(), StoreError> {
    *value = value.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
    Ok(())
}

fn integer_from_u64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOverflow)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::{BodyScanTracker, Doctor, FingerprintPolicy};
    use crate::domain::{IndexId, Sha256Digest};
    use crate::store::{IndexDb, StoreError};

    const OLD_FINGERPRINT: [u8; 32] = [0xaa; 32];
    const OTHER_FINGERPRINT: [u8; 32] = [0xbb; 32];

    #[test]
    fn body_scan_tracker_measures_nested_retention_instead_of_assuming_one() {
        let tracker = BodyScanTracker::default();
        let first = tracker.acquire().unwrap();
        assert_eq!(tracker.live(), 1);
        assert_eq!(tracker.max(), 1);

        {
            let _second = tracker.acquire().unwrap();
            assert_eq!(tracker.live(), 2);
            assert_eq!(tracker.max(), 2);
        }

        assert_eq!(tracker.live(), 1);
        drop(first);
        assert_eq!(tracker.live(), 0);
        assert_eq!(tracker.max(), 2);
        assert_eq!(tracker.finish(), 2);
    }

    #[test]
    fn tolerant_doctor_reports_a_coherent_stored_fingerprint() {
        let (_directory, path) = fresh_index();
        set_metadata_fingerprint(&path, &OLD_FINGERPRINT);

        let report = Doctor::run_with_policy(&path, FingerprintPolicy::Tolerate).unwrap();

        assert_eq!(
            report.pipeline_fingerprint,
            Sha256Digest::from_bytes(OLD_FINGERPRINT)
        );
    }

    #[test]
    fn tolerant_doctor_rejects_a_missing_or_malformed_fingerprint() {
        let (_missing_directory, missing_path) = fresh_index();
        let connection = Connection::open(&missing_path).unwrap();
        connection
            .execute(
                "UPDATE index_meta
                 SET key = 'unexpected_pipeline_key'
                 WHERE key = 'pipeline_fingerprint'",
                [],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            Doctor::run_with_policy(&missing_path, FingerprintPolicy::Tolerate),
            Err(StoreError::InvalidMetadata("pipeline_fingerprint"))
        ));

        let (_malformed_directory, malformed_path) = fresh_index();
        set_metadata_fingerprint(&malformed_path, &[0xaa]);
        assert!(matches!(
            Doctor::run_with_policy(&malformed_path, FingerprintPolicy::Tolerate),
            Err(StoreError::InvalidMetadata("pipeline_fingerprint"))
        ));
    }

    #[test]
    fn tolerant_doctor_requires_every_generation_to_match_the_stored_fingerprint() {
        let (_directory, path) = fresh_index();
        set_metadata_fingerprint(&path, &OLD_FINGERPRINT);
        insert_abandoned_generation(&path, 1, &OLD_FINGERPRINT);

        Doctor::run_with_policy(&path, FingerprintPolicy::Tolerate).unwrap();

        insert_abandoned_generation(&path, 2, &OTHER_FINGERPRINT);
        assert!(matches!(
            Doctor::run_with_policy(&path, FingerprintPolicy::Tolerate),
            Err(StoreError::GenerationInvariant(
                "generation pipeline fingerprint"
            ))
        ));
    }

    fn fresh_index() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("index.sqlite");
        drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
        (directory, path)
    }

    fn set_metadata_fingerprint(path: &Path, fingerprint: &[u8]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "UPDATE index_meta
                 SET value = ?1
                 WHERE key = 'pipeline_fingerprint'",
                params![fingerprint],
            )
            .unwrap();
    }

    fn insert_abandoned_generation(path: &Path, id: i64, fingerprint: &[u8; 32]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO generations(
                    id, state, created_at, pipeline_fingerprint
                 ) VALUES (?1, 'abandoned', '2026-07-26T00:00:00Z', ?2)",
                params![id, fingerprint.as_slice()],
            )
            .unwrap();
    }
}
