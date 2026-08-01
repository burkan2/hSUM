use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn hsum() -> &'static str {
    env!("CARGO_BIN_EXE_hsum")
}

fn run(home: &Path, repository: &Path, arguments: &[&str]) -> Output {
    Command::new(hsum())
        .args(arguments)
        .current_dir(repository)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .output()
        .unwrap()
}

fn fixture() -> (TempDir, TempDir, TempDir) {
    let repository = tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(
        repository.path().join("README.md"),
        "# JSONL CLI fixture\nFILESYSTEM_AUTHORITY_TOKEN\n",
    )
    .unwrap();
    let home = tempdir().unwrap();
    let snapshots = tempdir().unwrap();
    let init = run(
        home.path(),
        repository.path(),
        &["init", "--index", "jsonl-cli", "--project", "default"],
    );
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    (repository, home, snapshots)
}

fn source_list(home: &Path, repository: &Path) -> Value {
    let output = run(home, repository, &["source", "list", "--json"]);
    assert!(
        output.status.success(),
        "source list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn source_id(list: &Value, name: &str) -> String {
    list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == name)
        .unwrap()["source_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn search(home: &Path, repository: &Path, query: &str) -> Value {
    let output = run(home, repository, &["search", query, "--json"]);
    assert!(
        output.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn status(home: &Path, repository: &Path) -> Value {
    let output = run(home, repository, &["status", "--json"]);
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn source_commands_drive_atomic_default_strict_and_removal_lifecycle() {
    let (repository, home, snapshots) = fixture();
    let good_path = snapshots.path().join("good.jsonl");
    let bad_path = snapshots.path().join("bad.jsonl");
    fs::write(
        &good_path,
        "{\"id\":\"good\",\"source_uri\":\"runbook://good\",\"content\":\"GOOD_OLD_TOKEN\"}\n",
    )
    .unwrap();
    fs::write(
        &bad_path,
        "{\"id\":\"bad\",\"source_uri\":\"runbook://bad\",\"content\":}\n",
    )
    .unwrap();

    for (path, name) in [(&good_path, "good"), (&bad_path, "bad")] {
        let added = run(
            home.path(),
            repository.path(),
            &[
                "source",
                "add",
                "jsonl",
                path.to_str().unwrap(),
                "--name",
                name,
            ],
        );
        assert!(
            added.status.success(),
            "source add failed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
    }

    let listed = source_list(home.path(), repository.path());
    assert_eq!(listed["sources"].as_array().unwrap().len(), 3);
    let good_id = source_id(&listed, "good");
    let bad_id = source_id(&listed, "bad");
    let filesystem_id = listed["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["kind"] == "filesystem")
        .unwrap()["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(good_id, bad_id);

    let conflict_path = snapshots.path().join("conflict.jsonl");
    fs::write(
        &conflict_path,
        "{\"id\":\"conflict\",\"source_uri\":\"u:conflict\",\"content\":\"conflict\"}\n",
    )
    .unwrap();
    let conflict = run(
        home.path(),
        repository.path(),
        &[
            "source",
            "add",
            "jsonl",
            conflict_path.to_str().unwrap(),
            "--name",
            "good",
        ],
    );
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("CONFIG_INVALID"));
    assert_eq!(
        source_list(home.path(), repository.path())["sources"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let remove_filesystem = run(
        home.path(),
        repository.path(),
        &["source", "remove", &filesystem_id, "--confirm"],
    );
    assert_eq!(remove_filesystem.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&remove_filesystem.stderr).contains("CONFIG_INVALID"));

    let added_again = run(
        home.path(),
        repository.path(),
        &[
            "source",
            "add",
            "jsonl",
            good_path.to_str().unwrap(),
            "--name",
            "good",
        ],
    );
    assert!(added_again.status.success());
    assert!(
        String::from_utf8_lossy(&added_again.stdout)
            .starts_with("JSONL source already configured.")
    );
    assert_eq!(
        source_id(&source_list(home.path(), repository.path()), "good"),
        good_id
    );

    let before_dry_run = status(home.path(), repository.path());
    let dry_run = run(home.path(), repository.path(), &["ingest", "--dry-run"]);
    assert!(
        dry_run.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_text = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_text.contains("Targeted sources: 3"));
    assert!(dry_run_text.contains("Failed sources: 1"));
    assert_eq!(
        status(home.path(), repository.path())["index_epoch"],
        before_dry_run["index_epoch"]
    );
    let after_dry_run = source_list(home.path(), repository.path());
    let bad_after_dry_run = after_dry_run["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == "bad")
        .unwrap();
    assert_eq!(bad_after_dry_run["last_error"], Value::Null);

    let strict_dry_run = run(
        home.path(),
        repository.path(),
        &["ingest", "--dry-run", "--strict"],
    );
    assert_eq!(strict_dry_run.status.code(), Some(1));
    assert_eq!(
        status(home.path(), repository.path())["index_epoch"],
        before_dry_run["index_epoch"]
    );

    let partial = run(home.path(), repository.path(), &["ingest"]);
    assert_eq!(partial.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&partial.stderr).starts_with("PARTIAL:"));
    let after_partial = source_list(home.path(), repository.path());
    let bad = after_partial["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == "bad")
        .unwrap();
    assert_eq!(bad["last_error"]["code"], "SOURCE_JSONL_INVALID");

    let old_search = search(home.path(), repository.path(), "GOOD_OLD_TOKEN");
    assert_eq!(old_search["results"].as_array().unwrap().len(), 1);
    assert_eq!(old_search["results"][0]["source_state"], "snapshot_only");
    let old_citation = old_search["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let before_strict = status(home.path(), repository.path());

    fs::write(
        &good_path,
        "{\"id\":\"good\",\"source_uri\":\"runbook://moved\",\"content\":\"GOOD_NEW_TOKEN\"}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("README.md"),
        "# JSONL CLI fixture\nFILESYSTEM_NEW_TOKEN\n",
    )
    .unwrap();
    let strict = run(home.path(), repository.path(), &["ingest", "--strict"]);
    assert_eq!(
        strict.status.code(),
        Some(1),
        "strict stderr: {}",
        String::from_utf8_lossy(&strict.stderr)
    );
    assert!(String::from_utf8_lossy(&strict.stderr).contains("ENUMERATION_INCOMPLETE"));
    let after_strict = status(home.path(), repository.path());
    assert_eq!(after_strict["index_epoch"], before_strict["index_epoch"]);
    assert_eq!(
        search(home.path(), repository.path(), "GOOD_NEW_TOKEN")["results"],
        Value::Array(vec![])
    );
    assert_eq!(
        search(home.path(), repository.path(), "GOOD_OLD_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search(home.path(), repository.path(), "FILESYSTEM_NEW_TOKEN")["results"],
        Value::Array(vec![])
    );
    assert_eq!(
        search(home.path(), repository.path(), "FILESYSTEM_AUTHORITY_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    fs::write(
        &bad_path,
        "{\"id\":\"bad\",\"source_uri\":\"runbook://bad\",\"content\":\"BAD_FIXED_TOKEN\"}\n",
    )
    .unwrap();
    let committed = run(home.path(), repository.path(), &["ingest", "--strict"]);
    assert!(
        committed.status.success(),
        "strict ingest failed: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert_eq!(
        search(home.path(), repository.path(), "GOOD_NEW_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search(home.path(), repository.path(), "BAD_FIXED_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search(home.path(), repository.path(), "FILESYSTEM_NEW_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let database_path = home.path().join("data/indexes/jsonl-cli/index.sqlite");
    let connection = Connection::open(&database_path).unwrap();
    let generation_count: i64 = connection
        .query_row(
            "SELECT COUNT(DISTINCT dh.generation_id)
             FROM document_heads AS dh
             JOIN documents AS d ON d.id = dh.document_id
             WHERE d.source_id IN (?1, ?2, ?3)",
            rusqlite::params![
                uuid::Uuid::parse_str(&good_id)
                    .unwrap()
                    .as_bytes()
                    .as_slice(),
                uuid::Uuid::parse_str(&bad_id)
                    .unwrap()
                    .as_bytes()
                    .as_slice(),
                uuid::Uuid::parse_str(&filesystem_id)
                    .unwrap()
                    .as_bytes()
                    .as_slice(),
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generation_count, 1);
    drop(connection);

    fs::write(
        &bad_path,
        "{\"id\":\"bad\",\"source_uri\":\"runbook://bad\",\"content\":}\n",
    )
    .unwrap();
    let before_targeted_failure = status(home.path(), repository.path());
    let failed = run(
        home.path(),
        repository.path(),
        &["ingest", "--source", &bad_id],
    );
    assert_eq!(failed.status.code(), Some(1));
    let after_targeted_failure = status(home.path(), repository.path());
    assert_eq!(
        after_targeted_failure["index_epoch"],
        before_targeted_failure["index_epoch"]
    );
    assert_eq!(
        search(home.path(), repository.path(), "BAD_FIXED_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let removed = run(
        home.path(),
        repository.path(),
        &["source", "remove", "good", "--confirm"],
    );
    assert!(
        removed.status.success(),
        "source remove failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert_eq!(
        search(home.path(), repository.path(), "GOOD_NEW_TOKEN")["results"],
        Value::Array(vec![])
    );
    assert!(
        source_list(home.path(), repository.path())["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["name"] != "good")
    );

    let historical = run(
        home.path(),
        repository.path(),
        &["get", &old_citation, "--json"],
    );
    assert!(historical.status.success());
    let historical: Value = serde_json::from_slice(&historical.stdout).unwrap();
    assert_eq!(historical["content"], "GOOD_OLD_TOKEN");
    assert_eq!(historical["source_state"], "snapshot_only");

    let reattached = run(
        home.path(),
        repository.path(),
        &[
            "source",
            "add",
            "jsonl",
            good_path.to_str().unwrap(),
            "--name",
            "good",
        ],
    );
    assert!(reattached.status.success());
    assert_eq!(
        source_id(&source_list(home.path(), repository.path()), "good"),
        good_id
    );
}
