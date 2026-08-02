use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use hsum::domain::{ByteSpan, Citation, IndexId, LineSpan, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{QuoteBloom, SnapshotRevision, body_sha256, revision_sha256};
use hsum::search::{GetError, GetRequest};
use hsum::store::{
    DeleteConfirmations, Doctor, FilesystemScope, ForgetPlan, IndexDb, MaintenanceError, OpenMode,
    PlanEnvelope, PreparedChunk, PreparedDocument, PrunePlan, RestorePlan, SCHEMA_VERSION,
    StoreError, apply_forget, apply_forget_with_observer, apply_migration, apply_prune,
    apply_restore, create_backup, pipeline_fingerprint, plan_forget, plan_migration, plan_prune,
    prepare_passage_literals, read_plan, schema_checksum, write_plan,
};
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

mod support;
use support::private_tempdir as tempdir;

const INDEX_ID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
const FORGET_FAULT_CHECKPOINT: &str = "HSUM_TEST_FORGET_CHECKPOINT";
const FORGET_FAULT_INDEX: &str = "HSUM_TEST_FORGET_INDEX";
const FORGET_FAULT_PLAN: &str = "HSUM_TEST_FORGET_PLAN";
const FORGET_FAULT_RECOVERY: &str = "HSUM_TEST_FORGET_RECOVERY";
const FORGET_FAULT_RESTORE_PLAN: &str = "HSUM_TEST_FORGET_RESTORE_PLAN";
const FORGET_FAULT_EXIT: i32 = 87;

fn future_cutoff() -> OffsetDateTime {
    OffsetDateTime::parse("2099-01-01T00:00:00Z", &Rfc3339).unwrap()
}

#[test]
fn backup_is_verified_and_preserves_the_live_index_identity() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let backup = directory.path().join("backup.sqlite");
    let mut database = create_database(&index);
    database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &[prepared_document(b"notes.md", b"backup evidence\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let receipt = create_backup(&index, &backup, Duration::from_secs(1)).unwrap();

    assert_eq!(receipt.index_id.to_string(), INDEX_ID);
    assert_eq!(receipt.schema_version, SCHEMA_VERSION);
    assert_eq!(receipt.index_epoch, 1);
    assert!(receipt.file_bytes > 0);
    assert_eq!(Doctor::run(&backup).unwrap().index_id, receipt.index_id);
    assert!(matches!(
        create_backup(&index, &backup, Duration::from_secs(1)),
        Err(MaintenanceError::OutputExists(_))
    ));
}

#[test]
fn prune_plan_enumerates_citation_impact_and_apply_keeps_a_valid_baseline() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let plan_path = directory.path().join("prune.json");
    let backup = directory.path().join("before-prune.sqlite");
    let scope = fixture_scope();
    let mut database = create_database(&index);
    let old = prepared_document(b"notes.md", b"old evidence\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&old),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"new evidence\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let plan = plan_prune(&index, future_cutoff(), 1, Duration::from_secs(1)).unwrap();
    assert_eq!(plan.plan.index_epoch, 2);
    assert_eq!(plan.plan.generations_removed, 1);
    assert_eq!(plan.plan.affected_revisions.len(), 1);
    assert_eq!(plan.plan.affected_citation_count, 1);
    let impact = &plan.plan.affected_revisions[0];
    assert_eq!(impact.revision_sha256, old.revision_sha256);
    assert!(impact.canonical_stored_chunk_citations[0].contains("#bytes=0-13"));
    write_plan(&plan_path, &plan).unwrap();
    let loaded: PlanEnvelope<PrunePlan> = read_plan(&plan_path).unwrap();
    assert_eq!(loaded, plan);

    let outcome = apply_prune(
        &index,
        &loaded,
        loaded.plan_hash,
        &backup,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(outcome.affected_revisions, 1);
    assert_eq!(outcome.history_floor_epoch, 2);
    Doctor::run(&index).unwrap();
    Doctor::run(&backup).unwrap();

    let connection = Connection::open(&index).unwrap();
    assert_eq!(count(&connection, "document_versions"), 1);
    assert_eq!(count(&connection, "generations"), 1);
    assert_eq!(count(&connection, "prune_runs"), 1);
    drop(connection);

    let mut database = IndexDb::open_existing(&index, OpenMode::ReadWrite).unwrap();
    let next = database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"newest evidence\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(next.index_epoch, 3);
    drop(database);
    Doctor::run(&index).unwrap();
}

#[test]
fn prune_requires_both_the_cutoff_and_per_document_retention_conditions() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let backup = directory.path().join("before-prune.sqlite");
    let scope = fixture_scope();
    let mut database = create_database(&index);
    let oldest = prepared_document(b"notes.md", b"oldest evidence\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&oldest),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let oldest_citation = active_citation(&index, &scope, &oldest);
    let middle = prepared_document(b"notes.md", b"middle evidence\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&middle),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let middle_citation = active_citation(&index, &scope, &middle);
    let newest = prepared_document(b"notes.md", b"newest evidence\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&newest),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let no_age_match = plan_prune(
        &index,
        OffsetDateTime::parse("2000-01-01T00:00:00Z", &Rfc3339).unwrap(),
        1,
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(no_age_match.plan.affected_revisions.is_empty());
    assert!(no_age_match.plan.affected_generations.is_empty());

    let plan = plan_prune(&index, future_cutoff(), 2, Duration::from_secs(1)).unwrap();
    assert_eq!(plan.plan.keep_latest, 2);
    assert_eq!(plan.plan.affected_revisions.len(), 1);
    assert_eq!(
        plan.plan.affected_revisions[0].revision_sha256,
        oldest.revision_sha256
    );
    assert_eq!(plan.plan.affected_generations.len(), 2);
    let first = apply_prune(
        &index,
        &plan,
        plan.plan_hash,
        &backup,
        Duration::from_secs(1),
    )
    .unwrap();
    let retried = apply_prune(
        &index,
        &plan,
        plan.plan_hash,
        &backup,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(retried, first);

    let database = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        database.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: oldest_citation,
            max_bytes: 16 * 1024,
        }),
        Err(GetError::EvidenceNotFound)
    ));
    assert!(
        database
            .get_evidence(&GetRequest {
                project_id: scope.project_id,
                citation: middle_citation,
                max_bytes: 16 * 1024,
            })
            .is_ok()
    );
}

#[test]
fn prune_apply_rejects_a_stale_plan_before_creating_a_backup() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let backup = directory.path().join("should-not-exist.sqlite");
    let scope = fixture_scope();
    let mut database = create_database(&index);
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"one\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"two\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let plan = plan_prune(&index, future_cutoff(), 1, Duration::from_secs(1)).unwrap();
    let mut database = IndexDb::open_existing(&index, OpenMode::ReadWrite).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"three\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    assert!(matches!(
        apply_prune(
            &index,
            &plan,
            plan.plan_hash,
            &backup,
            Duration::from_secs(1)
        ),
        Err(MaintenanceError::PlanStale)
    ));
    assert!(!backup.exists());
}

#[test]
fn prune_removes_abandoned_history_from_an_empty_index_without_zeroing_the_floor() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let backup = directory.path().join("empty-backup.sqlite");
    drop(create_database(&index));
    let connection = Connection::open(&index).unwrap();
    connection
        .execute(
            "INSERT INTO generations(
                 id, state, created_at, pipeline_fingerprint
             ) VALUES (1, 'abandoned', '2026-08-01T00:00:00Z', ?1)",
            [pipeline_fingerprint().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    Doctor::run(&index).unwrap();

    let plan = plan_prune(&index, future_cutoff(), 1, Duration::from_secs(1)).unwrap();
    assert_eq!(plan.plan.index_epoch, 0);
    assert_eq!(plan.plan.history_floor_epoch, 1);
    assert_eq!(plan.plan.generations_removed, 1);
    let outcome = apply_prune(
        &index,
        &plan,
        plan.plan_hash,
        &backup,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(outcome.history_floor_epoch, 1);
    Doctor::run(&index).unwrap();
    let connection = Connection::open(&index).unwrap();
    assert_eq!(count(&connection, "generations"), 0);
}

#[test]
fn released_n_minus_one_fixture_migrates_only_after_plan_hash_confirmation() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let backup = directory.path().join("schema-3.sqlite");
    drop(create_database(&index));
    downgrade_to_schema_3(&index);

    let plan = plan_migration(&index, "fixture", Duration::from_secs(1)).unwrap();
    assert_eq!(plan.plan.from_schema_version, 3);
    assert_eq!(plan.plan.to_schema_version, 4);
    assert_eq!(plan.plan.steps.len(), 1);
    assert_eq!(plan.plan.steps[0].version, 4);
    let wrong = schema_checksum();
    assert!(matches!(
        apply_migration(
            &index,
            "fixture",
            &plan,
            wrong,
            &backup,
            Duration::from_secs(1)
        ),
        Err(MaintenanceError::ConfirmationMismatch)
    ));
    assert!(!backup.exists());

    let outcome = apply_migration(
        &index,
        "fixture",
        &plan,
        plan.plan_hash,
        &backup,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(outcome.from_schema_version, 3);
    assert_eq!(outcome.to_schema_version, 4);
    Doctor::run(&index).unwrap();
    let old = Connection::open(&backup).unwrap();
    assert_eq!(
        old.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        3
    );
}

#[test]
fn forget_physically_rewrites_tombstones_copies_and_restores_exact_state() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let recovery = directory.path().join("recovery.sqlite");
    let restore_plan_path = directory.path().join("restore.json");
    let safety = directory.path().join("safety.sqlite");
    let forgotten_copy = directory.path().join("forgotten-copy.sqlite");
    let scope = fixture_scope();
    let document = prepared_document(b"notes.md", b"FORGET_PHYSICAL_SENTINEL_7d8611f953ed\n");
    let mut database = create_database(&index);
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let citation = active_citation(&index, &scope, &document);
    let old_reader = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();

    let plan = plan_forget(
        &index,
        scope.project_id,
        std::slice::from_ref(&citation),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(plan.plan.affected_revisions.len(), 1);
    let outcome = apply_forget(
        &index,
        &plan,
        plan.plan_hash,
        &recovery,
        &restore_plan_path,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(outcome.replacement_epoch, 1);
    assert!(old_reader.verify_live_identity().is_err());
    assert!(
        !fs::read(&index)
            .unwrap()
            .windows(b"FORGET_PHYSICAL_SENTINEL_7d8611f953ed".len())
            .any(|window| window == b"FORGET_PHYSICAL_SENTINEL_7d8611f953ed")
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!Path::new(&format!("{}{}", index.display(), suffix)).exists());
    }
    Doctor::run(&index).unwrap();
    Doctor::run(&recovery).unwrap();

    let forgotten = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        forgotten.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: citation.clone(),
            max_bytes: 16 * 1024,
        }),
        Err(GetError::EvidenceForgotten)
    ));
    drop(forgotten);
    create_backup(&index, &forgotten_copy, Duration::from_secs(1)).unwrap();
    let copied = IndexDb::open_existing(&forgotten_copy, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        copied.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: citation.clone(),
            max_bytes: 16 * 1024,
        }),
        Err(GetError::EvidenceForgotten)
    ));
    drop(copied);

    let mut forgotten = IndexDb::open_existing(&index, OpenMode::ReadWrite).unwrap();
    assert!(matches!(
        forgotten.apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        ),
        Err(StoreError::ForgetTombstone)
    ));
    drop(forgotten);

    let restore_plan: PlanEnvelope<RestorePlan> = read_plan(&restore_plan_path).unwrap();
    let restored = apply_restore(
        &index,
        &restore_plan,
        restore_plan.plan_hash,
        &recovery,
        &safety,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(restored.replacement_epoch, 2);
    let recovered_restore = apply_restore(
        &index,
        &restore_plan,
        restore_plan.plan_hash,
        &recovery,
        &safety,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(recovered_restore.safety_backup, restored.safety_backup);
    Doctor::run(&index).unwrap();
    Doctor::run(&safety).unwrap();
    let restored_database = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();
    let evidence = restored_database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16 * 1024,
        })
        .unwrap();
    assert_eq!(evidence.content, document.body);
}

#[test]
fn forget_suppresses_new_revisions_and_restore_refuses_other_state_changes() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let recovery = directory.path().join("recovery.sqlite");
    let restore_plan_path = directory.path().join("restore.json");
    let safety = directory.path().join("must-not-exist.sqlite");
    let scope = fixture_scope();
    let original = prepared_document(b"notes.md", b"restore guard original\n");
    let mut database = create_database(&index);
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&original),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let citation = active_citation(&index, &scope, &original);
    let plan = plan_forget(
        &index,
        scope.project_id,
        &[citation],
        Duration::from_secs(1),
    )
    .unwrap();
    apply_forget(
        &index,
        &plan,
        plan.plan_hash,
        &recovery,
        &restore_plan_path,
        Duration::from_secs(1),
    )
    .unwrap();
    let mut database = IndexDb::open_existing(&index, OpenMode::ReadWrite).unwrap();
    assert!(matches!(
        database.apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"notes.md", b"later distinct revision\n")],
            &[],
            DeleteConfirmations::default(),
        ),
        Err(StoreError::ForgetTombstone)
    ));
    database
        .apply_filesystem_snapshot(
            &scope,
            &[prepared_document(b"other.md", b"unrelated state change\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let restore_plan: PlanEnvelope<RestorePlan> = read_plan(&restore_plan_path).unwrap();
    assert!(matches!(
        apply_restore(
            &index,
            &restore_plan,
            restore_plan.plan_hash,
            &recovery,
            &safety,
            Duration::from_secs(1),
        ),
        Err(MaintenanceError::RestoreStateMismatch)
    ));
    assert!(!safety.exists());
}

#[test]
fn replaying_the_pre_forget_backup_cannot_reenable_forgotten_evidence() {
    let directory = tempdir().unwrap();
    let index = directory.path().join("index.sqlite");
    let recovery = directory.path().join("recovery.sqlite");
    let replay = directory.path().join("replayed.sqlite");
    let restore_plan_path = directory.path().join("restore.json");
    let scope = fixture_scope();
    let document = prepared_document(b"notes.md", b"backup replay secret\n");
    let mut database = create_database(&index);
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let citation = active_citation(&index, &scope, &document);
    let plan = plan_forget(
        &index,
        scope.project_id,
        std::slice::from_ref(&citation),
        Duration::from_secs(1),
    )
    .unwrap();
    apply_forget(
        &index,
        &plan,
        plan.plan_hash,
        &recovery,
        &restore_plan_path,
        Duration::from_secs(1),
    )
    .unwrap();

    fs::copy(&recovery, &replay).unwrap();
    fs::rename(&replay, &index).unwrap();

    assert!(matches!(
        Doctor::run(&index),
        Err(StoreError::ForgetLedgerMismatch)
    ));
    let database = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        database.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16 * 1024,
        }),
        Err(GetError::EvidenceForgotten)
    ));
}

#[test]
fn physical_forget_crash_boundaries_leave_one_doctor_valid_database() {
    for checkpoint in [
        "ledger-prepared",
        "recovery-backup-created",
        "replacement-prepared",
        "old-readers-fenced",
        "replacement-published",
        "forget-committed",
    ] {
        let directory = tempdir().unwrap();
        let index = directory.path().join("index.sqlite");
        let plan_path = directory.path().join("forget.json");
        let recovery = directory.path().join("recovery.sqlite");
        let restore_plan = directory.path().join("restore.json");
        let scope = fixture_scope();
        let document = prepared_document(b"notes.md", b"forget crash boundary\n");
        let mut database = create_database(&index);
        database
            .apply_filesystem_snapshot(
                &scope,
                std::slice::from_ref(&document),
                &[],
                DeleteConfirmations::default(),
            )
            .unwrap();
        drop(database);
        let citation = active_citation(&index, &scope, &document);
        let plan = plan_forget(
            &index,
            scope.project_id,
            std::slice::from_ref(&citation),
            Duration::from_secs(1),
        )
        .unwrap();
        write_plan(&plan_path, &plan).unwrap();

        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("forget_fault_helper")
            .arg("--nocapture")
            .env(FORGET_FAULT_CHECKPOINT, checkpoint)
            .env(FORGET_FAULT_INDEX, &index)
            .env(FORGET_FAULT_PLAN, &plan_path)
            .env(FORGET_FAULT_RECOVERY, &recovery)
            .env(FORGET_FAULT_RESTORE_PLAN, &restore_plan)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(FORGET_FAULT_EXIT));
        Doctor::run(&index).unwrap();
        let database = IndexDb::open_existing(&index, OpenMode::ReadOnly).unwrap();
        let result = database.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16 * 1024,
        });
        assert!(matches!(result, Err(GetError::EvidenceForgotten)));
        drop(database);
        let mut database = IndexDb::open_existing(&index, OpenMode::ReadWrite).unwrap();
        assert!(matches!(
            database.apply_filesystem_snapshot(
                &scope,
                &[prepared_document(
                    b"notes.md",
                    b"new body after interrupted forget\n",
                )],
                &[],
                DeleteConfirmations::default(),
            ),
            Err(StoreError::ForgetTombstone)
        ));
        drop(database);

        let recovered = apply_forget(
            &index,
            &plan,
            plan.plan_hash,
            &recovery,
            &restore_plan,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(recovered.plan_hash, plan.plan_hash);
        assert_eq!(recovered.replacement_epoch, 1);
        Doctor::run(&index).unwrap();
    }
}

#[test]
fn forget_fault_helper() {
    let Ok(checkpoint) = std::env::var(FORGET_FAULT_CHECKPOINT) else {
        return;
    };
    let index = std::env::var_os(FORGET_FAULT_INDEX).unwrap();
    let plan_path = std::env::var_os(FORGET_FAULT_PLAN).unwrap();
    let recovery = std::env::var_os(FORGET_FAULT_RECOVERY).unwrap();
    let restore_plan = std::env::var_os(FORGET_FAULT_RESTORE_PLAN).unwrap();
    let plan: PlanEnvelope<ForgetPlan> = read_plan(Path::new(&plan_path)).unwrap();
    let result = apply_forget_with_observer(
        Path::new(&index),
        &plan,
        plan.plan_hash,
        Path::new(&recovery),
        Path::new(&restore_plan),
        Duration::from_secs(1),
        |observed| {
            if observed == checkpoint {
                std::process::exit(FORGET_FAULT_EXIT);
            }
        },
    );
    panic!("forget returned before fault checkpoint {checkpoint}: {result:?}");
}

fn create_database(path: &Path) -> IndexDb {
    IndexDb::create(path, INDEX_ID.parse::<IndexId>().unwrap()).unwrap()
}

fn active_citation(path: &Path, scope: &FilesystemScope, document: &PreparedDocument) -> Citation {
    let connection = Connection::open(path).unwrap();
    let document_id: Vec<u8> = connection
        .query_row(
            "SELECT id FROM documents WHERE source_id = ?1 AND connector_key = ?2",
            params![
                scope.source_id.as_uuid().as_bytes().as_slice(),
                document.connector_key,
            ],
            |row| row.get(0),
        )
        .unwrap();
    Citation {
        index_id: INDEX_ID.parse().unwrap(),
        source_id: scope.source_id,
        document_id: hsum::domain::DocumentId::from_uuid(Uuid::from_slice(&document_id).unwrap()),
        revision: document.revision_sha256,
        span: document.chunks[0].byte_span,
    }
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

fn prepared_document(connector_key: &[u8], body: &[u8]) -> PreparedDocument {
    let title = std::str::from_utf8(connector_key).unwrap();
    let source_uri = format!("repo://{title}");
    let metadata = json!({});
    let revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri: &source_uri,
        title,
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.clone(),
        title: title.to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256: revision,
        chunker_fingerprint: hsum::store::chunker_fingerprint(hsum::ingest::ChunkKind::Markdown),
        chunks: vec![PreparedChunk {
            ordinal: 0,
            byte_span: ByteSpan::new(0, body.len() as u64).unwrap(),
            line_span: LineSpan::new(1, 1).unwrap(),
            body_text: std::str::from_utf8(body).unwrap().to_owned(),
            content_sha256: body_sha256(body),
            quote_bloom: QuoteBloom::from_content(body).into_bytes(),
            literals: prepare_passage_literals(title, &source_uri, body),
        }],
    }
}

fn downgrade_to_schema_3(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute_batch(
            "DROP TABLE passages_vec_a;
             DROP TABLE passages_vec_b;
             DROP TABLE chunk_embeddings;
             DROP TABLE embedding_provenance;
             DELETE FROM index_meta
             WHERE key IN (
                 'embedding_model_id',
                 'embedding_revision',
                 'embedding_model_fingerprint',
                 'embedding_dimension',
                 'active_vector_slot'
             );
             DELETE FROM schema_migrations WHERE version = 4;",
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA writable_schema = ON;")
        .unwrap();
    connection
        .execute(
            "DELETE FROM sqlite_schema WHERE name = 'sqlite_sequence'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sqlite_schema SET sql = ?1
             WHERE type = 'table' AND name = 'generations'",
            [released_generation_sql()],
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA writable_schema = RESET;")
        .unwrap();
    connection.execute_batch("VACUUM;").unwrap();
    let checksum = schema_3_checksum();
    connection
        .execute(
            "UPDATE index_meta SET value = CAST('3' AS BLOB) WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'schema_checksum'",
            params![checksum.as_slice()],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    assert_eq!(schema_rows(&connection), released_schema_3_rows());
}

fn schema_3_checksum() -> [u8; 32] {
    let migrations = [
        (1_u32, include_str!("../migrations/0001_alpha1.sql")),
        (2_u32, include_str!("../migrations/0002_jsonl_sources.sql")),
        (3_u32, include_str!("../migrations/0003_maintenance.sql")),
    ];
    let mut hasher = Sha256::new();
    hasher.update(b"hsum.schema-chain.v1\0");
    for (version, sql) in migrations {
        hasher.update(version.to_be_bytes());
        hasher.update((sql.len() as u64).to_be_bytes());
        hasher.update(sql.as_bytes());
    }
    hasher.finalize().into()
}

fn released_schema_3_rows() -> Vec<(String, String, String, String)> {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_alpha1.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0002_jsonl_sources.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0003_maintenance.sql"))
        .unwrap();
    schema_rows(&connection)
}

fn released_generation_sql() -> String {
    released_schema_3_rows()
        .into_iter()
        .find_map(|(object_type, name, _, sql)| {
            (object_type == "table" && name == "generations").then_some(sql)
        })
        .unwrap()
}

fn schema_rows(connection: &Connection) -> Vec<(String, String, String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_schema ORDER BY type, name, tbl_name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
