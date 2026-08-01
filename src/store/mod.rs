mod capacity;
mod doctor;
mod forget_ledger;
mod generation;
mod lock;
mod maintenance;
mod managed_backup;
mod open;
mod project;
mod schema;
mod source;

pub use capacity::{
    FilesystemAssessment, FilesystemLocality, MINIMUM_STORAGE_RESERVE_BYTES, StorageInspection,
    StoragePreflight, StoragePreflightError, SyncRootKind, classify_sync_root,
    storage_reserve_bytes,
};
pub use doctor::{Doctor, DoctorRepairOutcome, DoctorReport, DoctorScanStats, DoctorSupportReport};
pub(crate) use doctor::{FingerprintPolicy, inspect_migration_source_with_policy};
pub(crate) use forget_ledger::{
    ForgetLedger, ForgetLedgerTarget, ForgetOperationState, ReplacementEpoch,
};
pub(crate) use generation::JsonlBatchSource;
pub use generation::{
    DEFAULT_WRITER_LOCK_TIMEOUT, DeleteConfirmations, FilesystemScope, IngestOutcome, IngestPlan,
    JsonlScope, LiteralField, PreparedChunk, PreparedDocument, SnapshotFailure,
    SourceIngestOutcome, SourceIngestState, SourceRemovalOutcome, prepare_passage_literals,
};
pub(crate) use generation::{PreparedDocumentSummary, PreparedSourceBatch};
pub use lock::{ReaderLease, ReplacementLock, WriterLock};
pub use maintenance::{
    BackupReceipt, ForgetOutcome, ForgetPlan, ForgottenRevision, MaintenanceError,
    MigrationOutcome, MigrationPlan, MigrationStep, PlanEnvelope, PruneOutcome, PrunePlan,
    PrunedGeneration, PrunedRevision, RestoreOutcome, RestorePlan, apply_forget,
    apply_forget_with_observer, apply_migration, apply_prune, apply_restore, create_backup,
    inspect_backup, plan_forget, plan_migration, plan_prune, read_plan, write_plan,
    write_private_json,
};
pub(crate) use maintenance::{create_plan_envelope, validate_plan_envelope};
pub use managed_backup::{
    BackupReservation, ManagedBackupCatalog, ManagedBackupDisposition,
    ManagedBackupDispositionOutcome, ManagedBackupError, ManagedBackupInventoryItem,
    ManagedBackupKind, ManagedBackupState,
};
pub use open::{IndexDb, OpenMode, StoreError};
pub use project::{
    ConfiguredProject, FilesystemReplacementOutcome, ProjectRegistration,
    create_project_with_timeout, list_projects, replace_project_filesystem_source_with_timeout,
};
pub use schema::{
    APPLICATION_ID, SCHEMA_VERSION, chunker_fingerprint, pipeline_descriptor, pipeline_fingerprint,
    schema_checksum,
};
pub use source::{
    ConfiguredSource, ConfiguredSourceKind, FilesystemSourceRegistration, SourceMembershipOutcome,
    SourceRegistration, attach_jsonl_source_with_timeout, configure_filesystem_source_with_timeout,
    configure_jsonl_source_with_timeout, detach_jsonl_source_with_timeout, list_index_sources,
    list_project_sources,
};
