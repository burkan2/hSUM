use std::env;
use std::process::{Command, ExitCode};

#[path = "xtask/reference_docs.rs"]
mod reference_docs;

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    match args.as_slice() {
        ["check"] => run_check(),
        ["references"] => run_references(false),
        ["references", "--check"] => run_references(true),
        ["references", "--check-remote"] => run_remote_references(),
        _ => {
            eprintln!("usage: cargo xtask <check|references [--check|--check-remote]>");
            ExitCode::from(2)
        }
    }
}

fn run_check() -> ExitCode {
    eprintln!("==> generated references");
    if let Err(error) = reference_docs::check() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }

    let steps: &[(&str, &str, &[&str])] = &[
        ("fmt", "cargo", &["fmt", "--all", "--", "--check"]),
        (
            "clippy",
            "cargo",
            &[
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        (
            "test",
            "cargo",
            &["test", "--all-targets", "--all-features"],
        ),
        ("docs", "cargo", &["test", "--doc", "--all-features"]),
        (
            "evaluation unit tests",
            "python3",
            &["-m", "unittest", "-v", "eval/test_harness.py"],
        ),
        (
            "frozen evaluation inputs",
            "python3",
            &["eval/harness.py", "validate"],
        ),
        (
            "retrieval determinism unit tests",
            "python3",
            &[
                "-m",
                "unittest",
                "-v",
                "benches/retrieval_determinism/test_harness.py",
            ],
        ),
        (
            "frozen retrieval determinism inputs",
            "python3",
            &["benches/retrieval_determinism/harness.py", "validate"],
        ),
        (
            "retrieval scale unit tests",
            "python3",
            &[
                "-m",
                "unittest",
                "-v",
                "benches/retrieval_scale/test_harness.py",
            ],
        ),
        (
            "frozen retrieval scale inputs",
            "python3",
            &["benches/retrieval_scale/harness.py", "validate"],
        ),
        (
            "retrieval stress unit tests",
            "python3",
            &[
                "-m",
                "unittest",
                "-v",
                "benches/retrieval_stress/test_harness.py",
            ],
        ),
        (
            "frozen retrieval stress inputs",
            "python3",
            &["benches/retrieval_stress/harness.py", "validate"],
        ),
    ];

    for (name, program, args) in steps {
        eprintln!("==> {name}");
        match Command::new(program).args(*args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status
                    .code()
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1);
                return ExitCode::from(code);
            }
            Err(error) => {
                eprintln!("failed to run {name}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}

fn run_references(check: bool) -> ExitCode {
    let outcome = if check {
        reference_docs::check()
    } else {
        reference_docs::write()
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_remote_references() -> ExitCode {
    match reference_docs::check_remote() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
