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

fn fixture() -> (TempDir, TempDir, TempDir, TempDir) {
    let first = tempdir().unwrap();
    fs::create_dir(first.path().join(".git")).unwrap();
    fs::write(
        first.path().join("README.md"),
        "# First filesystem source\nFIRST_REGISTER_TOKEN\n",
    )
    .unwrap();

    let second = tempdir().unwrap();
    fs::create_dir(second.path().join(".git")).unwrap();
    fs::write(
        second.path().join("README.md"),
        "# Second filesystem source\nSECOND_REGISTER_TOKEN\n",
    )
    .unwrap();

    let third = tempdir().unwrap();
    fs::create_dir(third.path().join(".git")).unwrap();
    fs::write(
        third.path().join("README.md"),
        "# Third filesystem source\nTHIRD_REGISTER_TOKEN\n",
    )
    .unwrap();

    let home = tempdir().unwrap();
    let init = run(
        home.path(),
        first.path(),
        &["init", "--index", "fs-source", "--project", "default"],
    );
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    (first, second, third, home)
}

fn source<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == name)
        .unwrap()
}

#[test]
fn filesystem_registration_is_safe_idempotent_and_reused_on_activation() {
    let (first, second, third, home) = fixture();
    let status_before = json(home.path(), first.path(), &["status", "--json"]);
    let projects_before = json(home.path(), first.path(), &["project", "list", "--json"]);
    let context_before = json(home.path(), first.path(), &["context", "--json"]);

    let add = run(
        home.path(),
        first.path(),
        &[
            "source",
            "add",
            "fs",
            second.path().to_str().unwrap(),
            "--name",
            "docs-root",
        ],
    );
    assert!(
        add.status.success(),
        "source add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let add_stdout = String::from_utf8_lossy(&add.stdout);
    assert!(add_stdout.starts_with("Filesystem source configured."));
    assert!(add_stdout.contains("Attached: no"));
    assert!(add_stdout.contains("Activate: hsum project set-root"));

    let attached = json(home.path(), first.path(), &["source", "list", "--json"]);
    assert_eq!(attached["sources"].as_array().unwrap().len(), 1);
    let all = json(
        home.path(),
        first.path(),
        &["source", "list", "--all", "--json"],
    );
    assert_eq!(all["sources"].as_array().unwrap().len(), 2);
    let registered = source(&all, "docs-root");
    assert_eq!(registered["kind"], "filesystem");
    assert_eq!(
        registered["logical_uri"],
        fs::canonicalize(second.path()).unwrap().to_str().unwrap()
    );
    assert_eq!(registered["attached"], false);
    assert_eq!(registered["active_documents"], 0);
    let registered_id = registered["source_id"].as_str().unwrap().to_owned();

    let status_after = json(home.path(), first.path(), &["status", "--json"]);
    let projects_after = json(home.path(), first.path(), &["project", "list", "--json"]);
    let context_after = json(home.path(), first.path(), &["context", "--json"]);
    assert_eq!(status_after["index_epoch"], status_before["index_epoch"]);
    assert_eq!(
        projects_after["projects"][0]["scope_revision"],
        projects_before["projects"][0]["scope_revision"]
    );
    assert_eq!(context_after["source_root"], context_before["source_root"]);
    assert!(
        json(
            home.path(),
            first.path(),
            &["search", "SECOND_REGISTER_TOKEN", "--json"],
        )["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let add_again = run(
        home.path(),
        first.path(),
        &[
            "source",
            "add",
            "fs",
            second.path().to_str().unwrap(),
            "--name",
            "docs-root",
        ],
    );
    assert!(add_again.status.success());
    assert!(
        String::from_utf8_lossy(&add_again.stdout)
            .starts_with("Filesystem source already configured.")
    );
    let all_again = json(
        home.path(),
        first.path(),
        &["source", "list", "--all", "--json"],
    );
    assert_eq!(source(&all_again, "docs-root")["source_id"], registered_id);

    for (path, name) in [
        (third.path(), "docs-root"),
        (second.path(), "different-name"),
    ] {
        let conflict = run(
            home.path(),
            first.path(),
            &[
                "source",
                "add",
                "fs",
                path.to_str().unwrap(),
                "--name",
                name,
            ],
        );
        assert_eq!(conflict.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&conflict.stderr).contains("CONFIG_INVALID"));
    }

    let broad = run(
        home.path(),
        first.path(),
        &["source", "add", "fs", "/", "--name", "broad-root"],
    );
    assert_eq!(broad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&broad.stderr).contains("BROAD_ROOT_CONFIRMATION_REQUIRED"));

    let activate = run(
        home.path(),
        first.path(),
        &[
            "project",
            "set-root",
            second.path().to_str().unwrap(),
            "--source-name",
            "docs-root",
            "--confirm",
        ],
    );
    assert!(
        activate.status.success(),
        "activation failed: {}",
        String::from_utf8_lossy(&activate.stderr)
    );
    assert!(String::from_utf8_lossy(&activate.stdout).contains(&registered_id));

    let selected = json(home.path(), second.path(), &["source", "list", "--json"]);
    assert_eq!(selected["sources"].as_array().unwrap().len(), 1);
    assert_eq!(source(&selected, "docs-root")["source_id"], registered_id);
    assert_eq!(source(&selected, "docs-root")["attached"], true);
    assert!(
        json(
            home.path(),
            second.path(),
            &["search", "SECOND_REGISTER_TOKEN", "--json"],
        )["results"]
            .as_array()
            .unwrap()
            .is_empty(),
        "activation must not ingest implicitly"
    );

    let ingest = run(
        home.path(),
        second.path(),
        &["ingest", "--source", &registered_id, "--strict"],
    );
    assert!(
        ingest.status.success(),
        "ingest failed: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    assert_eq!(
        json(
            home.path(),
            second.path(),
            &["search", "SECOND_REGISTER_TOKEN", "--json"],
        )["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let retire = run(
        home.path(),
        second.path(),
        &[
            "project",
            "set-root",
            third.path().to_str().unwrap(),
            "--source-name",
            "third-root",
            "--confirm",
        ],
    );
    assert!(retire.status.success());
    let status_before_reactivate = json(home.path(), third.path(), &["status", "--json"]);
    let projects_before_reactivate =
        json(home.path(), third.path(), &["project", "list", "--json"]);
    let reactivate = run(
        home.path(),
        third.path(),
        &[
            "source",
            "add",
            "fs",
            second.path().to_str().unwrap(),
            "--name",
            "docs-root",
        ],
    );
    assert!(reactivate.status.success());
    assert!(
        String::from_utf8_lossy(&reactivate.stdout).starts_with("Filesystem source reactivated.")
    );
    let reactivated = json(
        home.path(),
        third.path(),
        &["source", "list", "--all", "--json"],
    );
    assert_eq!(
        source(&reactivated, "docs-root")["source_id"],
        registered_id
    );
    assert_eq!(source(&reactivated, "docs-root")["attached"], false);
    assert_eq!(
        json(home.path(), third.path(), &["status", "--json"])["index_epoch"],
        status_before_reactivate["index_epoch"]
    );
    assert_eq!(
        json(home.path(), third.path(), &["project", "list", "--json"])["projects"][0]["scope_revision"],
        projects_before_reactivate["projects"][0]["scope_revision"]
    );
}
