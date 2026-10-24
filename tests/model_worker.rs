use std::io::Write;
use std::process::{Command, Stdio};

use hsum::model::builtin_manifests;
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn private_worker_is_offline_framed_and_reports_a_missing_verified_artifact() {
    let directory = tempdir().unwrap();
    let manifest = &builtin_manifests()[0];
    let request_id = Uuid::new_v4();
    let request = json!({
        "schema_version": "hsum.model-worker.v1",
        "request_id": request_id,
        "model_id": manifest.id,
        "model_fingerprint": manifest.fingerprint().unwrap(),
        "query": "semantic worker fixture"
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_hsum"))
        .arg("__model-worker")
        .arg("--cache-root")
        .arg(directory.path().join("offline-cache"))
        .env("HSUM_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    serde_json::to_writer(&mut stdin, &request).unwrap();
    stdin.write_all(b"\n").unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].is_empty());
    let response: serde_json::Value = serde_json::from_slice(lines[0]).unwrap();
    assert_eq!(response["schema_version"], "hsum.model-worker.v1");
    assert_eq!(response["request_id"], request_id.to_string());
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"], "model_missing");
}
