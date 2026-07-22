use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    hsum::runtime::run_from_env().await
}
