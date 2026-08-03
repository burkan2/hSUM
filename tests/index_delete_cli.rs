use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use hsum::config::{ManagedPaths, TrustRegistry};
use hsum::store::ReaderLease;
use tempfile::{TempDir, tempdir};

fn hsum() -> &'static str {
    env!("CARGO_BIN_EXE_hsum")
}

fn run(home: &Path, current_dir: &Path, arguments: &[&str]) -> Output {
    Command::new(hsum())
        .args(arguments)
        .current_dir(current_dir)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .output()
        .unwrap()
}

fn repository(token: &str) -> TempDir {
    let repository = tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(
        repository.path().join("README.md"),
        format!("# Index deletion fixture\n{token}\n"),
    )
    .unwrap();
    repository
}

fn initialize(home: &Path, repository: &Path, index: &str, write_pointer: bool) {
    let mut arguments = vec!["init", "--index", index, "--project", "default"];
    if write_pointer {
        arguments.push("--write-pointer");
    }
    let output = run(home, repository, &arguments);
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_private_config(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn confirmed_deletion_clears_authority_but_preserves_other_indexes_and_pointer_hints() {
    let home = tempdir().unwrap();
    let doomed = repository("DOOMED_INDEX_TOKEN");
    let survivor = repository("SURVIVOR_INDEX_TOKEN");
    let unrelated = tempdir().unwrap();
    initialize(home.path(), doomed.path(), "doomed", true);
    initialize(home.path(), survivor.path(), "survivor", false);
    let paths = ManagedPaths::resolve(Some(home.path())).unwrap();
    let database = paths.index_database(&"doomed".parse().unwrap());
    let trust_before_invalid = fs::read(paths.trust_registry_file()).unwrap();
    write_private_config(&paths.config_file(), "not valid TOML = [\n");
    let invalid_config = run(
        home.path(),
        unrelated.path(),
        &["index", "delete", "doomed", "--confirm"],
    );
    assert_eq!(invalid_config.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_config.stderr).contains("CONFIG_INVALID"));
    assert!(database.exists());
    assert_eq!(
        fs::read(paths.trust_registry_file()).unwrap(),
        trust_before_invalid
    );
    assert_eq!(
        fs::read_to_string(paths.config_file()).unwrap(),
        "not valid TOML = [\n"
    );

    write_private_config(
        &paths.config_file(),
        concat!(
            "schema_version = 2\n",
            "config_epoch = 1\n",
            "default_index = \"doomed\"\n",
            "default_project = \"default\"\n",
        ),
    );
    let doomed_directory = database.parent().unwrap().to_path_buf();
    let quarantine = doomed_directory.parent().unwrap().join(".doomed.deleting");
    let config_before_missing = fs::read(paths.config_file()).unwrap();
    let trust_before_missing = fs::read(paths.trust_registry_file()).unwrap();

    let missing = run(
        home.path(),
        unrelated.path(),
        &["index", "delete", "absent", "--confirm"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("INDEX_NOT_FOUND"));
    assert_eq!(
        fs::read(paths.config_file()).unwrap(),
        config_before_missing
    );
    assert_eq!(
        fs::read(paths.trust_registry_file()).unwrap(),
        trust_before_missing
    );

    let reader = ReaderLease::acquire(&database, Duration::from_secs(1)).unwrap();
    let blocked = run(
        home.path(),
        unrelated.path(),
        &[
            "index",
            "delete",
            "doomed",
            "--confirm",
            "--lock-timeout-ms",
            "0",
        ],
    );
    assert_eq!(blocked.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("WRITER_LOCK"));
    assert!(database.exists());
    assert_eq!(
        fs::read(paths.config_file()).unwrap(),
        config_before_missing
    );
    assert_eq!(
        fs::read(paths.trust_registry_file()).unwrap(),
        trust_before_missing
    );
    drop(reader);

    let deleted = run(
        home.path(),
        unrelated.path(),
        &["index", "delete", "doomed", "--confirm"],
    );
    assert!(
        deleted.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let stdout = String::from_utf8_lossy(&deleted.stdout);
    assert!(stdout.starts_with("Managed index deleted."));
    assert!(stdout.contains("Trust bindings removed: 1"));
    assert!(stdout.contains("Configured default cleared: yes"));
    assert!(stdout.contains("Interrupted deletion resumed: no"));
    assert!(!doomed_directory.exists());
    assert!(!quarantine.exists());
    assert!(doomed.path().join(".hsum.toml").exists());

    let config: toml::Value =
        toml::from_str(&fs::read_to_string(paths.config_file()).unwrap()).unwrap();
    assert_eq!(config["schema_version"].as_integer(), Some(2));
    assert_eq!(config["config_epoch"].as_integer(), Some(2));
    assert!(config.get("default_index").is_none());
    assert!(config.get("default_project").is_none());
    let registry = TrustRegistry::load(&paths.trust_registry_file()).unwrap();
    assert_eq!(registry.bindings().len(), 1);
    assert_eq!(registry.bindings()[0].index_name().as_str(), "survivor");

    let stale_pointer = run(home.path(), doomed.path(), &["context"]);
    assert_eq!(stale_pointer.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&stale_pointer.stderr).contains("PATH_TRUST"));
    let survivor_search = run(
        home.path(),
        survivor.path(),
        &["search", "SURVIVOR_INDEX_TOKEN", "--json"],
    );
    assert!(survivor_search.status.success());
    assert!(String::from_utf8_lossy(&survivor_search.stdout).contains("SURVIVOR_INDEX_TOKEN"));

    initialize(home.path(), doomed.path(), "doomed", false);
    assert!(paths.index_database(&"doomed".parse().unwrap()).exists());
}

#[test]
fn a_fixed_quarantine_makes_interrupted_directory_cleanup_resumable() {
    let home = tempdir().unwrap();
    let doomed = repository("RESUME_DELETE_TOKEN");
    initialize(home.path(), doomed.path(), "doomed", false);
    let paths = ManagedPaths::resolve(Some(home.path())).unwrap();
    let database = paths.index_database(&"doomed".parse().unwrap());
    let target = database.parent().unwrap();
    let quarantine = target.parent().unwrap().join(".doomed.deleting");
    fs::create_dir(&quarantine).unwrap();
    let trust_before_conflict = fs::read(paths.trust_registry_file()).unwrap();
    let conflict = run(
        home.path(),
        doomed.path(),
        &["index", "delete", "doomed", "--confirm"],
    );
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("CONFIG_INVALID"));
    assert!(target.exists());
    assert_eq!(
        fs::read(paths.trust_registry_file()).unwrap(),
        trust_before_conflict
    );
    fs::remove_dir(&quarantine).unwrap();
    fs::rename(target, &quarantine).unwrap();

    let resumed = run(
        home.path(),
        doomed.path(),
        &["index", "delete", "doomed", "--confirm"],
    );
    assert!(
        resumed.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("Interrupted deletion resumed: yes"));
    assert!(!target.exists());
    assert!(!quarantine.exists());
    assert!(
        TrustRegistry::load(&paths.trust_registry_file())
            .unwrap()
            .bindings()
            .is_empty()
    );
}
