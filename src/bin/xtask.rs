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
    let steps: &[(&str, &[&str])] = &[
        ("fmt", &["fmt", "--all", "--", "--check"]),
        (
            "clippy",
            &[
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("test", &["test", "--all-targets", "--all-features"]),
        ("docs", &["test", "--doc", "--all-features"]),
    ];

    for (name, args) in steps {
        eprintln!("==> {name}");
        match Command::new("cargo").args(*args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status
                    .code()
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1);
                return ExitCode::from(code);
            }
            Err(error) => {
                eprintln!("failed to run cargo {name}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}
