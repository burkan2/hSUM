mod capacity;
mod doctor;
mod generation;
mod lock;
mod open;
mod schema;

pub use capacity::{
    FilesystemAssessment, FilesystemLocality, MINIMUM_STORAGE_RESERVE_BYTES, StorageInspection,
    StoragePreflight, StoragePreflightError, SyncRootKind, classify_sync_root,
    storage_reserve_bytes,
};
pub use doctor::{Doctor, DoctorReport, DoctorScanStats};
pub(crate) use generation::PreparedDocumentSummary;
pub use generation::{
    DEFAULT_WRITER_LOCK_TIMEOUT, DeleteConfirmations, FilesystemScope, IngestOutcome, IngestPlan,
    LiteralField, PreparedChunk, PreparedDocument, SnapshotFailure, SourceIngestOutcome,
    SourceIngestState, prepare_passage_literals,
};
pub use lock::WriterLock;
pub use open::{IndexDb, OpenMode, StoreError};
pub use schema::{
    APPLICATION_ID, SCHEMA_VERSION, chunker_fingerprint, pipeline_fingerprint, schema_checksum,
};
