use std::fmt;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::{DocumentId, Sha256Digest, SourceId};
#[cfg(unix)]
use crate::ingest::DiscoveryError;
use crate::ingest::{HARD_MAX_FILE_BYTES, HARD_MAX_SOURCE_FILES};
use crate::store::{
    FilesystemAssessment, FilesystemLocality, IndexDb, OpenMode, StorageInspection,
    StoragePreflightError, StoreError,
};

const MAX_STATUS_SOURCE_NAME_BYTES: i64 = 64;
const MAX_STATUS_SOURCE_CONFIG_BYTES: i64 = 16 * 1024;
const MAX_STATUS_ERROR_CODE_BYTES: i64 = 64;
const MAX_STATUS_ERROR_DETAIL_BYTES: i64 = 64 * 1024;
const MAX_STATUS_TIMESTAMP_BYTES: i64 = 128;
const MAX_STATUS_SOURCES: i64 = 64;
const MAX_DRIFT_CONNECTOR_KEY_BYTES: i64 = 4096;
const MAX_DRIFT_TOTAL_CONNECTOR_KEY_BYTES: i64 = 4 * 1024 * 1024;

struct DriftTargetRow {
    source: Vec<u8>,
    document: Vec<u8>,
    connector_key: Vec<u8>,
    source_updated_at: Option<String>,
    body_size: i64,
    body_sha256: Vec<u8>,
}

/// Reads the durable index state and, when requested, observes the live source.
#[derive(Debug)]
pub struct Status;

impl Status {
    /// Reads only SQLite-authoritative state through a validated read-only,
    /// query-only connection.
    pub fn read(index_path: &Path) -> Result<StatusReport, StatusError> {
        let (report, _) = read_database_status(index_path, false)?;
        Ok(report)
    }

    /// Reads the durable status, closes SQLite, and only then probes live files.
    ///
    /// The explicit ordering is a contract: slow or hostile filesystem access
    /// cannot extend a SQLite read transaction or change the cited snapshots.
    pub fn read_with_drift(
        index_path: &Path,
        source_root: &Path,
        options: DriftOptions,
    ) -> Result<StatusWithDrift, StatusError> {
        let (status, targets) = read_database_status(index_path, true)?;
        let drift = run_bounded_probe(source_root.to_path_buf(), targets, options);
        Ok(StatusWithDrift { status, drift })
    }

    /// Builds status from a caller-owned read snapshot.
    ///
    /// Callers must pass a validated read-only connection or transaction. This
    /// exists so transports can combine project metadata and shared health
    /// diagnostics without opening a second, potentially newer snapshot.
    pub(crate) fn read_snapshot(
        connection: &Connection,
        database_read_only: bool,
    ) -> Result<StatusReport, StatusError> {
        read_status_snapshot(connection, database_read_only)
    }

    /// Adds storage capacity and locality diagnostics after SQLite is closed.
    pub(crate) fn attach_storage_status(index_path: &Path, report: &mut StatusReport) {
        attach_storage_status(index_path, report);
    }

    /// Captures the one filesystem target associated with a cited immutable
    /// revision while the caller's SQLite snapshot is still active.
    pub(crate) fn cited_drift_target(
        connection: &Connection,
        source_id: SourceId,
        document_id: DocumentId,
        revision_sha256: Sha256Digest,
    ) -> Result<Option<DriftTarget>, StatusError> {
        let invalid_fields: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM documents AS d
                 JOIN document_versions AS dv ON dv.document_id = d.id
                 JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
                 WHERE d.source_id = ?1
                   AND d.id = ?2
                   AND dv.revision_sha256 = ?3
                   AND (
                       typeof(d.source_id) != 'blob'
                       OR length(d.source_id) != 16
                       OR typeof(d.id) != 'blob'
                       OR length(d.id) != 16
                       OR typeof(d.connector_key) != 'blob'
                       OR length(d.connector_key) NOT BETWEEN 1 AND ?4
                       OR (
                           dv.source_updated_at IS NOT NULL
                           AND (
                               typeof(dv.source_updated_at) != 'text'
                               OR length(CAST(dv.source_updated_at AS BLOB)) > ?5
                           )
                       )
                       OR typeof(cb.original_bytes) != 'blob'
                       OR typeof(cb.body_sha256) != 'blob'
                       OR length(cb.body_sha256) != 32
                   )
                 LIMIT 1
             )",
            params![
                source_id.as_uuid().as_bytes().as_slice(),
                document_id.as_uuid().as_bytes().as_slice(),
                revision_sha256.as_bytes().as_slice(),
                MAX_DRIFT_CONNECTOR_KEY_BYTES,
                MAX_STATUS_TIMESTAMP_BYTES,
            ],
            |row| row.get(0),
        )?;
        if invalid_fields {
            return Err(StatusError::Invalid("bounded drift target fields"));
        }

        let row = connection
            .query_row(
                "SELECT d.source_id, d.id, d.connector_key,
                        dv.source_updated_at, length(cb.original_bytes), cb.body_sha256
                 FROM documents AS d
                 JOIN document_versions AS dv ON dv.document_id = d.id
                 JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
                 WHERE d.source_id = ?1 AND d.id = ?2 AND dv.revision_sha256 = ?3",
                params![
                    source_id.as_uuid().as_bytes().as_slice(),
                    document_id.as_uuid().as_bytes().as_slice(),
                    revision_sha256.as_bytes().as_slice(),
                ],
                |row| {
                    Ok(DriftTargetRow {
                        source: row.get(0)?,
                        document: row.get(1)?,
                        connector_key: row.get(2)?,
                        source_updated_at: row.get(3)?,
                        body_size: row.get(4)?,
                        body_sha256: row.get(5)?,
                    })
                },
            )
            .optional()?;
        row.map(drift_target_from_row).transpose()
    }

    /// Probes exactly one target through the global bounded worker.
    pub(crate) fn probe_cited_target(
        source_root: &Path,
        target: DriftTarget,
        options: DriftOptions,
    ) -> DocumentDrift {
        let mut report = run_bounded_probe(source_root.to_path_buf(), vec![target], options);
        report
            .observations
            .pop()
            .expect("one submitted target always produces one observation")
    }

    /// Produces the explicit freshness state for a connector whose configured
    /// snapshot is authoritative but whose records have no live file probe.
    pub(crate) fn snapshot_only_target(target: DriftTarget) -> DocumentDrift {
        observation(&target, DriftState::SnapshotOnly, None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StatusReport {
    pub index_epoch: u64,
    pub active_generation: Option<i64>,
    pub active_documents: usize,
    pub active_passages: usize,
    pub database_read_only: bool,
    pub query_only: bool,
    pub index_quota_bytes: Option<u64>,
    pub storage: Option<StorageStatus>,
    pub sources: Vec<SourceStatus>,
    pub problems: Vec<StatusProblem>,
}

impl StatusReport {
    pub fn actionable_problems(&self) -> &[StatusProblem] {
        &self.problems
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StorageStatus {
    pub managed_index_bytes: u64,
    pub reclaimable_bytes: u64,
    pub available_bytes: u64,
    pub reserve_bytes: u64,
    pub quota_bytes: Option<u64>,
    pub filesystem: FilesystemAssessment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceStatus {
    pub source_id: SourceId,
    pub name: SafeDisplayText,
    pub state: SourceSyncState,
    pub last_success_at: Option<String>,
    pub last_error_code: Option<SafeDisplayText>,
    pub last_error_detail: Option<SafeDisplayText>,
    pub last_error_at: Option<String>,
    pub active_documents: usize,
    pub active_passages: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceSyncState {
    NeverSucceeded,
    Healthy,
    Partial,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StatusProblem {
    pub code: &'static str,
    pub summary: &'static str,
    pub repair_command: &'static str,
}

/// Text that is safe to write to a terminal without interpreting source bytes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SafeDisplayText {
    human: String,
    machine: String,
}

impl SafeDisplayText {
    pub fn from_text(value: &str) -> Self {
        Self {
            human: escape_terminal_text(value),
            machine: value.to_owned(),
        }
    }

    pub fn from_bytes(value: &[u8]) -> Self {
        Self {
            human: escape_terminal_bytes(value),
            machine: std::str::from_utf8(value)
                .map(str::to_owned)
                .unwrap_or_else(|_| format!("hex:{}", hex::encode(value))),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.human
    }
}

impl fmt::Display for SafeDisplayText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.human)
    }
}

impl Serialize for SafeDisplayText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.machine)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriftOptions {
    pub verify_content_hash: bool,
    pub deadline: Duration,
}

impl Default for DriftOptions {
    fn default() -> Self {
        Self {
            verify_content_hash: false,
            deadline: Duration::from_millis(500),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StatusWithDrift {
    pub status: StatusReport,
    pub drift: DriftReport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DriftReport {
    pub observations: Vec<DocumentDrift>,
    pub deadline_reached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DocumentDrift {
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub connector: SafeDisplayText,
    pub state: DriftState,
    /// `None` means no content read was requested or content could not be read.
    pub content_matches: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftState {
    MetadataUnchanged,
    MetadataChanged,
    SnapshotOnly,
    Missing,
    Blocked,
    Unknown,
}

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("unable to read validated index status")]
    Store(#[source] StoreError),
    #[error("SQLite status query failed")]
    Sqlite(#[source] rusqlite::Error),
    #[error("index status contains invalid {0}")]
    Invalid(&'static str),
}

impl From<StoreError> for StatusError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for StatusError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DriftTarget {
    source_id: SourceId,
    document_id: DocumentId,
    connector_key: Vec<u8>,
    source_updated_at: Option<String>,
    body_size: u64,
    body_sha256: Sha256Digest,
}

struct DriftJob {
    root: PathBuf,
    targets: Arc<[DriftTarget]>,
    verify_content_hash: bool,
    deadline: Instant,
    response: mpsc::Sender<DriftReport>,
}

static DRIFT_WORKER: OnceLock<Option<SyncSender<DriftJob>>> = OnceLock::new();
#[cfg(test)]
static TEST_DRIFT_DELAY_MS: AtomicU64 = AtomicU64::new(0);

fn run_bounded_probe(
    root: PathBuf,
    targets: Vec<DriftTarget>,
    options: DriftOptions,
) -> DriftReport {
    if targets.is_empty() {
        return DriftReport {
            observations: Vec::new(),
            deadline_reached: false,
        };
    }
    let targets = Arc::<[DriftTarget]>::from(targets);
    let Some(deadline) = Instant::now().checked_add(options.deadline) else {
        return unknown_drift_report(&targets);
    };
    if options.deadline.is_zero() {
        return unknown_drift_report(&targets);
    }

    let Some(worker) = drift_worker() else {
        return unknown_drift_report(&targets);
    };
    let (response, receiver) = mpsc::channel();
    let job = DriftJob {
        root,
        targets: Arc::clone(&targets),
        verify_content_hash: options.verify_content_hash,
        deadline,
        response,
    };
    match worker.try_send(job) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            return unknown_drift_report(&targets);
        }
    }

    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(report) => report,
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
            unknown_drift_report(&targets)
        }
    }
}

fn drift_worker() -> Option<&'static SyncSender<DriftJob>> {
    DRIFT_WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<DriftJob>(1);
            thread::Builder::new()
                .name("hsum-drift-probe".to_owned())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        #[cfg(test)]
                        {
                            let delay = TEST_DRIFT_DELAY_MS.load(Ordering::SeqCst);
                            if delay != 0 {
                                thread::sleep(Duration::from_millis(delay));
                            }
                        }
                        let report = if Instant::now() >= job.deadline {
                            unknown_drift_report(&job.targets)
                        } else {
                            probe_targets_until(
                                &job.root,
                                &job.targets,
                                job.verify_content_hash,
                                job.deadline,
                            )
                        };
                        let _ = job.response.send(report);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

fn unknown_drift_report(targets: &[DriftTarget]) -> DriftReport {
    DriftReport {
        observations: targets
            .iter()
            .map(|target| observation(target, DriftState::Unknown, None))
            .collect(),
        deadline_reached: true,
    }
}

fn read_database_status(
    index_path: &Path,
    load_targets: bool,
) -> Result<(StatusReport, Vec<DriftTarget>), StatusError> {
    let database = IndexDb::open_existing(index_path, OpenMode::ReadOnly)?;
    let connection = database.connection();
    let database_read_only = database.is_read_only()?;
    let query_only: bool = connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
    if !database_read_only || !query_only {
        return Err(StatusError::Invalid("read-only connection state"));
    }
    let transaction = connection.unchecked_transaction()?;
    let mut report = read_status_snapshot(&transaction, database_read_only)?;
    let targets = if load_targets {
        read_drift_targets(&transaction)?
    } else {
        Vec::new()
    };
    transaction.rollback()?;

    // Keep this explicit. The filesystem probe must never run while this
    // connection (and any SQLite read state associated with it) is alive.
    drop(database);
    attach_storage_status(index_path, &mut report);
    Ok((report, targets))
}

fn read_status_snapshot(
    connection: &Connection,
    database_read_only: bool,
) -> Result<StatusReport, StatusError> {
    let query_only: bool = connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
    if !database_read_only || !query_only {
        return Err(StatusError::Invalid("read-only connection state"));
    }
    let index_epoch = metadata_u64(connection, "index_epoch")?;
    let active_generation = metadata_optional_i64(connection, "active_generation")?;
    let active_documents = count_usize(
        connection.query_row(
            "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
            [],
            |row| row.get(0),
        )?,
        "active document count",
    )?;
    let active_passages = count_usize(
        connection.query_row("SELECT COUNT(*) FROM active_passages", [], |row| row.get(0))?,
        "active passage count",
    )?;
    if let Some(generation_id) = active_generation {
        let committed: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM generations
                WHERE id = ?1 AND state = 'committed'
            )",
            [generation_id],
            |row| row.get(0),
        )?;
        if !committed {
            return Err(StatusError::Invalid("active generation"));
        }
    }
    let sources = read_sources(connection)?;
    let index_quota_bytes = read_index_quota(connection)?;
    let unfinished_generations: i64 = connection.query_row(
        "SELECT COUNT(*) FROM generations WHERE state = 'building'",
        [],
        |row| row.get(0),
    )?;
    let problems = build_problems(
        &sources,
        active_generation,
        active_documents,
        active_passages,
        unfinished_generations,
    );
    Ok(StatusReport {
        index_epoch,
        active_generation,
        active_documents,
        active_passages,
        database_read_only,
        query_only,
        index_quota_bytes,
        storage: None,
        sources,
        problems,
    })
}

fn read_index_quota(connection: &Connection) -> Result<Option<u64>, StatusError> {
    let mut statement = connection.prepare(
        "SELECT config_json
         FROM sources
         WHERE removed_at IS NULL
         ORDER BY id
         LIMIT ?1",
    )?;
    let configs = statement
        .query_map([MAX_STATUS_SOURCES], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut shared_quota = None;
    for (index, config) in configs.iter().enumerate() {
        let value: serde_json::Value = serde_json::from_str(config)
            .map_err(|_| StatusError::Invalid("source configuration"))?;
        let object = value
            .as_object()
            .ok_or(StatusError::Invalid("source configuration"))?;
        let quota = match object.get("index_quota_bytes") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|quota| *quota != 0)
                    .ok_or(StatusError::Invalid("index quota"))?,
            ),
        };
        if index == 0 {
            shared_quota = quota;
        } else if shared_quota != quota {
            return Err(StatusError::Invalid("inconsistent index quota"));
        }
    }
    Ok(shared_quota)
}

fn attach_storage_status(index_path: &Path, report: &mut StatusReport) {
    let inspection = match StorageInspection::run(index_path, report.index_quota_bytes) {
        Ok(inspection) => inspection,
        Err(StoragePreflightError::UnsupportedNetworkFilesystem { .. }) => {
            report.problems.push(StatusProblem {
                code: "UNSUPPORTED_NETWORK_STORAGE",
                summary: "The managed index is on an unsupported network filesystem.",
                repair_command: "hsum init --data-dir <local-path>",
            });
            return;
        }
        Err(_) => {
            report.problems.push(StatusProblem {
                code: "STORAGE_INSPECTION_UNAVAILABLE",
                summary: "Managed index capacity and filesystem locality could not be inspected.",
                repair_command: "hsum doctor",
            });
            return;
        }
    };

    if inspection.filesystem.locality == FilesystemLocality::Unknown {
        report.problems.push(StatusProblem {
            code: "STORAGE_LOCALITY_UNKNOWN",
            summary: "The managed index filesystem could not be proven local.",
            repair_command: "hsum doctor",
        });
    }
    if inspection.filesystem.sync_root.is_some() {
        report.problems.push(StatusProblem {
            code: "UNSUPPORTED_SYNC_STORAGE",
            summary: "The managed index is inside a recognized consumer-sync root.",
            repair_command: "hsum init --data-dir <local-path>",
        });
    }
    if inspection.available_bytes < inspection.reserve_bytes {
        report.problems.push(StatusProblem {
            code: "LOW_STORAGE_RESERVE",
            summary: "Available capacity is below the required recovery reserve.",
            repair_command: "free disk space, then run hsum doctor",
        });
    }
    if inspection.quota_bytes.is_some_and(|quota| {
        inspection
            .managed_index_bytes
            .checked_add(inspection.reserve_bytes)
            .is_none_or(|required| required > quota)
    }) {
        report.problems.push(StatusProblem {
            code: "INDEX_QUOTA_EXHAUSTED",
            summary: "Managed bytes plus the recovery reserve exceed the configured quota.",
            repair_command: "raise the explicit quota or free retained data",
        });
    }
    report.storage = Some(StorageStatus {
        managed_index_bytes: inspection.managed_index_bytes,
        reclaimable_bytes: 0,
        available_bytes: inspection.available_bytes,
        reserve_bytes: inspection.reserve_bytes,
        quota_bytes: inspection.quota_bytes,
        filesystem: inspection.filesystem,
    });
}

fn read_sources(connection: &Connection) -> Result<Vec<SourceStatus>, StatusError> {
    validate_status_source_bounds(connection)?;
    let mut statement = connection.prepare(
        "SELECT s.id, s.name, s.last_success_at,
                s.last_error_code, s.last_error_detail, s.last_error_at,
                (
                    SELECT COUNT(*)
                    FROM documents AS d
                    JOIN document_heads AS dh ON dh.document_id = d.id
                    WHERE d.source_id = s.id AND dh.state = 'active'
                ),
                (
                    SELECT COUNT(*)
                    FROM active_passages AS ap
                    WHERE ap.source_id = s.id
                )
         FROM sources AS s
         WHERE s.removed_at IS NULL
         ORDER BY s.name, s.id
         LIMIT ?1",
    )?;
    let rows = statement.query_map([MAX_STATUS_SOURCES], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;

    rows.map(|row| {
        let (
            id,
            name,
            last_success_at,
            last_error_code,
            last_error_detail,
            last_error_at,
            active_documents,
            active_passages,
        ) = row?;
        let state = match (last_success_at.is_some(), last_error_code.is_some()) {
            (false, false) => SourceSyncState::NeverSucceeded,
            (false, true) => SourceSyncState::Failed,
            (true, false) => SourceSyncState::Healthy,
            (true, true) => SourceSyncState::Partial,
        };
        Ok(SourceStatus {
            source_id: source_id(&id)?,
            name: SafeDisplayText::from_text(&name),
            state,
            last_success_at,
            last_error_code: last_error_code.as_deref().map(SafeDisplayText::from_text),
            last_error_detail: last_error_detail.as_deref().map(SafeDisplayText::from_text),
            last_error_at,
            active_documents: count_usize(active_documents, "source active document count")?,
            active_passages: count_usize(active_passages, "source active passage count")?,
        })
    })
    .collect()
}

fn validate_status_source_bounds(connection: &Connection) -> Result<(), StatusError> {
    let too_many_sources: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM sources
             LIMIT 1 OFFSET ?1
         )",
        [MAX_STATUS_SOURCES],
        |row| row.get(0),
    )?;
    if too_many_sources {
        return Err(StatusError::Invalid("source cardinality"));
    }

    let invalid_fields: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM sources
             WHERE length(id) != 16
                OR length(CAST(name AS BLOB)) NOT BETWEEN 1 AND ?1
                OR length(CAST(config_json AS BLOB)) > ?2
                OR (
                    last_success_at IS NOT NULL
                    AND length(CAST(last_success_at AS BLOB)) > ?3
                )
                OR (
                    last_error_code IS NOT NULL
                    AND length(CAST(last_error_code AS BLOB)) > ?4
                )
                OR (
                    last_error_detail IS NOT NULL
                    AND length(CAST(last_error_detail AS BLOB)) > ?5
                )
                OR (
                    last_error_at IS NOT NULL
                    AND length(CAST(last_error_at AS BLOB)) > ?3
                )
         )",
        params![
            MAX_STATUS_SOURCE_NAME_BYTES,
            MAX_STATUS_SOURCE_CONFIG_BYTES,
            MAX_STATUS_TIMESTAMP_BYTES,
            MAX_STATUS_ERROR_CODE_BYTES,
            MAX_STATUS_ERROR_DETAIL_BYTES,
        ],
        |row| row.get(0),
    )?;
    if invalid_fields {
        return Err(StatusError::Invalid("bounded source status fields"));
    }
    Ok(())
}

fn build_problems(
    sources: &[SourceStatus],
    active_generation: Option<i64>,
    active_documents: usize,
    active_passages: usize,
    unfinished_generations: i64,
) -> Vec<StatusProblem> {
    let mut problems = Vec::new();
    if sources.is_empty() {
        problems.push(StatusProblem {
            code: "NO_SOURCE",
            summary: "No source has been configured.",
            repair_command: "hsum init",
        });
    }
    if active_generation.is_none() {
        problems.push(StatusProblem {
            code: "NO_ACTIVE_GENERATION",
            summary: "No indexed generation is active.",
            repair_command: "hsum ingest",
        });
    }
    if active_generation.is_some() && active_documents == 0 {
        problems.push(StatusProblem {
            code: "NO_ACTIVE_DOCUMENTS",
            summary: "The active generation contains no documents.",
            repair_command: "hsum ingest",
        });
    }
    if active_documents != 0 && active_passages == 0 {
        problems.push(StatusProblem {
            code: "NO_SEARCHABLE_PASSAGES",
            summary: "Active documents contain no searchable passages.",
            repair_command: "hsum doctor",
        });
    }
    if unfinished_generations != 0 {
        problems.push(StatusProblem {
            code: "INCOMPLETE_GENERATION",
            summary: "An unfinished generation needs diagnosis.",
            repair_command: "hsum doctor",
        });
    }
    if sources.iter().any(|source| {
        matches!(
            source.state,
            SourceSyncState::Partial | SourceSyncState::Failed
        )
    }) {
        problems.push(StatusProblem {
            code: "SOURCE_SYNC_PARTIAL",
            summary: "At least one source retained prior evidence after a sync error.",
            repair_command: "hsum ingest",
        });
    }
    problems
}

fn read_drift_targets(connection: &Connection) -> Result<Vec<DriftTarget>, StatusError> {
    read_drift_targets_bounded(
        connection,
        HARD_MAX_SOURCE_FILES,
        usize::try_from(MAX_DRIFT_TOTAL_CONNECTOR_KEY_BYTES)
            .expect("drift connector budget fits usize"),
    )
}

fn read_drift_targets_bounded(
    connection: &Connection,
    max_targets: usize,
    max_connector_bytes: usize,
) -> Result<Vec<DriftTarget>, StatusError> {
    let max_targets =
        i64::try_from(max_targets).map_err(|_| StatusError::Invalid("drift target limit"))?;
    let max_connector_bytes = i64::try_from(max_connector_bytes)
        .map_err(|_| StatusError::Invalid("drift connector budget"))?;
    let exceeds_cardinality: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM document_heads AS dh
             JOIN documents AS d ON d.id = dh.document_id
             JOIN sources AS s ON s.id = d.source_id
             JOIN document_versions AS dv ON dv.id = dh.document_version_id
             JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
             WHERE dh.state = 'active' AND s.kind = 'filesystem'
             LIMIT 1 OFFSET ?1
         )",
        [max_targets],
        |row| row.get(0),
    )?;
    if exceeds_cardinality {
        return Err(StatusError::Invalid("drift target cardinality"));
    }

    let invalid_fields: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM document_heads AS dh
             JOIN documents AS d ON d.id = dh.document_id
             JOIN document_versions AS dv ON dv.id = dh.document_version_id
             JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
             WHERE dh.state = 'active'
               AND (
                   typeof(d.source_id) != 'blob'
                   OR length(d.source_id) != 16
                   OR typeof(d.id) != 'blob'
                   OR length(d.id) != 16
                   OR typeof(d.connector_key) != 'blob'
                   OR length(d.connector_key) NOT BETWEEN 1 AND ?1
                   OR (
                       dv.source_updated_at IS NOT NULL
                       AND (
                           typeof(dv.source_updated_at) != 'text'
                           OR length(CAST(dv.source_updated_at AS BLOB)) > ?2
                       )
                   )
                   OR typeof(cb.original_bytes) != 'blob'
                   OR typeof(cb.body_sha256) != 'blob'
                   OR length(cb.body_sha256) != 32
               )
             LIMIT 1
         )",
        params![MAX_DRIFT_CONNECTOR_KEY_BYTES, MAX_STATUS_TIMESTAMP_BYTES],
        |row| row.get(0),
    )?;
    if invalid_fields {
        return Err(StatusError::Invalid("bounded drift target fields"));
    }

    // The preceding cardinality and per-row checks prove this SUM cannot
    // overflow SQLite's signed integer: at most HARD_MAX_SOURCE_FILES rows
    // contribute at most MAX_DRIFT_CONNECTOR_KEY_BYTES bytes each.
    let exceeds_connector_budget: bool = connection.query_row(
        "SELECT COALESCE(SUM(length(d.connector_key)), 0) > ?1
         FROM document_heads AS dh
         JOIN documents AS d ON d.id = dh.document_id
         JOIN sources AS s ON s.id = d.source_id
         JOIN document_versions AS dv ON dv.id = dh.document_version_id
         JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
         WHERE dh.state = 'active' AND s.kind = 'filesystem'",
        [max_connector_bytes],
        |row| row.get(0),
    )?;
    if exceeds_connector_budget {
        return Err(StatusError::Invalid("drift connector byte budget"));
    }

    let mut statement = connection.prepare(
        "SELECT d.source_id, d.id, d.connector_key,
                dv.source_updated_at, length(cb.original_bytes), cb.body_sha256
         FROM document_heads AS dh
         JOIN documents AS d ON d.id = dh.document_id
         JOIN sources AS s ON s.id = d.source_id
         JOIN document_versions AS dv ON dv.id = dh.document_version_id
         JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
         WHERE dh.state = 'active' AND s.kind = 'filesystem'
         ORDER BY d.source_id, d.connector_key, d.id
         LIMIT ?1",
    )?;
    let rows = statement.query_map([max_targets], |row| {
        Ok(DriftTargetRow {
            source: row.get(0)?,
            document: row.get(1)?,
            connector_key: row.get(2)?,
            source_updated_at: row.get(3)?,
            body_size: row.get(4)?,
            body_sha256: row.get(5)?,
        })
    })?;
    let initial_capacity = usize::try_from(max_targets).unwrap_or_default().min(1024);
    let mut targets = Vec::with_capacity(initial_capacity);
    for row in rows {
        targets.push(drift_target_from_row(row?)?);
    }
    Ok(targets)
}

fn drift_target_from_row(row: DriftTargetRow) -> Result<DriftTarget, StatusError> {
    Ok(DriftTarget {
        source_id: source_id(&row.source)?,
        document_id: document_id(&row.document)?,
        connector_key: row.connector_key,
        source_updated_at: row.source_updated_at,
        body_size: u64::try_from(row.body_size)
            .map_err(|_| StatusError::Invalid("stored body size"))?,
        body_sha256: digest(&row.body_sha256)?,
    })
}

fn probe_targets_until(
    root: &Path,
    targets: &[DriftTarget],
    verify_content_hash: bool,
    deadline: Instant,
) -> DriftReport {
    let mut deadline_reached = false;
    let mut observations = Vec::with_capacity(targets.len());

    for target in targets {
        if Instant::now() >= deadline {
            deadline_reached = true;
            observations.push(observation(target, DriftState::Unknown, None));
            continue;
        }
        observations.push(probe_target(root, target, verify_content_hash));
    }

    DriftReport {
        observations,
        deadline_reached,
    }
}

#[cfg(unix)]
fn probe_target(root: &Path, target: &DriftTarget, verify_content_hash: bool) -> DocumentDrift {
    use rustix::fs::{FileType, fstat};
    use std::fs::File;
    use std::os::fd::AsFd;

    let descriptor = match open_beneath(root, &target.connector_key) {
        Ok(descriptor) => descriptor,
        Err(state) => return observation(target, state, None),
    };
    let stat = match fstat(&descriptor) {
        Ok(stat) => stat,
        Err(_) => return observation(target, DriftState::Unknown, None),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return observation(target, DriftState::Blocked, None);
    }

    let size_matches = u64::try_from(stat.st_size).ok() == Some(target.body_size);
    let timestamp_matches = target
        .source_updated_at
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|(seconds, nanoseconds)| {
            stat.st_mtime == seconds && u32::try_from(stat.st_mtime_nsec).ok() == Some(nanoseconds)
        });
    let mut state = if size_matches && timestamp_matches {
        DriftState::MetadataUnchanged
    } else if target.source_updated_at.is_some() {
        DriftState::MetadataChanged
    } else {
        DriftState::Unknown
    };

    if !verify_content_hash {
        return observation(target, state, None);
    }
    if !size_matches {
        return observation(target, DriftState::MetadataChanged, Some(false));
    }
    if target.body_size > HARD_MAX_FILE_BYTES {
        return observation(target, DriftState::Unknown, None);
    }

    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(usize::try_from(target.body_size).unwrap_or_default());
    if (&mut file)
        .take(HARD_MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 != target.body_size
    {
        return observation(target, DriftState::Unknown, None);
    }
    let after = match fstat(file.as_fd()) {
        Ok(after) => after,
        Err(_) => return observation(target, DriftState::Unknown, None),
    };
    if after.st_dev != stat.st_dev
        || after.st_ino != stat.st_ino
        || after.st_size != stat.st_size
        || after.st_mtime != stat.st_mtime
        || after.st_mtime_nsec != stat.st_mtime_nsec
    {
        return observation(target, DriftState::Unknown, None);
    }
    let content_matches = Sha256Digest::of_bytes(&bytes) == target.body_sha256;
    if !content_matches {
        state = DriftState::MetadataChanged;
    }
    observation(target, state, Some(content_matches))
}

#[cfg(not(unix))]
fn probe_target(_root: &Path, target: &DriftTarget, _verify_content_hash: bool) -> DocumentDrift {
    observation(target, DriftState::Unknown, None)
}

fn observation(
    target: &DriftTarget,
    state: DriftState,
    content_matches: Option<bool>,
) -> DocumentDrift {
    DocumentDrift {
        source_id: target.source_id,
        document_id: target.document_id,
        connector: SafeDisplayText::from_bytes(&target.connector_key),
        state,
        content_matches,
    }
}

#[cfg(unix)]
fn open_beneath(root: &Path, connector_key: &[u8]) -> Result<std::os::fd::OwnedFd, DriftState> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat, statat};
    use std::ffi::CString;

    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const FILE_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    if connector_key.is_empty() || connector_key.starts_with(b"/") {
        return Err(DriftState::Unknown);
    }
    let components = connector_key
        .split(|byte| *byte == b'/')
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, b"." | b".."))
    {
        return Err(DriftState::Unknown);
    }

    let mut directory = crate::ingest::open_source_root(root).map_err(map_source_root_error)?;
    for component in &components[..components.len() - 1] {
        let name = CString::new(*component).map_err(|_| DriftState::Unknown)?;
        let before = statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_errno)?;
        if FileType::from_raw_mode(before.st_mode) == FileType::Symlink {
            return Err(DriftState::Blocked);
        }
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(DriftState::Missing);
        }
        let child = openat(&directory, &name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_errno)?;
        let opened = fstat(&child).map_err(|_| DriftState::Unknown)?;
        if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
            return Err(DriftState::Unknown);
        }
        directory = child;
    }

    let name = CString::new(components[components.len() - 1]).map_err(|_| DriftState::Unknown)?;
    let before = statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_errno)?;
    if FileType::from_raw_mode(before.st_mode) == FileType::Symlink {
        return Err(DriftState::Blocked);
    }
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(DriftState::Blocked);
    }
    let file = openat(&directory, &name, FILE_FLAGS, Mode::empty()).map_err(map_errno)?;
    let opened = fstat(&file).map_err(|_| DriftState::Unknown)?;
    if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
        return Err(DriftState::Unknown);
    }
    Ok(file)
}

#[cfg(unix)]
fn map_source_root_error(error: DiscoveryError) -> DriftState {
    match error {
        DiscoveryError::RootIsSymlink { .. } => DriftState::Blocked,
        DiscoveryError::RootMissing { .. } | DiscoveryError::RootNotDirectory { .. } => {
            DriftState::Missing
        }
        DiscoveryError::RootOpen { .. }
        | DiscoveryError::DirectoryChanged { .. }
        | DiscoveryError::DirectoryUnreadable { .. }
        | DiscoveryError::InvalidIgnoreRule { .. }
        | DiscoveryError::InvalidIgnoreFile { .. }
        | DiscoveryError::InvalidPattern { .. }
        | DiscoveryError::SourceLimitExceeded { .. }
        | DiscoveryError::TraversalLimitExceeded { .. }
        | DiscoveryError::StagingEstimateExceeded { .. }
        | DiscoveryError::Staging { .. }
        | DiscoveryError::UnsupportedPlatform => DriftState::Unknown,
    }
}

#[cfg(unix)]
fn map_errno(error: rustix::io::Errno) -> DriftState {
    match error {
        rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => DriftState::Missing,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM | rustix::io::Errno::LOOP => {
            DriftState::Blocked
        }
        _ => DriftState::Unknown,
    }
}

fn parse_timestamp(value: &str) -> Option<(i64, u32)> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    Some((timestamp.unix_timestamp(), timestamp.nanosecond()))
}

fn metadata_u64(connection: &Connection, key: &'static str) -> Result<u64, StatusError> {
    let value: Vec<u8> = connection.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    std::str::from_utf8(&value)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(StatusError::Invalid(key))
}

fn metadata_optional_i64(
    connection: &Connection,
    key: &'static str,
) -> Result<Option<i64>, StatusError> {
    let value: Vec<u8> = connection.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    if value.is_empty() {
        return Ok(None);
    }
    std::str::from_utf8(&value)
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Some)
        .ok_or(StatusError::Invalid(key))
}

fn count_usize(value: i64, field: &'static str) -> Result<usize, StatusError> {
    usize::try_from(value).map_err(|_| StatusError::Invalid(field))
}

fn source_id(value: &[u8]) -> Result<SourceId, StatusError> {
    Ok(SourceId::from_uuid(
        Uuid::from_slice(value).map_err(|_| StatusError::Invalid("source UUID"))?,
    ))
}

fn document_id(value: &[u8]) -> Result<DocumentId, StatusError> {
    Ok(DocumentId::from_uuid(
        Uuid::from_slice(value).map_err(|_| StatusError::Invalid("document UUID"))?,
    ))
}

fn digest(value: &[u8]) -> Result<Sha256Digest, StatusError> {
    let bytes = value
        .try_into()
        .map_err(|_| StatusError::Invalid("body digest"))?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn escape_terminal_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, ' '..='~') {
            escaped.push(character);
        } else if character == '\n' {
            escaped.push_str("\\n");
        } else if character == '\r' {
            escaped.push_str("\\r");
        } else if character == '\t' {
            escaped.push_str("\\t");
        } else if character.is_ascii() {
            push_hex_byte(&mut escaped, character as u8);
        } else {
            use std::fmt::Write as _;
            let _ = write!(escaped, "\\u{{{:X}}}", u32::from(character));
        }
    }
    escaped
}

fn escape_terminal_bytes(value: &[u8]) -> String {
    let mut escaped = String::with_capacity(value.len());
    for &byte in value {
        if matches!(byte, b' '..=b'~') {
            escaped.push(char::from(byte));
        } else if byte == b'\n' {
            escaped.push_str("\\n");
        } else if byte == b'\r' {
            escaped.push_str("\\r");
        } else if byte == b'\t' {
            escaped.push_str("\\t");
        } else {
            push_hex_byte(&mut escaped, byte);
        }
    }
    escaped
}

fn push_hex_byte(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push_str("\\x");
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_bounds_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sources (
                     id BLOB NOT NULL,
                     name TEXT NOT NULL,
                     config_json TEXT NOT NULL,
                     last_success_at TEXT,
                     last_error_code TEXT,
                     last_error_detail TEXT,
                     last_error_at TEXT
                 ) STRICT;",
            )
            .unwrap();
        connection
    }

    fn insert_status_source(
        connection: &Connection,
        id_byte: u8,
        config_json: &str,
        error_detail: Option<&str>,
    ) {
        connection
            .execute(
                "INSERT INTO sources (
                     id, name, config_json, last_success_at,
                     last_error_code, last_error_detail, last_error_at
                 )
                 VALUES (?1, 'workspace', ?2, NULL, ?3, ?4, ?5)",
                params![
                    vec![id_byte; 16],
                    config_json,
                    error_detail.map(|_| "SOURCE_IO"),
                    error_detail,
                    error_detail.map(|_| "2026-07-20T00:00:00Z"),
                ],
            )
            .unwrap();
    }

    #[test]
    fn source_status_accepts_multiple_sources_and_rejects_cap_plus_one() {
        let connection = source_bounds_connection();
        for id in 0..MAX_STATUS_SOURCES {
            insert_status_source(&connection, u8::try_from(id).unwrap(), "{}", None);
        }
        assert!(validate_status_source_bounds(&connection).is_ok());

        insert_status_source(
            &connection,
            u8::try_from(MAX_STATUS_SOURCES).unwrap(),
            "{}",
            None,
        );

        assert!(matches!(
            validate_status_source_bounds(&connection),
            Err(StatusError::Invalid("source cardinality"))
        ));
    }

    #[test]
    fn source_status_rejects_oversized_materialized_fields() {
        let connection = source_bounds_connection();
        let oversized_detail =
            "x".repeat(usize::try_from(MAX_STATUS_ERROR_DETAIL_BYTES).unwrap() + 1);
        insert_status_source(&connection, 1, "{}", Some(&oversized_detail));

        assert!(matches!(
            validate_status_source_bounds(&connection),
            Err(StatusError::Invalid("bounded source status fields"))
        ));

        connection.execute("DELETE FROM sources", []).unwrap();
        let oversized_config =
            "x".repeat(usize::try_from(MAX_STATUS_SOURCE_CONFIG_BYTES).unwrap() + 1);
        insert_status_source(&connection, 1, &oversized_config, None);
        assert!(matches!(
            validate_status_source_bounds(&connection),
            Err(StatusError::Invalid("bounded source status fields"))
        ));
    }

    #[derive(Clone, Copy)]
    struct DriftFixtureIdentity {
        source_id: SourceId,
        document_id: DocumentId,
        revision_sha256: Sha256Digest,
    }

    fn drift_bounds_connection(rows: usize) -> (Connection, DriftFixtureIdentity) {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE documents (
                     id,
                     source_id,
                     connector_key
                 );
                 CREATE TABLE document_versions (
                     id INTEGER,
                     document_id,
                     content_blob_id INTEGER,
                     revision_sha256,
                     source_updated_at
                 );
                 CREATE TABLE content_blobs (
                     id INTEGER,
                     original_bytes,
                     body_sha256
                 );
                 CREATE TABLE document_heads (
                     document_id,
                     document_version_id INTEGER,
                     state
                 );
                 CREATE TABLE sources (
                     id,
                     kind
                 );",
            )
            .unwrap();

        let fixture_source = SourceId::from_uuid(Uuid::from_u128(1));
        connection
            .execute(
                "INSERT INTO sources(id, kind) VALUES (?1, 'filesystem')",
                [fixture_source.as_uuid().as_bytes().as_slice()],
            )
            .unwrap();

        let mut first = None;
        for ordinal in 0..rows {
            let source_id = fixture_source;
            let document_id =
                DocumentId::from_uuid(Uuid::from_u128(u128::try_from(ordinal).unwrap() + 2));
            let revision_sha256 = Sha256Digest::of_bytes(format!("revision-{ordinal}").as_bytes());
            let original_bytes = format!("body-{ordinal}").into_bytes();
            let body_sha256 = Sha256Digest::of_bytes(&original_bytes);
            let row_id = i64::try_from(ordinal).unwrap() + 1;
            connection
                .execute(
                    "INSERT INTO documents (id, source_id, connector_key)
                     VALUES (?1, ?2, ?3)",
                    params![
                        document_id.as_uuid().as_bytes().as_slice(),
                        source_id.as_uuid().as_bytes().as_slice(),
                        format!("file-{ordinal}.md").into_bytes(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO content_blobs (id, original_bytes, body_sha256)
                     VALUES (?1, ?2, ?3)",
                    params![row_id, original_bytes, body_sha256.as_bytes().as_slice()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO document_versions (
                         id, document_id, content_blob_id, revision_sha256, source_updated_at
                     )
                     VALUES (?1, ?2, ?1, ?3, '2026-07-20T00:00:00Z')",
                    params![
                        row_id,
                        document_id.as_uuid().as_bytes().as_slice(),
                        revision_sha256.as_bytes().as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO document_heads (document_id, document_version_id, state)
                     VALUES (?1, ?2, 'active')",
                    params![document_id.as_uuid().as_bytes().as_slice(), row_id],
                )
                .unwrap();
            first.get_or_insert(DriftFixtureIdentity {
                source_id,
                document_id,
                revision_sha256,
            });
        }

        (
            connection,
            first.unwrap_or(DriftFixtureIdentity {
                source_id: SourceId::new_v4(),
                document_id: DocumentId::new_v4(),
                revision_sha256: Sha256Digest::of_bytes(b"absent"),
            }),
        )
    }

    #[test]
    fn drift_targets_reject_cap_plus_one_before_loading_rows() {
        let (connection, _) = drift_bounds_connection(2);

        assert!(matches!(
            read_drift_targets_bounded(&connection, 1, 2 * 4096),
            Err(StatusError::Invalid("drift target cardinality"))
        ));
    }

    #[test]
    fn drift_targets_reject_an_aggregate_connector_budget_overrun() {
        let (connection, _) = drift_bounds_connection(2);

        assert!(matches!(
            read_drift_targets_bounded(&connection, 2, 17),
            Err(StatusError::Invalid("drift connector byte budget"))
        ));
    }

    #[test]
    fn drift_targets_reject_invalid_fields_before_loading_rows() {
        let corruptions = [
            "UPDATE documents SET source_id = zeroblob(17)",
            "UPDATE documents SET connector_key = zeroblob(4097)",
            "UPDATE documents SET connector_key = 'file.md'",
            "UPDATE content_blobs SET body_sha256 = zeroblob(31)",
            "UPDATE document_heads SET document_id = zeroblob(17);
             UPDATE document_versions SET document_id = zeroblob(17);
             UPDATE documents SET id = zeroblob(17);",
        ];

        for corruption in corruptions {
            let (connection, _) = drift_bounds_connection(1);
            connection.execute_batch(corruption).unwrap();
            assert!(
                matches!(
                    read_drift_targets_bounded(&connection, 1, 4096),
                    Err(StatusError::Invalid("bounded drift target fields"))
                ),
                "corruption must be rejected before row decoding: {corruption}"
            );
        }

        let (connection, _) = drift_bounds_connection(1);
        let oversized_timestamp =
            "x".repeat(usize::try_from(MAX_STATUS_TIMESTAMP_BYTES).unwrap() + 1);
        connection
            .execute(
                "UPDATE document_versions SET source_updated_at = ?1",
                [oversized_timestamp],
            )
            .unwrap();
        assert!(matches!(
            read_drift_targets_bounded(&connection, 1, 4096),
            Err(StatusError::Invalid("bounded drift target fields"))
        ));
    }

    #[test]
    fn cited_drift_target_applies_the_same_field_bounds() {
        for corruption in [
            "UPDATE documents SET connector_key = zeroblob(4097)",
            "UPDATE content_blobs SET body_sha256 = zeroblob(31)",
        ] {
            let (connection, identity) = drift_bounds_connection(1);
            connection.execute_batch(corruption).unwrap();
            assert!(matches!(
                Status::cited_drift_target(
                    &connection,
                    identity.source_id,
                    identity.document_id,
                    identity.revision_sha256,
                ),
                Err(StatusError::Invalid("bounded drift target fields"))
            ));
        }

        let (connection, identity) = drift_bounds_connection(1);
        let oversized_timestamp =
            "x".repeat(usize::try_from(MAX_STATUS_TIMESTAMP_BYTES).unwrap() + 1);
        connection
            .execute(
                "UPDATE document_versions SET source_updated_at = ?1",
                [oversized_timestamp],
            )
            .unwrap();
        assert!(matches!(
            Status::cited_drift_target(
                &connection,
                identity.source_id,
                identity.document_id,
                identity.revision_sha256,
            ),
            Err(StatusError::Invalid("bounded drift target fields"))
        ));
    }

    #[test]
    fn a_blocked_probe_worker_cannot_extend_the_callers_deadline() {
        TEST_DRIFT_DELAY_MS.store(1_000, Ordering::SeqCst);
        let target = DriftTarget {
            source_id: SourceId::new_v4(),
            document_id: DocumentId::new_v4(),
            connector_key: b"blocked.md".to_vec(),
            source_updated_at: None,
            body_size: 0,
            body_sha256: Sha256Digest::of_bytes(b""),
        };
        let started = Instant::now();
        let report = run_bounded_probe(
            PathBuf::from("/path-that-must-not-be-read-before-timeout"),
            vec![target],
            DriftOptions {
                verify_content_hash: true,
                deadline: Duration::from_millis(20),
            },
        );
        TEST_DRIFT_DELAY_MS.store(0, Ordering::SeqCst);

        assert!(started.elapsed() < Duration::from_millis(750));
        assert!(report.deadline_reached);
        assert_eq!(report.observations[0].state, DriftState::Unknown);
    }
}
