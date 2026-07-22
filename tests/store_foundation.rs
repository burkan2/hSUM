use hsum::domain::IndexId;
use hsum::store::{APPLICATION_ID, IndexDb, OpenMode, SCHEMA_VERSION, schema_checksum};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn private_tempdir() -> TempDir {
    let directory = tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[test]
fn bundled_sqlite_supports_required_fts5_and_identifier_tokens() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            r#"
            CREATE VIRTUAL TABLE evidence USING fts5(
                title,
                source_uri,
                body,
                tokenize = "unicode61 remove_diacritics 0 tokenchars '_-.:/'"
            );
            INSERT INTO evidence(rowid, title, source_uri, body)
            VALUES (7, 'search.rs', 'repo://src/search.rs', 'EVIDENCE_FORGOTTEN alpha-beta');
            "#,
        )
        .unwrap();

    let row_id: i64 = connection
        .query_row(
            "SELECT rowid FROM evidence WHERE evidence MATCH ?1",
            ["EVIDENCE_FORGOTTEN"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(row_id, 7);

    let path_row_id: i64 = connection
        .query_row(
            "SELECT rowid FROM evidence WHERE evidence MATCH ?1",
            ["\"repo://src/search.rs\""],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(path_row_id, 7);
}

#[test]
fn create_index_stamps_identity_schema_and_lexical_profile() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let index_id =
        IndexId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap());

    drop(IndexDb::create(&path, index_id).unwrap());

    let connection = Connection::open(&path).unwrap();
    let application_id: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    let schema_version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(application_id, APPLICATION_ID);
    assert_eq!(schema_version, SCHEMA_VERSION);
    assert_eq!(journal_mode, "wal");

    let stored_index_id: Vec<u8> = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'index_uuid'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored_schema_checksum: Vec<u8> = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'schema_checksum'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let embedding_profile: Vec<u8> = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'embedding_profile'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(stored_index_id, index_id.as_uuid().as_bytes());
    assert_eq!(stored_schema_checksum, schema_checksum().as_bytes());
    assert_eq!(embedding_profile, b"none");

    let fts_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'passages_fts')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(fts_exists);
}

#[test]
fn create_refuses_to_replace_an_existing_path() {
    let directory = private_tempdir();
    let path = directory.path().join("existing.sqlite");
    fs::write(&path, b"owned by the user").unwrap();
    let index_id = IndexId::new_v4();

    let error = match IndexDb::create(&path, index_id) {
        Ok(_) => panic!("existing paths must never be replaced"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        hsum::store::StoreError::AlreadyExists(ref actual) if actual == &path
    ));
    assert_eq!(fs::read(path).unwrap(), b"owned by the user");
}

#[test]
fn existing_indexes_reopen_only_in_the_requested_validated_mode() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let index_id = IndexId::new_v4();
    drop(IndexDb::create(&path, index_id).unwrap());

    let writable = IndexDb::open_existing(&path, OpenMode::ReadWrite).unwrap();
    assert!(!writable.is_read_only().unwrap());
    drop(writable);

    let read_only = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert!(read_only.is_read_only().unwrap());
    drop(read_only);

    let missing = directory.path().join("missing.sqlite");
    assert!(IndexDb::open_existing(&missing, OpenMode::ReadOnly).is_err());
    assert!(!missing.exists());
}

#[cfg(unix)]
#[test]
fn created_index_is_private_to_its_owner() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");

    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());

    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn opening_a_symlink_never_reaches_or_changes_its_target() {
    let directory = private_tempdir();
    let target = directory.path().join("target.sqlite");
    let alias = directory.path().join("alias.sqlite");
    drop(IndexDb::create(&target, IndexId::new_v4()).unwrap());
    let original = fs::read(&target).unwrap();
    symlink(&target, &alias).unwrap();

    let error = match IndexDb::open_existing(&alias, OpenMode::ReadWrite) {
        Ok(_) => panic!("a symlink must not be opened as an index"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        hsum::store::StoreError::UnsafeIndexPath(ref actual) if actual == &alias
    ));
    assert_eq!(fs::read(target).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn opening_through_a_user_created_ancestor_symlink_is_refused() {
    let directory = private_tempdir();
    let real_root = directory.path().join("real");
    let database_directory = real_root.join("data");
    private_directory(&database_directory);
    let path = database_directory.join("index.sqlite");
    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
    let original = fs::read(&path).unwrap();

    let alias_root = directory.path().join("alias");
    symlink(&real_root, &alias_root).unwrap();
    let alias_path = alias_root.join("data/index.sqlite");

    assert!(matches!(
        IndexDb::open_existing(&alias_path, OpenMode::ReadWrite),
        Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == alias_path
    ));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[cfg(target_os = "macos")]
#[test]
fn fixed_macos_var_alias_is_normalized_without_permitting_user_symlinks() {
    let directory = private_tempdir();
    assert!(directory.path().starts_with("/var"));
    let path = directory.path().join("index.sqlite");

    let database = IndexDb::create(&path, IndexId::new_v4()).unwrap();

    assert!(database.path().starts_with("/private/var"));
    database.verify_live_identity().unwrap();
}

#[cfg(unix)]
#[test]
fn opening_a_hard_link_is_refused_until_the_index_is_single_link_again() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let alias = directory.path().join("alias.sqlite");
    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
    fs::hard_link(&path, &alias).unwrap();

    for candidate in [&path, &alias] {
        assert!(matches!(
            IndexDb::open_existing(candidate, OpenMode::ReadOnly),
            Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == *candidate
        ));
    }

    fs::remove_file(alias).unwrap();
    drop(IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap());
}

#[cfg(unix)]
#[test]
fn opening_an_index_with_group_or_world_permissions_is_refused() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    assert!(matches!(
        IndexDb::open_existing(&path, OpenMode::ReadOnly),
        Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == path
    ));
}

#[cfg(unix)]
#[test]
fn creating_an_index_in_a_non_private_parent_is_refused_without_a_partial_file() {
    let directory = private_tempdir();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let path = directory.path().join("index.sqlite");

    assert!(matches!(
        IndexDb::create(&path, IndexId::new_v4()),
        Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == path
    ));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn unsafe_sidecars_are_refused_before_sqlite_can_touch_their_targets() {
    let scenarios = [
        ("-wal", "permissive"),
        ("-shm", "hardlink"),
        ("-journal", "fifo"),
        ("-wal", "symlink"),
    ];

    for (suffix, scenario) in scenarios {
        let directory = private_tempdir();
        let path = directory.path().join("index.sqlite");
        drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
        let checkpoint = Connection::open(&path).unwrap();
        checkpoint
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(checkpoint);
        for stale_suffix in ["-wal", "-shm", "-journal"] {
            let stale = sidecar_path(&path, stale_suffix);
            if stale.exists() {
                fs::remove_file(stale).unwrap();
            }
        }
        let sidecar = sidecar_path(&path, suffix);
        let target = directory.path().join(format!("target-{scenario}"));
        let marker = format!("must remain unchanged: {scenario}").into_bytes();

        match scenario {
            "permissive" => {
                fs::write(&sidecar, b"forged sidecar").unwrap();
                fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();
            }
            "hardlink" => {
                fs::write(&target, &marker).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                fs::hard_link(&target, &sidecar).unwrap();
            }
            "fifo" => {
                let status = Command::new("/usr/bin/mkfifo")
                    .arg(&sidecar)
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            "symlink" => {
                fs::write(&target, &marker).unwrap();
                symlink(&target, &sidecar).unwrap();
            }
            _ => unreachable!(),
        }

        assert!(matches!(
            IndexDb::open_existing(&path, OpenMode::ReadWrite),
            Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == sidecar
        ));
        if target.exists() {
            assert_eq!(fs::read(&target).unwrap(), marker);
        }
    }
}

#[cfg(unix)]
#[test]
fn create_requires_all_sqlite_sidecars_to_be_absent() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let orphan = sidecar_path(&path, "-wal");
    fs::write(&orphan, b"orphan must survive").unwrap();
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(matches!(
        IndexDb::create(&path, IndexId::new_v4()),
        Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == orphan
    ));
    assert!(!path.exists());
    assert_eq!(fs::read(orphan).unwrap(), b"orphan must survive");
}

#[cfg(unix)]
#[test]
fn valid_private_wal_and_shm_sidecars_are_accepted() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());

    let keeper = Connection::open(&path).unwrap();
    keeper
        .execute_batch(
            "BEGIN IMMEDIATE;
         UPDATE index_meta SET value = X'31' WHERE key = 'index_epoch';",
        )
        .unwrap();
    for suffix in ["-wal", "-shm"] {
        let sidecar = sidecar_path(&path, suffix);
        assert!(sidecar.exists());
        assert_eq!(
            fs::metadata(sidecar).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    database.verify_live_identity().unwrap();
    drop(database);
    keeper.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[test]
fn a_live_sqlite_handle_reports_main_file_rename_and_replacement() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    drop(IndexDb::create(&path, IndexId::new_v4()).unwrap());
    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();

    let moved = directory.path().join("moved.sqlite");
    fs::rename(&path, &moved).unwrap();
    fs::copy(&moved, &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(matches!(
        database.verify_live_identity(),
        Err(hsum::store::StoreError::UnsafeIndexPath(actual)) if actual == path
    ));
}

#[cfg(unix)]
#[test]
fn ancestor_aba_during_existing_open_cannot_substitute_or_mutate_database_b() {
    let directory = private_tempdir();
    let slot_a = directory.path().join("slot-a");
    let slot_b = directory.path().join("slot-b");
    let held_a = directory.path().join("held-a");
    private_directory(&slot_a);
    private_directory(&slot_b);
    let path_a = slot_a.join("index.sqlite");
    let path_b = slot_b.join("index.sqlite");
    drop(IndexDb::create(&path_a, IndexId::new_v4()).unwrap());
    drop(IndexDb::create(&path_b, IndexId::new_v4()).unwrap());
    let original_b = fs::read(&path_b).unwrap();

    let error = IndexDb::open_existing_with_observer(&path_a, OpenMode::ReadWrite, |checkpoint| {
        match checkpoint {
            "before_sqlite_open" => {
                fs::rename(&slot_a, &held_a).unwrap();
                fs::rename(&slot_b, &slot_a).unwrap();
            }
            "after_sqlite_open" => {
                fs::rename(&slot_a, &slot_b).unwrap();
                fs::rename(&held_a, &slot_a).unwrap();
            }
            _ => {}
        }
    })
    .err()
    .expect("ancestor ABA must abort the open");

    assert!(matches!(
        error,
        hsum::store::StoreError::UnsafeIndexPath(actual) if actual == path_a
    ));
    assert_eq!(fs::read(path_b).unwrap(), original_b);
}

#[cfg(unix)]
#[test]
fn ancestor_aba_during_create_cannot_substitute_or_mutate_database_b() {
    let directory = private_tempdir();
    let slot_a = directory.path().join("slot-a");
    let slot_b = directory.path().join("slot-b");
    let held_a = directory.path().join("held-a");
    private_directory(&slot_a);
    private_directory(&slot_b);
    let path_a = slot_a.join("index.sqlite");
    let path_b = slot_b.join("index.sqlite");
    drop(IndexDb::create(&path_b, IndexId::new_v4()).unwrap());
    let original_b = fs::read(&path_b).unwrap();

    let error =
        IndexDb::create_with_observer(&path_a, IndexId::new_v4(), |checkpoint| match checkpoint {
            "before_sqlite_open" => {
                fs::rename(&slot_a, &held_a).unwrap();
                fs::rename(&slot_b, &slot_a).unwrap();
            }
            "after_sqlite_open" => {
                fs::rename(&slot_a, &slot_b).unwrap();
                fs::rename(&held_a, &slot_a).unwrap();
            }
            _ => {}
        })
        .err()
        .expect("ancestor ABA must abort creation");

    assert!(matches!(
        error,
        hsum::store::StoreError::UnsafeIndexPath(actual) if actual == path_a
    ));
    assert!(!path_a.exists());
    assert_eq!(fs::read(path_b).unwrap(), original_b);
}

#[cfg(unix)]
#[test]
fn failed_create_rollback_never_deletes_a_replaced_sidecar() {
    let directory = private_tempdir();
    let path = directory.path().join("index.sqlite");
    let wal = sidecar_path(&path, "-wal");
    let displaced_wal = directory.path().join("displaced-wal");
    let attacker_marker = b"replacement must survive rollback";

    let error = IndexDb::create_with_observer(&path, IndexId::new_v4(), |checkpoint| {
        if checkpoint == "after_creation_transaction" {
            fs::rename(&wal, &displaced_wal).unwrap();
            fs::write(&wal, attacker_marker).unwrap();
            fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
        }
    })
    .err()
    .expect("replacing a tracked WAL must abort creation");

    assert!(matches!(error, hsum::store::StoreError::UnsafeIndexPath(_)));
    assert!(!path.exists());
    assert_eq!(fs::read(wal).unwrap(), attacker_marker);
    assert!(displaced_wal.exists());
}
