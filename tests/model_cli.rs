use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::tempdir;

const MODEL_ID: &str = "bge-small-en-v1-5-fp32";

fn run(home: &Path, current_dir: &Path, arguments: &[&str], offline: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hsum"));
    command
        .args(arguments)
        .current_dir(current_dir)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT");
    if offline {
        command.env("HSUM_OFFLINE", "1");
    } else {
        command.env_remove("HSUM_OFFLINE");
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn list_is_local_and_reports_the_exact_embedded_profile() {
    let home = tempdir().unwrap();
    let working = tempdir().unwrap();
    let listed = run(
        home.path(),
        working.path(),
        &["model", "list", "--json"],
        true,
    );
    assert!(listed.status.success(), "{}", stderr(&listed));
    let output: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(output["schema_version"], "hsum.model-list.v1");
    assert_eq!(output["selected_index"], Value::Null);
    assert_eq!(output["models"].as_array().unwrap().len(), 1);
    assert_eq!(output["models"][0]["id"], MODEL_ID);
    assert_eq!(output["models"][0]["state"], "missing");
    assert_eq!(output["models"][0]["dimension"], 384);
    assert_eq!(output["models"][0]["license_id"], "MIT");
    assert_eq!(output["models"][0]["expected_bytes"], 133_806_060);
    assert_eq!(
        output["models"][0]["manifest_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(!home.path().join("cache/models").exists());
}

#[test]
fn offline_install_fails_before_creating_a_partial_artifact() {
    let home = tempdir().unwrap();
    let working = tempdir().unwrap();
    let installed = run(
        home.path(),
        working.path(),
        &["model", "install", "embedding", MODEL_ID, "--json"],
        true,
    );
    assert_eq!(installed.status.code(), Some(3), "{}", stderr(&installed));
    let error: Value = serde_json::from_slice(&installed.stderr).unwrap();
    assert_eq!(error["code"], "MODEL_MISSING");
    assert_eq!(error["subcode"], "MODEL_NOT_INSTALLED");
    assert!(
        error["details"]["reason"]
            .as_str()
            .unwrap()
            .contains("HSUM_OFFLINE=1")
    );
    assert!(!home.path().join("cache/models").exists());
}

#[test]
fn airgapped_import_rejects_manifest_dimension_drift_before_copying() {
    let home = tempdir().unwrap();
    let working = tempdir().unwrap();
    let artifact = working.path().join("artifact");
    fs::create_dir(&artifact).unwrap();
    let mut receipt: Value =
        serde_json::from_str(include_str!("../assets/models/bge-small-en-v1.5-fp32.json")).unwrap();
    receipt["dimension"] = json!(768);
    fs::write(
        artifact.join("hsum-model.json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();

    let imported = run(
        home.path(),
        working.path(),
        &["model", "import", "artifact", "--json"],
        true,
    );
    assert_eq!(imported.status.code(), Some(3), "{}", stderr(&imported));
    let error: Value = serde_json::from_slice(&imported.stderr).unwrap();
    assert_eq!(error["code"], "MODEL_INCOMPATIBLE");
    assert_eq!(error["subcode"], "MODEL_DIMENSION");
    assert!(!home.path().join("cache/models").exists());
}
