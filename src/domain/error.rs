use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    NotFound,
    EvidenceForgotten,
    ModelMissing,
    ModelIncompatible,
    DownloadFailed,
    PermissionDenied,
    ResourceExhausted,
    IndexBusy,
    IndexCorrupt,
    IntegrityFailed,
    StaleCursor,
    Timeout,
    Cancelled,
    SchemaTooOld,
    SchemaTooNew,
    SourceInvalid,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::NotFound => "NOT_FOUND",
            Self::EvidenceForgotten => "EVIDENCE_FORGOTTEN",
            Self::ModelMissing => "MODEL_MISSING",
            Self::ModelIncompatible => "MODEL_INCOMPATIBLE",
            Self::DownloadFailed => "DOWNLOAD_FAILED",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Self::IndexBusy => "INDEX_BUSY",
            Self::IndexCorrupt => "INDEX_CORRUPT",
            Self::IntegrityFailed => "INTEGRITY_FAILED",
            Self::StaleCursor => "STALE_CURSOR",
            Self::Timeout => "TIMEOUT",
            Self::Cancelled => "CANCELLED",
            Self::SchemaTooOld => "SCHEMA_TOO_OLD",
            Self::SchemaTooNew => "SCHEMA_TOO_NEW",
            Self::SourceInvalid => "SOURCE_INVALID",
            Self::Internal => "INTERNAL",
        }
    }

    pub const fn process_exit_code(self) -> u8 {
        match self {
            Self::InvalidArgument => 2,
            Self::ModelMissing | Self::ModelIncompatible | Self::DownloadFailed => 3,
            Self::IndexBusy | Self::Timeout => 4,
            Self::IndexCorrupt
            | Self::IntegrityFailed
            | Self::SchemaTooOld
            | Self::SchemaTooNew => 5,
            Self::PermissionDenied | Self::ResourceExhausted => 7,
            Self::Cancelled => 130,
            Self::NotFound
            | Self::EvidenceForgotten
            | Self::StaleCursor
            | Self::SourceInvalid
            | Self::Internal => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorSubcode {
    QueryEmpty,
    QuerySyntax,
    LimitOutOfRange,
    CitationMalformed,
    PointerExists,
    FrameLimit,
    ConfigInvalid,
    PathInvalid,
    PointerInvalid,
    TrustRegistryInvalid,
    BroadRootConfirmationRequired,
    LargeSourceConfirmationRequired,
    EmptySnapshotConfirmationRequired,
    MassDeleteConfirmationRequired,
    TrustConfirmationRequired,
    IndexPathOccupied,
    IndexNotFound,
    ProjectNotFound,
    EvidenceNotFound,
    ScopeHidden,
    ForgetTombstone,
    ModelNotConfigured,
    ModelNotInstalled,
    ModelFingerprint,
    ModelDimension,
    NetworkTransient,
    ChecksumMismatch,
    SourceRead,
    IndexWrite,
    RepositoryNotActivated,
    PathTrust,
    DiskSpace,
    IndexQuota,
    MemoryBudget,
    UnsupportedStorage,
    StorageIo,
    SqliteIo,
    UnsupportedPlatform,
    WriterLock,
    ModelQueue,
    ModelRestarting,
    ApplicationId,
    SchemaChecksum,
    PipelineFingerprint,
    SqliteCorrupt,
    HeadIndexMismatch,
    ForgetLedgerMismatch,
    IndexEpoch,
    Generation,
    ScopeRevision,
    QueryFingerprint,
    RequestDeadline,
    DriftProbe,
    ClientCancelled,
    ClientDisconnected,
    MigrationRequired,
    UpgradeRequired,
    DowngradeUnsupported,
    InvalidUtf8,
    NulContent,
    FileTooLarge,
    EnumerationIncomplete,
    SourceChangedDuringRead,
    Invariant,
    NonfiniteScore,
    Unexpected,
}

impl ErrorSubcode {
    pub const ALL: [Self; 66] = [
        Self::QueryEmpty,
        Self::QuerySyntax,
        Self::LimitOutOfRange,
        Self::CitationMalformed,
        Self::PointerExists,
        Self::FrameLimit,
        Self::ConfigInvalid,
        Self::PathInvalid,
        Self::PointerInvalid,
        Self::TrustRegistryInvalid,
        Self::BroadRootConfirmationRequired,
        Self::LargeSourceConfirmationRequired,
        Self::EmptySnapshotConfirmationRequired,
        Self::MassDeleteConfirmationRequired,
        Self::TrustConfirmationRequired,
        Self::IndexPathOccupied,
        Self::IndexNotFound,
        Self::ProjectNotFound,
        Self::EvidenceNotFound,
        Self::ScopeHidden,
        Self::ForgetTombstone,
        Self::ModelNotConfigured,
        Self::ModelNotInstalled,
        Self::ModelFingerprint,
        Self::ModelDimension,
        Self::NetworkTransient,
        Self::ChecksumMismatch,
        Self::SourceRead,
        Self::IndexWrite,
        Self::RepositoryNotActivated,
        Self::PathTrust,
        Self::DiskSpace,
        Self::IndexQuota,
        Self::MemoryBudget,
        Self::UnsupportedStorage,
        Self::StorageIo,
        Self::SqliteIo,
        Self::UnsupportedPlatform,
        Self::WriterLock,
        Self::ModelQueue,
        Self::ModelRestarting,
        Self::ApplicationId,
        Self::SchemaChecksum,
        Self::PipelineFingerprint,
        Self::SqliteCorrupt,
        Self::HeadIndexMismatch,
        Self::ForgetLedgerMismatch,
        Self::IndexEpoch,
        Self::Generation,
        Self::ScopeRevision,
        Self::QueryFingerprint,
        Self::RequestDeadline,
        Self::DriftProbe,
        Self::ClientCancelled,
        Self::ClientDisconnected,
        Self::MigrationRequired,
        Self::UpgradeRequired,
        Self::DowngradeUnsupported,
        Self::InvalidUtf8,
        Self::NulContent,
        Self::FileTooLarge,
        Self::EnumerationIncomplete,
        Self::SourceChangedDuringRead,
        Self::Invariant,
        Self::NonfiniteScore,
        Self::Unexpected,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryEmpty => "QUERY_EMPTY",
            Self::QuerySyntax => "QUERY_SYNTAX",
            Self::LimitOutOfRange => "LIMIT_OUT_OF_RANGE",
            Self::CitationMalformed => "CITATION_MALFORMED",
            Self::PointerExists => "POINTER_EXISTS",
            Self::FrameLimit => "FRAME_LIMIT",
            Self::ConfigInvalid => "CONFIG_INVALID",
            Self::PathInvalid => "PATH_INVALID",
            Self::PointerInvalid => "POINTER_INVALID",
            Self::TrustRegistryInvalid => "TRUST_REGISTRY_INVALID",
            Self::BroadRootConfirmationRequired => "BROAD_ROOT_CONFIRMATION_REQUIRED",
            Self::LargeSourceConfirmationRequired => "LARGE_SOURCE_CONFIRMATION_REQUIRED",
            Self::EmptySnapshotConfirmationRequired => "EMPTY_SNAPSHOT_CONFIRMATION_REQUIRED",
            Self::MassDeleteConfirmationRequired => "MASS_DELETE_CONFIRMATION_REQUIRED",
            Self::TrustConfirmationRequired => "TRUST_CONFIRMATION_REQUIRED",
            Self::IndexPathOccupied => "INDEX_PATH_OCCUPIED",
            Self::IndexNotFound => "INDEX_NOT_FOUND",
            Self::ProjectNotFound => "PROJECT_NOT_FOUND",
            Self::EvidenceNotFound => "EVIDENCE_NOT_FOUND",
            Self::ScopeHidden => "SCOPE_HIDDEN",
            Self::ForgetTombstone => "FORGET_TOMBSTONE",
            Self::ModelNotConfigured => "MODEL_NOT_CONFIGURED",
            Self::ModelNotInstalled => "MODEL_NOT_INSTALLED",
            Self::ModelFingerprint => "MODEL_FINGERPRINT",
            Self::ModelDimension => "MODEL_DIMENSION",
            Self::NetworkTransient => "NETWORK_TRANSIENT",
            Self::ChecksumMismatch => "CHECKSUM_MISMATCH",
            Self::SourceRead => "SOURCE_READ",
            Self::IndexWrite => "INDEX_WRITE",
            Self::RepositoryNotActivated => "REPOSITORY_NOT_ACTIVATED",
            Self::PathTrust => "PATH_TRUST",
            Self::DiskSpace => "DISK_SPACE",
            Self::IndexQuota => "INDEX_QUOTA",
            Self::MemoryBudget => "MEMORY_BUDGET",
            Self::UnsupportedStorage => "UNSUPPORTED_STORAGE",
            Self::StorageIo => "STORAGE_IO",
            Self::SqliteIo => "SQLITE_IO",
            Self::UnsupportedPlatform => "UNSUPPORTED_PLATFORM",
            Self::WriterLock => "WRITER_LOCK",
            Self::ModelQueue => "MODEL_QUEUE",
            Self::ModelRestarting => "MODEL_RESTARTING",
            Self::ApplicationId => "APPLICATION_ID",
            Self::SchemaChecksum => "SCHEMA_CHECKSUM",
            Self::PipelineFingerprint => "PIPELINE_FINGERPRINT",
            Self::SqliteCorrupt => "SQLITE_CORRUPT",
            Self::HeadIndexMismatch => "HEAD_INDEX_MISMATCH",
            Self::ForgetLedgerMismatch => "FORGET_LEDGER_MISMATCH",
            Self::IndexEpoch => "INDEX_EPOCH",
            Self::Generation => "GENERATION",
            Self::ScopeRevision => "SCOPE_REVISION",
            Self::QueryFingerprint => "QUERY_FINGERPRINT",
            Self::RequestDeadline => "REQUEST_DEADLINE",
            Self::DriftProbe => "DRIFT_PROBE",
            Self::ClientCancelled => "CLIENT_CANCELLED",
            Self::ClientDisconnected => "CLIENT_DISCONNECTED",
            Self::MigrationRequired => "MIGRATION_REQUIRED",
            Self::UpgradeRequired => "UPGRADE_REQUIRED",
            Self::DowngradeUnsupported => "DOWNGRADE_UNSUPPORTED",
            Self::InvalidUtf8 => "INVALID_UTF8",
            Self::NulContent => "NUL_CONTENT",
            Self::FileTooLarge => "FILE_TOO_LARGE",
            Self::EnumerationIncomplete => "ENUMERATION_INCOMPLETE",
            Self::SourceChangedDuringRead => "SOURCE_CHANGED_DURING_READ",
            Self::Invariant => "INVARIANT",
            Self::NonfiniteScore => "NONFINITE_SCORE",
            Self::Unexpected => "UNEXPECTED",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|subcode| subcode.as_str() == value)
    }

    #[must_use]
    pub fn render_offline_help(self) -> String {
        let spec = self.spec();
        format!(
            "error: {}\ncategory: {}\nretryable: {}\nproblem: {}\ncause: {}\nfix: {}\n\
             example: {}",
            self,
            spec.code.as_str(),
            if spec.retryable { "yes" } else { "no" },
            spec.message,
            spec.cause,
            spec.fix,
            spec.fix,
        )
    }

    const fn spec(self) -> ErrorSpec {
        match self {
            Self::QueryEmpty => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the search query is empty",
                "the query contains no searchable input",
                "provide a nonempty query",
            ),
            Self::QuerySyntax => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the search query syntax is invalid",
                "a quote, atom, or byte limit violates the query contract",
                "correct the query shown by hsum search --help",
            ),
            Self::LimitOutOfRange => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "a requested limit is outside the supported range",
                "the value exceeds a documented bounded-work limit",
                "choose a value within the printed range",
            ),
            Self::CitationMalformed => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the evidence citation is invalid",
                "the value is not a canonical hsum://v1 citation",
                "copy the complete citation from hsum search or evidence_search",
            ),
            Self::PointerExists => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "a repository pointer already exists",
                "the requested pointer write conflicts with existing bytes",
                "inspect the pointer, then repeat with --force-pointer if replacement is intended",
            ),
            Self::FrameLimit => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the protocol request exceeds a framing limit",
                "the frame is too large, too deep, duplicated, or has unknown fields",
                "send a smaller request matching the advertised tool schema",
            ),
            Self::ConfigInvalid => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the hSUM configuration is invalid",
                "a configuration source is malformed, incomplete, unsafe, or incompatible",
                "repair the reported configuration source and retry",
            ),
            Self::PathInvalid => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "a required filesystem path is invalid",
                "the path is missing, relative where absolute is required, or has the wrong type",
                "select an existing absolute path of the required type",
            ),
            Self::PointerInvalid => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the repository pointer is invalid",
                "the pointer is malformed, unsafe, incompatible, or changed during inspection",
                "repair or remove .hsum.toml, then retry",
            ),
            Self::TrustRegistryInvalid => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the hSUM trust registry is invalid",
                "the private trust file is malformed, incompatible, ambiguous, or over its limit",
                "repair the reported trust entry before selecting or adding bindings",
            ),
            Self::BroadRootConfirmationRequired => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the selected source root requires explicit broad-root confirmation",
                "the root is the filesystem root, the account home, or above the Git worktree",
                "inspect the selected root, then repeat with --allow-broad-root if intended",
            ),
            Self::LargeSourceConfirmationRequired => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the selected source exceeds the default initialization budget",
                "the estimated file count or byte total requires an explicit large-source decision",
                "inspect the estimate, then repeat with --allow-large-source if intended",
            ),
            Self::EmptySnapshotConfirmationRequired => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "replacing a nonempty source with an empty snapshot requires confirmation",
                "the authoritative snapshot would remove every current document",
                "verify the source root, then repeat with --allow-empty-snapshot if intended",
            ),
            Self::MassDeleteConfirmationRequired => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the snapshot exceeds the mass-deletion confirmation threshold",
                "activation would tombstone an unusually large share of current documents",
                "inspect the ingest plan, then repeat with --allow-mass-delete if intended",
            ),
            Self::TrustConfirmationRequired => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "creating a repository trust binding requires confirmation",
                "the operation would add persistent user-side authority",
                "inspect the target, then repeat the trust command with --confirm",
            ),
            Self::IndexPathOccupied => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the managed index path is already occupied",
                "a file or directory already exists where a new managed index must be created",
                "select another index name or move the unrelated path without overwriting it",
            ),
            Self::IndexNotFound => ErrorSpec::new(
                ErrorCode::NotFound,
                false,
                "the selected hSUM index is unavailable",
                "no authorized managed index matches the selection",
                "run hsum context, then initialize or select the intended index",
            ),
            Self::ProjectNotFound => ErrorSpec::new(
                ErrorCode::NotFound,
                false,
                "the selected hSUM project is unavailable",
                "the project does not exist in the authorized index",
                "run hsum context and select an existing project",
            ),
            Self::EvidenceNotFound | Self::ScopeHidden => ErrorSpec::new(
                ErrorCode::NotFound,
                false,
                "the requested evidence is unavailable",
                "the citation is absent, pruned, or outside the bound project",
                "search again inside the authorized project",
            ),
            Self::ForgetTombstone => ErrorSpec::new(
                ErrorCode::EvidenceForgotten,
                false,
                "the cited evidence was explicitly forgotten",
                "a body-free deletion tombstone prevents resolution",
                "use the explicit restore ceremony only if resurrection is intended",
            ),
            Self::ModelNotConfigured => ErrorSpec::new(
                ErrorCode::ModelMissing,
                false,
                "this index has no configured model",
                "semantic retrieval was requested from a lexical-only index",
                "create a new model-enabled index when that release capability is available",
            ),
            Self::ModelNotInstalled => ErrorSpec::new(
                ErrorCode::ModelMissing,
                false,
                "the configured model is not installed",
                "the pinned local model artifact is absent",
                "install the exact pinned model before retrying",
            ),
            Self::ModelFingerprint | Self::ModelDimension => ErrorSpec::new(
                ErrorCode::ModelIncompatible,
                false,
                "the local model is incompatible with this index",
                "its fingerprint or vector dimension differs from the pinned index contract",
                "use the pinned model or create a new index",
            ),
            Self::NetworkTransient => ErrorSpec::new(
                ErrorCode::DownloadFailed,
                true,
                "the model download failed temporarily",
                "the explicit download encountered a transient network failure",
                "retry the same model install command",
            ),
            Self::ChecksumMismatch => ErrorSpec::new(
                ErrorCode::DownloadFailed,
                false,
                "the downloaded model checksum is invalid",
                "the artifact does not match the signed manifest",
                "verify the manifest and release before retrying",
            ),
            Self::SourceRead => ErrorSpec::new(
                ErrorCode::PermissionDenied,
                false,
                "hSUM cannot read the selected source",
                "filesystem permissions deny a required descriptor-relative read",
                "grant the current user read access and retry",
            ),
            Self::IndexWrite => ErrorSpec::new(
                ErrorCode::PermissionDenied,
                false,
                "hSUM cannot write the managed index",
                "filesystem permissions deny a required index mutation",
                "grant the current user access to the managed data directory",
            ),
            Self::RepositoryNotActivated => ErrorSpec::new(
                ErrorCode::PermissionDenied,
                false,
                "hSUM is not activated for this repository",
                "no user-side trust binding matches the current repository root",
                "run the activation command from the error details, then retry the same tool call",
            ),
            Self::PathTrust => ErrorSpec::new(
                ErrorCode::PermissionDenied,
                false,
                "this repository is not bound to an hSUM project",
                "selection did not resolve through a matching user-side trust binding",
                "run hsum trust <root> --confirm",
            ),
            Self::DiskSpace => ErrorSpec::new(
                ErrorCode::ResourceExhausted,
                false,
                "there is not enough free disk space",
                "the operation cannot preserve its required recovery reserve",
                "free disk space and retry",
            ),
            Self::IndexQuota => ErrorSpec::new(
                ErrorCode::ResourceExhausted,
                false,
                "the managed index quota would be exceeded",
                "the estimated operation peak exceeds the configured quota",
                "free retained data or explicitly raise the quota",
            ),
            Self::MemoryBudget => ErrorSpec::new(
                ErrorCode::ResourceExhausted,
                false,
                "the request exceeds the memory budget",
                "bounded processing cannot safely allocate the requested work",
                "reduce the request size and retry",
            ),
            Self::UnsupportedStorage => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "the selected index storage is unsupported",
                "the location is a known network filesystem or violates local managed-file safety",
                "move the managed data directory to a supported local filesystem",
            ),
            Self::StorageIo => ErrorSpec::new(
                ErrorCode::Internal,
                true,
                "the managed storage operation failed",
                "the operating system could not complete a required local filesystem operation",
                "check storage health and permissions, then retry with the request ID",
            ),
            Self::SqliteIo => ErrorSpec::new(
                ErrorCode::Internal,
                true,
                "SQLite could not complete a storage I/O operation",
                "the database encountered an operating-system or filesystem I/O failure",
                "check local storage health, preserve the request ID, and retry",
            ),
            Self::UnsupportedPlatform => ErrorSpec::new(
                ErrorCode::InvalidArgument,
                false,
                "this operation is unsupported on the current platform",
                "the alpha.3 filesystem safety implementation is unavailable on this operating system",
                "run hSUM on a documented supported platform",
            ),
            Self::WriterLock => ErrorSpec::new(
                ErrorCode::IndexBusy,
                true,
                "another process is mutating this index",
                "the advisory writer lock remained owned through the bounded wait",
                "retry after the reported backoff",
            ),
            Self::ModelQueue | Self::ModelRestarting => ErrorSpec::new(
                ErrorCode::IndexBusy,
                true,
                "the local model worker is busy",
                "the bounded model queue is full or restarting",
                "retry after the reported backoff or use lexical mode",
            ),
            Self::ApplicationId => ErrorSpec::new(
                ErrorCode::IndexCorrupt,
                false,
                "the selected file is not an hSUM index",
                "its SQLite application identifier does not match HSUM",
                "preserve the file and run hsum doctor",
            ),
            Self::SchemaChecksum => ErrorSpec::new(
                ErrorCode::IndexCorrupt,
                false,
                "the hSUM index schema checksum is invalid",
                "the live schema differs from the compiled migration",
                "quarantine the index and run hsum doctor",
            ),
            Self::PipelineFingerprint => ErrorSpec::new(
                ErrorCode::SchemaTooOld,
                false,
                "the index was built by a different hSUM indexing pipeline",
                "the indexing rules changed, so stored evidence no longer matches this binary",
                "run hsum init --rebuild to replace this index under the current rules; \
                 evidence recorded before the change does not survive the rebuild",
            ),
            Self::SqliteCorrupt => ErrorSpec::new(
                ErrorCode::IndexCorrupt,
                false,
                "SQLite reports index corruption",
                "the database failed a structural or integrity check",
                "quarantine the index and run hsum doctor",
            ),
            Self::HeadIndexMismatch => ErrorSpec::new(
                ErrorCode::IntegrityFailed,
                false,
                "active evidence indexes are out of parity",
                "document heads, passages, FTS, or literal rows disagree",
                "stop mutation and run hsum doctor",
            ),
            Self::ForgetLedgerMismatch => ErrorSpec::new(
                ErrorCode::IntegrityFailed,
                false,
                "the forget ledger does not match the index",
                "the body-free deletion chain failed identity or hash validation",
                "quarantine the index and run hsum doctor",
            ),
            Self::IndexEpoch | Self::Generation | Self::ScopeRevision | Self::QueryFingerprint => {
                ErrorSpec::new(
                    ErrorCode::StaleCursor,
                    false,
                    "the search cursor is stale",
                    "its bound index, generation, scope, or query state changed",
                    "restart pagination without the cursor",
                )
            }
            Self::RequestDeadline => ErrorSpec::new(
                ErrorCode::Timeout,
                true,
                "the request deadline expired",
                "bounded retrieval work did not complete before the deadline",
                "retry or raise the timeout within the documented bound",
            ),
            Self::DriftProbe => ErrorSpec::new(
                ErrorCode::Timeout,
                true,
                "the live-source probe timed out",
                "source state could not be verified within its post-query deadline",
                "retry the probe; stored evidence remains available",
            ),
            Self::ClientCancelled => ErrorSpec::new(
                ErrorCode::Cancelled,
                false,
                "the client cancelled the request",
                "the active request received an explicit cancellation",
                "start a new request if the evidence is still needed",
            ),
            Self::ClientDisconnected => ErrorSpec::new(
                ErrorCode::Cancelled,
                false,
                "the client disconnected",
                "the protocol transport closed before the response completed",
                "reconnect and start a new request",
            ),
            Self::MigrationRequired => ErrorSpec::new(
                ErrorCode::SchemaTooOld,
                false,
                "the index requires an explicit schema migration",
                "the index is one supported schema behind this binary",
                "run the printed migration plan command",
            ),
            Self::UpgradeRequired => ErrorSpec::new(
                ErrorCode::SchemaTooOld,
                false,
                "the index requires an intermediate hSUM upgrade",
                "its schema is older than this binary can diagnose directly",
                "install the printed intermediate version",
            ),
            Self::DowngradeUnsupported => ErrorSpec::new(
                ErrorCode::SchemaTooNew,
                false,
                "the index was created by a newer hSUM binary",
                "this binary cannot safely interpret the newer schema",
                "use the indicated newer binary",
            ),
            Self::InvalidUtf8 => ErrorSpec::new(
                ErrorCode::SourceInvalid,
                false,
                "a source file is not valid UTF-8",
                "alpha.3 indexes original UTF-8 text only",
                "convert or exclude the file",
            ),
            Self::NulContent => ErrorSpec::new(
                ErrorCode::SourceInvalid,
                false,
                "a source file contains NUL bytes",
                "the file is treated as non-text input",
                "convert or exclude the file",
            ),
            Self::FileTooLarge => ErrorSpec::new(
                ErrorCode::SourceInvalid,
                false,
                "a source file exceeds the configured size limit",
                "the file is above the bounded ingest ceiling",
                "exclude it or explicitly choose a permitted larger limit",
            ),
            Self::EnumerationIncomplete => ErrorSpec::new(
                ErrorCode::SourceInvalid,
                true,
                "the filesystem enumeration was incomplete",
                "at least one directory could not be listed authoritatively",
                "restore source access and retry without changing prior heads",
            ),
            Self::SourceChangedDuringRead => ErrorSpec::new(
                ErrorCode::SourceInvalid,
                true,
                "a source file changed while it was read",
                "two stable descriptor reads could not capture one snapshot",
                "retry after the writer becomes idle",
            ),
            Self::Invariant => ErrorSpec::new(
                ErrorCode::Internal,
                false,
                "an internal evidence invariant failed",
                "a state that should be impossible was detected",
                "preserve the request ID and run hsum doctor",
            ),
            Self::NonfiniteScore => ErrorSpec::new(
                ErrorCode::Internal,
                false,
                "a retrieval backend returned an invalid score",
                "a non-finite value cannot enter deterministic ranking",
                "preserve the request ID and run hsum doctor",
            ),
            Self::Unexpected => ErrorSpec::new(
                ErrorCode::Internal,
                false,
                "an unexpected internal failure occurred",
                "the operation failed outside a more specific public category",
                "preserve the request ID and run hsum doctor",
            ),
        }
    }
}

impl fmt::Display for ErrorSubcode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy)]
struct ErrorSpec {
    code: ErrorCode,
    retryable: bool,
    message: &'static str,
    cause: &'static str,
    fix: &'static str,
}

impl ErrorSpec {
    const fn new(
        code: ErrorCode,
        retryable: bool,
        message: &'static str,
        cause: &'static str,
        fix: &'static str,
    ) -> Self {
        Self {
            code,
            retryable,
            message,
            cause,
            fix,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicError {
    pub code: ErrorCode,
    pub subcode: ErrorSubcode,
    pub message: &'static str,
    pub retryable: bool,
    pub details: Value,
    pub next_action: &'static str,
    pub request_id: String,
    #[serde(skip)]
    cause: &'static str,
    #[serde(skip)]
    fix: &'static str,
}

impl PublicError {
    pub fn from_subcode(subcode: ErrorSubcode, request_id: impl Into<String>) -> Self {
        Self::with_details(subcode, request_id, Value::Object(Map::new()))
    }

    pub fn with_details(
        subcode: ErrorSubcode,
        request_id: impl Into<String>,
        details: Value,
    ) -> Self {
        let spec = subcode.spec();
        Self {
            code: spec.code,
            subcode,
            message: spec.message,
            retryable: spec.retryable,
            details,
            next_action: spec.fix,
            request_id: request_id.into(),
            cause: spec.cause,
            fix: spec.fix,
        }
    }

    pub fn citation_malformed(request_id: impl Into<String>) -> Self {
        Self::from_subcode(ErrorSubcode::CitationMalformed, request_id)
    }

    pub const fn process_exit_code(&self) -> u8 {
        self.code.process_exit_code()
    }

    pub fn render_human(&self) -> String {
        format!(
            "problem: {}\ncause: {}\nfix: {}\nlearn: hsum help error {} — code: {} — \
             request: {}",
            self.message, self.cause, self.fix, self.subcode, self.subcode, self.request_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_error_has_the_locked_four_line_shape() {
        let error = PublicError::citation_malformed("req-123");
        assert_eq!(
            error.render_human(),
            concat!(
                "problem: the evidence citation is invalid\n",
                "cause: the value is not a canonical hsum://v1 citation\n",
                "fix: copy the complete citation from hsum search or evidence_search\n",
                "learn: hsum help error CITATION_MALFORMED — code: CITATION_MALFORMED ",
                "— request: req-123"
            )
        );
    }

    #[test]
    fn json_uses_the_same_code_subcode_and_action_metadata() {
        let error = PublicError::citation_malformed("req-123");
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "INVALID_ARGUMENT");
        assert_eq!(value["subcode"], "CITATION_MALFORMED");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["request_id"], "req-123");
        assert!(value.get("docs_url").is_none());
    }

    #[test]
    fn every_frozen_subcode_has_cli_json_mcp_ready_metadata() {
        for subcode in ErrorSubcode::ALL {
            let error = PublicError::from_subcode(subcode, "req-catalog");
            let rendered = error.render_human();
            assert_eq!(rendered.lines().count(), 4, "{subcode}");
            assert!(rendered.contains(&format!("code: {subcode}")));
            assert!(rendered.contains(&format!("hsum help error {subcode}")));
            assert!(rendered.contains("request: req-catalog"));
            assert!(!error.message.is_empty());
            assert!(!error.next_action.is_empty());

            let json = serde_json::to_value(&error).unwrap();
            assert_eq!(json["subcode"], subcode.as_str());
            assert_eq!(json["next_action"], error.next_action);
            assert_eq!(json["retryable"], error.retryable);
        }
    }

    #[test]
    fn every_frozen_subcode_has_self_contained_offline_help() {
        for subcode in ErrorSubcode::ALL {
            assert_eq!(ErrorSubcode::parse(subcode.as_str()), Some(subcode));
            let rendered = subcode.render_offline_help();
            assert!(rendered.contains(&format!("error: {subcode}")));
            assert!(!rendered.contains("docs: "));
            for label in [
                "category:",
                "retryable:",
                "problem:",
                "cause:",
                "fix:",
                "example:",
            ] {
                assert!(
                    rendered.lines().any(|line| line.starts_with(label)),
                    "{subcode} omitted {label}"
                );
            }
        }
        assert_eq!(ErrorSubcode::parse("citation_malformed"), None);
        assert_eq!(ErrorSubcode::parse("NOT_A_SUBCODE"), None);
    }

    #[test]
    fn pipeline_fingerprint_subcode_names_the_pipeline_not_the_schema() {
        let subcode = ErrorSubcode::PipelineFingerprint;
        assert_eq!(subcode.as_str(), "PIPELINE_FINGERPRINT");
        assert_eq!(ErrorSubcode::parse("PIPELINE_FINGERPRINT"), Some(subcode));

        let rendered = subcode.render_offline_help();
        let problem = rendered
            .lines()
            .find(|line| line.starts_with("problem:"))
            .expect("offline help renders a problem line");
        let fix = rendered
            .lines()
            .find(|line| line.starts_with("fix:"))
            .expect("offline help renders a fix line");

        assert!(
            problem.contains("pipeline"),
            "problem line must name the pipeline: {problem:?}"
        );
        assert!(
            !problem.contains("schema checksum"),
            "problem line must not claim the schema checksum is invalid: {problem:?}"
        );
        assert!(
            fix.contains("hsum init --rebuild"),
            "fix line must name the working remedy: {fix:?}"
        );
    }

    #[test]
    fn process_exit_mapping_is_stable_at_public_code_boundary() {
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::QuerySyntax, "r").process_exit_code(),
            2
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::WriterLock, "r").process_exit_code(),
            4
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::SchemaChecksum, "r").process_exit_code(),
            5
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::DiskSpace, "r").process_exit_code(),
            7
        );
        assert_eq!(
            PublicError::from_subcode(ErrorSubcode::ClientCancelled, "r").process_exit_code(),
            130
        );
    }
}
