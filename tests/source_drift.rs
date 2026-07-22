#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::Duration;

use hsum::app::prepare_filesystem_snapshot;
use hsum::domain::{IndexId, ProjectId, SafeSlug, SourceId};
use hsum::ingest::DiscoveryOptions;
use hsum::search::{GetRequest, SearchRequest};
use hsum::status::{DriftOptions, DriftState, SourceSyncState, Status};
use hsum::store::{DeleteConfirmations, FilesystemScope, IndexDb, OpenMode, SnapshotFailure};

mod support;
use support::private_tempdir as tempdir;

fn scope(root: &Path) -> FilesystemScope {
    FilesystemScope {
        source_id: SourceId::new_v4(),
        source_name: SafeSlug::new("workspace").unwrap(),
        source_logical_uri: root.display().to_string(),
        source_config_json: "{}".to_owned(),
        project_id: ProjectId::new_v4(),
        project_name: SafeSlug::new("default").unwrap(),
    }
}

fn assert_authoritative_status_eq(
    mut left: hsum::status::StatusReport,
    mut right: hsum::status::StatusReport,
) {
    if let Some(storage) = &mut left.storage {
        storage.available_bytes = 0;
    }
    if let Some(storage) = &mut right.storage {
        storage.available_bytes = 0;
    }
    assert_eq!(left, right);
}

#[test]
fn status_is_read_only_authoritative_and_terminal_safe() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("notes.md"), b"alpha evidence\n").unwrap();
    let index_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&index_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let snapshot = prepare_filesystem_snapshot(&root, &DiscoveryOptions::default()).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &snapshot.documents,
            &[SnapshotFailure {
                connector_key: b"denied\nfile.md".to_vec(),
                code: "SOURCE_\u{1b}[31mDENIED".to_owned(),
                detail: "denied\n\u{1b}]8;;https://bad.invalid\u{7}file".to_owned(),
            }],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let first = Status::read(&index_path).unwrap();
    let second = Status::read(&index_path).unwrap();

    assert_authoritative_status_eq(first.clone(), second);
    assert!(first.database_read_only);
    assert!(first.query_only);
    assert_eq!(first.index_epoch, 1);
    assert!(first.active_generation.is_some());
    assert_eq!(first.active_documents, 1);
    assert_eq!(first.active_passages, 1);
    assert_eq!(first.sources.len(), 1);
    let source = &first.sources[0];
    assert_eq!(source.state, SourceSyncState::Partial);
    let code = source.last_error_code.as_ref().unwrap().as_str();
    let detail = source.last_error_detail.as_ref().unwrap().as_str();
    assert!(!code.contains('\u{1b}'));
    assert!(!detail.contains('\u{1b}'));
    assert!(!detail.contains('\n'));
    assert!(code.contains("\\x1B"));
    assert!(detail.contains("\\n"));
    assert!(
        first
            .actionable_problems()
            .iter()
            .any(|problem| problem.code == "SOURCE_SYNC_PARTIAL")
    );
}

#[test]
fn drift_reports_edit_delete_and_block_without_changing_stored_evidence() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("edited.md"), b"immutable alpha evidence\n").unwrap();
    fs::write(root.join("deleted.md"), b"delete this evidence\n").unwrap();
    fs::write(root.join("blocked.md"), b"block this evidence\n").unwrap();
    let index_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&index_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let snapshot = prepare_filesystem_snapshot(&root, &DiscoveryOptions::default()).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &snapshot.documents,
            &snapshot.failures,
            DeleteConfirmations::default(),
        )
        .unwrap();

    let search = database
        .search(
            scope.project_id,
            &SearchRequest::with_defaults("immutable").unwrap(),
        )
        .unwrap();
    let citation = search.results[0].citation();
    drop(database);

    let unchanged = Status::read_with_drift(
        &index_path,
        &root,
        DriftOptions {
            verify_content_hash: true,
            deadline: Duration::from_secs(2),
        },
    )
    .unwrap();
    assert!(unchanged.drift.observations.iter().all(|observation| {
        observation.state == DriftState::MetadataUnchanged
            && observation.content_matches == Some(true)
    }));

    let durable_before = unchanged.status.clone();
    fs::write(
        root.join("edited.md"),
        b"live bytes changed after the immutable ingest\n",
    )
    .unwrap();
    fs::remove_file(root.join("deleted.md")).unwrap();
    fs::remove_file(root.join("blocked.md")).unwrap();
    symlink("edited.md", root.join("blocked.md")).unwrap();

    let observed = Status::read_with_drift(
        &index_path,
        &root,
        DriftOptions {
            verify_content_hash: true,
            deadline: Duration::from_secs(2),
        },
    )
    .unwrap();
    assert_authoritative_status_eq(observed.status.clone(), durable_before);
    assert!(!observed.drift.deadline_reached);

    let by_path = observed
        .drift
        .observations
        .iter()
        .map(|observation| (observation.connector.as_str(), observation))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["edited.md"].state, DriftState::MetadataChanged);
    assert_eq!(by_path["edited.md"].content_matches, Some(false));
    assert_eq!(by_path["deleted.md"].state, DriftState::Missing);
    assert_eq!(by_path["deleted.md"].content_matches, None);
    assert_eq!(by_path["blocked.md"].state, DriftState::Blocked);
    assert_eq!(by_path["blocked.md"].content_matches, None);

    let database = IndexDb::open_existing(&index_path, OpenMode::ReadOnly).unwrap();
    let stored = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16 * 1024,
        })
        .unwrap();
    assert_eq!(stored.content, b"immutable alpha evidence\n");
}

#[test]
fn drift_probe_blocks_a_symlinked_source_root_ancestor() {
    let directory = tempdir().unwrap();
    let physical_parent = directory.path().join("physical");
    let root = physical_parent.join("source");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("notes.md"), b"bounded evidence\n").unwrap();
    let alias = directory.path().join("alias");
    symlink(&physical_parent, &alias).unwrap();
    let index_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&index_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let snapshot = prepare_filesystem_snapshot(&root, &DiscoveryOptions::default()).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &snapshot.documents,
            &snapshot.failures,
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let report = Status::read_with_drift(
        &index_path,
        &alias.join("source"),
        DriftOptions {
            verify_content_hash: true,
            deadline: Duration::from_secs(2),
        },
    )
    .unwrap();

    assert_eq!(report.drift.observations.len(), 1);
    assert_eq!(report.drift.observations[0].state, DriftState::Blocked);
    assert_eq!(report.drift.observations[0].content_matches, None);
}

#[test]
fn expired_drift_deadline_is_unknown_without_touching_the_source() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("notes.md"), b"deadline evidence\n").unwrap();
    let index_path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&index_path, IndexId::new_v4()).unwrap();
    let scope = scope(&root);
    let snapshot = prepare_filesystem_snapshot(&root, &DiscoveryOptions::default()).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &snapshot.documents,
            &snapshot.failures,
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);

    let report = Status::read_with_drift(
        &index_path,
        &root,
        DriftOptions {
            verify_content_hash: true,
            deadline: Duration::ZERO,
        },
    )
    .unwrap();

    assert!(report.drift.deadline_reached);
    assert_eq!(report.drift.observations.len(), 1);
    assert_eq!(report.drift.observations[0].state, DriftState::Unknown);
    assert_eq!(report.drift.observations[0].content_matches, None);
}
