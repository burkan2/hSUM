use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

use crate::app::SourceConfigError;
use crate::app::stored_source::{StoredSource, StoredSourceRootError, stored_source};
use crate::domain::{IndexId, ProjectId};
use crate::search::{
    MAX_SEARCH_LIMIT, SearchError, SearchRequest, SearchResponse, SearchStopReason,
};
use crate::status::{DocumentDrift, DriftOptions, Status, StatusError};
use crate::store::{ForgetLedger, IndexDb, OpenMode, StoreError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchEvidenceFieldLimits {
    pub max_results: usize,
    pub display_bytes: usize,
    pub passage_bytes: usize,
    pub timestamp_bytes: usize,
    pub duplicate_items: usize,
    pub citation_bytes: usize,
}

impl SearchEvidenceFieldLimits {
    pub const CLI: Self = Self {
        max_results: MAX_SEARCH_LIMIT,
        display_bytes: 16 * 1024,
        passage_bytes: 64 * 1024,
        timestamp_bytes: 128,
        duplicate_items: 4096,
        citation_bytes: 1024,
    };

    pub const fn new(
        max_results: usize,
        display_bytes: usize,
        passage_bytes: usize,
        timestamp_bytes: usize,
        duplicate_items: usize,
        citation_bytes: usize,
    ) -> Self {
        Self {
            max_results,
            display_bytes,
            passage_bytes,
            timestamp_bytes,
            duplicate_items,
            citation_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchEvidencePage {
    pub offset: usize,
    pub limit: usize,
    pub body_bytes: Option<usize>,
}

impl SearchEvidencePage {
    pub const fn unbounded(limit: usize) -> Self {
        Self {
            offset: 0,
            limit,
            body_bytes: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchEvidenceSnapshot {
    pub index_id: IndexId,
    pub scope_revision: u64,
    pub index_epoch: u64,
    pub generation: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SearchEvidenceRequest {
    pub index_path: std::path::PathBuf,
    pub project_id: ProjectId,
    pub search: SearchRequest,
    pub expected_snapshot: Option<SearchEvidenceSnapshot>,
    pub page: SearchEvidencePage,
    pub field_limits: SearchEvidenceFieldLimits,
    pub probe_budget: Duration,
    pub operation_deadline: Option<Instant>,
    pub deadline_stop_is_error: bool,
    pub connection_observer: Option<fn(&Connection)>,
    pub cancelled: Option<fn() -> bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchEvidenceOutcome {
    pub response: SearchResponse,
    pub snapshot: SearchEvidenceSnapshot,
    pub drift: Vec<DocumentDrift>,
    pub total_fetched: usize,
    pub body_bytes: usize,
}

#[derive(Debug, Error)]
pub enum SearchEvidenceError {
    #[error("unable to open the immutable evidence store")]
    Store(#[from] StoreError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error("stored filesystem source configuration is invalid")]
    SourceConfig(#[from] SourceConfigError),
    #[error("SQLite search orchestration failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored search evidence exceeds the transport field limits")]
    FieldLimit,
    #[error("the first search result exceeds the aggregate body limit")]
    BodyLimit,
    #[error("a search result has no source drift target")]
    SourceUnavailable,
    #[error("the search snapshot changed after the cursor was issued")]
    SnapshotChanged {
        expected: SearchEvidenceSnapshot,
        actual: SearchEvidenceSnapshot,
    },
    #[error("the search request was cancelled")]
    Cancelled,
    #[error("the search request deadline expired")]
    Deadline,
    #[error("stored search snapshot invariant failed: {0}")]
    Corrupt(&'static str),
}

#[derive(Debug)]
pub struct SearchEvidence;

impl SearchEvidence {
    pub fn execute(
        request: &SearchEvidenceRequest,
    ) -> Result<SearchEvidenceOutcome, SearchEvidenceError> {
        checkpoint(request)?;
        let database = IndexDb::open_existing(&request.index_path, OpenMode::ReadOnly)?;
        if let Some(observer) = request.connection_observer {
            observer(database.connection());
        }

        if let Some(expected) = request.expected_snapshot.as_ref() {
            let transaction = database.connection().unchecked_transaction()?;
            let actual = read_snapshot(&transaction, request.project_id)?;
            transaction.rollback()?;
            if expected != &actual {
                return Err(SearchEvidenceError::SnapshotChanged {
                    expected: expected.clone(),
                    actual,
                });
            }
        }

        checkpoint(request)?;
        let mut response = database.search(request.project_id, &request.search)?;
        if request.deadline_stop_is_error && response.stop_reason == SearchStopReason::Deadline {
            return Err(SearchEvidenceError::Deadline);
        }
        let index_id = read_index_id(database.connection())?;
        let forget_ledger = ForgetLedger::read(&request.index_path, index_id)?;
        response.results.retain(|passage| {
            !forget_ledger.suppresses_document(passage.source_id, passage.document_id)
        });
        validate_search_fields(&response, request.field_limits)?;
        checkpoint(request)?;

        let snapshot = SearchEvidenceSnapshot {
            index_id,
            scope_revision: response.scope_revision,
            index_epoch: response.index_epoch,
            generation: response.generation,
        };
        if let Some(expected) = request.expected_snapshot.as_ref()
            && expected != &snapshot
        {
            return Err(SearchEvidenceError::SnapshotChanged {
                expected: expected.clone(),
                actual: snapshot,
            });
        }

        let total_fetched = response.results.len();
        let (page_results, body_bytes) = select_page(&response, request.page)?;
        let cited_revisions = page_results
            .iter()
            .map(|passage| {
                (
                    passage.source_id,
                    passage.document_id,
                    passage.revision_sha256,
                )
            })
            .collect::<BTreeSet<_>>();
        let transaction = database.connection().unchecked_transaction()?;
        let drift_targets = cited_revisions
            .into_iter()
            .map(|(source_id, document_id, revision)| {
                checkpoint(request)?;
                let source = stored_source(
                    &transaction,
                    request.project_id,
                    source_id,
                    request.field_limits.display_bytes,
                )
                .map_err(map_stored_source_root_error)?;
                let target =
                    Status::cited_drift_target(&transaction, source_id, document_id, revision)?
                        .ok_or(SearchEvidenceError::SourceUnavailable)?;
                Ok((source, target))
            })
            .collect::<Result<Vec<_>, SearchEvidenceError>>()?;
        transaction.rollback()?;

        let now = Instant::now();
        if request
            .operation_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            return Err(SearchEvidenceError::Deadline);
        }
        let mut probe_deadline = now.checked_add(request.probe_budget).unwrap_or(now);
        if let Some(operation_deadline) = request.operation_deadline {
            probe_deadline = probe_deadline.min(operation_deadline);
        }
        let mut drift = Vec::with_capacity(drift_targets.len());
        for (source, target) in drift_targets {
            checkpoint(request)?;
            drift.push(match source {
                StoredSource::FilesystemRoot(root) => Status::probe_cited_target(
                    &root,
                    target,
                    DriftOptions {
                        verify_content_hash: false,
                        deadline: probe_deadline.saturating_duration_since(Instant::now()),
                    },
                ),
                StoredSource::SnapshotOnly => Status::snapshot_only_target(target),
            });
        }
        checkpoint(request)?;
        database.verify_live_identity()?;

        response.results = page_results;
        Ok(SearchEvidenceOutcome {
            response,
            snapshot,
            drift,
            total_fetched,
            body_bytes,
        })
    }
}

fn select_page(
    response: &SearchResponse,
    page: SearchEvidencePage,
) -> Result<(Vec<crate::search::EvidencePassage>, usize), SearchEvidenceError> {
    if page.limit == 0 {
        return Err(SearchEvidenceError::FieldLimit);
    }
    let mut body_bytes = 0_usize;
    let mut results = Vec::new();
    for passage in response.results.iter().skip(page.offset).take(page.limit) {
        let next_body_bytes = body_bytes.saturating_add(passage.content.len());
        if page
            .body_bytes
            .is_some_and(|maximum| next_body_bytes > maximum)
        {
            break;
        }
        body_bytes = next_body_bytes;
        results.push(passage.clone());
    }
    if response.results.len() > page.offset && results.is_empty() {
        return Err(SearchEvidenceError::BodyLimit);
    }
    Ok((results, body_bytes))
}

fn validate_search_fields(
    response: &SearchResponse,
    limits: SearchEvidenceFieldLimits,
) -> Result<(), SearchEvidenceError> {
    let oversized = response.results.len() > limits.max_results
        || response.results.iter().any(|passage| {
            passage.source_uri.len() > limits.display_bytes
                || passage.title.len() > limits.display_bytes
                || passage.content.len() > limits.passage_bytes
                || passage
                    .source_updated_at
                    .as_deref()
                    .is_some_and(|value| value.len() > limits.timestamp_bytes)
                || passage.indexed_at.len() > limits.timestamp_bytes
                || passage.duplicate_citations.len() > limits.duplicate_items
                || passage
                    .duplicate_citations
                    .iter()
                    .any(|duplicate| duplicate.citation.to_string().len() > limits.citation_bytes)
        });
    if oversized {
        return Err(SearchEvidenceError::FieldLimit);
    }
    Ok(())
}

fn read_snapshot(
    connection: &Connection,
    project_id: ProjectId,
) -> Result<SearchEvidenceSnapshot, SearchEvidenceError> {
    let scope_revision: i64 = connection
        .query_row(
            "SELECT scope_revision FROM projects WHERE id = ?1",
            [project_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(SearchError::ProjectNotFound)?;
    Ok(SearchEvidenceSnapshot {
        index_id: read_index_id(connection)?,
        scope_revision: u64::try_from(scope_revision)
            .map_err(|_| SearchEvidenceError::Corrupt("negative scope revision"))?,
        index_epoch: read_meta_text(connection, "index_epoch")?
            .parse()
            .map_err(|_| SearchEvidenceError::Corrupt("invalid index epoch"))?,
        generation: read_optional_meta_i64(connection, "active_generation")?,
    })
}

fn read_index_id(connection: &Connection) -> Result<IndexId, SearchEvidenceError> {
    let value: Vec<u8> = connection.query_row(
        "SELECT value FROM index_meta WHERE key = 'index_uuid'",
        [],
        |row| row.get(0),
    )?;
    let uuid = Uuid::from_slice(&value)
        .map_err(|_| SearchEvidenceError::Corrupt("invalid index identity"))?;
    Ok(IndexId::from_uuid(uuid))
}

fn read_meta_text(connection: &Connection, key: &str) -> Result<String, SearchEvidenceError> {
    let value: Vec<u8> = connection.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    String::from_utf8(value).map_err(|_| SearchEvidenceError::Corrupt("invalid index metadata"))
}

fn read_optional_meta_i64(
    connection: &Connection,
    key: &str,
) -> Result<Option<i64>, SearchEvidenceError> {
    let value = read_meta_text(connection, key)?;
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| SearchEvidenceError::Corrupt("invalid generation metadata"))
}

fn checkpoint(request: &SearchEvidenceRequest) -> Result<(), SearchEvidenceError> {
    if request.cancelled.is_some_and(|cancelled| cancelled()) {
        return Err(SearchEvidenceError::Cancelled);
    }
    if request
        .operation_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(SearchEvidenceError::Deadline);
    }
    Ok(())
}

fn map_stored_source_root_error(error: StoredSourceRootError) -> SearchEvidenceError {
    match error {
        StoredSourceRootError::Sqlite(error) => SearchEvidenceError::Sqlite(error),
        StoredSourceRootError::SourceConfig(error) => SearchEvidenceError::SourceConfig(error),
        StoredSourceRootError::JsonlSourceConfig(_) => SearchEvidenceError::SourceUnavailable,
        StoredSourceRootError::FieldLimit => SearchEvidenceError::FieldLimit,
        StoredSourceRootError::Unavailable => SearchEvidenceError::SourceUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancelled() -> bool {
        true
    }

    fn request() -> SearchEvidenceRequest {
        SearchEvidenceRequest {
            index_path: "/unused-after-checkpoint".into(),
            project_id: ProjectId::new_v4(),
            search: SearchRequest::with_defaults("fixture").unwrap(),
            expected_snapshot: None,
            page: SearchEvidencePage::unbounded(1),
            field_limits: SearchEvidenceFieldLimits::CLI,
            probe_budget: Duration::from_millis(500),
            operation_deadline: None,
            deadline_stop_is_error: false,
            connection_observer: None,
            cancelled: None,
        }
    }

    #[test]
    fn request_control_stops_before_opening_the_store() {
        let mut cancelled_request = request();
        cancelled_request.cancelled = Some(cancelled);
        assert!(matches!(
            SearchEvidence::execute(&cancelled_request),
            Err(SearchEvidenceError::Cancelled)
        ));

        let mut expired_request = request();
        expired_request.operation_deadline = Instant::now().checked_sub(Duration::from_secs(1));
        assert!(matches!(
            SearchEvidence::execute(&expired_request),
            Err(SearchEvidenceError::Deadline)
        ));
    }
}
