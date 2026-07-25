use std::path::PathBuf;

use hsum::domain::{IndexId, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{
    ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, body_sha256, chunk_bytes,
    revision_sha256,
};
use hsum::store::{
    DeleteConfirmations, FilesystemScope, FingerprintPolicy, IndexDb, OpenMode, PreparedChunk,
    PreparedDocument, StoreError, prepare_passage_literals,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

mod support;

const FIXTURE_INDEX_ID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
const STALE_FINGERPRINT: [u8; 32] = [0xbb; 32];
const CORRUPT_SCHEMA_CHECKSUM: [u8; 32] = [0xcc; 32];

/// A real, on-disk index plus the `TempDir` that must stay alive so the
/// directory holding it is not dropped mid-test.
struct Fixture {
    _directory: TempDir,
    database_path: PathBuf,
}

/// A stale index — one whose stored pipeline fingerprint predates the running
/// binary — must stay unopenable by default, and must open only when a caller
/// explicitly tolerates the mismatch.
#[test]
fn a_stale_fingerprint_is_rejected_by_default_and_tolerated_only_on_request() {
    let fixture = stale_fingerprint_index();

    let rejected = IndexDb::open_existing(&fixture.database_path, OpenMode::ReadOnly);
    assert!(
        matches!(rejected, Err(StoreError::PipelineFingerprintMismatch)),
        "default open must still refuse a stale index"
    );

    let tolerated = IndexDb::open_existing_with_policy(
        &fixture.database_path,
        OpenMode::ReadOnly,
        FingerprintPolicy::Tolerate,
    );
    assert!(
        tolerated.is_ok(),
        "an explicit tolerate policy must open the stale index: {:?}",
        tolerated.err()
    );
}

/// Tolerating a fingerprint mismatch must not tolerate anything else. A
/// corrupt schema checksum stays fatal in both policies.
#[test]
fn tolerating_the_fingerprint_does_not_relax_other_integrity_checks() {
    let fixture = stale_fingerprint_index_with_corrupt_schema_checksum();

    let tolerated = IndexDb::open_existing_with_policy(
        &fixture.database_path,
        OpenMode::ReadOnly,
        FingerprintPolicy::Tolerate,
    );
    assert!(
        matches!(tolerated, Err(StoreError::SchemaChecksumMismatch)),
        "schema checksum must remain fatal under Tolerate, got {:?}",
        tolerated.err()
    );
}

/// Builds a real index with one committed generation, then rewrites
/// `index_meta.pipeline_fingerprint` and every `generations.pipeline_fingerprint`
/// to a stale 32-byte value, simulating an index built by an older binary.
fn stale_fingerprint_index() -> Fixture {
    let directory = support::private_tempdir().unwrap();
    let database_path = directory.path().join("index.sqlite");
    create_fixture_with_generation(&database_path);

    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'pipeline_fingerprint'",
            params![STALE_FINGERPRINT.as_slice()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE generations SET pipeline_fingerprint = ?1",
            params![STALE_FINGERPRINT.as_slice()],
        )
        .unwrap();
    connection.close().unwrap();

    Fixture {
        _directory: directory,
        database_path,
    }
}

/// The same stale-fingerprint state as `stale_fingerprint_index`, plus a
/// corrupted `index_meta.schema_checksum` — this must stay fatal even when
/// the caller tolerates the fingerprint mismatch.
fn stale_fingerprint_index_with_corrupt_schema_checksum() -> Fixture {
    let directory = support::private_tempdir().unwrap();
    let database_path = directory.path().join("index.sqlite");
    create_fixture_with_generation(&database_path);

    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'pipeline_fingerprint'",
            params![STALE_FINGERPRINT.as_slice()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE generations SET pipeline_fingerprint = ?1",
            params![STALE_FINGERPRINT.as_slice()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'schema_checksum'",
            params![CORRUPT_SCHEMA_CHECKSUM.as_slice()],
        )
        .unwrap();
    connection.close().unwrap();

    Fixture {
        _directory: directory,
        database_path,
    }
}

fn create_fixture_with_generation(path: &std::path::Path) {
    let mut database = IndexDb::create(
        path,
        IndexId::from_uuid(Uuid::parse_str(FIXTURE_INDEX_ID).unwrap()),
    )
    .unwrap();
    let scope = fixture_scope();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[fixture_document(b"alpha-beta\n")],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
}

fn fixture_scope() -> FilesystemScope {
    let source_id =
        SourceId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401").unwrap());
    let project_id =
        ProjectId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402").unwrap());
    FilesystemScope {
        source_id,
        source_name: "fixture".parse::<SafeSlug>().unwrap(),
        source_logical_uri: "file:///fixture".to_owned(),
        source_config_json: r#"{"root":"/fixture"}"#.to_owned(),
        project_id,
        project_name: "fixture".parse::<SafeSlug>().unwrap(),
    }
}

fn fixture_document(body: &[u8]) -> PreparedDocument {
    let connector_key = b"notes.md";
    let source_uri = "repo://notes.md";
    let title = "notes.md";
    let metadata = json!({});
    let kind = ChunkKind::from_path(std::path::Path::new(source_uri)).unwrap();
    let revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri,
        title,
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.to_owned(),
        title: title.to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256: revision,
        chunker_fingerprint: hsum::store::chunker_fingerprint(kind),
        chunks: chunk_bytes(body, kind, ChunkSettings::default())
            .unwrap()
            .into_iter()
            .map(|chunk| PreparedChunk {
                ordinal: chunk.ordinal(),
                byte_span: chunk.span(),
                line_span: chunk.line_span(),
                body_text: chunk.text().to_owned(),
                content_sha256: body_sha256(chunk.text().as_bytes()),
                quote_bloom: QuoteBloom::from_content(chunk.text().as_bytes()).into_bytes(),
                literals: prepare_passage_literals(title, source_uri, chunk.text().as_bytes()),
            })
            .collect(),
    }
}
