use std::path::Path;

use hsum::domain::{ByteSpan, IndexId, LineSpan, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{ChunkKind, QuoteBloom, SnapshotRevision, body_sha256, revision_sha256};
use hsum::search::{Retriever, SearchError, SearchMode, SearchRequest, SearchStopReason};
use hsum::store::{
    DeleteConfirmations, FilesystemScope, IndexDb, OpenMode, PreparedChunk, PreparedDocument,
    prepare_passage_literals,
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

mod support;
use support::private_tempdir as tempdir;

const INDEX_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
const SOURCE_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401";
const PROJECT_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402";

#[test]
fn search_request_enforces_alpha_bounds_before_touching_sqlite() {
    assert!(matches!(
        SearchRequest::new("alpha", SearchMode::Auto, 0, 3_000, false),
        Err(SearchError::InvalidLimit {
            requested: 0,
            minimum: 1,
            maximum: 50,
        })
    ));
    assert!(matches!(
        SearchRequest::new("alpha", SearchMode::Auto, 51, 3_000, false),
        Err(SearchError::InvalidLimit {
            requested: 51,
            minimum: 1,
            maximum: 50,
        })
    ));
    assert!(matches!(
        SearchRequest::new("alpha", SearchMode::Auto, 10, 99, false),
        Err(SearchError::InvalidDeadline {
            requested_ms: 99,
            minimum_ms: 100,
            maximum_ms: 10_000,
        })
    ));
    assert!(matches!(
        SearchRequest::new("alpha", SearchMode::Lexical, 10, 10_001, false),
        Err(SearchError::InvalidDeadline {
            requested_ms: 10_001,
            minimum_ms: 100,
            maximum_ms: 10_000,
        })
    ));
}

#[test]
fn valid_punctuation_only_query_returns_an_empty_success() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![document(
            b"one.md",
            "one.md",
            "repo://one.md",
            b"alpha beta\n",
        )],
    );

    let response = search(&database, scope.project_id, "*(){}[]", SearchMode::Auto, 10);

    assert!(response.results.is_empty());
    assert_eq!(response.stop_reason, SearchStopReason::UniqueExhausted);
    assert_eq!(response.requested_mode, SearchMode::Auto);
    assert_eq!(response.effective_mode, SearchMode::Lexical);
}

#[test]
fn exact_literal_and_bm25_fuse_into_versioned_immutable_evidence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"src/search.rs",
                "search.md",
                "repo://src/search.rs",
                b"EVIDENCE_FORGOTTEN alpha-beta\n",
            ),
            document(
                b"src/other.rs",
                "other.md",
                "repo://src/other.rs",
                b"ordinary alpha material\n",
            ),
        ],
    );

    let response = search(
        &database,
        scope.project_id,
        "alpha-beta",
        SearchMode::Auto,
        10,
    );

    assert_eq!(response.generation, Some(1));
    assert_eq!(response.index_epoch, 1);
    assert_eq!(response.scope_revision, 0);
    assert_eq!(
        response.retrievers,
        vec![Retriever::Exact, Retriever::Lexical]
    );
    let first = &response.results[0];
    assert_eq!(first.index_id, index_id());
    assert_eq!(first.source_id, scope.source_id);
    assert_eq!(first.source_uri, "repo://src/search.rs");
    assert_eq!(first.title, "search.md");
    assert_eq!(first.byte_span, ByteSpan::new(0, 30).unwrap());
    assert_eq!(first.line_span, LineSpan::new(1, 1).unwrap());
    assert_eq!(first.content, "EVIDENCE_FORGOTTEN alpha-beta\n");
    assert_eq!(first.content_sha256, body_sha256(first.content.as_bytes()));
    assert_eq!(first.head_generation, 1);
    assert!(first.untrusted_content);
    assert_eq!(first.citation().index_id, index_id());
    assert_eq!(first.citation().revision, first.revision_sha256);
    assert!(
        first
            .score
            .lists
            .iter()
            .any(|signal| signal.retriever == Retriever::Exact && signal.rank == 1)
    );
    assert!(
        first
            .score
            .lists
            .iter()
            .any(|signal| signal.retriever == Retriever::Lexical && signal.rank == 1)
    );
}

#[test]
fn ascii_phrase_uses_exact_verification_and_raw_quote_uses_bloom_verification() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"phrase.md",
                "phrase.md",
                "repo://phrase.md",
                b"the alpha beta phrase\n",
            ),
            document(
                b"raw.md",
                "raw.md",
                "repo://raw.md",
                b"operator a+b remains byte exact\n",
            ),
        ],
    );

    let phrase = search(
        &database,
        scope.project_id,
        "\"alpha beta\"",
        SearchMode::Lexical,
        10,
    );
    assert_eq!(phrase.results[0].source_uri, "repo://phrase.md");
    assert!(
        phrase.results[0]
            .score
            .lists
            .iter()
            .any(|signal| signal.retriever == Retriever::Exact)
    );

    let raw = search(&database, scope.project_id, "\"a+b\"", SearchMode::Auto, 10);
    assert_eq!(raw.results[0].source_uri, "repo://raw.md");
    assert!(raw.retrievers.contains(&Retriever::ExactFallback));
    assert!(
        raw.results[0]
            .score
            .lists
            .iter()
            .any(|signal| signal.retriever == Retriever::ExactFallback)
    );
}

#[test]
fn exact_matching_is_byte_sensitive_across_canonical_unicode_forms() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"decomposed.md",
                "decomposed.md",
                "repo://decomposed.md",
                "cafe\u{301}\n".as_bytes(),
            ),
            document(
                b"composed.md",
                "composed.md",
                "repo://composed.md",
                "café\n".as_bytes(),
            ),
        ],
    );

    let response = search(
        &database,
        scope.project_id,
        "\"café\"",
        SearchMode::Auto,
        10,
    );

    let exact_results = response
        .results
        .iter()
        .filter(|result| {
            result.score.lists.iter().any(|signal| {
                matches!(
                    signal.retriever,
                    Retriever::Exact | Retriever::ExactFallback
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(exact_results.len(), 1);
    assert_eq!(exact_results[0].source_uri, "repo://composed.md");
}

#[test]
fn user_fts_operators_are_compiled_as_data_and_never_leak_sql_errors() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![document(
            b"one.md",
            "one.md",
            "repo://one.md",
            b"alpha beta gamma\n",
        )],
    );
    let request = SearchRequest::new(
        "alpha) OR (beta* NEAR(title:gamma)",
        SearchMode::Lexical,
        10,
        3_000,
        true,
    )
    .unwrap();

    let response = database.search(scope.project_id, &request).unwrap();

    assert!(response.results.is_empty());
    assert_eq!(response.stop_reason, SearchStopReason::UniqueExhausted);
}

#[test]
fn literal_postings_are_only_hints_and_cannot_manufacture_exact_evidence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![document(
            b"plain.md",
            "plain.md",
            "repo://plain.md",
            b"ordinary immutable bytes\n",
        )],
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO passage_literals(passage_id, literal, field)
             SELECT id, ?1, 'body' FROM active_passages LIMIT 1",
            [b"forged-token".as_slice()],
        )
        .unwrap();
    drop(connection);

    let response = search(
        &database,
        scope.project_id,
        "forged-token",
        SearchMode::Lexical,
        10,
    );

    assert!(response.results.is_empty());
}

#[test]
fn long_and_post_cap_identifiers_use_the_byte_verified_scan_path() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    let long_identifier = format!("{}-tail", "a".repeat(130));
    let post_cap_identifier = "token-69";
    let many_identifiers = (0..70)
        .map(|index| format!("token-{index:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"long.md",
                "long.md",
                "repo://long.md",
                format!("prefix {long_identifier} suffix\n").as_bytes(),
            ),
            document(
                b"capped.md",
                "capped.md",
                "repo://capped.md",
                format!("{many_identifiers}\n").as_bytes(),
            ),
        ],
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    let post_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM passage_literals WHERE literal = ?1",
            [post_cap_identifier.as_bytes()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(post_count, 0, "fixture must exercise the posting cap");
    drop(connection);

    for query in [&long_identifier, post_cap_identifier] {
        let response = search(&database, scope.project_id, query, SearchMode::Lexical, 10);
        assert!(
            response.results.iter().any(|result| {
                result
                    .score
                    .lists
                    .iter()
                    .any(|signal| signal.retriever == Retriever::Exact)
            }),
            "{query}"
        );
    }
}

#[test]
fn project_membership_is_applied_before_any_result_limit() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        (0..60)
            .map(|index| {
                document(
                    format!("doc-{index}.md").as_bytes(),
                    &format!("doc-{index}.md"),
                    &format!("repo://doc-{index}.md"),
                    b"globally frequent needle-token\n",
                )
            })
            .collect(),
    );
    let empty_project =
        ProjectId::from_uuid(Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a4499").unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO projects(id, name, scope_revision, created_at)
             VALUES (?1, 'empty', 0, '2026-07-20T00:00:00Z')",
            [empty_project.as_uuid().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);

    let response = search(
        &database,
        empty_project,
        "needle-token",
        SearchMode::Lexical,
        10,
    );

    assert!(response.results.is_empty());
    assert_eq!(response.examined.lexical, 0);
}

#[test]
fn content_hash_dedupe_retains_duplicate_citations_and_adapts_past_depth_fifty() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        (0..60)
            .map(|index| {
                document(
                    format!("same-{index}.md").as_bytes(),
                    &format!("same-{index}.md"),
                    &format!("repo://same-{index}.md"),
                    b"shared lexical evidence\n",
                )
            })
            .collect(),
    );

    let response = search(
        &database,
        scope.project_id,
        "shared",
        SearchMode::Lexical,
        10,
    );

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].duplicate_citations.len(), 59);
    assert_eq!(response.examined.lexical, 60);
    assert_eq!(response.stop_reason, SearchStopReason::UniqueExhausted);
}

#[test]
fn exact_candidates_adapt_from_fifty_to_the_unique_exhaustion_point() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        (0..60)
            .map(|index| {
                document(
                    format!("exact-{index}.md").as_bytes(),
                    &format!("exact-{index}.md"),
                    &format!("repo://exact-{index}.md"),
                    b"shared-id exact evidence\n",
                )
            })
            .collect(),
    );

    let response = search(
        &database,
        scope.project_id,
        "shared-id",
        SearchMode::Lexical,
        10,
    );

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].duplicate_citations.len(), 59);
    assert_eq!(response.examined.exact, 60);
    assert_eq!(response.stop_reason, SearchStopReason::UniqueExhausted);
}

#[test]
fn raw_exact_fallback_stops_at_the_five_hundred_candidate_budget() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        (0..501)
            .map(|index| {
                document(
                    format!("budget-{index}.md").as_bytes(),
                    &format!("budget-{index}.md"),
                    &format!("repo://budget-{index}.md"),
                    format!("ordinary evidence number {index}\n").as_bytes(),
                )
            })
            .collect(),
    );

    let response = search(
        &database,
        scope.project_id,
        "\"+=+\"",
        SearchMode::Lexical,
        10,
    );

    assert!(response.results.is_empty());
    assert_eq!(response.examined.exact_fallback, 500);
    assert_eq!(response.stop_reason, SearchStopReason::WorkBudgetExhausted);
}

#[test]
fn auto_and_lexical_have_identical_alpha_ranking_without_vector_surface() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"a.md",
                "a.md",
                "repo://a.md",
                b"alpha-beta evidence alpha\n",
            ),
            document(b"b.md", "b.md", "repo://b.md", b"alpha lexical evidence\n"),
        ],
    );

    let auto = search(
        &database,
        scope.project_id,
        "alpha-beta",
        SearchMode::Auto,
        10,
    );
    let lexical = search(
        &database,
        scope.project_id,
        "alpha-beta",
        SearchMode::Lexical,
        10,
    );

    let auto_order = auto
        .results
        .iter()
        .map(|result| (result.citation(), result.score.fusion_units))
        .collect::<Vec<_>>();
    let lexical_order = lexical
        .results
        .iter()
        .map(|result| (result.citation(), result.score.fusion_units))
        .collect::<Vec<_>>();
    assert_eq!(auto_order, lexical_order);
    assert_eq!(auto.effective_mode, SearchMode::Lexical);
    assert_eq!(lexical.effective_mode, SearchMode::Lexical);
    assert_eq!(auto.retrievers, vec![Retriever::Exact, Retriever::Lexical]);
}

#[test]
fn search_rejects_a_tampered_quote_bloom_before_using_it_as_a_negative_gate() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![document(
            b"raw.md",
            "raw.md",
            "repo://raw.md",
            b"operator a+b remains byte exact\n",
        )],
    );
    drop(database);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE chunks SET quote_bloom = zeroblob(512)", [])
        .unwrap();
    drop(connection);

    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    let request = SearchRequest::new("\"a+b\"", SearchMode::Auto, 10, 3_000, false).unwrap();
    let error = database.search(scope.project_id, &request).unwrap_err();
    assert!(matches!(
        error,
        SearchError::Corrupt("quote Bloom does not match passage content")
    ));
}

#[test]
fn search_rejects_an_oversized_nonwinning_candidate_before_materialization() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("index.sqlite");
    let mut database = create_database(&path);
    let scope = fixture_scope();
    ingest(
        &mut database,
        &scope,
        vec![
            document(
                b"a.md",
                "a.md",
                "repo://a.md",
                b"alpha deterministic candidate a\n",
            ),
            document(
                b"b.md",
                "b.md",
                "repo://b.md",
                b"alpha deterministic candidate b\n",
            ),
        ],
    );

    let request = SearchRequest::new("alpha", SearchMode::Auto, 1, 3_000, false).unwrap();
    let winner = database
        .search(scope.project_id, &request)
        .unwrap()
        .results
        .remove(0)
        .document_id;
    drop(database);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE document_versions
             SET title = ?1
             WHERE document_id = (
                 SELECT id
                 FROM documents
                 WHERE id != ?2
                 ORDER BY id
                 LIMIT 1
             )",
            rusqlite::params![
                "x".repeat(16 * 1024 + 1),
                winner.as_uuid().as_bytes().as_slice(),
            ],
        )
        .unwrap();
    drop(connection);

    let database = IndexDb::open_existing(&path, OpenMode::ReadOnly).unwrap();
    assert!(matches!(
        database.search(scope.project_id, &request),
        Err(SearchError::Corrupt(
            "stored search candidate exceeds alpha field bounds"
        ))
    ));
}

fn search(
    database: &IndexDb,
    project_id: ProjectId,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> hsum::search::SearchResponse {
    let request = SearchRequest::new(query, mode, limit, 3_000, true).unwrap();
    database.search(project_id, &request).unwrap()
}

fn create_database(path: &Path) -> IndexDb {
    IndexDb::create(path, index_id()).unwrap()
}

fn index_id() -> IndexId {
    IndexId::from_uuid(Uuid::parse_str(INDEX_UUID).unwrap())
}

fn fixture_scope() -> FilesystemScope {
    FilesystemScope {
        source_id: SourceId::from_uuid(Uuid::parse_str(SOURCE_UUID).unwrap()),
        source_name: "fixture".parse::<SafeSlug>().unwrap(),
        source_logical_uri: "file:///fixture".to_owned(),
        source_config_json: r#"{"root":"/fixture"}"#.to_owned(),
        project_id: ProjectId::from_uuid(Uuid::parse_str(PROJECT_UUID).unwrap()),
        project_name: "fixture".parse::<SafeSlug>().unwrap(),
    }
}

fn ingest(database: &mut IndexDb, scope: &FilesystemScope, documents: Vec<PreparedDocument>) {
    database
        .apply_filesystem_snapshot(scope, &documents, &[], DeleteConfirmations::default())
        .unwrap();
}

fn document(connector_key: &[u8], title: &str, source_uri: &str, body: &[u8]) -> PreparedDocument {
    let metadata = json!({});
    let revision_sha256 = revision_sha256(&SnapshotRevision {
        body,
        source_uri,
        title,
        metadata: &metadata,
        source_updated_at: None,
    })
    .unwrap();
    let line_count = body.iter().filter(|byte| **byte == b'\n').count().max(1);

    PreparedDocument {
        connector_key: connector_key.to_vec(),
        source_uri: source_uri.to_owned(),
        title: title.to_owned(),
        metadata_json: "{}".to_owned(),
        source_updated_at: None,
        body: body.to_vec(),
        body_sha256: body_sha256(body),
        revision_sha256,
        chunker_fingerprint: hsum::store::chunker_fingerprint(
            ChunkKind::from_path(Path::new(source_uri)).unwrap(),
        ),
        chunks: vec![PreparedChunk {
            ordinal: 0,
            byte_span: ByteSpan::new(0, body.len() as u64).unwrap(),
            line_span: LineSpan::new(1, line_count as u64).unwrap(),
            body_text: std::str::from_utf8(body).unwrap().to_owned(),
            content_sha256: body_sha256(body),
            quote_bloom: QuoteBloom::from_content(body).into_bytes(),
            literals: prepare_passage_literals(title, source_uri, body),
        }],
    }
}
