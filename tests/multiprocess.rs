#![cfg(unix)]

use std::env;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(debug_assertions)]
use hsum::domain::{ByteSpan, IndexId, LineSpan, ProjectId, SafeSlug, SourceId};
#[cfg(debug_assertions)]
use hsum::ingest::{QuoteBloom, SnapshotRevision, body_sha256, revision_sha256};
#[cfg(debug_assertions)]
use hsum::store::{
    DeleteConfirmations, Doctor, FilesystemScope, IndexDb, OpenMode, PreparedChunk,
    PreparedDocument, StoreError, WriterLock, prepare_passage_literals,
};
#[cfg(not(debug_assertions))]
use hsum::store::{StoreError, WriterLock};
#[cfg(debug_assertions)]
use rusqlite::{Connection, ErrorCode};
#[cfg(debug_assertions)]
use serde_json::json;
#[cfg(debug_assertions)]
use uuid::Uuid;

mod support;
use support::private_tempdir as tempdir;

#[test]
fn writer_lock_holder_helper() {
    if env::var_os("HSUM_TEST_LOCK_HELPER").is_none() {
        return;
    }

    let index_path = env::var_os("HSUM_TEST_INDEX_PATH").expect("index path");
    let ready_path = env::var_os("HSUM_TEST_READY_PATH").expect("ready path");
    let release_path = env::var_os("HSUM_TEST_RELEASE_PATH").expect("release path");
    let _lock = WriterLock::acquire(index_path.as_ref(), Duration::ZERO).expect("acquire lock");
    fs::write(&ready_path, b"ready").expect("publish readiness");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::path::Path::new(&release_path).exists() {
        assert!(
            Instant::now() < deadline,
            "parent did not release lock helper"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn second_process_receives_a_bounded_writer_lock_busy_error() {
    let directory = tempdir().unwrap();
    let index_path = directory.path().join("index.sqlite");
    let ready_path = directory.path().join("ready");
    let release_path = directory.path().join("release");
    let executable = env::current_exe().unwrap();

    let mut child = Command::new(executable)
        .arg("--exact")
        .arg("writer_lock_holder_helper")
        .arg("--nocapture")
        .env("HSUM_TEST_LOCK_HELPER", "1")
        .env("HSUM_TEST_INDEX_PATH", &index_path)
        .env("HSUM_TEST_READY_PATH", &ready_path)
        .env("HSUM_TEST_RELEASE_PATH", &release_path)
        .spawn()
        .unwrap();

    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while !ready_path.exists() {
        if Instant::now() >= ready_deadline {
            fs::write(&release_path, b"release").unwrap();
            let _ = child.wait();
            panic!("lock helper did not become ready");
        }
        thread::sleep(Duration::from_millis(5));
    }

    let started = Instant::now();
    let error = WriterLock::acquire(&index_path, Duration::from_millis(75)).unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(error, StoreError::WriterLockBusy { .. }));
    assert!(elapsed >= Duration::from_millis(50));
    assert!(elapsed < Duration::from_secs(2));

    fs::write(&release_path, b"release").unwrap();
    assert!(child.wait().unwrap().success());
    WriterLock::acquire(&index_path, Duration::ZERO).expect("released lock is reusable");
}

#[test]
fn writer_lock_refuses_a_symlink_sidecar_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let directory = tempdir().unwrap();
    let index_path = directory.path().join("index.sqlite");
    let victim_path = directory.path().join("victim");
    fs::write(&victim_path, b"unchanged").unwrap();
    symlink(&victim_path, WriterLock::sidecar_path(&index_path)).unwrap();

    let error = WriterLock::acquire(&index_path, Duration::ZERO).unwrap_err();
    assert!(matches!(error, StoreError::UnsafeWriterLock(_)));
    assert_eq!(fs::read(victim_path).unwrap(), b"unchanged");
}

#[cfg(debug_assertions)]
#[test]
fn sqlite_full_generation_helper() {
    if env::var_os("HSUM_TEST_SQLITE_FULL_HELPER").is_none() {
        return;
    }

    let index_path = env::var_os("HSUM_TEST_INDEX_PATH").expect("index path");
    let mut database =
        IndexDb::open_existing(index_path.as_ref(), OpenMode::ReadWrite).expect("open writer");
    let capped_pages = database
        .debug_cap_sqlite_pages_at_current_size()
        .expect("cap writer connection");
    assert!(capped_pages > 0);

    let prior = prepared_document(b"prior.md", "repo://prior.md", b"durable_prior_token\n");
    let mut oversized_snapshot = Vec::with_capacity(513);
    oversized_snapshot.push(prior);
    let body = format!("generation_new_token {}\n", "x".repeat(1_000));
    for ordinal in 0..512 {
        let key = format!("new-{ordinal:04}.md");
        let uri = format!("repo://{key}");
        oversized_snapshot.push(prepared_document(key.as_bytes(), &uri, body.as_bytes()));
    }

    let error = database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &oversized_snapshot,
            &[],
            DeleteConfirmations::default(),
        )
        .expect_err("the page cap must fail the generation");
    assert!(matches!(
        error,
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if code.code == ErrorCode::DiskFull
    ));
}

#[cfg(debug_assertions)]
#[test]
fn subprocess_sqlite_full_recovers_the_prior_generation_without_build_leaks() {
    let directory = tempdir().unwrap();
    let index_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&index_path, fixture_index_id()).unwrap();
    let prior = prepared_document(b"prior.md", "repo://prior.md", b"durable_prior_token\n");
    database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            std::slice::from_ref(&prior),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let prior = Connection::open(&index_path).unwrap();
    let prior_row_counts = evidence_row_counts(&prior);
    drop(prior);

    let output = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("sqlite_full_generation_helper")
        .arg("--nocapture")
        .env("HSUM_TEST_SQLITE_FULL_HELPER", "1")
        .env("HSUM_TEST_INDEX_PATH", &index_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "full helper failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report = Doctor::run(&index_path).unwrap();
    assert_eq!(report.index_id, fixture_index_id());
    let validated_reader = IndexDb::open_existing(&index_path, OpenMode::ReadOnly).unwrap();
    assert!(validated_reader.is_read_only().unwrap());
    drop(validated_reader);

    let recovered = Connection::open(&index_path).unwrap();
    assert_eq!(evidence_row_counts(&recovered), prior_row_counts);
    assert_eq!(count(&recovered, "generations"), 1);
    assert_eq!(
        recovered
            .query_row(
                "SELECT COUNT(*) FROM generations WHERE state = 'building'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        recovered
            .query_row(
                "SELECT COUNT(*) FROM generations WHERE state = 'committed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(count(&recovered, "documents"), 1);
    assert_eq!(count(&recovered, "content_blobs"), 1);
    assert_eq!(count(&recovered, "chunk_layouts"), 1);
    assert_eq!(count(&recovered, "chunks"), 1);
    assert_eq!(count(&recovered, "document_versions"), 1);
    assert_eq!(count(&recovered, "generation_changes"), 1);
    assert_eq!(count(&recovered, "document_heads"), 1);
    assert_eq!(count(&recovered, "active_passages"), 1);
    assert_eq!(count(&recovered, "passages_fts"), 1);
    assert_eq!(
        recovered
            .query_row(
                "SELECT generation_id
                 FROM document_heads AS dh
                 JOIN documents AS d ON d.id = dh.document_id
                 WHERE dh.state = 'active' AND d.connector_key = ?1",
                [b"prior.md".as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(fts_match_count(&recovered, "durable_prior_token"), 1);
    assert_eq!(fts_match_count(&recovered, "generation_new_token"), 0);
    let (active_generation, index_epoch): (Vec<u8>, Vec<u8>) = recovered
        .query_row(
            "SELECT
                (SELECT value FROM index_meta WHERE key = 'active_generation'),
                (SELECT value FROM index_meta WHERE key = 'index_epoch')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(active_generation, b"1");
    assert_eq!(index_epoch, b"1");
}

#[cfg(debug_assertions)]
fn fixture_index_id() -> IndexId {
    IndexId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap())
}

#[cfg(debug_assertions)]
fn fixture_scope() -> FilesystemScope {
    FilesystemScope {
        source_id: SourceId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401").unwrap(),
        ),
        source_name: "fixture".parse::<SafeSlug>().unwrap(),
        source_logical_uri: "file:///fixture".to_owned(),
        source_config_json: r#"{"root":"/fixture"}"#.to_owned(),
        project_id: ProjectId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402").unwrap(),
        ),
        project_name: "fixture".parse::<SafeSlug>().unwrap(),
    }
}

#[cfg(debug_assertions)]
fn prepared_document(connector_key: &[u8], source_uri: &str, body: &[u8]) -> PreparedDocument {
    let body_text = std::str::from_utf8(body).unwrap();
    let metadata = json!({});
    let revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri,
        title: source_uri.rsplit('/').next().unwrap(),
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    let line_count = body.iter().filter(|byte| **byte == b'\n').count().max(1);

    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.to_owned(),
        title: source_uri.rsplit('/').next().unwrap().to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256: revision,
        chunker_fingerprint: hsum::store::chunker_fingerprint(
            hsum::ingest::ChunkKind::from_path(std::path::Path::new(source_uri)).unwrap(),
        ),
        chunks: vec![PreparedChunk {
            ordinal: 0,
            byte_span: ByteSpan::new(0, body.len() as u64).unwrap(),
            line_span: LineSpan::new(1, line_count as u64).unwrap(),
            body_text: body_text.to_owned(),
            content_sha256: body_sha256(body),
            quote_bloom: QuoteBloom::from_content(body).into_bytes(),
            literals: prepare_passage_literals(
                source_uri.rsplit('/').next().unwrap(),
                source_uri,
                body,
            ),
        }],
    }
}

#[cfg(debug_assertions)]
fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[cfg(debug_assertions)]
fn fts_match_count(connection: &Connection, query: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM passages_fts WHERE passages_fts MATCH ?1",
            [query],
            |row| row.get(0),
        )
        .unwrap()
}

#[cfg(debug_assertions)]
fn evidence_row_counts(connection: &Connection) -> Vec<(&'static str, i64)> {
    [
        "generations",
        "sources",
        "projects",
        "project_sources",
        "documents",
        "content_blobs",
        "chunk_layouts",
        "chunks",
        "document_versions",
        "generation_changes",
        "document_heads",
        "active_passages",
        "passages_fts",
        "passage_literals",
        "source_sync_errors",
    ]
    .into_iter()
    .map(|table| (table, count(connection, table)))
    .collect()
}
