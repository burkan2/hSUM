use clap::{CommandFactory, Parser};
use hsum::cli::{
    AgentPolicyMode, Cli, ClientCommand, ClientKind, Command, CompletionShell, HelpCommand,
    IntegrationClient, IntegrationCommand, ProcessExitCategory, SearchMode, escape_terminal_bytes,
    escape_terminal_text, exit_code_for, render_completions, render_man, render_verbose_version,
    verbose_version_requested,
};
use proptest::prelude::*;

const CITATION: &str = concat!(
    "hsum://v1/",
    "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af/",
    "018f47f0-a032-7978-a41a-10d768f60755/",
    "018f47f0-a82c-76e0-8cc8-8922a657d632",
    "?rev=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "#bytes=120-480",
);

fn parse(arguments: &[&str]) -> Cli {
    Cli::try_parse_from(arguments).expect("arguments should satisfy the alpha.4 grammar")
}

fn assert_usage_error(arguments: &[&str]) {
    let error = Cli::try_parse_from(arguments).expect_err("arguments should be rejected");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn alpha4_command_inventory_is_exact_and_ordered() {
    let mut command = Cli::command();
    command.build();
    let names: Vec<_> = command
        .get_subcommands()
        .map(clap::Command::get_name)
        .collect();

    assert_eq!(
        names,
        [
            "init",
            "trust",
            "ingest",
            "search",
            "get",
            "status",
            "doctor",
            "context",
            "help",
            "client",
            "integration",
            "completions",
            "man",
            "mcp",
        ]
    );
}

#[test]
fn generated_help_has_only_alpha4_capabilities() {
    let mut command = Cli::command();
    let help = command.render_long_help().to_string();

    for available in [
        "init",
        "ingest",
        "search",
        "doctor",
        "completions",
        "mcp",
        "--no-progress",
        "--no-color",
        "--config",
        "--data-dir",
        "--cache-dir",
    ] {
        assert!(
            help.contains(available),
            "root help omitted alpha.4 surface {available:?}"
        );
    }
    for deferred in [
        "model install",
        "semantic",
        "hybrid",
        "reembed",
        "backup",
        "prune",
        "forget",
        "restore",
    ] {
        assert!(
            !help.contains(deferred),
            "root help leaked deferred surface {deferred:?}"
        );
    }
}

#[test]
fn help_version_and_global_flags_are_available_at_nested_commands() {
    for arguments in [
        &["hsum", "--help"][..],
        &["hsum", "--version"][..],
        &["hsum", "search", "--help"][..],
        &["hsum", "client", "config", "--help"][..],
    ] {
        let display =
            Cli::try_parse_from(arguments).expect_err("display action should short-circuit");
        assert_eq!(display.exit_code(), 0);
    }

    assert_usage_error(&["hsum", "help"]);
    assert_usage_error(&["hsum", "help", "search"]);
    assert_usage_error(&["hsum", "client", "help"]);
    assert_usage_error(&["hsum", "client", "help", "config"]);

    let parsed = parse(&[
        "hsum",
        "client",
        "doctor",
        "generic",
        "--verbose",
        "--verbose",
        "--quiet",
        "--no-progress",
        "--no-color",
        "--config",
        "/tmp/config.toml",
        "--data-dir",
        "/tmp/data",
        "--cache-dir",
        "/tmp/cache",
    ]);
    assert_eq!(parsed.global.verbose, 2);
    assert!(parsed.global.quiet);
    assert!(parsed.global.no_progress);
    assert!(parsed.global.no_color);
    assert_eq!(
        parsed.global.config.unwrap().to_string_lossy(),
        "/tmp/config.toml"
    );
    assert_eq!(
        parsed.global.data_dir.unwrap().to_string_lossy(),
        "/tmp/data"
    );
    assert_eq!(
        parsed.global.cache_dir.unwrap().to_string_lossy(),
        "/tmp/cache"
    );
}

#[test]
fn init_and_trust_signatures_enforce_explicit_confirmation() {
    let parsed = parse(&[
        "hsum",
        "init",
        "/work/repo",
        "--index",
        "main-index",
        "--project",
        "default",
        "--no-ingest",
        "--dry-run",
        "--write-pointer",
        "--force-pointer",
        "--allow-broad-root",
        "--allow-large-source",
        "--index-quota-bytes",
        "1073741824",
    ]);
    let Command::Init(arguments) = parsed.command else {
        panic!("expected init");
    };
    assert_eq!(arguments.path.unwrap().to_string_lossy(), "/work/repo");
    assert_eq!(arguments.index.unwrap().as_str(), "main-index");
    assert_eq!(arguments.project.unwrap().as_str(), "default");
    assert!(arguments.no_ingest);
    assert!(arguments.dry_run);
    assert!(arguments.write_pointer);
    assert!(arguments.force_pointer);
    assert!(arguments.allow_broad_root);
    assert!(arguments.allow_large_source);
    assert_eq!(arguments.index_quota_bytes, Some(1_073_741_824));

    let parsed = parse(&["hsum", "init", "/work/repo", "--rebuild", "--dry-run"]);
    let Command::Init(arguments) = parsed.command else {
        panic!("expected rebuild init");
    };
    assert!(arguments.rebuild);
    assert!(arguments.dry_run);

    assert_usage_error(&["hsum", "init", "--rebuild", "--no-ingest"]);
    assert_usage_error(&["hsum", "init", "--force-pointer"]);
    assert_usage_error(&["hsum", "trust", "/work/repo"]);

    let parsed = parse(&["hsum", "trust", "/work/repo", "--confirm"]);
    let Command::Trust(arguments) = parsed.command else {
        panic!("expected trust");
    };
    assert!(arguments.confirm);
}

#[test]
fn ingest_exposes_two_independent_deletion_guards() {
    let parsed = parse(&[
        "hsum",
        "ingest",
        "--dry-run",
        "--lock-timeout-ms",
        "60000",
        "--allow-empty-snapshot",
        "--allow-mass-delete",
    ]);
    let Command::Ingest(arguments) = parsed.command else {
        panic!("expected ingest");
    };
    assert!(arguments.dry_run);
    assert_eq!(arguments.lock_timeout_ms, 60_000);
    assert!(arguments.allow_empty_snapshot);
    assert!(arguments.allow_mass_delete);

    let parsed = parse(&["hsum", "ingest", "--allow-empty-snapshot"]);
    let Command::Ingest(arguments) = parsed.command else {
        panic!("expected ingest");
    };
    assert!(arguments.allow_empty_snapshot);
    assert!(!arguments.allow_mass_delete);
}

#[test]
fn numeric_bounds_are_rejected_by_the_parser_with_exit_two() {
    for arguments in [
        &["hsum", "search", "needle", "--limit", "0"][..],
        &["hsum", "search", "needle", "--limit", "51"][..],
        &["hsum", "search", "needle", "--timeout-ms", "99"][..],
        &["hsum", "search", "needle", "--timeout-ms", "10001"][..],
        &["hsum", "get", CITATION, "--max-bytes", "0"][..],
        &["hsum", "get", CITATION, "--max-bytes", "65537"][..],
        &["hsum", "ingest", "--lock-timeout-ms", "60001"][..],
    ] {
        assert_usage_error(arguments);
    }

    let parsed = parse(&[
        "hsum",
        "search",
        "needle",
        "--mode",
        "lexical",
        "--limit",
        "50",
        "--timeout-ms",
        "100",
    ]);
    let Command::Search(arguments) = parsed.command else {
        panic!("expected search");
    };
    assert_eq!(arguments.mode, SearchMode::Lexical);
    assert_eq!(arguments.limit, 50);
    assert_eq!(arguments.timeout_ms, 100);

    let parsed = parse(&["hsum", "get", CITATION, "--max-bytes", "65536"]);
    let Command::Get(arguments) = parsed.command else {
        panic!("expected get");
    };
    assert_eq!(arguments.max_bytes, 65_536);
    assert_eq!(arguments.citation_uri.to_string(), CITATION);
}

#[test]
fn get_rejects_noncanonical_citations_at_the_argument_boundary() {
    assert_usage_error(&["hsum", "get", "citation"]);
    assert_usage_error(&["hsum", "get", &CITATION.to_uppercase()]);
    parse(&["hsum", "get", CITATION]);
}

#[test]
fn defaults_match_the_alpha4_contract() {
    let parsed = parse(&["hsum", "search", "needle"]);
    let Command::Search(arguments) = parsed.command else {
        panic!("expected search");
    };
    assert_eq!(arguments.mode, SearchMode::Auto);
    assert_eq!(arguments.limit, 10);
    assert_eq!(arguments.timeout_ms, 3_000);

    let parsed = parse(&["hsum", "get", CITATION]);
    let Command::Get(arguments) = parsed.command else {
        panic!("expected get");
    };
    assert_eq!(arguments.max_bytes, 16_384);

    let parsed = parse(&["hsum", "ingest"]);
    let Command::Ingest(arguments) = parsed.command else {
        panic!("expected ingest");
    };
    assert_eq!(arguments.lock_timeout_ms, 5_000);
}

#[test]
fn alpha_search_and_read_only_doctor_reject_deferred_options() {
    assert_usage_error(&["hsum", "search", "needle", "--mode", "semantic"]);
    assert_usage_error(&["hsum", "search", "needle", "--mode", "hybrid"]);
    assert_usage_error(&["hsum", "search", "needle", "--embedding-model", "x"]);
    assert_usage_error(&["hsum", "ingest", "--reembed"]);
    assert_usage_error(&["hsum", "doctor", "--integrity"]);
    assert_usage_error(&["hsum", "doctor", "--repair"]);
    assert_usage_error(&["hsum", "doctor", "report"]);
    assert_usage_error(&["hsum", "model"]);
}

#[test]
fn client_and_mcp_shapes_are_tag_gated_and_project_safe() {
    let parsed = parse(&[
        "hsum",
        "client",
        "config",
        "claude-code",
        "--format",
        "toml",
    ]);
    let Command::Client(arguments) = parsed.command else {
        panic!("expected client");
    };
    let ClientCommand::Config(config) = arguments.command else {
        panic!("expected client config");
    };
    assert_eq!(config.client, ClientKind::ClaudeCode);

    parse(&[
        "hsum",
        "mcp",
        "--binding",
        "123e4567-e89b-42d3-a456-426614174000",
    ]);
    parse(&["hsum", "mcp", "--project", "default"]);
    assert_usage_error(&[
        "hsum",
        "mcp",
        "--binding",
        "123e4567-e89b-42d3-a456-426614174000",
        "--project",
        "default",
    ]);
}

#[test]
fn integration_shapes_require_confirmation_for_repository_and_client_mutations() {
    let parsed = parse(&[
        "hsum",
        "integration",
        "install",
        "codex",
        "--activate",
        ".",
        "--confirm",
    ]);
    let Command::Integration(arguments) = parsed.command else {
        panic!("expected integration");
    };
    let IntegrationCommand::Install(arguments) = arguments.command else {
        panic!("expected integration install");
    };
    assert_eq!(arguments.client, IntegrationClient::Codex);
    assert_eq!(arguments.activate.unwrap(), std::path::PathBuf::from("."));
    assert_eq!(arguments.agent_policy, AgentPolicyMode::Install);
    let skipped = parse(&[
        "hsum",
        "integration",
        "install",
        "codex",
        "--agent-policy",
        "skip",
    ]);
    let Command::Integration(skipped) = skipped.command else {
        panic!("expected integration");
    };
    let IntegrationCommand::Install(skipped) = skipped.command else {
        panic!("expected integration install");
    };
    assert_eq!(skipped.agent_policy, AgentPolicyMode::Skip);

    parse(&[
        "hsum",
        "integration",
        "activate",
        "codex",
        "--path",
        ".",
        "--confirm",
    ]);
    parse(&[
        "hsum",
        "integration",
        "authorize-workspace",
        "codex",
        "--path",
        "/work/projects",
        "--confirm",
    ]);
    parse(&[
        "hsum",
        "integration",
        "revoke-workspace",
        "codex",
        "--path",
        "/work/projects",
        "--confirm",
    ]);
    parse(&["hsum", "integration", "status", "codex", "--json"]);
    parse(&["hsum", "integration", "repair", "codex", "--confirm"]);
    parse(&["hsum", "integration", "uninstall", "codex", "--confirm"]);

    assert_usage_error(&["hsum", "integration", "install", "codex", "--activate", "."]);
    assert_usage_error(&["hsum", "integration", "activate", "codex"]);
    assert_usage_error(&[
        "hsum",
        "integration",
        "authorize-workspace",
        "codex",
        "--path",
        "/work/projects",
    ]);
    assert_usage_error(&[
        "hsum",
        "integration",
        "revoke-workspace",
        "codex",
        "--path",
        "/work/projects",
    ]);
    assert_usage_error(&["hsum", "integration", "repair", "codex"]);
    assert_usage_error(&["hsum", "integration", "uninstall", "codex"]);
}

#[test]
fn status_context_artifact_and_server_signatures_parse() {
    let parsed = parse(&["hsum", "status", "--problems", "--json"]);
    let Command::Status(arguments) = parsed.command else {
        panic!("expected status");
    };
    assert!(arguments.problems);
    assert!(arguments.json);

    let parsed = parse(&["hsum", "context", "--json"]);
    let Command::Context(arguments) = parsed.command else {
        panic!("expected context");
    };
    assert!(arguments.json);

    parse(&["hsum", "doctor"]);
    let parsed = parse(&["hsum", "help", "error", "PATH_TRUST"]);
    let Command::Help(arguments) = parsed.command else {
        panic!("expected offline help");
    };
    let HelpCommand::Error(arguments) = arguments.command;
    assert_eq!(arguments.subcode, "PATH_TRUST");
    parse(&["hsum", "completions", "powershell"]);
    parse(&["hsum", "man"]);
    parse(&["hsum", "mcp"]);
}

#[test]
fn completions_are_nonempty_deterministic_and_derived_from_alpha4_command() {
    for shell in [
        CompletionShell::Bash,
        CompletionShell::Zsh,
        CompletionShell::Fish,
        CompletionShell::PowerShell,
        CompletionShell::Elvish,
    ] {
        let first = render_completions(shell);
        let second = render_completions(shell);
        assert_eq!(first, second, "{shell} generation must be deterministic");
        assert!(!first.is_empty(), "{shell} completion output is empty");

        let text = String::from_utf8(first).expect("completion output must be UTF-8");
        assert!(text.contains("hsum"));
        assert!(text.contains("search"));
        assert!(
            !text.contains("semantic"),
            "{shell} completion leaked a deferred search mode"
        );
        assert!(
            !text.contains("_hsum__help"),
            "{shell} completion leaked Clap's implicit help subcommand"
        );
    }
}

#[test]
fn man_page_is_deterministic_and_derived_from_alpha4_command() {
    let first = render_man().expect("man generation should write to memory");
    let second = render_man().expect("man generation should write to memory");
    assert_eq!(first, second);

    let text = String::from_utf8(first).expect("man output must be UTF-8");
    let title = text.lines().find(|line| line.starts_with(".TH"));
    assert_eq!(title, Some(".TH hsum 1  \"hsum 0.1.0-alpha.4\" "));
    assert!(text.contains("Local evidence for agents."));
    assert!(text.contains("search"));
    assert!(!text.contains("semantic"));
    assert!(!text.contains("hsum-help(1)"));
}

#[test]
fn human_terminal_rendering_neutralizes_controls_and_preserves_printable_text() {
    let hostile = "repo/\u{1b}]0;owned\u{7}/line\nnext\r\t\u{202e}txt\u{2066}\u{feff}/café";
    let escaped = escape_terminal_text(hostile);

    assert_eq!(
        escaped,
        r"repo/\x1B]0;owned\x07/line\nnext\r\t\u{202E}txt\u{2066}\u{FEFF}/café"
    );
    assert!(!escaped.chars().any(char::is_control));

    assert_eq!(
        escape_terminal_bytes(b"valid\xff\x80name"),
        r"valid\xFF\x80name"
    );

    assert_eq!(
        escape_terminal_text(
            "\u{0378}\u{085f}\u{17b4}\u{110bd}\u{13430}\u{1bca0}\u{fe0f}\u{fdd0}\u{e000}"
        ),
        concat!(
            r"\u{0378}\u{085F}\u{17B4}\u{110BD}\u{13430}",
            r"\u{1BCA0}\u{FE0F}\u{FDD0}\u{E000}",
        )
    );
}

proptest! {
    #[test]
    fn escaped_byte_output_never_contains_terminal_control_characters(input in any::<Vec<u8>>()) {
        let escaped = escape_terminal_bytes(&input);
        prop_assert!(!escaped.chars().any(char::is_control));
        let has_dangerous_format_character = escaped.chars().any(|character| matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        ));
        prop_assert!(!has_dangerous_format_character);
    }
}

#[test]
fn verbose_version_report_is_golden_and_tag_gated() {
    assert_eq!(
        render_verbose_version(),
        concat!(
            "hsum 0.1.0-alpha.4\n",
            "API version: hsum.api.v1\n",
            "Readable schema versions: 1-1\n",
            "Writable schema versions: 1-1\n",
            "Target: ",
            env!("HSUM_TARGET_TRIPLE"),
            "\n",
            "Capabilities: filesystem-ingest, search-exact, search-bm25, evidence-get, \
             mcp-stdio-read-only\n",
        )
    );

    let rendered = render_verbose_version();
    for deferred in ["semantic", "hybrid", "vector", "jsonl", "watch", "model"] {
        assert!(
            !rendered.contains(deferred),
            "verbose version leaked deferred capability {deferred:?}"
        );
    }
}

#[test]
fn verbose_version_detection_matches_the_documented_grammar() {
    assert!(verbose_version_requested(["--version", "--verbose"]));
    assert!(verbose_version_requested(["--verbose", "--version"]));
    assert!(verbose_version_requested(["--version", "-v"]));
    assert!(verbose_version_requested(["--version", "-vv"]));

    assert!(!verbose_version_requested(["--version"]));
    assert!(!verbose_version_requested(["--version", "--quiet"]));
    assert!(!verbose_version_requested(["--version", "-q"]));
    assert!(!verbose_version_requested(["--version", "-"]));
    assert!(!verbose_version_requested(std::iter::empty::<&str>()));
}

#[test]
fn version_verbose_process_output_matches_the_golden_report() {
    let verbose = std::process::Command::new(env!("CARGO_BIN_EXE_hsum"))
        .args(["--version", "--verbose"])
        .output()
        .expect("the hsum binary should run");
    assert!(verbose.status.success());
    assert_eq!(
        String::from_utf8(verbose.stdout).expect("verbose version output must be UTF-8"),
        render_verbose_version()
    );
    assert!(verbose.stderr.is_empty());

    let plain = std::process::Command::new(env!("CARGO_BIN_EXE_hsum"))
        .arg("--version")
        .output()
        .expect("the hsum binary should run");
    assert!(plain.status.success());
    assert_eq!(
        String::from_utf8(plain.stdout).expect("version output must be UTF-8"),
        "hsum 0.1.0-alpha.4\n"
    );
    assert!(plain.stderr.is_empty());
}

#[test]
fn process_exit_mapping_is_complete_and_stable() {
    assert_eq!(exit_code_for(ProcessExitCategory::Success), 0);
    assert_eq!(exit_code_for(ProcessExitCategory::Failure), 1);
    assert_eq!(exit_code_for(ProcessExitCategory::InvalidArguments), 2);
    assert_eq!(exit_code_for(ProcessExitCategory::DependencyUnavailable), 3);
    assert_eq!(exit_code_for(ProcessExitCategory::BusyOrTimeout), 4);
    assert_eq!(exit_code_for(ProcessExitCategory::CorruptOrIncompatible), 5);
    assert_eq!(exit_code_for(ProcessExitCategory::PartialIngest), 6);
    assert_eq!(exit_code_for(ProcessExitCategory::PermissionOrResource), 7);
    assert_eq!(exit_code_for(ProcessExitCategory::Interrupted), 130);
}
