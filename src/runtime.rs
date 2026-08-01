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
use rusqlite::ffi::ErrorCode as SqliteErrorCode;
use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::app::{
    AddFilesystemSourceRequest, AddJsonlSourceRequest, ContextError, ContextRequest,
    DeleteIndexRequest, EffectiveContext, EvidenceSourceState, FilesystemIngestError, GetEvidence,
    GetEvidenceError, GetEvidenceFieldLimits, GetEvidenceRequest, IndexManagementError, InitError,
    InitNextStep, InitOutcome, InitRequest, PointerOutcome, ProjectIngestError, ProjectIngestPlan,
    ProjectManagementError, SearchEvidence, SearchEvidenceError, SearchEvidenceFieldLimits,
    SearchEvidencePage, SearchEvidenceRequest, SearchEvidenceSnapshot, SetProjectRootRequest,
    SourceConfigError, SourceManagementError, StatusEvidence, StatusEvidenceError,
    StatusEvidenceFieldLimits, StatusEvidenceRequest, TrustRequest, add_filesystem_source,
    add_jsonl_source, attach_jsonl_source, create_project, delete_index, detach_jsonl_source,
    ingest_project_sources_with_timeout, initialize, list_projects, list_sources_in_scope,
    plan_project_sources_with_timeout, remove_jsonl_source, resolve_context, resolve_trust_target,
    set_project_root, trust_repository, use_project,
};
use crate::cli::{
    AgentPolicyMode, BackupCommand, BackupCreateArgs, BackupListArgs, Cli, ClientCommand,
    ClientConfigArgs, ClientConfigFormat, ClientDoctorArgs, ClientKind, Command,
    ConfigMigrateApplyArgs, ConfigMigrateCommand, ConfigMigratePlanArgs, DoctorArgs, DoctorCommand,
    DoctorReportArgs, ErrorHelpArgs, ForgetApplyArgs, ForgetCommand, ForgetPlanArgs, GetArgs,
    GlobalOptions, HelpCommand, IndexCommand, IndexDeleteArgs, IngestArgs, InitArgs,
    IntegrationActivateArgs, IntegrationCommand, IntegrationInstallArgs, IntegrationRepairArgs,
    IntegrationStatusArgs, IntegrationUninstallArgs, IntegrationWorkspaceArgs, McpArgs,
    MigrateApplyArgs, MigrateCommand, MigratePlanArgs, ModelCommand, ModelImportArgs,
    ModelInstallArgs, ModelInstallKind, ModelListArgs, ModelRemoveArgs, ModelVerifyArgs,
    ProjectCommand, ProjectCreateArgs, ProjectListArgs, ProjectSetRootArgs, ProjectUseArgs,
    PruneApplyArgs, PruneCommand, PrunePlanArgs, RestoreApplyArgs, RestoreCommand, SearchArgs,
    SearchMode as CliSearchMode, SourceAddArgs, SourceAttachArgs, SourceCommand,
    SourceConnectorCommand, SourceDetachArgs, SourceListArgs, SourceRemoveArgs, StatusArgs,
    TrustArgs, escape_terminal_bytes, escape_terminal_text, exit_code_for, render_completions,
    render_man, render_verbose_version, verbose_version_requested,
};
use crate::config::{
    CONFIG_SCHEMA_VERSION, ConfigMigrationError, ConfigMigrationPlan, ManagedPaths,
    ManagedPathsError, PREVIOUS_CONFIG_SCHEMA_VERSION, PointerError, SelectionError, SelectionMode,
    SelectionSource, TRUST_PREVIOUS_SCHEMA_VERSION, TRUST_SCHEMA_VERSION, TrustError,
    apply_config_migration, plan_config_migration,
};
use crate::domain::{DocumentId, ErrorSubcode, PublicError, SourceId};
use crate::ingest::{ChunkError, DiscoveryError};
use crate::integration::{
    AgentPolicyError, AgentPolicyState, CodexAgentPolicy, CodexIntegration, CodexRegistrationState,
    IntegrationError, WorkspacePolicy, WorkspacePolicyError,
};
use crate::mcp::{
    MAX_MCP_FRAME_BYTES, McpServerError, serve_stdio, serve_workspace_stdio, validate_frame,
};
use crate::model::{ModelError, ModelMutation, ModelStore, discover_model_pins};
use crate::protocol::{
    API_VERSION as MCP_API_VERSION, CliSearchOutput, CliStatusOutput, EvidenceGetOutput,
    GetPacketError, SearchCursorError, SearchCursorStaleCause, SearchCursorState,
    decode_search_cursor, encode_search_cursor, retrieval_config_fingerprint,
    search_cursor_stale_cause,
};
use crate::search::query::QueryError;
use crate::search::{
    GetError, GetRequest, MAX_SEARCH_LIMIT, RankExplanation, Retriever, SearchError, SearchMode,
    SearchRequest, SearchResponse, SearchStopReason,
};
use crate::status::{DocumentDrift, SourceStatus, SourceSyncState, StatusError, StatusReport};
use crate::store::{
    BackupReservation, DEFAULT_WRITER_LOCK_TIMEOUT, DeleteConfirmations, Doctor, DoctorReport,
    DoctorSupportReport, FilesystemLocality, ForgetPlan, IngestOutcome, MaintenanceError,
    ManagedBackupCatalog, ManagedBackupDisposition, ManagedBackupDispositionOutcome,
    ManagedBackupError, ManagedBackupInventoryItem, ManagedBackupKind, MigrationPlan, PlanEnvelope,
    PrunePlan, ReaderLease, RestorePlan, SourceIngestState, StoragePreflight,
    StoragePreflightError, StoreError, WriterLock, apply_forget, apply_migration, apply_prune,
    apply_restore, create_backup, inspect_backup, plan_forget, plan_migration, plan_prune,
    read_plan, validate_plan_envelope, write_plan, write_private_json,
};

const SOURCE_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const CLIENT_DOCTOR_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_DOCTOR_STDERR_LIMIT: usize = 64 * 1024;
const INTEGRATION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

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
        Command::Source(arguments) => match arguments.command {
            SourceCommand::Add(arguments) => {
                run_source_add(&cli.global, managed_paths, current_dir, arguments)
            }
            SourceCommand::List(arguments) => {
                run_source_list(&cli.global, managed_paths, current_dir, arguments)
            }
            SourceCommand::Attach(arguments) => {
                run_source_attach(&cli.global, managed_paths, current_dir, arguments)
            }
            SourceCommand::Detach(arguments) => {
                run_source_detach(&cli.global, managed_paths, current_dir, arguments)
            }
            SourceCommand::Remove(arguments) => {
                run_source_remove(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Project(arguments) => match arguments.command {
            ProjectCommand::Create(arguments) => {
                run_project_create(&cli.global, managed_paths, current_dir, arguments)
            }
            ProjectCommand::List(arguments) => {
                run_project_list(&cli.global, managed_paths, current_dir, arguments)
            }
            ProjectCommand::Use(arguments) => {
                run_project_use(&cli.global, managed_paths, current_dir, arguments)
            }
            ProjectCommand::SetRoot(arguments) => {
                run_project_set_root(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Index(arguments) => match arguments.command {
            IndexCommand::Delete(arguments) => {
                run_index_delete(&cli.global, managed_paths, arguments)
            }
        },
        Command::Search(arguments) => {
            run_search(&cli.global, managed_paths, current_dir, arguments)
        }
        Command::Get(arguments) => run_get(&cli.global, managed_paths, current_dir, arguments),
        Command::Status(arguments) => {
            run_status(&cli.global, managed_paths, current_dir, arguments)
        }
        Command::Doctor(arguments) => match arguments.command.clone() {
            Some(DoctorCommand::Report(report)) => {
                run_doctor_report(&cli.global, managed_paths, current_dir, report)
            }
            None => run_doctor(&cli.global, managed_paths, current_dir, arguments),
        },
        Command::Backup(arguments) => match arguments.command {
            BackupCommand::Create(arguments) => {
                run_backup_create(&cli.global, managed_paths, current_dir, arguments)
            }
            BackupCommand::List(arguments) => {
                run_backup_list(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Prune(arguments) => match arguments.command {
            PruneCommand::Plan(arguments) => {
                run_prune_plan(&cli.global, managed_paths, current_dir, arguments)
            }
            PruneCommand::Apply(arguments) => {
                run_prune_apply(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Forget(arguments) => match arguments.command {
            ForgetCommand::Plan(arguments) => {
                run_forget_plan(&cli.global, managed_paths, current_dir, arguments)
            }
            ForgetCommand::Apply(arguments) => {
                run_forget_apply(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Restore(arguments) => match arguments.command {
            RestoreCommand::Apply(arguments) => {
                run_restore_apply(&cli.global, managed_paths, current_dir, arguments)
            }
        },
        Command::Migrate(arguments) => match arguments.command {
            MigrateCommand::Plan(arguments) => {
                run_migrate_plan(managed_paths, current_dir, arguments)
            }
            MigrateCommand::Apply(arguments) => {
                run_migrate_apply(managed_paths, current_dir, arguments)
            }
        },
        Command::Config(arguments) => match arguments.command {
            crate::cli::ConfigCommand::Migrate(arguments) => match arguments.command {
                ConfigMigrateCommand::Plan(arguments) => {
                    run_config_migrate_plan(&cli.global, managed_paths, current_dir, arguments)
                }
                ConfigMigrateCommand::Apply(arguments) => {
                    run_config_migrate_apply(&cli.global, managed_paths, current_dir, arguments)
                }
            },
        },
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
        Command::Integration(arguments) => match arguments.command {
            IntegrationCommand::Install(arguments) => {
                run_integration_install(&cli.global, managed_paths, current_dir, arguments)
            }
            IntegrationCommand::Activate(arguments) => {
                run_integration_activate(&cli.global, managed_paths, current_dir, arguments)
            }
            IntegrationCommand::AuthorizeWorkspace(arguments) => {
                run_integration_authorize_workspace(&cli.global, managed_paths, arguments)
            }
            IntegrationCommand::RevokeWorkspace(arguments) => {
                run_integration_revoke_workspace(&cli.global, managed_paths, arguments)
            }
            IntegrationCommand::Status(arguments) => {
                run_integration_status(&cli.global, managed_paths, arguments)
            }
            IntegrationCommand::Repair(arguments) => {
                run_integration_repair(&cli.global, managed_paths, arguments)
            }
            IntegrationCommand::Uninstall(arguments) => {
                run_integration_uninstall(&cli.global, managed_paths, arguments)
            }
        },
        Command::Model(arguments) => match arguments.command {
            ModelCommand::Install(arguments) => {
                run_model_install(managed_paths, current_dir, arguments).await
            }
            ModelCommand::Import(arguments) => {
                run_model_import(managed_paths, current_dir, arguments)
            }
            ModelCommand::List(arguments) => {
                run_model_list(&cli.global, managed_paths, current_dir, arguments)
            }
            ModelCommand::Verify(arguments) => run_model_verify(managed_paths, arguments),
            ModelCommand::Remove(arguments) => run_model_remove(managed_paths, arguments),
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

async fn run_model_install(
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ModelInstallArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let ModelInstallKind::Embedding = arguments.kind;
    let ca_bundle = arguments
        .ca_bundle
        .as_deref()
        .map(|path| absolute_request_path(&current_dir, path));
    let store = ModelStore::new(managed_paths.model_cache_dir());
    let outcome = store
        .install(&arguments.id, ca_bundle.as_deref())
        .await
        .map_err(map_model_error)?;
    render_model_mutation("install", &outcome, arguments.json)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_model_import(
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ModelImportArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let artifact = absolute_request_path(&current_dir, &arguments.artifact);
    let store = ModelStore::new(managed_paths.model_cache_dir());
    let outcome = store.import(&artifact).map_err(map_model_error)?;
    render_model_mutation("import", &outcome, arguments.json)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_model_list(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ModelListArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let selected_index = direct_context(global, managed_paths.clone(), current_dir)
        .ok()
        .map(|context| context.index_name.to_string());
    let store = ModelStore::new(managed_paths.model_cache_dir());
    let inventory = store.inventory();
    let mut output = Vec::with_capacity(inventory.len());
    for (manifest, item) in store.manifests().iter().zip(inventory) {
        let pinned_by =
            discover_model_pins(managed_paths.data_dir(), manifest).map_err(map_model_error)?;
        let pinned_by_selected_index = selected_index
            .as_ref()
            .is_some_and(|selected| pinned_by.iter().any(|index| index == selected));
        output.push(json!({
            "id": item.id,
            "kind": item.kind,
            "state": item.state,
            "dimension": item.dimension,
            "license_id": item.license_id,
            "upstream_revision": item.upstream_revision,
            "source_url": manifest.source_url,
            "manifest_sha256": item.fingerprint.to_string(),
            "expected_bytes": item.expected_bytes,
            "cache_path": json_path(&item.path),
            "pinned_by_selected_index": pinned_by_selected_index,
            "pinned_by_indexes": pinned_by,
        }));
    }

    if arguments.json {
        write_json_stdout(&json!({
            "schema_version": "hsum.model-list.v1",
            "request_id": Uuid::new_v4().to_string(),
            "selected_index": selected_index,
            "models": output,
        }))?;
    } else {
        let mut human = String::new();
        for (ordinal, model) in output.iter().enumerate() {
            if ordinal > 0 {
                human.push('\n');
            }
            push_line(
                &mut human,
                "Model",
                model["id"].as_str().expect("model IDs are strings"),
            );
            push_line(
                &mut human,
                "State",
                model["state"].as_str().expect("model state is a string"),
            );
            push_line(
                &mut human,
                "Dimension",
                &model["dimension"].as_u64().unwrap_or_default().to_string(),
            );
            push_line(
                &mut human,
                "Expected bytes",
                &model["expected_bytes"]
                    .as_u64()
                    .unwrap_or_default()
                    .to_string(),
            );
            push_line(
                &mut human,
                "Manifest SHA-256",
                model["manifest_sha256"]
                    .as_str()
                    .expect("manifest hash is a string"),
            );
            push_line(
                &mut human,
                "Upstream revision",
                model["upstream_revision"]
                    .as_str()
                    .expect("revision is a string"),
            );
            push_line(
                &mut human,
                "License",
                model["license_id"].as_str().expect("license is a string"),
            );
            push_line(
                &mut human,
                "Cache",
                model["cache_path"]
                    .as_str()
                    .expect("cache path is a string"),
            );
            push_line(
                &mut human,
                "Pinned by selected index",
                bool_text(model["pinned_by_selected_index"].as_bool().unwrap_or(false)),
            );
            let pins = model["pinned_by_indexes"]
                .as_array()
                .expect("pin list is an array")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            push_line(
                &mut human,
                "Pinned by indexes",
                &if pins.is_empty() {
                    "none".to_owned()
                } else {
                    pins.join(", ")
                },
            );
        }
        write_stdout(human.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_model_verify(
    managed_paths: ManagedPaths,
    arguments: ModelVerifyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let store = ModelStore::new(managed_paths.model_cache_dir());
    let verification = store.verify(&arguments.id).map_err(map_model_error)?;
    if arguments.json {
        write_json_stdout(&json!({
            "schema_version": "hsum.model-verify.v1",
            "request_id": Uuid::new_v4().to_string(),
            "id": verification.id,
            "verified": true,
            "files": verification.files,
            "bytes": verification.bytes,
            "manifest_sha256": verification.fingerprint.to_string(),
            "cache_path": json_path(&verification.path),
        }))?;
    } else {
        let mut output = String::new();
        output.push_str("Model verification passed.\n");
        push_line(&mut output, "Model", &verification.id);
        push_line(&mut output, "Files", &verification.files.to_string());
        push_line(&mut output, "Bytes", &verification.bytes.to_string());
        push_line(
            &mut output,
            "Manifest SHA-256",
            &verification.fingerprint.to_string(),
        );
        push_line(&mut output, "Cache", &human_path(&verification.path));
        write_stdout(output.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_model_remove(
    managed_paths: ManagedPaths,
    arguments: ModelRemoveArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let store = ModelStore::new(managed_paths.model_cache_dir());
    let manifest = store.manifest(&arguments.id).map_err(map_model_error)?;
    let pins = discover_model_pins(managed_paths.data_dir(), manifest).map_err(map_model_error)?;
    let removal = store
        .remove(&arguments.id, &pins, arguments.force)
        .map_err(map_model_error)?;
    if arguments.json {
        write_json_stdout(&json!({
            "schema_version": "hsum.model-remove.v1",
            "request_id": Uuid::new_v4().to_string(),
            "id": removal.id,
            "removed": true,
            "forced": removal.forced,
            "affected_indexes": removal.affected_indexes,
            "cache_path": json_path(&removal.path),
        }))?;
    } else {
        let mut output = String::new();
        output.push_str("Model removed.\n");
        push_line(&mut output, "Model", &removal.id);
        push_line(&mut output, "Cache", &human_path(&removal.path));
        push_line(&mut output, "Forced", bool_text(removal.forced));
        if !removal.affected_indexes.is_empty() {
            push_line(
                &mut output,
                "Indexes left degraded",
                &removal.affected_indexes.join(", "),
            );
        }
        write_stdout(output.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn render_model_mutation(
    operation: &'static str,
    outcome: &ModelMutation,
    json: bool,
) -> Result<(), RuntimeFailure> {
    let verification = outcome.verification();
    if json {
        return write_json_stdout(&json!({
            "schema_version": "hsum.model-mutation.v1",
            "request_id": Uuid::new_v4().to_string(),
            "operation": operation,
            "id": verification.id,
            "changed": outcome.changed(),
            "verified": true,
            "files": verification.files,
            "bytes": verification.bytes,
            "manifest_sha256": verification.fingerprint.to_string(),
            "cache_path": json_path(&verification.path),
        }));
    }

    let mut output = String::new();
    if outcome.changed() {
        output.push_str("Model installed and verified.\n");
    } else {
        output.push_str("Exact model is already installed and verified.\n");
    }
    push_line(&mut output, "Model", &verification.id);
    push_line(&mut output, "Files", &verification.files.to_string());
    push_line(&mut output, "Bytes", &verification.bytes.to_string());
    push_line(
        &mut output,
        "Manifest SHA-256",
        &verification.fingerprint.to_string(),
    );
    push_line(&mut output, "Cache", &human_path(&verification.path));
    write_stdout(output.as_bytes())
}

fn map_model_error(error: ModelError) -> RuntimeFailure {
    let subcode = match &error {
        ModelError::UnknownModel(_) => ErrorSubcode::ConfigInvalid,
        ModelError::NotInstalled { .. } | ModelError::Offline { .. } => {
            ErrorSubcode::ModelNotInstalled
        }
        ModelError::Pinned { .. } | ModelError::PinDiscovery(_) => ErrorSubcode::ModelPinned,
        ModelError::Dimension { .. } => ErrorSubcode::ModelDimension,
        ModelError::Checksum { .. } => ErrorSubcode::ChecksumMismatch,
        ModelError::NetworkTransient { .. } => ErrorSubcode::NetworkTransient,
        ModelError::NetworkPermanent { .. } | ModelError::NetworkConfiguration(_) => {
            ErrorSubcode::NetworkPermanent
        }
        ModelError::UnsafeArtifact(_)
        | ModelError::UnsupportedImport(_)
        | ModelError::ReceiptTooLarge => ErrorSubcode::PathInvalid,
        ModelError::Io(_) => ErrorSubcode::StorageIo,
        ModelError::ReceiptMissing(_)
        | ModelError::ManifestMismatch { .. }
        | ModelError::FileListMismatch { .. }
        | ModelError::FileSize { .. }
        | ModelError::DestinationOccupied(_)
        | ModelError::TooManyFiles
        | ModelError::IntegerOverflow
        | ModelError::Manifest(_)
        | ModelError::ReceiptJson(_) => ErrorSubcode::ModelFingerprint,
    };
    RuntimeFailure::from_error(subcode, "model", error)
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
    let mut output = subcode.render_offline_help();
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
                json!({
                    "operation": operation,
                    "reason": error.to_string(),
                }),
            )),
        }
    }

    fn cursor_error(subcode: ErrorSubcode, reason: &'static str) -> Self {
        Self {
            public: Box::new(PublicError::with_details(
                subcode,
                Uuid::new_v4().to_string(),
                json!({
                    "operation": "search",
                    "argument": "cursor",
                    "reason": reason,
                }),
            )),
        }
    }

    fn stale_cursor(subcode: ErrorSubcode) -> Self {
        Self::cursor_error(subcode, "cursor binding changed")
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
    request.rebuild = arguments.rebuild;
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
        if let Some(rebuild) = &outcome.rebuild {
            output.push_str(
                "Would replace the trusted index and invalidate every citation recorded in it.\n",
            );
            push_line(
                &mut output,
                "Previous binding",
                &rebuild.previous_binding_id.to_string(),
            );
            push_line(
                &mut output,
                "Previous index ID",
                &rebuild.previous_index_id.to_string(),
            );
            push_line(
                &mut output,
                "Previous active documents",
                &rebuild.active_documents.to_string(),
            );
            push_line(
                &mut output,
                "Previous active passages",
                &rebuild.active_passages.to_string(),
            );
        }
    } else if outcome.rebuild.is_some() {
        output.push_str(
            "hSUM index rebuilt. Prior evidence and citations no longer resolve in this index.\n",
        );
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
            output.push_str("Connect Codex:\n  ");
            output.push_str(&shell_quote_path(&executable));
            output.push_str(" integration install codex --confirm\n");
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
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let confirmations = DeleteConfirmations {
        allow_empty_snapshot: arguments.allow_empty_snapshot,
        allow_mass_delete: arguments.allow_mass_delete,
    };

    if arguments.dry_run {
        let plan = plan_project_sources_with_timeout(
            &context,
            &arguments.source,
            arguments.strict,
            Duration::from_millis(arguments.lock_timeout_ms),
        )
        .map_err(map_project_ingest_error)?;
        render_ingest_plan(&context, &plan, confirmations)?;
        return Ok(crate::cli::ProcessExitCategory::Success);
    }

    let outcome = ingest_project_sources_with_timeout(
        &context,
        &arguments.source,
        arguments.strict,
        confirmations,
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_project_ingest_error)?;

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

fn run_source_add(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SourceAddArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    match arguments.connector {
        SourceConnectorCommand::Fs(arguments) => {
            let request = AddFilesystemSourceRequest {
                database_path: context.database_path.clone(),
                project_id: context.project_id,
                path: arguments.path,
                source_name: arguments.name.clone(),
                current_dir,
                environment_home: env::var_os("HOME").map(PathBuf::from),
                allow_broad_root: arguments.allow_broad_root,
                index_quota_bytes: context.index_quota_bytes,
                lock_timeout: DEFAULT_WRITER_LOCK_TIMEOUT,
            };
            let (registration, config) =
                add_filesystem_source(&request).map_err(map_source_management_error)?;
            let mut output = String::new();
            output.push_str(if registration.created {
                "Filesystem source configured.\n"
            } else if registration.reactivated {
                "Filesystem source reactivated.\n"
            } else {
                "Filesystem source already configured.\n"
            });
            push_line(&mut output, "Source", arguments.name.as_str());
            push_line(
                &mut output,
                "Source ID",
                &registration.source_id.to_string(),
            );
            push_line(&mut output, "Root", &human_path(config.root()));
            push_line(
                &mut output,
                "Attached",
                if registration.attached { "yes" } else { "no" },
            );
            if registration.attached {
                output.push_str("Run `hsum ingest --source ");
                output.push_str(&registration.source_id.to_string());
                output.push_str("` to refresh this source.\n");
            } else {
                output.push_str("Activate: hsum project set-root ");
                output.push_str(&shell_quote_path(config.root()));
                output.push_str(" --source-name ");
                output.push_str(arguments.name.as_str());
                output.push_str(" --confirm");
                if arguments.allow_broad_root {
                    output.push_str(" --allow-broad-root");
                }
                output.push('\n');
            }
            write_stdout(output.as_bytes())?;
            Ok(crate::cli::ProcessExitCategory::Success)
        }
        SourceConnectorCommand::Jsonl(arguments) => run_source_add_jsonl(&context, arguments),
    }
}

fn run_source_add_jsonl(
    context: &EffectiveContext,
    arguments: crate::cli::JsonlSourceAddArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let request = AddJsonlSourceRequest {
        database_path: context.database_path.clone(),
        project_id: context.project_id,
        project_name: context.project_name.clone(),
        path: arguments.path,
        source_name: arguments.name.clone(),
        index_quota_bytes: context.index_quota_bytes,
        lock_timeout: DEFAULT_WRITER_LOCK_TIMEOUT,
    };
    let (registration, config) = add_jsonl_source(&request).map_err(map_source_management_error)?;
    let mut output = String::new();
    output.push_str(if registration.created {
        "JSONL source configured.\n"
    } else if registration.attached {
        "Existing JSONL source attached to the selected project.\n"
    } else {
        "JSONL source already configured.\n"
    });
    push_line(&mut output, "Source", arguments.name.as_str());
    push_line(
        &mut output,
        "Source ID",
        &registration.source_id.to_string(),
    );
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(&mut output, "Snapshot", &human_path(config.path()));
    output.push_str("Run `hsum ingest --source ");
    output.push_str(&registration.source_id.to_string());
    output.push_str("` to ingest this snapshot.\n");
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_source_list(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SourceListArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let _reader_lease = ReaderLease::acquire(&context.database_path, DEFAULT_WRITER_LOCK_TIMEOUT)
        .map_err(map_store_error)?;
    let sources = list_sources_in_scope(&context.database_path, context.project_id, arguments.all)
        .map_err(map_source_management_error)?;
    if arguments.json {
        let sources = sources
            .iter()
            .map(|source| {
                json!({
                    "source_id": source.source_id,
                    "kind": source.kind.as_str(),
                    "name": source.name.as_str(),
                    "logical_uri": source.logical_uri,
                    "attached": source.attached,
                    "active_documents": source.active_documents,
                    "last_success_at": source.last_success_at,
                    "last_error": source.last_error_code.as_ref().map(|code| json!({
                        "code": code,
                        "detail": source.last_error_detail,
                        "observed_at": source.last_error_at,
                    })),
                })
            })
            .collect::<Vec<_>>();
        write_json_stdout(&json!({
            "schema_version": MCP_API_VERSION,
            "project": context.project_name.as_str(),
            "project_id": context.project_id,
            "sources": sources,
        }))?;
    } else {
        let mut output = String::new();
        push_line(&mut output, "Project", context.project_name.as_str());
        for source in &sources {
            output.push_str("Source ");
            output.push_str(&source.source_id.to_string());
            output.push_str("  ");
            output.push_str(source.kind.as_str());
            output.push_str("  ");
            output.push_str(&escape_terminal_text(source.name.as_str()));
            output.push_str("  ");
            output.push_str(&escape_terminal_text(&source.logical_uri));
            if !source.attached {
                output.push_str("  unattached");
            }
            if let Some(code) = &source.last_error_code {
                output.push_str("  error=");
                output.push_str(&escape_terminal_text(code));
            }
            output.push('\n');
        }
        write_stdout(output.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_source_attach(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SourceAttachArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(
        arguments.confirm,
        "clap requires source attach confirmation"
    );
    let context = direct_context(global, managed_paths, current_dir)?;
    let outcome = attach_jsonl_source(
        &context.database_path,
        context.project_id,
        &arguments.source,
        context.index_quota_bytes,
        DEFAULT_WRITER_LOCK_TIMEOUT,
    )
    .map_err(map_source_management_error)?;
    let mut output = String::new();
    output.push_str(if outcome.changed {
        "JSONL source attached to the selected project.\n"
    } else {
        "JSONL source already attached to the selected project.\n"
    });
    push_line(&mut output, "Source", outcome.source_name.as_str());
    push_line(&mut output, "Source ID", &outcome.source_id.to_string());
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(
        &mut output,
        "Scope revision",
        &outcome.scope_revision.to_string(),
    );
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_source_detach(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SourceDetachArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(
        arguments.confirm,
        "clap requires source detach confirmation"
    );
    let context = direct_context(global, managed_paths, current_dir)?;
    let outcome = detach_jsonl_source(
        &context.database_path,
        context.project_id,
        &arguments.source,
        context.index_quota_bytes,
        DEFAULT_WRITER_LOCK_TIMEOUT,
    )
    .map_err(map_source_management_error)?;
    let mut output = String::new();
    output.push_str(if outcome.changed {
        "JSONL source detached from the selected project.\n"
    } else {
        "JSONL source already detached from the selected project.\n"
    });
    push_line(&mut output, "Source", outcome.source_name.as_str());
    push_line(&mut output, "Source ID", &outcome.source_id.to_string());
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(
        &mut output,
        "Scope revision",
        &outcome.scope_revision.to_string(),
    );
    output.push_str("Stored source heads and immutable versions were retained.\n");
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_source_remove(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SourceRemoveArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(
        arguments.confirm,
        "clap requires source removal confirmation"
    );
    let context = direct_context(global, managed_paths, current_dir)?;
    let outcome = remove_jsonl_source(
        &context.database_path,
        context.project_id,
        &arguments.source,
        context.index_quota_bytes,
        DEFAULT_WRITER_LOCK_TIMEOUT,
    )
    .map_err(map_source_management_error)?;
    let mut output = String::from("JSONL source removed from active project scopes.\n");
    push_line(&mut output, "Source", outcome.source_name.as_str());
    push_line(&mut output, "Source ID", &outcome.source_id.to_string());
    push_line(
        &mut output,
        "Generation",
        &outcome
            .generation_id
            .map_or_else(|| "unchanged".to_owned(), |value| value.to_string()),
    );
    push_line(
        &mut output,
        "Tombstoned documents",
        &outcome.tombstoned_documents.to_string(),
    );
    push_line(
        &mut output,
        "Detached projects",
        &outcome.detached_projects.to_string(),
    );
    output.push_str("Immutable versions remain available until explicit prune.\n");
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_project_create(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ProjectCreateArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let registration = create_project(&context, &arguments.name, DEFAULT_WRITER_LOCK_TIMEOUT)
        .map_err(map_project_management_error)?;
    let mut output = String::new();
    output.push_str(if registration.created {
        "Project created with the selected filesystem authority.\n"
    } else {
        "Project already exists.\n"
    });
    push_line(&mut output, "Project", arguments.name.as_str());
    push_line(
        &mut output,
        "Project ID",
        &registration.project_id.to_string(),
    );
    push_line(
        &mut output,
        "Filesystem source",
        &context.source_id.to_string(),
    );
    output.push_str("Run `hsum project use ");
    output.push_str(arguments.name.as_str());
    output.push_str(" --confirm` to select it persistently.\n");
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_project_list(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ProjectListArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let projects = list_projects(&context.database_path).map_err(map_project_management_error)?;
    if arguments.json {
        let projects = projects
            .iter()
            .map(|project| {
                json!({
                    "project_id": project.project_id,
                    "name": project.name.as_str(),
                    "selected": project.project_id == context.project_id,
                    "scope_revision": project.scope_revision,
                    "active_sources": project.active_sources,
                    "filesystem_source_id": project.filesystem_source_id,
                    "filesystem_source_name": project.filesystem_source_name.as_str(),
                    "filesystem_root": project.filesystem_root,
                })
            })
            .collect::<Vec<_>>();
        write_json_stdout(&json!({
            "schema_version": MCP_API_VERSION,
            "index": context.index_name.as_str(),
            "index_id": context.index_id,
            "selected_project_id": context.project_id,
            "projects": projects,
        }))?;
    } else {
        let mut output = String::new();
        push_line(&mut output, "Index", context.index_name.as_str());
        for project in &projects {
            output.push_str(if project.project_id == context.project_id {
                "* "
            } else {
                "  "
            });
            output.push_str(project.name.as_str());
            output.push_str("  ");
            output.push_str(&project.project_id.to_string());
            output.push_str("  sources=");
            output.push_str(&project.active_sources.to_string());
            output.push_str("  root=");
            output.push_str(&escape_terminal_text(&project.filesystem_root));
            output.push('\n');
        }
        write_stdout(output.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_project_use(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ProjectUseArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(arguments.confirm, "clap requires project use confirmation");
    let context = direct_context(global, managed_paths, current_dir)?;
    let outcome =
        use_project(&context, &arguments.project).map_err(map_project_management_error)?;
    let mut output = String::new();
    output.push_str(if outcome.changed {
        "Persistent project selection updated.\n"
    } else {
        "Project is already selected.\n"
    });
    push_line(&mut output, "Project", outcome.project.name.as_str());
    push_line(
        &mut output,
        "Project ID",
        &outcome.project.project_id.to_string(),
    );
    push_line(
        &mut output,
        "Root",
        &escape_terminal_text(&outcome.project.filesystem_root),
    );
    if Path::new(&outcome.project.filesystem_root) != context.source_root {
        output.push_str(
            "Run subsequent root-selected commands from the project's filesystem root.\n",
        );
    }
    write_stdout(output.as_bytes())?;
    if outcome.durability == Some(crate::config::AtomicSaveOutcome::DurabilityUnknown) {
        write_stderr(
            b"warning: the project selection was written, but parent-directory durability could \
              not be confirmed on this filesystem\n",
        )?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_project_set_root(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ProjectSetRootArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(
        arguments.confirm,
        "clap requires root replacement confirmation"
    );
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let outcome = set_project_root(&SetProjectRootRequest {
        context: &context,
        path: arguments.path,
        source_name: arguments.source_name,
        current_dir,
        environment_home: env::var_os("HOME").map(PathBuf::from),
        allow_broad_root: arguments.allow_broad_root,
        lock_timeout: DEFAULT_WRITER_LOCK_TIMEOUT,
    })
    .map_err(map_project_management_error)?;
    let mut output = String::new();
    output.push_str(if outcome.changed {
        "Project filesystem authority replaced.\n"
    } else {
        "Project filesystem authority already matches the requested root.\n"
    });
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(
        &mut output,
        "Root",
        &escape_terminal_text(&outcome.new_root),
    );
    push_line(&mut output, "Source", outcome.new_source_name.as_str());
    push_line(&mut output, "Source ID", &outcome.new_source_id.to_string());
    push_line(
        &mut output,
        "Scope revision",
        &outcome.scope_revision.to_string(),
    );
    if outcome.changed {
        output.push_str(
            "The old source membership and immutable versions were retained for history.\n",
        );
        output.push_str("Run `hsum ingest --strict` from the new root to index it.\n");
    }
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_index_delete(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IndexDeleteArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    debug_assert!(
        arguments.confirm,
        "clap requires index deletion confirmation"
    );
    let config_file = global
        .config
        .clone()
        .unwrap_or_else(|| managed_paths.config_file());
    let outcome = delete_index(&DeleteIndexRequest {
        managed_paths,
        config_file,
        config_file_explicit: global.config.is_some(),
        index_name: arguments.name,
        lock_timeout: Duration::from_millis(arguments.lock_timeout_ms),
    })
    .map_err(map_index_management_error)?;
    let mut output = String::from("Managed index deleted.\n");
    push_line(&mut output, "Index", outcome.index_name.as_str());
    push_line(&mut output, "Index ID", &outcome.index_id.to_string());
    push_line(
        &mut output,
        "Trust bindings removed",
        &outcome.removed_bindings.to_string(),
    );
    push_line(
        &mut output,
        "Configured default cleared",
        bool_text(outcome.cleared_configured_default),
    );
    push_line(
        &mut output,
        "Interrupted deletion resumed",
        bool_text(outcome.resumed_quarantine),
    );
    output.push_str(
        "Repository .hsum.toml pointers were retained as unauthoritative hints and can be removed manually.\n",
    );
    write_stdout(output.as_bytes())?;
    if outcome.durability_unknown {
        write_stderr(
            b"warning: index deletion is visible, but parent-directory durability could not be confirmed\n",
        )?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_search(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: SearchArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let _reader_lease = ReaderLease::acquire(&context.database_path, DEFAULT_WRITER_LOCK_TIMEOUT)
        .map_err(map_store_error)?;
    let mode = match arguments.mode {
        CliSearchMode::Auto => SearchMode::Auto,
        CliSearchMode::Lexical => SearchMode::Lexical,
    };
    let decoded_cursor = decode_search_cursor(
        arguments.cursor.as_deref(),
        context.project_id,
        &arguments.query,
        mode,
        arguments.explain,
    )
    .map_err(map_search_cursor_error)?;
    let config_fingerprint = retrieval_config_fingerprint();
    if decoded_cursor
        .state
        .as_ref()
        .is_some_and(|expected| expected.config_fingerprint.as_str() != config_fingerprint.as_str())
    {
        return Err(RuntimeFailure::stale_cursor(ErrorSubcode::QueryFingerprint));
    }
    let expected_snapshot = decoded_cursor
        .state
        .as_ref()
        .map(search_snapshot_from_cursor);
    let request = SearchRequest::new(
        &arguments.query,
        mode,
        MAX_SEARCH_LIMIT,
        arguments.timeout_ms,
        arguments.explain,
    )
    .map_err(map_search_error)?;
    let outcome = SearchEvidence::execute(&SearchEvidenceRequest {
        index_path: context.database_path.clone(),
        project_id: context.project_id,
        search: request,
        expected_snapshot,
        page: SearchEvidencePage {
            offset: decoded_cursor.offset,
            limit: usize::from(arguments.limit),
            body_bytes: None,
        },
        field_limits: SearchEvidenceFieldLimits::CLI,
        probe_budget: SOURCE_PROBE_TIMEOUT,
        operation_deadline: None,
        deadline_stop_is_error: decoded_cursor.state.is_some(),
        connection_observer: None,
        cancelled: None,
    })
    .map_err(map_search_evidence_error)?;
    let cursor_state = SearchCursorState {
        index_id: outcome.snapshot.index_id,
        scope_revision: outcome.snapshot.scope_revision,
        index_epoch: outcome.snapshot.index_epoch,
        generation: outcome.snapshot.generation,
        config_fingerprint,
    };
    if decoded_cursor
        .state
        .as_ref()
        .is_some_and(|expected| expected != &cursor_state)
    {
        return Err(RuntimeFailure::stale_cursor(cursor_stale_subcode(
            search_cursor_stale_cause(
                decoded_cursor.state.as_ref().expect("checked above"),
                &cursor_state,
            ),
        )));
    }
    let offset = decoded_cursor.offset;
    let consumed = offset.saturating_add(outcome.response.results.len());
    let next_cursor = if outcome.response.stop_reason != SearchStopReason::Deadline
        && consumed < outcome.total_fetched
        && consumed < MAX_SEARCH_LIMIT
    {
        Some(
            encode_search_cursor(
                consumed,
                context.project_id,
                &arguments.query,
                mode,
                arguments.explain,
                &cursor_state,
            )
            .map_err(map_search_cursor_error)?,
        )
    } else {
        None
    };
    let response = outcome.response;
    let drift = outcome.drift;

    if arguments.json {
        let output = CliSearchOutput::from_response(
            Uuid::new_v4().to_string(),
            context.project_name.as_str().to_owned(),
            response,
            drift.as_slice(),
            arguments.explain,
            next_cursor,
        );
        write_json_stdout(&output)?;
    } else {
        render_search_human(
            &response,
            Some(drift.as_slice()),
            arguments.explain,
            offset,
            next_cursor.as_deref(),
        )?;
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
    let _reader_lease = ReaderLease::acquire(&context.database_path, DEFAULT_WRITER_LOCK_TIMEOUT)
        .map_err(map_store_error)?;
    let request = GetRequest {
        project_id: context.project_id,
        citation: arguments.citation_uri,
        max_bytes: usize::try_from(arguments.max_bytes)
            .expect("u32 fits usize on supported targets"),
    };
    let outcome = GetEvidence::execute(&GetEvidenceRequest {
        index_path: context.database_path,
        request,
        verify_source_hash: arguments.verify_source_hash,
        probe_deadline: Instant::now()
            .checked_add(SOURCE_PROBE_TIMEOUT)
            .unwrap_or_else(Instant::now),
        field_limits: GetEvidenceFieldLimits::CLI,
        connection_observer: None,
    })
    .map_err(map_get_evidence_error)?;
    let output = EvidenceGetOutput::from_outcome(Uuid::new_v4().to_string(), outcome)
        .map_err(map_get_packet_error)?;
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
    let _reader_lease = ReaderLease::acquire(&context.database_path, DEFAULT_WRITER_LOCK_TIMEOUT)
        .map_err(map_store_error)?;
    let outcome = StatusEvidence::execute(&StatusEvidenceRequest {
        index_path: context.database_path.clone(),
        project_id: context.project_id,
        field_limits: StatusEvidenceFieldLimits::CLI,
        operation_deadline: None,
        connection_observer: None,
        cancelled: None,
    })
    .map_err(map_status_evidence_error)?;
    if arguments.json {
        let output = CliStatusOutput::from_outcome(
            Uuid::new_v4().to_string(),
            context.index_name.as_str().to_owned(),
            context.project_name.as_str().to_owned(),
            arguments.problems,
            outcome,
        );
        write_json_stdout(&output)?;
    } else {
        render_status_human(&context, &outcome.report, arguments.problems)?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_doctor(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: DoctorArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir)?;
    let report = if arguments.repair {
        let outcome = Doctor::repair_abandoned(
            &context.database_path,
            Duration::from_millis(arguments.lock_timeout_ms),
        )
        .map_err(map_store_error)?;
        let mut text = String::new();
        text.push_str("Doctor repair completed after full pre/post validation.\n");
        push_line(
            &mut text,
            "Abandoned generations removed",
            &outcome.removed_abandoned_generations.to_string(),
        );
        write_stdout(text.as_bytes())?;
        outcome.report
    } else {
        Doctor::run(&context.database_path).map_err(map_store_error)?
    };
    let _explicit_integrity = arguments.integrity;
    render_doctor_human(&context, &report)?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_doctor_report(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: DoctorReportArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let output = absolute_request_path(&current_dir, &arguments.output);
    let report = Doctor::support_report(&context.database_path).map_err(map_store_error)?;
    write_private_json(&output, &report).map_err(map_maintenance_error)?;

    let mut text = String::new();
    text.push_str("Body-free, query-free Doctor report written.\n");
    push_line(&mut text, "Report", &human_path(&output));
    push_line(
        &mut text,
        "Included fields",
        &DoctorSupportReport::INCLUDED_FIELDS.join(", "),
    );
    text.push_str(
        "Excluded: document bodies, passages, queries, source URIs, connector keys, and paths.\n",
    );
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_backup_create(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: BackupCreateArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let registry_path = managed_paths.managed_backup_registry_file();
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let lock_timeout = Duration::from_millis(arguments.lock_timeout_ms);
    let mut catalog = ManagedBackupCatalog::open(&registry_path, lock_timeout)
        .map_err(map_managed_backup_error)?;
    let requested_output = absolute_request_path(&current_dir, &arguments.output);
    let output = catalog
        .normalize_output(&requested_output)
        .map_err(map_managed_backup_error)?;
    let existing_reservation = catalog
        .reservation(
            context.index_id,
            &context.index_name,
            ManagedBackupKind::Manual,
            &output,
        )
        .map_err(map_managed_backup_error)?;
    if existing_reservation.is_none()
        && output.try_exists().map_err(|error| {
            RuntimeFailure::from_error(
                io_subcode(&error, ErrorSubcode::IndexWrite),
                "managed_backup",
                error,
            )
        })?
    {
        return Err(map_maintenance_error(MaintenanceError::OutputExists(
            output,
        )));
    }
    let reservation = catalog
        .reserve(
            context.index_id,
            &context.index_name,
            ManagedBackupKind::Manual,
            &output,
        )
        .map_err(map_managed_backup_error)?;
    let receipt = if reservation == BackupReservation::Pending
        && output.try_exists().map_err(|error| {
            RuntimeFailure::from_error(
                io_subcode(&error, ErrorSubcode::IndexWrite),
                "managed_backup",
                error,
            )
        })? {
        inspect_backup(&output, context.index_id).map_err(map_maintenance_error)?
    } else {
        match create_backup(&context.database_path, &output, lock_timeout) {
            Ok(receipt) => receipt,
            Err(error) => {
                catalog
                    .cancel_if_missing(context.index_id, &output)
                    .map_err(map_managed_backup_error)?;
                return Err(map_maintenance_error(error));
            }
        }
    };
    catalog
        .complete(context.index_id, &output, &receipt)
        .map_err(map_managed_backup_error)?;
    let mut text = String::new();
    text.push_str("Verified backup created.\n");
    push_line(&mut text, "Index ID", &receipt.index_id.to_string());
    push_line(
        &mut text,
        "Schema version",
        &receipt.schema_version.to_string(),
    );
    push_line(&mut text, "Index epoch", &receipt.index_epoch.to_string());
    push_line(&mut text, "Backup", &human_path(&receipt.output));
    push_line(&mut text, "Bytes", &receipt.file_bytes.to_string());
    push_line(&mut text, "SHA-256", &receipt.file_sha256.to_string());
    text.push_str("The backup is recorded in hSUM's managed inventory.\n");
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_backup_list(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: BackupListArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let selected = if arguments.all {
        None
    } else {
        let context = direct_context(global, managed_paths.clone(), current_dir)?;
        Some((context.index_id, context.index_name))
    };
    let registry_path = managed_paths.managed_backup_registry_file();
    let inventory = if managed_paths.data_dir().try_exists().map_err(|error| {
        RuntimeFailure::from_error(
            io_subcode(&error, ErrorSubcode::IndexWrite),
            "managed_backup",
            error,
        )
    })? {
        ManagedBackupCatalog::open(
            &registry_path,
            Duration::from_millis(arguments.lock_timeout_ms),
        )
        .map_err(map_managed_backup_error)?
        .inventory(selected.as_ref().map(|(index_id, _)| *index_id))
        .map_err(map_managed_backup_error)?
    } else {
        Vec::new()
    };
    if arguments.json {
        let value = json!({
            "format": "hsum.managed-backup-inventory.v1",
            "scope": selected.as_ref().map_or("all", |_| "selected-index"),
            "index_id": selected.as_ref().map(|(index_id, _)| index_id.to_string()),
            "index_name": selected.as_ref().map(|(_, index_name)| index_name.as_str()),
            "backups": inventory.iter().map(managed_backup_json).collect::<Vec<_>>(),
        });
        write_json_stdout(&value)?;
    } else {
        let mut text = String::from("Managed backup inventory.\n");
        push_line(
            &mut text,
            "Scope",
            if arguments.all {
                "all indexes"
            } else {
                "selected index"
            },
        );
        push_line(&mut text, "Backups", &inventory.len().to_string());
        push_line(
            &mut text,
            "May contain evidence",
            &inventory
                .iter()
                .filter(|item| item.state.may_contain_evidence())
                .count()
                .to_string(),
        );
        append_managed_backup_inventory(&mut text, &inventory);
        write_stdout(text.as_bytes())?;
    }
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn managed_backup_json(item: &ManagedBackupInventoryItem) -> Value {
    json!({
        "index_id": item.index_id.to_string(),
        "index_name": item.index_name.as_str(),
        "kind": item.kind.as_str(),
        "path": json_path(&item.path),
        "state": item.state.as_str(),
        "may_contain_evidence": item.state.may_contain_evidence(),
        "file_bytes": item.file_bytes,
        "file_sha256": item.file_sha256.map(|digest| digest.to_string()),
    })
}

fn append_managed_backup_inventory(text: &mut String, inventory: &[ManagedBackupInventoryItem]) {
    for (offset, item) in inventory.iter().enumerate() {
        text.push_str("Backup ");
        text.push_str(&(offset + 1).to_string());
        text.push_str(":\n");
        push_line(text, "  Index", item.index_name.as_str());
        push_line(text, "  Index ID", &item.index_id.to_string());
        push_line(text, "  Kind", item.kind.as_str());
        push_line(text, "  State", item.state.as_str());
        push_line(text, "  Path", &human_path(&item.path));
        if let Some(bytes) = item.file_bytes {
            push_line(text, "  Recorded bytes", &bytes.to_string());
        }
        if let Some(digest) = item.file_sha256 {
            push_line(text, "  Recorded SHA-256", &digest.to_string());
        }
    }
}

fn run_prune_plan(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: PrunePlanArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let output = absolute_request_path(&current_dir, &arguments.output);
    let before = OffsetDateTime::parse(&arguments.before, &Rfc3339).map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::ConfigInvalid, "prune_before", error)
    })?;
    let plan = plan_prune(
        &context.database_path,
        before,
        arguments.keep_latest,
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_maintenance_error)?;
    write_plan(&output, &plan).map_err(map_maintenance_error)?;
    let mut text = String::new();
    text.push_str("Prune plan written; no evidence was deleted.\n");
    push_line(&mut text, "Plan", &human_path(&output));
    push_line(&mut text, "Plan hash", &plan.plan_hash.to_string());
    push_line(
        &mut text,
        "Affected revisions",
        &plan.plan.affected_revisions.len().to_string(),
    );
    push_line(
        &mut text,
        "Affected citations",
        &plan.plan.affected_citation_count.to_string(),
    );
    push_line(
        &mut text,
        "Generations removed",
        &plan.plan.generations_removed.to_string(),
    );
    push_line(
        &mut text,
        "Logical reclaimable bytes",
        &plan.plan.logical_reclaimable_bytes.to_string(),
    );
    text.push_str("Review every affected revision and citation in the plan.\n");
    text.push_str("Apply: hsum prune apply ");
    text.push_str(&shell_quote_path(&output));
    text.push_str(" --backup <new-backup.sqlite> --confirm ");
    text.push_str(&plan.plan_hash.to_string());
    text.push('\n');
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_prune_apply(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: PruneApplyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let registry_path = managed_paths.managed_backup_registry_file();
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let plan_path = absolute_request_path(&current_dir, &arguments.plan);
    let plan: PlanEnvelope<PrunePlan> = read_plan(&plan_path).map_err(map_maintenance_error)?;
    validate_plan_envelope("hsum.prune-plan.v1", &plan, arguments.confirm)
        .map_err(map_maintenance_error)?;
    let lock_timeout = Duration::from_millis(arguments.lock_timeout_ms);
    let mut catalog = ManagedBackupCatalog::open(&registry_path, lock_timeout)
        .map_err(map_managed_backup_error)?;
    let requested_backup = absolute_request_path(&current_dir, &arguments.backup);
    let backup = catalog
        .normalize_output(&requested_backup)
        .map_err(map_managed_backup_error)?;
    catalog
        .reserve(
            context.index_id,
            &context.index_name,
            ManagedBackupKind::Prune,
            &backup,
        )
        .map_err(map_managed_backup_error)?;
    let outcome = match apply_prune(
        &context.database_path,
        &plan,
        arguments.confirm,
        &backup,
        lock_timeout,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            catalog
                .cancel_if_missing(context.index_id, &backup)
                .map_err(map_managed_backup_error)?;
            return Err(map_maintenance_error(error));
        }
    };
    catalog
        .complete(context.index_id, &backup, &outcome.backup)
        .map_err(map_managed_backup_error)?;
    let mut text = String::new();
    text.push_str("Prune applied and the resulting index passed Doctor.\n");
    push_line(&mut text, "Plan hash", &outcome.plan_hash.to_string());
    push_line(
        &mut text,
        "Affected revisions",
        &outcome.affected_revisions.to_string(),
    );
    push_line(
        &mut text,
        "Affected citations",
        &outcome.affected_citations.to_string(),
    );
    push_line(&mut text, "Backup", &human_path(&outcome.backup.output));
    text.push_str(
        "Rollback: stop hSUM readers, preserve the pruned index, replace it with the verified \
         backup above, then run `hsum doctor`.\n",
    );
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_forget_plan(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ForgetPlanArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let output = absolute_request_path(&current_dir, &arguments.output);
    let plan = plan_forget(
        &context.database_path,
        context.project_id,
        &arguments.citation,
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_maintenance_error)?;
    write_plan(&output, &plan).map_err(map_maintenance_error)?;
    let citation_count = plan
        .plan
        .affected_revisions
        .iter()
        .map(|revision| revision.canonical_stored_chunk_citations.len())
        .sum::<usize>();
    let body_bytes = plan
        .plan
        .affected_revisions
        .iter()
        .map(|revision| revision.original_body_bytes)
        .sum::<u64>();
    let mut text = String::new();
    text.push_str("Forget plan written; no evidence was changed.\n");
    push_line(&mut text, "Plan", &human_path(&output));
    push_line(&mut text, "Plan hash", &plan.plan_hash.to_string());
    push_line(
        &mut text,
        "Affected revisions",
        &plan.plan.affected_revisions.len().to_string(),
    );
    push_line(&mut text, "Affected citations", &citation_count.to_string());
    push_line(&mut text, "Evidence body bytes", &body_bytes.to_string());
    text.push_str("Review every revision namespace in the plan.\n");
    text.push_str("Apply: hsum forget apply ");
    text.push_str(&shell_quote_path(&output));
    text.push_str(
        " --recovery-backup <new-recovery.sqlite> --restore-plan <new-restore.json> \
         --keep-managed-backups --confirm ",
    );
    text.push_str(&plan.plan_hash.to_string());
    text.push('\n');
    text.push_str(
        "Choose --purge-managed-backups instead only after reviewing `hsum backup list`.\n",
    );
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_forget_apply(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ForgetApplyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let registry_path = managed_paths.managed_backup_registry_file();
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let plan_path = absolute_request_path(&current_dir, &arguments.plan);
    let restore_plan = absolute_request_path(&current_dir, &arguments.restore_plan);
    let plan: PlanEnvelope<ForgetPlan> = read_plan(&plan_path).map_err(map_maintenance_error)?;
    validate_plan_envelope("hsum.forget-plan.v1", &plan, arguments.confirm)
        .map_err(map_maintenance_error)?;
    let lock_timeout = Duration::from_millis(arguments.lock_timeout_ms);
    let mut catalog = ManagedBackupCatalog::open(&registry_path, lock_timeout)
        .map_err(map_managed_backup_error)?;
    let requested_recovery = absolute_request_path(&current_dir, &arguments.recovery_backup);
    let recovery_backup = catalog
        .normalize_output(&requested_recovery)
        .map_err(map_managed_backup_error)?;
    let prior_reservation = catalog
        .reservation(
            context.index_id,
            &context.index_name,
            ManagedBackupKind::ForgetRecovery,
            &recovery_backup,
        )
        .map_err(map_managed_backup_error)?;
    let recovery_exists = recovery_backup.try_exists().map_err(|error| {
        RuntimeFailure::from_error(
            io_subcode(&error, ErrorSubcode::IndexWrite),
            "managed_backup",
            error,
        )
    })?;
    let reservation = if prior_reservation.is_none() && recovery_exists {
        let recovered =
            inspect_backup(&recovery_backup, context.index_id).map_err(map_maintenance_error)?;
        if recovered.schema_version != plan.plan.schema_version
            || recovered.index_epoch != plan.plan.index_epoch
        {
            return Err(map_maintenance_error(
                MaintenanceError::RecoveryBackupMismatch,
            ));
        }
        catalog
            .reserve(
                context.index_id,
                &context.index_name,
                ManagedBackupKind::ForgetRecovery,
                &recovery_backup,
            )
            .map_err(map_managed_backup_error)?;
        catalog
            .complete(context.index_id, &recovery_backup, &recovered)
            .map_err(map_managed_backup_error)?;
        BackupReservation::Complete
    } else {
        catalog
            .reserve(
                context.index_id,
                &context.index_name,
                ManagedBackupKind::ForgetRecovery,
                &recovery_backup,
            )
            .map_err(map_managed_backup_error)?
    };
    if reservation == BackupReservation::Pending && recovery_exists {
        let recovered =
            inspect_backup(&recovery_backup, context.index_id).map_err(map_maintenance_error)?;
        if recovered.schema_version != plan.plan.schema_version
            || recovered.index_epoch != plan.plan.index_epoch
        {
            return Err(map_maintenance_error(
                MaintenanceError::RecoveryBackupMismatch,
            ));
        }
        catalog
            .complete(context.index_id, &recovery_backup, &recovered)
            .map_err(map_managed_backup_error)?;
    }
    let disposition = if arguments.purge_managed_backups {
        ManagedBackupDisposition::Purge
    } else {
        debug_assert!(arguments.keep_managed_backups);
        ManagedBackupDisposition::Keep
    };
    if disposition == ManagedBackupDisposition::Purge {
        catalog
            .preflight_purge(context.index_id)
            .map_err(map_managed_backup_error)?;
    }
    let outcome = match apply_forget(
        &context.database_path,
        &plan,
        arguments.confirm,
        &recovery_backup,
        &restore_plan,
        lock_timeout,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            catalog
                .cancel_if_missing(context.index_id, &recovery_backup)
                .map_err(map_managed_backup_error)?;
            return Err(map_maintenance_error(error));
        }
    };
    catalog
        .complete(context.index_id, &recovery_backup, &outcome.recovery_backup)
        .map_err(map_managed_backup_error)?;
    let backup_disposition = catalog
        .apply_disposition(context.index_id, disposition)
        .map_err(map_managed_backup_error)?;
    let mut text = String::new();
    text.push_str("Forget applied; the compacted replacement index passed Doctor.\n");
    push_line(&mut text, "Plan hash", &outcome.plan_hash.to_string());
    push_line(
        &mut text,
        "Forgotten revisions",
        &outcome.affected_revisions.to_string(),
    );
    push_line(
        &mut text,
        "Replacement epoch",
        &outcome.replacement_epoch.to_string(),
    );
    push_line(
        &mut text,
        "Recovery backup",
        &human_path(&outcome.recovery_backup.output),
    );
    push_line(
        &mut text,
        "Recovery SHA-256",
        &outcome.recovery_backup.file_sha256.to_string(),
    );
    push_line(
        &mut text,
        "Restore plan",
        &human_path(&outcome.restore_plan),
    );
    push_line(
        &mut text,
        "Restore plan hash",
        &outcome.restore_plan_hash.to_string(),
    );
    push_managed_backup_disposition(&mut text, &backup_disposition, disposition);
    if disposition == ManagedBackupDisposition::Keep {
        text.push_str(
            "Warning: retained managed backups may still contain the forgotten evidence; guard or destroy them according to your retention policy.\n",
        );
        text.push_str("Immediate undo only: hsum restore apply ");
        text.push_str(&shell_quote_path(&outcome.restore_plan));
        text.push_str(" --recovery-backup ");
        text.push_str(&shell_quote_path(&outcome.recovery_backup.output));
        text.push_str(" --safety-backup <new-safety.sqlite> --confirm ");
        text.push_str(&outcome.restore_plan_hash.to_string());
        text.push('\n');
    } else {
        text.push_str(
            "The recovery backup was purged; the restore plan remains as an audit artifact but immediate undo is unavailable.\n",
        );
    }
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn push_managed_backup_disposition(
    text: &mut String,
    outcome: &ManagedBackupDispositionOutcome,
    disposition: ManagedBackupDisposition,
) {
    text.push_str("Managed backup inventory at disposition:\n");
    append_managed_backup_inventory(text, &outcome.inventory);
    push_line(text, "Managed backups purged", &outcome.purged.to_string());
    push_line(
        text,
        "Managed backups retained",
        &outcome.retained.to_string(),
    );
    push_line(
        text,
        "Missing inventory entries cleared",
        &outcome.missing.to_string(),
    );
    push_line(
        text,
        "Disposition",
        match disposition {
            ManagedBackupDisposition::Keep => "kept",
            ManagedBackupDisposition::Purge => "purged",
        },
    );
}

fn run_restore_apply(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: RestoreApplyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let registry_path = managed_paths.managed_backup_registry_file();
    let context = direct_context(global, managed_paths, current_dir.clone())?;
    let plan_path = absolute_request_path(&current_dir, &arguments.plan);
    let recovery_backup = absolute_request_path(&current_dir, &arguments.recovery_backup);
    let plan: PlanEnvelope<RestorePlan> = read_plan(&plan_path).map_err(map_maintenance_error)?;
    validate_plan_envelope("hsum.restore-plan.v1", &plan, arguments.confirm)
        .map_err(map_maintenance_error)?;
    let lock_timeout = Duration::from_millis(arguments.lock_timeout_ms);
    let mut catalog = ManagedBackupCatalog::open(&registry_path, lock_timeout)
        .map_err(map_managed_backup_error)?;
    let requested_safety = absolute_request_path(&current_dir, &arguments.safety_backup);
    let safety_backup = catalog
        .normalize_output(&requested_safety)
        .map_err(map_managed_backup_error)?;
    catalog
        .reserve(
            context.index_id,
            &context.index_name,
            ManagedBackupKind::RestoreSafety,
            &safety_backup,
        )
        .map_err(map_managed_backup_error)?;
    let outcome = match apply_restore(
        &context.database_path,
        &plan,
        arguments.confirm,
        &recovery_backup,
        &safety_backup,
        lock_timeout,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            catalog
                .cancel_if_missing(context.index_id, &safety_backup)
                .map_err(map_managed_backup_error)?;
            return Err(map_maintenance_error(error));
        }
    };
    catalog
        .complete(context.index_id, &safety_backup, &outcome.safety_backup)
        .map_err(map_managed_backup_error)?;
    let mut text = String::new();
    text.push_str("Restore applied; the replacement index passed Doctor.\n");
    push_line(&mut text, "Plan hash", &outcome.plan_hash.to_string());
    push_line(
        &mut text,
        "Restored revisions",
        &outcome.restored_revisions.to_string(),
    );
    push_line(
        &mut text,
        "Replacement epoch",
        &outcome.replacement_epoch.to_string(),
    );
    push_line(
        &mut text,
        "Safety backup",
        &human_path(&outcome.safety_backup.output),
    );
    text.push_str("The safety backup intentionally preserves the pre-restore forgotten state.\n");
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_migrate_plan(
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: MigratePlanArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let database_path = managed_paths.index_database(&arguments.index);
    let output = absolute_request_path(&current_dir, &arguments.output);
    let plan = plan_migration(
        &database_path,
        arguments.index.as_str(),
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_maintenance_error)?;
    write_plan(&output, &plan).map_err(map_maintenance_error)?;
    let mut text = String::new();
    text.push_str("Migration plan written; the index was not changed.\n");
    push_line(&mut text, "Plan", &human_path(&output));
    push_line(&mut text, "Plan hash", &plan.plan_hash.to_string());
    push_line(
        &mut text,
        "Schema transition",
        &format!(
            "{} -> {}",
            plan.plan.from_schema_version, plan.plan.to_schema_version
        ),
    );
    push_line(
        &mut text,
        "Migration steps",
        &plan.plan.steps.len().to_string(),
    );
    if plan.plan.steps.is_empty() {
        text.push_str("The index already uses the current schema; there is nothing to apply.\n");
    } else {
        text.push_str("Apply: hsum migrate apply --index ");
        text.push_str(arguments.index.as_str());
        text.push(' ');
        text.push_str(&shell_quote_path(&output));
        text.push_str(" --backup <new-backup.sqlite> --confirm ");
        text.push_str(&plan.plan_hash.to_string());
        text.push('\n');
    }
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_migrate_apply(
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: MigrateApplyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let registry_path = managed_paths.managed_backup_registry_file();
    let database_path = managed_paths.index_database(&arguments.index);
    let plan_path = absolute_request_path(&current_dir, &arguments.plan);
    let plan: PlanEnvelope<MigrationPlan> = read_plan(&plan_path).map_err(map_maintenance_error)?;
    validate_plan_envelope("hsum.migration-plan.v1", &plan, arguments.confirm)
        .map_err(map_maintenance_error)?;
    let lock_timeout = Duration::from_millis(arguments.lock_timeout_ms);
    let mut catalog = ManagedBackupCatalog::open(&registry_path, lock_timeout)
        .map_err(map_managed_backup_error)?;
    let requested_backup = absolute_request_path(&current_dir, &arguments.backup);
    let backup = catalog
        .normalize_output(&requested_backup)
        .map_err(map_managed_backup_error)?;
    catalog
        .reserve(
            plan.plan.index_id,
            &arguments.index,
            ManagedBackupKind::Migration,
            &backup,
        )
        .map_err(map_managed_backup_error)?;
    let outcome = match apply_migration(
        &database_path,
        arguments.index.as_str(),
        &plan,
        arguments.confirm,
        &backup,
        lock_timeout,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            catalog
                .cancel_if_missing(plan.plan.index_id, &backup)
                .map_err(map_managed_backup_error)?;
            return Err(map_maintenance_error(error));
        }
    };
    catalog
        .complete(plan.plan.index_id, &backup, &outcome.backup)
        .map_err(map_managed_backup_error)?;
    let mut text = String::new();
    text.push_str("Migration applied and the resulting index passed Doctor.\n");
    push_line(&mut text, "Plan hash", &outcome.plan_hash.to_string());
    push_line(
        &mut text,
        "Schema transition",
        &format!(
            "{} -> {}",
            outcome.from_schema_version, outcome.to_schema_version
        ),
    );
    push_line(&mut text, "Backup", &human_path(&outcome.backup.output));
    text.push_str(
        "Rollback: stop hSUM readers, preserve the migrated index, replace it with the verified \
         backup above, and use the N-1 hSUM binary to run `hsum doctor`.\n",
    );
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_config_migrate_plan(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ConfigMigratePlanArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let config_file = global.config.as_deref().map_or_else(
        || managed_paths.config_file(),
        |path| absolute_request_path(&current_dir, path),
    );
    let trust_file = managed_paths.trust_registry_file();
    let output = absolute_request_path(&current_dir, &arguments.output);
    let backup_directory = absolute_request_path(&current_dir, &arguments.backup_dir);
    if output == config_file
        || output == trust_file
        || output.starts_with(&backup_directory)
        || backup_directory.starts_with(&output)
    {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::PathInvalid,
            "config_migration_paths",
            "the plan, live configuration, and backup bundle paths must not overlap",
        ));
    }
    let plan = plan_config_migration(
        &config_file,
        &trust_file,
        &backup_directory,
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_config_migration_error)?;
    write_plan(&output, &plan).map_err(map_maintenance_error)?;

    let mut text = String::new();
    text.push_str("Configuration migration plan written; no configuration files were changed.\n");
    push_line(&mut text, "Plan", &human_path(&output));
    push_line(&mut text, "Plan hash", &plan.plan_hash.to_string());
    push_line(
        &mut text,
        "Artifacts inspected",
        &plan.plan.artifacts.len().to_string(),
    );
    push_line(
        &mut text,
        "Migrations required",
        &plan.plan.migrations_required().to_string(),
    );
    push_line(
        &mut text,
        "Required peak bytes",
        &plan.plan.estimated_peak_bytes.to_string(),
    );
    push_line(
        &mut text,
        "Backup directory",
        &human_path(&backup_directory),
    );
    if plan.plan.migrations_required() == 0 {
        text.push_str("Configuration and trust files already use the current schema.\n");
    } else {
        text.push_str("Apply: hsum");
        if global.config.is_some() {
            text.push_str(" --config ");
            text.push_str(&shell_quote_path(&config_file));
        }
        text.push_str(" config migrate apply ");
        text.push_str(&shell_quote_path(&output));
        text.push_str(" --confirm ");
        text.push_str(&plan.plan_hash.to_string());
        text.push('\n');
    }
    write_stdout(text.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_config_migrate_apply(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: ConfigMigrateApplyArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    let config_file = global.config.as_deref().map_or_else(
        || managed_paths.config_file(),
        |path| absolute_request_path(&current_dir, path),
    );
    let trust_file = managed_paths.trust_registry_file();
    let plan_path = absolute_request_path(&current_dir, &arguments.plan);
    let plan: PlanEnvelope<ConfigMigrationPlan> =
        read_plan(&plan_path).map_err(map_maintenance_error)?;
    let outcome = apply_config_migration(
        &config_file,
        &trust_file,
        &plan,
        arguments.confirm,
        Duration::from_millis(arguments.lock_timeout_ms),
    )
    .map_err(map_config_migration_error)?;

    let mut text = String::new();
    text.push_str("Configuration migration applied and validated.\n");
    push_line(&mut text, "Plan hash", &outcome.plan_hash.to_string());
    push_line(
        &mut text,
        "Migrated artifacts",
        &outcome.migrated_artifacts.to_string(),
    );
    push_line(
        &mut text,
        "Already migrated artifacts",
        &outcome.already_migrated_artifacts.to_string(),
    );
    push_line(
        &mut text,
        "Backup directory",
        &human_path(&outcome.backup_directory),
    );
    text.push_str(
        "Rollback: stop hSUM processes and restore each exact .bak file present in the backup directory above with its private permissions.\n",
    );
    write_stdout(text.as_bytes())?;
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
    let probe = match arguments.client {
        ClientKind::Codex => run_client_stdio_probe_at(
            &launch,
            context.project_id.to_string(),
            context
                .canonical_root
                .clone()
                .unwrap_or_else(|| context.source_root.clone()),
        )?,
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Generic => {
            run_client_stdio_probe(&launch, context.project_id.to_string())?
        }
    };

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

fn run_integration_install(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: IntegrationInstallArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let agent_policy_manager = match arguments.agent_policy {
        AgentPolicyMode::Install => {
            let policy = CodexAgentPolicy::discover().map_err(map_agent_policy_error)?;
            policy.status().map_err(map_agent_policy_error)?;
            Some(policy)
        }
        AgentPolicyMode::Skip => None,
    };
    let activation = arguments
        .activate
        .map(|path| initialize_integration_repository(current_dir, managed_paths, path))
        .transpose()?;
    let hsum_executable = absolute_executable()?;
    let codex = CodexIntegration::discover().map_err(map_integration_error)?;
    let change = codex
        .install_or_repair(&hsum_executable, arguments.replace_conflict)
        .map_err(map_integration_error)?;
    let agent_policy = agent_policy_manager
        .map(|policy| policy.install().map_err(map_agent_policy_error))
        .transpose()?;
    let probe = activation
        .as_ref()
        .map(|outcome| {
            let query = match &outcome.next_step {
                InitNextStep::Search { query } => Some(query.as_str()),
                InitNextStep::Status => None,
            };
            run_client_stdio_probe_with_query(
                &ClientLaunchConfig {
                    executable: hsum_executable.clone(),
                    arguments: vec!["mcp".to_owned()],
                },
                outcome.project_id.to_string(),
                outcome.canonical_root.clone(),
                query,
            )
        })
        .transpose()?;

    let mut output = String::new();
    output.push_str("hSUM is registered with Codex.\n");
    push_line(&mut output, "Registration", "current");
    push_line(
        &mut output,
        "Registration changed",
        bool_text(change.changed),
    );
    push_line(
        &mut output,
        "Previous registration",
        change.previous.label(),
    );
    push_line(
        &mut output,
        "Codex executable",
        &human_path(codex.executable()),
    );
    push_line(
        &mut output,
        "hSUM executable",
        &human_path(&change.current.command),
    );
    if let Some(policy) = agent_policy {
        push_line(&mut output, "Agent policy", policy.current.label());
        push_line(
            &mut output,
            "Agent policy changed",
            bool_text(policy.changed),
        );
        push_line(&mut output, "Agent policy file", &human_path(&policy.path));
    } else {
        push_line(&mut output, "Agent policy", "skipped");
    }
    if let Some(outcome) = activation.as_ref() {
        render_integration_activation(&mut output, outcome);
    }
    if let Some(probe) = probe {
        push_line(
            &mut output,
            "Verified read-only tools",
            &probe.read_only_tools.to_string(),
        );
        push_line(&mut output, "Verified project ID", &probe.project_id);
        push_line(
            &mut output,
            "Citation round trip",
            if probe.citation_verified {
                "verified"
            } else {
                "not available; the index has no searchable passage"
            },
        );
    }
    output.push_str(
        "Codex will launch hSUM automatically for each task and hSUM will select only that \
         task's trusted repository.\n",
    );
    output.push_str(
        "hSUM uploads no corpus data or telemetry. Returned passages remain subject to the \
         agent client's model-provider policy.\n",
    );
    output.push_str(
        "READY FOR FUTURE TASKS: this task may predate MCP registration; hSUM CLI is ready now.\n",
    );
    write_stdout(output.as_bytes())?;

    Ok(activation
        .as_ref()
        .map_or(crate::cli::ProcessExitCategory::Success, |outcome| {
            outcome.ingest.as_ref().map_or(
                crate::cli::ProcessExitCategory::Success,
                ingest_exit_category,
            )
        }))
}

fn run_integration_activate(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    current_dir: PathBuf,
    arguments: IntegrationActivateArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let outcome = initialize_integration_repository(current_dir, managed_paths, arguments.path)?;
    let hsum_executable = absolute_executable()?;
    let codex = CodexIntegration::discover().map_err(map_integration_error)?;
    let change = codex
        .install_or_repair(&hsum_executable, false)
        .map_err(map_integration_error)?;
    let query = match &outcome.next_step {
        InitNextStep::Search { query } => Some(query.as_str()),
        InitNextStep::Status => None,
    };
    let probe = run_client_stdio_probe_with_query(
        &ClientLaunchConfig {
            executable: hsum_executable,
            arguments: vec!["mcp".to_owned()],
        },
        outcome.project_id.to_string(),
        outcome.canonical_root.clone(),
        query,
    )?;

    let mut output = String::new();
    output.push_str("hSUM is active for this repository.\n");
    render_integration_activation(&mut output, &outcome);
    push_line(&mut output, "Registration", "current");
    push_line(
        &mut output,
        "Registration changed",
        bool_text(change.changed),
    );
    push_line(
        &mut output,
        "Verified read-only tools",
        &probe.read_only_tools.to_string(),
    );
    push_line(&mut output, "Verified project ID", &probe.project_id);
    push_line(
        &mut output,
        "Citation round trip",
        if probe.citation_verified {
            "verified"
        } else {
            "not available; the index has no searchable passage"
        },
    );
    output.push_str(
        "READY NOW: an already-running hSUM workspace server can retry its original tool call; \
         hSUM CLI is also ready.\n",
    );
    write_stdout(output.as_bytes())?;

    Ok(outcome.ingest.as_ref().map_or(
        crate::cli::ProcessExitCategory::Success,
        ingest_exit_category,
    ))
}

fn run_integration_status(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IntegrationStatusArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = arguments.client;
    let hsum_executable = absolute_executable()?;
    let codex = CodexIntegration::discover().map_err(map_integration_error)?;
    let state = codex
        .status(&hsum_executable)
        .map_err(map_integration_error)?;
    let agent_policy = CodexAgentPolicy::discover()
        .and_then(|policy| policy.status())
        .map_err(map_agent_policy_error)?;
    let workspace_policy = WorkspacePolicy::load_or_empty(&managed_paths.integration_policy_file())
        .map_err(map_workspace_policy_error)?;

    if arguments.json {
        return write_json_stdout(&IntegrationStatusJson::new(
            &state,
            codex.executable(),
            &hsum_executable,
            agent_policy.0,
            &agent_policy.1,
            workspace_policy.roots().len(),
        ))
        .map(|()| crate::cli::ProcessExitCategory::Success);
    }

    let mut output = String::new();
    push_line(&mut output, "Client", "codex");
    push_line(&mut output, "Registration", state.label());
    push_line(
        &mut output,
        "Codex executable",
        &human_path(codex.executable()),
    );
    push_line(
        &mut output,
        "Expected hSUM executable",
        &human_path(&hsum_executable),
    );
    push_line(&mut output, "Agent policy", agent_policy.0.label());
    push_line(
        &mut output,
        "Agent policy file",
        &human_path(&agent_policy.1),
    );
    push_line(
        &mut output,
        "Authorized workspaces",
        &workspace_policy.roots().len().to_string(),
    );
    if let Some(registration) = state.registration() {
        push_line(
            &mut output,
            "Registered transport",
            if registration.is_stdio {
                "stdio"
            } else {
                "non-stdio"
            },
        );
        if registration.is_stdio {
            push_line(
                &mut output,
                "Registered executable",
                &human_path(&registration.command),
            );
            push_line(
                &mut output,
                "Registered arguments",
                &registration.arguments.join(" "),
            );
        }
    }
    match state {
        CodexRegistrationState::Current(_) => {
            output.push_str("READY FOR FUTURE TASKS: Codex will load hSUM automatically.\n");
        }
        CodexRegistrationState::Absent => {
            output.push_str("REPAIR REQUIRED: run `hsum integration repair codex --confirm`.\n");
        }
        CodexRegistrationState::LegacyBinding(_) | CodexRegistrationState::Stale(_) => {
            output.push_str("REPAIR REQUIRED: run `hsum integration repair codex --confirm`.\n");
        }
        CodexRegistrationState::Conflict(_) => {
            output.push_str(
                "REPAIR REQUIRED: inspect the foreign `hsum` entry, then explicitly use \
                 `--replace-conflict` only if replacement is intended.\n",
            );
        }
    }
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_integration_authorize_workspace(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IntegrationWorkspaceArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let path = managed_paths.integration_policy_file();
    let mut policy = WorkspacePolicy::load_or_empty(&path).map_err(map_workspace_policy_error)?;
    let (root, changed) = policy
        .authorize(&arguments.path)
        .map_err(map_workspace_policy_error)?;
    if changed {
        policy
            .save_atomic(&path)
            .map_err(map_workspace_policy_error)?;
    }

    let mut output = String::new();
    push_line(&mut output, "Client", "codex");
    push_line(&mut output, "Workspace", &human_path(&root));
    push_line(&mut output, "Authorization changed", bool_text(changed));
    output.push_str(
        "Compatibility record stored. Read-only MCP does not activate repositories from this \
         record; run `hsum integration activate codex --path <repository> --confirm` for each \
         repository.\n",
    );
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_integration_revoke_workspace(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IntegrationWorkspaceArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let path = managed_paths.integration_policy_file();
    let mut policy = WorkspacePolicy::load_or_empty(&path).map_err(map_workspace_policy_error)?;
    let (root, changed) = policy
        .revoke(&arguments.path)
        .map_err(map_workspace_policy_error)?;
    if changed {
        policy
            .save_atomic(&path)
            .map_err(map_workspace_policy_error)?;
    }

    let mut output = String::new();
    push_line(&mut output, "Client", "codex");
    push_line(&mut output, "Workspace", &human_path(&root));
    push_line(&mut output, "Authorization removed", bool_text(changed));
    output.push_str(
        "Existing repository bindings and indexes were kept. Read-only MCP does not perform \
         lazy activation.\n",
    );
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_integration_repair(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IntegrationRepairArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let hsum_executable = absolute_executable()?;
    let codex = CodexIntegration::discover().map_err(map_integration_error)?;
    let change = codex
        .install_or_repair(&hsum_executable, arguments.replace_conflict)
        .map_err(map_integration_error)?;

    let mut output = String::new();
    output.push_str("hSUM's Codex registration is healthy.\n");
    push_line(
        &mut output,
        "Previous registration",
        change.previous.label(),
    );
    push_line(&mut output, "Registration", "current");
    push_line(
        &mut output,
        "Registration changed",
        bool_text(change.changed),
    );
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn run_integration_uninstall(
    global: &GlobalOptions,
    managed_paths: ManagedPaths,
    arguments: IntegrationUninstallArgs,
) -> Result<crate::cli::ProcessExitCategory, RuntimeFailure> {
    ensure_integration_paths_are_stable(global)?;
    let _integration_lock = acquire_integration_lock(&managed_paths)?;
    let _ = (arguments.client, arguments.confirm);
    let hsum_executable = absolute_executable()?;
    let codex = CodexIntegration::discover().map_err(map_integration_error)?;
    let agent_policy = CodexAgentPolicy::discover().map_err(map_agent_policy_error)?;
    agent_policy.status().map_err(map_agent_policy_error)?;
    let previous = codex
        .uninstall(&hsum_executable)
        .map_err(map_integration_error)?;
    let policy = agent_policy.uninstall().map_err(map_agent_policy_error)?;
    let removed = !matches!(previous, CodexRegistrationState::Absent);

    let mut output = String::new();
    push_line(&mut output, "Client", "codex");
    push_line(&mut output, "Registration removed", bool_text(removed));
    push_line(
        &mut output,
        "Agent policy removed",
        bool_text(policy.changed),
    );
    output.push_str("Managed indexes and repository trust bindings were kept.\n");
    write_stdout(output.as_bytes())?;
    Ok(crate::cli::ProcessExitCategory::Success)
}

fn initialize_integration_repository(
    current_dir: PathBuf,
    managed_paths: ManagedPaths,
    path: PathBuf,
) -> Result<InitOutcome, RuntimeFailure> {
    let policy_path = managed_paths.integration_policy_file();
    let mut request = InitRequest::new(current_dir, managed_paths);
    request.requested_root = Some(path);
    let outcome = initialize(&request).map_err(map_init_error)?;
    let mut policy =
        WorkspacePolicy::load_or_empty(&policy_path).map_err(map_workspace_policy_error)?;
    if policy
        .mark_repository(&outcome.canonical_root)
        .map_err(map_workspace_policy_error)?
    {
        policy
            .save_atomic(&policy_path)
            .map_err(map_workspace_policy_error)?;
    }
    Ok(outcome)
}

fn render_integration_activation(output: &mut String, outcome: &InitOutcome) {
    push_line(output, "Root", &human_path(&outcome.canonical_root));
    push_line(output, "Index", outcome.index_name.as_str());
    push_line(output, "Project", outcome.project_name.as_str());
    push_line(output, "Project ID", &outcome.project_id.to_string());
    if let Some(ingest) = outcome.ingest.as_ref() {
        push_line(
            output,
            "Active documents",
            &ingest.active_documents.to_string(),
        );
        push_line(
            output,
            "Active passages",
            &ingest.active_passages.to_string(),
        );
    }
}

fn ensure_integration_paths_are_stable(global: &GlobalOptions) -> Result<(), RuntimeFailure> {
    if global.data_dir.is_some() || global.cache_dir.is_some() {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::ConfigInvalid,
            "integration_paths",
            "managed Codex registration does not support per-command data or cache overrides",
        ));
    }
    Ok(())
}

fn acquire_integration_lock(managed_paths: &ManagedPaths) -> Result<WriterLock, RuntimeFailure> {
    fs::create_dir_all(managed_paths.config_dir()).map_err(|error| {
        RuntimeFailure::from_error(
            ErrorSubcode::IndexWrite,
            "integration_lock_directory",
            error,
        )
    })?;
    WriterLock::acquire(
        &managed_paths.config_dir().join("integration"),
        INTEGRATION_LOCK_TIMEOUT,
    )
    .map_err(|error| RuntimeFailure::from_error(store_subcode(&error), "integration_lock", error))
}

#[derive(Serialize)]
struct IntegrationStatusJson {
    schema_version: &'static str,
    request_id: String,
    client: &'static str,
    registration: &'static str,
    codex_executable: String,
    expected_hsum_executable: String,
    registered_executable: Option<String>,
    registered_arguments: Option<Vec<String>>,
    stdio: Option<bool>,
    enabled: Option<bool>,
    fixed_cwd: Option<String>,
    has_environment: Option<bool>,
    has_tool_filters: Option<bool>,
    agent_policy: &'static str,
    agent_policy_file: String,
    authorized_workspaces: usize,
    next_actions: Vec<String>,
}

impl IntegrationStatusJson {
    fn new(
        state: &CodexRegistrationState,
        codex: &Path,
        expected_hsum: &Path,
        agent_policy: AgentPolicyState,
        agent_policy_file: &Path,
        authorized_workspaces: usize,
    ) -> Self {
        let registration = state.registration();
        Self {
            schema_version: MCP_API_VERSION,
            request_id: Uuid::new_v4().to_string(),
            client: "codex",
            registration: state.label(),
            codex_executable: json_path(codex),
            expected_hsum_executable: json_path(expected_hsum),
            registered_executable: registration
                .filter(|value| value.is_stdio)
                .map(|value| json_path(&value.command)),
            registered_arguments: registration
                .filter(|value| value.is_stdio)
                .map(|value| value.arguments.clone()),
            stdio: registration.map(|value| value.is_stdio),
            enabled: registration.map(|value| value.enabled),
            fixed_cwd: registration.and_then(|value| value.cwd.as_deref().map(json_path)),
            has_environment: registration.map(|value| value.has_environment),
            has_tool_filters: registration.map(|value| value.has_tool_filters),
            agent_policy: agent_policy.label(),
            agent_policy_file: json_path(agent_policy_file),
            authorized_workspaces,
            next_actions: match state {
                CodexRegistrationState::Current(_) if agent_policy == AgentPolicyState::Current => {
                    Vec::new()
                }
                CodexRegistrationState::Current(_) => {
                    vec!["hsum integration install codex --agent-policy install".to_owned()]
                }
                CodexRegistrationState::Absent
                | CodexRegistrationState::LegacyBinding(_)
                | CodexRegistrationState::Stale(_) => {
                    vec!["hsum integration repair codex --confirm".to_owned()]
                }
                CodexRegistrationState::Conflict(_) => vec![
                    "inspect the existing Codex MCP entry named hsum".to_owned(),
                    "hsum integration repair codex --confirm --replace-conflict".to_owned(),
                ],
            },
        }
    }
}

fn map_agent_policy_error(error: AgentPolicyError) -> RuntimeFailure {
    RuntimeFailure::from_error(ErrorSubcode::ConfigInvalid, "codex_agent_policy", error)
}

fn map_workspace_policy_error(error: WorkspacePolicyError) -> RuntimeFailure {
    RuntimeFailure::from_error(ErrorSubcode::ConfigInvalid, "codex_workspace_policy", error)
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
    citation_verified: bool,
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
    let expected_arguments = match client {
        ClientKind::Codex => vec!["mcp".to_owned()],
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Generic => {
            vec![
                "mcp".to_owned(),
                "--binding".to_owned(),
                expected_binding.to_owned(),
            ]
        }
    };
    if arguments != expected_arguments {
        return Err(RuntimeFailure::from_error(
            ErrorSubcode::ConfigInvalid,
            "client_doctor_config",
            "generated client configuration did not preserve the expected MCP scope",
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
    let probe = run_client_stdio_probe_at(
        launch,
        expected_project_id,
        working_directory.path().to_path_buf(),
    )?;
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
    Ok(probe)
}

fn run_client_stdio_probe_at(
    launch: &ClientLaunchConfig,
    expected_project_id: String,
    working_directory: PathBuf,
) -> Result<ClientProbe, RuntimeFailure> {
    run_client_stdio_probe_with_query(launch, expected_project_id, working_directory, None)
}

fn run_client_stdio_probe_with_query(
    launch: &ClientLaunchConfig,
    expected_project_id: String,
    working_directory: PathBuf,
    query: Option<&str>,
) -> Result<ClientProbe, RuntimeFailure> {
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
        .current_dir(&working_directory)
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

    let requests = client_probe_requests(query)?;
    write_probe_request(&mut stdin, &requests[0])?;
    let mut initialized = false;
    let mut listed_tools = None;
    let mut status_project = None;
    let mut search_citation = None;
    let requires_citation = query.is_some();
    let mut citation_verified = false;
    let mut sent_tools = false;
    let mut sent_status = false;
    let mut sent_search = false;
    let mut sent_get = false;
    while !initialized
        || listed_tools.is_none()
        || status_project.is_none()
        || (requires_citation && !citation_verified)
    {
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
                    &mut search_citation,
                    &mut citation_verified,
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
                if status_project.is_some() && query.is_some() && !sent_search {
                    write_probe_request(&mut stdin, &requests[4])?;
                    sent_search = true;
                }
                if let Some(citation) = search_citation.as_deref()
                    && !sent_get
                {
                    let get = serialize_probe_request(&json!({
                        "jsonrpc": "2.0",
                        "id": 5,
                        "method": "tools/call",
                        "params": {
                            "name": "evidence_get",
                            "arguments": {
                                "citation_uri": citation,
                                "verify_source_hash": true
                            }
                        }
                    }))?;
                    write_probe_request(&mut stdin, &get)?;
                    sent_get = true;
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
    Ok(ClientProbe {
        project_id: status_project.expect("probe loop requires status response"),
        read_only_tools: listed_tools.expect("probe loop requires tools response"),
        working_directory,
        citation_verified,
    })
}

fn client_probe_requests(query: Option<&str>) -> Result<Vec<Vec<u8>>, RuntimeFailure> {
    let mut requests = vec![
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
    if let Some(query) = query {
        requests.push(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": query,
                    "limit": 1,
                    "timeout_ms": 3000,
                    "explain": false
                }
            }
        }));
    }
    requests.iter().map(serialize_probe_request).collect()
}

fn serialize_probe_request(request: &Value) -> Result<Vec<u8>, RuntimeFailure> {
    let frame = serde_json::to_vec(request).map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::Unexpected, "client_doctor_frame", error)
    })?;
    validate_frame(&frame).map_err(|error| {
        RuntimeFailure::from_error(ErrorSubcode::FrameLimit, "client_doctor_frame", error)
    })?;
    Ok(frame)
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
    search_citation: &mut Option<String>,
    citation_verified: &mut bool,
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
        4 if search_citation.is_none() => {
            let search = response
                .pointer("/result/structuredContent")
                .ok_or_else(|| {
                    probe_protocol_error(
                        "the MCP evidence_search response omitted structured content",
                    )
                })?;
            if search.get("schema_version").and_then(Value::as_str) != Some(MCP_API_VERSION)
                || search.get("project_id").and_then(Value::as_str) != Some(expected_project_id)
            {
                return Err(probe_protocol_error(
                    "the MCP evidence_search response escaped the configured project binding",
                ));
            }
            let citation = search
                .pointer("/results/0/citation_uri")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    probe_protocol_error(
                        "the MCP evidence_search response did not return a probe citation",
                    )
                })?;
            *search_citation = Some(citation.to_owned());
        }
        5 if !*citation_verified => {
            let evidence = response
                .pointer("/result/structuredContent")
                .ok_or_else(|| {
                    probe_protocol_error("the MCP evidence_get response omitted structured content")
                })?;
            if evidence.get("schema_version").and_then(Value::as_str) != Some(MCP_API_VERSION)
                || evidence
                    .get("requested_citation_uri")
                    .and_then(Value::as_str)
                    != search_citation.as_deref()
                || evidence
                    .get("content")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(probe_protocol_error(
                    "the MCP evidence_get response did not resolve the probe citation",
                ));
            }
            *citation_verified = true;
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
    if arguments.binding.is_none() && arguments.project.is_none() {
        let _ = global;
        serve_workspace_stdio(current_dir, managed_paths)
            .await
            .map_err(map_mcp_server_error)?;
        return Ok(crate::cli::ProcessExitCategory::Success);
    }

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

fn render_ingest_plan(
    context: &EffectiveContext,
    project_plan: &ProjectIngestPlan,
    confirmations: DeleteConfirmations,
) -> Result<(), RuntimeFailure> {
    let plan = &project_plan.aggregate;
    let mut output = String::new();
    output.push_str("DRY RUN — no index data was changed.\n");
    push_line(&mut output, "Project", context.project_name.as_str());
    push_line(
        &mut output,
        "Targeted sources",
        &project_plan.targeted_sources.to_string(),
    );
    push_line(
        &mut output,
        "Failed sources",
        &project_plan.failed_sources.to_string(),
    );
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
        output.push_str("Ingest failed; no generation was activated.\n");
    } else {
        output.push_str("Ingest complete.\n");
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
    offset: usize,
    next_cursor: Option<&str>,
) -> Result<(), RuntimeFailure> {
    let executable = absolute_executable()?;
    let mut output = String::new();
    for (index, passage) in response.results.iter().enumerate() {
        let state = EvidenceSourceState::from_observation(drift_for(
            drift,
            passage.source_id,
            passage.document_id,
        ));
        output.push_str(&(offset + index + 1).to_string());
        output.push_str("  ");
        output.push_str(&escape_terminal_text(&passage.source_uri));
        output.push(':');
        output.push_str(&passage.line_span.start().to_string());
        output.push('-');
        output.push_str(&passage.line_span.end().to_string());
        output.push_str("  [");
        output.push_str(state.as_str());
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
    if let Some(next_cursor) = next_cursor {
        output.push_str("next cursor: ");
        output.push_str(&escape_terminal_text(next_cursor));
        output.push('\n');
    }
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
    push_line(
        &mut output,
        "Abandoned generations",
        &report.abandoned_generations.to_string(),
    );
    output.push_str(
        "Security note: filesystem hard links cannot be distinguished from their original \
         regular files; secret-path rules are defense in depth.\n",
    );
    write_stdout(output.as_bytes())
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
    let arguments = match client {
        ClientKind::Codex => json!(["mcp"]),
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Generic => {
            json!(["mcp", "--binding", binding])
        }
    };
    let server = json!({
        "command": json_path(executable),
        "args": arguments,
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
            | Command::Source(crate::cli::SourceArgs {
                command: SourceCommand::List(SourceListArgs { json: true, .. }),
            })
            | Command::Project(crate::cli::ProjectArgs {
                command: ProjectCommand::List(ProjectListArgs { json: true }),
            })
            | Command::Integration(crate::cli::IntegrationArgs {
                command: IntegrationCommand::Status(IntegrationStatusArgs { json: true, .. }),
            })
            | Command::Model(crate::cli::ModelArgs {
                command: ModelCommand::Install(ModelInstallArgs { json: true, .. })
                    | ModelCommand::Import(ModelImportArgs { json: true, .. })
                    | ModelCommand::List(ModelListArgs { json: true })
                    | ModelCommand::Verify(ModelVerifyArgs { json: true, .. })
                    | ModelCommand::Remove(ModelRemoveArgs { json: true, .. }),
            })
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

fn absolute_request_path(current_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    }
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
        | ContextError::InvalidConfigEpoch
        | ContextError::IncompleteConfiguredDefault
        | ContextError::IncompleteEnvironmentSelection
        | ContextError::NonUtf8Environment
        | ContextError::LogicalSelection(_)
        | ContextError::InvalidFilesystemSourceConfig(_) => ErrorSubcode::ConfigInvalid,
        ContextError::ConfigSchema { found } => config_schema_subcode(*found),
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
    let subcode = init_subcode(&error);
    RuntimeFailure::from_error(subcode, "init", error)
}

fn init_subcode(error: &InitError) -> ErrorSubcode {
    match error {
        InitError::BroadRootConfirmationRequired { .. } => {
            ErrorSubcode::BroadRootConfirmationRequired
        }
        InitError::LargeSourceConfirmationRequired { .. } => {
            ErrorSubcode::LargeSourceConfirmationRequired
        }
        InitError::TrustConfirmationRequired => ErrorSubcode::TrustConfirmationRequired,
        InitError::RebuildWithoutIngest => ErrorSubcode::ConfigInvalid,
        InitError::RebuildBindingRequired { .. } => ErrorSubcode::IndexNotFound,
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
        | InitError::InvalidTrustedCardinality
        | InitError::RebuildBindingChanged { .. } => ErrorSubcode::PathTrust,
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
    }
}

fn map_project_ingest_error(error: ProjectIngestError) -> RuntimeFailure {
    let subcode = match &error {
        ProjectIngestError::NoSources
        | ProjectIngestError::FilesystemAuthorityMismatch
        | ProjectIngestError::InconsistentIndexQuota { .. }
        | ProjectIngestError::JsonlConfig(_) => ErrorSubcode::ConfigInvalid,
        ProjectIngestError::StrictFilesystemSource { .. }
        | ProjectIngestError::StrictFilesystemFailures { .. } => {
            ErrorSubcode::EnumerationIncomplete
        }
        ProjectIngestError::StrictJsonlSource { .. } => ErrorSubcode::EnumerationIncomplete,
        ProjectIngestError::SourceManagement(error) => source_management_subcode(error),
        ProjectIngestError::Filesystem(error) => filesystem_ingest_subcode(error),
        ProjectIngestError::Store(error) => store_subcode(error),
        ProjectIngestError::StoragePreflight(error) => storage_preflight_subcode(error),
    };
    RuntimeFailure::from_error(subcode, "ingest", error)
}

fn map_project_management_error(error: ProjectManagementError) -> RuntimeFailure {
    match error {
        ProjectManagementError::Init(error) => map_init_error(error),
        ProjectManagementError::Store(error) => {
            let subcode = store_subcode(&error);
            RuntimeFailure::from_error(subcode, "project", error)
        }
        ProjectManagementError::StoragePreflight(error) => {
            let subcode = storage_preflight_subcode(&error);
            RuntimeFailure::from_error(subcode, "project", error)
        }
        ProjectManagementError::Trust(error) => {
            let subcode = trust_subcode(&error);
            RuntimeFailure::from_error(subcode, "project", error)
        }
        ProjectManagementError::Config(error) => {
            let subcode = source_config_subcode(&error);
            RuntimeFailure::from_error(subcode, "project", error)
        }
        error @ ProjectManagementError::ProjectNotFound(_) => {
            RuntimeFailure::from_error(ErrorSubcode::ProjectNotFound, "project", error)
        }
        error @ ProjectManagementError::PersistentBindingRequired => {
            RuntimeFailure::from_error(ErrorSubcode::PathTrust, "project", error)
        }
        error @ ProjectManagementError::NonUtf8Root => {
            RuntimeFailure::from_error(ErrorSubcode::PathInvalid, "project", error)
        }
        error @ ProjectManagementError::SourceNameCollision => {
            RuntimeFailure::from_error(ErrorSubcode::ConfigInvalid, "project", error)
        }
        error @ ProjectManagementError::TrustRollback { .. } => {
            RuntimeFailure::from_error(ErrorSubcode::Invariant, "project", error)
        }
    }
}

fn map_source_management_error(error: SourceManagementError) -> RuntimeFailure {
    let subcode = source_management_subcode(&error);
    RuntimeFailure::from_error(subcode, "source", error)
}

fn map_index_management_error(error: IndexManagementError) -> RuntimeFailure {
    let subcode = match &error {
        IndexManagementError::IndexNotFound(_) => ErrorSubcode::IndexNotFound,
        IndexManagementError::Store(error) => store_subcode(error),
        IndexManagementError::Trust(error) => trust_subcode(error),
        IndexManagementError::ConfigSchema { found } => config_schema_subcode(*found),
        IndexManagementError::UnsafeManagedDirectory(_) => ErrorSubcode::UnsupportedStorage,
        IndexManagementError::InvalidManagedPath(_) => ErrorSubcode::PathInvalid,
        IndexManagementError::QuarantineCleanup { source, .. }
        | IndexManagementError::Io(source) => io_subcode(source, ErrorSubcode::IndexWrite),
        IndexManagementError::DeletionRecoveryConflict
        | IndexManagementError::ConfigPathOverlap
        | IndexManagementError::IncompleteConfiguredDefault
        | IndexManagementError::InvalidConfigEpoch
        | IndexManagementError::ConfigEpochOverflow
        | IndexManagementError::ConfigUnsafe
        | IndexManagementError::ConfigTooLarge
        | IndexManagementError::ConfigChanged
        | IndexManagementError::ConfigNotUtf8
        | IndexManagementError::ConfigToml(_)
        | IndexManagementError::LogicalSelection(_) => ErrorSubcode::ConfigInvalid,
        IndexManagementError::ConfigSerialize(_) => ErrorSubcode::Invariant,
    };
    RuntimeFailure::from_error(subcode, "index_delete", error)
}

fn source_management_subcode(error: &SourceManagementError) -> ErrorSubcode {
    match error {
        SourceManagementError::Init(error) => init_subcode(error),
        SourceManagementError::Trust(error) => trust_subcode(error),
        SourceManagementError::Canonicalize(error)
            if error.kind() == io::ErrorKind::PermissionDenied =>
        {
            ErrorSubcode::SourceRead
        }
        SourceManagementError::Canonicalize(_)
        | SourceManagementError::NotRegularFile
        | SourceManagementError::NonUtf8Path
        | SourceManagementError::NonUtf8FilesystemRoot => ErrorSubcode::PathInvalid,
        SourceManagementError::FilesystemConfig(error) => source_config_subcode(error),
        SourceManagementError::Config(_)
        | SourceManagementError::SourceNotFound(_)
        | SourceManagementError::RemovalRequiresJsonl
        | SourceManagementError::MembershipRequiresJsonl => ErrorSubcode::ConfigInvalid,
        SourceManagementError::Store(error) => store_subcode(error),
        SourceManagementError::StoragePreflight(error) => storage_preflight_subcode(error),
    }
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
        | TrustError::InvalidConfigEpoch
        | TrustError::ConfigEpochOverflow
        | TrustError::InvalidIndexName(_)
        | TrustError::InvalidProjectName(_)
        | TrustError::NonCanonicalRoot { .. }
        | TrustError::InvalidStoredRoot { .. }
        | TrustError::ConflictingRoot { .. }
        | TrustError::ConflictingIdentity
        | TrustError::DuplicateBinding { .. }
        | TrustError::NonRandomBinding { .. }
        | TrustError::BindingNotFound { .. }
        | TrustError::TooManyBindings { .. }
        | TrustError::NonUtf8Root { .. }
        | TrustError::UnsafeFile
        | TrustError::TooLarge
        | TrustError::ChangedDuringRead
        | TrustError::NotUtf8
        | TrustError::InvalidIdentity(_) => ErrorSubcode::TrustRegistryInvalid,
        TrustError::UnsupportedSchema { found } => trust_schema_subcode(*found),
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

const fn schema_subcode(found: u32, current: u32, previous: u32) -> ErrorSubcode {
    if found == previous {
        ErrorSubcode::MigrationRequired
    } else if found < previous {
        ErrorSubcode::UpgradeRequired
    } else if found > current {
        ErrorSubcode::DowngradeUnsupported
    } else {
        ErrorSubcode::ConfigInvalid
    }
}

const fn config_schema_subcode(found: u32) -> ErrorSubcode {
    schema_subcode(found, CONFIG_SCHEMA_VERSION, PREVIOUS_CONFIG_SCHEMA_VERSION)
}

const fn trust_schema_subcode(found: u32) -> ErrorSubcode {
    schema_subcode(found, TRUST_SCHEMA_VERSION, TRUST_PREVIOUS_SCHEMA_VERSION)
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

fn map_search_evidence_error(error: SearchEvidenceError) -> RuntimeFailure {
    match error {
        SearchEvidenceError::Store(error) => map_store_error(error),
        SearchEvidenceError::Search(error) => map_search_error(error),
        SearchEvidenceError::Status(error) => map_status_error(error),
        SearchEvidenceError::SourceConfig(error) => {
            RuntimeFailure::from_error(source_config_subcode(&error), "search_source_root", error)
        }
        SearchEvidenceError::Sqlite(error) => {
            RuntimeFailure::from_error(sqlite_subcode(&error), "search", error)
        }
        error @ (SearchEvidenceError::FieldLimit | SearchEvidenceError::BodyLimit) => {
            RuntimeFailure::from_error(ErrorSubcode::MemoryBudget, "search", error)
        }
        error @ (SearchEvidenceError::SourceUnavailable | SearchEvidenceError::Corrupt(_)) => {
            RuntimeFailure::from_error(ErrorSubcode::HeadIndexMismatch, "search", error)
        }
        SearchEvidenceError::SnapshotChanged { expected, actual } => {
            let subcode = if expected.index_id != actual.index_id
                || expected.index_epoch != actual.index_epoch
            {
                ErrorSubcode::IndexEpoch
            } else if expected.generation != actual.generation {
                ErrorSubcode::Generation
            } else if expected.scope_revision != actual.scope_revision {
                ErrorSubcode::ScopeRevision
            } else {
                ErrorSubcode::QueryFingerprint
            };
            RuntimeFailure::stale_cursor(subcode)
        }
        SearchEvidenceError::Cancelled => RuntimeFailure::from_error(
            ErrorSubcode::ClientCancelled,
            "search",
            "search request cancelled",
        ),
        SearchEvidenceError::Deadline => RuntimeFailure::from_error(
            ErrorSubcode::RequestDeadline,
            "search",
            "search deadline expired",
        ),
    }
}

fn map_search_cursor_error(error: SearchCursorError) -> RuntimeFailure {
    match error {
        SearchCursorError::Malformed => {
            RuntimeFailure::cursor_error(ErrorSubcode::QuerySyntax, "malformed")
        }
        SearchCursorError::QueryFingerprint => {
            RuntimeFailure::stale_cursor(ErrorSubcode::QueryFingerprint)
        }
        SearchCursorError::InvalidConfiguration => {
            RuntimeFailure::from_error(ErrorSubcode::Invariant, "encode_cursor_config", error)
        }
    }
}

fn cursor_stale_subcode(cause: SearchCursorStaleCause) -> ErrorSubcode {
    match cause {
        SearchCursorStaleCause::IndexEpoch => ErrorSubcode::IndexEpoch,
        SearchCursorStaleCause::Generation => ErrorSubcode::Generation,
        SearchCursorStaleCause::ScopeRevision => ErrorSubcode::ScopeRevision,
        SearchCursorStaleCause::QueryFingerprint => ErrorSubcode::QueryFingerprint,
    }
}

fn search_snapshot_from_cursor(state: &SearchCursorState) -> SearchEvidenceSnapshot {
    SearchEvidenceSnapshot {
        index_id: state.index_id,
        scope_revision: state.scope_revision,
        index_epoch: state.index_epoch,
        generation: state.generation,
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
        SearchError::Store(error) => store_subcode(error),
        SearchError::ExactMatcher => ErrorSubcode::Invariant,
    };
    RuntimeFailure::from_error(subcode, "search", error)
}

fn map_get_evidence_error(error: GetEvidenceError) -> RuntimeFailure {
    match error {
        GetEvidenceError::Store(error) => map_store_error(error),
        GetEvidenceError::Get(error) => map_get_error(error),
        GetEvidenceError::Status(error) => map_status_error(error),
        GetEvidenceError::Sqlite(error) => {
            RuntimeFailure::from_error(sqlite_subcode(&error), "get", error)
        }
        GetEvidenceError::SourceConfig(_)
        | GetEvidenceError::FieldLimit
        | GetEvidenceError::SourceUnavailable => {
            RuntimeFailure::from_error(ErrorSubcode::HeadIndexMismatch, "get", error)
        }
    }
}

fn map_get_packet_error(error: GetPacketError) -> RuntimeFailure {
    RuntimeFailure::from_error(ErrorSubcode::Invariant, "get", error)
}

fn map_get_error(error: GetError) -> RuntimeFailure {
    let subcode = match &error {
        GetError::ScopeDenied => ErrorSubcode::ScopeHidden,
        GetError::EvidenceNotFound => ErrorSubcode::EvidenceNotFound,
        GetError::EvidenceForgotten => ErrorSubcode::ForgetTombstone,
        GetError::InvalidBound { .. } | GetError::BoundBelowPassage { .. } => {
            ErrorSubcode::LimitOutOfRange
        }
        GetError::Corrupt(_) => ErrorSubcode::HeadIndexMismatch,
        GetError::Sqlite(error) => sqlite_subcode(error),
        GetError::Store(error) => store_subcode(error),
    };
    RuntimeFailure::from_error(subcode, "get", error)
}

fn map_status_evidence_error(error: StatusEvidenceError) -> RuntimeFailure {
    match error {
        StatusEvidenceError::Store(error) => map_store_error(error),
        StatusEvidenceError::Status(error) => map_status_error(error),
        StatusEvidenceError::Sqlite(error) => {
            RuntimeFailure::from_error(sqlite_subcode(&error), "status", error)
        }
        StatusEvidenceError::ProjectNotFound => RuntimeFailure::from_error(
            ErrorSubcode::ProjectNotFound,
            "status",
            "bound project does not exist",
        ),
        StatusEvidenceError::FieldLimit => RuntimeFailure::from_error(
            ErrorSubcode::MemoryBudget,
            "status",
            "stored status exceeds field limits",
        ),
        error @ StatusEvidenceError::Corrupt(_) => {
            RuntimeFailure::from_error(ErrorSubcode::HeadIndexMismatch, "status", error)
        }
        StatusEvidenceError::Cancelled => RuntimeFailure::from_error(
            ErrorSubcode::ClientCancelled,
            "status",
            "status request cancelled",
        ),
        StatusEvidenceError::Deadline => RuntimeFailure::from_error(
            ErrorSubcode::RequestDeadline,
            "status",
            "status deadline expired",
        ),
    }
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

fn map_maintenance_error(error: MaintenanceError) -> RuntimeFailure {
    match error {
        MaintenanceError::Store(error) => map_store_error(error),
        error @ MaintenanceError::StoragePreflight(_) => {
            let MaintenanceError::StoragePreflight(source) = &error else {
                unreachable!()
            };
            RuntimeFailure::from_error(storage_preflight_subcode(source), "maintenance", error)
        }
        error @ MaintenanceError::Io(_) => {
            let MaintenanceError::Io(source) = &error else {
                unreachable!()
            };
            RuntimeFailure::from_error(
                io_subcode(source, ErrorSubcode::IndexWrite),
                "maintenance",
                error,
            )
        }
        error => {
            let subcode = match &error {
                MaintenanceError::Time(_) | MaintenanceError::Json(_) => ErrorSubcode::Unexpected,
                MaintenanceError::OutputExists(_) => ErrorSubcode::MaintenanceOutputExists,
                MaintenanceError::OutputHasNoParent(_)
                | MaintenanceError::BackupOverlapsIndex
                | MaintenanceError::UnsafePlanFile(_)
                | MaintenanceError::ReplacementPathMismatch
                | MaintenanceError::OutputPathsOverlap => ErrorSubcode::PathInvalid,
                MaintenanceError::BackupIdentityMismatch
                | MaintenanceError::RecoveryBackupMismatch => {
                    ErrorSubcode::MaintenanceBackupMismatch
                }
                MaintenanceError::BackupSidecarPresent => ErrorSubcode::UnsupportedStorage,
                MaintenanceError::PlanTooLarge => ErrorSubcode::MemoryBudget,
                MaintenanceError::PlanFormat
                | MaintenanceError::PlanHashInvalid
                | MaintenanceError::PlanIndexMismatch => ErrorSubcode::MaintenancePlanInvalid,
                MaintenanceError::ConfirmationMismatch => {
                    ErrorSubcode::MaintenanceConfirmationMismatch
                }
                MaintenanceError::PlanStale => ErrorSubcode::MaintenancePlanStale,
                MaintenanceError::RestoreStateMismatch => ErrorSubcode::RestoreStateMismatch,
                MaintenanceError::NoMaintenanceWork => ErrorSubcode::MaintenanceNoWork,
                MaintenanceError::HistoryNotPruned => ErrorSubcode::ForgetRequiresPrune,
                MaintenanceError::InvalidPruneSelector => ErrorSubcode::PruneSelectorInvalid,
                MaintenanceError::ForgetTargetUnavailable => ErrorSubcode::EvidenceNotFound,
                MaintenanceError::UnsupportedMigrationPath => ErrorSubcode::UpgradeRequired,
                MaintenanceError::InvalidStoredIdentity
                | MaintenanceError::InvalidStoredDigest
                | MaintenanceError::InvalidStoredSpan => ErrorSubcode::Invariant,
                MaintenanceError::Store(_)
                | MaintenanceError::StoragePreflight(_)
                | MaintenanceError::Io(_) => unreachable!(),
            };
            RuntimeFailure::from_error(subcode, "maintenance", error)
        }
    }
}

fn map_managed_backup_error(error: ManagedBackupError) -> RuntimeFailure {
    match error {
        ManagedBackupError::Store(error) => map_store_error(error),
        error @ ManagedBackupError::Io(_) => {
            let ManagedBackupError::Io(source) = &error else {
                unreachable!()
            };
            RuntimeFailure::from_error(
                io_subcode(source, ErrorSubcode::IndexWrite),
                "managed_backup",
                error,
            )
        }
        error => {
            let subcode = match &error {
                ManagedBackupError::RegistryPathInvalid(_)
                | ManagedBackupError::BackupPathInvalid(_) => ErrorSubcode::PathInvalid,
                ManagedBackupError::UnsafeRegistry(_) | ManagedBackupError::UnsafeBackup(_) => {
                    ErrorSubcode::UnsupportedStorage
                }
                ManagedBackupError::RegistryTooLarge
                | ManagedBackupError::RegistryTooManyEntries => ErrorSubcode::MemoryBudget,
                ManagedBackupError::PendingBackup(_)
                | ManagedBackupError::BackupChanged(_)
                | ManagedBackupError::BackupPathAlreadyManaged(_)
                | ManagedBackupError::ReservationMissing(_)
                | ManagedBackupError::ReceiptMismatch(_) => ErrorSubcode::MaintenanceBackupMismatch,
                ManagedBackupError::Json(_)
                | ManagedBackupError::RegistryFormat
                | ManagedBackupError::RegistryOrder
                | ManagedBackupError::RegistryIndexName
                | ManagedBackupError::RegistryChanged
                | ManagedBackupError::RegistryEpochOverflow
                | ManagedBackupError::PathEncoding => ErrorSubcode::ConfigInvalid,
                ManagedBackupError::Store(_) | ManagedBackupError::Io(_) => unreachable!(),
            };
            RuntimeFailure::from_error(subcode, "managed_backup", error)
        }
    }
}

fn map_config_migration_error(error: ConfigMigrationError) -> RuntimeFailure {
    match error {
        ConfigMigrationError::Maintenance(error) => map_maintenance_error(error),
        ConfigMigrationError::Store(error) => map_store_error(error),
        ConfigMigrationError::StoragePreflight(error) => {
            let subcode = storage_preflight_subcode(&error);
            RuntimeFailure::from_error(subcode, "config_migration", error)
        }
        ConfigMigrationError::Trust(error) => {
            let subcode = trust_subcode(&error);
            RuntimeFailure::from_error(subcode, "config_migration", error)
        }
        error => {
            let subcode = match &error {
                ConfigMigrationError::Toml(_)
                | ConfigMigrationError::InvalidConfigEpoch(_)
                | ConfigMigrationError::IncompleteConfiguredDefault
                | ConfigMigrationError::LogicalSelection(_) => ErrorSubcode::ConfigInvalid,
                ConfigMigrationError::TomlSerialize(_) => ErrorSubcode::Invariant,
                ConfigMigrationError::Json(_) => ErrorSubcode::Unexpected,
                ConfigMigrationError::Io(source) => io_subcode(source, ErrorSubcode::IndexWrite),
                ConfigMigrationError::PathsMustBeAbsolute
                | ConfigMigrationError::PathsOverlap
                | ConfigMigrationError::PathHasNoParent(_)
                | ConfigMigrationError::UnsafeFile(_)
                | ConfigMigrationError::UnsafeBackupDirectory(_) => ErrorSubcode::PathInvalid,
                ConfigMigrationError::PlanPathMismatch | ConfigMigrationError::PlanInvalid => {
                    ErrorSubcode::MaintenancePlanInvalid
                }
                ConfigMigrationError::PlanStale => ErrorSubcode::MaintenancePlanStale,
                ConfigMigrationError::BackupMismatch => ErrorSubcode::MaintenanceBackupMismatch,
                ConfigMigrationError::FileTooLarge(_) => ErrorSubcode::FileTooLarge,
                ConfigMigrationError::FileChanged(_) => ErrorSubcode::SourceChangedDuringRead,
                ConfigMigrationError::NotUtf8(_) => ErrorSubcode::InvalidUtf8,
                ConfigMigrationError::UnsupportedSchema { found, current, .. } => {
                    schema_subcode(*found, *current, current.saturating_sub(1))
                }
                ConfigMigrationError::Maintenance(_)
                | ConfigMigrationError::Store(_)
                | ConfigMigrationError::StoragePreflight(_)
                | ConfigMigrationError::Trust(_) => unreachable!(),
            };
            RuntimeFailure::from_error(subcode, "config_migration", error)
        }
    }
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
        StoreError::WriterLockBusy { .. } | StoreError::ReplacementLockBusy { .. } => {
            ErrorSubcode::WriterLock
        }
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
        | StoreError::InvalidSchemaVersion(_)
        | StoreError::InvalidMetadata(_) => ErrorSubcode::SchemaChecksum,
        StoreError::PipelineFingerprintMismatch => ErrorSubcode::PipelineFingerprint,
        StoreError::IntegrityCheckFailed(_) => ErrorSubcode::SqliteCorrupt,
        StoreError::ForeignKeyCheckFailed
        | StoreError::ActiveIndexParity(_)
        | StoreError::ImmutableEvidenceMismatch(_)
        | StoreError::GenerationInvariant(_)
        | StoreError::ChunkLayoutMismatch
        | StoreError::ScopeConflict => ErrorSubcode::HeadIndexMismatch,
        StoreError::ForgetTombstone => ErrorSubcode::ForgetTombstone,
        StoreError::ForgetLedgerMismatch => ErrorSubcode::ForgetLedgerMismatch,
        StoreError::ProjectNotFound => ErrorSubcode::ProjectNotFound,
        StoreError::ProjectLimitExceeded { .. } => ErrorSubcode::ConfigInvalid,
        StoreError::SourceConflict
        | StoreError::SourceNotFound
        | StoreError::SourceLimitExceeded { .. }
        | StoreError::UnsupportedSourceKind => ErrorSubcode::ConfigInvalid,
        StoreError::UnsafeWriterLock(_)
        | StoreError::WriterLockUnsupported
        | StoreError::UnsafeReplacementLock(_)
        | StoreError::ReplacementLockUnsupported => ErrorSubcode::UnsupportedStorage,
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
        | StoreError::WriterLockMismatch
        | StoreError::ReplacementLockMismatch => ErrorSubcode::Invariant,
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

fn map_integration_error(error: IntegrationError) -> RuntimeFailure {
    let subcode = match &error {
        IntegrationError::CodexNotFound => ErrorSubcode::PathInvalid,
        IntegrationError::Io(source) if source.kind() == io::ErrorKind::NotFound => {
            ErrorSubcode::PathInvalid
        }
        IntegrationError::Io(_) => ErrorSubcode::Unexpected,
        IntegrationError::RegistrationConflict
        | IntegrationError::InspectFailed { .. }
        | IntegrationError::InvalidRegistration(_)
        | IntegrationError::UnsupportedRegistration(_)
        | IntegrationError::CommandFailed { .. }
        | IntegrationError::VerificationFailed => ErrorSubcode::ConfigInvalid,
        IntegrationError::OutputTooLarge => ErrorSubcode::MemoryBudget,
    };
    RuntimeFailure::from_error(subcode, "codex_integration", error)
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
            PublicError::from_subcode(ErrorSubcode::DiskSpace, "request").code,
            ErrorCode::ResourceExhausted
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::SqliteIo, "request").code,
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
    fn maintenance_failures_have_operation_specific_recovery_subcodes() {
        for (error, expected) in [
            (
                MaintenanceError::OutputExists(PathBuf::from("plan.json")),
                ErrorSubcode::MaintenanceOutputExists,
            ),
            (
                MaintenanceError::PlanHashInvalid,
                ErrorSubcode::MaintenancePlanInvalid,
            ),
            (
                MaintenanceError::ConfirmationMismatch,
                ErrorSubcode::MaintenanceConfirmationMismatch,
            ),
            (
                MaintenanceError::PlanStale,
                ErrorSubcode::MaintenancePlanStale,
            ),
            (
                MaintenanceError::NoMaintenanceWork,
                ErrorSubcode::MaintenanceNoWork,
            ),
            (
                MaintenanceError::HistoryNotPruned,
                ErrorSubcode::ForgetRequiresPrune,
            ),
            (
                MaintenanceError::RestoreStateMismatch,
                ErrorSubcode::RestoreStateMismatch,
            ),
            (
                MaintenanceError::BackupIdentityMismatch,
                ErrorSubcode::MaintenanceBackupMismatch,
            ),
            (
                MaintenanceError::InvalidPruneSelector,
                ErrorSubcode::PruneSelectorInvalid,
            ),
        ] {
            assert_eq!(map_maintenance_error(error).public.subcode, expected);
        }
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
