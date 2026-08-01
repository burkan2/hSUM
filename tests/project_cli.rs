use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

fn json(home: &Path, repository: &Path, arguments: &[&str]) -> Value {
    let output = run(home, repository, arguments);
    assert!(
        output.status.success(),
        "command {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn search(home: &Path, repository: &Path, query: &str) -> Value {
    json(home, repository, &["search", query, "--json"])
}

fn fixture() -> (TempDir, TempDir, TempDir, TempDir) {
    let first = tempdir().unwrap();
    fs::create_dir(first.path().join(".git")).unwrap();
    fs::write(
        first.path().join("README.md"),
        "# First project root\nFIRST_ROOT_TOKEN\n",
    )
    .unwrap();
    let second = tempdir().unwrap();
    fs::create_dir(second.path().join(".git")).unwrap();
    fs::write(
        second.path().join("README.md"),
        "# Second project root\nSECOND_ROOT_TOKEN\n",
    )
    .unwrap();
    let snapshots = tempdir().unwrap();
    let home = tempdir().unwrap();
    let init = run(
        home.path(),
        first.path(),
        &["init", "--index", "projects", "--project", "default"],
    );
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    (first, second, snapshots, home)
}

#[test]
fn named_projects_isolate_membership_switch_roots_and_preserve_history() {
    let (first, second, snapshots, home) = fixture();
    let jsonl = snapshots.path().join("records.jsonl");
    fs::write(
        &jsonl,
        "{\"id\":\"shared\",\"source_uri\":\"runbook://shared\",\"content\":\"SHARED_JSONL_TOKEN\"}\n",
    )
    .unwrap();
    let add = run(
        home.path(),
        first.path(),
        &[
            "source",
            "add",
            "jsonl",
            jsonl.to_str().unwrap(),
            "--name",
            "records",
        ],
    );
    assert!(add.status.success());
    assert!(
        run(home.path(), first.path(), &["ingest", "--strict"])
            .status
            .success()
    );

    let old_filesystem = search(home.path(), first.path(), "FIRST_ROOT_TOKEN");
    let old_citation = old_filesystem["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        search(home.path(), first.path(), "SHARED_JSONL_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let broad_root = run(
        home.path(),
        first.path(),
        &["project", "set-root", "/", "--confirm"],
    );
    assert_eq!(broad_root.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&broad_root.stderr).contains("BROAD_ROOT_CONFIRMATION_REQUIRED")
    );

    let sources_before = json(home.path(), first.path(), &["source", "list", "--json"]);
    let filesystem_name = sources_before["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["kind"] == "filesystem")
        .unwrap()["name"]
        .as_str()
        .unwrap();
    let attach_filesystem = run(
        home.path(),
        first.path(),
        &["source", "attach", filesystem_name, "--confirm"],
    );
    assert_eq!(attach_filesystem.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&attach_filesystem.stderr).contains("CONFIG_INVALID"));

    let create = run(home.path(), first.path(), &["project", "create", "docs"]);
    assert!(create.status.success());
    let create_again = run(home.path(), first.path(), &["project", "create", "docs"]);
    assert!(create_again.status.success());
    assert!(String::from_utf8_lossy(&create_again.stdout).starts_with("Project already exists."));
    let projects = json(home.path(), first.path(), &["project", "list", "--json"]);
    assert_eq!(projects["projects"].as_array().unwrap().len(), 2);
    assert_eq!(
        projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|project| project["selected"] == true)
            .count(),
        1
    );

    let before_detach = json(home.path(), first.path(), &["status", "--json"]);
    let detach = run(
        home.path(),
        first.path(),
        &["source", "detach", "records", "--confirm"],
    );
    assert!(detach.status.success());
    let detach_again = run(
        home.path(),
        first.path(),
        &["source", "detach", "records", "--confirm"],
    );
    assert!(detach_again.status.success());
    assert!(
        String::from_utf8_lossy(&detach_again.stdout).starts_with("JSONL source already detached")
    );
    let after_detach = json(home.path(), first.path(), &["status", "--json"]);
    assert_eq!(after_detach["index_epoch"], before_detach["index_epoch"]);
    assert!(
        search(home.path(), first.path(), "SHARED_JSONL_TOKEN")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let all_sources = json(
        home.path(),
        first.path(),
        &["source", "list", "--all", "--json"],
    );
    let records = all_sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == "records")
        .unwrap();
    assert_eq!(records["attached"], false);

    let use_docs = run(
        home.path(),
        first.path(),
        &["project", "use", "docs", "--confirm"],
    );
    assert!(use_docs.status.success());
    let attach = run(
        home.path(),
        first.path(),
        &["source", "attach", "records", "--confirm"],
    );
    assert!(attach.status.success());
    let attach_again = run(
        home.path(),
        first.path(),
        &["source", "attach", "records", "--confirm"],
    );
    assert!(attach_again.status.success());
    assert!(
        String::from_utf8_lossy(&attach_again.stdout).starts_with("JSONL source already attached")
    );
    assert_eq!(
        search(home.path(), first.path(), "SHARED_JSONL_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let use_default = run(
        home.path(),
        first.path(),
        &["project", "use", "default", "--confirm"],
    );
    assert!(use_default.status.success());
    let reattach_default = run(
        home.path(),
        first.path(),
        &["source", "attach", "records", "--confirm"],
    );
    assert!(reattach_default.status.success());

    let replace = run(
        home.path(),
        first.path(),
        &[
            "project",
            "set-root",
            second.path().to_str().unwrap(),
            "--source-name",
            "second-filesystem",
            "--confirm",
        ],
    );
    assert!(
        replace.status.success(),
        "set-root failed: {}",
        String::from_utf8_lossy(&replace.stderr)
    );
    assert!(
        run(home.path(), second.path(), &["ingest", "--strict"])
            .status
            .success()
    );
    let init_rerun = run(
        home.path(),
        second.path(),
        &["init", "--index", "projects", "--project", "default"],
    );
    assert!(
        init_rerun.status.success(),
        "init rerun after set-root failed: {}",
        String::from_utf8_lossy(&init_rerun.stderr)
    );
    assert!(
        String::from_utf8_lossy(&init_rerun.stdout).starts_with("Existing hSUM index selected.")
    );
    assert_eq!(
        search(home.path(), second.path(), "SECOND_ROOT_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        search(home.path(), second.path(), "FIRST_ROOT_TOKEN")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        search(home.path(), second.path(), "SHARED_JSONL_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let historical = run(
        home.path(),
        second.path(),
        &["get", &old_citation, "--json"],
    );
    assert!(
        historical.status.success(),
        "historical get failed: {}",
        String::from_utf8_lossy(&historical.stderr)
    );
    assert!(String::from_utf8_lossy(&historical.stdout).contains("FIRST_ROOT_TOKEN"));

    let switch_back = run(
        home.path(),
        second.path(),
        &["project", "use", "docs", "--confirm"],
    );
    assert!(switch_back.status.success());
    assert_eq!(
        search(home.path(), first.path(), "FIRST_ROOT_TOKEN")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        search(home.path(), first.path(), "SECOND_ROOT_TOKEN")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
