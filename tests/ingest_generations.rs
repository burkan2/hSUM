use hsum::domain::{ByteSpan, IndexId, LineSpan, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{QuoteBloom, SnapshotRevision, body_sha256, revision_sha256};
use hsum::store::{
    DeleteConfirmations, Doctor, FilesystemScope, IndexDb, OpenMode, PreparedChunk,
    PreparedDocument, SnapshotFailure, StoreError, prepare_passage_literals,
};
use rusqlite::{Connection, ErrorCode};
use serde_json::json;
use uuid::Uuid;

mod support;
use support::private_tempdir as tempdir;

#[test]
fn first_ingest_builds_immutable_history_and_active_indexes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let documents = vec![
        prepared_document(b"README.md", "repo://README.md", b"# hSUM\nalpha-beta\n"),
        prepared_document(
            b"src/lib.rs",
            "repo://src/lib.rs",
            b"EVIDENCE_FORGOTTEN alpha-beta\n",
        ),
    ];

    let outcome = database
        .apply_filesystem_snapshot(&scope, &documents, &[], DeleteConfirmations::default())
        .unwrap();

    assert_eq!(outcome.changed_documents, 2);
    assert_eq!(outcome.unchanged_documents, 0);
    assert_eq!(outcome.tombstoned_documents, 0);
    assert_eq!(outcome.active_documents, 2);
    assert_eq!(outcome.active_passages, 2);
    assert_eq!(outcome.index_epoch, 1);
    assert_eq!(outcome.generation_id, Some(1));

    drop(database);
    let connection = Connection::open(&path).unwrap();
    assert_eq!(count(&connection, "content_blobs"), 2);
    assert_eq!(count(&connection, "chunk_layouts"), 2);
    assert_eq!(count(&connection, "chunks"), 2);
    assert_eq!(count(&connection, "document_versions"), 2);
    assert_eq!(count(&connection, "document_heads"), 2);
    assert_eq!(count(&connection, "active_passages"), 2);
    assert_eq!(count(&connection, "passages_fts"), 2);
    assert!(count(&connection, "passage_literals") >= 2);

    let match_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM passages_fts
             WHERE passages_fts MATCH 'EVIDENCE_FORGOTTEN'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(match_count, 1);
}

#[test]
fn unchanged_ingest_adds_no_generation_or_index_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let documents = vec![prepared_document(
        b"notes.md",
        "repo://notes.md",
        b"stable alpha-beta\n",
    )];
    database
        .apply_filesystem_snapshot(&scope, &documents, &[], DeleteConfirmations::default())
        .unwrap();
    let before = row_counts(&path);

    let outcome = database
        .apply_filesystem_snapshot(&scope, &documents, &[], DeleteConfirmations::default())
        .unwrap();

    assert_eq!(outcome.changed_documents, 0);
    assert_eq!(outcome.unchanged_documents, 1);
    assert_eq!(outcome.index_epoch, 1);
    assert_eq!(outcome.generation_id, None);
    assert_eq!(row_counts(&path), before);
}

#[test]
fn editing_one_document_writes_only_its_delta() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![
        prepared_document(b"a.md", "repo://a.md", b"old alpha-beta\n"),
        prepared_document(b"b.md", "repo://b.md", b"stable beta-gamma\n"),
    ];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();
    let edited = vec![
        prepared_document(b"a.md", "repo://a.md", b"new alpha-beta\n"),
        initial[1].clone(),
    ];

    let outcome = database
        .apply_filesystem_snapshot(&scope, &edited, &[], DeleteConfirmations::default())
        .unwrap();

    assert_eq!(outcome.changed_documents, 1);
    assert_eq!(outcome.unchanged_documents, 1);
    assert_eq!(outcome.index_epoch, 2);
    assert_eq!(outcome.generation_id, Some(2));

    drop(database);
    let connection = Connection::open(path).unwrap();
    assert_eq!(count(&connection, "content_blobs"), 3);
    assert_eq!(count(&connection, "chunk_layouts"), 3);
    assert_eq!(count(&connection, "chunks"), 3);
    assert_eq!(count(&connection, "document_versions"), 3);
    assert_eq!(count(&connection, "document_heads"), 2);
    assert_eq!(count(&connection, "active_passages"), 2);
    assert_eq!(count(&connection, "passages_fts"), 2);

    let latest_change_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM generation_changes WHERE generation_id = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(latest_change_count, 1);
}

#[test]
fn a_reader_spanning_commit_sees_the_complete_old_snapshot_then_the_new_snapshot() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(
                b"notes.md",
                "repo://notes.md",
                b"generation_old_token\n",
            )],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();

    let old_reader = Connection::open(&path).unwrap();
    old_reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert_eq!(fts_match_count(&old_reader, "generation_old_token"), 1);
    assert_eq!(fts_match_count(&old_reader, "generation_new_token"), 0);

    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(
                b"notes.md",
                "repo://notes.md",
                b"generation_new_token\n",
            )],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();

    assert_eq!(fts_match_count(&old_reader, "generation_old_token"), 1);
    assert_eq!(fts_match_count(&old_reader, "generation_new_token"), 0);

    let new_reader = Connection::open(&path).unwrap();
    assert_eq!(fts_match_count(&new_reader, "generation_old_token"), 0);
    assert_eq!(fts_match_count(&new_reader, "generation_new_token"), 1);
    drop(new_reader);

    old_reader.execute_batch("COMMIT").unwrap();
    assert_eq!(fts_match_count(&old_reader, "generation_old_token"), 0);
    assert_eq!(fts_match_count(&old_reader, "generation_new_token"), 1);
}

#[test]
fn sqlite_full_rolls_back_the_generation_and_a_fresh_reader_recovers_the_prior_snapshot() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let prior = prepared_document(b"prior.md", "repo://prior.md", b"durable_prior_token\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&prior),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let mut oversized_snapshot = Vec::with_capacity(513);
    oversized_snapshot.push(prior);
    let body = format!("{}\n", "x".repeat(1_000));
    for ordinal in 0..512 {
        let key = format!("new-{ordinal:04}.md");
        let uri = format!("repo://{key}");
        oversized_snapshot.push(prepared_document(key.as_bytes(), &uri, body.as_bytes()));
    }
    let mut database = IndexDb::open_existing(&path, OpenMode::ReadWrite).unwrap();
    assert!(database.debug_cap_sqlite_pages_at_current_size().unwrap() > 0);
    let error = database
        .apply_filesystem_snapshot(
            &scope,
            &oversized_snapshot,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if code.code == ErrorCode::DiskFull
    ));
    drop(database);

    Doctor::run(&path).unwrap();
    let recovered = Connection::open(&path).unwrap();
    assert_eq!(count(&recovered, "generations"), 1);
    assert_eq!(count(&recovered, "document_heads"), 1);
    assert_eq!(fts_match_count(&recovered, "durable_prior_token"), 1);
    assert_eq!(fts_match_count(&recovered, "xxxxxxxx"), 0);
}

#[test]
fn unique_same_body_rename_retains_document_identity_and_reuses_chunks() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![prepared_document(
        b"old.md",
        "repo://old.md",
        b"rename-me alpha-beta\n",
    )];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();
    let original_id = document_id_for_key(&path, b"old.md");
    let renamed = vec![prepared_document(
        b"new.md",
        "repo://new.md",
        b"rename-me alpha-beta\n",
    )];

    let outcome = database
        .apply_filesystem_snapshot(&scope, &renamed, &[], DeleteConfirmations::default())
        .unwrap();

    assert_eq!(outcome.changed_documents, 1);
    assert_eq!(outcome.tombstoned_documents, 0);
    assert_eq!(document_id_for_key(&path, b"new.md"), original_id);

    drop(database);
    let connection = Connection::open(path).unwrap();
    assert_eq!(count(&connection, "documents"), 1);
    assert_eq!(count(&connection, "content_blobs"), 1);
    assert_eq!(count(&connection, "chunk_layouts"), 1);
    assert_eq!(count(&connection, "chunks"), 1);
    assert_eq!(count(&connection, "document_versions"), 2);
}

#[test]
fn mutation_refuses_a_corrupt_active_search_baseline() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![
        prepared_document(b"a.md", "repo://a.md", b"alpha evidence\n"),
        prepared_document(b"b.md", "repo://b.md", b"beta evidence\n"),
    ];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();
    drop(database);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "DELETE FROM passages_fts
             WHERE rowid = (SELECT MIN(rowid) FROM passages_fts)",
            [],
        )
        .unwrap();
    drop(connection);

    let mut database = IndexDb::open_existing(&path, OpenMode::ReadWrite).unwrap();
    let changed = vec![
        initial[0].clone(),
        prepared_document(b"b.md", "repo://b.md", b"beta evidence changed\n"),
    ];
    let error = database
        .apply_filesystem_snapshot(&scope, &changed, &[], DeleteConfirmations::default())
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::ImmutableEvidenceMismatch(_)
            | StoreError::ActiveIndexParity(_)
            | StoreError::Sqlite(_)
    ));
    drop(database);
    let connection = Connection::open(path).unwrap();
    assert_eq!(count(&connection, "generations"), 1);
}

#[test]
fn failed_present_file_carries_its_prior_head_forward() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![
        prepared_document(b"a.md", "repo://a.md", b"alpha-beta\n"),
        prepared_document(b"b.md", "repo://b.md", b"beta-gamma\n"),
    ];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();

    let outcome = database
        .apply_filesystem_snapshot(
            &scope,
            &initial[..1],
            &[SnapshotFailure {
                connector_key: b"b.md".to_vec(),
                code: "SOURCE_CHANGED_DURING_READ".to_owned(),
                detail: "file changed twice".to_owned(),
            }],
            DeleteConfirmations::default(),
        )
        .unwrap();

    assert_eq!(outcome.changed_documents, 0);
    assert_eq!(outcome.unchanged_documents, 1);
    assert_eq!(outcome.tombstoned_documents, 0);
    assert_eq!(outcome.carried_forward_documents, 1);
    assert_eq!(outcome.failed_documents, 1);
    assert!(outcome.is_partial());
    assert_eq!(
        outcome.source_outcomes[0].state,
        hsum::store::SourceIngestState::Partial
    );
    assert_eq!(outcome.active_documents, 2);
    assert_eq!(outcome.generation_id, None);
    assert_eq!(outcome.index_epoch, 1);

    drop(database);
    let connection = Connection::open(path).unwrap();
    assert_eq!(count(&connection, "source_sync_errors"), 0);
    assert_eq!(count(&connection, "active_passages"), 2);
    assert_eq!(count(&connection, "generations"), 1);
}

#[test]
fn a_failed_new_file_is_not_counted_as_carried_forward() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![prepared_document(b"a.md", "repo://a.md", b"alpha-beta\n")];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();

    let outcome = database
        .apply_filesystem_snapshot(
            &scope,
            &initial,
            &[SnapshotFailure {
                connector_key: b"new.md".to_vec(),
                code: "SOURCE_CHANGED_DURING_READ".to_owned(),
                detail: "new file changed twice".to_owned(),
            }],
            DeleteConfirmations::default(),
        )
        .unwrap();

    assert_eq!(outcome.carried_forward_documents, 0);
    assert_eq!(outcome.failed_documents, 1);
    assert!(outcome.is_partial());
    assert_eq!(
        outcome.source_outcomes[0].state,
        hsum::store::SourceIngestState::Partial
    );
    assert_eq!(outcome.active_documents, 1);
    assert_eq!(outcome.generation_id, None);
    assert_eq!(outcome.index_epoch, 1);
}

#[test]
fn all_failed_snapshot_keeps_generation_epoch_and_last_success() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![prepared_document(b"a.md", "repo://a.md", b"alpha-beta\n")];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();
    let inspection = Connection::open(&path).unwrap();
    let before_last_success: String = inspection
        .query_row(
            "SELECT last_success_at FROM sources WHERE id = ?1",
            [scope.source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    drop(inspection);

    let outcome = database
        .apply_filesystem_snapshot(
            &scope,
            &[],
            &[SnapshotFailure {
                connector_key: b"a.md".to_vec(),
                code: "SOURCE_CHANGED_DURING_READ".to_owned(),
                detail: "file changed twice".to_owned(),
            }],
            DeleteConfirmations::default(),
        )
        .unwrap();

    assert_eq!(outcome.generation_id, None);
    assert_eq!(outcome.index_epoch, 1);
    assert_eq!(outcome.active_documents, 1);
    assert_eq!(
        outcome.source_outcomes[0].state,
        hsum::store::SourceIngestState::Failed
    );
    let inspection = Connection::open(&path).unwrap();
    let (after_last_success, error_code): (String, String) = inspection
        .query_row(
            "SELECT last_success_at, last_error_code
             FROM sources WHERE id = ?1",
            [scope.source_id.as_uuid().as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after_last_success, before_last_success);
    assert_eq!(error_code, "SOURCE_CHANGED_DURING_READ");
    assert_eq!(count(&inspection, "generations"), 1);
}

#[test]
fn a_failed_new_file_cannot_bypass_the_independent_empty_snapshot_guard() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"a.md", "repo://a.md", b"alpha-beta\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let failures = [SnapshotFailure {
        connector_key: b"new-invalid.md".to_vec(),
        code: "SOURCE_INVALID_UTF8".to_owned(),
        detail: "new file is not UTF-8".to_owned(),
    }];

    let plan = database
        .plan_filesystem_snapshot_with_timeout(&scope, &[], &failures, std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(plan.projected_active_documents, 0);
    assert!(plan.requires_empty_snapshot_confirmation);
    assert!(plan.requires_mass_delete_confirmation);

    let error = database
        .apply_filesystem_snapshot(
            &scope,
            &[],
            &failures,
            DeleteConfirmations {
                allow_empty_snapshot: false,
                allow_mass_delete: true,
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::EmptySnapshotConfirmationRequired
    ));
    let inspection = Connection::open(&path).unwrap();
    assert_eq!(count(&inspection, "generations"), 1);
    assert_eq!(
        count(&inspection, "active_passages"),
        1,
        "the rejected snapshot must leave the active corpus untouched"
    );
    drop(inspection);

    let outcome = database
        .apply_filesystem_snapshot(
            &scope,
            &[],
            &failures,
            DeleteConfirmations {
                allow_empty_snapshot: true,
                allow_mass_delete: true,
            },
        )
        .unwrap();
    assert_eq!(outcome.generation_id, Some(2));
    assert_eq!(outcome.index_epoch, 2);
    assert_eq!(outcome.tombstoned_documents, 1);
    assert_eq!(
        outcome.source_outcomes[0].state,
        hsum::store::SourceIngestState::Partial,
        "a committed deletion plus a failed observed file is a partial ingest"
    );
}

#[test]
fn deletion_guards_are_separate_and_exactly_twenty_five_percent_is_allowed() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let initial = vec![
        prepared_document(b"a.md", "repo://a.md", b"a alpha-beta\n"),
        prepared_document(b"b.md", "repo://b.md", b"b alpha-beta\n"),
        prepared_document(b"c.md", "repo://c.md", b"c alpha-beta\n"),
        prepared_document(b"d.md", "repo://d.md", b"d alpha-beta\n"),
    ];
    database
        .apply_filesystem_snapshot(&scope, &initial, &[], DeleteConfirmations::default())
        .unwrap();

    let quarter = database
        .apply_filesystem_snapshot(&scope, &initial[..3], &[], DeleteConfirmations::default())
        .unwrap();
    assert_eq!(quarter.tombstoned_documents, 1);
    assert_eq!(quarter.active_documents, 3);

    let empty_error = database
        .apply_filesystem_snapshot(&scope, &[], &[], DeleteConfirmations::default())
        .unwrap_err();
    assert!(matches!(
        empty_error,
        StoreError::EmptySnapshotConfirmationRequired
    ));

    let mass_error = database
        .apply_filesystem_snapshot(&scope, &initial[..1], &[], DeleteConfirmations::default())
        .unwrap_err();
    assert!(matches!(
        mass_error,
        StoreError::MassDeleteConfirmationRequired {
            absent: 2,
            eligible_prior: 3,
        }
    ));

    let allowed = database
        .apply_filesystem_snapshot(
            &scope,
            &initial[..1],
            &[],
            DeleteConfirmations {
                allow_empty_snapshot: false,
                allow_mass_delete: true,
            },
        )
        .unwrap();
    assert_eq!(allowed.tombstoned_documents, 2);
    assert_eq!(allowed.active_documents, 1);
}

#[test]
fn prepared_evidence_semantics_are_recomputed_before_any_write() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let valid = prepared_document(b"notes.md", "repo://notes.md", b"stable alpha-beta\n");

    let mut forged_revision = valid.clone();
    forged_revision.revision_sha256 = hsum::domain::Sha256Digest::of_bytes(b"forged revision");
    assert!(matches!(
        database.apply_filesystem_snapshot(
            &scope,
            &[forged_revision],
            &[],
            DeleteConfirmations::default()
        ),
        Err(StoreError::InvalidPreparedDocument(_))
    ));

    let mut wrong_pipeline = valid.clone();
    wrong_pipeline.chunker_fingerprint = hsum::domain::Sha256Digest::of_bytes(b"different chunker");
    assert!(matches!(
        database.apply_filesystem_snapshot(
            &scope,
            &[wrong_pipeline],
            &[],
            DeleteConfirmations::default()
        ),
        Err(StoreError::InvalidPreparedDocument(_))
    ));

    let mut stale_bloom = valid.clone();
    stale_bloom.chunks[0].quote_bloom[0] ^= 1;
    assert!(matches!(
        database.apply_filesystem_snapshot(
            &scope,
            &[stale_bloom],
            &[],
            DeleteConfirmations::default()
        ),
        Err(StoreError::InvalidPreparedDocument(_))
    ));

    let mut wrong_lines = valid;
    wrong_lines.chunks[0].line_span = LineSpan::new(2, 2).unwrap();
    assert!(matches!(
        database.apply_filesystem_snapshot(
            &scope,
            &[wrong_lines],
            &[],
            DeleteConfirmations::default()
        ),
        Err(StoreError::InvalidPreparedDocument(_))
    ));

    assert_eq!(row_counts(&path)[0], 0);
}

fn create_database(path: &std::path::Path) -> IndexDb {
    IndexDb::create(
        path,
        IndexId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap()),
    )
    .unwrap()
}

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

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn fts_match_count(connection: &Connection, query: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM passages_fts WHERE passages_fts MATCH ?1",
            [query],
            |row| row.get(0),
        )
        .unwrap()
}

fn document_id_for_key(path: &std::path::Path, key: &[u8]) -> Vec<u8> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT id FROM documents WHERE connector_key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn row_counts(path: &std::path::Path) -> Vec<i64> {
    let connection = Connection::open(path).unwrap();
    [
        "generations",
        "content_blobs",
        "chunk_layouts",
        "chunks",
        "document_versions",
        "generation_changes",
        "active_passages",
        "passages_fts",
        "passage_literals",
    ]
    .into_iter()
    .map(|table| count(&connection, table))
    .collect()
}
