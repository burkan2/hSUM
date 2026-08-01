use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
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
        "# Managed backups\nMANAGED_BACKUP_SECRET\n",
    )
    .unwrap();
    let home = tempdir().unwrap();
    let initialized = run(
        home.path(),
        repository.path(),
        &["init", "--index", "managed", "--project", "default"],
    );
    assert!(
        initialized.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    (repository, home)
}

fn active_citation(home: &Path, repository: &Path) -> String {
    let searched = run(
        home,
        repository,
        &["search", "MANAGED_BACKUP_SECRET", "--json"],
    );
    assert!(searched.status.success());
    let output: Value = serde_json::from_slice(&searched.stdout).unwrap();
    output["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn forget_plan(home: &Path, repository: &Path, citation: &str) -> (std::path::PathBuf, String) {
    let plan_path = repository.join("forget.json");
    let planned = run(
        home,
        repository,
        &[
            "forget",
            "plan",
            plan_path.to_str().unwrap(),
            "--citation",
            citation,
        ],
    );
    assert!(
        planned.status.success(),
        "forget plan failed: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    (plan_path, plan["plan_hash"].as_str().unwrap().to_owned())
}

fn inventory(home: &Path, repository: &Path) -> Value {
    let listed = run(home, repository, &["backup", "list", "--json"]);
    assert!(
        listed.status.success(),
        "backup list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    serde_json::from_slice(&listed.stdout).unwrap()
}

#[test]
fn occupied_unmanaged_output_is_not_adopted_into_the_registry() {
    let (repository, home) = fixture();
    let occupied = repository.path().join("user-file.sqlite");
    fs::write(&occupied, b"user-owned bytes").unwrap();
    let refused = run(
        home.path(),
        repository.path(),
        &["backup", "create", occupied.to_str().unwrap(), "--confirm"],
    );
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("MAINTENANCE_OUTPUT_EXISTS"));
    assert_eq!(fs::read(&occupied).unwrap(), b"user-owned bytes");
    assert!(
        inventory(home.path(), repository.path())["backups"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn forget_requires_an_explicit_keep_choice_and_reports_every_managed_backup() {
    let (repository, home) = fixture();
    let citation = active_citation(home.path(), repository.path());
    let manual = repository.path().join("manual.sqlite");
    let created = run(
        home.path(),
        repository.path(),
        &["backup", "create", manual.to_str().unwrap(), "--confirm"],
    );
    assert!(created.status.success());
    let registry_path = home.path().join("data/managed-backups.json");
    let mut interrupted_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    interrupted_registry["entries"][0]["receipt"] = Value::Null;
    fs::write(
        &registry_path,
        serde_json::to_vec(&interrupted_registry).unwrap(),
    )
    .unwrap();
    assert_eq!(
        inventory(home.path(), repository.path())["backups"][0]["state"],
        "pending-present"
    );
    let resumed = run(
        home.path(),
        repository.path(),
        &["backup", "create", manual.to_str().unwrap(), "--confirm"],
    );
    assert!(
        resumed.status.success(),
        "backup registration resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let before = inventory(home.path(), repository.path());
    assert_eq!(before["backups"].as_array().unwrap().len(), 1);
    assert_eq!(before["backups"][0]["kind"], "manual");
    assert_eq!(before["backups"][0]["state"], "verified");

    let (plan, hash) = forget_plan(home.path(), repository.path(), &citation);
    let recovery = repository.path().join("recovery.sqlite");
    let restore = repository.path().join("restore.json");
    let missing_choice = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            plan.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore.to_str().unwrap(),
            "--confirm",
            &hash,
        ],
    );
    assert_eq!(missing_choice.status.code(), Some(2));
    assert!(!recovery.exists());
    assert!(!restore.exists());
    assert!(
        run(home.path(), repository.path(), &["get", &citation])
            .status
            .success()
    );

    let kept = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            plan.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore.to_str().unwrap(),
            "--keep-managed-backups",
            "--confirm",
            &hash,
        ],
    );
    assert!(
        kept.status.success(),
        "forget keep failed: {}",
        String::from_utf8_lossy(&kept.stderr)
    );
    let output = String::from_utf8_lossy(&kept.stdout);
    assert!(output.contains("Managed backups retained: 2"));
    assert!(output.contains(manual.to_str().unwrap()));
    assert!(output.contains(recovery.to_str().unwrap()));
    assert!(manual.exists());
    assert!(recovery.exists());
    let after = inventory(home.path(), repository.path());
    assert_eq!(after["backups"].as_array().unwrap().len(), 2);
    assert!(
        after["backups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["state"] == "verified")
    );
    let deleted = run(
        home.path(),
        repository.path(),
        &["index", "delete", "managed", "--confirm"],
    );
    assert!(deleted.status.success());
    let retained_after_delete = run(
        home.path(),
        repository.path(),
        &["backup", "list", "--all", "--json"],
    );
    assert!(retained_after_delete.status.success());
    let retained_after_delete: Value =
        serde_json::from_slice(&retained_after_delete.stdout).unwrap();
    assert_eq!(
        retained_after_delete["backups"].as_array().unwrap().len(),
        2,
        "whole-index deletion must not discard retained backup inventory"
    );
}

#[test]
fn purge_removes_only_unchanged_managed_backups_and_clears_the_inventory() {
    let (repository, home) = fixture();
    let citation = active_citation(home.path(), repository.path());
    let managed = repository.path().join("managed.sqlite");
    assert!(
        run(
            home.path(),
            repository.path(),
            &["backup", "create", managed.to_str().unwrap(), "--confirm",],
        )
        .status
        .success()
    );
    let user_copy = repository.path().join("user-created-copy.sqlite");
    fs::copy(&managed, &user_copy).unwrap();
    let missing = repository.path().join("already-removed.sqlite");
    assert!(
        run(
            home.path(),
            repository.path(),
            &["backup", "create", missing.to_str().unwrap(), "--confirm",],
        )
        .status
        .success()
    );
    fs::remove_file(&missing).unwrap();
    let (plan, hash) = forget_plan(home.path(), repository.path(), &citation);
    let recovery = repository.path().join("recovery.sqlite");
    let restore = repository.path().join("restore.json");
    let purged = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            plan.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore.to_str().unwrap(),
            "--purge-managed-backups",
            "--confirm",
            &hash,
        ],
    );
    assert!(
        purged.status.success(),
        "forget purge failed: {}",
        String::from_utf8_lossy(&purged.stderr)
    );
    assert!(String::from_utf8_lossy(&purged.stdout).contains("Managed backups purged: 2"));
    assert!(
        String::from_utf8_lossy(&purged.stdout).contains("Missing inventory entries cleared: 1")
    );
    assert!(!managed.exists());
    assert!(!recovery.exists());
    assert!(
        user_copy.exists(),
        "user-created copies are outside hSUM scope"
    );
    assert!(restore.exists(), "the body-free audit plan is retained");
    assert_eq!(
        inventory(home.path(), repository.path())["backups"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let forgotten = run(home.path(), repository.path(), &["get", &citation]);
    assert_eq!(forgotten.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&forgotten.stderr).contains("FORGET_TOMBSTONE"));
}

#[test]
fn changed_backup_blocks_purge_before_forget_but_can_be_explicitly_retained() {
    let (repository, home) = fixture();
    let citation = active_citation(home.path(), repository.path());
    let managed = repository.path().join("changed.sqlite");
    assert!(
        run(
            home.path(),
            repository.path(),
            &["backup", "create", managed.to_str().unwrap(), "--confirm",],
        )
        .status
        .success()
    );
    OpenOptions::new()
        .append(true)
        .open(&managed)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert_eq!(
        inventory(home.path(), repository.path())["backups"][0]["state"],
        "changed"
    );
    let (plan, hash) = forget_plan(home.path(), repository.path(), &citation);
    let recovery = repository.path().join("recovery.sqlite");
    let restore = repository.path().join("restore.json");
    let refused = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            plan.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore.to_str().unwrap(),
            "--purge-managed-backups",
            "--confirm",
            &hash,
        ],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("MAINTENANCE_BACKUP_MISMATCH"),
        "unexpected refusal: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!recovery.exists());
    assert!(!restore.exists());
    assert!(
        run(home.path(), repository.path(), &["get", &citation])
            .status
            .success(),
        "purge preflight must precede the forget mutation"
    );

    let kept = run(
        home.path(),
        repository.path(),
        &[
            "forget",
            "apply",
            plan.to_str().unwrap(),
            "--recovery-backup",
            recovery.to_str().unwrap(),
            "--restore-plan",
            restore.to_str().unwrap(),
            "--keep-managed-backups",
            "--confirm",
            &hash,
        ],
    );
    assert!(
        kept.status.success(),
        "explicit keep failed: {}",
        String::from_utf8_lossy(&kept.stderr)
    );
    assert!(String::from_utf8_lossy(&kept.stdout).contains("State: changed"));
}
