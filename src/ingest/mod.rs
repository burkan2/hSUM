mod chunk;
mod filesystem;
mod literals;
mod pipeline;
mod quote_bloom;

pub use chunk::{
    Chunk, ChunkError, ChunkKind, ChunkSettings, ChunkSettingsError, DEFAULT_CHUNK_MAX_BYTES,
    DEFAULT_CHUNK_OVERLAP_BYTES, DEFAULT_CHUNK_TARGET_BYTES, chunk_bytes,
};
pub use filesystem::FilesystemSpool;
#[cfg(unix)]
pub(crate) use filesystem::open_source_root;
pub use filesystem::{
    DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_SOURCE_FILES,
    DiscoveryConfigError, DiscoveryError, DiscoveryOptions, FileIssue, FileIssueKind,
    FilesystemFile, FilesystemSnapshot, HARD_MAX_DIRECTORY_DEPTH, HARD_MAX_ENTRIES_PER_DIRECTORY,
    HARD_MAX_FILE_BYTES, HARD_MAX_SOURCE_BYTES, HARD_MAX_SOURCE_FILES,
    HARD_MAX_VISITED_DIRECTORIES, HARD_MAX_VISITED_ENTRIES, SourceTimestamp, SpooledFilesystemFile,
    discover_files, discover_files_spooled, repo_uri,
};
pub(crate) use filesystem::{
    FilesystemDiscoveryEstimate, discover_files_spooled_bounded, estimate_filesystem_discovery,
};
pub use literals::{
    MAX_IDENTIFIER_LITERAL_BYTES, MAX_IDENTIFIER_LITERALS_PER_PASSAGE,
    MIN_IDENTIFIER_LITERAL_BYTES, extract_identifier_literals,
};
pub use pipeline::{RevisionHashError, SnapshotRevision, body_sha256, revision_sha256};
pub use quote_bloom::{QUOTE_BLOOM_BITS, QUOTE_BLOOM_BYTES, QUOTE_BLOOM_HASHES, QuoteBloom};
