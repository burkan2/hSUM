use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

#[test]
fn process_migration_diagnoses_n_minus_one_and_preserves_exact_backups() {
    let home = tempdir().unwrap();
    let repository = tempdir().unwrap();
    let initialized = hsum(home.path(), repository.path(), &["init", "--no-ingest"]);
    assert!(initialized.status.success(), "{}", stderr(&initialized));

    let config_directory = home.path().join("config");
    let config_file = config_directory.join("config.toml");
    let trust_file = config_directory.join("trusted-projects.toml");
    let config_v1 = concat!(
        "schema_version = 1\n",
        "default_index = \"hsum\"\n",
        "default_project = \"default\"\n",
    )
    .as_bytes()
    .to_vec();
    write_private(&config_file, &config_v1);
    let trust_v2 = fs::read_to_string(&trust_file).unwrap();
    let trust_v1 = trust_v2
        .lines()
        .filter(|line| !line.starts_with("config_epoch = "))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("schema_version = 2", "schema_version = 1")
        + "\n";
    write_private(&trust_file, trust_v1.as_bytes());

    let diagnosed = hsum(home.path(), repository.path(), &["context"]);
    assert_eq!(diagnosed.status.code(), Some(5), "{}", stderr(&diagnosed));
    assert!(stderr(&diagnosed).contains("code: MIGRATION_REQUIRED"));
    assert_eq!(fs::read(&config_file).unwrap(), config_v1);
    assert_eq!(fs::read_to_string(&trust_file).unwrap(), trust_v1);

    let overlapping = hsum(
        home.path(),
        repository.path(),
        &[
            "config",
            "migrate",
            "plan",
            config_file.to_str().unwrap(),
            "--backup-dir",
            home.path().join("overlap-backup").to_str().unwrap(),
        ],
    );
    assert_eq!(
        overlapping.status.code(),
        Some(2),
        "{}",
        stderr(&overlapping)
    );
    assert_eq!(fs::read(&config_file).unwrap(), config_v1);

    let plan_file = home.path().join("config-migration.json");
    let backup_directory = home.path().join("config-before-v2");
    let planned = hsum(
        home.path(),
        repository.path(),
        &[
            "config",
            "migrate",
            "plan",
            plan_file.to_str().unwrap(),
            "--backup-dir",
            backup_directory.to_str().unwrap(),
        ],
    );
    assert!(planned.status.success(), "{}", stderr(&planned));
    assert!(!backup_directory.exists());
    let plan: Value = serde_json::from_slice(&fs::read(&plan_file).unwrap()).unwrap();
    let plan_hash = plan["plan_hash"].as_str().unwrap();

    let wrong = hsum(
        home.path(),
        repository.path(),
        &[
            "config",
            "migrate",
            "apply",
            plan_file.to_str().unwrap(),
            "--confirm",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ],
    );
    assert_eq!(wrong.status.code(), Some(2), "{}", stderr(&wrong));
    assert!(!backup_directory.exists());

    let applied = hsum(
        home.path(),
        repository.path(),
        &[
            "config",
            "migrate",
            "apply",
            plan_file.to_str().unwrap(),
            "--confirm",
            plan_hash,
        ],
    );
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert_eq!(
        fs::read(backup_directory.join("config.toml.bak")).unwrap(),
        config_v1
    );
    assert_eq!(
        fs::read_to_string(backup_directory.join("trusted-projects.toml.bak")).unwrap(),
        trust_v1
    );
    assert!(
        fs::read_to_string(&config_file)
            .unwrap()
            .contains("config_epoch = 1")
    );
    assert!(
        fs::read_to_string(&trust_file)
            .unwrap()
            .contains("config_epoch = 1")
    );

    let context = hsum(home.path(), repository.path(), &["context", "--json"]);
    assert!(context.status.success(), "{}", stderr(&context));
}

fn hsum(home: &Path, current_dir: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hsum"))
        .current_dir(current_dir)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .args(arguments)
        .output()
        .unwrap()
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
