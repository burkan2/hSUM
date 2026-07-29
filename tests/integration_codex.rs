#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use hsum::integration::{CodexIntegration, CodexRegistrationState, IntegrationError};
use tempfile::{TempDir, tempdir};

struct FakeCodex {
    root: TempDir,
    adapter: CodexIntegration,
    desired_hsum: PathBuf,
    state_file: PathBuf,
}

impl FakeCodex {
    fn new(state: &str) -> Self {
        let root = tempdir().unwrap();
        let executable = root.path().join("codex");
        let state_file = root.path().join("state");
        let command_file = root.path().join("command");
        let desired_hsum = root.path().join("hsum");
        fs::write(&state_file, state).unwrap();
        fs::write(&command_file, desired_hsum.as_os_str().as_encoded_bytes()).unwrap();
        fs::write(&executable, fake_codex_script()).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            adapter: CodexIntegration::from_executable(executable),
            desired_hsum,
            state_file,
        }
    }

    fn state(&self) -> String {
        fs::read_to_string(&self.state_file).unwrap()
    }

    fn run_hsum(&self, home: &PathBuf, current_dir: &PathBuf, arguments: &[&str]) -> Output {
        let mut paths = vec![self.root.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Command::new(env!("CARGO_BIN_EXE_hsum"))
            .args(arguments)
            .current_dir(current_dir)
            .env("HSUM_HOME", home)
            .env("CODEX_HOME", home.join("codex"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env_remove("HSUM_INDEX")
            .env_remove("HSUM_PROJECT")
            .output()
            .unwrap()
    }
}

#[test]
fn status_distinguishes_absent_exact_legacy_stale_and_conflicting_entries() {
    let absent = FakeCodex::new("absent");
    assert!(matches!(
        absent.adapter.status(&absent.desired_hsum).unwrap(),
        CodexRegistrationState::Absent
    ));

    let exact = FakeCodex::new("exact");
    assert!(matches!(
        exact.adapter.status(&exact.desired_hsum).unwrap(),
        CodexRegistrationState::Current(_)
    ));

    let legacy = FakeCodex::new("legacy");
    assert!(matches!(
        legacy.adapter.status(&legacy.desired_hsum).unwrap(),
        CodexRegistrationState::LegacyBinding(_)
    ));

    let stale = FakeCodex::new("stale");
    assert!(matches!(
        stale.adapter.status(&stale.desired_hsum).unwrap(),
        CodexRegistrationState::Stale(_)
    ));

    let conflict = FakeCodex::new("conflict");
    assert!(matches!(
        conflict.adapter.status(&conflict.desired_hsum).unwrap(),
        CodexRegistrationState::Conflict(_)
    ));

    let http = FakeCodex::new("http");
    assert!(matches!(
        http.adapter.status(&http.desired_hsum).unwrap(),
        CodexRegistrationState::Conflict(_)
    ));
}

#[test]
fn install_is_idempotent_and_migrates_owned_entries() {
    for (state, expected_label) in [
        ("absent", "absent"),
        ("legacy", "legacy_binding"),
        ("stale", "stale"),
    ] {
        let fake = FakeCodex::new(state);
        let changed = fake
            .adapter
            .install_or_repair(&fake.desired_hsum, false)
            .unwrap();
        assert!(changed.changed, "{state}");
        assert_eq!(changed.previous.label(), expected_label);
        assert!(matches!(
            fake.adapter.status(&fake.desired_hsum).unwrap(),
            CodexRegistrationState::Current(_)
        ));

        let unchanged = fake
            .adapter
            .install_or_repair(&fake.desired_hsum, false)
            .unwrap();
        assert!(!unchanged.changed, "{state}");
    }
}

#[test]
fn conflict_requires_an_explicit_override_and_uninstall_never_removes_it() {
    let fake = FakeCodex::new("conflict");
    assert!(matches!(
        fake.adapter.install_or_repair(&fake.desired_hsum, false),
        Err(IntegrationError::RegistrationConflict)
    ));
    assert_eq!(fake.state(), "conflict");
    assert!(matches!(
        fake.adapter.uninstall(&fake.desired_hsum),
        Err(IntegrationError::RegistrationConflict)
    ));
    assert_eq!(fake.state(), "conflict");

    let replaced = fake
        .adapter
        .install_or_repair(&fake.desired_hsum, true)
        .unwrap();
    assert!(replaced.changed);
    assert!(matches!(
        fake.adapter.status(&fake.desired_hsum).unwrap(),
        CodexRegistrationState::Current(_)
    ));
}

#[test]
fn malformed_write_failed_and_readback_mismatch_states_fail_closed() {
    let malformed = FakeCodex::new("malformed");
    assert!(matches!(
        malformed.adapter.status(&malformed.desired_hsum),
        Err(IntegrationError::InvalidRegistration(_))
    ));

    let write_failed = FakeCodex::new("write_failed");
    assert!(matches!(
        write_failed
            .adapter
            .install_or_repair(&write_failed.desired_hsum, false),
        Err(IntegrationError::CommandFailed {
            operation: "register",
            ..
        })
    ));

    let mismatch = FakeCodex::new("mismatch");
    assert!(matches!(
        mismatch
            .adapter
            .install_or_repair(&mismatch.desired_hsum, false),
        Err(IntegrationError::VerificationFailed)
    ));
}

#[test]
fn verification_retries_once_after_a_concurrent_rewrite() {
    let fake = FakeCodex::new("concurrent");
    let changed = fake
        .adapter
        .install_or_repair(&fake.desired_hsum, false)
        .unwrap();
    assert!(changed.changed);
    assert_eq!(fake.state(), "exact");
    assert!(matches!(
        fake.adapter.status(&fake.desired_hsum).unwrap(),
        CodexRegistrationState::Current(_)
    ));
}

#[test]
fn uninstall_is_owned_only_idempotent_and_keeps_external_state_out_of_output() {
    let fake = FakeCodex::new("exact");
    assert!(matches!(
        fake.adapter.uninstall(&fake.desired_hsum).unwrap(),
        CodexRegistrationState::Current(_)
    ));
    assert_eq!(fake.state(), "absent");
    assert!(matches!(
        fake.adapter.uninstall(&fake.desired_hsum).unwrap(),
        CodexRegistrationState::Absent
    ));
}

#[test]
fn real_hsum_process_onboards_two_repositories_with_one_global_registration() {
    let fake = FakeCodex::new("absent");
    let hsum_home = tempdir().unwrap();
    let codex_home = hsum_home.path().join("codex");
    fs::create_dir(&codex_home).unwrap();
    fs::write(codex_home.join("AGENTS.md"), "keep-existing-guidance\n").unwrap();
    let first_repository = tempdir().unwrap();
    fs::create_dir(first_repository.path().join(".git")).unwrap();
    fs::write(
        first_repository.path().join("first.md"),
        "first-repository-evidence\n",
    )
    .unwrap();

    let install = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &first_repository.path().to_path_buf(),
        &[
            "integration",
            "install",
            "codex",
            "--activate",
            ".",
            "--confirm",
        ],
    );
    assert!(
        install.status.success(),
        "install stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(install.stderr.is_empty());
    let install_output = String::from_utf8(install.stdout).unwrap();
    assert!(install_output.contains("hSUM is registered with Codex"));
    assert!(install_output.contains("Verified read-only tools: 4"));
    assert!(install_output.contains("Citation round trip: verified"));
    assert!(install_output.contains("Agent policy: current"));
    assert!(install_output.contains("READY FOR FUTURE TASKS"));
    assert_eq!(fake.state(), "exact");
    let policy_text = fs::read_to_string(codex_home.join("AGENTS.md")).unwrap();
    assert!(policy_text.starts_with("keep-existing-guidance\n"));
    assert!(policy_text.contains("hsum-agent-policy:v1"));

    let reinstall = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &first_repository.path().to_path_buf(),
        &[
            "integration",
            "install",
            "codex",
            "--activate",
            ".",
            "--confirm",
        ],
    );
    assert!(
        reinstall.status.success(),
        "reinstall stderr: {}",
        String::from_utf8_lossy(&reinstall.stderr)
    );
    assert!(String::from_utf8_lossy(&reinstall.stdout).contains("Registration changed: no"));

    let second_repository = tempdir().unwrap();
    fs::create_dir(second_repository.path().join(".git")).unwrap();
    fs::write(
        second_repository.path().join("second.md"),
        "second-repository-evidence\n",
    )
    .unwrap();
    let activate = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &[
            "integration",
            "activate",
            "codex",
            "--path",
            ".",
            "--confirm",
        ],
    );
    assert!(
        activate.status.success(),
        "activate stderr: {}",
        String::from_utf8_lossy(&activate.stderr)
    );
    let activate_output = String::from_utf8(activate.stdout).unwrap();
    assert!(activate_output.contains("hSUM is active for this repository"));
    assert!(activate_output.contains("Verified read-only tools: 4"));
    assert!(activate_output.contains("Citation round trip: verified"));
    assert!(activate_output.contains("READY NOW"));

    let status = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &["integration", "status", "codex", "--json"],
    );
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["registration"], "current");
    assert_eq!(status["registered_arguments"], serde_json::json!(["mcp"]));
    assert_eq!(status["agent_policy"], "current");
    assert_eq!(status["authorized_workspaces"], 0);
    assert_eq!(status["next_actions"], serde_json::json!([]));

    let workspace_parent = tempdir().unwrap();
    let workspace = workspace_parent.path().join("projects");
    fs::create_dir(&workspace).unwrap();
    let authorize = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &[
            "integration",
            "authorize-workspace",
            "codex",
            "--path",
            workspace.to_str().unwrap(),
            "--confirm",
        ],
    );
    assert!(authorize.status.success());
    assert!(String::from_utf8_lossy(&authorize.stdout).contains("Authorization changed: yes"));
    let authorized_status = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &["integration", "status", "codex", "--json"],
    );
    let authorized_status: serde_json::Value =
        serde_json::from_slice(&authorized_status.stdout).unwrap();
    assert_eq!(authorized_status["authorized_workspaces"], 1);

    let revoke = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &[
            "integration",
            "revoke-workspace",
            "codex",
            "--path",
            workspace.to_str().unwrap(),
            "--confirm",
        ],
    );
    assert!(revoke.status.success());
    assert!(String::from_utf8_lossy(&revoke.stdout).contains("Authorization removed: yes"));

    let uninstall = fake.run_hsum(
        &hsum_home.path().to_path_buf(),
        &second_repository.path().to_path_buf(),
        &["integration", "uninstall", "codex", "--confirm"],
    );
    assert!(uninstall.status.success());
    assert_eq!(fake.state(), "absent");
    assert_eq!(
        fs::read_to_string(codex_home.join("AGENTS.md")).unwrap(),
        "keep-existing-guidance\n"
    );
    assert!(
        hsum_home
            .path()
            .join("config/trusted-projects.toml")
            .is_file()
    );
    assert!(
        fs::read_dir(hsum_home.path().join("data/indexes"))
            .unwrap()
            .count()
            >= 2
    );
}

fn fake_codex_script() -> &'static [u8] {
    br#"#!/bin/sh
set -eu
root=$(dirname "$0")
state_file="$root/state"
command_file="$root/command"
mode=$(tr -d '\n' < "$state_file")

dynamic_entry() {
  command=$(cat "$command_file")
  printf '{"name":"hsum","enabled":true,"disabled_reason":null,"transport":{"type":"stdio","command":"%s","args":["mcp"],"env":null,"env_vars":[],"cwd":null},"enabled_tools":null,"disabled_tools":null,"startup_timeout_sec":null,"tool_timeout_sec":null}\n' "$command"
}

owned_entry() {
  printf '{"name":"hsum","enabled":true,"transport":{"type":"stdio","command":"/old/hsum","args":["mcp"],"env":null,"env_vars":[],"cwd":null},"enabled_tools":null,"disabled_tools":null}\n'
}

conflict_entry() {
  printf '{"name":"hsum","enabled":true,"transport":{"type":"stdio","command":"/opt/foreign-tool","args":["serve"],"env":null,"env_vars":[],"cwd":null},"enabled_tools":null,"disabled_tools":null}\n'
}

http_entry() {
  printf '{"name":"hsum","enabled":true,"transport":{"type":"streamable_http","url":"https://example.invalid/mcp"},"enabled_tools":null,"disabled_tools":null}\n'
}

if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "get" ]; then
  case "$mode" in
    absent)
      printf "Error: No MCP server named 'hsum' found.\n" >&2
      exit 1
      ;;
    exact)
      dynamic_entry
      ;;
    legacy)
      printf '{"name":"hsum","enabled":true,"transport":{"type":"stdio","command":"/old/hsum","args":["mcp","--binding","123e4567-e89b-42d3-a456-426614174000"],"env":null,"env_vars":[],"cwd":null},"enabled_tools":null,"disabled_tools":null}\n'
      ;;
    stale|write_failed|mismatch|concurrent)
      owned_entry
      ;;
    conflict)
      conflict_entry
      ;;
    http)
      http_entry
      ;;
    concurrent_conflict)
      printf 'concurrent_retry' > "$state_file"
      conflict_entry
      ;;
    malformed)
      printf '{'
      ;;
    *)
      printf 'unexpected fake state: %s\n' "$mode" >&2
      exit 2
      ;;
  esac
  exit 0
fi

if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "add" ]; then
  if [ "$mode" = "write_failed" ]; then
    printf 'simulated Codex write failure\n' >&2
    exit 1
  fi
  printf '%s' "${5:-}" > "$command_file"
  case "$mode" in
    mismatch)
      ;;
    concurrent)
      printf 'concurrent_conflict' > "$state_file"
      ;;
    concurrent_retry)
      printf 'exact' > "$state_file"
      ;;
    *)
      printf 'exact' > "$state_file"
      ;;
  esac
  printf "Added global MCP server 'hsum'.\n"
  exit 0
fi

if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "remove" ]; then
  printf 'absent' > "$state_file"
  printf "Removed global MCP server 'hsum'.\n"
  exit 0
fi

printf 'unsupported fake Codex invocation\n' >&2
exit 2
"#
}
