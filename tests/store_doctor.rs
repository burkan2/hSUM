use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hsum::domain::{IndexId, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{
    ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, body_sha256, chunk_bytes,
    revision_sha256,
};
use hsum::store::{
    APPLICATION_ID, DeleteConfirmations, Doctor, FilesystemScope, IndexDb, PreparedChunk,
    PreparedDocument, SCHEMA_VERSION, StoreError, pipeline_fingerprint, prepare_passage_literals,
    schema_checksum,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::{TempDir, tempdir as raw_tempdir};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const FIXTURE_INDEX_ID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
const CHURN_DOCUMENT_COUNT: usize = 25_000;
const MAX_DOCTOR_HEAP_GROWTH_BYTES: usize = 1024 * 1024;

struct TrackingAllocator;

static LIVE_HEAP_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_HEAP_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_heap_growth(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_heap_shrink(layout.size());
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            if new_size >= layout.size() {
                record_heap_growth(new_size - layout.size());
            } else {
                record_heap_shrink(layout.size() - new_size);
            }
        }
        resized
    }
}

fn record_heap_growth(bytes: usize) {
    let live = LIVE_HEAP_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_HEAP_BYTES.fetch_max(live, Ordering::Relaxed);
}

fn record_heap_shrink(bytes: usize) {
    LIVE_HEAP_BYTES.fetch_sub(bytes, Ordering::Relaxed);
}

fn reset_peak_heap_bytes() -> usize {
    let live = LIVE_HEAP_BYTES.load(Ordering::Relaxed);
    PEAK_HEAP_BYTES.store(live, Ordering::Relaxed);
    live
}

type LogicalSnapshot = (
    Vec<(String, Vec<u8>)>,
    Vec<(i64, String, Vec<u8>)>,
    Vec<(String, String)>,
);

fn tempdir() -> std::io::Result<TempDir> {
    let directory = raw_tempdir()?;
    #[cfg(unix)]
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

#[test]
fn doctor_accepts_a_fresh_index_without_logical_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let index_id = create_fixture(&path);
    let before = logical_snapshot(&path);

    let report = Doctor::run(&path).unwrap();

    assert_eq!(report.application_id, APPLICATION_ID);
    assert_eq!(report.schema_version, SCHEMA_VERSION);
    assert_eq!(report.index_id, index_id);
    assert_eq!(report.schema_checksum, schema_checksum());
    assert_eq!(report.pipeline_fingerprint, pipeline_fingerprint());
    assert_eq!(report.journal_mode, "wal");
    assert!(report.read_only);
    assert_eq!(report.scan.content_blobs, 0);
    assert_eq!(report.scan.max_body_rows_in_flight, 0);
    assert_eq!(report.abandoned_generations, 0);
    assert_eq!(logical_snapshot(&path), before);
}

#[test]
fn doctor_repair_removes_only_abandoned_generations_and_is_idempotent() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    create_indexed_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO generations(
                state, created_at, pipeline_fingerprint
             ) VALUES ('abandoned', '2026-08-01T00:00:00Z', ?1)",
            [pipeline_fingerprint().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);

    assert_eq!(Doctor::run(&path).unwrap().abandoned_generations, 1);
    let repaired = Doctor::repair_abandoned(&path, Duration::from_secs(1)).unwrap();
    assert_eq!(repaired.removed_abandoned_generations, 1);
    assert_eq!(repaired.report.abandoned_generations, 0);

    let connection = Connection::open(&path).unwrap();
    let committed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM generations WHERE state = 'committed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(committed, 1);
    drop(connection);

    let repeated = Doctor::repair_abandoned(&path, Duration::from_secs(1)).unwrap();
    assert_eq!(repeated.removed_abandoned_generations, 0);
    assert_eq!(repeated.report.abandoned_generations, 0);
}

#[test]
fn doctor_support_report_is_body_free_and_query_free() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    create_indexed_fixture(&path);

    let report = Doctor::support_report(&path).unwrap();
    let encoded = serde_json::to_string(&report).unwrap();

    assert_eq!(report.format, "hsum.doctor-report.v1");
    assert!(report.body_free);
    assert!(report.query_free);
    assert!(!encoded.contains("alpha-beta"));
    assert!(!encoded.contains("repo:///"));
    assert!(!encoded.contains("connector_key"));
}

#[test]
fn doctor_does_not_create_a_missing_index() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("missing.sqlite");

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::MissingPath(ref actual) if actual == &path
    ));
    assert!(!path.exists());
}

#[test]
fn doctor_rejects_a_wrong_application_id_before_trusting_the_schema() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("wrong-application.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "application_id", 0x1234_5678_i32)
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::InvalidApplicationId {
            expected: APPLICATION_ID,
            actual: 0x1234_5678,
        }
    ));
}

#[test]
fn doctor_rejects_a_newer_schema_version() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("future.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "user_version", i64::from(SCHEMA_VERSION) + 1)
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::UnsupportedSchemaVersion {
            current: SCHEMA_VERSION,
            found,
        } if found == SCHEMA_VERSION + 1
    ));
}

#[test]
fn doctor_rejects_a_tampered_schema_checksum() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tampered.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = zeroblob(32)
             WHERE key = 'schema_checksum'",
            [],
        )
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(error, StoreError::SchemaChecksumMismatch));
}

#[test]
fn doctor_rejects_live_schema_drift_with_unchanged_metadata() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("schema-drift.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("ALTER TABLE sources ADD COLUMN injected TEXT;")
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(error, StoreError::SchemaManifestMismatch));
}

#[test]
fn doctor_rejects_an_extra_schema_object_before_hashing_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("extra-schema-object.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE injected(value TEXT);")
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(error, StoreError::SchemaManifestMismatch));
}

#[test]
fn doctor_rejects_oversized_schema_sql_before_materializing_it() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("oversized-schema-sql.sqlite");
    create_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    let original_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'sources'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let oversized_sql = format!("{original_sql}\n/*{}*/", "x".repeat(64 * 1024 + 1));
    connection
        .pragma_update(None, "writable_schema", true)
        .unwrap();
    connection
        .execute(
            "UPDATE sqlite_schema SET sql = ?1
             WHERE type = 'table' AND name = 'sources'",
            [oversized_sql],
        )
        .unwrap();
    let schema_version: i64 = connection
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap();
    connection
        .pragma_update(None, "schema_version", schema_version + 1)
        .unwrap();
    connection
        .pragma_update(None, "writable_schema", false)
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(error, StoreError::SchemaManifestMismatch));
}

#[test]
fn doctor_rejects_an_unidentified_sqlite_database() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("random.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated(value TEXT);")
        .unwrap();
    drop(connection);
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::InvalidApplicationId {
            expected: APPLICATION_ID,
            actual: 0,
        }
    ));
}

#[test]
fn doctor_validates_an_indexed_active_snapshot() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("indexed.sqlite");
    create_indexed_fixture(&path);

    let report = Doctor::run(&path).unwrap();
    assert_eq!(report.scan.content_blobs, 1);
    assert_eq!(report.scan.chunk_layouts, 1);
    assert_eq!(report.scan.chunks, 1);
    assert_eq!(report.scan.document_versions, 1);
    assert_eq!(report.scan.active_passages, 1);
    assert_eq!(report.scan.max_body_rows_in_flight, 1);
}

#[test]
fn doctor_streams_a_large_corpus_one_original_body_at_a_time() {
    const DOCUMENT_COUNT: usize = 256;

    let directory = tempdir().unwrap();
    let path = directory.path().join("large-corpus.sqlite");
    let mut database = IndexDb::create(
        &path,
        IndexId::from_uuid(Uuid::parse_str(FIXTURE_INDEX_ID).unwrap()),
    )
    .unwrap();
    let documents = (0..DOCUMENT_COUNT)
        .map(|index| {
            let connector_key = format!("notes/{index:04}.md");
            let source_uri = format!("repo://{connector_key}");
            let body = format!("document-{index:04}-alpha-beta {}", "x".repeat(700));
            fixture_document_at(
                connector_key.as_bytes(),
                &source_uri,
                &connector_key,
                body.as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    let expected_bytes = documents
        .iter()
        .map(|document| u64::try_from(document.body.len()).unwrap())
        .sum::<u64>();
    database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(documents);
    drop(database);

    let report = Doctor::run(&path).unwrap();
    let document_count = u64::try_from(DOCUMENT_COUNT).unwrap();

    assert_eq!(report.scan.content_blobs, document_count);
    assert_eq!(report.scan.chunk_layouts, document_count);
    assert_eq!(report.scan.chunks, document_count);
    assert_eq!(report.scan.document_versions, document_count);
    assert_eq!(report.scan.active_passages, document_count);
    assert_eq!(report.scan.original_body_bytes, expected_bytes);
    assert_eq!(report.scan.max_body_rows_in_flight, 1);
    assert!(
        report.scan.original_body_bytes
            > report.scan.max_single_body_bytes.checked_mul(100).unwrap()
    );
}

#[test]
fn doctor_churn_heap_helper() {
    if env::var_os("HSUM_TEST_DOCTOR_CHURN_HELPER").is_none() {
        return;
    }

    let path = env::var_os("HSUM_TEST_INDEX_PATH").expect("index path");
    let expected_documents = env::var("HSUM_TEST_DOCUMENT_COUNT")
        .expect("document count")
        .parse::<u64>()
        .expect("valid document count");
    let baseline = reset_peak_heap_bytes();
    let report = Doctor::run(Path::new(&path)).expect("Doctor accepts churn fixture");
    assert_eq!(report.scan.document_versions, expected_documents);
    drop(report);
    let peak = PEAK_HEAP_BYTES.load(Ordering::Relaxed);
    println!(
        "HSUM_DOCTOR_PEAK_HEAP_GROWTH={}",
        peak.saturating_sub(baseline)
    );
}

#[test]
fn doctor_bounds_replay_heap_across_churn_heavy_history() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("historical-churn.sqlite");
    create_churn_fixture(&path, CHURN_DOCUMENT_COUNT);

    let output = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("doctor_churn_heap_helper")
        .arg("--nocapture")
        .env("HSUM_TEST_DOCTOR_CHURN_HELPER", "1")
        .env("HSUM_TEST_INDEX_PATH", &path)
        .env("HSUM_TEST_DOCUMENT_COUNT", CHURN_DOCUMENT_COUNT.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Doctor churn helper failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let peak_growth = stdout
        .split_whitespace()
        .find_map(|field| {
            field
                .strip_prefix("HSUM_DOCTOR_PEAK_HEAP_GROWTH=")
                .and_then(|value| value.parse::<usize>().ok())
        })
        .expect("helper reports peak heap growth");
    assert!(
        peak_growth <= MAX_DOCTOR_HEAP_GROWTH_BYTES,
        "Doctor grew the Rust heap by {peak_growth} bytes while replaying \
         {CHURN_DOCUMENT_COUNT} historical documents"
    );
}

#[test]
fn doctor_rejects_missing_active_fts_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("fts-drift.sqlite");
    create_indexed_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection.execute("DELETE FROM passages_fts", []).unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::ActiveIndexParity("passages_fts")
    ));
}

#[test]
fn doctor_rejects_missing_active_literal_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("literal-drift.sqlite");
    create_indexed_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM passage_literals", [])
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::ActiveIndexParity("passage_literals")
    ));
}

#[test]
fn doctor_rejects_a_tampered_quote_bloom() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("bloom-drift.sqlite");
    create_indexed_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE chunks SET quote_bloom = zeroblob(512)", [])
        .unwrap();
    drop(connection);

    let error = Doctor::run(&path).unwrap_err();

    assert!(matches!(
        error,
        StoreError::ImmutableEvidenceMismatch("deterministic chunk content")
    ));
}

#[test]
fn doctor_rejects_tampered_content_and_revision_hashes() {
    let directory = tempdir().unwrap();
    let content_path = directory.path().join("content-drift.sqlite");
    create_indexed_fixture(&content_path);
    let connection = Connection::open(&content_path).unwrap();
    connection
        .execute("UPDATE content_blobs SET original_bytes = X'616263'", [])
        .unwrap();
    drop(connection);
    assert!(matches!(
        Doctor::run(&content_path),
        Err(StoreError::ImmutableEvidenceMismatch("content blob digest"))
    ));

    let revision_path = directory.path().join("revision-drift.sqlite");
    create_indexed_fixture(&revision_path);
    let connection = Connection::open(&revision_path).unwrap();
    connection
        .execute(
            "UPDATE document_versions SET revision_sha256 = zeroblob(32)",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        Doctor::run(&revision_path),
        Err(StoreError::ImmutableEvidenceMismatch(
            "document version revision"
        ))
    ));
}

#[test]
fn doctor_rejects_an_active_generation_pointer_that_is_not_latest_committed() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("generation-drift.sqlite");
    create_indexed_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = X''
             WHERE key = 'active_generation'",
            [],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        Doctor::run(&path),
        Err(StoreError::GenerationInvariant(
            "active generation is not the latest committed generation"
        ))
    ));
}

#[test]
fn doctor_rejects_deleted_changes_from_the_first_of_two_generations() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("deleted-generation-history.sqlite");
    create_two_generation_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM generation_changes WHERE generation_id = 1", [])
        .unwrap();
    drop(connection);

    assert!(matches!(
        Doctor::run(&path),
        Err(StoreError::GenerationInvariant(
            "committed generation has no document changes"
        ))
    ));
}

#[test]
fn doctor_rejects_an_epoch_that_disagrees_with_two_committed_generations() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("epoch-history-drift.sqlite");
    create_two_generation_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = CAST(999 AS BLOB)
             WHERE key = 'index_epoch'",
            [],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        Doctor::run(&path),
        Err(StoreError::GenerationInvariant(
            "index epoch does not equal retained committed activation history"
        ))
    ));
}

fn create_fixture(path: &Path) -> IndexId {
    let index_id = IndexId::from_uuid(Uuid::parse_str(FIXTURE_INDEX_ID).unwrap());
    drop(hsum::store::IndexDb::create(path, index_id).unwrap());
    index_id
}

fn create_indexed_fixture(path: &Path) {
    create_generation_fixture(path, &[b"alpha-beta\n"]);
}

fn create_two_generation_fixture(path: &Path) {
    create_generation_fixture(path, &[b"alpha-beta\n", b"gamma-delta\n"]);
}

fn create_churn_fixture(path: &Path, document_count: usize) {
    let index_id = IndexId::from_uuid(Uuid::parse_str(FIXTURE_INDEX_ID).unwrap());
    drop(IndexDb::create(path, index_id).unwrap());

    let body = b"historical alpha-beta\n";
    let body_digest = body_sha256(body);
    let kind = ChunkKind::Markdown;
    let fingerprint = hsum::store::chunker_fingerprint(kind);
    let chunks = chunk_bytes(body, kind, ChunkSettings::default()).unwrap();
    assert_eq!(chunks.len(), 1);
    let chunk = &chunks[0];
    let source_id = fixture_scope().source_id.as_uuid();
    let timestamp = "2026-07-20T00:00:00Z";

    let mut connection = Connection::open(path).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO sources(
                id, kind, name, logical_uri, config_json, created_at
             ) VALUES (?1, 'filesystem', 'churn', 'file:///churn', '{}', ?2)",
            params![source_id.as_bytes().as_slice(), timestamp],
        )
        .unwrap();
    for generation_id in [1_i64, 2_i64] {
        transaction
            .execute(
                "INSERT INTO generations(
                    id, state, created_at, committed_at, pipeline_fingerprint
                 ) VALUES (?1, 'committed', ?2, ?2, ?3)",
                params![
                    generation_id,
                    timestamp,
                    pipeline_fingerprint().as_bytes().as_slice(),
                ],
            )
            .unwrap();
    }
    transaction
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'index_epoch'",
            [b"2".as_slice()],
        )
        .unwrap();
    transaction
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'active_generation'",
            [b"2".as_slice()],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO content_blobs(id, body_sha256, original_bytes)
             VALUES (1, ?1, ?2)",
            params![body_digest.as_bytes().as_slice(), body],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO chunk_layouts(id, content_blob_id, chunker_fingerprint)
             VALUES (1, 1, ?1)",
            [fingerprint.as_bytes().as_slice()],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO chunks(
                id, chunk_layout_id, ordinal, start_byte, end_byte,
                start_line, end_line, body_text, content_sha256, quote_bloom
             ) VALUES (1, 1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                i64::from(chunk.ordinal()),
                i64::try_from(chunk.span().start()).unwrap(),
                i64::try_from(chunk.span().end()).unwrap(),
                i64::try_from(chunk.line_span().start()).unwrap(),
                i64::try_from(chunk.line_span().end()).unwrap(),
                chunk.text(),
                body_sha256(chunk.text().as_bytes()).as_bytes().as_slice(),
                QuoteBloom::from_content(chunk.text().as_bytes())
                    .as_bytes()
                    .as_slice(),
            ],
        )
        .unwrap();

    {
        let mut insert_document = transaction
            .prepare(
                "INSERT INTO documents(
                    id, source_id, connector_key, current_source_uri, tombstoned_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .unwrap();
        let mut insert_version = transaction
            .prepare(
                "INSERT INTO document_versions(
                    id, document_id, content_blob_id, revision_sha256,
                    source_uri, title, metadata_json, indexed_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, '{}', ?6)",
            )
            .unwrap();
        let mut insert_head = transaction
            .prepare(
                "INSERT INTO document_heads(
                    document_id, document_version_id, state, generation_id
                 ) VALUES (?1, NULL, 'tombstoned', 2)",
            )
            .unwrap();
        let mut insert_active_change = transaction
            .prepare(
                "INSERT INTO generation_changes(
                    generation_id, document_id, prior_version_id,
                    next_version_id, next_state
                 ) VALUES (1, ?1, NULL, ?2, 'active')",
            )
            .unwrap();
        let mut insert_tombstone_change = transaction
            .prepare(
                "INSERT INTO generation_changes(
                    generation_id, document_id, prior_version_id,
                    next_version_id, next_state
                 ) VALUES (2, ?1, ?2, NULL, 'tombstoned')",
            )
            .unwrap();

        for ordinal in 0..document_count {
            let ordinal = u64::try_from(ordinal).unwrap();
            let mut document_id = [0_u8; 16];
            document_id[..8].copy_from_slice(b"HSUMCHRN");
            document_id[8..].copy_from_slice(&ordinal.to_be_bytes());
            let connector_key = format!("notes/{ordinal:08}.md");
            let source_uri = format!("repo://{connector_key}");
            let title = connector_key.clone();
            let revision = revision_sha256(&SnapshotRevision {
                body,
                source_uri: &source_uri,
                title: &title,
                metadata: &json!({}),
                source_updated_at: None,
            })
            .unwrap();
            let version_id = i64::try_from(ordinal + 1).unwrap();

            insert_document
                .execute(params![
                    document_id.as_slice(),
                    source_id.as_bytes().as_slice(),
                    connector_key.as_bytes(),
                    &source_uri,
                    timestamp,
                ])
                .unwrap();
            insert_version
                .execute(params![
                    version_id,
                    document_id.as_slice(),
                    revision.as_bytes().as_slice(),
                    &source_uri,
                    &title,
                    timestamp,
                ])
                .unwrap();
            insert_head.execute([document_id.as_slice()]).unwrap();
            insert_active_change
                .execute(params![document_id.as_slice(), version_id])
                .unwrap();
            insert_tombstone_change
                .execute(params![document_id.as_slice(), version_id])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn create_generation_fixture(path: &Path, bodies: &[&[u8]]) {
    let mut database = IndexDb::create(
        path,
        IndexId::from_uuid(Uuid::parse_str(FIXTURE_INDEX_ID).unwrap()),
    )
    .unwrap();
    let scope = fixture_scope();
    for body in bodies {
        database
            .apply_filesystem_snapshot(
                &scope,
                &[fixture_document(body)],
                &[],
                DeleteConfirmations::default(),
            )
            .unwrap();
    }
}

fn fixture_scope() -> FilesystemScope {
    let source_id =
        SourceId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401").unwrap());
    let project_id =
        ProjectId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402").unwrap());
    FilesystemScope {
        source_id,
        source_name: "fixture".parse::<SafeSlug>().unwrap(),
        source_logical_uri: "file:///fixture".to_owned(),
        source_config_json: r#"{"root":"/fixture"}"#.to_owned(),
        project_id,
        project_name: "fixture".parse::<SafeSlug>().unwrap(),
    }
}

fn fixture_document(body: &[u8]) -> PreparedDocument {
    fixture_document_at(b"notes.md", "repo://notes.md", "notes.md", body)
}

fn fixture_document_at(
    connector_key: &[u8],
    source_uri: &str,
    title: &str,
    body: &[u8],
) -> PreparedDocument {
    let metadata = json!({});
    let kind = ChunkKind::from_path(Path::new(source_uri)).unwrap();
    let revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri,
        title,
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.to_owned(),
        title: title.to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256: revision,
        chunker_fingerprint: hsum::store::chunker_fingerprint(kind),
        chunks: chunk_bytes(body, kind, ChunkSettings::default())
            .unwrap()
            .into_iter()
            .map(|chunk| PreparedChunk {
                ordinal: chunk.ordinal(),
                byte_span: chunk.span(),
                line_span: chunk.line_span(),
                body_text: chunk.text().to_owned(),
                content_sha256: body_sha256(chunk.text().as_bytes()),
                quote_bloom: QuoteBloom::from_content(chunk.text().as_bytes()).into_bytes(),
                literals: prepare_passage_literals(title, source_uri, chunk.text().as_bytes()),
            })
            .collect(),
    }
}

fn logical_snapshot(path: &Path) -> LogicalSnapshot {
    let connection = Connection::open(path).unwrap();
    let metadata = collect_rows(
        &connection,
        "SELECT key, value FROM index_meta ORDER BY key",
        |row| Ok((row.get(0)?, row.get(1)?)),
    );
    let migrations = collect_rows(
        &connection,
        "SELECT version, applied_at, checksum
         FROM schema_migrations ORDER BY version",
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    );
    let schema = collect_rows(
        &connection,
        "SELECT type, name FROM sqlite_schema ORDER BY type, name",
        |row| Ok((row.get(0)?, row.get(1)?)),
    );
    (metadata, migrations, schema)
}

fn collect_rows<T>(
    connection: &Connection,
    sql: &str,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map([], map)
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn schema_checksum_is_a_frozen_migration_fixture() {
    assert_eq!(
        schema_checksum().to_string(),
        "2d3e3ab56c1e1f553d0f992a8abc71c252dcba6e1a904df541b188b251b33708"
    );
}
