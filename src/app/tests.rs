use std::fs;

use super::{
    FilesystemIngestError, FilesystemIngestPolicy, FilesystemSourceConfig, ingest_filesystem,
    ingest_filesystem_with_policy, ingest_filesystem_with_timeout,
    plan_filesystem_ingest_with_timeout, prepare_filesystem_snapshot, prepare_jsonl_snapshot,
};
use crate::domain::{IndexId, ProjectId, SafeSlug, SourceId};
use crate::ingest::{DiscoveryOptions, QuoteBloom};
use crate::status::{SourceSyncState, Status};
use crate::store::{
    DeleteConfirmations, Doctor, FilesystemScope, IndexDb, MINIMUM_STORAGE_RESERVE_BYTES,
    StoragePreflightError, StoreError, WriterLock,
};
use rusqlite::Connection;
use tempfile::{TempDir, tempdir};

fn private_tempdir() -> TempDir {
    let directory = tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

#[test]
fn jsonl_preparation_uses_decoded_content_as_the_citation_coordinate_space() {
    let prepared = prepare_jsonl_snapshot(
        br#"{"id":"stable","source_uri":"runbook://payments","content":"a\n\u03b2","metadata":{"z":2,"a":1}}
"#,
    )
    .unwrap();
    assert_eq!(prepared.documents.len(), 1);
    assert!(prepared.explicit_deletions.is_empty());
    let document = &prepared.documents[0];
    assert_eq!(document.connector_key, b"stable");
    assert_eq!(document.body, "a\nβ".as_bytes());
    assert_eq!(document.metadata_json, r#"{"a":1,"z":2}"#);
    assert_eq!(document.chunks[0].byte_span.start(), 0);
    assert_eq!(document.chunks[0].byte_span.end(), 4);
    assert_eq!(document.chunks[0].body_text, "a\nβ");
}

#[test]
fn jsonl_preparation_is_order_independent_and_keeps_explicit_deletions_separate() {
    let prepared = prepare_jsonl_snapshot(
        br#"{"id":"z","source_uri":"runbook://z","deleted":true}
{"id":"b","source_uri":"runbook://b","content":"beta"}
{"id":"a","source_uri":"runbook://a","content":"alpha"}
"#,
    )
    .unwrap();
    assert_eq!(
        prepared
            .documents
            .iter()
            .map(|document| document.connector_key.as_slice())
            .collect::<Vec<_>>(),
        [b"a".as_slice(), b"b".as_slice()]
    );
    assert_eq!(prepared.explicit_deletions, [b"z".to_vec()]);
}

fn scope(root: &std::path::Path) -> FilesystemScope {
    FilesystemScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new("workspace").unwrap(),
        source_logical_uri: root.display().to_string(),
        source_config_json: FilesystemSourceConfig::new(
            root.to_path_buf(),
            DiscoveryOptions::default(),
        )
        .unwrap()
        .to_canonical_json()
        .unwrap(),
        project_id: ProjectId::new_v4(),
        project_name: SafeSlug::new("default").unwrap(),
    }
}

#[test]
fn preparation_preserves_bytes_and_builds_complete_passage_evidence() {
    let directory = private_tempdir();
    fs::write(
        directory.path().join("notes.md"),
        b"\xef\xbb\xbf# Alpha\r\nliteral_token\r\n",
    )
    .unwrap();
    fs::write(directory.path().join("invalid.txt"), [0xff, 0xfe]).unwrap();

    let snapshot =
        prepare_filesystem_snapshot(directory.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(snapshot.documents.len(), 1);
    assert_eq!(snapshot.failures.len(), 1);
    assert_eq!(snapshot.failures[0].code, "SOURCE_INVALID_UTF8");

    let document = &snapshot.documents[0];
    assert_eq!(document.connector_key, b"notes.md");
    assert_eq!(document.source_uri, "repo://notes.md");
    assert_eq!(document.title, "notes.md");
    assert_eq!(document.body, b"\xef\xbb\xbf# Alpha\r\nliteral_token\r\n");
    assert!(document.source_updated_at.is_some());
    assert_eq!(document.chunks[0].byte_span.start(), 3);
    assert_eq!(
        document.chunks[0].quote_bloom,
        QuoteBloom::from_content(document.chunks[0].body_text.as_bytes()).into_bytes()
    );
    assert!(
        document.chunks[0]
            .literals
            .iter()
            .any(|(literal, _)| literal == b"literal_token")
    );
}

#[test]
fn strict_mode_refuses_all_database_mutation_before_a_generation() {
    let directory = private_tempdir();
    fs::write(directory.path().join("good.md"), b"alpha evidence\n").unwrap();
    fs::write(directory.path().join("invalid.txt"), [0xff, 0xfe]).unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let scope = scope(directory.path());

    let error = ingest_filesystem(
        &mut database,
        &scope,
        directory.path(),
        &DiscoveryOptions::default(),
        true,
        DeleteConfirmations::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        FilesystemIngestError::StrictSourceFailures { .. }
    ));

    let outcome = ingest_filesystem(
        &mut database,
        &scope,
        directory.path(),
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();
    assert_eq!(outcome.changed_documents, 1);
    assert_eq!(outcome.active_documents, 1);
}

#[test]
fn an_unchanged_filesystem_snapshot_does_not_create_a_generation() {
    let directory = private_tempdir();
    fs::write(directory.path().join("notes.md"), b"alpha evidence\n").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let scope = scope(directory.path());

    let first = ingest_filesystem(
        &mut database,
        &scope,
        directory.path(),
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();
    let second = ingest_filesystem(
        &mut database,
        &scope,
        directory.path(),
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();

    assert!(first.generation_id.is_some());
    assert_eq!(second.generation_id, None);
    assert_eq!(second.unchanged_documents, 1);
}

#[test]
fn a_bom_only_file_is_an_active_document_with_no_searchable_passages() {
    let directory = private_tempdir();
    fs::write(directory.path().join("bom.txt"), b"\xef\xbb\xbf").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();

    let outcome = ingest_filesystem(
        &mut database,
        &scope(directory.path()),
        directory.path(),
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();

    assert_eq!(outcome.active_documents, 1);
    assert_eq!(outcome.active_passages, 0);
}

#[test]
fn identical_bytes_with_different_source_kinds_use_distinct_chunk_layouts() {
    let directory = private_tempdir();
    let body = (0..180)
        .map(|index| format!("# Heading {index}\nfn item_{index}() {{}}\n\n"))
        .collect::<String>();
    fs::write(directory.path().join("same.md"), body.as_bytes()).unwrap();
    fs::write(directory.path().join("same.rs"), body.as_bytes()).unwrap();
    let prepared =
        prepare_filesystem_snapshot(directory.path(), &DiscoveryOptions::default()).unwrap();
    assert_eq!(prepared.documents.len(), 2);
    assert_eq!(
        prepared.documents[0].body_sha256,
        prepared.documents[1].body_sha256
    );
    assert_ne!(
        prepared.documents[0].chunker_fingerprint,
        prepared.documents[1].chunker_fingerprint
    );

    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let outcome = database
        .apply_filesystem_snapshot(
            &scope(directory.path()),
            &prepared.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(outcome.active_documents, 2);
    drop(database);

    let connection = Connection::open(&database_path).unwrap();
    let blob_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM content_blobs", [], |row| row.get(0))
        .unwrap();
    let layout_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM chunk_layouts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(blob_count, 1);
    assert_eq!(layout_count, 2);
    drop(connection);
    Doctor::run(&database_path).unwrap();
}

#[cfg(unix)]
#[test]
fn ingest_acquires_the_writer_lock_before_discovery() {
    let directory = private_tempdir();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let _writer_lock = WriterLock::acquire(&database_path, std::time::Duration::ZERO).unwrap();
    let missing_root = directory.path().join("missing-root");

    let error = ingest_filesystem_with_timeout(
        &mut database,
        &scope(&missing_root),
        &missing_root,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
        std::time::Duration::ZERO,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        FilesystemIngestError::Store(StoreError::WriterLockBusy { .. })
    ));
}

#[test]
fn source_level_failure_keeps_heads_and_epoch_but_advances_error_status() {
    let directory = private_tempdir();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("notes.md"), b"durable evidence\n").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let initial = ingest_filesystem(
        &mut database,
        &scope,
        &root,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();
    fs::remove_dir_all(&root).unwrap();

    let error = ingest_filesystem(
        &mut database,
        &scope,
        &root,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        FilesystemIngestError::Discovery(crate::ingest::DiscoveryError::RootMissing { .. })
    ));
    drop(database);

    let status = Status::read(&database_path).unwrap();
    assert_eq!(status.index_epoch, initial.index_epoch);
    assert_eq!(status.active_documents, initial.active_documents);
    assert_eq!(status.sources[0].state, SourceSyncState::Partial);
    assert_eq!(
        status.sources[0].last_error_code.as_ref().unwrap().as_str(),
        "SOURCE_UNAVAILABLE"
    );
}

#[test]
fn dry_run_plan_reports_exact_deltas_without_database_mutation() {
    let directory = private_tempdir();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("notes.md"), b"version one\n").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let initial = ingest_filesystem(
        &mut database,
        &scope,
        &root,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap();
    fs::write(root.join("notes.md"), b"version two\n").unwrap();
    fs::write(root.join("invalid.txt"), [0xff, 0xfe]).unwrap();

    let plan = plan_filesystem_ingest_with_timeout(
        &database,
        &scope,
        &root,
        &DiscoveryOptions::default(),
        std::time::Duration::ZERO,
    )
    .unwrap();

    assert_eq!(plan.changed_documents, 1);
    assert_eq!(plan.unchanged_documents, 0);
    assert_eq!(plan.failed_documents, 1);
    assert!(plan.would_create_generation);
    assert!(plan.estimated_write_bytes > 0);
    drop(database);
    let status = Status::read(&database_path).unwrap();
    assert_eq!(status.index_epoch, initial.index_epoch);
    let connection = Connection::open(&database_path).unwrap();
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM generations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(generations, 1);
}

#[test]
fn index_quota_refusal_includes_staging_peak_before_scope_or_generation_mutation() {
    let directory = private_tempdir();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("notes.md"), b"quota bounded evidence\n").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let mut scope = scope(&root);
    scope.source_config_json =
        FilesystemSourceConfig::new(root.clone(), DiscoveryOptions::default())
            .unwrap()
            .with_index_quota_bytes(Some(1))
            .unwrap()
            .to_canonical_json()
            .unwrap();

    let error = ingest_filesystem_with_policy(
        &mut database,
        &scope,
        &root,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
        FilesystemIngestPolicy {
            lock_timeout: std::time::Duration::ZERO,
            index_quota_bytes: Some(1),
        },
    )
    .unwrap_err();

    let required_bytes = match error {
        FilesystemIngestError::StoragePreflight(StoragePreflightError::QuotaExceeded {
            required_bytes,
            ..
        }) => required_bytes,
        other => panic!("expected a quota refusal, received {other:?}"),
    };
    assert!(
        required_bytes
            >= MINIMUM_STORAGE_RESERVE_BYTES + 7 * b"quota bounded evidence\n".len() as u64
    );
    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM generations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn bound_scope_rejects_root_options_and_quota_overrides_before_mutation() {
    let directory = private_tempdir();
    let root_a = directory.path().join("source-a");
    let root_b = directory.path().join("source-b");
    fs::create_dir(&root_a).unwrap();
    fs::create_dir(&root_b).unwrap();
    fs::write(root_a.join("inside.md"), b"inside authority\n").unwrap();
    fs::write(root_b.join("outside.md"), b"outside authority\n").unwrap();
    let database_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&database_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root_a);

    let wrong_root = plan_filesystem_ingest_with_timeout(
        &database,
        &scope,
        &root_b,
        &DiscoveryOptions::default(),
        std::time::Duration::ZERO,
    )
    .unwrap_err();
    assert!(matches!(
        wrong_root,
        FilesystemIngestError::SourceAuthorityMismatch
    ));

    let wrong_options = ingest_filesystem(
        &mut database,
        &scope,
        &root_a,
        &DiscoveryOptions::default().allow_sensitive(true),
        false,
        DeleteConfirmations::default(),
    )
    .unwrap_err();
    assert!(matches!(
        wrong_options,
        FilesystemIngestError::SourceAuthorityMismatch
    ));

    let wrong_quota = ingest_filesystem_with_policy(
        &mut database,
        &scope,
        &root_a,
        &DiscoveryOptions::default(),
        false,
        DeleteConfirmations::default(),
        FilesystemIngestPolicy {
            lock_timeout: std::time::Duration::ZERO,
            index_quota_bytes: Some(MINIMUM_STORAGE_RESERVE_BYTES * 2),
        },
    )
    .unwrap_err();
    assert!(matches!(
        wrong_quota,
        FilesystemIngestError::SourceAuthorityMismatch
    ));
    drop(database);

    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM generations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
