use std::fs;
use std::time::Duration;

use hsum::domain::{
    ByteSpan, Citation, DocumentId, IndexId, LineSpan, ProjectId, SafeSlug, Sha256Digest, SourceId,
};
use hsum::ingest::{QuoteBloom, SnapshotRevision, body_sha256, revision_sha256};
use hsum::model::{IndexModelState, ModelArtifactState, builtin_manifests, discover_model_pins};
use hsum::search::{Retriever, SearchError, SearchMode, SearchRequest, SearchStopReason};
use hsum::store::{
    DeleteConfirmations, Doctor, EMBEDDING_DIMENSION, EmbeddingCacheOutcome, EmbeddingModelPin,
    EmbeddingProvenanceRecord, FilesystemScope, IndexDb, IndexEmbeddingProfile, OpenMode,
    PlanEnvelope, PreparedChunk, PreparedChunkEmbedding, PreparedDocument, RestorePlan,
    SCHEMA_VERSION, StoreError, WriterLock, apply_forget, apply_prune, apply_restore,
    create_backup, pipeline_fingerprint, pipeline_fingerprint_for, plan_forget, plan_prune,
    prepare_embedding_input, prepare_passage_literals, read_plan,
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::{TempDir, tempdir};
use time::OffsetDateTime;
use uuid::Uuid;

fn private_tempdir() -> TempDir {
    let directory = tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

fn fixture_pin() -> EmbeddingModelPin {
    EmbeddingModelPin::new(
        "bge-small-en-v1-5-fp32",
        "9f78e9edb8a1f157b5840bf59a6674c69b37f5f6",
        "c7ca8f3bc43084589f39556f58ef5bc3da372435d4ca7c4310e4d90286264f66"
            .parse::<Sha256Digest>()
            .unwrap(),
        EMBEDDING_DIMENSION,
    )
    .unwrap()
}

#[test]
fn fresh_lexical_index_has_empty_native_vector_storage() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let index_id = IndexId::new_v4();

    let database = IndexDb::create(&path, index_id).unwrap();
    assert_eq!(SCHEMA_VERSION, 4);
    assert_eq!(
        database.embedding_profile().unwrap(),
        IndexEmbeddingProfile::LexicalOnly
    );
    drop(database);

    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "v0.1.7"
    );
    for table in [
        "embedding_provenance",
        "chunk_embeddings",
        "passages_vec_a",
        "passages_vec_b",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing {table}");
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }
    assert_eq!(metadata(&connection, "embedding_profile"), b"none".to_vec());
    assert_eq!(metadata(&connection, "embedding_dimension"), b"0".to_vec());
    assert_eq!(metadata(&connection, "active_vector_slot"), b"0".to_vec());
}

#[test]
fn pinned_profile_is_exact_immutable_and_reopens() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let pin = fixture_pin();
    let profile = IndexEmbeddingProfile::Pinned(pin.clone());

    let database =
        IndexDb::create_with_embedding_profile(&path, IndexId::new_v4(), &profile).unwrap();
    assert_eq!(database.embedding_profile().unwrap(), profile);
    drop(database);

    let reopened = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert_eq!(reopened.embedding_profile().unwrap(), profile);
    drop(reopened);

    let connection = Connection::open(&path).unwrap();
    assert_eq!(metadata(&connection, "embedding_profile"), b"pinned");
    assert_eq!(
        metadata(&connection, "embedding_model_id"),
        pin.model_id().as_bytes()
    );
    assert_eq!(
        metadata(&connection, "embedding_revision"),
        pin.upstream_revision().as_bytes()
    );
    assert_eq!(
        metadata(&connection, "embedding_model_fingerprint"),
        pin.model_fingerprint().as_bytes()
    );
    assert_eq!(
        metadata(&connection, "embedding_dimension"),
        EMBEDDING_DIMENSION.to_string().as_bytes()
    );
    assert_eq!(
        metadata(&connection, "pipeline_fingerprint"),
        pipeline_fingerprint_for(&profile).as_bytes()
    );
    assert_ne!(pipeline_fingerprint_for(&profile), pipeline_fingerprint());
}

#[test]
fn model_pin_rejects_noncanonical_or_unsupported_identity() {
    let fingerprint = Sha256Digest::of_bytes(b"model");
    for (id, revision, dimension) in [
        (
            "BGE-SMALL",
            "9f78e9edb8a1f157b5840bf59a6674c69b37f5f6",
            EMBEDDING_DIMENSION,
        ),
        ("bge-small", "main", EMBEDDING_DIMENSION),
        (
            "bge-small",
            "9f78e9edb8a1f157b5840bf59a6674c69b37f5f6",
            EMBEDDING_DIMENSION + 1,
        ),
    ] {
        assert!(EmbeddingModelPin::new(id, revision, fingerprint, dimension).is_err());
    }
}

#[test]
fn five_state_model_lifecycle_is_derived_from_durable_facts() {
    let lexical = IndexEmbeddingProfile::LexicalOnly;
    let pinned = IndexEmbeddingProfile::Pinned(fixture_pin());
    assert_eq!(
        IndexModelState::derive(&lexical, ModelArtifactState::Missing, false),
        IndexModelState::LexicalOnly
    );
    assert_eq!(
        IndexModelState::derive(&pinned, ModelArtifactState::Missing, false),
        IndexModelState::ConfiguredUninstalled
    );
    assert_eq!(
        IndexModelState::derive(&pinned, ModelArtifactState::Installed, false),
        IndexModelState::InstalledUnindexed
    );
    assert_eq!(
        IndexModelState::derive(&pinned, ModelArtifactState::Installed, true),
        IndexModelState::Indexed
    );
    assert_eq!(
        IndexModelState::derive(&pinned, ModelArtifactState::Missing, true),
        IndexModelState::DegradedMissing
    );
    assert_eq!(
        IndexModelState::derive_with_history(&pinned, ModelArtifactState::Missing, false, true,),
        IndexModelState::DegradedMissing
    );
}

#[test]
fn model_pin_discovery_reads_the_exact_structured_index_profile() {
    let directory = private_tempdir();
    let data_dir = directory.path().join("data");
    let indexes = data_dir.join("indexes");
    let pinned_dir = indexes.join("semantic");
    let lexical_dir = indexes.join("lexical");
    fs::create_dir_all(&pinned_dir).unwrap();
    fs::create_dir_all(&lexical_dir).unwrap();
    #[cfg(unix)]
    for path in [&pinned_dir, &lexical_dir] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let manifest = &builtin_manifests()[0];
    let pin = EmbeddingModelPin::new(
        manifest.id.clone(),
        manifest.upstream_revision.clone(),
        manifest.fingerprint().unwrap(),
        manifest.dimension,
    )
    .unwrap();
    drop(
        IndexDb::create_with_embedding_profile(
            &pinned_dir.join("index.sqlite"),
            IndexId::new_v4(),
            &IndexEmbeddingProfile::Pinned(pin),
        )
        .unwrap(),
    );
    drop(IndexDb::create(&lexical_dir.join("index.sqlite"), IndexId::new_v4()).unwrap());

    assert_eq!(
        discover_model_pins(&data_dir, manifest).unwrap(),
        vec!["semantic".to_owned()]
    );
}

#[test]
fn provenance_separates_exact_target_identity_from_cache_compatibility() {
    let macos = fixture_provenance("macos", "aarch64");
    let linux = fixture_provenance("linux", "x86_64");

    assert_ne!(macos.fingerprint(), linux.fingerprint());
    assert_eq!(
        macos.compatibility_fingerprint(),
        linux.compatibility_fingerprint()
    );
    assert_eq!(
        EmbeddingProvenanceRecord::from_json(macos.canonical_json())
            .unwrap()
            .fingerprint(),
        macos.fingerprint()
    );
}

#[test]
fn provenance_rejects_noncanonical_files_targets_and_worker_configuration() {
    let canonical = fixture_provenance("linux", "x86_64");
    let value = serde_json::from_str::<serde_json::Value>(canonical.canonical_json()).unwrap();

    let mut unsupported_target = value.clone();
    unsupported_target["target_arch"] = json!("aarch64");
    assert!(EmbeddingProvenanceRecord::from_json(&unsupported_target.to_string()).is_err());

    let mut wrong_workers = value.clone();
    wrong_workers["intra_threads"] = json!(4);
    assert!(EmbeddingProvenanceRecord::from_json(&wrong_workers.to_string()).is_err());

    let mut malformed_files = value;
    malformed_files["files"][0]
        .as_object_mut()
        .unwrap()
        .remove("bytes");
    assert!(EmbeddingProvenanceRecord::from_json(&malformed_files.to_string()).is_err());
}

#[test]
fn exact_embedding_cache_is_immutable_reusable_and_doctor_validated() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, chunk_id) = pinned_database_with_chunk(&path);
    assert!(!database.has_complete_vector_membership().unwrap());
    let embedding = fixture_embedding(chunk_id);

    let inserted = database.cache_chunk_embedding(&embedding).unwrap();
    let reused = database.cache_chunk_embedding(&embedding).unwrap();
    assert!(matches!(inserted, EmbeddingCacheOutcome::Inserted { .. }));
    assert!(matches!(reused, EmbeddingCacheOutcome::Reused { .. }));
    assert_eq!(embedding_id(inserted), embedding_id(reused));
    drop(database);

    Doctor::run(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(count(&connection, "embedding_provenance"), 1);
    assert_eq!(count(&connection, "chunk_embeddings"), 1);
}

#[test]
fn embedding_cache_obeys_the_single_writer_lease() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, chunk_id) = pinned_database_with_chunk(&path);
    let writer_lock = WriterLock::acquire(&path, Duration::ZERO).unwrap();

    assert!(matches!(
        database.cache_chunk_embedding_with_timeout(&fixture_embedding(chunk_id), Duration::ZERO,),
        Err(StoreError::WriterLockBusy { .. })
    ));
    drop(writer_lock);
    database
        .cache_chunk_embedding_with_timeout(&fixture_embedding(chunk_id), Duration::ZERO)
        .unwrap();
}

#[test]
fn compatible_cross_target_provenance_reuses_the_first_audited_vector() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, chunk_id) = pinned_database_with_chunk(&path);
    let first = fixture_embedding(chunk_id);
    let first_outcome = database.cache_chunk_embedding(&first).unwrap();
    let mut compatible_vector = vec![0.0; EMBEDDING_DIMENSION as usize];
    compatible_vector[0] = 0.999_999_5;
    compatible_vector[1] = 0.001;
    let compatible = PreparedChunkEmbedding::new(
        chunk_id,
        Sha256Digest::of_bytes(b"canonical embedding input"),
        compatible_vector,
        fixture_provenance("linux", "x86_64"),
    )
    .unwrap();

    let reused = database.cache_chunk_embedding(&compatible).unwrap();
    assert!(matches!(reused, EmbeddingCacheOutcome::Reused { .. }));
    assert_eq!(embedding_id(first_outcome), embedding_id(reused));
    drop(database);
    Doctor::run(&path).unwrap();
}

#[test]
fn embedding_input_is_nfc_and_lf_normalized() {
    let decomposed = prepare_embedding_input("cafe\u{301}", "repo://notes\r\nmd", "a\r\nb\rc");
    let composed = prepare_embedding_input("caf\u{e9}", "repo://notes\nmd", "a\nb\nc");
    assert_eq!(decomposed, composed);
    assert_eq!(
        composed,
        "Title: caf\u{e9}\nSource: repo://notes\nmd\n\na\nb\nc"
    );
}

#[test]
fn cached_reembedding_builds_and_atomically_flips_the_shadow_slot() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);
    let inputs = database.load_embedding_input_batch(0, 8).unwrap();
    assert_eq!(inputs.len(), 1);
    assert!(!database.has_cached_embedding(&inputs[0]).unwrap());
    let uncached_plan = database.plan_reembedding().unwrap();
    assert_eq!(uncached_plan.passages, 1);
    assert_eq!(uncached_plan.cached_inputs, 0);
    assert_eq!(uncached_plan.missing_inputs, 1);
    cache_active_inputs(&mut database);
    let cached_plan = database.plan_reembedding().unwrap();
    assert_eq!(cached_plan.passages, 1);
    assert_eq!(cached_plan.cached_inputs, 1);
    assert_eq!(cached_plan.missing_inputs, 0);
    assert!(database.has_cached_embedding(&inputs[0]).unwrap());
    assert!(cached_plan.estimated_write_bytes < uncached_plan.estimated_write_bytes);

    let old_reader = Connection::open(&path).unwrap();
    old_reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert_eq!(metadata(&old_reader, "active_vector_slot"), b"0");
    assert_eq!(count(&old_reader, "passages_vec_a"), 0);

    let outcome = database.commit_cached_reembedding().unwrap();
    assert_eq!(outcome.generation_id, 2);
    assert_eq!(outcome.index_epoch, 2);
    assert_eq!(outcome.passages, 1);
    assert_eq!(outcome.active_vector_slot, 1);
    assert!(database.has_complete_vector_membership().unwrap());

    assert_eq!(metadata(&old_reader, "active_vector_slot"), b"0");
    assert_eq!(count(&old_reader, "passages_vec_a"), 0);
    old_reader.execute_batch("COMMIT").unwrap();
    assert_eq!(metadata(&old_reader, "active_vector_slot"), b"1");
    assert_eq!(count(&old_reader, "passages_vec_b"), 1);
    drop(old_reader);
    drop(database);
    Doctor::run(&path).unwrap();
}

#[test]
fn semantic_search_requires_validated_complete_vectors_and_returns_guarded_evidence() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);
    let scope = fixture_scope();
    let request =
        SearchRequest::new("conceptual query", SearchMode::Semantic, 10, 3_000, true).unwrap();

    assert!(matches!(
        database.search(scope.project_id, &request),
        Err(SearchError::QueryEmbeddingRequired)
    ));
    let query = unit_vector(0);
    let request = request.with_query_embedding(&query).unwrap();
    assert!(matches!(
        database.search(scope.project_id, &request),
        Err(SearchError::SemanticUnavailable)
    ));

    cache_active_inputs(&mut database);
    let outcome = database.commit_cached_reembedding().unwrap();
    let response = database.search(scope.project_id, &request).unwrap();

    assert_eq!(response.generation, Some(outcome.generation_id));
    assert_eq!(response.effective_mode, SearchMode::Semantic);
    assert_eq!(response.retrievers, vec![Retriever::Vector]);
    assert_eq!(response.stop_reason, SearchStopReason::UniqueExhausted);
    assert_eq!(response.examined.vector, 1);
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].content, "semantic alpha beta\n");
    assert_eq!(response.results[0].score.lists.len(), 1);
    assert_eq!(
        response.results[0].score.lists[0].retriever,
        Retriever::Vector
    );
    assert_eq!(response.results[0].score.lists[0].backend_score, Some(0.0));

    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE generations
             SET embedding_model_fingerprint = ?1
             WHERE id = ?2",
            rusqlite::params![
                Sha256Digest::of_bytes(b"different model")
                    .as_bytes()
                    .as_slice(),
                outcome.generation_id,
            ],
        )
        .unwrap();
    assert!(matches!(
        database.search(scope.project_id, &request),
        Err(SearchError::SemanticUnavailable)
    ));
}

#[test]
fn semantic_query_embedding_rejects_wrong_shape_nonfinite_and_unnormalized_input() {
    let request = SearchRequest::new("query", SearchMode::Semantic, 10, 3_000, false).unwrap();
    assert!(matches!(
        request
            .clone()
            .with_query_embedding(&vec![0.0; EMBEDDING_DIMENSION as usize - 1]),
        Err(SearchError::InvalidQueryEmbedding("vector dimension"))
    ));
    let mut nonfinite = unit_vector(0);
    nonfinite[1] = f32::NAN;
    assert!(matches!(
        request.clone().with_query_embedding(&nonfinite),
        Err(SearchError::InvalidQueryEmbedding("non-finite component"))
    ));
    assert!(matches!(
        request.with_query_embedding(&vec![0.0; EMBEDDING_DIMENSION as usize]),
        Err(SearchError::InvalidQueryEmbedding("vector normalization"))
    ));
}

#[test]
fn incomplete_reembedding_cache_rolls_back_without_a_generation_or_slot_flip() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);

    assert!(matches!(
        database.commit_cached_reembedding(),
        Err(StoreError::InvalidEmbedding("embedding cache incomplete"))
    ));
    assert!(!database.has_complete_vector_membership().unwrap());
    drop(database);
    let connection = Connection::open(&path).unwrap();
    assert_eq!(count(&connection, "generations"), 1);
    assert_eq!(metadata(&connection, "active_generation"), b"1");
    assert_eq!(metadata(&connection, "active_vector_slot"), b"0");
    assert_eq!(count(&connection, "passages_vec_a"), 0);
    assert_eq!(count(&connection, "passages_vec_b"), 0);
    drop(connection);
    Doctor::run(&path).unwrap();
}

#[test]
fn later_lexical_ingest_atomically_invalidates_complete_vector_membership() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);
    cache_active_inputs(&mut database);
    database.commit_cached_reembedding().unwrap();
    assert!(database.has_complete_vector_membership().unwrap());

    let outcome = database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &[prepared_document_with_body(b"changed semantic evidence\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    assert_eq!(outcome.generation_id, Some(3));
    assert!(!database.has_complete_vector_membership().unwrap());
    drop(database);

    let connection = Connection::open(&path).unwrap();
    assert_eq!(metadata(&connection, "active_vector_slot"), b"1");
    assert_eq!(count(&connection, "passages_vec_b"), 0);
    assert_eq!(
        connection
            .query_row(
                "SELECT vector_state FROM generations WHERE id = 3",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "absent"
    );
    drop(connection);
    Doctor::run(&path).unwrap();
}

#[test]
fn verified_backup_preserves_complete_vector_membership_and_cache_evidence() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let backup = directory.path().join("backup.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);
    cache_active_inputs(&mut database);
    database.commit_cached_reembedding().unwrap();
    drop(database);

    create_backup(&path, &backup, Duration::from_secs(5)).unwrap();
    Doctor::run(&backup).unwrap();
    let backup_database = IndexDb::open_existing(&backup, OpenMode::ReadOnly).unwrap();
    assert!(backup_database.has_complete_vector_membership().unwrap());
    drop(backup_database);
    let connection = Connection::open(&backup).unwrap();
    assert_eq!(count(&connection, "chunk_embeddings"), 1);
    assert_eq!(count(&connection, "embedding_provenance"), 1);
    assert_eq!(count(&connection, "passages_vec_b"), 1);
}

#[test]
fn prune_reclaims_only_historical_embedding_cache_and_preserves_active_vectors() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let backup = directory.path().join("pre-prune.sqlite");
    let (mut database, _) = pinned_database_with_chunk(&path);
    cache_active_inputs(&mut database);
    database.commit_cached_reembedding().unwrap();
    database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &[prepared_document_with_body(
                b"replacement semantic evidence\n",
            )],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    cache_active_inputs(&mut database);
    database.commit_cached_reembedding().unwrap();
    assert!(database.has_complete_vector_membership().unwrap());
    drop(database);

    let plan = plan_prune(
        &path,
        OffsetDateTime::now_utc() + time::Duration::days(1),
        1,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(plan.plan.affected_revisions.len(), 1);
    apply_prune(
        &path,
        &plan,
        plan.plan_hash,
        &backup,
        Duration::from_secs(5),
    )
    .unwrap();

    Doctor::run(&path).unwrap();
    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert!(database.has_complete_vector_membership().unwrap());
    drop(database);
    let connection = Connection::open(&path).unwrap();
    assert_eq!(count(&connection, "chunk_embeddings"), 1);
    assert_eq!(count(&connection, "embedding_provenance"), 1);
    assert_eq!(count(&connection, "passages_vec_a"), 1);
}

#[test]
fn physical_forget_removes_vector_evidence_and_restore_recovers_it_exactly() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let prune_backup = directory.path().join("pre-prune.sqlite");
    let recovery = directory.path().join("recovery.sqlite");
    let restore_plan_path = directory.path().join("restore.json");
    let safety = directory.path().join("safety.sqlite");
    let scope = fixture_scope();
    let document = prepared_document();
    let (mut database, _) = pinned_database_with_chunk(&path);
    cache_active_inputs(&mut database);
    database.commit_cached_reembedding().unwrap();
    drop(database);

    let prune = plan_prune(
        &path,
        OffsetDateTime::now_utc() + time::Duration::days(1),
        1,
        Duration::from_secs(5),
    )
    .unwrap();
    apply_prune(
        &path,
        &prune,
        prune.plan_hash,
        &prune_backup,
        Duration::from_secs(5),
    )
    .unwrap();

    let report = Doctor::run(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    let raw_document_id: Vec<u8> = connection
        .query_row("SELECT id FROM documents", [], |row| row.get(0))
        .unwrap();
    drop(connection);
    let citation = Citation {
        index_id: report.index_id,
        source_id: scope.source_id,
        document_id: DocumentId::from_uuid(Uuid::from_slice(&raw_document_id).unwrap()),
        revision: document.revision_sha256,
        span: document.chunks[0].byte_span,
    };
    let forget = plan_forget(
        &path,
        scope.project_id,
        std::slice::from_ref(&citation),
        Duration::from_secs(5),
    )
    .unwrap();
    apply_forget(
        &path,
        &forget,
        forget.plan_hash,
        &recovery,
        &restore_plan_path,
        Duration::from_secs(5),
    )
    .unwrap();

    Doctor::run(&path).unwrap();
    let forgotten = Connection::open(&path).unwrap();
    assert_eq!(count(&forgotten, "chunk_embeddings"), 0);
    assert_eq!(count(&forgotten, "embedding_provenance"), 0);
    assert_eq!(count(&forgotten, "passages_vec_a"), 0);
    assert_eq!(count(&forgotten, "passages_vec_b"), 0);
    drop(forgotten);

    let restore_plan: PlanEnvelope<RestorePlan> = read_plan(&restore_plan_path).unwrap();
    apply_restore(
        &path,
        &restore_plan,
        restore_plan.plan_hash,
        &recovery,
        &safety,
        Duration::from_secs(5),
    )
    .unwrap();
    Doctor::run(&path).unwrap();
    let restored = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert!(restored.has_complete_vector_membership().unwrap());
    drop(restored);
    let restored = Connection::open(&path).unwrap();
    assert_eq!(count(&restored, "chunk_embeddings"), 1);
    assert_eq!(count(&restored, "embedding_provenance"), 1);
    assert_eq!(count(&restored, "passages_vec_b"), 1);
}

#[test]
fn doctor_rejects_tampered_embedding_provenance() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, chunk_id) = pinned_database_with_chunk(&path);
    database
        .cache_chunk_embedding(&fixture_embedding(chunk_id))
        .unwrap();
    drop(database);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE embedding_provenance SET canonical_json = '{}'", [])
        .unwrap();
    drop(connection);

    assert!(matches!(
        Doctor::run(&path),
        Err(StoreError::ImmutableEvidenceMismatch(
            "embedding provenance"
        ))
    ));
}

#[test]
fn doctor_rejects_tampered_embedding_vectors() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let (mut database, chunk_id) = pinned_database_with_chunk(&path);
    database
        .cache_chunk_embedding(&fixture_embedding(chunk_id))
        .unwrap();
    drop(database);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE chunk_embeddings SET vector_blob = zeroblob(?1)",
            [i64::from(EMBEDDING_DIMENSION) * 4],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        Doctor::run(&path),
        Err(StoreError::ImmutableEvidenceMismatch("embedding vector"))
    ));
}

fn pinned_database_with_chunk(path: &std::path::Path) -> (IndexDb, i64) {
    let profile = IndexEmbeddingProfile::Pinned(fixture_pin());
    let mut database =
        IndexDb::create_with_embedding_profile(path, IndexId::new_v4(), &profile).unwrap();
    database
        .apply_filesystem_snapshot(
            &fixture_scope(),
            &[prepared_document()],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let connection = Connection::open(path).unwrap();
    let chunk_id = connection
        .query_row("SELECT id FROM chunks", [], |row| row.get(0))
        .unwrap();
    drop(connection);
    let reopened = IndexDb::open_existing(path, OpenMode::ReadWrite).unwrap();
    (reopened, chunk_id)
}

fn fixture_embedding(chunk_id: i64) -> PreparedChunkEmbedding {
    PreparedChunkEmbedding::new(
        chunk_id,
        Sha256Digest::of_bytes(b"canonical embedding input"),
        unit_vector(0),
        fixture_provenance("macos", "aarch64"),
    )
    .unwrap()
}

fn cache_active_inputs(database: &mut IndexDb) {
    let inputs = database.load_embedding_input_batch(0, 8).unwrap();
    assert_eq!(inputs.len(), 1);
    assert!(
        database
            .load_embedding_input_batch(inputs[0].passage_id(), 8)
            .unwrap()
            .is_empty()
    );
    let embedding = PreparedChunkEmbedding::from_input(
        &inputs[0],
        unit_vector(0),
        fixture_provenance("macos", "aarch64"),
    )
    .unwrap();
    database.cache_chunk_embedding(&embedding).unwrap();
}

fn unit_vector(coordinate: usize) -> Vec<f32> {
    let mut vector = vec![0.0; EMBEDDING_DIMENSION as usize];
    vector[coordinate] = 1.0;
    vector
}

fn fixture_provenance(target_os: &str, target_arch: &str) -> EmbeddingProvenanceRecord {
    let pin = fixture_pin();
    let value = json!({
        "schema_version": "hsum.embedding-provenance.v1",
        "model_id": pin.model_id(),
        "upstream_repository": "BAAI/bge-small-en-v1.5",
        "upstream_revision": pin.upstream_revision(),
        "manifest_sha256": pin.model_fingerprint().to_string(),
        "files": [{
            "path": "model.onnx",
            "bytes": 1,
            "sha256": Sha256Digest::of_bytes(b"fixture").to_string()
        }],
        "vector_dimension": EMBEDDING_DIMENSION,
        "pooling": "cls",
        "normalization": "l2_after_pooling",
        "normalization_implementation": "fastembed::common::normalize",
        "component_type": "ieee754_binary32",
        "quantization": "none",
        "output_selection": "fastembed_default_precedence",
        "fastembed_version": "5.17.4",
        "ort_crate_version": "2.0.0-rc.13",
        "onnx_runtime_version": "1.28.0",
        "onnx_runtime_build_info": "fixture CPU build",
        "execution_provider": "CPUExecutionProvider",
        "execution_provider_configuration": "default_cpu_fallback",
        "graph_optimization_level": "level3",
        "target_os": target_os,
        "target_arch": target_arch,
        "target_endianness": "little",
        "max_length": 512,
        "intra_threads": 2
    });
    EmbeddingProvenanceRecord::from_json(&value.to_string()).unwrap()
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

fn prepared_document() -> PreparedDocument {
    prepared_document_with_body(b"semantic alpha beta\n")
}

fn prepared_document_with_body(body: &[u8]) -> PreparedDocument {
    let connector_key = b"notes.md";
    let source_uri = "repo://notes.md";
    let title = "notes.md";
    let metadata = json!({});
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
        chunker_fingerprint: hsum::store::chunker_fingerprint(hsum::ingest::ChunkKind::Markdown),
        chunks: vec![PreparedChunk {
            ordinal: 0,
            byte_span: ByteSpan::new(0, body.len() as u64).unwrap(),
            line_span: LineSpan::new(1, 1).unwrap(),
            body_text: std::str::from_utf8(body).unwrap().to_owned(),
            content_sha256: body_sha256(body),
            quote_bloom: QuoteBloom::from_content(body).into_bytes(),
            literals: prepare_passage_literals(title, source_uri, body),
        }],
    }
}

fn embedding_id(outcome: EmbeddingCacheOutcome) -> i64 {
    match outcome {
        EmbeddingCacheOutcome::Inserted { embedding_id }
        | EmbeddingCacheOutcome::Reused { embedding_id } => embedding_id,
    }
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn metadata(connection: &Connection, key: &str) -> Vec<u8> {
    connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}
