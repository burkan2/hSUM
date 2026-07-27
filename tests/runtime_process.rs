use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use hsum::config::TrustRegistry;
use hsum::store::WriterLock;
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const RELEASED_ALPHA1_PIPELINE_FINGERPRINT: &str =
    "bb24fc64a8602c9ec0479ae687f848b7d5b029294796701966b7bbafc8a23bab";

fn hsum() -> &'static str {
    env!("CARGO_BIN_EXE_hsum")
}

fn repository() -> TempDir {
    let repository = tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::write(
        repository.path().join("notes.md"),
        b"# Runtime fixture\nalpha-beta local evidence\n",
    )
    .unwrap();
    repository
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

fn run_mcp_handshake(home: &Path, current_dir: &Path, arguments: &[&str]) -> Output {
    let mut child = Command::new(hsum())
        .args(arguments)
        .current_dir(current_dir)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"runtime-process-test","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
"#,
        )
        .unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn initialized() -> (TempDir, TempDir) {
    let repository = repository();
    let home = tempdir().unwrap();
    let output = run(
        home.path(),
        repository.path(),
        &["init", "--index", "runtime-fixture", "--project", "default"],
    );
    assert!(
        output.status.success(),
        "init stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Lexical search is ready"));
    (repository, home)
}

#[test]
fn init_search_get_status_context_and_doctor_are_process_complete() {
    let (repository, home) = initialized();

    let search = run(
        home.path(),
        repository.path(),
        &["search", "alpha-beta", "--json", "--explain"],
    );
    assert!(
        search.status.success(),
        "search stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    assert!(search.stderr.is_empty());
    let search_json: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search_json["schema_version"], "hsum.api.v1");
    assert_eq!(search_json["project"], "default");
    assert_eq!(search_json["results"].as_array().unwrap().len(), 1);
    assert_eq!(search_json["results"][0]["untrusted_content"], true);
    assert!(search_json["results"][0]["score"].is_object());
    let citation = search_json["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();

    let get = run(
        home.path(),
        repository.path(),
        &["get", &citation, "--verify-source-hash", "--json"],
    );
    assert!(
        get.status.success(),
        "get stderr: {}",
        String::from_utf8_lossy(&get.stderr)
    );
    assert!(get.stderr.is_empty());
    let get_json: Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(get_json["schema_version"], "hsum.api.v1");
    assert_eq!(get_json["requested_citation_uri"], citation);
    assert!(
        get_json["content"]
            .as_str()
            .unwrap()
            .contains("alpha-beta local evidence")
    );
    assert_eq!(get_json["source_hash_verification"], "unchanged");
    assert_eq!(get_json["untrusted_content"], true);

    fs::write(
        repository.path().join("notes.md"),
        b"# Runtime fixture\nalpha-beta newer local evidence\n",
    )
    .unwrap();
    let ingest = run(home.path(), repository.path(), &["ingest"]);
    assert!(
        ingest.status.success(),
        "ingest stderr: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let historical_get = run(
        home.path(),
        repository.path(),
        &["get", &citation, "--verify-source-hash", "--json"],
    );
    assert!(
        historical_get.status.success(),
        "historical get stderr: {}",
        String::from_utf8_lossy(&historical_get.stderr)
    );
    let historical_get: Value = serde_json::from_slice(&historical_get.stdout).unwrap();
    assert_eq!(historical_get["source_hash_verification"], "changed");

    let status = run(home.path(), repository.path(), &["status", "--json"]);
    assert!(status.status.success());
    assert!(status.stderr.is_empty());
    let status_json: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["active_documents"], 1);
    assert_eq!(status_json["active_passages"], 1);
    assert_eq!(status_json["database_read_only"], true);
    assert_eq!(status_json["query_only"], true);

    let problems = run(home.path(), repository.path(), &["status", "--problems"]);
    assert!(problems.status.success());
    assert!(problems.stderr.is_empty());
    assert_eq!(
        String::from_utf8(problems.stdout).unwrap(),
        "No actionable problems.\n"
    );

    let context = run(home.path(), repository.path(), &["context", "--json"]);
    assert!(context.status.success());
    assert!(context.stderr.is_empty());
    let context_json: Value = serde_json::from_slice(&context.stdout).unwrap();
    assert_eq!(context_json["index"], "runtime-fixture");
    assert_eq!(context_json["project"], "default");
    assert_eq!(context_json["selection_source"], "trusted_root");
    assert!(Path::new(context_json["database_path"].as_str().unwrap()).is_absolute());

    let doctor = run(home.path(), repository.path(), &["doctor"]);
    assert!(doctor.status.success());
    assert!(doctor.stderr.is_empty());
    let doctor_output = String::from_utf8_lossy(&doctor.stdout);
    assert!(doctor_output.contains("Index diagnosis passed"));
    assert!(doctor_output.contains("hard links cannot be distinguished"));
}

#[test]
fn stale_index_recovery_is_strict_dry_runnable_and_end_to_end() {
    let (repository, home) = initialized();
    let search = run(
        home.path(),
        repository.path(),
        &["search", "alpha-beta", "--json"],
    );
    assert!(search.status.success());
    let search_json: Value = serde_json::from_slice(&search.stdout).unwrap();
    let old_citation = search_json["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let trust_path = home.path().join("config/trusted-projects.toml");
    let registry_before = TrustRegistry::load(&trust_path).unwrap();
    let old_binding = registry_before.bindings()[0].binding_id();
    let database_path = home
        .path()
        .join("data/indexes/runtime-fixture/index.sqlite");
    let released_fingerprint = hex::decode(RELEASED_ALPHA1_PIPELINE_FINGERPRINT).unwrap();
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute(
            "UPDATE index_meta
             SET value = ?1
             WHERE key = 'pipeline_fingerprint'",
            [&released_fingerprint],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE generations
             SET pipeline_fingerprint = ?1",
            [&released_fingerprint],
        )
        .unwrap();
    drop(connection);

    for arguments in [
        &["search", "alpha-beta"][..],
        &["status"][..],
        &["context"][..],
        &["doctor"][..],
        &["ingest", "--dry-run"][..],
        &["ingest"][..],
        &["init"][..],
    ] {
        let output = run(home.path(), repository.path(), arguments);
        assert!(
            !output.status.success(),
            "{arguments:?} unexpectedly succeeded"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("code: PIPELINE_FINGERPRINT"),
            "{arguments:?} returned the wrong error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("hsum init --rebuild"),
            "{arguments:?} did not name the recovery command"
        );
    }

    let database_before = fs::read(&database_path).unwrap();
    let trust_before = fs::read(&trust_path).unwrap();
    let dry_run = run(
        home.path(),
        repository.path(),
        &["init", "--rebuild", "--dry-run"],
    );
    assert!(
        dry_run.status.success(),
        "rebuild dry-run stderr: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_text = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_text.contains("Would replace the trusted index"));
    assert!(dry_run_text.contains("Previous index ID"));
    assert_eq!(fs::read(&database_path).unwrap(), database_before);
    assert_eq!(fs::read(&trust_path).unwrap(), trust_before);

    let rebuilt = run(home.path(), repository.path(), &["init", "--rebuild"]);
    assert!(
        rebuilt.status.success(),
        "rebuild stderr: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    let rebuilt_text = String::from_utf8_lossy(&rebuilt.stdout);
    assert!(rebuilt_text.contains("hSUM index rebuilt"));
    assert!(rebuilt_text.contains("Prior evidence and citations no longer resolve"));

    let registry_after = TrustRegistry::load(&trust_path).unwrap();
    assert_eq!(registry_after.bindings().len(), 1);
    assert_ne!(registry_after.bindings()[0].binding_id(), old_binding);

    let new_search = run(
        home.path(),
        repository.path(),
        &["search", "alpha-beta", "--json"],
    );
    assert!(
        new_search.status.success(),
        "post-rebuild search stderr: {}",
        String::from_utf8_lossy(&new_search.stderr)
    );
    let old_get = run(
        home.path(),
        repository.path(),
        &["get", &old_citation, "--json"],
    );
    assert!(!old_get.status.success());
    let old_get_error: Value = serde_json::from_slice(&old_get.stderr).unwrap();
    assert_eq!(old_get_error["code"], "NOT_FOUND");
}

#[test]
fn dry_runs_plan_real_changes_without_advancing_the_index() {
    let source = repository();
    let dry_home = tempdir().unwrap();
    let init_dry_run = run(
        dry_home.path(),
        source.path(),
        &[
            "init",
            "--index",
            "dry-fixture",
            "--project",
            "default",
            "--dry-run",
            "--write-pointer",
        ],
    );
    assert!(init_dry_run.status.success());
    assert!(init_dry_run.stderr.is_empty());
    assert!(String::from_utf8_lossy(&init_dry_run.stdout).contains("no files were changed"));
    assert!(!source.path().join(".hsum.toml").exists());
    assert!(
        !dry_home
            .path()
            .join("data/indexes/dry-fixture/index.sqlite")
            .exists()
    );
    assert!(
        !dry_home
            .path()
            .join("config/trusted-projects.toml")
            .exists()
    );

    let (repository, home) = initialized();
    let before = run(home.path(), repository.path(), &["status", "--json"]);
    let before: Value = serde_json::from_slice(&before.stdout).unwrap();
    fs::write(
        repository.path().join("notes.md"),
        b"# Runtime fixture\nalpha-beta changed evidence\n",
    )
    .unwrap();
    let plan = run(
        home.path(),
        repository.path(),
        &[
            "ingest",
            "--dry-run",
            "--lock-timeout-ms",
            "0",
            "--allow-empty-snapshot",
            "--allow-mass-delete",
        ],
    );
    assert!(
        plan.status.success(),
        "plan stderr: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_text = String::from_utf8_lossy(&plan.stdout);
    assert!(plan_text.contains("Changed documents: 1"));
    assert!(plan_text.contains("Would create generation: yes"));
    assert!(plan_text.contains("no index data was changed"));

    let after = run(home.path(), repository.path(), &["status", "--json"]);
    let after: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["index_epoch"], before["index_epoch"]);
    assert_eq!(after["active_generation"], before["active_generation"]);
}

#[test]
fn artifacts_trust_and_client_config_keep_output_channels_clean() {
    let (repository, home) = initialized();

    for arguments in [
        &["completions", "bash"][..],
        &["completions", "zsh"][..],
        &["man"][..],
    ] {
        let output = run(home.path(), repository.path(), arguments);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }

    let config = run(
        home.path(),
        repository.path(),
        &["client", "config", "generic", "--format", "json"],
    );
    assert!(config.status.success());
    let warning = String::from_utf8_lossy(&config.stderr);
    assert!(warning.contains("uploads no corpus data or telemetry"));
    assert!(warning.contains("cloud-backed agent"));
    let config_json: Value = serde_json::from_slice(&config.stdout).unwrap();
    let executable = config_json["hsum"]["command"].as_str().unwrap();
    assert!(Path::new(executable).is_absolute());
    assert_eq!(config_json["hsum"]["args"][0], "mcp");
    assert_eq!(config_json["hsum"]["args"][1], "--binding");
    assert!(
        config_json["hsum"]["args"][2]
            .as_str()
            .unwrap()
            .parse::<uuid::Uuid>()
            .is_ok()
    );

    let toml_config = run(
        home.path(),
        repository.path(),
        &["client", "config", "codex", "--format", "toml"],
    );
    assert!(
        toml_config.status.success(),
        "TOML config stderr: {}",
        String::from_utf8_lossy(&toml_config.stderr)
    );
    assert!(String::from_utf8_lossy(&toml_config.stderr).contains("cloud-backed agent"));
    let toml_config: toml::Value =
        toml::from_str(&String::from_utf8(toml_config.stdout).unwrap()).unwrap();
    assert!(
        Path::new(
            toml_config["mcp_servers"]["hsum"]["command"]
                .as_str()
                .unwrap()
        )
        .is_absolute()
    );
    assert_eq!(
        toml_config["mcp_servers"]["hsum"]["args"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        toml_config["mcp_servers"]["hsum"]["args"][0]
            .as_str()
            .unwrap(),
        "mcp"
    );

    let client_doctor = run(
        home.path(),
        repository.path(),
        &["client", "doctor", "generic"],
    );
    assert!(
        client_doctor.status.success(),
        "client doctor stderr: {}",
        String::from_utf8_lossy(&client_doctor.stderr)
    );
    assert!(client_doctor.stderr.is_empty());
    assert!(
        String::from_utf8_lossy(&client_doctor.stdout)
            .contains("framing, and read-only probe are healthy")
    );

    let codex_doctor = run(
        home.path(),
        repository.path(),
        &["client", "doctor", "codex"],
    );
    assert!(
        codex_doctor.status.success(),
        "Codex client doctor stderr: {}",
        String::from_utf8_lossy(&codex_doctor.stderr)
    );
    assert!(codex_doctor.stderr.is_empty());
    assert!(
        String::from_utf8_lossy(&codex_doctor.stdout)
            .contains("framing, and read-only probe are healthy")
    );

    let pointer = run(home.path(), repository.path(), &["init", "--write-pointer"]);
    assert!(pointer.status.success());
    let trust = run(
        home.path(),
        repository.path(),
        &["trust", repository.path().to_str().unwrap(), "--confirm"],
    );
    assert!(
        trust.status.success(),
        "trust stderr: {}",
        String::from_utf8_lossy(&trust.stderr)
    );
    assert!(trust.stderr.is_empty());
    assert!(String::from_utf8_lossy(&trust.stdout).contains("Binding:"));
}

#[test]
fn errors_use_stderr_stable_exits_and_mcp_stdout_contains_only_frames() {
    let (repository, home) = initialized();

    let relative_override = run(
        home.path(),
        repository.path(),
        &["context", "--data-dir", "relative"],
    );
    assert_eq!(relative_override.status.code(), Some(2));
    assert!(relative_override.stdout.is_empty());
    let error = String::from_utf8_lossy(&relative_override.stderr);
    assert_eq!(error.lines().count(), 4);
    assert!(error.contains("PATH_INVALID"));
    assert!(error.contains("request: "));

    let malformed_config = home.path().join("config/malformed.toml");
    fs::write(&malformed_config, b"schema_version = [").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&malformed_config, fs::Permissions::from_mode(0o600)).unwrap();
    let untrusted_current = tempdir().unwrap();
    let malformed = run(
        home.path(),
        untrusted_current.path(),
        &[
            "context",
            "--json",
            "--config",
            malformed_config.to_str().unwrap(),
        ],
    );
    assert_eq!(malformed.status.code(), Some(2));
    assert!(malformed.stdout.is_empty());
    let malformed_error: Value = serde_json::from_slice(&malformed.stderr).unwrap();
    assert_eq!(malformed_error["code"], "INVALID_ARGUMENT");
    assert_eq!(malformed_error["subcode"], "CONFIG_INVALID");
    assert!(
        malformed_error["request_id"]
            .as_str()
            .is_some_and(|request_id| !request_id.is_empty())
    );

    let cursor = run(
        home.path(),
        repository.path(),
        &["search", "alpha-beta", "--cursor", "opaque", "--json"],
    );
    assert_eq!(cursor.status.code(), Some(2));
    assert!(cursor.stdout.is_empty());
    let cursor_error: Value = serde_json::from_slice(&cursor.stderr).unwrap();
    assert_eq!(cursor_error["code"], "INVALID_ARGUMENT");
    assert_eq!(cursor_error["subcode"], "QUERY_SYNTAX");
    assert_eq!(cursor_error["details"]["argument"], "cursor");

    let registry = TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    let binding = registry.bindings()[0].binding_id().to_string();
    let output = run_mcp_handshake(
        home.path(),
        repository.path(),
        &["mcp", "--binding", &binding],
    );
    assert!(
        output.status.success(),
        "MCP stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let frames = String::from_utf8(output.stdout).unwrap();
    let parsed = frames
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!parsed.is_empty());
    assert_eq!(parsed[0]["id"], 1);
    assert_eq!(parsed[0]["result"]["serverInfo"]["name"], "hsum");

    let project_output = run_mcp_handshake(
        home.path(),
        repository.path(),
        &["mcp", "--project", "default"],
    );
    assert!(
        project_output.status.success(),
        "project MCP stderr: {}",
        String::from_utf8_lossy(&project_output.stderr)
    );
    assert!(project_output.stderr.is_empty());
    for frame in String::from_utf8(project_output.stdout).unwrap().lines() {
        serde_json::from_str::<Value>(frame).unwrap();
    }

    let workspace_output = run_mcp_handshake(home.path(), repository.path(), &["mcp"]);
    assert!(
        workspace_output.status.success(),
        "workspace MCP stderr: {}",
        String::from_utf8_lossy(&workspace_output.stderr)
    );
    assert!(workspace_output.stderr.is_empty());
    let workspace_frames = String::from_utf8(workspace_output.stdout).unwrap();
    let workspace_initialized: Value =
        serde_json::from_str(workspace_frames.lines().next().unwrap()).unwrap();
    assert_eq!(workspace_initialized["id"], 1);
    assert_eq!(
        workspace_initialized["result"]["serverInfo"]["name"],
        "hsum"
    );
}

#[test]
fn broad_root_confirmation_and_persisted_quota_are_process_visible() {
    let repository = repository();
    let broad_home = tempdir().unwrap();
    let broad = run(
        broad_home.path(),
        repository.path(),
        &["init", "/", "--dry-run", "--index", "broad-root"],
    );
    assert_eq!(broad.status.code(), Some(2));
    assert!(broad.stdout.is_empty());
    let broad_error = String::from_utf8_lossy(&broad.stderr);
    assert_eq!(broad_error.lines().count(), 4);
    assert!(broad_error.contains("BROAD_ROOT_CONFIRMATION_REQUIRED"));
    assert!(broad_error.contains("request: "));

    let quota_home = tempdir().unwrap();
    let quota_bytes = "1000000000";
    let init = run(
        quota_home.path(),
        repository.path(),
        &[
            "init",
            "--index",
            "quota-fixture",
            "--project",
            "default",
            "--index-quota-bytes",
            quota_bytes,
        ],
    );
    assert!(
        init.status.success(),
        "quota init stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let context = run(quota_home.path(), repository.path(), &["context", "--json"]);
    assert!(context.status.success());
    let context: Value = serde_json::from_slice(&context.stdout).unwrap();
    assert_eq!(context["index_quota_bytes"], 1_000_000_000_u64);

    let human_context = run(quota_home.path(), repository.path(), &["context"]);
    assert!(human_context.status.success());
    assert!(
        String::from_utf8(human_context.stdout)
            .unwrap()
            .contains("Index quota bytes: 1000000000")
    );

    let status = run(quota_home.path(), repository.path(), &["status", "--json"]);
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["index_quota_bytes"], 1_000_000_000_u64);
    assert_eq!(status["storage"]["quota_bytes"], 1_000_000_000_u64);
    assert!(status["storage"]["managed_index_bytes"].is_u64());

    let human = run(quota_home.path(), repository.path(), &["status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Index quota bytes: 1000000000"));
    assert!(human.contains("Managed index bytes:"));
    assert!(human.contains("Recovery reserve bytes:"));
}

#[test]
fn lock_timeout_partial_exit_and_terminal_escaping_are_enforced_by_the_process() {
    let (repository, home) = initialized();
    let database_path = home
        .path()
        .join("data/indexes/runtime-fixture/index.sqlite");
    let lock = WriterLock::acquire(&database_path, Duration::ZERO).unwrap();
    let busy = run(
        home.path(),
        repository.path(),
        &["ingest", "--lock-timeout-ms", "0"],
    );
    assert_eq!(busy.status.code(), Some(4));
    assert!(busy.stdout.is_empty());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("WRITER_LOCK"));
    drop(lock);

    fs::write(repository.path().join("invalid.txt"), [0xff, 0xfe]).unwrap();
    fs::write(
        repository.path().join("hostile.txt"),
        b"hostile-token \x1b]0;owned\x07\nnext\n",
    )
    .unwrap();
    let partial = run(home.path(), repository.path(), &["ingest"]);
    assert_eq!(partial.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&partial.stdout).contains("Failed documents: 1"));
    assert!(String::from_utf8_lossy(&partial.stderr).starts_with("PARTIAL:"));

    let search = run(home.path(), repository.path(), &["search", "hostile-token"]);
    assert!(search.status.success());
    assert!(search.stderr.is_empty());
    assert!(!search.stdout.contains(&0x1b));
    assert!(!search.stdout.contains(&0x07));
    let output = String::from_utf8(search.stdout).unwrap();
    assert!(output.contains("\\x1B"));
    assert!(output.contains("\\x07"));
}

#[test]
fn all_source_failure_exits_one_without_activating_a_generation() {
    let (repository, home) = initialized();
    let before = run(home.path(), repository.path(), &["status", "--json"]);
    assert!(before.status.success());
    let before: Value = serde_json::from_slice(&before.stdout).unwrap();

    fs::write(repository.path().join("notes.md"), [0xff, 0xfe]).unwrap();
    let failed = run(home.path(), repository.path(), &["ingest"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&failed.stdout)
            .starts_with("Filesystem ingest failed; no generation was activated.\n")
    );
    assert!(String::from_utf8_lossy(&failed.stdout).contains(": failed (accepted 0, failed 1"));
    assert!(String::from_utf8_lossy(&failed.stderr).starts_with("FAILED:"));

    let after = run(home.path(), repository.path(), &["status", "--json"]);
    assert!(after.status.success());
    assert!(after.stderr.is_empty());
    let after: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["index_epoch"], before["index_epoch"]);
    assert_eq!(after["active_generation"], before["active_generation"]);
    assert_eq!(after["active_documents"], before["active_documents"]);
    assert_eq!(after["active_passages"], before["active_passages"]);
}

#[test]
fn offline_error_help_is_self_contained_and_linked_from_real_failures() {
    let repository = repository();
    let home = tempdir().unwrap();

    // Offline help needs no index, trust state, or network.
    let help = run(
        home.path(),
        repository.path(),
        &["help", "error", "QUERY_SYNTAX"],
    );
    assert!(
        help.status.success(),
        "help stderr: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(help.stderr.is_empty());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.starts_with("error: QUERY_SYNTAX\n"));
    for label in [
        "category: ",
        "retryable: ",
        "problem: ",
        "cause: ",
        "fix: ",
        "example: ",
    ] {
        assert!(text.contains(label), "offline help missing {label:?}");
    }
    assert!(!text.contains("https://"));

    let unknown = run(
        home.path(),
        repository.path(),
        &["help", "error", "NOT_A_SUBCODE"],
    );
    assert_eq!(unknown.status.code(), Some(2));
    assert!(unknown.stdout.is_empty());
    let unknown_error = String::from_utf8_lossy(&unknown.stderr);
    assert_eq!(unknown_error.lines().count(), 4);
    assert!(unknown_error.contains("learn: hsum help error "));

    // A real failure names the offline command above rather than a hosted URL.
    let (repository, home) = initialized();
    let failing = run(home.path(), repository.path(), &["search", "\"broken"]);
    assert_eq!(failing.status.code(), Some(2));
    let failure_text = String::from_utf8_lossy(&failing.stderr);
    assert!(failure_text.contains("learn: hsum help error QUERY_SYNTAX"));
    assert!(!failure_text.contains("https://"));
}

/// Drives one real `hsum mcp` subprocess: initialize handshake, then the
/// supplied tool-call frames. Keeps stdin open until every awaited response
/// id has arrived on stdout, so in-flight requests are answered rather than
/// cancelled by an early EOF.
fn run_mcp_tool_calls(
    home: &Path,
    current_dir: &Path,
    arguments: &[&str],
    tool_frames: &[Value],
    awaited_ids: &[u64],
) -> Vec<Value> {
    use std::io::BufRead;

    let mut child = Command::new(hsum())
        .args(arguments)
        .current_dir(current_dir)
        .env("HSUM_HOME", home)
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut script = String::from(concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"runtime-process-test","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
    ));
    for frame in tool_frames {
        script.push_str(&serde_json::to_string(frame).unwrap());
        script.push('\n');
    }
    stdin.write_all(script.as_bytes()).unwrap();
    stdin.flush().unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut frames = Vec::new();
    let mut outstanding = awaited_ids.to_vec();
    for line in std::io::BufReader::new(stdout).lines() {
        let frame: Value = serde_json::from_str(&line.unwrap()).unwrap();
        outstanding.retain(|id| frame["id"] != json!(id));
        frames.push(frame);
        if outstanding.is_empty() {
            break;
        }
    }
    assert!(outstanding.is_empty(), "missing responses: {outstanding:?}");
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    frames
}

fn frame_with_id(frames: &[Value], id: u64) -> &Value {
    frames
        .iter()
        .find(|frame| frame["id"] == json!(id))
        .unwrap_or_else(|| panic!("no response frame with id {id}"))
}

#[test]
fn cli_json_and_mcp_return_equivalent_evidence_on_one_shared_fixture() {
    let repository = tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    // Distinct match densities give a meaningful, non-trivial rank order that
    // both transports must reproduce exactly.
    fs::write(
        repository.path().join("guide.md"),
        b"# Guide\nparity-alpha appears here first.\nparity-alpha again.\nparity-alpha third.\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("notes.md"),
        b"# Notes\nparity-alpha appears once with beta context.\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("readme.md"),
        b"# Readme\nparity-alpha twice here.\nparity-alpha again here.\n",
    )
    .unwrap();
    let home = tempdir().unwrap();
    let init = run(
        home.path(),
        repository.path(),
        &["init", "--index", "parity-fixture", "--project", "default"],
    );
    assert!(
        init.status.success(),
        "init stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let cli = run(
        home.path(),
        repository.path(),
        &[
            "search",
            "parity-alpha",
            "--limit",
            "10",
            "--json",
            "--explain",
        ],
    );
    assert!(
        cli.status.success(),
        "search stderr: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_packet: Value = serde_json::from_slice(&cli.stdout).unwrap();

    let cli_error_run = run(
        home.path(),
        repository.path(),
        &["search", "\"parity-alpha", "--json"],
    );
    assert_eq!(cli_error_run.status.code(), Some(2));
    let cli_error: Value = serde_json::from_slice(&cli_error_run.stderr).unwrap();

    let registry = TrustRegistry::load(&home.path().join("config/trusted-projects.toml")).unwrap();
    let binding = registry.bindings()[0].binding_id().to_string();
    let frames = run_mcp_tool_calls(
        home.path(),
        repository.path(),
        &["mcp", "--binding", &binding],
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "evidence_search",
                    "arguments": {
                        "query": "parity-alpha",
                        "mode": "auto",
                        "limit": 10,
                        "timeout_ms": 10_000,
                        "explain": true
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "evidence_search",
                    "arguments": {"query": "\"parity-alpha"}
                }
            }),
        ],
        &[2, 3],
    );
    let mcp_packet = &frame_with_id(&frames, 2)["result"]["structuredContent"];
    let mcp_error = &frame_with_id(&frames, 3)["error"]["data"];

    for key in [
        "schema_version",
        "project_id",
        "scope_revision",
        "generation",
        "index_epoch",
        "requested_mode",
        "effective_mode",
        "retrievers",
        "stop_reason",
    ] {
        assert_eq!(cli_packet[key], mcp_packet[key], "top-level {key} parity");
    }
    assert_eq!(cli_packet["requested_mode"], "auto");
    assert_eq!(cli_packet["effective_mode"], "lexical");
    assert!(
        cli_packet["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        mcp_packet["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let cli_results = cli_packet["results"].as_array().unwrap();
    let mcp_results = mcp_packet["results"].as_array().unwrap();
    assert!(cli_results.len() >= 3, "fixture must rank multiple files");
    assert_eq!(cli_results.len(), mcp_results.len());
    for (cli_result, mcp_result) in cli_results.iter().zip(mcp_results) {
        for key in [
            "citation_uri",
            "index_id",
            "source_id",
            "document_id",
            "revision_sha256",
            "source_uri",
            "title",
            "content",
            "content_sha256",
            "source_updated_at",
            "indexed_at",
            "head_generation",
            "source_state",
            "untrusted_content",
            "duplicate_citations",
        ] {
            assert_eq!(cli_result[key], mcp_result[key], "passage {key} parity");
        }
        assert_eq!(cli_result["untrusted_content"], json!(true));
        assert_eq!(cli_result["source_state"], "metadata_unchanged");
        assert_eq!(
            cli_result["span"]["start_byte"],
            mcp_result["byte_span"]["start"]
        );
        assert_eq!(
            cli_result["span"]["end_byte"],
            mcp_result["byte_span"]["end"]
        );
        assert_eq!(
            cli_result["span"]["start_line"],
            mcp_result["line_span"]["start"]
        );
        assert_eq!(
            cli_result["span"]["end_line"],
            mcp_result["line_span"]["end"]
        );
        assert_eq!(
            cli_result["score"]["fused"], mcp_result["score"]["fused"],
            "fused score parity"
        );
        assert_eq!(
            cli_result["score"]["fusion_units"], mcp_result["score"]["fusion_units"],
            "fusion unit parity"
        );
        let cli_lists = cli_result["score"]["lists"].as_array().unwrap();
        let mcp_lists = mcp_result["score"]["lists"].as_array().unwrap();
        assert_eq!(cli_lists.len(), mcp_lists.len());
        for (cli_rank, mcp_rank) in cli_lists.iter().zip(mcp_lists) {
            assert_eq!(cli_rank["name"], mcp_rank["retriever"], "retriever parity");
            assert_eq!(cli_rank["rank"], mcp_rank["rank"]);
            assert_eq!(
                cli_rank["contribution_units"],
                mcp_rank["contribution_units"]
            );
            assert_eq!(cli_rank["backend_score"], mcp_rank["backend_score"]);
        }
    }

    for key in ["code", "subcode", "message", "retryable"] {
        assert_eq!(cli_error[key], mcp_error[key], "error {key} parity");
    }
    assert!(cli_error.get("docs_url").is_none());
    assert!(mcp_error.get("docs_url").is_none());
    assert_eq!(cli_error["code"], "INVALID_ARGUMENT");
    assert_eq!(cli_error["subcode"], "QUERY_SYNTAX");
    assert_eq!(cli_error["retryable"], json!(false));
}
