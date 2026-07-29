use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use thiserror::Error;

const SERVER_NAME: &str = "hsum";
const MAX_CODEX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexRegistration {
    pub is_stdio: bool,
    pub command: PathBuf,
    pub arguments: Vec<String>,
    pub enabled: bool,
    pub cwd: Option<PathBuf>,
    pub has_environment: bool,
    pub has_tool_filters: bool,
}

impl CodexRegistration {
    fn desired(executable: &Path) -> Self {
        Self {
            is_stdio: true,
            command: executable.to_path_buf(),
            arguments: vec!["mcp".to_owned()],
            enabled: true,
            cwd: None,
            has_environment: false,
            has_tool_filters: false,
        }
    }

    fn is_exact(&self, executable: &Path) -> bool {
        self == &Self::desired(executable)
    }

    fn identifies_hsum(&self, executable: &Path) -> bool {
        self.command == executable
            || self
                .command
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name == "hsum" || name == "hsum.exe")
    }

    fn is_legacy_binding(&self) -> bool {
        self.arguments.len() == 3
            && self.arguments[0] == "mcp"
            && self.arguments[1] == "--binding"
            && self.arguments[2].parse::<uuid::Uuid>().is_ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexRegistrationState {
    Absent,
    Current(CodexRegistration),
    LegacyBinding(CodexRegistration),
    Stale(CodexRegistration),
    Conflict(CodexRegistration),
}

impl CodexRegistrationState {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Current(_) => "current",
            Self::LegacyBinding(_) => "legacy_binding",
            Self::Stale(_) => "stale",
            Self::Conflict(_) => "conflict",
        }
    }

    pub const fn registration(&self) -> Option<&CodexRegistration> {
        match self {
            Self::Absent => None,
            Self::Current(registration)
            | Self::LegacyBinding(registration)
            | Self::Stale(registration)
            | Self::Conflict(registration) => Some(registration),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexRegistrationChange {
    pub previous: CodexRegistrationState,
    pub current: CodexRegistration,
    pub changed: bool,
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("Codex CLI was not found on PATH")]
    CodexNotFound,
    #[error("failed to inspect the Codex MCP registration")]
    InspectFailed { status: Option<i32>, stderr: String },
    #[error("Codex returned an invalid MCP registration")]
    InvalidRegistration(#[source] serde_json::Error),
    #[error("Codex returned an unsupported MCP registration shape: {0}")]
    UnsupportedRegistration(&'static str),
    #[error("the Codex MCP name 'hsum' is owned by another command")]
    RegistrationConflict,
    #[error("Codex failed to {operation} the hSUM MCP registration")]
    CommandFailed {
        operation: &'static str,
        status: Option<i32>,
        stderr: String,
    },
    #[error("Codex did not preserve the requested hSUM MCP registration")]
    VerificationFailed,
    #[error("Codex command output exceeded 64 KiB")]
    OutputTooLarge,
    #[error("failed to launch the Codex CLI")]
    Io(#[source] io::Error),
}

#[derive(Clone, Debug)]
pub struct CodexIntegration {
    executable: PathBuf,
}

impl CodexIntegration {
    pub fn discover() -> Result<Self, IntegrationError> {
        let path = env::var_os("PATH").ok_or(IntegrationError::CodexNotFound)?;
        for directory in env::split_paths(&path) {
            for name in codex_executable_names() {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    let executable = candidate.canonicalize().map_err(IntegrationError::Io)?;
                    return Ok(Self { executable });
                }
            }
        }
        Err(IntegrationError::CodexNotFound)
    }

    pub fn from_executable(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn status(
        &self,
        hsum_executable: &Path,
    ) -> Result<CodexRegistrationState, IntegrationError> {
        let output = self.run(["mcp", "get", SERVER_NAME, "--json"])?;
        if !output.status.success() {
            let stderr = bounded_text(&output.stderr)?;
            if is_missing_registration(&stderr) {
                return Ok(CodexRegistrationState::Absent);
            }
            return Err(IntegrationError::InspectFailed {
                status: output.status.code(),
                stderr,
            });
        }
        let registration = parse_registration(&output.stdout)?;
        Ok(classify_registration(registration, hsum_executable))
    }

    pub fn install_or_repair(
        &self,
        hsum_executable: &Path,
        replace_conflict: bool,
    ) -> Result<CodexRegistrationChange, IntegrationError> {
        let previous = self.status(hsum_executable)?;
        match &previous {
            CodexRegistrationState::Current(current) => {
                return Ok(CodexRegistrationChange {
                    previous: previous.clone(),
                    current: current.clone(),
                    changed: false,
                });
            }
            CodexRegistrationState::Conflict(_) if !replace_conflict => {
                return Err(IntegrationError::RegistrationConflict);
            }
            CodexRegistrationState::Absent
            | CodexRegistrationState::LegacyBinding(_)
            | CodexRegistrationState::Stale(_)
            | CodexRegistrationState::Conflict(_) => {}
        }

        for _ in 0..2 {
            self.add(hsum_executable)?;
            if let CodexRegistrationState::Current(current) = self.status(hsum_executable)? {
                return Ok(CodexRegistrationChange {
                    previous,
                    current,
                    changed: true,
                });
            }
        }
        Err(IntegrationError::VerificationFailed)
    }

    pub fn uninstall(
        &self,
        hsum_executable: &Path,
    ) -> Result<CodexRegistrationState, IntegrationError> {
        let previous = self.status(hsum_executable)?;
        match previous {
            CodexRegistrationState::Absent => Ok(CodexRegistrationState::Absent),
            CodexRegistrationState::Conflict(_) => Err(IntegrationError::RegistrationConflict),
            CodexRegistrationState::Current(_)
            | CodexRegistrationState::LegacyBinding(_)
            | CodexRegistrationState::Stale(_) => {
                let output = self.run(["mcp", "remove", SERVER_NAME])?;
                require_success("remove", output)?;
                match self.status(hsum_executable)? {
                    CodexRegistrationState::Absent => Ok(previous),
                    _ => Err(IntegrationError::VerificationFailed),
                }
            }
        }
    }

    fn add(&self, hsum_executable: &Path) -> Result<(), IntegrationError> {
        let arguments = add_arguments(hsum_executable);
        let output = Command::new(&self.executable)
            .args(&arguments)
            .output()
            .map_err(IntegrationError::Io)?;
        require_success("register", output)
    }

    fn run<I, S>(&self, arguments: I) -> Result<Output, IntegrationError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(&self.executable)
            .args(arguments)
            .output()
            .map_err(IntegrationError::Io)
    }
}

fn add_arguments(hsum_executable: &Path) -> Vec<OsString> {
    vec![
        OsString::from("mcp"),
        OsString::from("add"),
        OsString::from(SERVER_NAME),
        OsString::from("--"),
        hsum_executable.as_os_str().to_owned(),
        OsString::from("mcp"),
    ]
}

fn require_success(operation: &'static str, output: Output) -> Result<(), IntegrationError> {
    if output.stdout.len() > MAX_CODEX_OUTPUT_BYTES || output.stderr.len() > MAX_CODEX_OUTPUT_BYTES
    {
        return Err(IntegrationError::OutputTooLarge);
    }
    if output.status.success() {
        return Ok(());
    }
    Err(IntegrationError::CommandFailed {
        operation,
        status: output.status.code(),
        stderr: bounded_text(&output.stderr)?,
    })
}

fn bounded_text(bytes: &[u8]) -> Result<String, IntegrationError> {
    if bytes.len() > MAX_CODEX_OUTPUT_BYTES {
        return Err(IntegrationError::OutputTooLarge);
    }
    Ok(String::from_utf8_lossy(bytes).trim().to_owned())
}

fn is_missing_registration(stderr: &str) -> bool {
    stderr.contains("No MCP server named 'hsum' found")
}

fn parse_registration(bytes: &[u8]) -> Result<CodexRegistration, IntegrationError> {
    if bytes.len() > MAX_CODEX_OUTPUT_BYTES {
        return Err(IntegrationError::OutputTooLarge);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(IntegrationError::InvalidRegistration)?;
    if value.get("name").and_then(Value::as_str) != Some(SERVER_NAME) {
        return Err(IntegrationError::UnsupportedRegistration(
            "entry name is not hsum",
        ));
    }
    let transport = value.get("transport").and_then(Value::as_object).ok_or(
        IntegrationError::UnsupportedRegistration("transport is absent"),
    )?;
    if transport.get("type").and_then(Value::as_str) != Some("stdio") {
        return Ok(CodexRegistration {
            is_stdio: false,
            command: PathBuf::new(),
            arguments: Vec::new(),
            enabled: value
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            cwd: None,
            has_environment: false,
            has_tool_filters: value
                .get("enabled_tools")
                .is_some_and(|value| !value.is_null())
                || value
                    .get("disabled_tools")
                    .is_some_and(|value| !value.is_null()),
        });
    }
    let command = transport.get("command").and_then(Value::as_str).ok_or(
        IntegrationError::UnsupportedRegistration("stdio command is absent"),
    )?;
    let arguments =
        transport
            .get("args")
            .and_then(Value::as_array)
            .ok_or(IntegrationError::UnsupportedRegistration(
                "stdio arguments are absent",
            ))?
            .iter()
            .map(|argument| {
                argument.as_str().map(str::to_owned).ok_or(
                    IntegrationError::UnsupportedRegistration("stdio argument is not a string"),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
    let cwd = match transport.get("cwd") {
        None | Some(Value::Null) => None,
        Some(Value::String(cwd)) => Some(PathBuf::from(cwd)),
        Some(_) => {
            return Err(IntegrationError::UnsupportedRegistration(
                "stdio cwd is not a string",
            ));
        }
    };
    let has_environment = transport.get("env").is_some_and(|value| !value.is_null())
        || transport
            .get("env_vars")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.is_empty());
    let has_tool_filters = value
        .get("enabled_tools")
        .is_some_and(|value| !value.is_null())
        || value
            .get("disabled_tools")
            .is_some_and(|value| !value.is_null());
    Ok(CodexRegistration {
        is_stdio: true,
        command: PathBuf::from(command),
        arguments,
        enabled: value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        cwd,
        has_environment,
        has_tool_filters,
    })
}

fn classify_registration(
    registration: CodexRegistration,
    hsum_executable: &Path,
) -> CodexRegistrationState {
    if registration.is_exact(hsum_executable) {
        CodexRegistrationState::Current(registration)
    } else if registration.identifies_hsum(hsum_executable) && registration.is_legacy_binding() {
        CodexRegistrationState::LegacyBinding(registration)
    } else if registration.identifies_hsum(hsum_executable) {
        CodexRegistrationState::Stale(registration)
    } else {
        CodexRegistrationState::Conflict(registration)
    }
}

#[cfg(windows)]
fn codex_executable_names() -> [&'static str; 2] {
    ["codex.exe", "codex"]
}

#[cfg(not(windows))]
fn codex_executable_names() -> [&'static str; 1] {
    ["codex"]
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = r#"{
      "name": "hsum",
      "enabled": true,
      "transport": {
        "type": "stdio",
        "command": "/opt/hsum",
        "args": ["mcp"],
        "env": null,
        "env_vars": [],
        "cwd": null
      },
      "enabled_tools": null,
      "disabled_tools": null
    }"#;

    #[test]
    fn current_registration_requires_dynamic_scope_and_no_overrides() {
        let registration = parse_registration(CURRENT.as_bytes()).unwrap();
        assert_eq!(
            classify_registration(registration, Path::new("/opt/hsum")),
            CodexRegistrationState::Current(CodexRegistration::desired(Path::new("/opt/hsum")))
        );

        let with_cwd = CURRENT.replace("\"cwd\": null", "\"cwd\": \"/tmp/repo\"");
        assert!(matches!(
            classify_registration(
                parse_registration(with_cwd.as_bytes()).unwrap(),
                Path::new("/opt/hsum")
            ),
            CodexRegistrationState::Stale(_)
        ));
    }

    #[test]
    fn legacy_hsum_is_migratable_but_foreign_name_conflicts() {
        let legacy = CURRENT.replace(
            "\"args\": [\"mcp\"]",
            "\"args\": [\"mcp\", \"--binding\", \
             \"123e4567-e89b-42d3-a456-426614174000\"]",
        );
        assert!(matches!(
            classify_registration(
                parse_registration(legacy.as_bytes()).unwrap(),
                Path::new("/new/hsum")
            ),
            CodexRegistrationState::LegacyBinding(_)
        ));

        let foreign = CURRENT.replace("\"command\": \"/opt/hsum\"", "\"command\": \"/opt/other\"");
        assert!(matches!(
            classify_registration(
                parse_registration(foreign.as_bytes()).unwrap(),
                Path::new("/new/hsum")
            ),
            CodexRegistrationState::Conflict(_)
        ));
    }

    #[test]
    fn add_uses_codex_supported_stdio_separator_without_repository_scope() {
        assert_eq!(
            add_arguments(Path::new("/opt/hsum")),
            ["mcp", "add", "hsum", "--", "/opt/hsum", "mcp"].map(OsString::from)
        );
    }
}
