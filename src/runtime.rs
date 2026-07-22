use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use rusqlite::OptionalExtension;
use rusqlite::ffi::ErrorCode as SqliteErrorCode;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{
    ContextError, ContextRequest, EffectiveContext, FilesystemIngestError, FilesystemIngestPolicy,
    InitError, InitNextStep, InitRequest, PointerOutcome, SourceConfigError, TrustRequest,
    ingest_filesystem_with_policy, initialize, plan_filesystem_ingest_with_timeout,
    resolve_context, resolve_trust_target, trust_repository,
};
use crate::cli::{
    Cli, ClientCommand, ClientConfigArgs, ClientConfigFormat, ClientDoctorArgs, ClientKind,
    Command, ErrorHelpArgs, GetArgs, GlobalOptions, HelpCommand, IngestArgs, InitArgs, McpArgs,
    SearchArgs, SearchMode as CliSearchMode, StatusArgs, TrustArgs, escape_terminal_bytes,
    escape_terminal_text, exit_code_for, render_completions, render_man, render_verbose_version,
    verbose_version_requested,
};
use crate::config::{
    ManagedPaths, ManagedPathsError, PointerError, SelectionError, SelectionMode, SelectionSource,
    TrustError,
};
use crate::domain::{DocumentId, ErrorSubcode, PublicError, Sha256Digest, SourceId};
use crate::ingest::{ChunkError, DiscoveryError};
use crate::mcp::{
    MAX_MCP_FRAME_BYTES, MCP_API_VERSION, McpServerError, serve_stdio, validate_frame,
};
use crate::search::query::QueryError;
use crate::search::{
    DuplicateReason, GetError, GetRequest, RankExplanation, Retriever, SearchError, SearchMode,
    SearchRequest, SearchResponse, SearchStopReason,
};
use crate::status::{
    DocumentDrift, DriftOptions, DriftState, SourceStatus, SourceSyncState, Status, StatusError,
    StatusReport,
};
use crate::store::{
    DeleteConfirmations, Doctor, DoctorReport, FilesystemLocality, FilesystemScope, IndexDb,
    IngestOutcome, IngestPlan, OpenMode, SourceIngestState, StoragePreflight,
    StoragePreflightError, StoreError,
};

const DOCS_BASE: &str = "https://hsum.dev/docs/0.1.0-alpha.1";
const SOURCE_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const CLIENT_DOCTOR_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_DOCTOR_STDERR_LIMIT: usize = 64 * 1024;

/// Parse the process arguments, execute one command, and return its stable exit status.
pub async fn run_from_env() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // Clap's version action short-circuits before `--verbose` is
            // visible, so the verbose form is recovered from the raw
            // arguments and rendered here instead.
            if error.kind() == clap::error::ErrorKind::DisplayVersion
                && verbose_version_requested(env::args().skip(1))
            {
                return match write_stdout(render_verbose_version().as_bytes()) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(failure) => ExitCode::from(failure.public.process_exit_code()),
                };
            }
            let code = error.exit_code();
            let _ = error.print();
            return exit_code(code);
        }
    };
    let json_error = command_requests_json(&cli.command);
    match execute(cli).await {
        Ok(category) => ExitCode::from(exit_code_for(category)),
        Err(failure) => {
            let _ = render_failure(&failure.public, json_error);
            ExitCode::from(failure.public.process_exit_code())
        }
    }
}

/// Execute an already-parsed CLI request.
pub async fn execute(cli: Cli) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    if let Command::Help(arguments) = &cli.command {
        return match &arguments.command {
            HelpCommand::Error(arguments) => run_error_help(arguments),
        };
    }

    let managed_paths = managed_paths(&cli.global)?;
    let current_dir = env::current_dir().map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::Unexpected, "current_directory", error)
    })?;

    match cli.command {
        Command::Init(arguments) => run_init(&cli.global, managed_paths, current_dir, arguments),
        Command::Trust(arguments) => run_trust(&cli.global, managed_paths, current_dir, arguments),
        Command::Ingest(arguments) => {
            run_ingest(&cli.global, managed_paths, current_dir, arguments)
        }
        Command::Search(arguments) => {
            run_search(&cli.global, managed_paths, current_dir, arguments)
        }
        Command::Get(arguments) => run_get(&cli.global, managed_paths, current_dir, arguments),
        Command::Status(arguments) => {
            run_status(&cli.global, managed_paths, current_dir, arguments)
        }
        Command::Doctor(_) => run_doctor(&cli.global, managed_paths, current_dir),
        Command::Context(arguments) => {
            run_context(&cli.global, managed_paths, current_dir, arguments.json)
        }
        Command::Help(_) => unreachable!("offline help returns before context resolution"),
        Command::Client(arguments) => match arguments.command {
            ClientCommand::Config(arguments) => {
                run_client_config(&cli.global, managed_paths, current_dir, arguments)
            }
            ClientCommand::Doctor(arguments) => {
                run_client_doctor(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Completions(arguments) => {
            write_stdout(&render_completions(arguments.shell))?;
            Ok(crate::cli::ProcessExitCategory::Success)
        }
        Command::Man(_) => {
            let manual = render_man().map_err(|error| {
                RuntimeFailure::from_error(ErrorSubcode::Unexpected, "manual", error)
            })?;
            write_stdout(&manual)?;
            Ok(crate::cli::ProcessExitCategory::Success)
        }
        Command::Mcp(arguments) => {
            run_mcp(&cli.global, managed_paths, current_dir, arguments).await
        }
    }
}

fn run_error_help(
    arguments: &ErrorHelpArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let Some(subcode) = ErrorSubcode::parse(&arguments.subcode) else {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::QuerySyntax,
            "help_error",
            "unknown error subcode; use the exact SCREAMING_SNAKE_CASE code from an hSUM error",
        ));
    };
    let mut output = subcode.render_offline_help(DOCS_BASE);
    output.push('\n');
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

#[derive(Debug)]
pub struct RuntimeFailure {
    pub public: Box<PublicError>,
}

impl RuntimeFailure {
    fn from_error(
        subcode: ErrorSubcode,
        operation: &'static str,
        error: impl fmt::Display,
    ) -> Self {
        Self {
            public: Box::new(PublicError::with_details(
                subcode,
                Uuid::new_v4().to_string(),
                DOCS_BASE,
                json!({
                    "operation": operation,
                    "reason": error.to_string(),
                }),
            )),
        }
    }

    fn unsupported_cursor() -> Self {
        Self {
            public: Box::new(PublicError::with_details(
                ErrorSubcode::QuerySyntax,
                Uuid::new_v4().to_string(),
                DOCS_BASE,
                json!({
                    "operation": "search",
                    "argument": "cursor",
                    "reason": "CLI cursor pagination is unsupported in alpha.1; omit --cursor",
                }),
            )),
        }
    }
}

impl From<ContextError> for RuntimeFailure {
    fn from(error: ContextError) -> Self {
        map_context_error(error)
    }
}

fn run_init(
    _global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: InitArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let mut request = InitRequest::new(current_dir, managed_paths);
    request.requested_root = arguments.path;
    request.index_name = arguments.index;
    request.project_name = arguments.project;
    request.no_ingest = arguments.no_ingest;
    request.dry_run = arguments.dry_run;
    request.write_pointer = arguments.write_pointer;
    request.force_pointer = arguments.force_pointer;
    request.allow_broad_root = arguments.allow_broad_root;
    request.allow_large_source = arguments.allow_large_source;
    request.index_quota_bytes = arguments.index_quota_bytes;

    let outcome = initialize(&request).map_err(map_init_error)?;
    let ingest_exit = outcome.ingest.as_ref().map_or(
        crate::cli::ProcessExitCategory::Success,
        ingest_exit_category,
    );
    let executable = absolute_executable()?;
    let mut output = String::new();
    if outcome.dry_run {
        output.push_str("DRY RUN — no files were changed.\n");
    } else if outcome.reused {
        output.push_str("Existing hSUM index selected.\n");
    } else {
        output.push_str("hSUM index initialized.\n");
    }
    push_line(&mut output, "Root", &human_path(&outcome.canonical_root));
    push_line(&mut output, "Index", outcome.index_name.as_str());
    push_line(&mut output, "Project", outcome.project_name.as_str());
    push_line(&mut output, "Data", &human_path(&outcome.database_path));
    if let Some(binding_id) = outcome.binding_id {
        push_line(&mut output, "Binding", &binding_id.to_string());
    }
    push_line(
        &mut output,
        "Eligible files",
        &outcome.source_estimate.eligible_files.to_string(),
    );
    push_line(
        &mut output,
        "Eligible bytes",
        &outcome.source_estimate.eligible_bytes.to_string(),
    );
    push_line(
        &mut output,
        "Skipped files",
        &outcome.source_estimate.skipped_files.to_string(),
    );
    push_line(&mut output, "Pointer", pointer_outcome(outcome.pointer));
    if let Some(ingest) = &outcome.ingest {
        push_line(
            &mut output,
            "Active documents",
            &ingest.active_documents.to_string(),
        );
        push_line(
            &mut output,
            "Active passages",
            &ingest.active_passages.to_string(),
        );
    }
    if !outcome.dry_run {
        match (
            ingest_exit,
            outcome.ingest.as_ref().map(|ingest| ingest.active_passages),
        ) {
            (crate::cli::ProcessExitCategory::Failure, _) => {
                output.push_str(
                    "Index metadata is ready, but every targeted source failed and no generation \
                     was activated. No model or network was used.\n",
                );
            }
            (_, Some(passages)) if passages != 0 => {
                output.push_str("Lexical search is ready. No model or network was used.\n");
            }
            (_, Some(_)) if arguments.no_ingest => {
                output.push_str(
                    "Filesystem source configured without an initial ingest. No model or network \
                     was used.\n",
                );
            }
            (_, Some(_)) => {
                output.push_str(
                    "Index is ready; no searchable passages are active. No model or network was \
                     used.\n",
                );
            }
            (_, None) if arguments.no_ingest => {
                output.push_str(
                    "Filesystem source configured without an initial ingest. No model or network \
                     was used.\n",
                );
            }
            (_, None) => {
                output.push_str("Existing index selected. No model or network was used.\n");
            }
        }
        output.push_str("Try:\n  ");
        match &outcome.next_step {
            InitNextStep::Search { query } => {
                output.push_str(&shell_quote_path(&executable));
                output.push_str(" search ");
                output.push_str(&shell_quote(query));
            }
            InitNextStep::Status => {
                output.push_str(&shell_quote_path(&executable));
                output.push_str(" status");
            }
        }
        output.push('\n');
        if outcome.binding_id.is_some() {
            output.push_str("Connect an agent:\n  ");
            output.push_str(&shell_quote_path(&executable));
            output.push_str(" client config codex\n");
        }
    }
    write_stdout(output.as_bytes())?;
    match ingest_exit {
        crate::cli::ProcessExitCategory::Failure => {
            write_stderr(
                b"FAILED: every targeted source failed; no generation was activated.\n\
                  Run `hsum status --problems` and repair the reported source before retrying.\n",
            )?;
        }
        crate::cli::ProcessExitCategory::PartialIngest => {
            write_stderr(
                b"PARTIAL: the initial filesystem ingest retained source failures.\n\
                  Run `hsum status --problems` and repair the reported source before retrying.\n",
            )?;
        }
        _ => {}
    }
    if outcome.pointer == PointerOutcome::WrittenDurabilityUnknown {
        write_stderr(
            b"warning: the repository pointer was written, but parent-directory durability could \
              not be confirmed on this filesystem\n",
        )?;
    }
    if outcome.trust_durability == Some(crate::config::AtomicSaveOutcome::DurabilityUnknown) {
        write_stderr(
            b"warning: the trust binding was written, but parent-directory durability could not \
              be confirmed on this filesystem\n",
        )?;
    }
    if let Some(preflight) = &outcome.storage_preflight {
        render_storage_warnings(preflight)?;
    }
    Ok(ingest_exit)
}

fn run_trust(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: TrustArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let target = resolve_trust_target(
        &arguments.path,
        &managed_paths,
        arguments.index.clone(),
        arguments.project.clone(),
    )
    .or_else(|error| {
        if !matches!(error, ContextError::TrustTargetIncomplete) {
            return Err(error);
        }
        let selected = direct_context(global, managed_paths.clone(), current_dir.clone())?;
        resolve_trust_target(
            &arguments.path,
            &managed_paths,
            Some(selected.index_name),
            Some(selected.project_name),
        )
    })
    .map_err(map_context_error)?;

    let outcome = trust_repository(&TrustRequest {
        root: arguments.path,
        managed_paths,
        target: target.1,
        confirm: arguments.confirm,
    })
    .map_err(map_init_error)?;
    let mut output = String::new();
    output.push_str(if outcome.created {
        "Repository trust binding created.\n"
    } else {
        "Repository trust binding already exists.\n"
    });
    push_line(
        &mut output,
        "Root",
        &human_path(outcome.binding.canonical_root()),
    );
    push_line(&mut output, "Index", outcome.binding.index_name().as_str());
    push_line(
        &mut output,
        "Project",
        outcome.binding.project_name().as_str(),
    );
    push_line(
        &mut output,
        "Binding",
        &outcome.binding.binding_id().to_string(),
    );
    write_stdout(output.as_bytes())?;
    if outcome.durability == Some(crate::config::AtomicSaveOutcome::DurabilityUnknown) {
        write_stderr(
            b"warning: the trust binding was written, but parent-directory durability could not \
              be confirmed on this filesystem\n",
        )?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_ingest(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: IngestArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let confirmations = DeleteConfirmations {
        allow_empty_snapshot: arguments.allow_empty_snapshot,
        allow_mass_delete: arguments.allow_mass_delete,
    };

    if arguments.dry_run {
        let scope = filesystem_scope(&context);
        let database = IndexDb::open_existing(&context.database_path, OpenMode::ReadOnly)
            .map_err(map_store_error)?;
        let plan = plan_filesystem_ingest_with_timeout(
            &database,
            &scope,
            &context.source_root,
            &context.source_discovery_options,
            Duration::from_millis(arguments.lock_timeout_ms),
        )
        .map_err(map_filesystem_ingest_error)?;
        drop(database);
        render_ingest_plan(&context, &plan, confirmations)?;
        return Ok(crate::cli::ProcessExitCategory::Success);
    }

    let scope = filesystem_scope(&context);
    let mut database = IndexDb::open_existing(&context.database_path, OpenMode::ReadWrite)
        .map_err(map_store_error)?;
    let outcome = ingest_filesystem_with_policy(
        &mut database,
        &scope,
        &context.source_root,
        &context.source_discovery_options,
        false,
        confirmations,
        FilesystemIngestPolicy {
            lock_timeout: Duration::from_millis(arguments.lock_timeout_ms),
            index_quota_bytes: context.index_quota_bytes,
        },
    )
    .map_err(map_filesystem_ingest_error)?;
    drop(database);

    let ingest_exit = ingest_exit_category(&outcome);
    render_ingest_outcome(&outcome)?;
    if let Some(preflight) = &outcome.storage_preflight {
        render_storage_warnings(preflight)?;
    }
    match ingest_exit {
        crate::cli::ProcessExitCategory::Failure => {
            write_stderr(
                b"FAILED: every targeted source failed; no generation was activated.\n\
                  Run `hsum status --problems` and repair the reported source before retrying.\n",
            )?;
        }
        crate::cli::ProcessExitCategory::PartialIngest => {
            write_stderr(
                b"PARTIAL: at least one source item failed; prior evidence was retained where possible.\n\
                  Run `hsum status --problems` and repair the reported source before retrying.\n",
            )?;
        }
        _ => {}
    }
    Ok(ingest_exit)
}

fn ingest_exit_category(outcome: &IngestOutcome) -> crate::cli::ProcessExitCategory {
    if !outcome.source_outcomes.is_empty()
        && outcome
            .source_outcomes
            .iter()
            .all(|source| source.state == SourceIngestState::Failed)
    {
        crate::cli::ProcessExitCategory::Failure
    } else if outcome.source_outcomes.iter().any(|source| {
        matches!(
            source.state,
            SourceIngestState::Partial | SourceIngestState::Failed
        )
    }) {
        crate::cli::ProcessExitCategory::PartialIngest
    } else {
        crate::cli::ProcessExitCategory::Success
    }
}

fn run_search(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SearchArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    if arguments.cursor.is_some() {
        return Err(RuntimeFailure::unsupported_cursor());
    }
    let context = direct_context(global, managed_paths, current_dir)?;
    let request = SearchRequest::new(
        &arguments.query,
        match arguments.mode {
            CliSearchMode::Auto => SearchMode::Auto,
            CliSearchMode::Lexical => SearchMode::Lexical,
        },
        usize::from(arguments.limit),
        arguments.timeout_ms,
        arguments.explain,
    )
    .map_err(map_search_error)?;
    let database = IndexDb::open_existing(&context.database_path, OpenMode::ReadOnly)
        .map_err(map_store_error)?;
    let response = database
        .search(context.project_id, &request)
        .map_err(map_search_error)?;
    let cited_revisions = response
        .results
        .iter()
        .map(|passage| {
            (
                passage.source_id,
                passage.document_id,
                passage.revision_sha256,
            )
        })
        .collect::<BTreeSet<_>>();
    let transaction = database
        .connection()
        .unchecked_transaction()
        .map_err(|error| {
            RuntimeFailure::from_error(sqlite_subcode(&error), "search_drift_snapshot", error)
        })?;
    let drift_targets = cited_revisions
        .into_iter()
        .map(|(source_id, document_id, revision_sha256)| {
            Status::cited_drift_target(&transaction, source_id, document_id, revision_sha256)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_status_error)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    transaction.rollback().map_err(|error| {
        RuntimeFailure::from_error(sqlite_subcode(&error), "search_drift_snapshot", error)
    })?;
    drop(database);
    let probe_deadline = Instant::now()
        .checked_add(SOURCE_PROBE_TIMEOUT)
        .unwrap_or_else(Instant::now);
    let drift = drift_targets
        .into_iter()
        .map(|target| {
            Status::probe_cited_target(
                &context.source_root,
                target,
                DriftOptions {
                    verify_content_hash: false,
                    deadline: probe_deadline.saturating_duration_since(Instant::now()),
                },
            )
        })
        .collect::<Vec<_>>();

    if arguments.json {
        let output = SearchJson::from_response(
            &context,
            response,
            Some(drift.as_slice()),
            arguments.explain,
        );
        write_json_stdout(&output)?;
    } else {
        render_search_human(&response, Some(drift.as_slice()), arguments.explain)?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_get(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: GetArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let request = GetRequest {
        project_id: context.project_id,
        citation: arguments.citation_uri,
        max_bytes: usize::try_from(arguments.max_bytes)
            .expect("u32 fits usize on supported targets"),
    };
    let database = IndexDb::open_existing(&context.database_path, OpenMode::ReadOnly)
        .map_err(map_store_error)?;
    let response = database.get_evidence(&request).map_err(map_get_error)?;
    let verification_snapshot = if arguments.verify_source_hash {
        let transaction = database
            .connection()
            .unchecked_transaction()
            .map_err(|error| {
                RuntimeFailure::from_error(
                    sqlite_subcode(&error),
                    "get_verification_snapshot",
                    error,
                )
            })?;
        let current_head = current_head_body_sha256(
            &transaction,
            context.project_id,
            request.citation.source_id,
            request.citation.document_id,
        )?;
        let drift_target = Status::cited_drift_target(
            &transaction,
            request.citation.source_id,
            request.citation.document_id,
            request.citation.revision,
        )
        .map_err(map_status_error)?;
        drop(transaction);
        Some((current_head, drift_target))
    } else {
        None
    };
    drop(database);

    let verification = if arguments.verify_source_hash {
        let (current_head, drift_target) =
            verification_snapshot.expect("verification snapshot was requested");
        let observation = drift_target.map(|target| {
            Status::probe_cited_target(
                &context.source_root,
                target,
                DriftOptions {
                    verify_content_hash: true,
                    deadline: SOURCE_PROBE_TIMEOUT,
                },
            )
        });
        verify_source_hash(response.body_sha256, current_head, observation.as_ref())
    } else {
        "not_requested".to_owned()
    };
    let content = String::from_utf8(response.content)
        .map_err(|error| RuntimeFailure::from_error(ErrorSubcode::Invariant, "get", error))?;
    let metadata: Value = serde_json::from_str(&response.metadata_json)
        .map_err(|error| RuntimeFailure::from_error(ErrorSubcode::Invariant, "get", error))?;

    let output = GetJson {
        schema_version: MCP_API_VERSION,
        request_id: Uuid::new_v4().to_string(),
        requested_citation_uri: response.requested_citation.to_string(),
        returned_citation_uri: response.returned_citation.to_string(),
        requested_line_span: LineSpanJson {
            start: response.requested_line_span.start(),
            end: response.requested_line_span.end(),
        },
        returned_line_span: LineSpanJson {
            start: response.returned_line_span.start(),
            end: response.returned_line_span.end(),
        },
        source_uri: response.source_uri,
        title: response.title,
        metadata,
        source_updated_at: response.source_updated_at,
        indexed_at: response.indexed_at,
        content,
        body_sha256: response.body_sha256.to_string(),
        source_hash_verification: verification,
        untrusted_content: response.untrusted_content,
    };
    if arguments.json {
        write_json_stdout(&output)?;
    } else {
        let mut human = String::new();
        push_line(
            &mut human,
            "Citation",
            &escape_terminal_text(&output.returned_citation_uri),
        );
        push_line(
            &mut human,
            "Source",
            &escape_terminal_text(&output.source_uri),
        );
        push_line(&mut human, "Title", &escape_terminal_text(&output.title));
        push_line(
            &mut human,
            "Lines",
            &format!(
                "{}-{}",
                output.returned_line_span.start, output.returned_line_span.end
            ),
        );
        push_line(&mut human, "Source hash", &output.source_hash_verification);
        push_line(
            &mut human,
            "Content",
            &escape_terminal_text(&output.content),
        );
        write_stdout(human.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_status(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: StatusArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let report = Status::read(&context.database_path).map_err(map_status_error)?;
    if arguments.json {
        write_json_stdout(&StatusJson {
            schema_version: MCP_API_VERSION,
            request_id: Uuid::new_v4().to_string(),
            index_id: context.index_id.to_string(),
            index: context.index_name.as_str(),
            project_id: context.project_id.to_string(),
            project: context.project_name.as_str(),
            problems_only: arguments.problems,
            report: &report,
        })?;
    } else {
        render_status_human(&context, &report, arguments.problems)?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_doctor(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let report = Doctor::run(&context.database_path).map_err(map_store_error)?;
    render_doctor_human(&context, &report)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_context(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    json: bool,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let config_file = global
        .config
        .clone()
        .unwrap_or_else(|| context.managed_paths.config_file());
    if json {
        write_json_stdout(&ContextJson::from_context(&context, &config_file))?;
    } else {
        let mut output = String::new();
        push_line(&mut output, "Index", context.index_name.as_str());
        push_line(&mut output, "Index ID", &context.index_id.to_string());
        push_line(&mut output, "Project", context.project_name.as_str());
        push_line(&mut output, "Project ID", &context.project_id.to_string());
        push_line(
            &mut output,
            "Source root",
            &human_path(&context.source_root),
        );
        push_line(
            &mut output,
            "Index quota bytes",
            &context
                .index_quota_bytes
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        );
        push_line(
            &mut output,
            "Binding",
            &context
                .binding_id
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        );
        push_line(
            &mut output,
            "Selection",
            selection_source(context.selection_source),
        );
        push_line(&mut output, "Database", &human_path(&context.database_path));
        push_line(&mut output, "Config file", &human_path(&config_file));
        push_line(
            &mut output,
            "Config directory",
            &human_path(context.managed_paths.config_dir()),
        );
        push_line(
            &mut output,
            "Data directory",
            &human_path(context.managed_paths.data_dir()),
        );
        push_line(
            &mut output,
            "Cache directory",
            &human_path(context.managed_paths.cache_dir()),
        );
        write_stdout(output.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_client_config(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ClientConfigArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let binding_id = context.binding_id.ok_or_else(|| {
        RuntimeFailure::from_error(
            ErrorSubcode::PathTrust,
            "client_config",
            "the selected context has no user-side trust binding",
        )
    })?;
    let executable = absolute_executable()?;
    let config = client_config_value(arguments.client, &executable, &binding_id.to_string());
    let bytes = match arguments.format {
        ClientConfigFormat::Json => {
            let mut bytes = serde_json::to_vec_pretty(&config).map_err(|error| {
                RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_config", error)
            })?;
            bytes.push(b'\n');
            bytes
        }
        ClientConfigFormat::Toml => {
            let mut text = toml::to_string_pretty(&config).map_err(|error| {
                RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_config", error)
            })?;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.into_bytes()
        }
    };
    write_stderr(
        b"Before connecting: hSUM uploads no corpus data or telemetry. A cloud-backed agent may \
          send returned passages to its model provider under that client's policy.\n",
    )?;
    write_stdout(&bytes)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_client_doctor(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ClientDoctorArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let binding_id = context.binding_id.ok_or_else(|| {
        RuntimeFailure::from_error(
            ErrorSubcode::PathTrust,
            "client_doctor",
            "the selected context has no user-side trust binding",
        )
    })?;
    let executable = absolute_executable()?;
    let config = client_config_value(arguments.client, &executable, &binding_id.to_string());
    let launch = validate_client_launch_config(
        &config,
        arguments.client,
        &executable,
        &binding_id.to_string(),
    )?;
    let probe = run_client_stdio_probe(&launch, context.project_id.to_string())?;

    let mut output = String::new();
    push_line(&mut output, "Client", client_kind(arguments.client));
    push_line(&mut output, "Executable", &human_path(&launch.executable));
    push_line(&mut output, "Binding", &binding_id.to_string());
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(&mut output, "Project ID", &probe.project_id);
    push_line(
        &mut output,
        "Read-only tools",
        &probe.read_only_tools.to_string(),
    );
    push_line(
        &mut output,
        "Probe working directory",
        &human_path(&probe.working_directory),
    );
    push_line(
        &mut output,
        "Selection environment",
        "HSUM_INDEX and HSUM_PROJECT stripped",
    );
    output.push_str(
        "Client configuration inputs, binding scope, stdio framing, and read-only probe are \
         healthy.\n",
    );
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

#[derive(Debug)]
struct ClientLaunchConfig {
    executable: PathBuf,
    arguments: Vec<String>,
}

#[derive(Debug)]
struct ClientProbe {
    project_id: String,
    read_only_tools: usize,
    working_directory: PathBuf,
}

fn validate_client_launch_config(
    config: &Value,
    client: ClientKind,
    expected_executable: &Path,
    expected_binding: &str,
) -> Result<ClientLaunchConfig, RuntimeFailure> {
    let server = match client {
        ClientKind::Codex => config.pointer("/mcp_servers/hsum"),
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop => config.pointer("/mcpServers/hsum"),
        ClientKind::Generic => config.pointer("/hsum"),
    }
    .ok_or_else(|| {
        RuntimeFailure::from_error(
            ErrorSubcode::ConfigInvalid,
            "client_doctor_config",
            "generated client configuration omitted the hsum server entry",
        )
    })?;
    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RuntimeFailure::from_error(
                ErrorSubcode::ConfigInvalid,
                "client_doctor_config",
                "generated client configuration omitted its executable",
            )
        })?;
    let executable = PathBuf::from(command);
    if !executable.is_absolute() || executable != expected_executable {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::ConfigInvalid,
            "client_doctor_config",
            "generated client configuration did not preserve the canonical absolute executable",
        ));
    }
    let arguments = server
        .get("args")
        .and_then(Value::as_array)
        .and_then(|arguments| {
            arguments
                .iter()
                .map(Value::as_str)
                .map(|argument| argument.map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| {
            RuntimeFailure::from_error(
                ErrorSubcode::ConfigInvalid,
                "client_doctor_config",
                "generated client configuration has invalid arguments",
            )
        })?;
    if arguments != ["mcp", "--binding", expected_binding] {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::ConfigInvalid,
            "client_doctor_config",
            "generated client configuration did not preserve the selected trust binding",
        ));
    }
    Ok(ClientLaunchConfig {
        executable,
        arguments,
    })
}

fn run_client_stdio_probe(
    launch: &ClientLaunchConfig,
    expected_project_id: String,
) -> Result<ClientProbe, RuntimeFailure> {
    let working_directory = IsolatedProbeDirectory::create()?;
    let deadline = Instant::now()
        .checked_add(CLIENT_DOCTOR_TIMEOUT)
        .ok_or_else(|| {
            RuntimeFailure::from_error(
                ErrorSubcode::RequestDeadline,
                "client_doctor",
                "the client-doctor deadline could not be represented",
            )
        })?;
    let mut child = ProcessCommand::new(&launch.executable)
        .args(&launch.arguments)
        .current_dir(working_directory.path())
        .env_remove("HSUM_INDEX")
        .env_remove("HSUM_PROJECT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(ProbeChild::new)
        .map_err(|error| {
            RuntimeFailure::from_error(ErrorSubcode::PathInvalid, "client_doctor_spawn", error)
        })?;
    let mut stdin = child.take_stdin()?;
    let stdout = child.take_stdout()?;
    let stderr = child.take_stderr()?;

    let (stdout_tx, stdout_rx) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_bounded_probe_frame(&mut reader) {
                Ok(Some(frame)) => {
                    if stdout_tx.send(ProbeStreamEvent::Frame(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = stdout_tx.send(ProbeStreamEvent::End);
                    break;
                }
                Err(error) => {
                    let _ = stdout_tx.send(ProbeStreamEvent::Error(error));
                    break;
                }
            }
        }
    });
    let stderr_reader =
        thread::spawn(move || read_capped_stream(stderr, CLIENT_DOCTOR_STDERR_LIMIT));

    let requests = client_probe_requests()?;
    write_probe_request(&mut stdin, &requests[0])?;
    let mut initialized = false;
    let mut listed_tools = None;
    let mut status_project = None;
    let mut sent_tools = false;
    let mut sent_status = false;
    while !initialized || listed_tools.is_none() || status_project.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(client_doctor_timeout());
        }
        let event = match stdout_rx.recv_timeout(remaining) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => return Err(client_doctor_timeout()),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(client_doctor_disconnect(
                    "the MCP stdout reader disconnected before the probe completed",
                ));
            }
        };
        match event {
            ProbeStreamEvent::Frame(frame) => {
                validate_frame(&frame).map_err(|error| {
                    RuntimeFailure::from_error(
                        ErrorSubcode::FrameLimit,
                        "client_doctor_stdout",
                        error,
                    )
                })?;
                let value: Value = serde_json::from_slice(&frame).map_err(|error| {
                    RuntimeFailure::from_error(
                        ErrorSubcode::FrameLimit,
                        "client_doctor_stdout",
                        error,
                    )
                })?;
                validate_probe_response(
                    &value,
                    &expected_project_id,
                    &mut initialized,
                    &mut listed_tools,
                    &mut status_project,
                )?;
                if initialized && !sent_tools {
                    write_probe_request(&mut stdin, &requests[1])?;
                    write_probe_request(&mut stdin, &requests[2])?;
                    sent_tools = true;
                }
                if listed_tools.is_some() && !sent_status {
                    write_probe_request(&mut stdin, &requests[3])?;
                    sent_status = true;
                }
            }
            ProbeStreamEvent::End => {
                return Err(client_doctor_disconnect(
                    "the MCP server closed stdout before the probe completed",
                ));
            }
            ProbeStreamEvent::Error(error) => {
                return Err(RuntimeFailure::from_error(
                    ErrorSubcode::FrameLimit,
                    "client_doctor_stdout",
                    error.description(),
                ));
            }
        }
    }

    drop(stdin);
    let status = child.wait_until(deadline)?;
    stdout_reader.join().map_err(|_| {
        RuntimeFailure::from_error(
            ErrorSubcode::Unexpected,
            "client_doctor_stdout",
            "the MCP stdout reader panicked",
        )
    })?;
    for event in stdout_rx.try_iter() {
        match event {
            ProbeStreamEvent::End => {}
            ProbeStreamEvent::Frame(_) => {
                return Err(RuntimeFailure::from_error(
                    ErrorSubcode::FrameLimit,
                    "client_doctor_stdout",
                    "the MCP subprocess emitted an unexpected extra stdout frame",
                ));
            }
            ProbeStreamEvent::Error(error) => {
                return Err(RuntimeFailure::from_error(
                    ErrorSubcode::FrameLimit,
                    "client_doctor_stdout",
                    error.description(),
                ));
            }
        }
    }
    let stderr = stderr_reader.join().map_err(|_| {
        RuntimeFailure::from_error(
            ErrorSubcode::Unexpected,
            "client_doctor_stderr",
            "the MCP stderr reader panicked",
        )
    })??;
    if stderr.overflowed || !stderr.bytes.is_empty() {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::Unexpected,
            "client_doctor_stderr",
            "the MCP subprocess emitted unexpected stderr output",
        ));
    }
    if !status.success() {
        return Err(client_doctor_disconnect(
            "the MCP subprocess exited unsuccessfully",
        ));
    }
    if fs::read_dir(working_directory.path())
        .map_err(|error| {
            RuntimeFailure::from_error(
                ErrorSubcode::StorageIo,
                "client_doctor_working_directory",
                error,
            )
        })?
        .next()
        .is_some()
    {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::Invariant,
            "client_doctor_working_directory",
            "the read-only MCP probe wrote into its empty working directory",
        ));
    }

    Ok(ClientProbe {
        project_id: status_project.expect("probe loop requires status response"),
        read_only_tools: listed_tools.expect("probe loop requires tools response"),
        working_directory: working_directory.path().to_path_buf(),
    })
}

fn client_probe_requests() -> Result<[Vec<u8>; 4], RuntimeFailure> {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-client-doctor", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "evidence_status", "arguments": {}}
        }),
    ];
    let serialize = |request: &Value| -> Result<Vec<u8>, RuntimeFailure> {
        let frame = serde_json::to_vec(&request).map_err(|error| {
            RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_doctor_frame", error)
        })?;
        validate_frame(&frame).map_err(|error| {
            RuntimeFailure::from_error(ErrorSubcode::FrameLimit, "client_doctor_frame", error)
        })?;
        Ok(frame)
    };
    Ok([
        serialize(&requests[0])?,
        serialize(&requests[1])?,
        serialize(&requests[2])?,
        serialize(&requests[3])?,
    ])
}

fn write_probe_request(
    stdin: &mut std::process::ChildStdin,
    frame: &[u8],
) -> Result<(), RuntimeFailure> {
    stdin.write_all(frame).map_err(|error| {
        RuntimeFailure::from_error(
            ErrorSubcode::ClientDisconnected,
            "client_doctor_write",
            error,
        )
    })?;
    stdin.write_all(b"\n").map_err(|error| {
        RuntimeFailure::from_error(
            ErrorSubcode::ClientDisconnected,
            "client_doctor_write",
            error,
        )
    })?;
    stdin.flush().map_err(|error| {
        RuntimeFailure::from_error(
            ErrorSubcode::ClientDisconnected,
            "client_doctor_write",
            error,
        )
    })
}

fn validate_probe_response(
    response: &Value,
    expected_project_id: &str,
    initialized: &mut bool,
    listed_tools: &mut Option<usize>,
    status_project: &mut Option<String>,
) -> Result<(), RuntimeFailure> {
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(probe_protocol_error(
            "the MCP subprocess returned an invalid JSON-RPC version",
        ));
    }
    if let Some(error) = response.get("error") {
        let subcode = error
            .pointer("/data/subcode")
            .and_then(Value::as_str)
            .and_then(ErrorSubcode::parse)
            .unwrap_or(ErrorSubcode::Unexpected);
        return Err(RuntimeFailure::from_error(
            subcode,
            "client_doctor_probe",
            "the MCP subprocess returned a protocol error",
        ));
    }
    let id = response.get("id").and_then(Value::as_u64).ok_or_else(|| {
        probe_protocol_error("the MCP subprocess returned a response without a numeric ID")
    })?;
    match id {
        1 if !*initialized => {
            if response
                .pointer("/result/serverInfo/name")
                .and_then(Value::as_str)
                != Some("hsum")
            {
                return Err(probe_protocol_error(
                    "the initialized MCP server did not identify itself as hsum",
                ));
            }
            *initialized = true;
        }
        2 if listed_tools.is_none() => {
            let tools = response
                .pointer("/result/tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    probe_protocol_error("the MCP tools/list response omitted its tools array")
                })?;
            if !tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("evidence_status"))
            {
                return Err(probe_protocol_error(
                    "the MCP tools/list response omitted evidence_status",
                ));
            }
            *listed_tools = Some(tools.len());
        }
        3 if status_project.is_none() => {
            let status = response
                .pointer("/result/structuredContent")
                .ok_or_else(|| {
                    probe_protocol_error(
                        "the MCP evidence_status response omitted structured content",
                    )
                })?;
            if status.get("schema_version").and_then(Value::as_str) != Some(MCP_API_VERSION)
                || status.get("read_only").and_then(Value::as_bool) != Some(true)
                || status.get("query_only").and_then(Value::as_bool) != Some(true)
            {
                return Err(probe_protocol_error(
                    "the MCP evidence_status response was not versioned, read-only, and query-only",
                ));
            }
            let project_id = status
                .get("project_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    probe_protocol_error("the MCP evidence_status response omitted its project ID")
                })?;
            if project_id != expected_project_id {
                return Err(probe_protocol_error(
                    "the MCP evidence_status response escaped the configured project binding",
                ));
            }
            *status_project = Some(project_id.to_owned());
        }
        _ => {
            return Err(probe_protocol_error(
                "the MCP subprocess returned a duplicate or unexpected response ID",
            ));
        }
    }
    Ok(())
}

fn probe_protocol_error(reason: &'static str) -> RuntimeFailure {
    RuntimeFailure::from_error(ErrorSubcode::Invariant, "client_doctor_probe", reason)
}

fn client_doctor_disconnect(reason: &'static str) -> RuntimeFailure {
    RuntimeFailure::from_error(
        ErrorSubcode::ClientDisconnected,
        "client_doctor_probe",
        reason,
    )
}

fn client_doctor_timeout() -> RuntimeFailure {
    RuntimeFailure::from_error(
        ErrorSubcode::RequestDeadline,
        "client_doctor_probe",
        "the MCP subprocess did not complete the bounded probe in time",
    )
}

enum ProbeStreamEvent {
    Frame(Vec<u8>),
    End,
    Error(ProbeFrameReadError),
}

#[derive(Clone, Copy)]
enum ProbeFrameReadError {
    Io,
    Empty,
    Unterminated,
    TooLarge,
}

impl ProbeFrameReadError {
    const fn description(self) -> &'static str {
        match self {
            Self::Io => "the MCP stdout stream could not be read",
            Self::Empty => "the MCP stdout stream contained an empty frame",
            Self::Unterminated => "the MCP stdout stream ended with an unterminated frame",
            Self::TooLarge => "the MCP stdout stream exceeded the frame-size limit",
        }
    }
}

fn read_bounded_probe_frame<R: BufRead>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, ProbeFrameReadError> {
    let mut frame = Vec::new();
    let mut limited = Read::by_ref(reader).take((MAX_MCP_FRAME_BYTES + 2) as u64);
    let read = limited
        .read_until(b'\n', &mut frame)
        .map_err(|_| ProbeFrameReadError::Io)?;
    if read == 0 {
        return Ok(None);
    }
    if frame.last() != Some(&b'\n') {
        return Err(if frame.len() > MAX_MCP_FRAME_BYTES {
            ProbeFrameReadError::TooLarge
        } else {
            ProbeFrameReadError::Unterminated
        });
    }
    frame.pop();
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    if frame.is_empty() {
        return Err(ProbeFrameReadError::Empty);
    }
    if frame.len() > MAX_MCP_FRAME_BYTES {
        return Err(ProbeFrameReadError::TooLarge);
    }
    Ok(Some(frame))
}

struct CappedStream {
    bytes: Vec<u8>,
    overflowed: bool,
}

fn read_capped_stream<R: Read>(
    mut reader: R,
    limit: usize,
) -> Result<CappedStream, RuntimeFailure> {
    let mut bytes = Vec::new();
    let mut overflowed = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_doctor_stderr", error)
        })?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        overflowed |= read > remaining;
    }
    Ok(CappedStream { bytes, overflowed })
}

struct ProbeChild {
    child: Child,
    reaped: bool,
}

impl ProbeChild {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn take_stdin(&mut self) -> Result<std::process::ChildStdin, RuntimeFailure> {
        self.child.stdin.take().ok_or_else(|| {
            client_doctor_disconnect("the MCP subprocess did not expose a stdin pipe")
        })
    }

    fn take_stdout(&mut self) -> Result<std::process::ChildStdout, RuntimeFailure> {
        self.child.stdout.take().ok_or_else(|| {
            client_doctor_disconnect("the MCP subprocess did not expose a stdout pipe")
        })
    }

    fn take_stderr(&mut self) -> Result<std::process::ChildStderr, RuntimeFailure> {
        self.child.stderr.take().ok_or_else(|| {
            client_doctor_disconnect("the MCP subprocess did not expose a stderr pipe")
        })
    }

    fn wait_until(
        &mut self,
        deadline: Instant,
    ) -> Result<std::process::ExitStatus, RuntimeFailure> {
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| {
                RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_doctor_wait", error)
            })? {
                self.reaped = true;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(client_doctor_timeout());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        if !self.reaped && self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct IsolatedProbeDirectory {
    path: PathBuf,
}

impl IsolatedProbeDirectory {
    fn create() -> Result<Self, RuntimeFailure> {
        let parent = env::temp_dir().canonicalize().map_err(|error| {
            RuntimeFailure::from_error(
                ErrorSubcode::PathInvalid,
                "client_doctor_working_directory",
                error,
            )
        })?;
        for _ in 0..8 {
            let path = parent.join(format!("hsum-client-doctor-{}", Uuid::new_v4()));
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(&path)
            };
            #[cfg(not(unix))]
            let created = fs::create_dir(&path);
            match created {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(RuntimeFailure::from_error(
                        ErrorSubcode::StorageIo,
                        "client_doctor_working_directory",
                        error,
                    ));
                }
            }
        }
        Err(RuntimeFailure::from_error(
            ErrorSubcode::StorageIo,
            "client_doctor_working_directory",
            "could not reserve a unique empty probe directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IsolatedProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

async fn run_mcp(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: McpArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let mut request = ContextRequest::direct(current_dir, managed_paths);
    request.mode = SelectionMode::Mcp;
    request.binding = arguments.binding;
    request.project = arguments.project;
    // MCP selection deliberately ignores environment and configured defaults.
    request.config_file = None;
    let context = resolve_context(&request).map_err(map_context_error)?;
    let _ = global;
    serve_stdio(context.database_path, context.project_id)
        .await
        .map_err(map_mcp_server_error)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn managed_paths(global: &GlobalOptions) -> Result<ManagedPaths, RuntimeFailure> {
    ManagedPaths::from_environment()
        .and_then(|paths| paths.with_overrides(global.data_dir.clone(), global.cache_dir.clone()))
        .map_err(map_managed_paths_error)
}

fn direct_context(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
) -> Result<EffectiveContext, ContextError> {
    let mut request = ContextRequest::direct(current_dir, managed_paths);
    request.config_file = global.config.clone();
    resolve_context(&request)
}

fn filesystem_scope(context: &EffectiveContext) -> FilesystemScope {
    FilesystemScope {
        source_id: context.source_id,
        source_name: context.source_name.clone(),
        source_logical_uri: context.source_root.to_string_lossy().into_owned(),
        source_config_json: context.source_config_json.clone(),
        project_id: context.project_id,
        project_name: context.project_name.clone(),
    }
}

fn current_head_body_sha256(
    connection: &rusqlite::Connection,
    project_id: crate::domain::ProjectId,
    source_id: SourceId,
    document_id: DocumentId,
) -> Result<Option<Sha256Digest>, RuntimeFailure> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT cb.body_sha256
             FROM document_heads AS dh
             JOIN documents AS d ON d.id = dh.document_id
             JOIN document_versions AS dv ON dv.id = dh.document_version_id
             JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
             JOIN project_sources AS ps ON ps.source_id = d.source_id
             WHERE dh.state = 'active'
               AND ps.project_id = ?1
               AND d.source_id = ?2
               AND d.id = ?3",
            rusqlite::params![
                project_id.as_uuid().as_bytes().as_slice(),
                source_id.as_uuid().as_bytes().as_slice(),
                document_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            RuntimeFailure::from_error(sqlite_subcode(&error), "get_verification_snapshot", error)
        })?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        RuntimeFailure::from_error(
            ErrorSubcode::HeadIndexMismatch,
            "get_verification_snapshot",
            "current head body digest is not 32 bytes",
        )
    })?;
    Ok(Some(Sha256Digest::from_bytes(bytes)))
}

fn verify_source_hash(
    cited_body_sha256: Sha256Digest,
    current_head_sha256: Option<Sha256Digest>,
    observation: Option<&DocumentDrift>,
) -> String {
    let Some(observation) = observation else {
        return "unverifiable".to_owned();
    };
    match (
        observation.content_matches,
        observation.state,
        current_head_sha256,
    ) {
        (Some(true), _, Some(current)) if current == cited_body_sha256 => "unchanged",
        (Some(true), _, Some(_)) => "changed",
        (Some(false), _, _) => "changed",
        (None, DriftState::Missing, _) => "missing",
        (None, DriftState::Blocked, _) => "blocked",
        (Some(_), _, None) | (None, _, _) => "unverifiable",
    }
    .to_owned()
}

fn render_ingest_plan(
    context: &EffectiveContext,
    plan: &IngestPlan,
    confirmations: DeleteConfirmations,
) -> Result<(), RuntimeFailure> {
    let mut output = String::new();
    output.push_str("DRY RUN — no index data was changed.\n");
    push_line(&mut output, "Root", &human_path(&context.source_root));
    push_line(
        &mut output,
        "Prior active documents",
        &plan.prior_active_documents.to_string(),
    );
    push_line(
        &mut output,
        "New documents",
        &plan.new_documents.to_string(),
    );
    push_line(
        &mut output,
        "Changed documents",
        &plan.changed_documents.to_string(),
    );
    push_line(
        &mut output,
        "Renamed documents",
        &plan.renamed_documents.to_string(),
    );
    push_line(
        &mut output,
        "Unchanged documents",
        &plan.unchanged_documents.to_string(),
    );
    push_line(
        &mut output,
        "Tombstoned documents",
        &plan.tombstoned_documents.to_string(),
    );
    push_line(
        &mut output,
        "Carried forward",
        &plan.carried_forward_documents.to_string(),
    );
    push_line(
        &mut output,
        "Failed documents",
        &plan.failed_documents.to_string(),
    );
    push_line(
        &mut output,
        "Projected active documents",
        &plan.projected_active_documents.to_string(),
    );
    push_line(
        &mut output,
        "Would create generation",
        bool_text(plan.would_create_generation),
    );
    push_line(
        &mut output,
        "Estimated write bytes",
        &plan.estimated_write_bytes.to_string(),
    );
    push_line(
        &mut output,
        "Empty snapshot confirmation",
        confirmation_state(
            plan.requires_empty_snapshot_confirmation,
            confirmations.allow_empty_snapshot,
        ),
    );
    push_line(
        &mut output,
        "Mass delete confirmation",
        confirmation_state(
            plan.requires_mass_delete_confirmation,
            confirmations.allow_mass_delete,
        ),
    );
    write_stdout(output.as_bytes())?;
    if plan.failed_documents != 0 {
        write_stderr(
            format!(
                "warning: {} source document(s) failed validation; dry-run made no changes\n",
                plan.failed_documents
            )
            .as_bytes(),
        )?;
    }
    Ok(())
}

fn render_ingest_outcome(outcome: &IngestOutcome) -> Result<(), RuntimeFailure> {
    let mut output = String::new();
    if ingest_exit_category(outcome) == crate::cli::ProcessExitCategory::Failure {
        output.push_str("Filesystem ingest failed; no generation was activated.\n");
    } else {
        output.push_str("Filesystem ingest complete.\n");
    }
    push_line(
        &mut output,
        "Generation",
        &outcome
            .generation_id
            .map_or_else(|| "unchanged".to_owned(), |value| value.to_string()),
    );
    push_line(
        &mut output,
        "Changed documents",
        &outcome.changed_documents.to_string(),
    );
    push_line(
        &mut output,
        "Unchanged documents",
        &outcome.unchanged_documents.to_string(),
    );
    push_line(
        &mut output,
        "Tombstoned documents",
        &outcome.tombstoned_documents.to_string(),
    );
    push_line(
        &mut output,
        "Carried forward",
        &outcome.carried_forward_documents.to_string(),
    );
    push_line(
        &mut output,
        "Failed documents",
        &outcome.failed_documents.to_string(),
    );
    push_line(
        &mut output,
        "Active documents",
        &outcome.active_documents.to_string(),
    );
    push_line(
        &mut output,
        "Active passages",
        &outcome.active_passages.to_string(),
    );
    push_line(&mut output, "Index epoch", &outcome.index_epoch.to_string());
    for source in &outcome.source_outcomes {
        output.push_str("Source ");
        output.push_str(&source.source_id.to_string());
        output.push_str(": ");
        output.push_str(match source.state {
            SourceIngestState::Success => "success",
            SourceIngestState::Partial => "partial",
            SourceIngestState::Failed => "failed",
        });
        output.push_str(" (accepted ");
        output.push_str(&source.accepted_documents.to_string());
        output.push_str(", failed ");
        output.push_str(&source.failed_documents.to_string());
        output.push_str(", carried forward ");
        output.push_str(&source.carried_forward_documents.to_string());
        output.push_str(")\n");
    }
    write_stdout(output.as_bytes())
}

fn render_search_human(
    response: &SearchResponse,
    drift: Option<&[DocumentDrift]>,
    explain: bool,
) -> Result<(), RuntimeFailure> {
    let executable = absolute_executable()?;
    let mut output = String::new();
    for (index, passage) in response.results.iter().enumerate() {
        let state = source_state(drift_for(drift, passage.source_id, passage.document_id));
        output.push_str(&(index + 1).to_string());
        output.push_str("  ");
        output.push_str(&escape_terminal_text(&passage.source_uri));
        output.push(':');
        output.push_str(&passage.line_span.start().to_string());
        output.push('-');
        output.push_str(&passage.line_span.end().to_string());
        output.push_str("  [");
        output.push_str(state);
        output.push_str("]\n   ");
        output.push_str(
            &passage
                .score
                .lists
                .iter()
                .map(rank_summary)
                .collect::<Vec<_>>()
                .join(" + "),
        );
        output.push_str("\n   ");
        output.push_str(&escape_terminal_text(&passage.content));
        output.push_str("\n   citation: ");
        output.push_str(&shell_quote(&passage.citation().to_string()));
        output.push_str("\n   get: ");
        output.push_str(&shell_quote_path(&executable));
        output.push_str(" get ");
        output.push_str(&shell_quote(&passage.citation().to_string()));
        output.push('\n');
        if explain {
            output.push_str("   score: ");
            output.push_str(&format!(
                "{:.12} ({} fusion units)\n",
                passage.score.fused, passage.score.fusion_units
            ));
        }
    }
    if response.results.is_empty() {
        output.push_str("No matching evidence.\n");
    }
    output.push_str("stop: ");
    output.push_str(search_stop_reason(response.stop_reason));
    output.push('\n');
    write_stdout(output.as_bytes())
}

fn render_status_human(
    context: &EffectiveContext,
    report: &StatusReport,
    problems_only: bool,
) -> Result<(), RuntimeFailure> {
    let mut output = String::new();
    if !problems_only {
        push_line(&mut output, "Index", context.index_name.as_str());
        push_line(&mut output, "Project", context.project_name.as_str());
        push_line(
            &mut output,
            "Generation",
            &report
                .active_generation
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        );
        push_line(&mut output, "Index epoch", &report.index_epoch.to_string());
        push_line(
            &mut output,
            "Active documents",
            &report.active_documents.to_string(),
        );
        push_line(
            &mut output,
            "Active passages",
            &report.active_passages.to_string(),
        );
        push_line(
            &mut output,
            "Index quota bytes",
            &report
                .index_quota_bytes
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        );
        if let Some(storage) = &report.storage {
            push_line(
                &mut output,
                "Managed index bytes",
                &storage.managed_index_bytes.to_string(),
            );
            push_line(
                &mut output,
                "Reclaimable bytes",
                &storage.reclaimable_bytes.to_string(),
            );
            push_line(
                &mut output,
                "Available bytes",
                &storage.available_bytes.to_string(),
            );
            push_line(
                &mut output,
                "Recovery reserve bytes",
                &storage.reserve_bytes.to_string(),
            );
            push_line(
                &mut output,
                "Filesystem locality",
                match storage.filesystem.locality {
                    FilesystemLocality::Local => "local",
                    FilesystemLocality::Unknown => "unknown",
                },
            );
            push_line(
                &mut output,
                "Filesystem type",
                storage
                    .filesystem
                    .filesystem_type
                    .as_deref()
                    .unwrap_or("unknown"),
            );
            if storage.filesystem.sync_root.is_some() {
                push_line(&mut output, "Consumer sync root", "recognized");
            }
        }
        for source in &report.sources {
            push_source_status(&mut output, source);
        }
    } else {
        for source in &report.sources {
            if matches!(
                source.state,
                SourceSyncState::Partial | SourceSyncState::Failed
            ) {
                push_source_status(&mut output, source);
            }
        }
    }
    if report.problems.is_empty() {
        output.push_str("No actionable problems.\n");
    } else {
        for problem in &report.problems {
            output.push_str(problem.code);
            output.push_str(": ");
            output.push_str(problem.summary);
            output.push_str("\n  fix: ");
            output.push_str(problem.repair_command);
            output.push('\n');
        }
    }
    write_stdout(output.as_bytes())
}

fn push_source_status(output: &mut String, source: &SourceStatus) {
    output.push_str("Source ");
    output.push_str(source.name.as_str());
    output.push_str(": ");
    output.push_str(source_sync_state(source.state));
    output.push_str(" (");
    output.push_str(&source.active_documents.to_string());
    output.push_str(" documents, ");
    output.push_str(&source.active_passages.to_string());
    output.push_str(" passages)\n");
    if let Some(code) = &source.last_error_code {
        output.push_str("  error: ");
        output.push_str(code.as_str());
        if let Some(detail) = &source.last_error_detail {
            output.push_str(" — ");
            output.push_str(detail.as_str());
        }
        output.push('\n');
    }
}

fn render_doctor_human(
    context: &EffectiveContext,
    report: &DoctorReport,
) -> Result<(), RuntimeFailure> {
    let mut output = String::new();
    output.push_str("Index diagnosis passed.\n");
    push_line(&mut output, "Database", &human_path(&context.database_path));
    push_line(
        &mut output,
        "Application ID",
        &format!("{:#010x}", report.application_id),
    );
    push_line(
        &mut output,
        "Schema version",
        &report.schema_version.to_string(),
    );
    push_line(&mut output, "Index ID", &report.index_id.to_string());
    push_line(
        &mut output,
        "Schema checksum",
        &report.schema_checksum.to_string(),
    );
    push_line(
        &mut output,
        "Pipeline fingerprint",
        &report.pipeline_fingerprint.to_string(),
    );
    push_line(&mut output, "Journal mode", &report.journal_mode);
    push_line(&mut output, "Read only", bool_text(report.read_only));
    output.push_str(
        "Security note: filesystem hard links cannot be distinguished from their original \
         regular files; secret-path rules are defense in depth.\n",
    );
    write_stdout(output.as_bytes())
}

#[derive(Serialize)]
struct SearchJson {
    schema_version: &'static str,
    request_id: String,
    generation: Option<i64>,
    index_epoch: u64,
    project: String,
    project_id: String,
    scope_revision: u64,
    requested_mode: String,
    effective_mode: String,
    retrievers: Vec<String>,
    degraded_mode: Vec<String>,
    hints: Vec<String>,
    results: Vec<SearchPassageJson>,
    next_cursor: Option<String>,
    stop_reason: &'static str,
    examined: CandidateCountsJson,
    timing_ms: TimingJson,
}

impl SearchJson {
    fn from_response(
        context: &EffectiveContext,
        response: SearchResponse,
        drift: Option<&[DocumentDrift]>,
        explain: bool,
    ) -> Self {
        Self {
            schema_version: MCP_API_VERSION,
            request_id: Uuid::new_v4().to_string(),
            generation: response.generation,
            index_epoch: response.index_epoch,
            project: context.project_name.as_str().to_owned(),
            project_id: response.project_id.to_string(),
            scope_revision: response.scope_revision,
            requested_mode: response.requested_mode.as_str().to_owned(),
            effective_mode: response.effective_mode.as_str().to_owned(),
            retrievers: response
                .retrievers
                .iter()
                .map(|value| value.as_str().to_owned())
                .collect(),
            degraded_mode: Vec::new(),
            hints: Vec::new(),
            results: response
                .results
                .into_iter()
                .map(|passage| {
                    let source_state =
                        source_state(drift_for(drift, passage.source_id, passage.document_id));
                    let score = explain.then(|| SearchScoreJson {
                        fused: passage.score.fused,
                        fusion_units: passage.score.fusion_units,
                        lists: passage
                            .score
                            .lists
                            .iter()
                            .map(RankJson::from_rank)
                            .collect(),
                    });
                    SearchPassageJson {
                        citation_uri: passage.citation().to_string(),
                        index_id: passage.index_id.to_string(),
                        source_id: passage.source_id.to_string(),
                        document_id: passage.document_id.to_string(),
                        revision_sha256: passage.revision_sha256.to_string(),
                        source_uri: passage.source_uri,
                        title: passage.title,
                        span: SpanJson {
                            start_byte: passage.byte_span.start(),
                            end_byte: passage.byte_span.end(),
                            start_line: passage.line_span.start(),
                            end_line: passage.line_span.end(),
                        },
                        content: passage.content,
                        content_sha256: passage.content_sha256.to_string(),
                        source_updated_at: passage.source_updated_at,
                        indexed_at: passage.indexed_at,
                        head_generation: passage.head_generation,
                        source_state,
                        untrusted_content: passage.untrusted_content,
                        score,
                        duplicate_citations: passage
                            .duplicate_citations
                            .into_iter()
                            .map(|duplicate| DuplicateJson {
                                citation_uri: duplicate.citation.to_string(),
                                reason: match duplicate.reason {
                                    DuplicateReason::SameContent => "same_content",
                                    DuplicateReason::OverlappingSpan => "overlapping_span",
                                },
                            })
                            .collect(),
                    }
                })
                .collect(),
            next_cursor: None,
            stop_reason: search_stop_reason(response.stop_reason),
            examined: CandidateCountsJson {
                exact: response.examined.exact,
                exact_fallback: response.examined.exact_fallback,
                lexical: response.examined.lexical,
            },
            timing_ms: TimingJson {
                exact: response.timing.exact_ms,
                exact_fallback: response.timing.exact_fallback_ms,
                lexical: response.timing.lexical_ms,
                fusion: response.timing.fusion_ms,
                total: response.timing.total_ms,
            },
        }
    }
}

#[derive(Serialize)]
struct SearchPassageJson {
    citation_uri: String,
    index_id: String,
    source_id: String,
    document_id: String,
    revision_sha256: String,
    source_uri: String,
    title: String,
    span: SpanJson,
    content: String,
    content_sha256: String,
    source_updated_at: Option<String>,
    indexed_at: String,
    head_generation: i64,
    source_state: &'static str,
    untrusted_content: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<SearchScoreJson>,
    duplicate_citations: Vec<DuplicateJson>,
}

#[derive(Serialize)]
struct SpanJson {
    start_byte: u64,
    end_byte: u64,
    start_line: u64,
    end_line: u64,
}

#[derive(Serialize)]
struct SearchScoreJson {
    fused: f64,
    fusion_units: u64,
    lists: Vec<RankJson>,
}

#[derive(Serialize)]
struct RankJson {
    name: &'static str,
    rank: usize,
    contribution_units: u64,
    backend_score: Option<f64>,
}

impl RankJson {
    fn from_rank(rank: &RankExplanation) -> Self {
        Self {
            name: rank.retriever.as_str(),
            rank: rank.rank,
            contribution_units: rank.contribution_units,
            backend_score: rank.backend_score,
        }
    }
}

#[derive(Serialize)]
struct DuplicateJson {
    citation_uri: String,
    reason: &'static str,
}

#[derive(Serialize)]
struct CandidateCountsJson {
    exact: usize,
    exact_fallback: usize,
    lexical: usize,
}

#[derive(Serialize)]
struct TimingJson {
    exact: u64,
    exact_fallback: u64,
    lexical: u64,
    fusion: u64,
    total: u64,
}

#[derive(Serialize)]
struct GetJson {
    schema_version: &'static str,
    request_id: String,
    requested_citation_uri: String,
    returned_citation_uri: String,
    requested_line_span: LineSpanJson,
    returned_line_span: LineSpanJson,
    source_uri: String,
    title: String,
    metadata: Value,
    source_updated_at: Option<String>,
    indexed_at: String,
    content: String,
    body_sha256: String,
    source_hash_verification: String,
    untrusted_content: bool,
}

#[derive(Serialize)]
struct LineSpanJson {
    start: u64,
    end: u64,
}

#[derive(Serialize)]
struct StatusJson<'a> {
    schema_version: &'static str,
    request_id: String,
    index_id: String,
    index: &'a str,
    project_id: String,
    project: &'a str,
    problems_only: bool,
    #[serde(flatten)]
    report: &'a StatusReport,
}

#[derive(Serialize)]
struct ContextJson {
    schema_version: &'static str,
    request_id: String,
    index_id: String,
    index: String,
    project_id: String,
    project: String,
    scope_revision: u64,
    source_id: String,
    source: String,
    source_root: String,
    canonical_root: Option<String>,
    binding_id: Option<String>,
    selection_source: &'static str,
    database_path: String,
    index_quota_bytes: Option<u64>,
    config_file: String,
    config_dir: String,
    data_dir: String,
    cache_dir: String,
}

impl ContextJson {
    fn from_context(context: &EffectiveContext, config_file: &Path) -> Self {
        Self {
            schema_version: MCP_API_VERSION,
            request_id: Uuid::new_v4().to_string(),
            index_id: context.index_id.to_string(),
            index: context.index_name.as_str().to_owned(),
            project_id: context.project_id.to_string(),
            project: context.project_name.as_str().to_owned(),
            scope_revision: context.scope_revision,
            source_id: context.source_id.to_string(),
            source: context.source_name.as_str().to_owned(),
            source_root: json_path(&context.source_root),
            canonical_root: context.canonical_root.as_deref().map(json_path),
            binding_id: context.binding_id.map(|value| value.to_string()),
            selection_source: selection_source(context.selection_source),
            database_path: json_path(&context.database_path),
            index_quota_bytes: context.index_quota_bytes,
            config_file: json_path(config_file),
            config_dir: json_path(context.managed_paths.config_dir()),
            data_dir: json_path(context.managed_paths.data_dir()),
            cache_dir: json_path(context.managed_paths.cache_dir()),
        }
    }
}

fn client_config_value(client: ClientKind, executable: &Path, binding: &str) -> Value {
    let server = json!({
        "command": json_path(executable),
        "args": ["mcp", "--binding", binding],
    });
    match client {
        ClientKind::Codex => json!({"mcp_servers": {"hsum": server}}),
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop => {
            json!({"mcpServers": {"hsum": server}})
        }
        ClientKind::Generic => json!({"hsum": server}),
    }
}

fn command_requests_json(command: &Command) -> bool {
    matches!(
        command,
        Command::Search(SearchArgs { json: true, .. })
            | Command::Get(GetArgs { json: true, .. })
            | Command::Status(StatusArgs { json: true, .. })
            | Command::Context(crate::cli::ContextArgs { json: true })
    )
}

fn render_failure(error: &PublicError, json: bool) -> io::Result<()> {
    if json {
        let mut bytes = serde_json::to_vec(error).map_err(io::Error::other)?;
        bytes.push(b'\n');
        write_stderr_raw(&bytes)
    } else {
        let mut text = error.render_human();
        text.push('\n');
        write_stderr_raw(text.as_bytes())
    }
}

fn write_json_stdout(value: &impl Serialize) -> Result<(), RuntimeFailure> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::Unexpected, "serialize_output", error)
    })?;
    bytes.push(b'\n');
    write_stdout(&bytes)
}

fn write_stdout(bytes: &[u8]) -> Result<(), RuntimeFailure> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(bytes)
        .map_err(|error| RuntimeFailure::from_error(ErrorSubcode::Unexpected, "stdout", error))?;
    stdout
        .flush()
        .map_err(|error| RuntimeFailure::from_error(ErrorSubcode::Unexpected, "stdout", error))
}

fn write_stderr(bytes: &[u8]) -> Result<(), RuntimeFailure> {
    write_stderr_raw(bytes)
        .map_err(|error| RuntimeFailure::from_error(ErrorSubcode::Unexpected, "stderr", error))
}

fn write_stderr_raw(bytes: &[u8]) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(bytes)?;
    stderr.flush()
}

fn push_line(output: &mut String, label: &str, value: &str) {
    output.push_str(label);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn absolute_executable() -> Result<PathBuf, RuntimeFailure> {
    let executable = env::current_exe().map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::Unexpected, "current_executable", error)
    })?;
    let absolute = executable.canonicalize().map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::Unexpected, "current_executable", error)
    })?;
    if !absolute.is_absolute() {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::Invariant,
            "current_executable",
            "the current executable did not resolve to an absolute path",
        ));
    }
    Ok(absolute)
}

#[cfg(unix)]
fn human_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    escape_terminal_bytes(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn human_path(path: &Path) -> String {
    escape_terminal_text(&path.to_string_lossy())
}

fn json_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&human_path(path))
}

fn shell_quote(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('\'');
    for character in value.chars() {
        if character == '\'' {
            output.push_str("'\\''");
        } else {
            output.push(character);
        }
    }
    output.push('\'');
    output
}

fn bool_text(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn confirmation_state(required: bool, supplied: bool) -> &'static str {
    match (required, supplied) {
        (false, _) => "not required",
        (true, true) => "required and supplied",
        (true, false) => "required; supply the matching confirmation flag",
    }
}

fn pointer_outcome(outcome: PointerOutcome) -> &'static str {
    match outcome {
        PointerOutcome::NotRequested => "not requested",
        PointerOutcome::WouldWrite => "would write",
        PointerOutcome::Written => "written",
        PointerOutcome::WrittenDurabilityUnknown => "written; parent-directory durability unknown",
    }
}

fn selection_source(source: SelectionSource) -> &'static str {
    match source {
        SelectionSource::Explicit => "explicit",
        SelectionSource::Environment => "environment",
        SelectionSource::TrustedRoot => "trusted_root",
        SelectionSource::ConfiguredDefault => "configured_default",
    }
}

fn client_kind(client: ClientKind) -> &'static str {
    match client {
        ClientKind::Codex => "codex",
        ClientKind::ClaudeCode => "claude-code",
        ClientKind::ClaudeDesktop => "claude-desktop",
        ClientKind::Generic => "generic",
    }
}

fn source_sync_state(state: SourceSyncState) -> &'static str {
    match state {
        SourceSyncState::NeverSucceeded => "never_succeeded",
        SourceSyncState::Healthy => "healthy",
        SourceSyncState::Partial => "partial",
        SourceSyncState::Failed => "failed",
    }
}

fn search_stop_reason(reason: SearchStopReason) -> &'static str {
    match reason {
        SearchStopReason::LimitReached => "limit_reached",
        SearchStopReason::UniqueExhausted => "unique_exhausted",
        SearchStopReason::WorkBudgetExhausted => "work_budget_exhausted",
        SearchStopReason::Deadline => "deadline",
    }
}

fn rank_summary(rank: &RankExplanation) -> String {
    let name = match rank.retriever {
        Retriever::Exact => "exact",
        Retriever::ExactFallback => "exact fallback",
        Retriever::Lexical => "BM25",
    };
    format!("{name} #{}", rank.rank)
}

fn drift_for(
    drift: Option<&[DocumentDrift]>,
    source_id: SourceId,
    document_id: DocumentId,
) -> Option<&DocumentDrift> {
    drift?.iter().find(|observation| {
        observation.source_id == source_id && observation.document_id == document_id
    })
}

fn source_state(observation: Option<&DocumentDrift>) -> &'static str {
    match observation.map(|value| value.state) {
        Some(DriftState::MetadataUnchanged) => "metadata_unchanged",
        Some(DriftState::MetadataChanged) => "changed_since_ingest",
        Some(DriftState::Missing) => "missing_since_ingest",
        Some(DriftState::Blocked | DriftState::Unknown) | None => "unverifiable",
    }
}

fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or_else(|_| ExitCode::from(1), ExitCode::from)
}

fn map_managed_paths_error(error: ManagedPathsError) -> RuntimeFailure {
    let subcode = match error {
        ManagedPathsError::EmptyHomeOverride
        | ManagedPathsError::RelativeHomeOverride { .. }
        | ManagedPathsError::RelativeDirectoryOverride { .. }
        | ManagedPathsError::Unavailable => ErrorSubcode::PathInvalid,
    };
    RuntimeFailure::from_error(subcode, "managed_paths", error)
}

fn map_context_error(error: ContextError) -> RuntimeFailure {
    let subcode = match &error {
        ContextError::ProjectNotFound => ErrorSubcode::ProjectNotFound,
        ContextError::Store(store) => store_subcode(store),
        ContextError::Sqlite(error) => sqlite_subcode(error),
        ContextError::Selection(SelectionError::NotConfigured) => ErrorSubcode::IndexNotFound,
        ContextError::Selection(
            SelectionError::TrustRequired
            | SelectionError::PointerIsOnlyHint
            | SelectionError::BindingNotTrusted { .. }
            | SelectionError::AmbiguousBinding { .. }
            | SelectionError::AmbiguousTrustedRoot { .. },
        )
        | ContextError::McpTrustRequired
        | ContextError::TrustTargetIncomplete
        | ContextError::TrustedIndexIdentityMismatch
        | ContextError::TrustedProjectIdentityMismatch
        | ContextError::TrustedSourceRootMismatch => ErrorSubcode::PathTrust,
        ContextError::ConfigRead(_)
        | ContextError::ConfigUnsafe
        | ContextError::ConfigTooLarge
        | ContextError::ConfigChangedDuringRead
        | ContextError::ConfigNotUtf8
        | ContextError::ConfigMalformed(_)
        | ContextError::ConfigSchema { .. }
        | ContextError::IncompleteConfiguredDefault
        | ContextError::IncompleteEnvironmentSelection
        | ContextError::NonUtf8Environment
        | ContextError::LogicalSelection(_)
        | ContextError::InvalidFilesystemSourceConfig(_) => ErrorSubcode::ConfigInvalid,
        ContextError::Pointer(error) => pointer_subcode(error),
        ContextError::Trust(error) => trust_subcode(error),
        ContextError::Io(source) if source.kind() == io::ErrorKind::PermissionDenied => {
            ErrorSubcode::SourceRead
        }
        ContextError::Io(_) => ErrorSubcode::PathInvalid,
        ContextError::ConflictingMcpScope => ErrorSubcode::QuerySyntax,
        ContextError::InvalidDatabaseValue(_)
        | ContextError::InvalidSourceName(_)
        | ContextError::Identity(_)
        | ContextError::AlphaSourceCardinality { .. }
        | ContextError::AlphaSourceMustBeFilesystem
        | ContextError::SourceConfigurationRootMismatch => ErrorSubcode::HeadIndexMismatch,
    };
    RuntimeFailure::from_error(subcode, "context", error)
}

fn map_init_error(error: InitError) -> RuntimeFailure {
    let subcode = match &error {
        InitError::BroadRootConfirmationRequired { .. } => {
            ErrorSubcode::BroadRootConfirmationRequired
        }
        InitError::LargeSourceConfirmationRequired { .. } => {
            ErrorSubcode::LargeSourceConfirmationRequired
        }
        InitError::TrustConfirmationRequired => ErrorSubcode::TrustConfirmationRequired,
        InitError::ForcePointerWithoutWrite | InitError::UnsafePointerPath { .. } => {
            ErrorSubcode::PointerInvalid
        }
        InitError::PointerExists { .. } => ErrorSubcode::PointerExists,
        InitError::IndexPathOccupied { .. } => ErrorSubcode::IndexPathOccupied,
        InitError::FilesystemIngest(error) => filesystem_ingest_subcode(error),
        InitError::StoragePreflight(error) => storage_preflight_subcode(error),
        InitError::Store(error) => store_subcode(error),
        InitError::Trust(error) => trust_subcode(error),
        InitError::Pointer(error) => pointer_subcode(error),
        InitError::SourceConfig(error) => source_config_subcode(error),
        InitError::TrustedIndexMissing { .. } => ErrorSubcode::IndexNotFound,
        InitError::ExistingBindingConflict { .. }
        | InitError::AmbiguousRootBindings { .. }
        | InitError::LogicalIndexConflict { .. }
        | InitError::TrustedIndexIdentityMismatch { .. }
        | InitError::TrustedProjectIdentityMismatch { .. }
        | InitError::TrustedSourceIdentityMismatch { .. }
        | InitError::TrustedSourceRootMismatch { .. }
        | InitError::AlphaSourceCardinality { .. }
        | InitError::InvalidTrustedCardinality => ErrorSubcode::PathTrust,
        InitError::SourceEstimateOverflow => ErrorSubcode::MemoryBudget,
        InitError::IndexNameSpaceExhausted { .. } => ErrorSubcode::IndexPathOccupied,
        InitError::AccountHomeUnavailable(_)
        | InitError::NonUtf8Root { .. }
        | InitError::DatabasePathHasNoParent { .. }
        | InitError::TrustPathHasNoParent { .. }
        | InitError::PointerPathHasNoParent { .. } => ErrorSubcode::PathInvalid,
        InitError::PointerRollbackConflict { .. } => ErrorSubcode::PointerInvalid,
        InitError::Slug(_) => ErrorSubcode::ConfigInvalid,
        InitError::Io(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            ErrorSubcode::IndexWrite
        }
        InitError::Io(error) if error.kind() == io::ErrorKind::StorageFull => {
            ErrorSubcode::DiskSpace
        }
        InitError::Io(_) => ErrorSubcode::StorageIo,
    };
    RuntimeFailure::from_error(subcode, "init", error)
}

fn map_filesystem_ingest_error(error: FilesystemIngestError) -> RuntimeFailure {
    let subcode = filesystem_ingest_subcode(&error);
    RuntimeFailure::from_error(subcode, "ingest", error)
}

fn filesystem_ingest_subcode(error: &FilesystemIngestError) -> ErrorSubcode {
    match error {
        FilesystemIngestError::Discovery(error) => discovery_subcode(error),
        FilesystemIngestError::Chunk { source, .. } => match source {
            ChunkError::InvalidUtf8 { .. } => ErrorSubcode::InvalidUtf8,
            ChunkError::NulContent => ErrorSubcode::NulContent,
            ChunkError::BoundaryInvariant | ChunkError::TooManyChunks => ErrorSubcode::Invariant,
        },
        FilesystemIngestError::Store(error) => store_subcode(error),
        FilesystemIngestError::StoragePreflight(error) => storage_preflight_subcode(error),
        FilesystemIngestError::StrictSourceFailures { .. } => ErrorSubcode::EnumerationIncomplete,
        FilesystemIngestError::UnsupportedDiscoveredPath { .. }
        | FilesystemIngestError::Revision(_) => ErrorSubcode::SourceRead,
        FilesystemIngestError::StagingRead(error) if error.kind() == io::ErrorKind::StorageFull => {
            ErrorSubcode::DiskSpace
        }
        FilesystemIngestError::StagingRead(_) => ErrorSubcode::StorageIo,
        FilesystemIngestError::StagingDirectoryMissing => ErrorSubcode::PathInvalid,
        FilesystemIngestError::SourceEstimateOverflow => ErrorSubcode::MemoryBudget,
        FilesystemIngestError::SourceAuthorityMismatch => ErrorSubcode::ConfigInvalid,
    }
}

fn storage_preflight_subcode(error: &StoragePreflightError) -> ErrorSubcode {
    match error {
        StoragePreflightError::InsufficientCapacity { .. } => ErrorSubcode::DiskSpace,
        StoragePreflightError::QuotaExceeded { .. } => ErrorSubcode::IndexQuota,
        StoragePreflightError::ArithmeticOverflow { .. } => ErrorSubcode::MemoryBudget,
        StoragePreflightError::UnsupportedNetworkFilesystem { .. }
        | StoragePreflightError::UnsafeManagedFile { .. } => ErrorSubcode::UnsupportedStorage,
        StoragePreflightError::PathHasNoParent { .. } => ErrorSubcode::PathInvalid,
        StoragePreflightError::FilesystemInspectionUnavailable { .. }
        | StoragePreflightError::CapacityUnavailable { .. } => ErrorSubcode::StorageIo,
    }
}

fn pointer_subcode(error: &PointerError) -> ErrorSubcode {
    match error {
        PointerError::Malformed(_)
        | PointerError::UnsupportedSchema { .. }
        | PointerError::InvalidIndexName(_)
        | PointerError::InvalidProjectName(_)
        | PointerError::Unsafe
        | PointerError::TooLarge
        | PointerError::Changed
        | PointerError::NotUtf8 => ErrorSubcode::PointerInvalid,
        PointerError::Read(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            ErrorSubcode::SourceRead
        }
        PointerError::Read(_) => ErrorSubcode::PointerInvalid,
        PointerError::Serialize(_) => ErrorSubcode::Invariant,
    }
}

fn trust_subcode(error: &TrustError) -> ErrorSubcode {
    match error {
        TrustError::Malformed(_)
        | TrustError::UnsupportedSchema { .. }
        | TrustError::InvalidIndexName(_)
        | TrustError::InvalidProjectName(_)
        | TrustError::NonCanonicalRoot { .. }
        | TrustError::InvalidStoredRoot { .. }
        | TrustError::ConflictingRoot { .. }
        | TrustError::ConflictingIdentity
        | TrustError::DuplicateBinding { .. }
        | TrustError::NonRandomBinding { .. }
        | TrustError::TooManyBindings { .. }
        | TrustError::NonUtf8Root { .. }
        | TrustError::UnsafeFile
        | TrustError::TooLarge
        | TrustError::ChangedDuringRead
        | TrustError::NotUtf8
        | TrustError::InvalidIdentity(_) => ErrorSubcode::TrustRegistryInvalid,
        TrustError::CanonicalizeRoot { .. }
        | TrustError::RootNotDirectory { .. }
        | TrustError::MissingParent { .. } => ErrorSubcode::PathInvalid,
        TrustError::Read(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            ErrorSubcode::SourceRead
        }
        TrustError::Read(_) => ErrorSubcode::TrustRegistryInvalid,
        TrustError::Serialize(_) => ErrorSubcode::Invariant,
        TrustError::Write(error) if error.kind() == io::ErrorKind::StorageFull => {
            ErrorSubcode::DiskSpace
        }
        TrustError::Write(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            ErrorSubcode::IndexWrite
        }
        TrustError::Write(_) => ErrorSubcode::StorageIo,
    }
}

fn source_config_subcode(error: &SourceConfigError) -> ErrorSubcode {
    match error {
        SourceConfigError::TooLarge
        | SourceConfigError::UnsupportedSchema { .. }
        | SourceConfigError::RootNotAbsolute
        | SourceConfigError::RootNotLexicallyCanonical
        | SourceConfigError::NonUtf8Root
        | SourceConfigError::Parse(_)
        | SourceConfigError::InvalidIndexQuota
        | SourceConfigError::Discovery(_) => ErrorSubcode::ConfigInvalid,
        SourceConfigError::Serialize(_) => ErrorSubcode::Invariant,
    }
}

fn sqlite_subcode(error: &rusqlite::Error) -> ErrorSubcode {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => match failure.code {
            SqliteErrorCode::DiskFull => ErrorSubcode::DiskSpace,
            SqliteErrorCode::SystemIoFailure
            | SqliteErrorCode::CannotOpen
            | SqliteErrorCode::FileLockingProtocolFailed => ErrorSubcode::SqliteIo,
            SqliteErrorCode::DatabaseCorrupt | SqliteErrorCode::NotADatabase => {
                ErrorSubcode::SqliteCorrupt
            }
            SqliteErrorCode::DatabaseBusy | SqliteErrorCode::DatabaseLocked => {
                ErrorSubcode::WriterLock
            }
            SqliteErrorCode::PermissionDenied
            | SqliteErrorCode::ReadOnly
            | SqliteErrorCode::AuthorizationForStatementDenied => ErrorSubcode::IndexWrite,
            SqliteErrorCode::OutOfMemory | SqliteErrorCode::TooBig => ErrorSubcode::MemoryBudget,
            SqliteErrorCode::OperationInterrupted => ErrorSubcode::ClientCancelled,
            SqliteErrorCode::NoLargeFileSupport => ErrorSubcode::UnsupportedStorage,
            SqliteErrorCode::ConstraintViolation
            | SqliteErrorCode::TypeMismatch
            | SqliteErrorCode::ApiMisuse
            | SqliteErrorCode::ParameterOutOfRange
            | SqliteErrorCode::SchemaChanged => ErrorSubcode::Invariant,
            _ => ErrorSubcode::Unexpected,
        },
        rusqlite::Error::InvalidPath(_) => ErrorSubcode::PathInvalid,
        rusqlite::Error::QueryReturnedNoRows
        | rusqlite::Error::QueryReturnedMoreThanOneRow
        | rusqlite::Error::FromSqlConversionFailure(_, _, _)
        | rusqlite::Error::IntegralValueOutOfRange(_, _)
        | rusqlite::Error::Utf8Error(_, _)
        | rusqlite::Error::InvalidColumnIndex(_)
        | rusqlite::Error::InvalidColumnName(_)
        | rusqlite::Error::InvalidColumnType(_, _, _) => ErrorSubcode::HeadIndexMismatch,
        _ => ErrorSubcode::Invariant,
    }
}

fn io_subcode(error: &io::Error, permission: ErrorSubcode) -> ErrorSubcode {
    match error.kind() {
        io::ErrorKind::PermissionDenied => permission,
        io::ErrorKind::StorageFull => ErrorSubcode::DiskSpace,
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput => {
            ErrorSubcode::PathInvalid
        }
        _ => ErrorSubcode::StorageIo,
    }
}

fn render_storage_warnings(preflight: &StoragePreflight) -> Result<(), RuntimeFailure> {
    if preflight.filesystem.locality == FilesystemLocality::Unknown {
        write_stderr(
            b"warning: the managed index filesystem could not be proven local; run `hsum doctor` \
              before relying on crash-safety guarantees\n",
        )?;
    }
    if preflight.filesystem.sync_root.is_some() {
        write_stderr(
            b"warning: the managed index is inside a recognized consumer-sync root; this storage \
              location is unsupported for hSUM indexes\n",
        )?;
    }
    Ok(())
}

fn discovery_subcode(error: &DiscoveryError) -> ErrorSubcode {
    match error {
        DiscoveryError::RootMissing { .. }
        | DiscoveryError::RootIsSymlink { .. }
        | DiscoveryError::RootNotDirectory { .. } => ErrorSubcode::PathInvalid,
        DiscoveryError::RootOpen { .. } => ErrorSubcode::SourceRead,
        DiscoveryError::DirectoryUnreadable { .. } | DiscoveryError::DirectoryChanged { .. } => {
            ErrorSubcode::EnumerationIncomplete
        }
        DiscoveryError::StagingEstimateExceeded { .. } => ErrorSubcode::EnumerationIncomplete,
        DiscoveryError::SourceLimitExceeded { .. } => ErrorSubcode::MemoryBudget,
        DiscoveryError::TraversalLimitExceeded { .. } => ErrorSubcode::MemoryBudget,
        DiscoveryError::InvalidIgnoreRule { .. }
        | DiscoveryError::InvalidIgnoreFile { .. }
        | DiscoveryError::InvalidPattern { .. } => ErrorSubcode::ConfigInvalid,
        DiscoveryError::Staging { kind, .. } => match kind {
            io::ErrorKind::StorageFull => ErrorSubcode::DiskSpace,
            io::ErrorKind::PermissionDenied => ErrorSubcode::IndexWrite,
            io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidInput => ErrorSubcode::PathInvalid,
            _ => ErrorSubcode::StorageIo,
        },
        DiscoveryError::UnsupportedPlatform => ErrorSubcode::UnsupportedPlatform,
    }
}

fn map_search_error(error: SearchError) -> RuntimeFailure {
    let subcode = match &error {
        SearchError::Query(QueryError::Blank) => ErrorSubcode::QueryEmpty,
        SearchError::Query(_) => ErrorSubcode::QuerySyntax,
        SearchError::InvalidLimit { .. } | SearchError::InvalidDeadline { .. } => {
            ErrorSubcode::LimitOutOfRange
        }
        SearchError::ProjectNotFound => ErrorSubcode::ProjectNotFound,
        SearchError::DeadlineExceeded => ErrorSubcode::RequestDeadline,
        SearchError::NonFiniteScore => ErrorSubcode::NonfiniteScore,
        SearchError::Corrupt(_) => ErrorSubcode::HeadIndexMismatch,
        SearchError::Sqlite(error) => sqlite_subcode(error),
        SearchError::ExactMatcher => ErrorSubcode::Invariant,
    };
    RuntimeFailure::from_error(subcode, "search", error)
}

fn map_get_error(error: GetError) -> RuntimeFailure {
    let subcode = match &error {
        GetError::ScopeDenied => ErrorSubcode::ScopeHidden,
        GetError::EvidenceNotFound => ErrorSubcode::EvidenceNotFound,
        GetError::InvalidBound { .. } | GetError::BoundBelowPassage { .. } => {
            ErrorSubcode::LimitOutOfRange
        }
        GetError::Corrupt(_) => ErrorSubcode::HeadIndexMismatch,
        GetError::Sqlite(error) => sqlite_subcode(error),
    };
    RuntimeFailure::from_error(subcode, "get", error)
}

fn map_status_error(error: StatusError) -> RuntimeFailure {
    let subcode = match &error {
        StatusError::Store(error) => store_subcode(error),
        StatusError::Sqlite(error) => sqlite_subcode(error),
        StatusError::Invalid(_) => ErrorSubcode::Invariant,
    };
    RuntimeFailure::from_error(subcode, "status", error)
}

fn map_store_error(error: StoreError) -> RuntimeFailure {
    let subcode = store_subcode(&error);
    RuntimeFailure::from_error(subcode, "index", error)
}

fn store_subcode(error: &StoreError) -> ErrorSubcode {
    match error {
        StoreError::Sqlite(error) => sqlite_subcode(error),
        StoreError::Io(error) => io_subcode(error, ErrorSubcode::IndexWrite),
        StoreError::Time(_) => ErrorSubcode::Unexpected,
        StoreError::AlreadyExists(_) => ErrorSubcode::IndexPathOccupied,
        StoreError::WalUnavailable(_) => ErrorSubcode::UnsupportedStorage,
        StoreError::MissingPath(_) => ErrorSubcode::IndexNotFound,
        StoreError::UnsafeIndexPath(_) => ErrorSubcode::UnsupportedStorage,
        StoreError::WriterLockBusy { .. } => ErrorSubcode::WriterLock,
        StoreError::InvalidApplicationId { .. } => ErrorSubcode::ApplicationId,
        StoreError::UnsupportedSchemaVersion { current, found } if found > current => {
            ErrorSubcode::DowngradeUnsupported
        }
        StoreError::UnsupportedSchemaVersion { .. } => ErrorSubcode::MigrationRequired,
        StoreError::SchemaChecksumMismatch
        | StoreError::SchemaManifestMismatch
        | StoreError::MissingSchemaObject(_)
        | StoreError::UnexpectedExecutableSchema
        | StoreError::MigrationChainInvalid
        | StoreError::PipelineFingerprintMismatch
        | StoreError::InvalidSchemaVersion(_)
        | StoreError::InvalidMetadata(_) => ErrorSubcode::SchemaChecksum,
        StoreError::IntegrityCheckFailed(_) => ErrorSubcode::SqliteCorrupt,
        StoreError::ForeignKeyCheckFailed
        | StoreError::ActiveIndexParity(_)
        | StoreError::ImmutableEvidenceMismatch(_)
        | StoreError::GenerationInvariant(_)
        | StoreError::ChunkLayoutMismatch
        | StoreError::ScopeConflict => ErrorSubcode::HeadIndexMismatch,
        StoreError::UnsafeWriterLock(_) | StoreError::WriterLockUnsupported => {
            ErrorSubcode::UnsupportedStorage
        }
        StoreError::ReadOnlyRequired | StoreError::ReadWriteRequired => ErrorSubcode::Invariant,
        StoreError::EmptySnapshotConfirmationRequired => {
            ErrorSubcode::EmptySnapshotConfirmationRequired
        }
        StoreError::MassDeleteConfirmationRequired { .. } => {
            ErrorSubcode::MassDeleteConfirmationRequired
        }
        StoreError::IntegerOverflow => ErrorSubcode::MemoryBudget,
        StoreError::InvalidPreparedDocument(_)
        | StoreError::DuplicateConnectorKey
        | StoreError::HashCollision
        | StoreError::WriterLockMismatch => ErrorSubcode::Invariant,
    }
}

fn map_mcp_server_error(error: McpServerError) -> RuntimeFailure {
    let subcode = match &error {
        McpServerError::Store(error) => store_subcode(error),
        McpServerError::ProjectNotFound => ErrorSubcode::ProjectNotFound,
        McpServerError::InvalidMetadata => ErrorSubcode::Invariant,
        McpServerError::Initialize(_) => ErrorSubcode::ClientDisconnected,
        McpServerError::Join(_) => ErrorSubcode::Unexpected,
    };
    RuntimeFailure::from_error(subcode, "mcp", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BroadRootReason;
    use crate::domain::ErrorCode;

    fn sqlite_failure(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    #[test]
    fn sqlite_capacity_io_and_corruption_failures_stay_distinct() {
        let full = sqlite_failure(rusqlite::ffi::SQLITE_FULL);
        let io = sqlite_failure(rusqlite::ffi::SQLITE_IOERR);
        let corrupt = sqlite_failure(rusqlite::ffi::SQLITE_CORRUPT);
        let not_database = sqlite_failure(rusqlite::ffi::SQLITE_NOTADB);

        assert_eq!(sqlite_subcode(&full), ErrorSubcode::DiskSpace);
        assert_eq!(sqlite_subcode(&io), ErrorSubcode::SqliteIo);
        assert_eq!(sqlite_subcode(&corrupt), ErrorSubcode::SqliteCorrupt);
        assert_eq!(sqlite_subcode(&not_database), ErrorSubcode::SqliteCorrupt);
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::DiskSpace, "request", DOCS_BASE).code,
            ErrorCode::ResourceExhausted
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::SqliteIo, "request", DOCS_BASE).code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn confirmations_and_occupied_paths_have_specific_public_subcodes() {
        let broad = map_init_error(InitError::BroadRootConfirmationRequired {
            root: PathBuf::from("/"),
            reason: BroadRootReason::FilesystemRoot,
        });
        let large = map_init_error(InitError::LargeSourceConfirmationRequired {
            files: 2,
            bytes: 3,
            max_files: 1,
            max_bytes: 1,
        });
        let trust = map_init_error(InitError::TrustConfirmationRequired);

        assert_eq!(
            broad.public.subcode,
            ErrorSubcode::BroadRootConfirmationRequired
        );
        assert_eq!(
            large.public.subcode,
            ErrorSubcode::LargeSourceConfirmationRequired
        );
        assert_eq!(
            trust.public.subcode,
            ErrorSubcode::TrustConfirmationRequired
        );
        assert_eq!(
            store_subcode(&StoreError::EmptySnapshotConfirmationRequired),
            ErrorSubcode::EmptySnapshotConfirmationRequired
        );
        assert_eq!(
            store_subcode(&StoreError::MassDeleteConfirmationRequired {
                absent: 2,
                eligible_prior: 3,
            }),
            ErrorSubcode::MassDeleteConfirmationRequired
        );
        assert_eq!(
            store_subcode(&StoreError::AlreadyExists(PathBuf::from("index.sqlite"))),
            ErrorSubcode::IndexPathOccupied
        );
    }

    #[test]
    fn malformed_authority_and_storage_failures_do_not_collapse() {
        assert_eq!(
            pointer_subcode(&PointerError::TooLarge),
            ErrorSubcode::PointerInvalid
        );
        assert_eq!(
            trust_subcode(&TrustError::TooLarge),
            ErrorSubcode::TrustRegistryInvalid
        );
        assert_eq!(
            source_config_subcode(&SourceConfigError::InvalidIndexQuota),
            ErrorSubcode::ConfigInvalid
        );
        assert_eq!(
            storage_preflight_subcode(&StoragePreflightError::UnsupportedNetworkFilesystem {
                filesystem_type: "nfs".to_owned(),
            }),
            ErrorSubcode::UnsupportedStorage
        );
        assert_eq!(
            storage_preflight_subcode(&StoragePreflightError::InsufficientCapacity {
                available_bytes: 1,
                required_bytes: 2,
                reserve_bytes: 1,
            }),
            ErrorSubcode::DiskSpace
        );
        assert_eq!(
            storage_preflight_subcode(&StoragePreflightError::QuotaExceeded {
                quota_bytes: 1,
                required_bytes: 2,
                reserve_bytes: 1,
            }),
            ErrorSubcode::IndexQuota
        );
        assert_eq!(
            discovery_subcode(&DiscoveryError::Staging {
                kind: io::ErrorKind::StorageFull,
                detail: "fault-injected ENOSPC".to_owned(),
            }),
            ErrorSubcode::DiskSpace
        );
    }
}
