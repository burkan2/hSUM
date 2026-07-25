use std::time::Duration;

use hsum::domain::{Citation, IndexId, ProjectId, SafeSlug, SourceId};
use hsum::ingest::{
    ChunkKind, ChunkSettings, QuoteBloom, SnapshotRevision, body_sha256, chunk_bytes,
    revision_sha256,
};
use hsum::mcp::{
    EvidenceGetInput, EvidenceProjectInput, EvidenceSearchInput, EvidenceSearchMode,
    EvidenceStatusInput, FrameValidationError, HsumMcpServer, MAX_MCP_FRAME_BYTES,
    MAX_MCP_GET_BODY_BYTES, MAX_MCP_NESTING_DEPTH, MAX_MCP_RESPONSE_BYTES,
    MAX_MCP_SEARCH_BODY_BYTES, McpServerError, retrieval_config_fingerprint, serve_io,
    validate_frame, validate_response_size,
};
use hsum::store::{
    DeleteConfirmations, FilesystemScope, IndexDb, OpenMode, PreparedChunk, PreparedDocument,
    prepare_passage_literals,
};
use rmcp::ServerHandler;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

const INDEX_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
const SOURCE_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a4401";
const PROJECT_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402";
static HEAVY_MCP_TEST_STATE: std::sync::LazyLock<(std::sync::Mutex<bool>, std::sync::Condvar)> =
    std::sync::LazyLock::new(|| (std::sync::Mutex::new(false), std::sync::Condvar::new()));

struct HeavyMcpTestGuard;

impl Drop for HeavyMcpTestGuard {
    fn drop(&mut self) {
        let (lock, available) = &*HEAVY_MCP_TEST_STATE;
        let mut active = lock.lock().unwrap();
        *active = false;
        available.notify_one();
    }
}

fn heavy_mcp_test_guard() -> HeavyMcpTestGuard {
    let (lock, available) = &*HEAVY_MCP_TEST_STATE;
    let mut active = lock.lock().unwrap();
    while *active {
        active = available.wait(active).unwrap();
    }
    *active = true;
    HeavyMcpTestGuard
}

#[test]
fn server_advertises_exactly_four_read_only_project_bound_tools() {
    let fixture = fixture();
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let tools = server.tool_definitions();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "evidence_get",
            "evidence_project",
            "evidence_search",
            "evidence_status"
        ]
    );

    for tool in &tools {
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            tool.input_schema.get("x-hsum-api-version"),
            Some(&Value::String("hsum.api.v1".to_owned()))
        );
        let input_schema_id = tool
            .input_schema
            .get("$id")
            .and_then(Value::as_str)
            .unwrap();
        assert!(input_schema_id.starts_with("urn:hsum:schema:"));
        assert!(input_schema_id.contains(tool.name.as_ref()));
        let output = tool.output_schema.as_ref().unwrap();
        assert_eq!(
            output.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            output.get("x-hsum-api-version"),
            Some(&Value::String("hsum.api.v1".to_owned()))
        );
        let output_properties = output
            .get("properties")
            .and_then(Value::as_object)
            .expect("tool output properties");
        assert!(output_properties.contains_key("schema_version"));
        assert!(output_properties.contains_key("request_id"));
        assert!(!output_properties.contains_key("api_version"));
        let annotations = tool.annotations.as_ref().unwrap();
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
    }

    assert_schema_properties(
        &tools,
        "evidence_search",
        &["cursor", "explain", "limit", "mode", "query", "timeout_ms"],
    );
    assert_schema_properties(
        &tools,
        "evidence_get",
        &["citation_uri", "max_bytes", "verify_source_hash"],
    );
    assert_schema_properties(&tools, "evidence_project", &[]);
    assert_schema_properties(&tools, "evidence_status", &[]);
    for tool in &tools {
        let schema = serde_json::to_string(&tool.input_schema).unwrap();
        assert!(!schema.contains("\"project_id\""));
        assert!(!schema.contains("\"index_id\""));
    }
}

#[test]
fn serde_inputs_reject_project_index_and_other_unknown_overrides() {
    assert!(
        serde_json::from_value::<EvidenceSearchInput>(json!({
            "query": "alpha",
            "project_id": PROJECT_UUID
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<EvidenceGetInput>(json!({
            "citation_uri": "not-a-citation",
            "index": "other"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<EvidenceProjectInput>(json!({"project": "other"})).is_err());
    assert!(serde_json::from_value::<EvidenceStatusInput>(json!({"extra": true})).is_err());
}

#[test]
fn framing_rejects_size_depth_duplicates_and_bounded_container_abuse() {
    let oversized = vec![b' '; MAX_MCP_FRAME_BYTES + 1];
    assert_eq!(
        validate_frame(&oversized),
        Err(FrameValidationError::TooLarge)
    );

    let depth_32 = format!(
        "{}0{}",
        "[".repeat(MAX_MCP_NESTING_DEPTH),
        "]".repeat(MAX_MCP_NESTING_DEPTH)
    );
    assert_eq!(validate_frame(depth_32.as_bytes()), Ok(()));
    let depth_33 = format!(
        "{}0{}",
        "[".repeat(MAX_MCP_NESTING_DEPTH + 1),
        "]".repeat(MAX_MCP_NESTING_DEPTH + 1)
    );
    assert_eq!(
        validate_frame(depth_33.as_bytes()),
        Err(FrameValidationError::TooDeep)
    );

    assert_eq!(
        validate_frame(br#"{"jsonrpc":"2.0","id":1,"id":2,"method":"ping"}"#),
        Err(FrameValidationError::DuplicateKey)
    );
    let long_string = format!(r#"{{"value":"{}"}}"#, "x".repeat(256 * 1024 + 1));
    assert_eq!(
        validate_frame(long_string.as_bytes()),
        Err(FrameValidationError::StringTooLong)
    );
    let too_many_items = format!("[{}]", "0,".repeat(4097).trim_end_matches(','));
    assert_eq!(
        validate_frame(too_many_items.as_bytes()),
        Err(FrameValidationError::TooManyItems)
    );

    let many_nested_nodes = format!("[{}]", "[0,0,0,0],".repeat(4096).trim_end_matches(','));
    assert_eq!(
        validate_frame(many_nested_nodes.as_bytes()),
        Err(FrameValidationError::TooManyItems)
    );
}

#[test]
fn response_and_body_caps_are_fixed_protocol_constants() {
    assert_eq!(MAX_MCP_RESPONSE_BYTES, 512 * 1024);
    assert_eq!(MAX_MCP_SEARCH_BODY_BYTES, 256 * 1024);
    assert_eq!(MAX_MCP_GET_BODY_BYTES, 64 * 1024);
    assert!(validate_response_size(&json!({"content": "x".repeat(1_000)})).is_ok());
    assert!(
        validate_response_size(&json!({
            "content": "x".repeat(MAX_MCP_RESPONSE_BYTES)
        }))
        .is_err()
    );
}

#[test]
fn retrieval_config_fingerprint_is_frozen_for_alpha_one() {
    assert_eq!(
        retrieval_config_fingerprint(),
        "e10f7cefc58da192422751b4fec7cf845677e0cdfea8177138e2671a297c325a"
    );
}

#[test]
fn handlers_use_hardened_readers_and_keep_foreign_citations_non_disclosing() {
    let fixture = fixture();
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    assert!(server.database_is_hardened_read_only().unwrap());
    assert_eq!(server.bound_project_id(), fixture.scope.project_id);

    let project = server
        .evidence_project(Parameters(EvidenceProjectInput::default()))
        .unwrap()
        .0;
    assert_eq!(project.schema_version, "hsum.api.v1");
    assert!(Uuid::parse_str(&project.request_id).is_ok());
    assert_eq!(project.project_id, PROJECT_UUID);
    assert_eq!(project.sources.len(), 1);
    assert_eq!(project.sources[0].source_id, SOURCE_UUID);

    let status = server
        .evidence_status(Parameters(EvidenceStatusInput::default()))
        .unwrap()
        .0;
    assert_eq!(status.schema_version, "hsum.api.v1");
    assert!(Uuid::parse_str(&status.request_id).is_ok());
    assert!(status.read_only);
    assert!(status.query_only);
    assert_eq!(status.document_count, 1);
    assert_eq!(status.passage_count, 1);

    let search = server
        .evidence_search(Parameters(EvidenceSearchInput {
            query: "alpha-beta".to_owned(),
            mode: Some(EvidenceSearchMode::Auto),
            limit: Some(10),
            cursor: None,
            timeout_ms: Some(3_000),
            explain: Some(true),
        }))
        .unwrap()
        .0;
    assert_eq!(search.schema_version, "hsum.api.v1");
    assert!(Uuid::parse_str(&search.request_id).is_ok());
    assert_eq!(search.results.len(), 1);
    assert!(search.body_bytes <= MAX_MCP_SEARCH_BODY_BYTES);
    assert!(search.results[0].untrusted_content);

    let get = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: search.results[0].citation_uri.clone(),
            max_bytes: Some(MAX_MCP_GET_BODY_BYTES),
            verify_source_hash: Some(false),
        }))
        .unwrap()
        .0;
    assert_eq!(get.schema_version, "hsum.api.v1");
    assert!(Uuid::parse_str(&get.request_id).is_ok());
    assert_eq!(get.content, "alpha-beta local evidence\n");
    assert_eq!(get.source_state, "missing_since_ingest");
    assert_eq!(get.source_hash_verification, "not_requested");

    let mut foreign = search.results[0].citation_uri.parse::<Citation>().unwrap();
    foreign.source_id = SourceId::new_v4();
    let foreign_error = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: foreign.to_string(),
            max_bytes: None,
            verify_source_hash: None,
        }))
        .err()
        .expect("foreign citation must fail");
    let missing_error = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: Citation {
                document_id: hsum::domain::DocumentId::new_v4(),
                ..search.results[0].citation_uri.parse::<Citation>().unwrap()
            }
            .to_string(),
            max_bytes: None,
            verify_source_hash: None,
        }))
        .err()
        .expect("missing citation must fail");
    assert_eq!(foreign_error.code, missing_error.code);
    assert_eq!(foreign_error.message, missing_error.message);
    assert_eq!(
        foreign_error.message,
        "the requested evidence is unavailable"
    );
    assert_public_error(&foreign_error, "NOT_FOUND", "EVIDENCE_NOT_FOUND", false);

    let wrong_project = ProjectId::new_v4();
    assert!(matches!(
        HsumMcpServer::new(&fixture.path, wrong_project),
        Err(McpServerError::ProjectNotFound)
    ));
}

#[test]
fn status_includes_shared_actionable_index_problems() {
    let fixture = fixture_with_documents(Vec::new());
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let status = server
        .evidence_status(Parameters(EvidenceStatusInput::default()))
        .unwrap()
        .0;
    assert_eq!(status.document_count, 0);
    assert_eq!(status.passage_count, 0);
    assert!(status.index_problems.iter().any(|problem| matches!(
        problem.code.as_str(),
        "NO_ACTIVE_GENERATION" | "NO_ACTIVE_DOCUMENTS"
    )));
    assert!(
        status
            .index_problems
            .iter()
            .all(|problem| { !problem.summary.is_empty() && !problem.repair_command.is_empty() })
    );
}

#[test]
fn status_accepts_legal_persisted_error_detail_and_bounds_the_wire_value() {
    let fixture = fixture();
    let detail = "é".repeat(8_000);
    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE sources
             SET last_error_code = 'CONNECTOR_KEY_REJECTED',
                 last_error_detail = ?1,
                 last_error_at = '2099-01-01T00:00:00Z'
             WHERE id = ?2",
            rusqlite::params![
                detail,
                Uuid::parse_str(SOURCE_UUID).unwrap().as_bytes().as_slice()
            ],
        )
        .unwrap();
    drop(connection);

    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let status = server
        .evidence_status(Parameters(EvidenceStatusInput::default()))
        .unwrap()
        .0;
    let issue = status
        .health_issues
        .iter()
        .find(|issue| issue.code == "CONNECTOR_KEY_REJECTED")
        .expect("persisted source error should remain visible");
    assert_eq!(issue.detail.len(), 4 * 1024);
    assert!(issue.detail.is_char_boundary(issue.detail.len()));
}

#[test]
fn project_and_status_packets_each_come_from_one_wal_snapshot() {
    const A_TIME: &str = "2099-01-01T00:00:00Z";
    const B_TIME: &str = "2099-01-01T00:00:01Z";

    let fixture = fixture();
    set_snapshot_state(&fixture.path, 100, 100, A_TIME, false);
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let writer_path = fixture.path.clone();
    let writer = std::thread::spawn(move || {
        for iteration in 0..250 {
            if iteration % 2 == 0 {
                set_snapshot_state(&writer_path, 200, 200, B_TIME, true);
            } else {
                set_snapshot_state(&writer_path, 100, 100, A_TIME, false);
            }
        }
    });

    for _ in 0..100 {
        let project = server
            .evidence_project(Parameters(EvidenceProjectInput::default()))
            .unwrap()
            .0;
        let project_pair = (
            project.scope_revision,
            project.sources[0].last_success_at.as_deref(),
        );
        assert!(
            matches!(project_pair, (100, Some(A_TIME)) | (200, Some(B_TIME))),
            "mixed project snapshot: {project_pair:?}"
        );

        let status = server
            .evidence_status(Parameters(EvidenceStatusInput::default()))
            .unwrap()
            .0;
        match status.index_epoch {
            100 => {
                assert!(status.health_issues.is_empty());
                assert!(
                    status
                        .index_problems
                        .iter()
                        .all(|problem| problem.code != "SOURCE_SYNC_PARTIAL")
                );
            }
            200 => {
                assert_eq!(status.health_issues.len(), 1);
                assert!(
                    status
                        .index_problems
                        .iter()
                        .any(|problem| problem.code == "SOURCE_SYNC_PARTIAL")
                );
            }
            epoch => panic!("unexpected snapshot epoch {epoch}"),
        }
    }
    writer.join().unwrap();
}

#[test]
fn get_source_hash_probe_runs_after_the_stored_read_and_reports_drift() {
    let directory = tempdir().unwrap();
    harden_fixture_directory(directory.path());
    let path = directory.path().join("index.sqlite");
    let source_path = directory.path().join("notes.md");
    std::fs::write(&source_path, b"alpha-beta local evidence\n").unwrap();
    let root = directory.path().to_str().unwrap();
    let mut scope = fixture_scope();
    scope.source_logical_uri = root.to_owned();
    scope.source_config_json = json!({"root": root}).to_string();
    let mut database = IndexDb::create(&path, index_id()).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[document(
                b"notes.md",
                "notes.md",
                "repo://notes.md",
                b"alpha-beta local evidence\n",
            )],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let server = HsumMcpServer::new(&path, scope.project_id).unwrap();
    let citation = server
        .evidence_search(Parameters(search_input("alpha-beta", 1, None)))
        .unwrap()
        .0
        .results
        .remove(0)
        .citation_uri;

    let unchanged = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: citation.clone(),
            max_bytes: None,
            verify_source_hash: Some(true),
        }))
        .unwrap()
        .0;
    assert_eq!(unchanged.source_hash_verification, "unchanged");
    assert_eq!(unchanged.source_state, "content_unchanged");
    assert_eq!(unchanged.content, "alpha-beta local evidence\n");

    std::fs::write(&source_path, b"omega-beta local evidence\n").unwrap();
    let changed = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: citation.clone(),
            max_bytes: None,
            verify_source_hash: Some(true),
        }))
        .unwrap()
        .0;
    assert_eq!(changed.source_hash_verification, "changed");
    assert_eq!(changed.source_state, "changed_since_ingest");
    assert_eq!(changed.content, "alpha-beta local evidence\n");

    let mut database = IndexDb::open_existing(&path, OpenMode::ReadWrite).unwrap();
    database
        .apply_filesystem_snapshot(
            &scope,
            &[document(
                b"notes.md",
                "notes.md",
                "repo://notes.md",
                b"omega-beta local evidence\n",
            )],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let old_revision = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: citation,
            max_bytes: None,
            verify_source_hash: Some(true),
        }))
        .unwrap()
        .0;
    assert_eq!(old_revision.source_hash_verification, "changed");
    assert_eq!(old_revision.source_state, "changed_since_ingest");
    assert_eq!(old_revision.content, "alpha-beta local evidence\n");
}

#[test]
fn get_rejects_oversized_source_config_before_materializing_it() {
    let fixture = fixture();
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let citation = server
        .evidence_search(Parameters(search_input("alpha-beta", 1, None)))
        .unwrap()
        .0
        .results
        .remove(0)
        .citation_uri;

    let oversized_config =
        "x".repeat(hsum::app::MAX_FILESYSTEM_SOURCE_CONFIG_BYTES.saturating_add(1));
    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE sources SET config_json = ?1 WHERE id = ?2",
            rusqlite::params![
                oversized_config,
                Uuid::parse_str(SOURCE_UUID).unwrap().as_bytes().as_slice()
            ],
        )
        .unwrap();
    drop(connection);

    let error = server
        .evidence_get(Parameters(EvidenceGetInput {
            citation_uri: citation,
            max_bytes: None,
            verify_source_hash: Some(false),
        }))
        .err()
        .expect("oversized source config must not be materialized by evidence_get");
    assert_public_error(&error, "RESOURCE_EXHAUSTED", "MEMORY_BUDGET", false);
}

#[test]
fn cursor_is_opaque_and_distinguishes_malformed_query_and_state_changes() {
    let fixture = fixture_with_documents(vec![
        document(b"a.md", "a.md", "repo://a.md", b"alpha evidence a\n"),
        document(b"b.md", "b.md", "repo://b.md", b"alpha evidence b\n"),
    ]);
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let first = server
        .evidence_search(Parameters(search_input("alpha", 1, None)))
        .unwrap()
        .0;
    let generation_cursor = first.next_cursor.unwrap();
    assert!(generation_cursor.starts_with("v1."));
    assert!(!generation_cursor.contains(':'));
    assert!(
        generation_cursor[3..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    );
    let mut tampered_bytes = generation_cursor.clone().into_bytes();
    let last = tampered_bytes.len() - 1;
    tampered_bytes[last] = if tampered_bytes[last] == b'A' {
        b'B'
    } else {
        b'A'
    };
    let tampered_offset = String::from_utf8(tampered_bytes).unwrap();
    let offset_error = server
        .evidence_search(Parameters(search_input("alpha", 1, Some(tampered_offset))))
        .err()
        .expect("cursor payload is integrity protected");
    assert_public_error(&offset_error, "INVALID_ARGUMENT", "QUERY_SYNTAX", false);

    let query_error = server
        .evidence_search(Parameters(search_input(
            "different-query",
            1,
            Some(generation_cursor.clone()),
        )))
        .err()
        .expect("cursor query binding must be distinct from malformed input");
    assert_public_error(&query_error, "STALE_CURSOR", "QUERY_FINGERPRINT", false);

    let mut database = IndexDb::open_existing(&fixture.path, OpenMode::ReadWrite).unwrap();
    database
        .apply_filesystem_snapshot(
            &fixture.scope,
            &[
                document(
                    b"a.md",
                    "a.md",
                    "repo://a.md",
                    b"alpha evidence a changed\n",
                ),
                document(b"b.md", "b.md", "repo://b.md", b"alpha evidence b\n"),
            ],
            &[],
            DeleteConfirmations::default(),
        )
        .unwrap();
    drop(database);
    let stale_generation = server
        .evidence_search(Parameters(search_input(
            "alpha",
            1,
            Some(generation_cursor),
        )))
        .err()
        .expect("cursor must become stale after a committed generation");
    assert_public_error(&stale_generation, "STALE_CURSOR", "INDEX_EPOCH", false);

    let fresh = server
        .evidence_search(Parameters(search_input("alpha", 1, None)))
        .unwrap()
        .0;
    let index_cursor = fresh.next_cursor.unwrap();
    let replacement_index_id = Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44b0").unwrap();
    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'index_uuid'",
            [replacement_index_id.as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    let stale_index = server
        .evidence_search(Parameters(search_input("alpha", 1, Some(index_cursor))))
        .err()
        .expect("cursor must become stale after index identity changes");
    assert_public_error(&stale_index, "STALE_CURSOR", "INDEX_EPOCH", false);

    let scope_cursor = server
        .evidence_search(Parameters(search_input("alpha", 1, None)))
        .unwrap()
        .0
        .next_cursor
        .unwrap();
    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [fixture.scope.project_id.as_uuid().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    let stale_scope = server
        .evidence_search(Parameters(search_input("alpha", 1, Some(scope_cursor))))
        .err()
        .expect("cursor must become stale after a scope revision");
    assert_public_error(&stale_scope, "STALE_CURSOR", "SCOPE_REVISION", false);

    let generation_fixture = fixture_with_documents(vec![
        document(b"ga.md", "ga.md", "repo://ga.md", b"alpha evidence ga\n"),
        document(b"gb.md", "gb.md", "repo://gb.md", b"alpha evidence gb\n"),
    ]);
    let generation_server = HsumMcpServer::new(
        &generation_fixture.path,
        generation_fixture.scope.project_id,
    )
    .unwrap();
    let generation_only_cursor = generation_server
        .evidence_search(Parameters(search_input("alpha", 1, None)))
        .unwrap()
        .0
        .next_cursor
        .unwrap();
    let connection = rusqlite::Connection::open(&generation_fixture.path).unwrap();
    connection
        .execute(
            "INSERT INTO generations(
                 state, created_at, committed_at, pipeline_fingerprint
             )
             SELECT 'committed', '2099-01-01T00:00:00Z',
                    '2099-01-01T00:00:00Z', pipeline_fingerprint
             FROM generations
             ORDER BY id DESC LIMIT 1",
            [],
        )
        .unwrap();
    let replacement_generation = connection.last_insert_rowid().to_string();
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'active_generation'",
            [replacement_generation.as_bytes()],
        )
        .unwrap();
    drop(connection);
    let stale_generation_only = generation_server
        .evidence_search(Parameters(search_input(
            "alpha",
            1,
            Some(generation_only_cursor),
        )))
        .err()
        .expect("generation-only changes have their own stale cause");
    assert_public_error(&stale_generation_only, "STALE_CURSOR", "GENERATION", false);
}

#[test]
fn pagination_slices_one_stable_top_fifty_ordering() {
    let duplicate_documents = (0..70).map(|index| {
        let key = format!("a-duplicate-{index:03}.md");
        let uri = format!("repo://{key}");
        document(
            key.as_bytes(),
            &key,
            &uri,
            b"alpha shared duplicate evidence\n",
        )
    });
    let unique_documents = (0..70).map(|index| {
        let key = format!("z-unique-{index:03}.md");
        let uri = format!("repo://{key}");
        let body = format!("alpha shared unique evidence marker-{index:03}\n");
        document(key.as_bytes(), &key, &uri, body.as_bytes())
    });
    let documents = duplicate_documents.chain(unique_documents).collect();
    let fixture = fixture_with_documents(documents);
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();

    let expected = server
        .evidence_search(Parameters(search_input("alpha", 20, None)))
        .unwrap()
        .0
        .results
        .into_iter()
        .map(|passage| passage.citation_uri)
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 20);

    let first = server
        .evidence_search(Parameters(search_input("alpha", 7, None)))
        .unwrap()
        .0;
    let second = server
        .evidence_search(Parameters(search_input(
            "alpha",
            7,
            first.next_cursor.clone(),
        )))
        .unwrap()
        .0;
    let third = server
        .evidence_search(Parameters(search_input(
            "alpha",
            6,
            second.next_cursor.clone(),
        )))
        .unwrap()
        .0;
    let paged = first
        .results
        .into_iter()
        .chain(second.results)
        .chain(third.results)
        .map(|passage| passage.citation_uri)
        .collect::<Vec<_>>();
    assert_eq!(paged, expected);
}

#[test]
fn wire_response_cap_paginates_real_content_without_empty_or_repeated_pages() {
    let _serial = heavy_mcp_test_guard();
    // Fifty canonical 1,800-byte chunks carry only 90 KiB of raw body, so the
    // 256 KiB aggregate body cap is unreachable on this normal path and stays
    // defense-in-depth; the reachable pagination boundary is the serialized
    // 512 KiB wire cap, which the escape-heavy fill crosses deterministically.
    let documents = (0..50)
        .map(|index| {
            let key = format!("body-cap-{index:02}.md");
            let uri = format!("repo://{key}");
            let body = escape_heavy_body(format!("alpha marker-{index:02} "));
            document(key.as_bytes(), &key, &uri, &body)
        })
        .collect();
    let fixture = fixture_with_documents(documents);
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();

    let first = server
        .evidence_search(Parameters(EvidenceSearchInput {
            timeout_ms: Some(10_000),
            explain: Some(false),
            ..search_input("alpha", 50, None)
        }))
        .unwrap()
        .0;
    assert!(!first.results.is_empty());
    assert!(first.results.len() < 50);
    assert!(first.truncated);
    assert!(first.body_bytes > 64 * 1024);
    assert!(first.body_bytes <= MAX_MCP_SEARCH_BODY_BYTES);
    assert_eq!(
        first.body_bytes,
        first
            .results
            .iter()
            .map(|passage| passage.content.len())
            .sum::<usize>()
    );
    let first_citations = first
        .results
        .iter()
        .map(|passage| passage.citation_uri.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let cursor = first.next_cursor.expect("body cap must produce a cursor");

    let second = server
        .evidence_search(Parameters(EvidenceSearchInput {
            timeout_ms: Some(10_000),
            explain: Some(false),
            ..search_input("alpha", 50, Some(cursor))
        }))
        .unwrap()
        .0;
    assert!(!second.results.is_empty());
    assert!(
        second
            .results
            .iter()
            .all(|passage| !first_citations.contains(&passage.citation_uri))
    );
    assert_eq!(first.results.len() + second.results.len(), 50);
    assert!(second.next_cursor.is_none());
}

#[test]
fn first_passage_over_aggregate_body_cap_returns_resource_error_without_cursor() {
    let _serial = heavy_mcp_test_guard();
    let fixture = fixture();
    rewrite_search_passages(&fixture.path, |_| {
        let mut content = b"alpha-beta ".to_vec();
        content.resize(MAX_MCP_SEARCH_BODY_BYTES + 1, b'x');
        content
    });
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let result = server.evidence_search(Parameters(EvidenceSearchInput {
        query: "alpha-beta".to_owned(),
        mode: Some(EvidenceSearchMode::Lexical),
        limit: Some(1),
        cursor: None,
        timeout_ms: Some(10_000),
        explain: Some(false),
    }));
    let error = match result {
        Ok(output) => panic!(
            "unpageable result unexpectedly succeeded with {} results and {} body bytes",
            output.0.results.len(),
            output.0.body_bytes
        ),
        Err(error) => error,
    };
    assert_public_error(&error, "RESOURCE_EXHAUSTED", "MEMORY_BUDGET", false);
}

#[test]
fn oversized_nonwinning_candidate_is_rejected_before_unbounded_materialization() {
    let _serial = heavy_mcp_test_guard();
    let fixture = fixture_with_documents(vec![
        document(b"a.md", "a.md", "repo://a.md", b"alpha candidate a\n"),
        document(b"b.md", "b.md", "repo://b.md", b"alpha candidate b\n"),
    ]);
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let winner = server
        .evidence_search(Parameters(EvidenceSearchInput {
            timeout_ms: Some(10_000),
            explain: Some(false),
            ..search_input("alpha", 1, None)
        }))
        .unwrap()
        .0
        .results
        .remove(0)
        .citation_uri
        .parse::<Citation>()
        .unwrap()
        .document_id;

    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    let losing_chunk_id: i64 = connection
        .query_row(
            "SELECT c.id
             FROM chunks AS c
             JOIN chunk_layouts AS cl ON cl.id = c.chunk_layout_id
             JOIN document_versions AS dv ON dv.content_blob_id = cl.content_blob_id
             WHERE dv.document_id != ?1
             ORDER BY c.id
             LIMIT 1",
            [winner.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    let original_chunks = {
        let mut statement = connection
            .prepare("SELECT id, body_text FROM chunks ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()
            .unwrap()
    };
    drop(connection);

    rewrite_search_passages(&fixture.path, |passage_id| {
        if passage_id == losing_chunk_id {
            let mut content = b"alpha oversized losing candidate ".to_vec();
            content.resize(321 * 1024, b'x');
            content
        } else {
            original_chunks[&passage_id].as_bytes().to_vec()
        }
    });
    let error = server
        .evidence_search(Parameters(EvidenceSearchInput {
            timeout_ms: Some(10_000),
            explain: Some(false),
            ..search_input("alpha", 1, None)
        }))
        .err()
        .expect("the MCP SQLite value ceiling must reject the oversized losing candidate");
    assert_public_error(&error, "RESOURCE_EXHAUSTED", "MEMORY_BUDGET", false);
}

#[tokio::test]
async fn incomplete_cursor_rerun_returns_retryable_deadline_and_can_retry_same_cursor() {
    let _serial = heavy_mcp_test_guard();
    let documents = (0..50)
        .map(|index| {
            let key = format!("deadline-{index:02}.md");
            let uri = format!("repo://{key}");
            let body = format!("alpha deadline marker-{index:02}\n");
            document(key.as_bytes(), &key, &uri, body.as_bytes())
        })
        .collect();
    let fixture = fixture_with_documents(documents);
    let (server_stream, client_stream) = tokio::io::duplex(2 * 1024 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let path = fixture.path.clone();
    let project_id = fixture.scope.project_id;
    let server =
        tokio::spawn(async move { serve_io(path, project_id, server_read, server_write).await });
    let mut client_read = BufReader::new(client_read);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-cursor-test", "version": "1"}
            }
        }),
    )
    .await;
    assert_eq!(read_frame(&mut client_read).await["id"], 1);
    write_frame(
        &mut client_write,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    let search_call = |id: u64, cursor: Option<&str>, timeout_ms: u64| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "mode": "auto",
                    "limit": 1,
                    "cursor": cursor,
                    "timeout_ms": timeout_ms,
                    "explain": false
                }
            }
        })
    };

    write_frame(&mut client_write, &search_call(2, None, 10_000)).await;
    let first = read_frame(&mut client_read).await;
    assert!(first.get("error").is_none());
    let first_citation = first["result"]["structuredContent"]["results"][0]["citation_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let cursor = first["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("fixture must have a second page")
        .to_owned();

    // The routed deadline starts at route entry, and 100 ms is the minimum
    // search deadline: any admission or dispatch time leaves strictly less
    // than the minimum, so this rerun deterministically returns a retryable
    // TIMEOUT without oversized fixture chunks or wall-clock racing.
    write_frame(&mut client_write, &search_call(3, Some(&cursor), 100)).await;
    let timed_out = read_frame(&mut client_read).await;
    assert_public_error_value(
        &timed_out["error"]["data"],
        "TIMEOUT",
        "REQUEST_DEADLINE",
        true,
    );

    // A deadline failure must not consume or invalidate the cursor.
    write_frame(&mut client_write, &search_call(4, Some(&cursor), 10_000)).await;
    let retry = read_frame(&mut client_read).await;
    assert!(retry.get("error").is_none());
    let retry_results = retry["result"]["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(retry_results.len(), 1);
    assert_ne!(
        retry_results[0]["citation_uri"].as_str().unwrap(),
        first_citation
    );

    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE projects SET scope_revision = scope_revision + 1 WHERE id = ?1",
            [fixture.scope.project_id.as_uuid().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    write_frame(&mut client_write, &search_call(5, Some(&cursor), 10_000)).await;
    let stale = read_frame(&mut client_read).await;
    assert_public_error_value(
        &stale["error"]["data"],
        "STALE_CURSOR",
        "SCOPE_REVISION",
        false,
    );

    client_write.shutdown().await.unwrap();
    drop(client_write);
    let result = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server did not stop after stdin disconnect")
        .expect("server task panicked");
    assert!(result.is_ok());
}

#[tokio::test]
async fn stdio_protocol_recovers_after_duplicate_key_and_exits_cleanly_on_disconnect() {
    let fixture = fixture();
    let (server_stream, client_stream) = tokio::io::duplex(2 * 1024 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let path = fixture.path.clone();
    let project_id = fixture.scope.project_id;
    let server =
        tokio::spawn(async move { serve_io(path, project_id, server_read, server_write).await });
    let mut client_read = BufReader::new(client_read);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-contract-test", "version": "1"}
            }
        }),
    )
    .await;
    let initialized = read_frame(&mut client_read).await;
    assert_eq!(initialized["id"], 1);
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        Value::String("hsum".to_owned())
    );

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/invalid",
            "unknown": true
        }),
    )
    .await;
    write_frame(
        &mut client_write,
        &json!({"jsonrpc": "2.0", "id": 6, "method": "tools/list", "params": {}}),
    )
    .await;
    let response_after_invalid_notification = read_frame(&mut client_read).await;
    assert_eq!(response_after_invalid_notification["id"], 6);

    let oversized_request_id = "i".repeat(8 * 1024 - 1);
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": oversized_request_id,
            "method": "ping"
        }),
    )
    .await;
    let request_id_error = read_frame(&mut client_read).await;
    assert!(request_id_error["id"].is_null());
    assert_eq!(request_id_error["error"]["code"], -32600);
    assert_eq!(request_id_error["error"]["data"]["subcode"], "FRAME_LIMIT");
    assert_eq!(
        request_id_error["error"]["data"]["details"]["reason"],
        "request_id_too_long"
    );
    assert_public_error_value(
        &request_id_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    client_write
        .write_all(br#"{"jsonrpc":"2.0","#)
        .await
        .unwrap();
    client_write.write_all(b"\n").await.unwrap();
    client_write.flush().await.unwrap();
    let parse_error = read_frame(&mut client_read).await;
    assert_eq!(parse_error["error"]["code"], -32700);
    assert_eq!(parse_error["error"]["data"]["subcode"], "FRAME_LIMIT");
    assert_eq!(
        parse_error["error"]["data"]["details"]["reason"],
        "invalid_json"
    );
    assert_public_error_value(
        &parse_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "ping",
            "unknown": true
        }),
    )
    .await;
    let unknown_field_error = read_frame(&mut client_read).await;
    assert_eq!(unknown_field_error["id"], 8);
    assert_eq!(unknown_field_error["error"]["code"], -32600);
    assert_eq!(
        unknown_field_error["error"]["data"]["subcode"],
        "FRAME_LIMIT"
    );
    assert_eq!(
        unknown_field_error["error"]["data"]["details"]["reason"],
        "unknown_field"
    );
    assert_public_error_value(
        &unknown_field_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 7
        }),
    )
    .await;
    let invalid_envelope = read_frame(&mut client_read).await;
    assert_eq!(invalid_envelope["id"], 7);
    assert_eq!(invalid_envelope["error"]["code"], -32600);
    assert_eq!(invalid_envelope["error"]["data"]["subcode"], "FRAME_LIMIT");
    assert_eq!(
        invalid_envelope["error"]["data"]["details"]["reason"],
        "invalid_protocol_request"
    );
    assert_public_error_value(
        &invalid_envelope["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    client_write
        .write_all(br#"{"jsonrpc":"2.0","id":9,"id":10,"method":"ping"}"#)
        .await
        .unwrap();
    client_write.write_all(b"\n").await.unwrap();
    client_write.flush().await.unwrap();
    let duplicate_error = read_frame(&mut client_read).await;
    assert_eq!(duplicate_error["error"]["code"], -32600);
    assert_eq!(duplicate_error["error"]["data"]["subcode"], "FRAME_LIMIT");
    assert_eq!(
        duplicate_error["error"]["data"]["details"]["reason"],
        "duplicate_key"
    );
    assert_public_error_value(
        &duplicate_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    let oversized_prefix = br#"{"jsonrpc":"2.0","id":11,"method":"ping","padding":""#;
    client_write.write_all(oversized_prefix).await.unwrap();
    client_write
        .write_all(&vec![
            b'x';
            MAX_MCP_FRAME_BYTES + 1 - oversized_prefix.len()
        ])
        .await
        .unwrap();
    client_write.write_all(b"\"}\n").await.unwrap();
    client_write.flush().await.unwrap();
    let oversized_error = read_frame(&mut client_read).await;
    assert_eq!(oversized_error["error"]["code"], -32600);
    assert_eq!(oversized_error["error"]["data"]["subcode"], "FRAME_LIMIT");
    assert_eq!(
        oversized_error["error"]["data"]["details"]["reason"],
        "frame_too_large"
    );
    assert_public_error_value(
        &oversized_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );

    write_frame(
        &mut client_write,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let listed = read_frame(&mut client_read).await;
    assert_eq!(listed["id"], 2);
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 4);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "secret-token-should-not-echo": "super-secret-value"
                }
            }
        }),
    )
    .await;
    let override_error = read_frame(&mut client_read).await;
    assert_eq!(override_error["id"], 3);
    assert_eq!(override_error["error"]["code"], -32602);
    assert_public_error_value(
        &override_error["error"]["data"],
        "INVALID_ARGUMENT",
        "FRAME_LIMIT",
        false,
    );
    assert_eq!(
        override_error["error"]["data"]["details"]["reason"],
        "unknown_field"
    );
    let override_wire = override_error.to_string();
    assert!(!override_wire.contains("secret-token-should-not-echo"));
    assert!(!override_wire.contains("super-secret-value"));
    assert_ne!(
        override_error["error"]["data"]["request_id"],
        Value::String("3".to_owned())
    );

    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE sources SET config_json = ?1 WHERE id = ?2",
            rusqlite::params![
                json!({"root": "/fixture", "index_quota_bytes": 1}).to_string(),
                Uuid::parse_str(SOURCE_UUID).unwrap().as_bytes().as_slice(),
            ],
        )
        .unwrap();
    drop(connection);
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {"name": "evidence_status", "arguments": {}}
        }),
    )
    .await;
    let quota_status = read_frame(&mut client_read).await;
    assert_eq!(quota_status["id"], 12);
    let quota_packet = &quota_status["result"]["structuredContent"];
    assert_eq!(quota_packet["schema_version"], "hsum.api.v1");
    assert!(
        Uuid::parse_str(quota_packet["request_id"].as_str().unwrap()).is_ok(),
        "success request IDs are server-generated UUIDs"
    );
    assert!(
        quota_packet["index_problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|problem| problem["code"] == "INDEX_QUOTA_EXHAUSTED")
    );

    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE sources SET config_json = '{}' WHERE id = ?1",
            [Uuid::parse_str(SOURCE_UUID).unwrap().as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha-beta",
                    "mode": "lexical",
                    "limit": 1,
                    "timeout_ms": 3_000,
                    "explain": false
                }
            }
        }),
    )
    .await;
    let corrupt_root = read_frame(&mut client_read).await;
    assert_eq!(corrupt_root["id"], 13);
    assert_public_error_value(
        &corrupt_root["error"]["data"],
        "INTEGRITY_FAILED",
        "HEAD_INDEX_MISMATCH",
        false,
    );

    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute(
            "UPDATE projects SET name = ?1 WHERE id = ?2",
            rusqlite::params![
                "x".repeat(4_097),
                Uuid::parse_str(PROJECT_UUID).unwrap().as_bytes().as_slice(),
            ],
        )
        .unwrap();
    drop(connection);
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/call",
            "params": {"name": "evidence_project", "arguments": {}}
        }),
    )
    .await;
    let oversized_project = read_frame(&mut client_read).await;
    assert_eq!(oversized_project["id"], 14);
    assert_public_error_value(
        &oversized_project["error"]["data"],
        "RESOURCE_EXHAUSTED",
        "MEMORY_BUDGET",
        false,
    );

    client_write.shutdown().await.unwrap();
    drop(client_write);
    let result = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server did not stop after stdin disconnect")
        .expect("server task panicked");
    assert!(result.is_ok());
}

#[tokio::test]
async fn stdio_store_open_failures_preserve_their_public_taxonomy() {
    let fixture = fixture();
    let (server_stream, client_stream) = tokio::io::duplex(256 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let path = fixture.path.clone();
    let project_id = fixture.scope.project_id;
    let server_path = path.clone();
    let server =
        tokio::spawn(
            async move { serve_io(server_path, project_id, server_read, server_write).await },
        );
    let mut client_read = BufReader::new(client_read);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-open-error-test", "version": "1"}
            }
        }),
    )
    .await;
    let initialized = read_frame(&mut client_read).await;
    assert_eq!(initialized["id"], 1);
    write_frame(
        &mut client_write,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    std::fs::remove_file(&path).unwrap();
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "evidence_status", "arguments": {}}
        }),
    )
    .await;
    let missing = read_frame(&mut client_read).await;
    assert_eq!(missing["id"], 2);
    assert_public_error_value(
        &missing["error"]["data"],
        "NOT_FOUND",
        "INDEX_NOT_FOUND",
        false,
    );

    let replacement = rusqlite::Connection::open(&path).unwrap();
    replacement
        .execute("CREATE TABLE unrelated(value TEXT)", [])
        .unwrap();
    drop(replacement);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "evidence_status", "arguments": {}}
        }),
    )
    .await;
    let wrong_application = read_frame(&mut client_read).await;
    assert_eq!(wrong_application["id"], 3);
    assert_public_error_value(
        &wrong_application["error"]["data"],
        "INDEX_CORRUPT",
        "APPLICATION_ID",
        false,
    );

    client_write.shutdown().await.unwrap();
    drop(client_write);
    let result = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server did not stop after stdin disconnect")
        .expect("server task panicked");
    assert!(result.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdio_deadline_cancel_and_disconnect_release_blocking_capacity() {
    const FIRST_LONG_REQUEST_ID: u64 = 2;
    const LAST_LONG_REQUEST_ID: u64 = 13;
    const QUEUED_DEADLINE_REQUEST_ID: u64 = 14;
    const RECOVERY_REQUEST_ID: u64 = 15;
    const DISCONNECT_REQUEST_ID: u64 = 16;

    let _serial = heavy_mcp_test_guard();
    // Escape-heavy canonical chunks keep every long search serializing near
    // the wire cap, so the twelve admitted searches hold blocking slots long
    // enough for the queued minimum-deadline request to time out in admission.
    let documents = (0..50)
        .map(|index| {
            let key = format!("cancel-{index:02}.md");
            let uri = format!("repo://{key}");
            let body = escape_heavy_body(format!("alpha cancel marker-{index:02} "));
            document(key.as_bytes(), &key, &uri, &body)
        })
        .collect();
    let fixture = fixture_with_documents(documents);

    let (server_stream, client_stream) = tokio::io::duplex(2 * 1024 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let path = fixture.path.clone();
    let project_id = fixture.scope.project_id;
    let server =
        tokio::spawn(async move { serve_io(path, project_id, server_read, server_write).await });
    let mut client_read = BufReader::new(client_read);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-cancel-test", "version": "1"}
            }
        }),
    )
    .await;
    assert_eq!(read_frame(&mut client_read).await["id"], 1);
    write_frame(
        &mut client_write,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    for request_id in FIRST_LONG_REQUEST_ID..=LAST_LONG_REQUEST_ID {
        write_frame(
            &mut client_write,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {
                    "name": "evidence_search",
                    "arguments": {
                        "query": "alpha",
                        "mode": "auto",
                        "limit": 50,
                        "timeout_ms": 10_000,
                        "explain": false
                    }
                }
            }),
        )
        .await;
    }
    // Give the service a scheduling turn to admit all four long searches
    // before the short-deadline request races them for a slot.
    tokio::time::sleep(Duration::from_millis(50)).await;
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": QUEUED_DEADLINE_REQUEST_ID,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "mode": "auto",
                    "limit": 1,
                    "timeout_ms": 100,
                    "explain": false
                }
            }
        }),
    )
    .await;
    let queued_deadline = read_until_id(
        &mut client_read,
        QUEUED_DEADLINE_REQUEST_ID,
        Duration::from_secs(4),
    )
    .await;
    assert_public_error_value(
        &queued_deadline["error"]["data"],
        "TIMEOUT",
        "REQUEST_DEADLINE",
        true,
    );
    assert_eq!(
        queued_deadline["error"]["data"]["details"]["operation"],
        "blocking_admission"
    );

    for request_id in FIRST_LONG_REQUEST_ID..=LAST_LONG_REQUEST_ID {
        write_frame(
            &mut client_write,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": request_id, "reason": "contract test"}
            }),
        )
        .await;
    }
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": RECOVERY_REQUEST_ID,
            "method": "tools/call",
            "params": {"name": "evidence_status", "arguments": {}}
        }),
    )
    .await;
    let recovered = read_until_id_excluding(
        &mut client_read,
        RECOVERY_REQUEST_ID,
        FIRST_LONG_REQUEST_ID..=LAST_LONG_REQUEST_ID,
        Duration::from_secs(4),
    )
    .await;
    assert!(recovered.get("error").is_none());
    assert_eq!(
        recovered["result"]["structuredContent"]["schema_version"],
        "hsum.api.v1"
    );

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": DISCONNECT_REQUEST_ID,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "mode": "auto",
                    "limit": 50,
                    "timeout_ms": 10_000,
                    "explain": false
                }
            }
        }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    client_write.shutdown().await.unwrap();
    drop(client_write);
    let result = tokio::time::timeout(Duration::from_secs(4), server)
        .await
        .expect("server did not reap in-flight work after stdin disconnect")
        .expect("server task panicked");
    assert!(result.is_ok());
}

#[tokio::test]
async fn stdio_escape_heavy_search_at_full_window_stays_within_wire_response_cap() {
    let _serial = heavy_mcp_test_guard();
    let documents = (0..50)
        .map(|document_index| {
            let key = format!("escape-heavy-{document_index:02}.md");
            let uri = format!("repo://{key}");
            let body = escape_heavy_body(format!("alpha unique-{document_index:02} "));
            document(key.as_bytes(), &key, &uri, &body)
        })
        .collect();
    let fixture = fixture_with_documents(documents);
    let (server_stream, client_stream) = tokio::io::duplex(2 * 1024 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let path = fixture.path.clone();
    let project_id = fixture.scope.project_id;
    let server =
        tokio::spawn(async move { serve_io(path, project_id, server_read, server_write).await });
    let mut client_read = BufReader::new(client_read);

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "hsum-size-test", "version": "1"}
            }
        }),
    )
    .await;
    let _initialized = read_frame(&mut client_read).await;
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;
    let request_id = "r".repeat(8 * 1024 - 2);
    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "mode": "lexical",
                    "limit": 50,
                    "timeout_ms": 10_000,
                    "explain": false
                }
            }
        }),
    )
    .await;

    let mut response_line = String::new();
    client_read.read_line(&mut response_line).await.unwrap();
    assert!(!response_line.is_empty());
    let wire_bytes = response_line.trim_end().len();
    assert!(wire_bytes <= MAX_MCP_RESPONSE_BYTES);
    assert!(
        wire_bytes > 400 * 1024,
        "fixture must exercise pagination near the 512 KiB wire cap; got {wire_bytes}"
    );
    let response: Value = serde_json::from_str(&response_line).unwrap();
    assert_eq!(response["id"], request_id);
    assert!(response.get("error").is_none());
    assert_eq!(response["result"]["content"], json!([]));
    assert_eq!(
        response["result"]["structuredContent"]["schema_version"],
        "hsum.api.v1"
    );
    let public_request_id = response["result"]["structuredContent"]["request_id"]
        .as_str()
        .unwrap();
    assert!(Uuid::parse_str(public_request_id).is_ok());
    assert_ne!(public_request_id, request_id);
    let results = response["result"]["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert!(!results.is_empty());
    assert!(results.len() < 50);
    let aggregate_body_bytes = results
        .iter()
        .map(|result| result["content"].as_str().unwrap().len())
        .sum::<usize>();
    // 50 canonical 1,800-byte chunks carry at most 90 KiB of raw body; the
    // wire cap, not the 256 KiB aggregate body cap, is what trims this page.
    assert!(aggregate_body_bytes > 64 * 1024);
    assert!(aggregate_body_bytes <= MAX_MCP_SEARCH_BODY_BYTES);
    let cursor = response["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("response-size trimming must retain a progress cursor")
        .to_owned();
    let first_citations = results
        .iter()
        .map(|result| result["citation_uri"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let duplicated_result = json!({
        "content": [{
            "type": "text",
            "text": response["result"]["structuredContent"].to_string()
        }],
        "structuredContent": response["result"]["structuredContent"].clone(),
        "isError": false
    });
    let duplicated_envelope = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": duplicated_result
    });
    assert!(
        serde_json::to_vec(&duplicated_envelope).unwrap().len() > MAX_MCP_RESPONSE_BYTES,
        "the fixture must reproduce RMCP's former duplicate-output overflow"
    );

    write_frame(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": "alpha",
                    "mode": "lexical",
                    "limit": 50,
                    "cursor": cursor,
                    "timeout_ms": 10_000,
                    "explain": false
                }
            }
        }),
    )
    .await;
    let second = read_frame(&mut client_read).await;
    let second_results = second["result"]["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert!(!second_results.is_empty());
    assert!(
        second_results
            .iter()
            .all(|result| { !first_citations.contains(result["citation_uri"].as_str().unwrap()) })
    );

    client_write.shutdown().await.unwrap();
    drop(client_write);
    let result = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server did not stop after stdin disconnect")
        .expect("server task panicked");
    assert!(result.is_ok());
}

#[test]
fn server_instructions_state_when_to_use_hsum_and_keep_the_untrusted_boundary() {
    let fixture = fixture();
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let info = server.get_info();
    let instructions = info.instructions.expect("server advertises instructions");

    // The safety boundary must survive any future rewrite of this prose.
    assert!(
        instructions.contains("untrusted evidence"),
        "instructions must keep the untrusted-content boundary"
    );
    assert!(
        instructions.contains("never as instruction"),
        "instructions must forbid treating passages as instructions"
    );

    // The usage policy is the point of this change.
    assert!(
        instructions.contains("evidence_search"),
        "instructions must name the search tool"
    );
    assert!(
        instructions.contains("evidence_get"),
        "instructions must name the get tool"
    );
    assert!(
        instructions.contains("not a live view"),
        "instructions must state the ingest-time boundary"
    );
    assert!(
        instructions.len() > 400,
        "one-line instructions cannot carry a usage policy; got {} bytes",
        instructions.len()
    );
}

#[test]
fn tool_descriptions_say_when_to_reach_for_each_tool() {
    let fixture = fixture();
    let server = HsumMcpServer::new(&fixture.path, fixture.scope.project_id).unwrap();
    let tools = server.tool_definitions();

    let description_of = |name: &str| -> String {
        tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .unwrap_or_else(|| panic!("{name} is advertised"))
            .description
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has a description"))
            .to_string()
    };

    // Every tool earns its place in the model's tool list.
    for tool in &tools {
        let description = tool
            .description
            .as_ref()
            .unwrap_or_else(|| panic!("{} has a description", tool.name));
        assert!(
            description.len() > 80,
            "{} description is too thin to guide selection: {description:?}",
            tool.name
        );
    }

    let search = description_of("evidence_search");
    assert!(
        search.contains("citation"),
        "search must promise a citation"
    );
    assert!(
        search.contains("grep"),
        "search must state the comparative case against grep"
    );

    let get = description_of("evidence_get");
    assert!(
        get.contains("verify_source_hash"),
        "get must name the drift parameter"
    );
    assert!(
        get.contains("at ingest"),
        "get must state that bytes are as-of-ingest"
    );

    let project = description_of("evidence_project");
    assert!(
        project.contains("extensions"),
        "project must tell the caller what is indexable"
    );

    let status = description_of("evidence_status");
    assert!(
        status.contains("empty index"),
        "status must explain how to tell an empty index from a genuine absence"
    );
}

struct Fixture {
    path: std::path::PathBuf,
    scope: FilesystemScope,
    _directory: tempfile::TempDir,
}

fn fixture() -> Fixture {
    fixture_with_documents(vec![document(
        b"notes.md",
        "notes.md",
        "repo://notes.md",
        b"alpha-beta local evidence\n",
    )])
}

fn fixture_with_documents(documents: Vec<PreparedDocument>) -> Fixture {
    let directory = tempdir().unwrap();
    harden_fixture_directory(directory.path());
    let path = directory.path().join("index.sqlite");
    let mut database = IndexDb::create(&path, index_id()).unwrap();
    let scope = fixture_scope();
    database
        .apply_filesystem_snapshot(&scope, &documents, &[], DeleteConfirmations::default())
        .unwrap();
    drop(database);
    Fixture {
        path,
        scope,
        _directory: directory,
    }
}

fn harden_fixture_directory(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn search_input(query: &str, limit: usize, cursor: Option<String>) -> EvidenceSearchInput {
    EvidenceSearchInput {
        query: query.to_owned(),
        mode: Some(EvidenceSearchMode::Auto),
        limit: Some(limit),
        cursor,
        timeout_ms: Some(3_000),
        explain: Some(true),
    }
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

/// One canonical maximum-size chunk whose serialized JSON is ~6x its byte
/// length: the prefix keeps `alpha` searchable and the 0x01 fill serializes
/// as JSON `\u0001`. Fifty of these stay far under the 256 KiB aggregate body cap
/// while deterministically crossing the 512 KiB wire response target.
fn escape_heavy_body(prefix: String) -> Vec<u8> {
    let mut body = prefix.into_bytes();
    assert!(body.len() < 1_799, "prefix must leave room for escape fill");
    body.resize(1_799, 0x01);
    body.push(b'\n');
    body
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
    let chunks = chunk_bytes(body, ChunkKind::Markdown, ChunkSettings::default())
        .unwrap()
        .into_iter()
        .map(|chunk| {
            let content = chunk.text().as_bytes();
            PreparedChunk {
                ordinal: chunk.ordinal(),
                byte_span: chunk.span(),
                line_span: chunk.line_span(),
                body_text: chunk.text().to_owned(),
                content_sha256: body_sha256(content),
                quote_bloom: QuoteBloom::from_content(content).into_bytes(),
                literals: prepare_passage_literals(title, source_uri, content),
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
        revision_sha256,
        chunker_fingerprint: hsum::store::chunker_fingerprint(ChunkKind::Markdown),
        chunks,
    }
}

fn set_snapshot_state(
    path: &std::path::Path,
    epoch: u64,
    scope_revision: u64,
    last_success_at: &str,
    with_error: bool,
) {
    let mut database = IndexDb::open_existing(path, OpenMode::ReadWrite).unwrap();
    database
        .debug_set_synthetic_snapshot_state(
            ProjectId::from_uuid(Uuid::parse_str(PROJECT_UUID).unwrap()),
            SourceId::from_uuid(Uuid::parse_str(SOURCE_UUID).unwrap()),
            epoch,
            scope_revision,
            last_success_at,
            with_error.then_some(("TEST_PARTIAL", "synthetic concurrent snapshot state")),
        )
        .unwrap();
}

fn rewrite_search_passages(path: &std::path::Path, mut content_for: impl FnMut(i64) -> Vec<u8>) {
    let mut connection = rusqlite::Connection::open(path).unwrap();
    let passage_ids = {
        let mut statement = connection
            .prepare("SELECT id FROM chunks ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let transaction = connection.transaction().unwrap();
    for passage_id in passage_ids {
        let content = content_for(passage_id);
        let text = String::from_utf8(content.clone()).unwrap();
        let digest = body_sha256(&content);
        let bloom = QuoteBloom::from_content(&content).into_bytes();
        transaction
            .execute(
                "UPDATE chunks
                 SET start_byte = 0, end_byte = ?1,
                     start_line = 1, end_line = 1,
                     body_text = ?2, content_sha256 = ?3, quote_bloom = ?4
                 WHERE id = ?5",
                rusqlite::params![
                    i64::try_from(content.len()).unwrap(),
                    text,
                    digest.as_bytes().as_slice(),
                    bloom.as_slice(),
                    passage_id,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn assert_schema_properties(tools: &[rmcp::model::Tool], name: &str, expected: &[&str]) {
    let tool = tools.iter().find(|tool| tool.name == name).unwrap();
    let actual = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    assert_eq!(actual, expected);
}

fn assert_public_error(error: &rmcp::model::ErrorData, code: &str, subcode: &str, retryable: bool) {
    let data = error.data.as_ref().expect("public error data");
    assert_public_error_value(data, code, subcode, retryable);
    assert_eq!(data["message"], error.message.as_ref());
}

fn assert_public_error_value(data: &Value, code: &str, subcode: &str, retryable: bool) {
    assert_eq!(data["code"], code);
    assert_eq!(data["subcode"], subcode);
    assert_eq!(data["retryable"], retryable);
    assert!(
        data["message"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(data["details"].is_object());
    assert!(
        data["next_action"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(data.get("docs_url").is_none());
    assert!(
        data["request_id"]
            .as_str()
            .is_some_and(|value| Uuid::parse_str(value).is_ok())
    );
}

async fn write_frame<W>(writer: &mut W, value: &Value)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer
        .write_all(serde_json::to_string(value).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn read_frame<R>(reader: &mut BufReader<R>) -> Value
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "server disconnected before responding");
    serde_json::from_str(&line).unwrap()
}

async fn read_until_id<R>(reader: &mut BufReader<R>, target_id: u64, timeout: Duration) -> Value
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(remaining, read_frame(reader))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for response id {target_id}"));
        if frame["id"] == target_id {
            return frame;
        }
    }
}

async fn read_until_id_excluding<R>(
    reader: &mut BufReader<R>,
    target_id: u64,
    forbidden_ids: std::ops::RangeInclusive<u64>,
    timeout: Duration,
) -> Value
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(remaining, read_frame(reader))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for response id {target_id}"));
        if let Some(id) = frame["id"].as_u64() {
            assert!(
                !forbidden_ids.contains(&id),
                "RMCP must suppress late responses for cancelled request {id}"
            );
            if id == target_id {
                return frame;
            }
        }
    }
}
