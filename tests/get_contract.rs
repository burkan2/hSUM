use hsum::domain::{ByteSpan, Citation, DocumentId, IndexId, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{
    ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, body_sha256, chunk_bytes,
    revision_sha256,
};
use hsum::search::{GetError, GetRequest};
use hsum::store::{
    DeleteConfirmations, FilesystemScope, IndexDb, OpenMode, PreparedChunk, PreparedDocument,
    prepare_passage_literals,
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

mod support;
use support::private_tempdir as tempdir;

#[test]
fn get_uses_the_immutable_revision_after_the_head_changes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let old = prepared_document(b"notes.md", "repo://notes.md", b"old alpha-beta\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&old),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let citation = citation_for(&path, &scope, &old, 0);
    let new = prepared_document(b"notes.md", "repo://notes.md", b"new alpha-beta\n");
    database
        .apply_filesystem_snapshot(&scope, &[new], &[], DeleteConfirmations::default())
        .unwrap();

    let response = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: citation.clone(),
            max_bytes: 16_384,
        })
        .unwrap();

    assert_eq!(response.requested_citation, citation);
    assert_eq!(response.returned_citation, citation);
    assert_eq!(response.content, b"old alpha-beta\n");
    assert!(response.untrusted_content);
}

#[test]
fn get_expands_whole_chunks_left_then_right_within_the_bound() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let document = prepared_three_chunks();
    assert!(document.chunks.len() >= 3);
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let middle = citation_for(&path, &scope, &document, 1);
    let left_start = document.chunks[0].byte_span.start() as usize;
    let middle_end = document.chunks[1].byte_span.end() as usize;
    let right_end = document.chunks[2].byte_span.end() as usize;
    let left_bound = middle_end - left_start;
    let complete_bound = right_end - left_start;

    let left_first = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: middle.clone(),
            max_bytes: left_bound,
        })
        .unwrap();
    assert_eq!(left_first.content, document.body[left_start..middle_end]);
    assert_eq!(
        left_first.returned_citation.span,
        ByteSpan::new(left_start as u64, middle_end as u64).unwrap()
    );

    let complete = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: middle,
            max_bytes: complete_bound,
        })
        .unwrap();
    assert_eq!(complete.content, document.body[left_start..right_end]);
    assert_eq!(
        complete.returned_citation.span,
        ByteSpan::new(left_start as u64, right_end as u64).unwrap()
    );
}

#[test]
fn get_binds_a_historical_shared_body_to_its_frozen_source_kind_layout() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let mut body = String::new();
    for index in 0..28 {
        body.push_str(&format!(
            "plain paragraph {index} carries enough repeated filler for a shared boundary\n\n"
        ));
    }
    for index in 0..140 {
        body.push_str(&format!("# Heading {index}\nfn item_{index}() {{}}\n\n"));
    }

    let markdown = prepared_document(b"shared.md", "repo://shared.md", body.as_bytes());
    let rust = prepared_document(b"shared.rs", "repo://shared.rs", body.as_bytes());
    assert!(markdown.chunks.len() >= 2);
    assert!(rust.chunks.len() >= 2);
    assert_eq!(markdown.chunks[0].byte_span, rust.chunks[0].byte_span);
    assert_ne!(markdown.chunks[1].byte_span, rust.chunks[1].byte_span);

    database
        .apply_filesystem_snapshot(
            &scope,
            &[markdown.clone(), rust.clone()],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let historical_citation = citation_for(&path, &scope, &markdown, 0);
    let updated_markdown = prepared_document(
        b"shared.md",
        "repo://shared.md",
        b"# Replacement\nThe markdown head has moved.\n",
    );
    database
        .apply_filesystem_snapshot(
            &scope,
            &[updated_markdown, rust],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();

    let expected_end = markdown.chunks[1].byte_span.end() as usize;
    let response = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: historical_citation,
            max_bytes: expected_end,
        })
        .unwrap();

    assert_eq!(
        response.returned_citation.span,
        ByteSpan::new(0, expected_end as u64).unwrap()
    );
    assert_eq!(response.content, body.as_bytes()[..expected_end]);
}

#[test]
fn get_rejects_a_bound_smaller_than_the_cited_passage_and_foreign_scope() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let document = prepared_document(b"notes.md", "repo://notes.md", b"alpha-beta\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let citation = citation_for(&path, &scope, &document, 0);

    let too_small = database
        .get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation: citation.clone(),
            max_bytes: 4,
        })
        .unwrap_err();
    assert!(matches!(
        too_small,
        GetError::BoundBelowPassage {
            requested: 4,
            required: 11,
        }
    ));

    let foreign = database
        .get_evidence(&GetRequest {
            project_id: ProjectId::new_v4(),
            citation,
            max_bytes: 16_384,
        })
        .unwrap_err();
    assert!(matches!(foreign, GetError::ScopeDenied));
}

#[test]
fn get_rejects_oversized_version_fields_before_materialization() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let document = prepared_document(b"notes.md", "repo://notes.md", b"bounded evidence\n");
    database
        .apply_filesystem_snapshot(
            &scope,
            std::slice::from_ref(&document),
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    let citation = citation_for(&path, &scope, &document, 0);
    drop(database);

    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE document_versions SET title = ?1 WHERE document_id = ?2",
            rusqlite::params![
                "x".repeat(16 * 1024 + 1),
                citation.document_id.as_uuid().as_bytes().as_slice(),
            ],
        )
        .unwrap();

    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        database.get_evidence(&GetRequest {
            project_id: scope.project_id,
            citation,
            max_bytes: 16_384,
        }),
        Err(GetError::Corrupt(
            "stored evidence exceeds alpha field bounds"
        ))
    ));
}

fn create_database(path: &std::path::Path) -> IndexDb {
    IndexDb::create(
        path,
        IndexId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap()),
    )
    .unwrap()
}

fn fixture_scope() -> FilesystemScope {
    FilesystemScope {
        source_id: SourceId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401").unwrap(),
        ),
        source_name: "fixture".parse::<SafeSlug>().unwrap(),
        source_logical_uri: "file:///fixture".to_owned(),
        source_config_json: r#"{"root":"/fixture"}"#.to_owned(),
        project_id: ProjectId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402").unwrap(),
        ),
        project_name: "fixture".parse::<SafeSlug>().unwrap(),
    }
}

fn prepared_document(connector_key: &[u8], source_uri: &str, body: &[u8]) -> PreparedDocument {
    prepared_with_chunks(connector_key, source_uri, body)
}

fn prepared_three_chunks() -> PreparedDocument {
    let body = format!(
        "# Zero\n{}\n\n# One\n{}\n\n# Two\n{}\n",
        "zero ".repeat(260),
        "one ".repeat(260),
        "two ".repeat(260)
    );
    prepared_with_chunks(b"notes.md", "repo://notes.md", body.as_bytes())
}

fn prepared_with_chunks(connector_key: &[u8], source_uri: &str, body: &[u8]) -> PreparedDocument {
    let metadata = json!({});
    let title = source_uri.rsplit('/').next().unwrap();
    let revision = revision_sha256(&SnapshotRevision {
        body,
        source_uri,
        title,
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    let chunks = chunk_bytes(
        body,
        ChunkKind::from_path(std::path::Path::new(source_uri)).unwrap(),
        ChunkSettings::default(),
    )
    .unwrap()
    .into_iter()
    .map(|chunk| {
        let bytes = chunk.text().as_bytes();
        PreparedChunk {
            ordinal: chunk.ordinal(),
            byte_span: chunk.span(),
            line_span: chunk.line_span(),
            body_text: chunk.text().to_owned(),
            content_sha256: body_sha256(bytes),
            quote_bloom: QuoteBloom::from_content(bytes).into_bytes(),
            literals: prepare_passage_literals(title, source_uri, bytes),
        }
    })
    .collect();

    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.to_owned(),
        title: title.to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256: revision,
        chunker_fingerprint: hsum::store::chunker_fingerprint(
            ChunkKind::from_path(std::path::Path::new(source_uri)).unwrap(),
        ),
        chunks,
    }
}

fn citation_for(
    path: &std::path::Path,
    scope: &FilesystemScope,
    document: &PreparedDocument,
    ordinal: usize,
) -> Citation {
    let raw_document_id: Vec<u8> = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT id FROM documents WHERE connector_key = ?1",
            [&document.connector_key],
            |row| row.get(0),
        )
        .unwrap();
    Citation {
        index_id: IndexId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap(),
        ),
        source_id: scope.source_id,
        document_id: DocumentId::from_uuid(Uuid::from_slice(&raw_document_id).unwrap()),
        revision: document.revision_sha256,
        span: document.chunks[ordinal].byte_span,
    }
}
