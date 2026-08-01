use std::time::Duration;

use hsum::app::{
    EvidenceSourceState, GetEvidence, GetEvidenceFieldLimits, GetEvidenceRequest,
    JsonlBatchIngestError, JsonlFileIngestError, JsonlIngestTarget, JsonlSourceConfig,
    SourceHashVerification, ingest_jsonl_sources_with_timeout, ingest_jsonl_with_timeout,
    prepare_jsonl_snapshot,
};
use hsum::domain::{IndexId, ProjectId, SafeSlug, SourceId};
use hsum::search::{GetRequest, SearchRequest};
use hsum::status::Status;
use hsum::store::{
    DeleteConfirmations, Doctor, IndexDb, JsonlScope, SourceIngestState, StoragePreflightError,
    StoreError,
};
use rusqlite::Connection;
use tempfile::tempdir;

fn scope() -> JsonlScope {
    JsonlScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new("records").unwrap(),
        source_logical_uri: "/fixture/records.jsonl".to_owned(),
        source_config_json: r#"{"path":"/fixture/records.jsonl","schema_version":1}"#.to_owned(),
        project_id: ProjectId::new_v4(),
        project_name: SafeSlug::new("default").unwrap(),
    }
}

fn database() -> (tempfile::TempDir, std::path::PathBuf, IndexDb) {
    let directory = tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.path().join("index.sqlite");
    let database = IndexDb::create(&path, IndexId::new_v4()).unwrap();
    (directory, path, database)
}

#[test]
fn decoded_content_is_stored_with_exact_chunk_and_citation_offsets() {
    let (_directory, path, mut database) = database();
    let scope = scope();
    let prepared = prepare_jsonl_snapshot(
        br#"{"id":"alpha","source_uri":"runbook://alpha","content":"a\n\u03b2"}
"#,
    )
    .unwrap();

    let outcome = database
        .apply_jsonl_snapshot(
            &scope,
            &prepared.documents,
            &prepared.explicit_deletions,
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(outcome.active_documents, 1);
    drop(database);

    let connection = Connection::open(&path).unwrap();
    let (bytes, start, end, text): (Vec<u8>, i64, i64, String) = connection
        .query_row(
            "SELECT cb.original_bytes, c.start_byte, c.end_byte, c.body_text
             FROM document_versions AS dv
             JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
             JOIN chunk_layouts AS cl ON cl.content_blob_id = cb.id
             JOIN chunks AS c ON c.chunk_layout_id = cl.id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(bytes, "a\nβ".as_bytes());
    assert_eq!((start, end, text.as_str()), (0, 4, "a\nβ"));
    drop(connection);
    Doctor::run(&path).unwrap();
}

#[test]
fn stable_id_preserves_document_identity_across_reordering_and_uri_changes() {
    let (_directory, path, mut database) = database();
    let scope = scope();
    let initial = prepare_jsonl_snapshot(
        br#"{"id":"b","source_uri":"runbook://b","content":"beta"}
{"id":"a","source_uri":"runbook://old","content":"alpha"}
"#,
    )
    .unwrap();
    database
        .apply_jsonl_snapshot(
            &scope,
            &initial.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let before = document_id(&path, b"a");

    let reordered = prepare_jsonl_snapshot(
        br#"{"id":"a","source_uri":"runbook://new","content":"alpha"}
{"id":"b","source_uri":"runbook://b","content":"beta"}
"#,
    )
    .unwrap();
    let changed = database
        .apply_jsonl_snapshot(
            &scope,
            &reordered.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(changed.changed_documents, 1);
    assert_eq!(document_id(&path, b"a"), before);
    assert_eq!(current_uri(&path, b"a"), "runbook://new");

    let unchanged = database
        .apply_jsonl_snapshot(
            &scope,
            &reordered.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(unchanged.generation_id, None);
    assert_eq!(unchanged.unchanged_documents, 2);
}

#[test]
fn explicit_tombstones_do_not_consume_the_absence_budget() {
    let (_directory, _path, mut database) = database();
    let scope = scope();
    let initial = prepare_jsonl_snapshot(
        br#"{"id":"a","source_uri":"u:a","content":"alpha"}
{"id":"b","source_uri":"u:b","content":"beta"}
"#,
    )
    .unwrap();
    database
        .apply_jsonl_snapshot(
            &scope,
            &initial.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();

    let deleted = prepare_jsonl_snapshot(
        br#"{"id":"a","source_uri":"u:a","content":"alpha"}
{"id":"b","source_uri":"u:b","deleted":true}
"#,
    )
    .unwrap();
    let plan = database
        .plan_jsonl_snapshot_with_timeout(
            &scope,
            &deleted.documents,
            &deleted.explicit_deletions,
            std::time::Duration::ZERO,
        )
        .unwrap();
    assert_eq!(plan.tombstoned_documents, 1);
    assert!(!plan.requires_mass_delete_confirmation);
    let outcome = database
        .apply_jsonl_snapshot(
            &scope,
            &deleted.documents,
            &deleted.explicit_deletions,
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(outcome.tombstoned_documents, 1);
    assert_eq!(outcome.active_documents, 1);
}

#[test]
fn absent_ids_are_authoritative_only_after_success_and_reuse_deletion_guards() {
    let (_directory, path, mut database) = database();
    let scope = scope();
    let initial = prepare_jsonl_snapshot(
        br#"{"id":"a","source_uri":"u:a","content":"a"}
{"id":"b","source_uri":"u:b","content":"b"}
{"id":"c","source_uri":"u:c","content":"c"}
{"id":"d","source_uri":"u:d","content":"d"}
"#,
    )
    .unwrap();
    database
        .apply_jsonl_snapshot(
            &scope,
            &initial.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();

    let malformed = br#"{"id":"a","source_uri":"u:a","content":"new"}
{"id":"broken","source_uri":"u:broken","content":}
"#;
    assert!(prepare_jsonl_snapshot(malformed).is_err());
    assert_eq!(
        active_documents(&path),
        4,
        "a valid prefix was never applied"
    );

    let quarter = prepare_jsonl_snapshot(
        br#"{"id":"a","source_uri":"u:a","content":"a"}
{"id":"b","source_uri":"u:b","content":"b"}
{"id":"c","source_uri":"u:c","content":"c"}
"#,
    )
    .unwrap();
    let outcome = database
        .apply_jsonl_snapshot(
            &scope,
            &quarter.documents,
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(outcome.tombstoned_documents, 1);

    let empty = prepare_jsonl_snapshot(b"\n  \r\n").unwrap();
    assert!(matches!(
        database.apply_jsonl_snapshot(
            &scope,
            &empty.documents,
            &empty.explicit_deletions,
            DeleteConfirmations::default(),
        ),
        Err(StoreError::EmptySnapshotConfirmationRequired)
    ));
    assert_eq!(active_documents(&path), 3);
}

#[test]
fn invalid_file_never_applies_a_valid_prefix_and_default_records_failure() {
    let (directory, path, mut database) = database();
    let snapshot_path = directory.path().join("records.jsonl");
    let (scope, config) = file_scope(&snapshot_path, ProjectId::new_v4(), "records");
    std::fs::write(&snapshot_path, live_line("a", "old")).unwrap();
    seed_file_source(&mut database, &scope, &config);
    let prior_epoch = index_epoch(&path);

    std::fs::write(
        &snapshot_path,
        b"{\"id\":\"a\",\"source_uri\":\"u:a\",\"content\":\"new\"}\n\
          {\"id\":\"broken\",\"source_uri\":\"u:broken\",\"content\":}\n",
    )
    .unwrap();
    assert!(
        ingest_jsonl_with_timeout(
            &mut database,
            &scope,
            &config,
            false,
            DeleteConfirmations::default(),
            Duration::ZERO,
        )
        .is_err()
    );

    assert_eq!(current_body(&path, b"a"), b"old");
    assert_eq!(index_epoch(&path), prior_epoch);
    assert_eq!(
        source_error(&path, scope.source_id).as_deref(),
        Some("SOURCE_JSONL_INVALID")
    );
    let detail_bytes: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT length(CAST(last_error_detail AS BLOB))
             FROM sources WHERE id = ?1",
            [scope.source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(detail_bytes <= 64 * 1024);
    Status::read(&path).unwrap();
}

#[test]
fn default_multi_source_ingest_commits_success_and_carries_failed_source_heads() {
    let (directory, path, mut database) = database();
    let project_id = ProjectId::new_v4();
    let good_path = directory.path().join("good.jsonl");
    let bad_path = directory.path().join("bad.jsonl");
    let (good_scope, good_config) = file_scope(&good_path, project_id, "good");
    let (bad_scope, bad_config) = file_scope(&bad_path, project_id, "bad");
    std::fs::write(&good_path, live_line("good", "old-good")).unwrap();
    std::fs::write(&bad_path, live_line("bad", "old-bad")).unwrap();
    seed_file_source(&mut database, &good_scope, &good_config);
    seed_file_source(&mut database, &bad_scope, &bad_config);
    let prior_epoch = index_epoch(&path);

    std::fs::write(&good_path, live_line("good", "new-good")).unwrap();
    std::fs::write(
        &bad_path,
        b"{\"id\":\"bad\",\"source_uri\":\"u:bad\",\"content\":}\n",
    )
    .unwrap();
    let outcome = ingest_jsonl_sources_with_timeout(
        &mut database,
        &[
            JsonlIngestTarget::new(&bad_scope, &bad_config),
            JsonlIngestTarget::new(&good_scope, &good_config),
        ],
        false,
        DeleteConfirmations::default(),
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(current_body(&path, b"good"), b"new-good");
    assert_eq!(current_body(&path, b"bad"), b"old-bad");
    assert_eq!(index_epoch(&path), prior_epoch + 1);
    assert_eq!(outcome.failed_documents, 1);
    assert_eq!(outcome.carried_forward_documents, 1);
    assert_eq!(outcome.source_outcomes.len(), 2);
    assert!(outcome.source_outcomes.iter().any(|source| {
        source.source_id == good_scope.source_id && source.state == SourceIngestState::Success
    }));
    assert!(outcome.source_outcomes.iter().any(|source| {
        source.source_id == bad_scope.source_id && source.state == SourceIngestState::Failed
    }));
    assert_eq!(
        source_error(&path, bad_scope.source_id).as_deref(),
        Some("SOURCE_JSONL_INVALID")
    );
}

#[test]
fn strict_multi_source_ingest_aborts_before_any_head_or_diagnostic_changes() {
    let (directory, path, mut database) = database();
    let project_id = ProjectId::new_v4();
    let good_path = directory.path().join("good.jsonl");
    let bad_path = directory.path().join("bad.jsonl");
    let (good_scope, good_config) = file_scope(&good_path, project_id, "good");
    let (bad_scope, bad_config) = file_scope(&bad_path, project_id, "bad");
    std::fs::write(&good_path, live_line("good", "old-good")).unwrap();
    std::fs::write(&bad_path, live_line("bad", "old-bad")).unwrap();
    seed_file_source(&mut database, &good_scope, &good_config);
    seed_file_source(&mut database, &bad_scope, &bad_config);
    let prior_epoch = index_epoch(&path);

    std::fs::write(&good_path, live_line("good", "new-good")).unwrap();
    std::fs::write(
        &bad_path,
        b"{\"id\":\"bad\",\"source_uri\":\"u:bad\",\"content\":}\n",
    )
    .unwrap();
    let result = ingest_jsonl_sources_with_timeout(
        &mut database,
        &[
            JsonlIngestTarget::new(&good_scope, &good_config),
            JsonlIngestTarget::new(&bad_scope, &bad_config),
        ],
        true,
        DeleteConfirmations::default(),
        Duration::ZERO,
    );

    assert!(matches!(
        result,
        Err(JsonlBatchIngestError::StrictSource { .. })
    ));
    assert_eq!(current_body(&path, b"good"), b"old-good");
    assert_eq!(current_body(&path, b"bad"), b"old-bad");
    assert_eq!(index_epoch(&path), prior_epoch);
    assert_eq!(source_error(&path, good_scope.source_id), None);
    assert_eq!(source_error(&path, bad_scope.source_id), None);
}

#[test]
fn successful_multi_source_batch_uses_one_generation_and_one_epoch_switch() {
    let (directory, path, mut database) = database();
    let project_id = ProjectId::new_v4();
    let first_path = directory.path().join("first.jsonl");
    let second_path = directory.path().join("second.jsonl");
    let (first_scope, first_config) = file_scope(&first_path, project_id, "first");
    let (second_scope, second_config) = file_scope(&second_path, project_id, "second");
    std::fs::write(&first_path, live_line("first", "old-first")).unwrap();
    std::fs::write(&second_path, live_line("second", "old-second")).unwrap();
    seed_file_source(&mut database, &first_scope, &first_config);
    seed_file_source(&mut database, &second_scope, &second_config);
    let prior_epoch = index_epoch(&path);

    std::fs::write(&first_path, live_line("first", "new-first")).unwrap();
    std::fs::write(&second_path, live_line("second", "new-second")).unwrap();
    let outcome = ingest_jsonl_sources_with_timeout(
        &mut database,
        &[
            JsonlIngestTarget::new(&second_scope, &second_config),
            JsonlIngestTarget::new(&first_scope, &first_config),
        ],
        true,
        DeleteConfirmations::default(),
        Duration::ZERO,
    )
    .unwrap();

    let generation_id = outcome
        .generation_id
        .expect("both source edits create one generation");
    assert_eq!(outcome.changed_documents, 2);
    assert_eq!(outcome.index_epoch, prior_epoch + 1);
    assert_eq!(head_generation(&path, b"first"), generation_id);
    assert_eq!(head_generation(&path, b"second"), generation_id);
}

#[test]
fn jsonl_get_uses_immutable_bytes_and_reports_snapshot_only_verification() {
    let (directory, path, mut database) = database();
    let snapshot_path = directory.path().join("evidence.jsonl");
    let (scope, config) = file_scope(&snapshot_path, ProjectId::new_v4(), "evidence");
    std::fs::write(
        &snapshot_path,
        live_line("evidence", "immutable needle bytes"),
    )
    .unwrap();
    seed_file_source(&mut database, &scope, &config);
    let search = database
        .search(
            scope.project_id,
            &SearchRequest::with_defaults("needle").unwrap(),
        )
        .unwrap();
    let citation = search.results[0].citation();
    drop(database);

    let outcome = GetEvidence::execute(&GetEvidenceRequest {
        index_path: path,
        request: GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16 * 1024,
        },
        verify_source_hash: true,
        probe_deadline: std::time::Instant::now() + Duration::from_secs(1),
        field_limits: GetEvidenceFieldLimits::CLI,
        connection_observer: None,
    })
    .unwrap();

    assert_eq!(outcome.evidence.content, b"immutable needle bytes");
    assert_eq!(outcome.source_state, EvidenceSourceState::SnapshotOnly);
    assert_eq!(
        outcome.source_hash_verification,
        SourceHashVerification::SnapshotOnly
    );
}

#[test]
fn jsonl_only_status_preserves_the_configured_index_quota() {
    let (directory, path, mut database) = database();
    let snapshot_path = directory.path().join("quota.jsonl");
    let config = JsonlSourceConfig::new(snapshot_path.clone())
        .unwrap()
        .with_index_quota_bytes(Some(512 * 1024 * 1024))
        .unwrap();
    let scope = JsonlScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new("quota").unwrap(),
        source_logical_uri: snapshot_path.to_str().unwrap().to_owned(),
        source_config_json: config.to_canonical_json().unwrap(),
        project_id: ProjectId::new_v4(),
        project_name: SafeSlug::new("default").unwrap(),
    };
    std::fs::write(&snapshot_path, live_line("quota", "quota evidence")).unwrap();
    seed_file_source(&mut database, &scope, &config);
    drop(database);

    assert_eq!(
        Status::read(&path).unwrap().index_quota_bytes,
        Some(512 * 1024 * 1024)
    );
}

#[test]
fn jsonl_quota_refusal_precedes_scope_and_generation_mutation() {
    let (directory, path, mut database) = database();
    let snapshot_path = directory.path().join("over-quota.jsonl");
    let config = JsonlSourceConfig::new(snapshot_path.clone())
        .unwrap()
        .with_index_quota_bytes(Some(1))
        .unwrap();
    let scope = JsonlScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new("over-quota").unwrap(),
        source_logical_uri: snapshot_path.to_str().unwrap().to_owned(),
        source_config_json: config.to_canonical_json().unwrap(),
        project_id: ProjectId::new_v4(),
        project_name: SafeSlug::new("default").unwrap(),
    };
    std::fs::write(&snapshot_path, live_line("quota", "bounded evidence")).unwrap();

    let error = ingest_jsonl_with_timeout(
        &mut database,
        &scope,
        &config,
        false,
        DeleteConfirmations::default(),
        Duration::ZERO,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        JsonlFileIngestError::StoragePreflight(StoragePreflightError::QuotaExceeded { .. })
    ));
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(index_epoch(&path), 0);
}

fn file_scope(
    path: &std::path::Path,
    project_id: ProjectId,
    name: &str,
) -> (JsonlScope, JsonlSourceConfig) {
    let config = JsonlSourceConfig::new(path.to_path_buf()).unwrap();
    let scope = JsonlScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new(name).unwrap(),
        source_logical_uri: path.to_str().unwrap().to_owned(),
        source_config_json: config.to_canonical_json().unwrap(),
        project_id,
        project_name: SafeSlug::new("default").unwrap(),
    };
    (scope, config)
}

fn seed_file_source(database: &mut IndexDb, scope: &JsonlScope, config: &JsonlSourceConfig) {
    ingest_jsonl_with_timeout(
        database,
        scope,
        config,
        false,
        DeleteConfirmations::default(),
        Duration::ZERO,
    )
    .unwrap();
}

fn live_line(id: &str, content: &str) -> Vec<u8> {
    format!("{{\"id\":\"{id}\",\"source_uri\":\"u:{id}\",\"content\":\"{content}\"}}\n")
        .into_bytes()
}

fn document_id(path: &std::path::Path, key: &[u8]) -> Vec<u8> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT id FROM documents WHERE connector_key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn current_uri(path: &std::path::Path, key: &[u8]) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT current_source_uri FROM documents WHERE connector_key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn active_documents(path: &std::path::Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn current_body(path: &std::path::Path, key: &[u8]) -> Vec<u8> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT cb.original_bytes
             FROM documents AS d
             JOIN document_heads AS dh ON dh.document_id = d.id
             JOIN document_versions AS dv ON dv.id = dh.document_version_id
             JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
             WHERE d.connector_key = ?1 AND dh.state = 'active'",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn index_epoch(path: &std::path::Path) -> u64 {
    let value: Vec<u8> = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'index_epoch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    std::str::from_utf8(&value).unwrap().parse().unwrap()
}

fn source_error(path: &std::path::Path, source_id: SourceId) -> Option<String> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT last_error_code FROM sources WHERE id = ?1",
            [source_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .unwrap()
}

fn head_generation(path: &std::path::Path, key: &[u8]) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT dh.generation_id
             FROM documents AS d
             JOIN document_heads AS dh ON dh.document_id = d.id
             WHERE d.connector_key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}
