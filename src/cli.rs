use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use clap::{
    ArgAction, ArgGroup, Args, CommandFactory, Parser, Subcommand, ValueEnum, value_parser,
};
use clap_complete::aot::generate;

use crate::config::BindingId;
use crate::domain::{Citation, SafeSlug, Sha256Digest};
use crate::protocol::API_VERSION as MCP_API_VERSION;
use crate::store::SCHEMA_VERSION;

pub use clap_complete::aot::Shell as CompletionShell;

const LOCK_TIMEOUT_MAX_MS: u64 = 60_000;
const SEARCH_TIMEOUT_MIN_MS: u64 = 100;
const SEARCH_TIMEOUT_MAX_MS: u64 = 10_000;

/// The complete command-line contract available in this checkout.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "hsum",
    bin_name = "hsum",
    about = "Local evidence for agents.",
    version,
    propagate_version = true,
    disable_help_subcommand = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Options shared by every command.
    #[command(flatten)]
    pub global: GlobalOptions,

    /// The requested hSUM operation.
    #[command(subcommand)]
    pub command: Command,
}

/// Options with consistent behavior across all hSUM commands.
#[derive(Clone, Debug, Default, Args)]
pub struct GlobalOptions {
    /// Increase diagnostic detail; repeat for more detail.
    #[arg(long, short = 'v', global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Suppress progress while preserving errors and requested results.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Disable progress while preserving warnings and lifecycle summaries.
    #[arg(long, global = true)]
    pub no_progress: bool,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Use an explicit configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Override the managed data directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,

    /// Override the managed cache directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub cache_dir: Option<PathBuf>,
}

/// Current commands. Later release-train commands do not appear as stubs.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Create one local filesystem-backed index and project.
    Init(InitArgs),

    /// Authorize a canonical repository root for direct and MCP use.
    Trust(TrustArgs),

    /// Refresh configured sources into one atomic generation.
    Ingest(IngestArgs),

    /// Configure and manage sources in the selected managed index and project.
    Source(SourceArgs),

    /// Create, inspect, select, or retarget projects in one managed index.
    Project(ProjectArgs),

    /// Manage complete indexes by explicit logical name.
    Index(IndexArgs),

    /// Search exact identifiers, conservative quotes, and active lexical text.
    Search(SearchArgs),

    /// Resolve immutable bytes from an hsum://v1 citation.
    Get(GetArgs),

    /// Show index state and actionable problems.
    Status(StatusArgs),

    /// Diagnose, narrowly repair, or export a body-free report for the selected index.
    Doctor(DoctorArgs),

    /// Create or inventory verified, self-contained SQLite backups.
    Backup(BackupArgs),

    /// Plan or apply citation-aware history pruning.
    Prune(PruneArgs),

    /// Plan or apply durable citation-namespace forgetting.
    Forget(ForgetArgs),

    /// Apply the exact guarded restore plan emitted by forget.
    Restore(RestoreArgs),

    /// Plan or apply a released N-1 schema migration.
    Migrate(MigrateArgs),

    /// Inspect or migrate user configuration and trust-registry schemas.
    Config(ConfigArgs),

    /// Show effective index, project, root, binding, and configuration origins.
    Context(ContextArgs),

    /// Show offline guidance for a stable hSUM error subcode.
    Help(HelpArgs),

    /// Generate or diagnose agent-client configuration.
    Client(ClientArgs),

    /// Install, activate, inspect, repair, or remove an agent integration.
    Integration(IntegrationArgs),

    /// Install, import, inspect, verify, or remove pinned local model artifacts.
    Model(ModelArgs),

    /// Generate shell completion content on stdout.
    Completions(CompletionsArgs),

    /// Generate the hsum manual page on stdout.
    Man(ManArgs),

    /// Start a project-bound, read-only MCP stdio server.
    Mcp(McpArgs),
}

/// Arguments for `hsum init`.
#[derive(Clone, Debug, Args)]
pub struct InitArgs {
    /// Filesystem root; defaults to the enclosing Git root or current directory.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Collision-safe logical index name.
    #[arg(long, value_name = "NAME")]
    pub index: Option<SafeSlug>,

    /// Collision-safe logical project name.
    #[arg(long, value_name = "NAME")]
    pub project: Option<SafeSlug>,

    /// Replace the trusted index for this root and re-ingest from scratch.
    #[arg(long, conflicts_with = "no_ingest")]
    pub rebuild: bool,

    /// Create configuration and trust state without running the first ingest.
    #[arg(long)]
    pub no_ingest: bool,

    /// Inspect selection and corpus estimates without writing.
    #[arg(long)]
    pub dry_run: bool,

    /// Write the optional repository pointer.
    #[arg(long)]
    pub write_pointer: bool,

    /// Replace a conflicting repository pointer.
    #[arg(long, requires = "write_pointer")]
    pub force_pointer: bool,

    /// Confirm selection of a normally refused broad filesystem root.
    #[arg(long)]
    pub allow_broad_root: bool,

    /// Confirm a source above the default file-count or byte limits.
    #[arg(long)]
    pub allow_large_source: bool,

    /// Maximum managed index bytes, including the required recovery reserve.
    #[arg(
        long,
        value_name = "BYTES",
        value_parser = value_parser!(u64).range(1..)
    )]
    pub index_quota_bytes: Option<u64>,
}

/// Arguments for `hsum trust`.
#[derive(Clone, Debug, Args)]
pub struct TrustArgs {
    /// Repository root to canonicalize and authorize.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Explicit logical index name.
    #[arg(long, value_name = "NAME")]
    pub index: Option<SafeSlug>,

    /// Explicit logical project name.
    #[arg(long, value_name = "NAME")]
    pub project: Option<SafeSlug>,

    /// Confirm creation of the user-side trust binding.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum ingest`.
#[derive(Clone, Debug, Args)]
pub struct IngestArgs {
    /// Discover, validate, and estimate changes without committing them.
    #[arg(long)]
    pub dry_run: bool,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,

    /// Confirm replacing a previously nonempty source with an empty snapshot.
    #[arg(long)]
    pub allow_empty_snapshot: bool,

    /// Confirm an authoritative deletion above the mass-delete budget.
    #[arg(long)]
    pub allow_mass_delete: bool,

    /// Limit ingest to one source UUID or name; repeat to target several sources.
    #[arg(long, value_name = "ID_OR_NAME", action = ArgAction::Append)]
    pub source: Vec<String>,

    /// Abort the complete index-wide ingest if any targeted source fails.
    #[arg(long)]
    pub strict: bool,
}

/// Arguments for `hsum source`.
#[derive(Clone, Debug, Args)]
pub struct SourceArgs {
    #[command(subcommand)]
    pub command: SourceCommand,
}

/// Source-management operations available in the current checkout.
#[derive(Clone, Debug, Subcommand)]
pub enum SourceCommand {
    /// Configure a source in the selected managed index.
    Add(SourceAddArgs),

    /// List the selected project's configured sources.
    List(SourceListArgs),

    /// Attach an existing JSONL source to the selected project.
    Attach(SourceAttachArgs),

    /// Detach a JSONL source from only the selected project.
    Detach(SourceDetachArgs),

    /// Tombstone and detach a JSONL source.
    Remove(SourceRemoveArgs),
}

/// Arguments for `hsum source add`.
#[derive(Clone, Debug, Args)]
pub struct SourceAddArgs {
    #[command(subcommand)]
    pub connector: SourceConnectorCommand,
}

/// Concrete source connectors. A shared plugin abstraction remains deferred.
#[derive(Clone, Debug, Subcommand)]
pub enum SourceConnectorCommand {
    /// Register a canonical filesystem source without attaching or ingesting it.
    Fs(FilesystemSourceAddArgs),

    /// Add a strict UTF-8 JSONL complete-snapshot source.
    Jsonl(JsonlSourceAddArgs),
}

/// Arguments for `hsum source add fs`.
#[derive(Clone, Debug, Args)]
pub struct FilesystemSourceAddArgs {
    /// Filesystem root to canonicalize and configure.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Stable source name, unique within the index.
    #[arg(long, value_name = "NAME")]
    pub name: SafeSlug,

    /// Confirm selection of a normally refused broad filesystem root.
    #[arg(long)]
    pub allow_broad_root: bool,
}

/// Arguments for `hsum source add jsonl`.
#[derive(Clone, Debug, Args)]
pub struct JsonlSourceAddArgs {
    /// JSONL snapshot file to canonicalize and configure.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Stable source name, unique within the index.
    #[arg(long, value_name = "NAME")]
    pub name: SafeSlug,
}

/// Arguments for `hsum source list`.
#[derive(Clone, Debug, Default, Args)]
pub struct SourceListArgs {
    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,

    /// Include active sources that are not attached to the selected project.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `hsum source attach`.
#[derive(Clone, Debug, Args)]
pub struct SourceAttachArgs {
    /// Existing source UUID or exact source name.
    #[arg(value_name = "ID_OR_NAME")]
    pub source: String,

    /// Confirm changing the selected project's evidence scope.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum source detach`.
#[derive(Clone, Debug, Args)]
pub struct SourceDetachArgs {
    /// Attached source UUID or exact source name.
    #[arg(value_name = "ID_OR_NAME")]
    pub source: String,

    /// Confirm changing the selected project's evidence scope.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum source remove`.
#[derive(Clone, Debug, Args)]
pub struct SourceRemoveArgs {
    /// Source UUID or exact source name.
    #[arg(value_name = "ID_OR_NAME")]
    pub source: String,

    /// Confirm tombstoning active heads and detaching the source.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum project`.
#[derive(Clone, Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommand,
}

/// Project-management operations available in the current checkout.
#[derive(Clone, Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project sharing the selected filesystem authority.
    Create(ProjectCreateArgs),

    /// List projects in the selected managed index.
    List(ProjectListArgs),

    /// Persistently select a project for the current trust binding.
    Use(ProjectUseArgs),

    /// Replace the selected project's filesystem authority.
    SetRoot(ProjectSetRootArgs),
}

/// Arguments for `hsum project create`.
#[derive(Clone, Debug, Args)]
pub struct ProjectCreateArgs {
    /// Project name, unique within the managed index.
    #[arg(value_name = "NAME")]
    pub name: SafeSlug,
}

/// Arguments for `hsum project list`.
#[derive(Clone, Debug, Default, Args)]
pub struct ProjectListArgs {
    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `hsum project use`.
#[derive(Clone, Debug, Args)]
pub struct ProjectUseArgs {
    /// Project UUID or exact project name.
    #[arg(value_name = "ID_OR_NAME")]
    pub project: String,

    /// Confirm changing the persistent trust binding.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum project set-root`.
#[derive(Clone, Debug, Args)]
pub struct ProjectSetRootArgs {
    /// New filesystem root to canonicalize and configure.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Optional globally unique name for a newly created filesystem source.
    #[arg(long, value_name = "NAME")]
    pub source_name: Option<SafeSlug>,

    /// Confirm replacing the selected project's filesystem authority.
    #[arg(long, required = true)]
    pub confirm: bool,

    /// Confirm selection of a normally refused broad filesystem root.
    #[arg(long)]
    pub allow_broad_root: bool,
}

/// Arguments for `hsum index`.
#[derive(Clone, Debug, Args)]
pub struct IndexArgs {
    #[command(subcommand)]
    pub command: IndexCommand,
}

/// Whole-index operations available in the current checkout.
#[derive(Clone, Debug, Subcommand)]
pub enum IndexCommand {
    /// Remove one named managed index and all of its managed evidence.
    Delete(IndexDeleteArgs),
}

/// Arguments for `hsum index delete`.
#[derive(Clone, Debug, Args)]
pub struct IndexDeleteArgs {
    /// Exact logical name of the managed index to remove.
    #[arg(value_name = "NAME")]
    pub name: SafeSlug,

    /// Confirm permanent removal of the complete managed index.
    #[arg(long, required = true)]
    pub confirm: bool,

    /// Maximum wait for active index readers and writers.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Retrieval modes available in alpha.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SearchMode {
    /// Exact identifiers and quotes plus lexical BM25.
    Auto,
    /// Exact identifiers and quotes plus lexical BM25.
    Lexical,
}

/// Arguments for `hsum search`.
#[derive(Clone, Debug, Args)]
pub struct SearchArgs {
    /// Query text.
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// Retrieval mode; alpha.4 auto and lexical are equivalent.
    #[arg(long, value_enum, default_value = "auto")]
    pub mode: SearchMode,

    /// Maximum number of returned passages.
    #[arg(
        long,
        default_value = "10",
        value_parser = value_parser!(u8).range(1..=50)
    )]
    pub limit: u8,

    /// Continue a stable, scope-bound result page.
    #[arg(long, value_name = "VALUE")]
    pub cursor: Option<String>,

    /// End bounded retrieval work after this deadline.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "3000",
        value_parser = value_parser!(u64)
            .range(SEARCH_TIMEOUT_MIN_MS..=SEARCH_TIMEOUT_MAX_MS)
    )]
    pub timeout_ms: u64,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,

    /// Include detailed retriever scoring and stop information.
    #[arg(long)]
    pub explain: bool,
}

/// Arguments for `hsum get`.
#[derive(Clone, Debug, Args)]
pub struct GetArgs {
    /// Canonical immutable hsum://v1 citation.
    #[arg(value_name = "CITATION_URI")]
    pub citation_uri: Citation,

    /// Maximum number of returned UTF-8 bytes.
    #[arg(
        long,
        default_value = "16384",
        value_parser = value_parser!(u32).range(1..=65_536)
    )]
    pub max_bytes: u32,

    /// Probe the current source hash after resolving the stored revision.
    #[arg(long)]
    pub verify_source_hash: bool,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `hsum status`.
#[derive(Clone, Debug, Args)]
pub struct StatusArgs {
    /// Filter the report to actionable problems without hiding partial state.
    #[arg(long)]
    pub problems: bool,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `hsum doctor`.
#[derive(Clone, Debug, Default, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct DoctorArgs {
    /// Explicitly request the full SQLite, history, evidence, and derived-index scan.
    #[arg(long)]
    pub integrity: bool,

    /// Remove only abandoned generation rows after full pre/post validation.
    #[arg(long, requires = "confirm")]
    pub repair: bool,

    /// Confirm the narrowly bounded abandoned-generation repair.
    #[arg(long, requires = "repair")]
    pub confirm: bool,

    /// Maximum time to wait for the writer lock during repair.
    #[arg(
        long,
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,

    #[command(subcommand)]
    pub command: Option<DoctorCommand>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum DoctorCommand {
    /// Write a local body-free, query-free support report without overwriting a path.
    Report(DoctorReportArgs),
}

#[derive(Clone, Debug, Args)]
pub struct DoctorReportArgs {
    /// New private JSON report path.
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,
}

/// Arguments for `hsum backup`.
#[derive(Clone, Debug, Args)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub command: BackupCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum BackupCommand {
    /// Create and verify a new backup without overwriting any path.
    Create(BackupCreateArgs),

    /// Inventory hSUM-managed backups and their current verification state.
    List(BackupListArgs),
}

#[derive(Clone, Debug, Args)]
pub struct BackupCreateArgs {
    /// New SQLite backup path.
    #[arg(value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Confirm the capacity-consuming backup operation.
    #[arg(long, required = true)]
    pub confirm: bool,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Args)]
pub struct BackupListArgs {
    /// List backups for every known index, including deleted indexes.
    #[arg(long)]
    pub all: bool,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,

    /// Maximum wait for the managed-backup registry lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum prune`.
#[derive(Clone, Debug, Args)]
pub struct PruneArgs {
    #[command(subcommand)]
    pub command: PruneCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum PruneCommand {
    /// Write an immutable citation-impact manifest without deleting data.
    Plan(PrunePlanArgs),

    /// Revalidate and apply an exact prune plan after creating a backup.
    Apply(PruneApplyArgs),
}

#[derive(Clone, Debug, Args)]
pub struct PrunePlanArgs {
    /// New JSON plan path.
    #[arg(value_name = "PLAN")]
    pub output: PathBuf,

    /// Prune revisions indexed before this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339", required = true)]
    pub before: String,

    /// Keep at least the newest N revisions of every logical document.
    #[arg(
        long,
        value_name = "COUNT",
        default_value = "1",
        value_parser = value_parser!(u64).range(1..)
    )]
    pub keep_latest: u64,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Args)]
pub struct PruneApplyArgs {
    /// Reviewed JSON plan path.
    #[arg(value_name = "PLAN")]
    pub plan: PathBuf,

    /// New pre-prune SQLite backup path.
    #[arg(long, value_name = "OUTPUT", required = true)]
    pub backup: PathBuf,

    /// Exact SHA-256 printed by `prune plan`.
    #[arg(long, value_name = "PLAN_HASH", required = true)]
    pub confirm: Sha256Digest,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum forget`.
#[derive(Clone, Debug, Args)]
pub struct ForgetArgs {
    #[command(subcommand)]
    pub command: ForgetCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ForgetCommand {
    /// Write an immutable plan for active revision namespaces.
    Plan(ForgetPlanArgs),

    /// Create recovery artifacts and atomically publish a compacted tombstone index.
    Apply(ForgetApplyArgs),
}

#[derive(Clone, Debug, Args)]
pub struct ForgetPlanArgs {
    /// New JSON plan path.
    #[arg(value_name = "PLAN")]
    pub output: PathBuf,

    /// Citation selecting a whole immutable revision namespace; repeat as needed.
    #[arg(long, value_name = "CITATION", required = true, action = ArgAction::Append)]
    pub citation: Vec<Citation>,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Args)]
#[command(group(
    ArgGroup::new("managed_backup_disposition")
        .required(true)
        .args(["purge_managed_backups", "keep_managed_backups"])
))]
pub struct ForgetApplyArgs {
    /// Reviewed JSON forget plan path.
    #[arg(value_name = "PLAN")]
    pub plan: PathBuf,

    /// New evidence-bearing pre-forget recovery backup path.
    #[arg(long, value_name = "OUTPUT", required = true)]
    pub recovery_backup: PathBuf,

    /// New restore plan path bound to the finalized forgotten index.
    #[arg(long, value_name = "OUTPUT", required = true)]
    pub restore_plan: PathBuf,

    /// Delete every unchanged hSUM-managed backup for this index after forgetting.
    #[arg(long)]
    pub purge_managed_backups: bool,

    /// Retain and report every hSUM-managed backup that may still contain evidence.
    #[arg(long)]
    pub keep_managed_backups: bool,

    /// Exact SHA-256 printed by `forget plan`.
    #[arg(long, value_name = "PLAN_HASH", required = true)]
    pub confirm: Sha256Digest,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum restore`.
#[derive(Clone, Debug, Args)]
pub struct RestoreArgs {
    #[command(subcommand)]
    pub command: RestoreCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum RestoreCommand {
    /// Restore only the exact, unchanged post-forget state named by a restore plan.
    Apply(RestoreApplyArgs),
}

#[derive(Clone, Debug, Args)]
pub struct RestoreApplyArgs {
    /// Restore plan emitted by `forget apply`.
    #[arg(value_name = "PLAN")]
    pub plan: PathBuf,

    /// Evidence-bearing recovery backup named by the restore plan hash.
    #[arg(long, value_name = "BACKUP", required = true)]
    pub recovery_backup: PathBuf,

    /// New backup of the forgotten state, created before restoration.
    #[arg(long, value_name = "OUTPUT", required = true)]
    pub safety_backup: PathBuf,

    /// Exact SHA-256 printed by `forget apply` for the restore plan.
    #[arg(long, value_name = "PLAN_HASH", required = true)]
    pub confirm: Sha256Digest,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum migrate`.
#[derive(Clone, Debug, Args)]
pub struct MigrateArgs {
    #[command(subcommand)]
    pub command: MigrateCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum MigrateCommand {
    /// Diagnose a managed index and write an immutable migration plan.
    Plan(MigratePlanArgs),

    /// Revalidate and apply an exact migration plan after creating a backup.
    Apply(MigrateApplyArgs),
}

#[derive(Clone, Debug, Args)]
pub struct MigratePlanArgs {
    /// Managed index name; required because older schemas cannot resolve context.
    #[arg(long, value_name = "NAME", required = true)]
    pub index: SafeSlug,

    /// New JSON plan path.
    #[arg(value_name = "PLAN")]
    pub output: PathBuf,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Args)]
pub struct MigrateApplyArgs {
    /// Managed index name named in the reviewed plan.
    #[arg(long, value_name = "NAME", required = true)]
    pub index: SafeSlug,

    /// Reviewed JSON plan path.
    #[arg(value_name = "PLAN")]
    pub plan: PathBuf,

    /// New pre-migration SQLite backup path.
    #[arg(long, value_name = "OUTPUT", required = true)]
    pub backup: PathBuf,

    /// Exact SHA-256 printed by `migrate plan`.
    #[arg(long, value_name = "PLAN_HASH", required = true)]
    pub confirm: Sha256Digest,

    /// Maximum wait for the single-writer advisory lock.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum config`.
#[derive(Clone, Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigCommand {
    /// Plan a current-plus-N-1 configuration migration without changing files.
    Migrate(ConfigMigrateArgs),
}

#[derive(Clone, Debug, Args)]
pub struct ConfigMigrateArgs {
    #[command(subcommand)]
    pub command: ConfigMigrateCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigMigrateCommand {
    /// Diagnose configuration files and write an immutable migration plan.
    Plan(ConfigMigratePlanArgs),

    /// Revalidate and apply an exact plan after creating private backups.
    Apply(ConfigMigrateApplyArgs),
}

#[derive(Clone, Debug, Args)]
pub struct ConfigMigratePlanArgs {
    /// New JSON plan path.
    #[arg(value_name = "PLAN")]
    pub output: PathBuf,

    /// New or resumable private directory for exact pre-migration files.
    #[arg(long, value_name = "DIRECTORY", required = true)]
    pub backup_dir: PathBuf,

    /// Maximum wait for the configuration writer locks.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Args)]
pub struct ConfigMigrateApplyArgs {
    /// Reviewed JSON plan path.
    #[arg(value_name = "PLAN")]
    pub plan: PathBuf,

    /// Exact SHA-256 printed by `config migrate plan`.
    #[arg(long, value_name = "PLAN_HASH", required = true)]
    pub confirm: Sha256Digest,

    /// Maximum wait for the configuration writer locks.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value = "5000",
        value_parser = value_parser!(u64).range(0..=LOCK_TIMEOUT_MAX_MS)
    )]
    pub lock_timeout_ms: u64,
}

/// Arguments for `hsum context`.
#[derive(Clone, Debug, Args)]
pub struct ContextArgs {
    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for hSUM's offline help catalog.
#[derive(Clone, Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct HelpArgs {
    /// Offline help topic.
    #[command(subcommand)]
    pub command: HelpCommand,
}

/// Offline help topics available in alpha.4.
#[derive(Clone, Debug, Subcommand)]
pub enum HelpCommand {
    /// Explain one stable public error subcode without opening a browser.
    Error(ErrorHelpArgs),
}

/// Arguments for `hsum help error`.
#[derive(Clone, Debug, Args)]
pub struct ErrorHelpArgs {
    /// Stable SCREAMING_SNAKE_CASE public error subcode.
    #[arg(value_name = "SUBCODE")]
    pub subcode: String,
}

/// Arguments for `hsum client`.
#[derive(Clone, Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct ClientArgs {
    /// Client operation.
    #[command(subcommand)]
    pub command: ClientCommand,
}

/// Alpha.1 client operations.
#[derive(Clone, Debug, Subcommand)]
pub enum ClientCommand {
    /// Print a copyable client configuration snippet.
    Config(ClientConfigArgs),

    /// Validate executable, binding, scope, framing, and a read-only probe.
    Doctor(ClientDoctorArgs),
}

/// Supported agent clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ClientKind {
    Codex,
    ClaudeCode,
    ClaudeDesktop,
    Generic,
}

/// Generated client configuration formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ClientConfigFormat {
    Json,
    Toml,
}

/// Arguments for `hsum client config`.
#[derive(Clone, Debug, Args)]
pub struct ClientConfigArgs {
    /// Agent client whose stdio configuration should be generated.
    #[arg(value_enum)]
    pub client: ClientKind,

    /// Serialization format for the copyable snippet.
    #[arg(long, value_enum, default_value = "json")]
    pub format: ClientConfigFormat,
}

/// Arguments for `hsum client doctor`.
#[derive(Clone, Debug, Args)]
pub struct ClientDoctorArgs {
    /// Agent client whose installed configuration should be diagnosed.
    #[arg(value_enum)]
    pub client: ClientKind,
}

/// Arguments for `hsum integration`.
#[derive(Clone, Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct IntegrationArgs {
    /// Integration lifecycle operation.
    #[command(subcommand)]
    pub command: IntegrationCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum IntegrationCommand {
    /// Register hSUM globally and optionally activate one repository.
    Install(IntegrationInstallArgs),

    /// Initialize or validate one repository for an existing integration.
    Activate(IntegrationActivateArgs),

    /// Manage a legacy alpha.4 workspace authorization record.
    #[command(hide = true)]
    AuthorizeWorkspace(IntegrationWorkspaceArgs),

    /// Remove a legacy alpha.4 workspace authorization record.
    #[command(hide = true)]
    RevokeWorkspace(IntegrationWorkspaceArgs),

    /// Inspect the effective client registration.
    Status(IntegrationStatusArgs),

    /// Replace a missing, legacy, or stale hSUM registration.
    Repair(IntegrationRepairArgs),

    /// Remove hSUM's client registration without deleting indexes.
    Uninstall(IntegrationUninstallArgs),
}

/// Agent clients with a managed integration adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum IntegrationClient {
    Codex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum AgentPolicyMode {
    Install,
    Skip,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationInstallArgs {
    /// Agent client to register.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Repository to initialize and authorize during installation.
    #[arg(long, value_name = "PATH", requires = "confirm")]
    pub activate: Option<PathBuf>,

    /// Confirm repository indexing and trust creation.
    #[arg(long)]
    pub confirm: bool,

    /// Replace a foreign Codex MCP entry named `hsum`.
    #[arg(long)]
    pub replace_conflict: bool,

    /// Install managed global guidance that teaches Codex when to use hSUM.
    #[arg(long, value_enum, default_value = "install")]
    pub agent_policy: AgentPolicyMode,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationActivateArgs {
    /// Agent client whose repository integration should be activated.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Repository to initialize and authorize.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Confirm repository indexing and trust creation.
    #[arg(long, required = true)]
    pub confirm: bool,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationWorkspaceArgs {
    /// Agent client whose legacy workspace policy should change.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Canonical parent directory containing separate Git repositories.
    #[arg(long, value_name = "PATH")]
    pub path: PathBuf,

    /// Confirm the exact legacy workspace-record change.
    #[arg(long, required = true)]
    pub confirm: bool,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationStatusArgs {
    /// Agent client whose effective registration should be inspected.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationRepairArgs {
    /// Agent client whose registration should be repaired.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Confirm mutation of the client registration.
    #[arg(long, required = true)]
    pub confirm: bool,

    /// Replace a foreign Codex MCP entry named `hsum`.
    #[arg(long)]
    pub replace_conflict: bool,
}

#[derive(Clone, Debug, Args)]
pub struct IntegrationUninstallArgs {
    /// Agent client whose hSUM registration should be removed.
    #[arg(value_enum)]
    pub client: IntegrationClient,

    /// Confirm removal of the client registration.
    #[arg(long, required = true)]
    pub confirm: bool,
}

/// Arguments for `hsum model`.
#[derive(Clone, Debug, Args)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: ModelCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ModelCommand {
    /// Explicitly download one embedded-manifest model over HTTPS.
    Install(ModelInstallArgs),

    /// Import a verified air-gapped model directory or its receipt file.
    Import(ModelImportArgs),

    /// List embedded model profiles and local installation state.
    List(ModelListArgs),

    /// Recompute every checksum for one installed model.
    Verify(ModelVerifyArgs),

    /// Remove one installed model, refusing discoverable index pins by default.
    Remove(ModelRemoveArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ModelInstallKind {
    Embedding,
}

#[derive(Clone, Debug, Args)]
pub struct ModelInstallArgs {
    /// Artifact class; Beta.1 supports local text embeddings only.
    #[arg(value_enum)]
    pub kind: ModelInstallKind,

    /// Exact model ID from `hsum model list`.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Additional PEM certificate authority for a private HTTPS root.
    #[arg(long, value_name = "PATH")]
    pub ca_bundle: Option<PathBuf>,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ModelImportArgs {
    /// Directory containing `hsum-model.json`, or that receipt file itself.
    #[arg(value_name = "ARTIFACT_OR_DIRECTORY")]
    pub artifact: PathBuf,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ModelListArgs {
    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ModelVerifyArgs {
    /// Exact model ID from `hsum model list`.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ModelRemoveArgs {
    /// Exact model ID from `hsum model list`.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Remove even when discoverable indexes pin this model.
    #[arg(long)]
    pub force: bool,

    /// Emit exactly one JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `hsum completions`.
#[derive(Clone, Debug, Args)]
pub struct CompletionsArgs {
    /// Shell completion format.
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

/// Arguments for `hsum man`.
#[derive(Clone, Debug, Default, Args)]
pub struct ManArgs {}

/// Arguments for `hsum mcp`.
#[derive(Clone, Debug, Args)]
pub struct McpArgs {
    /// Random user-side binding UUID generated by hsum trust or init.
    #[arg(long, value_name = "UUID", conflicts_with = "project")]
    pub binding: Option<BindingId>,

    /// Project selected only inside an already trusted direct-CLI context.
    #[arg(long, value_name = "NAME", conflicts_with = "binding")]
    pub project: Option<SafeSlug>,
}

/// Capabilities compiled into this binary, in stable report order.
///
/// The list is tag-gated: deferred surface such as vector modes or watch mode
/// must never appear here before its release tag.
pub const COMPILED_CAPABILITIES: [&str; 18] = [
    "filesystem-ingest",
    "filesystem-source-registration",
    "confirmed-index-deletion",
    "jsonl-snapshot",
    "multi-source-ingest",
    "multi-project",
    "search-exact",
    "search-bm25",
    "evidence-get",
    "backup-verified",
    "managed-backup-disposition",
    "prune-plan-apply",
    "forget-physical-rewrite",
    "restore-exact-state",
    "migrate-n-minus-one",
    "config-migrate-n-minus-one",
    "mcp-stdio-read-only",
    "model-artifact-lifecycle",
];

/// Detect the `hsum --version --verbose` form from the raw argument list.
///
/// Clap's version action short-circuits before global flags reach the parsed
/// structure, so the verbose request must be recovered from the arguments the
/// process actually received. Accepted spellings are `--verbose` and the
/// repeatable short form (`-v`, `-vv`, ...).
#[must_use]
pub fn verbose_version_requested<I, S>(arguments: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    arguments.into_iter().any(|argument| {
        let argument = argument.as_ref();
        argument == "--verbose"
            || argument
                .strip_prefix('-')
                .is_some_and(|short| !short.is_empty() && short.chars().all(|flag| flag == 'v'))
    })
}

/// Render the `hsum --version --verbose` report.
///
/// Every line comes from the same compiled constants that gate the alpha.4
/// surface: crate version, versioned API identifier, the schema range this
/// binary can read and write, the build-target triple, and the tag-gated
/// capability list.
#[must_use]
pub fn render_verbose_version() -> String {
    format!(
        "hsum {}\n\
         API version: {MCP_API_VERSION}\n\
         Readable schema versions: {}-{SCHEMA_VERSION}\n\
         Writable schema versions: {SCHEMA_VERSION}-{SCHEMA_VERSION}\n\
         Target: {}\n\
         Capabilities: {}\n",
        env!("CARGO_PKG_VERSION"),
        SCHEMA_VERSION - 1,
        env!("HSUM_TARGET_TRIPLE"),
        COMPILED_CAPABILITIES.join(", "),
    )
}

/// Generate one completion script from the same command graph as `--help`.
#[must_use]
pub fn render_completions(shell: CompletionShell) -> Vec<u8> {
    let mut command = Cli::command();
    let mut output = Vec::new();
    generate(shell, &mut command, "hsum", &mut output);
    output
}

/// Generate the root hsum(1) manual from the same command graph as `--help`.
pub fn render_man() -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    clap_mangen::Man::new(Cli::command()).render(&mut output)?;
    Ok(output)
}

/// Stable categories at the process boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessExitCategory {
    Success,
    Failure,
    InvalidArguments,
    DependencyUnavailable,
    BusyOrTimeout,
    CorruptOrIncompatible,
    PartialIngest,
    PermissionOrResource,
    Interrupted,
}

/// Map a public outcome category to the stable hSUM process exit code.
#[must_use]
pub const fn exit_code_for(category: ProcessExitCategory) -> u8 {
    match category {
        ProcessExitCategory::Success => 0,
        ProcessExitCategory::Failure => 1,
        ProcessExitCategory::InvalidArguments => 2,
        ProcessExitCategory::DependencyUnavailable => 3,
        ProcessExitCategory::BusyOrTimeout => 4,
        ProcessExitCategory::CorruptOrIncompatible => 5,
        ProcessExitCategory::PartialIngest => 6,
        ProcessExitCategory::PermissionOrResource => 7,
        ProcessExitCategory::Interrupted => 130,
    }
}

/// Escape untrusted UTF-8 text before including it in human terminal output.
#[must_use]
pub fn escape_terminal_text(input: &str) -> String {
    escape_terminal_bytes(input.as_bytes())
}

/// Escape untrusted path or content bytes before human terminal rendering.
///
/// Valid printable UTF-8 is retained. Invalid bytes, ANSI/OSC introducers,
/// newlines, terminal controls, bidi overrides, and invisible format controls
/// are rendered as visible ASCII escape sequences.
#[must_use]
pub fn escape_terminal_bytes(input: &[u8]) -> String {
    let mut escaped = String::with_capacity(input.len());
    let mut remaining = input;

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                escape_valid_text(text, &mut escaped);
                break;
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                let (valid, invalid_and_rest) = remaining.split_at(valid_length);
                let valid = std::str::from_utf8(valid)
                    .expect("the UTF-8 validator marks this prefix as valid");
                escape_valid_text(valid, &mut escaped);

                let invalid_length = error.error_len().unwrap_or(invalid_and_rest.len());
                let (invalid, rest) = invalid_and_rest.split_at(invalid_length);
                for byte in invalid {
                    write!(escaped, "\\x{byte:02X}").expect("formatting into a String cannot fail");
                }
                remaining = rest;
            }
        }
    }

    escaped
}

fn escape_valid_text(input: &str, output: &mut String) {
    for character in input.chars() {
        match character {
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() && u32::from(character) <= 0xff => {
                write!(output, "\\x{:02X}", u32::from(character))
                    .expect("formatting into a String cannot fail");
            }
            character if character.is_control() || is_nonprinting_terminal_character(character) => {
                write!(output, "\\u{{{:04X}}}", u32::from(character))
                    .expect("formatting into a String cannot fail");
            }
            character => output.push(character),
        }
    }
}

// The explicit ranges freeze security-sensitive Unicode 16.0
// `General_Category=Cf` and `Default_Ignorable_Code_Point` behavior. Rust's
// Unicode-versioned debug table additionally classifies unassigned scalars
// without hSUM carrying hundreds of brittle `Cn` ranges.
fn is_nonprinting_terminal_character(character: char) -> bool {
    let scalar = character as u32;
    let is_noncharacter = (0xfdd0..=0xfdef).contains(&scalar) || (scalar & 0xffff) >= 0xfffe;
    let is_private_use = matches!(
        character,
        '\u{e000}'..='\u{f8ff}'
            | '\u{f0000}'..='\u{ffffd}'
            | '\u{100000}'..='\u{10fffd}'
    );
    let is_format_or_default_ignorable = matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    );
    let is_not_rust_printable = !character.is_ascii_graphic()
        && character != ' '
        && !character.escape_debug().eq(std::iter::once(character));

    is_noncharacter || is_private_use || is_format_or_default_ignorable || is_not_rust_printable
}
