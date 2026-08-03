use std::env;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("check"), None) => run_check(),
        _ => {
            eprintln!("usage: cargo xtask check");
            ExitCode::from(2)
        }
    }
}

fn run_check() -> ExitCode {
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
