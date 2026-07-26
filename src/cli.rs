use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum, value_parser};
use clap_complete::aot::generate;

use crate::config::BindingId;
use crate::domain::{Citation, SafeSlug};
use crate::mcp::MCP_API_VERSION;
use crate::store::SCHEMA_VERSION;

pub use clap_complete::aot::Shell as CompletionShell;

const LOCK_TIMEOUT_MAX_MS: u64 = 60_000;
const SEARCH_TIMEOUT_MIN_MS: u64 = 100;
const SEARCH_TIMEOUT_MAX_MS: u64 = 10_000;

/// The complete command-line contract available in alpha.3.
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

    /// The requested alpha.3 operation.
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

/// Alpha.1 commands. Later release-train commands do not appear as stubs.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Create one local filesystem-backed index and project.
    Init(InitArgs),

    /// Authorize a canonical repository root for direct and MCP use.
    Trust(TrustArgs),

    /// Refresh the filesystem source into one atomic generation.
    Ingest(IngestArgs),

    /// Search exact identifiers, conservative quotes, and active lexical text.
    Search(SearchArgs),

    /// Resolve immutable bytes from an hsum://v1 citation.
    Get(GetArgs),

    /// Show index state and actionable problems.
    Status(StatusArgs),

    /// Diagnose the selected index without mutating it.
    Doctor(DoctorArgs),

    /// Show effective index, project, root, binding, and configuration origins.
    Context(ContextArgs),

    /// Show offline guidance for a stable hSUM error subcode.
    Help(HelpArgs),

    /// Generate or diagnose agent-client configuration.
    Client(ClientArgs),

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
}

/// Retrieval modes available in alpha.3.
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

    /// Retrieval mode; alpha.3 auto and lexical are equivalent.
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

/// Alpha.1 doctor is deliberately read-only and has no repair switches.
#[derive(Clone, Debug, Default, Args)]
pub struct DoctorArgs {}

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

/// Offline help topics available in alpha.3.
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

/// Capabilities compiled into this alpha.3 binary, in stable report order.
///
/// The list is tag-gated: deferred surface such as JSONL sources, vector
/// modes, or watch mode must never appear here before its release tag.
pub const COMPILED_CAPABILITIES: [&str; 5] = [
    "filesystem-ingest",
    "search-exact",
    "search-bm25",
    "evidence-get",
    "mcp-stdio-read-only",
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
/// Every line comes from the same compiled constants that gate the alpha.3
/// surface: crate version, versioned API identifier, the schema range this
/// binary can read and write, the build-target triple, and the tag-gated
/// capability list.
#[must_use]
pub fn render_verbose_version() -> String {
    format!(
        "hsum {}\n\
         API version: {MCP_API_VERSION}\n\
         Readable schema versions: {SCHEMA_VERSION}-{SCHEMA_VERSION}\n\
         Writable schema versions: {SCHEMA_VERSION}-{SCHEMA_VERSION}\n\
         Target: {}\n\
         Capabilities: {}\n",
        env!("CARGO_PKG_VERSION"),
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
