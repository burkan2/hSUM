use std::fs::{self, File};
use std::path::Path;

use hsum::app::{
    BroadRootReason, FilesystemSourceConfig, InitError, InitNextStep, InitRequest, PointerOutcome,
    TrustRequest, TrustTarget, initialize, prepare_filesystem_snapshot, trust_repository,
};
use hsum::config::{ManagedPaths, RepositoryPointer, TrustRegistry};
use hsum::domain::{ProjectId, SafeSlug};
use hsum::ingest::{DEFAULT_MAX_SOURCE_BYTES, HARD_MAX_SOURCE_BYTES, HARD_MAX_SOURCE_FILES};
use hsum::search::SearchRequest;
use hsum::status::{SourceSyncState, Status};
use hsum::store::{
    Doctor, IndexDb, MINIMUM_STORAGE_RESERVE_BYTES, OpenMode, StoragePreflightError, WriterLock,
};
use rusqlite::Connection;
use tempfile::{TempDir, tempdir};

const RELEASED_ALPHA1_PIPELINE_FINGERPRINT: &str =
    "bb24fc64a8602c9ec0479ae687f848b7d5b029294796701966b7bbafc8a23bab";

fn request(root: &Path, home: &TempDir) -> InitRequest {
    let managed = ManagedPaths::resolve(Some(home.path())).unwrap();
    let mut request = InitRequest::new(root.to_path_buf(), managed);
    request.requested_root = Some(root.to_path_buf());
    request.home_dir = Some(home.path().join("not-the-source-home"));
    request.index_name = Some(SafeSlug::new("alpha").unwrap());
    request.project_name = Some(SafeSlug::new("default").unwrap());
    request
}

#[test]
fn truly_bare_init_selects_the_enclosing_git_root_and_collision_safe_names() {
    let tree = tempdir().unwrap();
    let repository = tree.path().join("My Repo");
    let nested = repository.join("nested");
    let home = tempdir().unwrap();
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::write(repository.join("README.md"), b"# Bare init\n").unwrap();
    let managed = ManagedPaths::resolve(Some(home.path())).unwrap();
    let mut init = InitRequest::new(nested, managed);
    init.home_dir = Some(home.path().join("not-the-source-home"));

    let outcome = initialize(&init).unwrap();

    assert_eq!(
        outcome.canonical_root,
        fs::canonicalize(&repository).unwrap()
    );
    assert_eq!(outcome.index_name.as_str(), "my-repo");
    assert_eq!(outcome.project_name.as_str(), "default");
    assert_eq!(
        outcome.database_path,
        home.path().join("data/indexes/my-repo/index.sqlite")
    );
    assert!(!repository.join(".hsum.toml").exists());
}

#[test]
fn bare_init_creates_managed_scope_and_ingests_without_writing_a_pointer() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(
        root.path().join("notes.md"),
        b"# Alpha evidence\nstable_identifier\n",
    )
    .unwrap();

    let outcome = initialize(&request(root.path(), &home)).unwrap();

    assert!(outcome.database_path.is_file());
    assert!(!root.path().join(".hsum.toml").exists());
    assert_eq!(outcome.pointer, PointerOutcome::NotRequested);
    assert!(!outcome.reused);
    assert_eq!(outcome.source_estimate.eligible_files, 1);
    let ingest = outcome.ingest.unwrap();
    assert_eq!(ingest.active_documents, 1);
    assert!(ingest.active_passages > 0);
    let InitNextStep::Search { query } = outcome.next_step else {
        panic!("nonempty init should emit a verified search query");
    };
    assert!(matches!(
        query.as_str(),
        "Alpha evidence" | "stable_identifier"
    ));
    let database = IndexDb::open_existing(&outcome.database_path, OpenMode::ReadOnly).unwrap();
    let response = database
        .search(
            outcome.project_id,
            &SearchRequest::with_defaults(&query).unwrap(),
        )
        .unwrap();
    assert!(response.results.iter().any(|result| {
        result.source_uri == "repo://notes.md" && result.content.contains(query.as_str())
    }));

    let registry = TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    assert_eq!(registry.bindings().len(), 1);
    assert_eq!(
        registry.bindings()[0].canonical_root(),
        fs::canonicalize(root.path()).unwrap()
    );
}

#[test]
fn an_existing_pointer_does_not_block_or_authorize_bare_init() {
    let first_root = tempdir().unwrap();
    let second_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(first_root.path().join("one.md"), b"one\n").unwrap();
    fs::write(second_root.path().join("two.md"), b"two\n").unwrap();

    let first = initialize(&request(first_root.path(), &home)).unwrap();
    let pointer_bytes = b"schema_version = 1\nindex = \"alpha\"\nproject = \"default\"\n".to_vec();
    fs::write(second_root.path().join(".hsum.toml"), &pointer_bytes).unwrap();
    let mut second_request = request(second_root.path(), &home);
    second_request.index_name = Some(SafeSlug::new("beta").unwrap());

    let second = initialize(&second_request).unwrap();

    assert_ne!(first.index_id, second.index_id);
    assert_eq!(
        fs::read(second_root.path().join(".hsum.toml")).unwrap(),
        pointer_bytes
    );
    let registry = TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    assert_eq!(registry.bindings().len(), 2);
}

#[test]
fn pointer_collision_refuses_before_managed_storage_is_created() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let pointer_path = root.path().join(".hsum.toml");
    let original = b"do not overwrite these bytes\n";
    fs::write(&pointer_path, original).unwrap();
    let mut init = request(root.path(), &home);
    init.write_pointer = true;

    let error = initialize(&init).unwrap_err();

    assert!(matches!(error, InitError::PointerExists { .. }));
    assert_eq!(fs::read(pointer_path).unwrap(), original);
    assert!(!home.path().join("data/indexes/alpha/index.sqlite").exists());
    assert!(!home.path().join("config/trusted-projects.toml").exists());
}

#[test]
fn forced_pointer_rerun_changes_only_the_pointer() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"alpha\n").unwrap();
    let first = initialize(&request(root.path(), &home)).unwrap();
    let trust_path = home.path().join("config/trusted-projects.toml");
    let pointer_path = root.path().join(".hsum.toml");
    fs::write(&pointer_path, b"old pointer bytes\n").unwrap();
    let database_before = fs::read(&first.database_path).unwrap();
    let trust_before = fs::read(&trust_path).unwrap();

    let mut rerun = request(root.path(), &home);
    rerun.write_pointer = true;
    rerun.force_pointer = true;
    let outcome = initialize(&rerun).unwrap();

    assert!(outcome.reused);
    assert!(outcome.ingest.is_none());
    assert_eq!(outcome.pointer, PointerOutcome::Written);
    assert_eq!(fs::read(&first.database_path).unwrap(), database_before);
    assert_eq!(fs::read(&trust_path).unwrap(), trust_before);
    let pointer = RepositoryPointer::parse(&fs::read_to_string(pointer_path).unwrap()).unwrap();
    assert_eq!(pointer.index_name().as_str(), "alpha");
    assert_eq!(pointer.project_name().as_str(), "default");
}

#[test]
fn an_empty_source_still_initializes_exactly_one_source_and_project() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();

    let outcome = initialize(&request(root.path(), &home)).unwrap();

    let ingest = outcome.ingest.unwrap();
    assert_eq!(ingest.active_documents, 0);
    assert_eq!(ingest.active_passages, 0);
    assert_eq!(ingest.generation_id, None);
    let connection = Connection::open(outcome.database_path).unwrap();
    let sources: i64 = connection
        .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
        .unwrap();
    let projects: i64 = connection
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM generations", [], |row| row.get(0))
        .unwrap();
    assert_eq!((sources, projects, generations), (1, 1, 0));
}

#[cfg(unix)]
#[test]
fn root_home_and_above_git_roots_need_the_exact_broad_root_confirmation() {
    let source = tempdir().unwrap();
    let home = tempdir().unwrap();
    let mut filesystem_root = request(source.path(), &home);
    filesystem_root.requested_root = Some(Path::new("/").to_path_buf());
    assert!(matches!(
        initialize(&filesystem_root),
        Err(InitError::BroadRootConfirmationRequired {
            reason: BroadRootReason::FilesystemRoot,
            ..
        })
    ));

    let mut home_root = request(source.path(), &home);
    home_root.home_dir = Some(source.path().to_path_buf());
    assert!(matches!(
        initialize(&home_root),
        Err(InitError::BroadRootConfirmationRequired {
            reason: BroadRootReason::HomeDirectory,
            ..
        })
    ));
    home_root.allow_broad_root = true;
    assert!(initialize(&home_root).is_ok());

    let tree = tempdir().unwrap();
    let repository = tree.path().join("repo");
    let current = repository.join("nested");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(&current).unwrap();
    let mut above_git = request(tree.path(), &home);
    above_git.current_dir = current;
    assert!(matches!(
        initialize(&above_git),
        Err(InitError::BroadRootConfirmationRequired {
            reason: BroadRootReason::AboveGitWorktree { .. },
            ..
        })
    ));
}

#[test]
fn large_source_confirmation_is_checked_before_database_creation() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = File::create(root.path().join("huge.md")).unwrap();
    file.set_len(DEFAULT_MAX_SOURCE_BYTES + 1).unwrap();

    let mut init = request(root.path(), &home);
    let error = initialize(&init).unwrap_err();

    assert!(matches!(
        error,
        InitError::LargeSourceConfirmationRequired { .. }
    ));
    assert!(!home.path().join("data/indexes/alpha/index.sqlite").exists());
    assert!(!home.path().join("config/trusted-projects.toml").exists());

    init.allow_large_source = true;
    let outcome = initialize(&init).unwrap();
    assert!(outcome.database_path.is_file());
    assert_eq!(outcome.source_estimate.eligible_files, 1);
    assert_eq!(outcome.source_estimate.skipped_files, 1);
    let connection = Connection::open(&outcome.database_path).unwrap();
    let config_json: String = connection
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    let config = FilesystemSourceConfig::parse(&config_json).unwrap();
    assert_eq!(
        config.discovery_options().max_source_files(),
        HARD_MAX_SOURCE_FILES
    );
    assert_eq!(
        config.discovery_options().max_source_bytes(),
        HARD_MAX_SOURCE_BYTES
    );
    let refreshed = prepare_filesystem_snapshot(root.path(), config.discovery_options()).unwrap();
    assert_eq!(refreshed.documents.len(), 0);
    assert_eq!(refreshed.failures.len(), 1);
}

#[test]
fn configured_index_quota_is_preflighted_before_creation_and_persisted() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"quota evidence\n").unwrap();
    let mut init = request(root.path(), &home);
    init.index_quota_bytes = Some(1);

    let error = initialize(&init).unwrap_err();

    assert!(matches!(
        error,
        InitError::StoragePreflight(StoragePreflightError::QuotaExceeded { .. })
    ));
    assert!(!home.path().join("data/indexes/alpha/index.sqlite").exists());
    assert!(!home.path().join("config/trusted-projects.toml").exists());

    let quota = MINIMUM_STORAGE_RESERVE_BYTES * 2;
    init.index_quota_bytes = Some(quota);
    let outcome = initialize(&init).unwrap();
    assert_eq!(
        outcome
            .storage_preflight
            .as_ref()
            .and_then(|preflight| preflight.quota_bytes),
        Some(quota)
    );
    let connection = Connection::open(&outcome.database_path).unwrap();
    let config_json: String = connection
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        FilesystemSourceConfig::parse(&config_json)
            .unwrap()
            .index_quota_bytes(),
        Some(quota)
    );
    let status = Status::read(&outcome.database_path).unwrap();
    assert_eq!(status.index_quota_bytes, Some(quota));
    let storage = status
        .storage
        .expect("status should report inspected local storage");
    assert_eq!(storage.quota_bytes, Some(quota));
    assert!(storage.managed_index_bytes > 0);
    assert_eq!(storage.reclaimable_bytes, 0);
    assert!(storage.reserve_bytes >= MINIMUM_STORAGE_RESERVE_BYTES);
}

#[test]
fn no_ingest_configures_the_source_without_claiming_a_successful_scan() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"not yet ingested\n").unwrap();
    let mut init = request(root.path(), &home);
    init.no_ingest = true;

    let outcome = initialize(&init).unwrap();

    assert!(outcome.ingest.is_none());
    let status = Status::read(&outcome.database_path).unwrap();
    assert_eq!(status.active_generation, None);
    assert_eq!(status.active_documents, 0);
    assert_eq!(status.sources.len(), 1);
    assert_eq!(status.sources[0].state, SourceSyncState::NeverSucceeded);
    assert!(status.sources[0].last_success_at.is_none());
}

#[test]
fn rebuild_requires_an_existing_binding_and_an_initial_ingest() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let mut rebuild = request(root.path(), &home);
    rebuild.rebuild = true;

    assert!(matches!(
        initialize(&rebuild),
        Err(InitError::RebuildBindingRequired { .. })
    ));

    rebuild.no_ingest = true;
    assert!(matches!(
        initialize(&rebuild),
        Err(InitError::RebuildWithoutIngest)
    ));
    assert!(!home.path().join("config/trusted-projects.toml").exists());
}

#[test]
fn rebuild_replaces_one_coherent_stale_index_and_preserves_other_bindings() {
    let first_root = tempdir().unwrap();
    let second_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(
        first_root.path().join("notes.md"),
        b"# Rebuilt evidence\nreplacement_identifier\n",
    )
    .unwrap();
    fs::write(second_root.path().join("notes.md"), b"other binding\n").unwrap();

    let first = initialize(&request(first_root.path(), &home)).unwrap();
    let mut second_request = request(second_root.path(), &home);
    second_request.index_name = Some(SafeSlug::new("beta").unwrap());
    let second = initialize(&second_request).unwrap();
    let registry_before =
        TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    let first_binding_before = registry_before
        .bindings()
        .iter()
        .find(|binding| binding.index_id() == first.index_id)
        .unwrap()
        .clone();
    let second_binding_before = registry_before
        .bindings()
        .iter()
        .find(|binding| binding.index_id() == second.index_id)
        .unwrap()
        .clone();
    make_fingerprint_stale(&first.database_path);

    let mut rebuild = request(first_root.path(), &home);
    rebuild.rebuild = true;
    let rebuilt = initialize(&rebuild).unwrap();

    let summary = rebuilt.rebuild.as_ref().expect("rebuild is reported");
    assert_eq!(
        summary.previous_binding_id,
        first_binding_before.binding_id()
    );
    assert_eq!(summary.previous_index_id, first.index_id);
    assert_eq!(summary.active_documents, 1);
    assert!(summary.active_passages > 0);
    assert_ne!(rebuilt.index_id, first.index_id);
    assert_eq!(rebuilt.index_name, first.index_name);
    assert!(Doctor::run(&rebuilt.database_path).is_ok());

    let registry_after =
        TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    assert_eq!(registry_after.bindings().len(), 2);
    let first_binding_after = registry_after
        .bindings()
        .iter()
        .find(|binding| binding.canonical_root() == fs::canonicalize(first_root.path()).unwrap())
        .unwrap();
    assert_ne!(
        first_binding_after.binding_id(),
        first_binding_before.binding_id()
    );
    assert_eq!(
        registry_after
            .bindings()
            .iter()
            .find(|binding| binding.binding_id() == second_binding_before.binding_id()),
        Some(&second_binding_before)
    );
}

#[test]
fn rebuild_also_replaces_a_healthy_index_when_explicitly_requested() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"healthy replacement\n").unwrap();
    let first = initialize(&request(root.path(), &home)).unwrap();

    let mut rebuild = request(root.path(), &home);
    rebuild.rebuild = true;
    let outcome = initialize(&rebuild).unwrap();

    assert_ne!(outcome.index_id, first.index_id);
    assert_eq!(
        outcome
            .rebuild
            .as_ref()
            .map(|summary| summary.previous_index_id),
        Some(first.index_id)
    );
    assert!(Doctor::run(&outcome.database_path).is_ok());
}

#[test]
fn rebuild_dry_run_validates_but_writes_nothing() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"dry run evidence\n").unwrap();
    let first = initialize(&request(root.path(), &home)).unwrap();
    make_fingerprint_stale(&first.database_path);
    let trust_path = home.path().join("config/trusted-projects.toml");
    let writer_lock_path = WriterLock::sidecar_path(&first.database_path);
    fs::remove_file(&writer_lock_path).unwrap();
    let database_before = fs::read(&first.database_path).unwrap();
    let trust_before = fs::read(&trust_path).unwrap();

    let mut rebuild = request(root.path(), &home);
    rebuild.rebuild = true;
    rebuild.dry_run = true;
    let outcome = initialize(&rebuild).unwrap();

    assert!(outcome.dry_run);
    assert!(outcome.rebuild.is_some());
    assert_eq!(fs::read(&first.database_path).unwrap(), database_before);
    assert_eq!(fs::read(&trust_path).unwrap(), trust_before);
    assert!(!writer_lock_path.exists());
    assert!(matches!(
        IndexDb::open_existing(&first.database_path, OpenMode::ReadOnly),
        Err(hsum::store::StoreError::PipelineFingerprintMismatch)
    ));
}

#[test]
fn rebuild_refuses_corruption_without_changing_database_or_trust() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"corruption boundary\n").unwrap();
    let first = initialize(&request(root.path(), &home)).unwrap();
    make_fingerprint_stale(&first.database_path);
    let connection = Connection::open(&first.database_path).unwrap();
    connection
        .execute(
            "UPDATE index_meta
             SET value = zeroblob(32)
             WHERE key = 'schema_checksum'",
            [],
        )
        .unwrap();
    drop(connection);
    let trust_path = home.path().join("config/trusted-projects.toml");
    let database_before = fs::read(&first.database_path).unwrap();
    let trust_before = fs::read(&trust_path).unwrap();

    let mut rebuild = request(root.path(), &home);
    rebuild.rebuild = true;
    let error = initialize(&rebuild).unwrap_err();

    assert!(matches!(
        error,
        InitError::Store(hsum::store::StoreError::SchemaChecksumMismatch)
    ));
    assert_eq!(fs::read(&first.database_path).unwrap(), database_before);
    assert_eq!(fs::read(&trust_path).unwrap(), trust_before);
}

fn make_fingerprint_stale(path: &Path) {
    let released_fingerprint = hex::decode(RELEASED_ALPHA1_PIPELINE_FINGERPRINT).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "UPDATE index_meta
             SET value = ?1
             WHERE key = 'pipeline_fingerprint'",
            [&released_fingerprint],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE generations
             SET pipeline_fingerprint = ?1",
            [&released_fingerprint],
        )
        .unwrap();
}

#[test]
fn an_occupied_database_path_keeps_its_original_bytes_on_failure() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let database_path = home.path().join("data/indexes/alpha/index.sqlite");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    let original = b"unrelated existing bytes";
    fs::write(&database_path, original).unwrap();

    let error = initialize(&request(root.path(), &home)).unwrap_err();

    assert!(matches!(error, InitError::IndexPathOccupied { .. }));
    assert_eq!(fs::read(database_path).unwrap(), original);
    assert!(!home.path().join("config/trusted-projects.toml").exists());
}

#[test]
fn explicit_trust_is_required_idempotent_and_conflict_safe() {
    let source_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initialized = initialize(&request(source_root.path(), &home)).unwrap();
    let target = TrustTarget {
        index_id: initialized.index_id,
        index_name: initialized.index_name.clone(),
        project_id: initialized.project_id,
        project_name: initialized.project_name.clone(),
    };
    let trust_path = home.path().join("config/trusted-projects.toml");
    fs::remove_file(&trust_path).unwrap();
    let mut trust = TrustRequest {
        root: source_root.path().to_path_buf(),
        managed_paths: ManagedPaths::resolve(Some(home.path())).unwrap(),
        target: target.clone(),
        confirm: false,
    };

    assert!(matches!(
        trust_repository(&trust),
        Err(InitError::TrustConfirmationRequired)
    ));
    assert!(!trust_path.exists());

    trust.confirm = true;
    let created = trust_repository(&trust).unwrap();
    assert!(created.created);
    let after_created = fs::read(&trust_path).unwrap();
    let repeated = trust_repository(&trust).unwrap();
    assert!(!repeated.created);
    assert_eq!(repeated.binding, created.binding);
    assert_eq!(fs::read(&trust_path).unwrap(), after_created);

    trust.target.project_id = ProjectId::new_v4();
    let conflict = trust_repository(&trust).unwrap_err();
    assert!(matches!(
        conflict,
        InitError::TrustedProjectIdentityMismatch { .. }
    ));
    assert_eq!(fs::read(&trust_path).unwrap(), after_created);
}

#[test]
fn rerun_refuses_a_binding_whose_exact_project_tuple_is_absent() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"alpha\n").unwrap();
    let initialized = initialize(&request(root.path(), &home)).unwrap();
    let connection = Connection::open(&initialized.database_path).unwrap();
    connection
        .execute("UPDATE projects SET name = 'other-project'", [])
        .unwrap();
    drop(connection);

    let error = initialize(&request(root.path(), &home)).unwrap_err();

    assert!(matches!(
        error,
        InitError::TrustedProjectIdentityMismatch { .. }
    ));
}

#[test]
fn rerun_refuses_a_binding_whose_source_logical_uri_targets_another_root() {
    let root = tempdir().unwrap();
    let redirected = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(root.path().join("notes.md"), b"alpha\n").unwrap();
    let initialized = initialize(&request(root.path(), &home)).unwrap();
    let connection = Connection::open(&initialized.database_path).unwrap();
    connection
        .execute(
            "UPDATE sources SET logical_uri = ?1",
            [redirected.path().to_str().unwrap()],
        )
        .unwrap();
    drop(connection);

    let error = initialize(&request(root.path(), &home)).unwrap_err();

    assert!(matches!(error, InitError::TrustedSourceRootMismatch { .. }));
}

#[test]
fn explicit_trust_refuses_a_database_whose_source_config_targets_another_root() {
    let root = tempdir().unwrap();
    let redirected = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initialized = initialize(&request(root.path(), &home)).unwrap();
    let trust_path = home.path().join("config/trusted-projects.toml");
    let trust_before = fs::read(&trust_path).unwrap();
    let connection = Connection::open(&initialized.database_path).unwrap();
    let original: String = connection
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&original).unwrap();
    config["root"] = redirected.path().to_str().unwrap().into();
    connection
        .execute(
            "UPDATE sources SET config_json = ?1",
            [serde_json::to_string(&config).unwrap()],
        )
        .unwrap();
    drop(connection);
    let trust = TrustRequest {
        root: root.path().to_path_buf(),
        managed_paths: ManagedPaths::resolve(Some(home.path())).unwrap(),
        target: TrustTarget {
            index_id: initialized.index_id,
            index_name: initialized.index_name,
            project_id: initialized.project_id,
            project_name: initialized.project_name,
        },
        confirm: true,
    };

    let error = trust_repository(&trust).unwrap_err();

    assert!(matches!(error, InitError::TrustedSourceRootMismatch { .. }));
    assert_eq!(fs::read(trust_path).unwrap(), trust_before);
}

#[test]
fn explicit_trust_refuses_to_bind_a_source_database_to_another_root() {
    let source_root = tempdir().unwrap();
    let other_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initialized = initialize(&request(source_root.path(), &home)).unwrap();
    let trust_path = home.path().join("config/trusted-projects.toml");
    let trust_before = fs::read(&trust_path).unwrap();
    let trust = TrustRequest {
        root: other_root.path().to_path_buf(),
        managed_paths: ManagedPaths::resolve(Some(home.path())).unwrap(),
        target: TrustTarget {
            index_id: initialized.index_id,
            index_name: initialized.index_name,
            project_id: initialized.project_id,
            project_name: initialized.project_name,
        },
        confirm: true,
    };

    let error = trust_repository(&trust).unwrap_err();

    assert!(matches!(error, InitError::TrustedSourceRootMismatch { .. }));
    assert_eq!(fs::read(trust_path).unwrap(), trust_before);
}

#[test]
fn explicit_trust_refuses_a_project_id_that_is_not_in_the_database() {
    let source_root = tempdir().unwrap();
    let new_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initialized = initialize(&request(source_root.path(), &home)).unwrap();
    let trust_path = home.path().join("config/trusted-projects.toml");
    let before = fs::read(&trust_path).unwrap();
    let trust = TrustRequest {
        root: new_root.path().to_path_buf(),
        managed_paths: ManagedPaths::resolve(Some(home.path())).unwrap(),
        target: TrustTarget {
            index_id: initialized.index_id,
            index_name: initialized.index_name,
            project_id: ProjectId::new_v4(),
            project_name: initialized.project_name,
        },
        confirm: true,
    };

    let error = trust_repository(&trust).unwrap_err();

    assert!(matches!(
        error,
        InitError::TrustedProjectIdentityMismatch { .. }
    ));
    assert_eq!(fs::read(trust_path).unwrap(), before);
}

#[test]
fn explicit_trust_enforces_alpha_source_cardinality() {
    let source_root = tempdir().unwrap();
    let new_root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initialized = initialize(&request(source_root.path(), &home)).unwrap();
    let connection = Connection::open(&initialized.database_path).unwrap();
    connection
        .execute("DELETE FROM project_sources", [])
        .unwrap();
    drop(connection);
    let trust = TrustRequest {
        root: new_root.path().to_path_buf(),
        managed_paths: ManagedPaths::resolve(Some(home.path())).unwrap(),
        target: TrustTarget {
            index_id: initialized.index_id,
            index_name: initialized.index_name,
            project_id: initialized.project_id,
            project_name: initialized.project_name,
        },
        confirm: true,
    };

    let error = trust_repository(&trust).unwrap_err();

    assert!(matches!(
        error,
        InitError::AlphaSourceCardinality { found: 0 }
    ));
}
