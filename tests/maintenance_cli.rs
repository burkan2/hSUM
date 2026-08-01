use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

fn run(home: &Path, repository: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hsum"))
        .args(arguments)
        .current_dir(repository)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .output()
        .unwrap()
}

fn fixture() -> (TempDir, TempDir) {
    let repository = tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repository.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(
        repository.path().join("README.md"),
        "# Maintenance\nOLD_MAINTENANCE_TOKEN\n",
    )
    .unwrap();
    let home = tempdir().unwrap();
    let init = run(
        home.path(),
        repository.path(),
        &["init", "--index", "maintenance", "--project", "default"],
    );
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    (repository, home)
}

#[test]
fn process_backup_and_prune_ceremony_invalidates_the_manifested_revision() {
    let (repository, home) = fixture();
    let search = run(
        home.path(),
        repository.path(),
        &["search", "OLD_MAINTENANCE_TOKEN", "--json"],
    );
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    let old_citation = search["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(
        repository.path().join("README.md"),
        "# Maintenance\nNEW_MAINTENANCE_TOKEN\n",
    )
    .unwrap();
    assert!(
        run(home.path(), repository.path(), &["ingest"])
            .status
            .success()
    );

    let standalone_backup = repository.path().join("standalone.sqlite");
    let backup = run(
        home.path(),
        repository.path(),
        &[
            "backup",
            "create",
            standalone_backup.to_str().unwrap(),
            "--confirm",
        ],
    );
    assert!(
        backup.status.success(),
        "backup failed: {}",
        String::from_utf8_lossy(&backup.stderr)
    );
    assert!(String::from_utf8_lossy(&backup.stdout).contains("Verified backup created."));

    let plan_path = repository.path().join("prune.json");
    let plan_output = run(
        home.path(),
        repository.path(),
        &[
            "prune",
            "plan",
            plan_path.to_str().unwrap(),
            "--before",
            "2099-01-01T00:00:00Z",
        ],
    );
    assert!(
        plan_output.status.success(),
        "plan failed: {}",
        String::from_utf8_lossy(&plan_output.stderr)
    );
    let plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let hash = plan["plan_hash"].as_str().unwrap();
    let citations = plan["plan"]["affected_revisions"][0]["canonical_stored_chunk_citations"]
        .as_array()
        .unwrap();
    assert!(citations.iter().any(|value| value == &old_citation));

    let pre_prune = repository.path().join("pre-prune.sqlite");
    let apply = run(
        home.path(),
        repository.path(),
        &[
            "prune",
            "apply",
            plan_path.to_str().unwrap(),
            "--backup",
            pre_prune.to_str().unwrap(),
            "--confirm",
            hash,
        ],
    );
    assert!(
        apply.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(String::from_utf8_lossy(&apply.stdout).contains("Rollback:"));
    assert!(
        run(home.path(), repository.path(), &["doctor"])
            .status
            .success()
    );
    let get = run(
        home.path(),
        repository.path(),
        &["get", &old_citation, "--json"],
    );
    assert_eq!(get.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&get.stderr).contains("EVIDENCE_NOT_FOUND"));
}

#[test]
fn process_migration_ceremony_upgrades_a_managed_n_minus_one_index() {
    let (repository, home) = fixture();
    let database = home.path().join("data/indexes/maintenance/index.sqlite");
    downgrade_to_schema_2(&database);
    let plan_path = repository.path().join("migration.json");
    let plan = run(
        home.path(),
        repository.path(),
        &[
            "migrate",
            "plan",
            "--index",
            "maintenance",
            plan_path.to_str().unwrap(),
        ],
    );
    assert!(
        plan.status.success(),
        "migration plan failed: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let hash = plan["plan_hash"].as_str().unwrap();
    assert_eq!(plan["plan"]["from_schema_version"], 2);
    assert_eq!(plan["plan"]["to_schema_version"], 3);

    let backup = repository.path().join("schema-2.sqlite");
    let apply = run(
        home.path(),
        repository.path(),
        &[
            "migrate",
            "apply",
            "--index",
            "maintenance",
            plan_path.to_str().unwrap(),
            "--backup",
            backup.to_str().unwrap(),
            "--confirm",
            hash,
        ],
    );
    assert!(
        apply.status.success(),
        "migration apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(String::from_utf8_lossy(&apply.stdout).contains("2 -> 3"));
    assert!(
        run(home.path(), repository.path(), &["doctor"])
            .status
            .success()
    );
}

#[test]
fn process_forget_and_immediate_restore_ceremony_round_trips_one_citation() {
    let (repository, home) = fixture();
    let search = run(
        home.path(),
        repository.path(),
        &["search", "OLD_MAINTENANCE_TOKEN", "--json"],
    );
    assert!(search.status.success());
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    let citation = search["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let forget_plan_path = repository.path().join("forget.json");
    let planned = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "plan",
            forget_plan_path.to_str().unwrap(),
            "--citation",
            &citation,
        ],
    );
    assert!(
        planned.status.success(),
        "forget plan failed: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let forget_plan: Value = serde_json::from_slice(&fs::read(&forget_plan_path).unwrap()).unwrap();
    let forget_hash = forget_plan["plan_hash"].as_str().unwrap();
    let recovery = repository.path().join("forget-recovery.sqlite");
    let restore_plan_path = repository.path().join("restore.json");
    let forgotten = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            forget_plan_path.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore_plan_path.to_str().unwrap(),
            "--keep-managed-backups",
            "--confirm",
            forget_hash,
        ],
    );
    assert!(
        forgotten.status.success(),
        "forget apply failed: {}",
        String::from_utf8_lossy(&forgotten.stderr)
    );
    let forgotten_output = String::from_utf8_lossy(&forgotten.stdout);
    assert!(forgotten_output.contains("retained managed backups may still contain"));
    let unavailable = run(
        home.path(),
        repository.path(),
        &["get", &citation, "--json"],
    );
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("FORGET_TOMBSTONE"));
    assert!(
        run(home.path(), repository.path(), &["doctor"])
            .status
            .success()
    );

    let restore_plan: Value =
        serde_json::from_slice(&fs::read(&restore_plan_path).unwrap()).unwrap();
    let restore_hash = restore_plan["plan_hash"].as_str().unwrap();
    let safety = repository.path().join("forgotten-state.sqlite");
    let restored = run(
        home.path(),
        repository.path(),
        &[
            "restore",
            "apply",
            restore_plan_path.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--safety-backup",
            safety.to_str().unwrap(),
            "--confirm",
            restore_hash,
        ],
    );
    assert!(
        restored.status.success(),
        "restore failed: {}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let available = run(
        home.path(),
        repository.path(),
        &["get", &citation, "--json"],
    );
    assert!(
        available.status.success(),
        "restored get failed: {}",
        String::from_utf8_lossy(&available.stderr)
    );
    let available: Value = serde_json::from_slice(&available.stdout).unwrap();
    assert!(
        available["content"]
            .as_str()
            .unwrap()
            .contains("OLD_MAINTENANCE_TOKEN")
    );
}

fn downgrade_to_schema_2(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute_batch(
            "DROP TABLE pruned_revision_namespaces;
             DROP TABLE prune_runs;
             DROP TABLE forgotten_documents;
             DROP TABLE forget_runs;
             DROP TABLE restore_runs;
             DELETE FROM index_meta WHERE key = 'history_floor_epoch';
             DELETE FROM index_meta WHERE key = 'replacement_epoch';
             DELETE FROM schema_migrations WHERE version = 3;",
        )
        .unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = CAST('2' AS BLOB) WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'schema_checksum'",
            params![schema_2_checksum().as_slice()],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
}

fn schema_2_checksum() -> [u8; 32] {
    let migrations = [
        (1_u32, include_str!("../migrations/0001_alpha1.sql")),
        (2_u32, include_str!("../migrations/0002_jsonl_sources.sql")),
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
