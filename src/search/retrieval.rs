use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use rusqlite::limits::Limit;
use rusqlite::types::Value;
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params, params_from_iter};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    ByteSpan, Citation, DocumentId, IndexId, LineSpan, ProjectId, Sha256Digest, SourceId,
};
use crate::ingest::{MAX_IDENTIFIER_LITERAL_BYTES, QuoteBloom};
use crate::search::query::{ExactAtomKind, ParsedQuery, QueryError};
use crate::store::{EMBEDDING_DIMENSION, IndexDb};

pub const DEFAULT_SEARCH_LIMIT: usize = 10;
pub const MIN_SEARCH_LIMIT: usize = 1;
pub const MAX_SEARCH_LIMIT: usize = 50;
pub const DEFAULT_SEARCH_DEADLINE_MS: u64 = 3_000;
pub const MIN_SEARCH_DEADLINE_MS: u64 = 100;
pub const MAX_SEARCH_DEADLINE_MS: u64 = 10_000;

const INITIAL_CANDIDATE_DEPTH: usize = 50;
const MAX_CANDIDATES_PER_LIST: usize = 500;
const VECTOR_CANDIDATES_PER_SOURCE: usize = 50;
const MAX_PROJECT_SOURCES: usize = 64;
const MAX_SEARCH_SOURCE_URI_BYTES: usize = 16 * 1024;
const MAX_SEARCH_TITLE_BYTES: usize = 16 * 1024;
const MAX_SEARCH_CONTENT_BYTES: usize = 1_800;
const MAX_SEARCH_TIMESTAMP_BYTES: usize = 128;
const MAX_SEARCH_SQL_VALUE_BYTES: i32 = 64 * 1024;
const RANK_FUSION_SCALE: u64 = 1_000_000_000_000;
const RANK_FUSION_K: u64 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Auto,
    Lexical,
    Semantic,
}

impl SearchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Retriever {
    Exact,
    ExactFallback,
    Lexical,
    Vector,
}

impl Retriever {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::ExactFallback => "exact_fallback",
            Self::Lexical => "lexical",
            Self::Vector => "vector",
        }
    }
}

impl fmt::Display for Retriever {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    query: ParsedQuery,
    mode: SearchMode,
    limit: usize,
    deadline_ms: u64,
    explain: bool,
    query_embedding: Option<Vec<u8>>,
}

impl SearchRequest {
    pub fn new(
        query: &str,
        mode: SearchMode,
        limit: usize,
        deadline_ms: u64,
        explain: bool,
    ) -> Result<Self, SearchError> {
        if !(MIN_SEARCH_LIMIT..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(SearchError::InvalidLimit {
                requested: limit,
                minimum: MIN_SEARCH_LIMIT,
                maximum: MAX_SEARCH_LIMIT,
            });
        }
        if !(MIN_SEARCH_DEADLINE_MS..=MAX_SEARCH_DEADLINE_MS).contains(&deadline_ms) {
            return Err(SearchError::InvalidDeadline {
                requested_ms: deadline_ms,
                minimum_ms: MIN_SEARCH_DEADLINE_MS,
                maximum_ms: MAX_SEARCH_DEADLINE_MS,
            });
        }

        Ok(Self {
            query: ParsedQuery::parse(query)?,
            mode,
            limit,
            deadline_ms,
            explain,
            query_embedding: None,
        })
    }

    pub fn with_defaults(query: &str) -> Result<Self, SearchError> {
        Self::new(
            query,
            SearchMode::Auto,
            DEFAULT_SEARCH_LIMIT,
            DEFAULT_SEARCH_DEADLINE_MS,
            false,
        )
    }

    pub fn query(&self) -> &ParsedQuery {
        &self.query
    }

    pub const fn mode(&self) -> SearchMode {
        self.mode
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }

    pub const fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    pub const fn explain(&self) -> bool {
        self.explain
    }

    pub fn with_query_embedding(mut self, embedding: &[f32]) -> Result<Self, SearchError> {
        if embedding.len() != EMBEDDING_DIMENSION as usize {
            return Err(SearchError::InvalidQueryEmbedding("vector dimension"));
        }
        if embedding.iter().any(|component| !component.is_finite()) {
            return Err(SearchError::InvalidQueryEmbedding("non-finite component"));
        }
        let norm = embedding
            .iter()
            .map(|component| component * component)
            .sum::<f32>()
            .sqrt();
        if !norm.is_finite() || (norm - 1.0).abs() > 0.001 {
            return Err(SearchError::InvalidQueryEmbedding("vector normalization"));
        }
        let mut blob = Vec::with_capacity(std::mem::size_of_val(embedding));
        for component in embedding {
            blob.extend_from_slice(&component.to_le_bytes());
        }
        self.query_embedding = Some(blob);
        Ok(self)
    }

    fn query_embedding(&self) -> Result<&[u8], SearchError> {
        self.query_embedding
            .as_deref()
            .ok_or(SearchError::QueryEmbeddingRequired)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStopReason {
    LimitReached,
    UniqueExhausted,
    WorkBudgetExhausted,
    Deadline,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CandidateCounts {
    pub exact: usize,
    pub exact_fallback: usize,
    pub lexical: usize,
    pub vector: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchTiming {
    pub exact_ms: u64,
    pub exact_fallback_ms: u64,
    pub lexical_ms: u64,
    pub vector_ms: u64,
    pub fusion_ms: u64,
    pub total_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResponse {
    pub project_id: ProjectId,
    pub scope_revision: u64,
    pub generation: Option<i64>,
    pub index_epoch: u64,
    pub requested_mode: SearchMode,
    pub effective_mode: SearchMode,
    pub retrievers: Vec<Retriever>,
    pub results: Vec<EvidencePassage>,
    pub stop_reason: SearchStopReason,
    pub examined: CandidateCounts,
    pub timing: SearchTiming,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvidencePassage {
    pub index_id: IndexId,
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub revision_sha256: Sha256Digest,
    pub source_uri: String,
    pub title: String,
    pub byte_span: ByteSpan,
    pub line_span: LineSpan,
    pub content: String,
    pub content_sha256: Sha256Digest,
    pub source_updated_at: Option<String>,
    pub indexed_at: String,
    pub head_generation: i64,
    pub untrusted_content: bool,
    pub score: SearchScore,
    pub duplicate_citations: Vec<DuplicateCitation>,
}

impl EvidencePassage {
    pub fn citation(&self) -> Citation {
        Citation {
            index_id: self.index_id,
            source_id: self.source_id,
            document_id: self.document_id,
            revision: self.revision_sha256,
            span: self.byte_span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchScore {
    pub fused: f64,
    pub fusion_units: u64,
    pub lists: Vec<RankExplanation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RankExplanation {
    pub retriever: Retriever,
    pub rank: usize,
    pub contribution_units: u64,
    pub backend_score: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateReason {
    SameContent,
    OverlappingSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateCitation {
    pub citation: Citation,
    pub reason: DuplicateReason,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error("search limit must be between {minimum} and {maximum}; received {requested}")]
    InvalidLimit {
        requested: usize,
        minimum: usize,
        maximum: usize,
    },
    #[error(
        "search deadline must be between {minimum_ms} and {maximum_ms} milliseconds; received {requested_ms}"
    )]
    InvalidDeadline {
        requested_ms: u64,
        minimum_ms: u64,
        maximum_ms: u64,
    },
    #[error("bound project does not exist")]
    ProjectNotFound,
    #[error("stored search invariant failed: {0}")]
    Corrupt(&'static str),
    #[error("a retrieval backend returned a non-finite score")]
    NonFiniteScore,
    #[error("semantic retrieval requires one validated query embedding")]
    QueryEmbeddingRequired,
    #[error("query embedding is invalid: {0}")]
    InvalidQueryEmbedding(&'static str),
    #[error("the active generation has no complete compatible vector membership")]
    SemanticUnavailable,
    #[error("exact matcher could not be built")]
    ExactMatcher,
    #[error("search deadline expired")]
    DeadlineExceeded,
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the validated index path changed during search")]
    Store(#[from] crate::store::StoreError),
}

impl IndexDb {
    pub fn search(
        &self,
        project_id: ProjectId,
        request: &SearchRequest,
    ) -> Result<SearchResponse, SearchError> {
        let _value_limit = SearchValueLimit::arm(self.connection())?;
        let started = Instant::now();
        let deadline = started + Duration::from_millis(request.deadline_ms);
        let interrupt = DeadlineInterrupt::arm(self.connection(), deadline);
        let result = execute_search(self, project_id, request, started, deadline);
        drop(interrupt);

        let result = match result {
            Err(SearchError::Sqlite(error)) if is_interrupted(&error) => {
                Err(SearchError::DeadlineExceeded)
            }
            result => result,
        }?;
        self.verify_live_identity()?;
        Ok(result)
    }
}

struct SearchValueLimit<'connection> {
    connection: &'connection Connection,
    previous: i32,
}

impl<'connection> SearchValueLimit<'connection> {
    fn arm(connection: &'connection Connection) -> Result<Self, rusqlite::Error> {
        let previous =
            connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SEARCH_SQL_VALUE_BYTES)?;
        Ok(Self {
            connection,
            previous,
        })
    }
}

impl Drop for SearchValueLimit<'_> {
    fn drop(&mut self) {
        if let Err(error) = self
            .connection
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, self.previous)
        {
            debug_assert!(
                false,
                "failed to restore SQLite search value limit: {error}"
            );
        }
    }
}

fn execute_search(
    database: &IndexDb,
    project_id: ProjectId,
    request: &SearchRequest,
    started: Instant,
    deadline: Instant,
) -> Result<SearchResponse, SearchError> {
    let transaction = database.connection().unchecked_transaction()?;
    let context = load_search_context(&transaction, project_id)?;

    if request.mode == SearchMode::Semantic {
        let response = execute_semantic_search(
            &transaction,
            project_id,
            request,
            &context,
            started,
            deadline,
        )?;
        transaction.commit()?;
        return Ok(response);
    }

    let exact_started = Instant::now();
    let mut exact = ExactPager::new(&request.query)?;
    #[cfg(test)]
    run_injected_exact_timeout_query(&transaction)?;
    let mut exact_target = INITIAL_CANDIDATE_DEPTH;
    exact.expand_to(&transaction, project_id, exact_target, deadline)?;
    let mut exact_candidates = exact.sorted_candidates();
    let mut exact_depth = exact_candidates.len().min(exact_target);
    let mut exact_elapsed = exact_started.elapsed();

    let lexical_expression = compile_lexical_expression(request.query.original());
    let lexical_started = Instant::now();
    let mut lexical = Vec::new();
    let mut lexical_exhausted = lexical_expression.is_none();
    let mut lexical_deadline = exact.deadline || Instant::now() >= deadline;
    if !lexical_deadline && let Some(expression) = lexical_expression.as_deref() {
        let page = fetch_lexical_page(
            &transaction,
            project_id,
            expression,
            0,
            INITIAL_CANDIDATE_DEPTH,
            deadline,
        )?;
        lexical_exhausted = page.exhausted;
        lexical_deadline = page.deadline;
        lexical.extend(page.candidates);
    }
    let mut lexical_elapsed = lexical_started.elapsed();

    let mut fusion_elapsed = Duration::ZERO;
    let stop_reason;
    let mut results;

    loop {
        let fusion_started = Instant::now();
        results = fuse_and_dedupe(
            &context,
            &exact_candidates[..exact_depth],
            &lexical,
            &[],
            request.explain,
            deadline,
        )?;
        fusion_elapsed += fusion_started.elapsed();

        if results.len() >= request.limit {
            results.truncate(request.limit);
            stop_reason = SearchStopReason::LimitReached;
            break;
        }
        if exact.deadline || lexical_deadline || Instant::now() >= deadline {
            stop_reason = SearchStopReason::Deadline;
            break;
        }

        let mut progressed = false;
        if !exact.exhausted() && !exact.work_exhausted() && exact_target < MAX_CANDIDATES_PER_LIST {
            exact_target = (exact_target + INITIAL_CANDIDATE_DEPTH).min(MAX_CANDIDATES_PER_LIST);
            let page_started = Instant::now();
            let expanded = exact.expand_to(&transaction, project_id, exact_target, deadline)?;
            exact_elapsed += page_started.elapsed();
            exact_candidates = exact.sorted_candidates();
            let next_depth = exact_candidates.len().min(exact_target);
            progressed |= expanded || next_depth > exact_depth;
            exact_depth = next_depth;
        }

        if !exact.deadline
            && Instant::now() < deadline
            && !lexical_exhausted
            && lexical.len() < MAX_CANDIDATES_PER_LIST
        {
            let Some(expression) = lexical_expression.as_deref() else {
                return Err(SearchError::Corrupt(
                    "lexical expression disappeared during paging",
                ));
            };
            let page_started = Instant::now();
            let page = fetch_lexical_page(
                &transaction,
                project_id,
                expression,
                lexical.len(),
                (MAX_CANDIDATES_PER_LIST - lexical.len()).min(INITIAL_CANDIDATE_DEPTH),
                deadline,
            )?;
            lexical_elapsed += page_started.elapsed();
            lexical_exhausted = page.exhausted;
            lexical_deadline = page.deadline;
            progressed |= !page.candidates.is_empty();
            lexical.extend(page.candidates);
        }

        if !progressed {
            stop_reason = if exact.work_exhausted()
                || (!lexical_exhausted && lexical.len() == MAX_CANDIDATES_PER_LIST)
            {
                SearchStopReason::WorkBudgetExhausted
            } else {
                SearchStopReason::UniqueExhausted
            };
            break;
        }
    }

    transaction.commit()?;
    let mut retrievers = vec![Retriever::Exact];
    if exact.fallback_used {
        retrievers.push(Retriever::ExactFallback);
    }
    retrievers.push(Retriever::Lexical);

    Ok(SearchResponse {
        project_id,
        scope_revision: context.scope_revision,
        generation: context.generation,
        index_epoch: context.index_epoch,
        requested_mode: request.mode,
        effective_mode: SearchMode::Lexical,
        retrievers,
        results,
        stop_reason,
        examined: CandidateCounts {
            exact: exact_depth,
            exact_fallback: exact.fallback_examined,
            lexical: lexical.len(),
            vector: 0,
        },
        timing: SearchTiming {
            exact_ms: millis(exact_elapsed),
            exact_fallback_ms: millis(exact.fallback_elapsed),
            lexical_ms: millis(lexical_elapsed),
            vector_ms: 0,
            fusion_ms: millis(fusion_elapsed),
            total_ms: millis(started.elapsed()),
        },
    })
}

fn execute_semantic_search(
    connection: &Connection,
    project_id: ProjectId,
    request: &SearchRequest,
    context: &SearchContext,
    started: Instant,
    deadline: Instant,
) -> Result<SearchResponse, SearchError> {
    let query_embedding = request.query_embedding()?;
    if !context.vectors_complete {
        return Err(SearchError::SemanticUnavailable);
    }

    let vector_started = Instant::now();
    let vector_page = fetch_project_vector_candidates(
        connection,
        project_id,
        context.vector_slot,
        query_embedding,
        deadline,
    )?;
    let vector_elapsed = vector_started.elapsed();
    let examined = vector_page.candidates.len();
    let mut depth = examined.min(INITIAL_CANDIDATE_DEPTH);
    let mut fusion_elapsed = Duration::ZERO;
    let stop_reason;
    let mut results;

    loop {
        let fusion_started = Instant::now();
        results = fuse_and_dedupe(
            context,
            &[],
            &[],
            &vector_page.candidates[..depth],
            request.explain,
            deadline,
        )?;
        fusion_elapsed += fusion_started.elapsed();

        if results.len() >= request.limit {
            results.truncate(request.limit);
            stop_reason = SearchStopReason::LimitReached;
            break;
        }
        if depth == examined {
            stop_reason = if vector_page.work_exhausted {
                SearchStopReason::WorkBudgetExhausted
            } else {
                SearchStopReason::UniqueExhausted
            };
            break;
        }
        if Instant::now() >= deadline {
            stop_reason = SearchStopReason::Deadline;
            break;
        }
        depth = (depth + INITIAL_CANDIDATE_DEPTH).min(examined);
    }

    Ok(SearchResponse {
        project_id,
        scope_revision: context.scope_revision,
        generation: context.generation,
        index_epoch: context.index_epoch,
        requested_mode: request.mode,
        effective_mode: SearchMode::Semantic,
        retrievers: vec![Retriever::Vector],
        results,
        stop_reason,
        examined: CandidateCounts {
            exact: 0,
            exact_fallback: 0,
            lexical: 0,
            vector: examined,
        },
        timing: SearchTiming {
            exact_ms: 0,
            exact_fallback_ms: 0,
            lexical_ms: 0,
            vector_ms: millis(vector_elapsed),
            fusion_ms: millis(fusion_elapsed),
            total_ms: millis(started.elapsed()),
        },
    })
}

struct VectorPage {
    candidates: Vec<VectorCandidate>,
    work_exhausted: bool,
}

#[derive(Clone)]
struct VectorCandidate {
    passage: Passage,
    distance: f64,
}

#[derive(Clone, Copy)]
struct RawVectorCandidate {
    rowid: i64,
    distance: f64,
}

fn fetch_project_vector_candidates(
    connection: &Connection,
    project_id: ProjectId,
    slot: VectorSlot,
    query_embedding: &[u8],
    deadline: Instant,
) -> Result<VectorPage, SearchError> {
    let mut source_statement = connection.prepare(
        "SELECT s.id
         FROM project_sources AS ps
         JOIN sources AS s ON s.id = ps.source_id
         WHERE ps.project_id = ?1
           AND ps.removed_at IS NULL
           AND s.removed_at IS NULL
         ORDER BY s.id
         LIMIT 65",
    )?;
    let source_rows = source_statement.query_map([uuid_bytes(project_id).as_slice()], |row| {
        row.get::<_, Vec<u8>>(0)
    })?;
    let mut source_ids = Vec::new();
    for source_id in source_rows {
        source_ids.push(SourceId::from_uuid(uuid_from_blob(&source_id?)?));
    }
    if source_ids.len() > MAX_PROJECT_SOURCES {
        return Err(SearchError::Corrupt("project source cardinality"));
    }

    let mut candidates = Vec::with_capacity(
        source_ids
            .len()
            .saturating_mul(VECTOR_CANDIDATES_PER_SOURCE),
    );
    for source_id in source_ids {
        if Instant::now() >= deadline {
            return Err(SearchError::DeadlineExceeded);
        }
        let raw = deterministic_source_knn(
            connection,
            slot,
            query_embedding,
            source_id,
            VECTOR_CANDIDATES_PER_SOURCE,
        )?;
        candidates.extend(materialize_source_vector_candidates(
            connection, project_id, source_id, &raw,
        )?);
    }
    candidates.sort_by(compare_vector_candidates);
    let work_exhausted = candidates.len() > MAX_CANDIDATES_PER_LIST;
    candidates.truncate(MAX_CANDIDATES_PER_LIST);
    Ok(VectorPage {
        candidates,
        work_exhausted,
    })
}

fn deterministic_source_knn(
    connection: &Connection,
    slot: VectorSlot,
    query_embedding: &[u8],
    source_id: SourceId,
    k: usize,
) -> Result<Vec<RawVectorCandidate>, SearchError> {
    let sql = format!(
        "SELECT rowid, distance FROM {}
         WHERE embedding MATCH ?1 AND source_id = ?2 AND k = ?3
         ORDER BY distance",
        slot.table()
    );
    let mut candidates = query_raw_vector_candidates(
        connection,
        &sql,
        params![
            query_embedding,
            source_id.to_string(),
            i64::try_from(k + 1)
                .map_err(|_| SearchError::Corrupt("vector candidate limit overflow"))?,
        ],
    )?;
    if has_vector_boundary_tie(&candidates, k) {
        return exact_source_knn(connection, slot, query_embedding, source_id, k);
    }
    candidates.truncate(k);
    candidates.sort_by(compare_raw_vector_candidates);
    Ok(candidates)
}

fn has_vector_boundary_tie(candidates: &[RawVectorCandidate], k: usize) -> bool {
    k > 0
        && candidates
            .get(k - 1)
            .zip(candidates.get(k))
            .is_some_and(|(left, right)| left.distance == right.distance)
}

fn exact_source_knn(
    connection: &Connection,
    slot: VectorSlot,
    query_embedding: &[u8],
    source_id: SourceId,
    k: usize,
) -> Result<Vec<RawVectorCandidate>, SearchError> {
    let sql = format!(
        "SELECT rowid, vec_distance_cosine(embedding, ?1) AS exact_distance
         FROM {} WHERE source_id = ?2
         ORDER BY exact_distance, rowid LIMIT ?3",
        slot.table()
    );
    query_raw_vector_candidates(
        connection,
        &sql,
        params![
            query_embedding,
            source_id.to_string(),
            i64::try_from(k)
                .map_err(|_| SearchError::Corrupt("vector candidate limit overflow"))?,
        ],
    )
}

fn query_raw_vector_candidates<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<RawVectorCandidate>, SearchError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(parameters, |row| {
        Ok(RawVectorCandidate {
            rowid: row.get(0)?,
            distance: row.get(1)?,
        })
    })?;
    let mut candidates = Vec::new();
    for candidate in rows {
        let candidate = candidate?;
        if !candidate.distance.is_finite() {
            return Err(SearchError::NonFiniteScore);
        }
        candidates.push(candidate);
    }
    Ok(candidates)
}

fn materialize_source_vector_candidates(
    connection: &Connection,
    project_id: ProjectId,
    source_id: SourceId,
    raw: &[RawVectorCandidate],
) -> Result<Vec<VectorCandidate>, SearchError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", raw.len())
        .collect::<Vec<_>>()
        .join(", ");
    let columns = passage_columns();
    let sql = format!(
        "SELECT {columns}
         FROM active_passages AS ap
         {PASSAGE_JOINS}
         JOIN sources AS s ON s.id = ap.source_id AND s.removed_at IS NULL
         WHERE ps.project_id = ?
           AND ap.source_id = ?
           AND ap.id IN ({placeholders})"
    );
    let mut values = Vec::with_capacity(raw.len() + 2);
    values.push(Value::Blob(uuid_bytes(project_id).to_vec()));
    values.push(Value::Blob(source_id.as_uuid().as_bytes().to_vec()));
    values.extend(raw.iter().map(|candidate| Value::Integer(candidate.rowid)));

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| read_guarded_passage(row, 0))?;
    let mut passages = BTreeMap::new();
    for passage in rows {
        let passage = passage??;
        let passage_id = passage.id;
        if passage.source_id != source_id || passages.insert(passage_id, passage).is_some() {
            return Err(SearchError::Corrupt("vector candidate materialization"));
        }
    }

    raw.iter()
        .map(|candidate| {
            let passage = passages
                .remove(&candidate.rowid)
                .ok_or(SearchError::Corrupt(
                    "vector candidate is not active in project",
                ))?;
            Ok(VectorCandidate {
                passage,
                distance: candidate.distance,
            })
        })
        .collect()
}

fn compare_raw_vector_candidates(
    left: &RawVectorCandidate,
    right: &RawVectorCandidate,
) -> Ordering {
    left.distance
        .total_cmp(&right.distance)
        .then_with(|| left.rowid.cmp(&right.rowid))
}

fn compare_vector_candidates(left: &VectorCandidate, right: &VectorCandidate) -> Ordering {
    left.distance
        .total_cmp(&right.distance)
        .then_with(|| compare_passage_identity(&left.passage, &right.passage))
}

struct DeadlineInterrupt {
    state: Arc<DeadlineInterruptState>,
    worker: Option<JoinHandle<()>>,
}

struct DeadlineInterruptState {
    finished: Mutex<bool>,
    wake: Condvar,
}

impl DeadlineInterrupt {
    fn arm(connection: &Connection, deadline: Instant) -> Self {
        let state = Arc::new(DeadlineInterruptState {
            finished: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker_state = Arc::clone(&state);
        let interrupt = connection.get_interrupt_handle();
        let worker = thread::spawn(move || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let finished = lock_finished(&worker_state.finished);
            let (finished, wait) =
                match worker_state
                    .wake
                    .wait_timeout_while(finished, remaining, |finished| !*finished)
                {
                    Ok(result) => result,
                    Err(poisoned) => poisoned.into_inner(),
                };
            if !*finished && wait.timed_out() {
                drop(finished);
                interrupt.interrupt();
            }
        });

        Self {
            state,
            worker: Some(worker),
        }
    }
}

impl Drop for DeadlineInterrupt {
    fn drop(&mut self) {
        *lock_finished(&self.state.finished) = true;
        self.state.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn lock_finished(mutex: &Mutex<bool>) -> std::sync::MutexGuard<'_, bool> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn is_interrupted(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == ErrorCode::OperationInterrupted
    )
}

#[derive(Clone)]
struct SearchContext {
    index_id: IndexId,
    scope_revision: u64,
    generation: Option<i64>,
    index_epoch: u64,
    vector_slot: VectorSlot,
    vectors_complete: bool,
}

#[derive(Clone, Copy)]
enum VectorSlot {
    A,
    B,
}

impl VectorSlot {
    const fn table(self) -> &'static str {
        match self {
            Self::A => "passages_vec_a",
            Self::B => "passages_vec_b",
        }
    }
}

fn load_search_context(
    connection: &Connection,
    project_id: ProjectId,
) -> Result<SearchContext, SearchError> {
    let project_bytes = uuid_bytes(project_id);
    let scope_revision = connection
        .query_row(
            "SELECT scope_revision FROM projects WHERE id = ?1",
            [project_bytes.as_slice()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(SearchError::ProjectNotFound)
        .and_then(nonnegative_u64)?;
    let index_uuid = metadata(connection, "index_uuid")?;
    let index_id = IndexId::from_uuid(uuid_from_blob(&index_uuid)?);
    let generation_bytes = metadata(connection, "active_generation")?;
    let generation = if generation_bytes.is_empty() {
        None
    } else {
        Some(parse_metadata_i64(&generation_bytes, "active_generation")?)
    };
    let index_epoch = parse_metadata_u64(&metadata(connection, "index_epoch")?, "index_epoch")?;
    let vector_slot = match metadata(connection, "active_vector_slot")?.as_slice() {
        b"0" => VectorSlot::A,
        b"1" => VectorSlot::B,
        _ => return Err(SearchError::Corrupt("active_vector_slot")),
    };
    let vectors_complete = load_vectors_complete(connection, generation)?;

    Ok(SearchContext {
        index_id,
        scope_revision,
        generation,
        index_epoch,
        vector_slot,
        vectors_complete,
    })
}

fn load_vectors_complete(
    connection: &Connection,
    generation: Option<i64>,
) -> Result<bool, SearchError> {
    let profile = metadata(connection, "embedding_profile")?;
    let expected_pin = match profile.as_slice() {
        b"none" => None,
        b"pinned" => {
            let fingerprint = metadata(connection, "embedding_model_fingerprint")?;
            if fingerprint.len() != 32
                || parse_metadata_i64(
                    &metadata(connection, "embedding_dimension")?,
                    "embedding_dimension",
                )? != i64::from(EMBEDDING_DIMENSION)
            {
                return Err(SearchError::Corrupt("embedding profile metadata"));
            }
            Some(fingerprint)
        }
        _ => return Err(SearchError::Corrupt("embedding_profile")),
    };
    let Some(generation) = generation else {
        return Ok(false);
    };
    let (state, fingerprint, dimension) = connection
        .query_row(
            "SELECT vector_state, embedding_model_fingerprint, embedding_dimension
             FROM generations
             WHERE id = ?1 AND state = 'committed'",
            [generation],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(SearchError::Corrupt("active generation is missing"))?;
    match state.as_str() {
        "absent" => Ok(false),
        "complete" => Ok(expected_pin.is_some_and(|expected| {
            fingerprint.as_deref() == Some(expected.as_slice())
                && dimension == Some(i64::from(EMBEDDING_DIMENSION))
        })),
        _ => Err(SearchError::Corrupt("generation vector state")),
    }
}

fn metadata(connection: &Connection, key: &'static str) -> Result<Vec<u8>, SearchError> {
    connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(SearchError::Corrupt("required index metadata is missing"))
}

fn parse_metadata_i64(bytes: &[u8], key: &'static str) -> Result<i64, SearchError> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .ok_or(SearchError::Corrupt(key))
}

fn parse_metadata_u64(bytes: &[u8], key: &'static str) -> Result<u64, SearchError> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(SearchError::Corrupt(key))
}

#[derive(Clone)]
struct ExactCandidate {
    passage: Passage,
    matched_atoms: usize,
    matched_bytes: usize,
    longest_atom: usize,
    fallback: bool,
}

struct ExactPager {
    matcher: Option<ExactMatcher>,
    sources: Vec<ExactCandidateSource>,
    seen: BTreeSet<i64>,
    candidates: BTreeMap<i64, ExactCandidate>,
    examined: usize,
    next_source: usize,
    deadline: bool,
    fallback_used: bool,
    fallback_examined: usize,
    fallback_elapsed: Duration,
}

impl ExactPager {
    fn new(query: &ParsedQuery) -> Result<Self, SearchError> {
        let patterns = query
            .exact_atoms()
            .iter()
            .map(|atom| ExactPattern {
                bytes: atom.text().as_bytes().to_vec(),
                fallback: atom.kind() == ExactAtomKind::Quoted
                    && !tokenizer_compatible_quote(atom.text()),
            })
            .collect::<Vec<_>>();
        let matcher = if patterns.is_empty() {
            None
        } else {
            Some(ExactMatcher::new(patterns)?)
        };

        let identifier_atoms = query
            .exact_atoms()
            .iter()
            .filter(|atom| {
                atom.kind() == ExactAtomKind::Identifier
                    && atom.text().len() <= MAX_IDENTIFIER_LITERAL_BYTES
            })
            .map(|atom| atom.text().to_owned())
            .collect::<Vec<_>>();
        let mut sources = Vec::new();
        if !identifier_atoms.is_empty() {
            sources.push(ExactCandidateSource::Identifier {
                atoms: identifier_atoms,
                offset: 0,
                exhausted: false,
            });
        }
        sources.extend(
            query
                .exact_atoms()
                .iter()
                .filter(|atom| {
                    atom.kind() == ExactAtomKind::Quoted && tokenizer_compatible_quote(atom.text())
                })
                .map(|atom| ExactCandidateSource::Phrase {
                    phrase: atom.text().to_owned(),
                    offset: 0,
                    exhausted: false,
                }),
        );
        if matcher.is_some() {
            // Postings are capped and FTS rows are candidate hints. The
            // deterministic scan keeps valid long or post-cap atoms visible.
            sources.push(ExactCandidateSource::ProjectScan {
                offset: 0,
                exhausted: false,
            });
        }

        let fallback_used = matcher.as_ref().is_some_and(ExactMatcher::has_fallback);
        Ok(Self {
            matcher,
            sources,
            seen: BTreeSet::new(),
            candidates: BTreeMap::new(),
            examined: 0,
            next_source: 0,
            deadline: false,
            fallback_used,
            fallback_examined: 0,
            fallback_elapsed: Duration::ZERO,
        })
    }

    fn expand_to(
        &mut self,
        connection: &Connection,
        project_id: ProjectId,
        target_matches: usize,
        deadline: Instant,
    ) -> Result<bool, SearchError> {
        let initial_matches = self.candidates.len();
        while self.candidates.len() < target_matches
            && self.examined < MAX_CANDIDATES_PER_LIST
            && self.sources.iter().any(|source| !source.exhausted())
        {
            if Instant::now() >= deadline {
                self.deadline = true;
                break;
            }

            let active_sources = self
                .sources
                .iter()
                .filter(|source| !source.exhausted())
                .count();
            let needed = target_matches.saturating_sub(self.candidates.len()).max(1);
            let quota = needed.div_ceil(active_sources).max(1);
            let source_count = self.sources.len();
            let mut fetched_any = false;

            for step in 0..source_count {
                let source_index = (self.next_source + step) % source_count;
                if self.sources[source_index].exhausted()
                    || self.examined >= MAX_CANDIDATES_PER_LIST
                {
                    continue;
                }
                if Instant::now() >= deadline {
                    self.deadline = true;
                    break;
                }

                let limit = quota.min(MAX_CANDIDATES_PER_LIST - self.examined);
                let page_started = Instant::now();
                let page = self.sources[source_index].fetch(connection, project_id, limit)?;
                let raw_scan = page.raw_scan;
                let page_len = page.passages.len();
                fetched_any |= !page.passages.is_empty();

                for passage in page.passages {
                    self.examined += 1;
                    if !self.seen.insert(passage.id) {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        self.deadline = true;
                        break;
                    }
                    let Some(matcher) = self.matcher.as_ref() else {
                        return Err(SearchError::Corrupt(
                            "exact candidate exists without a matcher",
                        ));
                    };
                    if let Some(candidate) = matcher.verify(passage, deadline)? {
                        self.candidates.insert(candidate.passage.id, candidate);
                    }
                }
                if raw_scan && self.fallback_used {
                    self.fallback_examined += page_len;
                    self.fallback_elapsed += page_started.elapsed();
                }
                if self.deadline {
                    break;
                }
            }
            self.next_source = (self.next_source + 1) % source_count.max(1);
            if self.deadline || !fetched_any {
                break;
            }
        }
        Ok(self.candidates.len() > initial_matches)
    }

    fn sorted_candidates(&self) -> Vec<ExactCandidate> {
        let mut candidates = self.candidates.values().cloned().collect::<Vec<_>>();
        candidates.sort_by(compare_exact_candidates);
        candidates
    }

    fn exhausted(&self) -> bool {
        self.sources.iter().all(ExactCandidateSource::exhausted)
    }

    fn work_exhausted(&self) -> bool {
        self.examined >= MAX_CANDIDATES_PER_LIST && !self.exhausted()
    }
}

struct ExactMatcher {
    patterns: Vec<ExactPattern>,
    automaton: AhoCorasick,
}

struct ExactPattern {
    bytes: Vec<u8>,
    fallback: bool,
}

impl ExactMatcher {
    fn new(patterns: Vec<ExactPattern>) -> Result<Self, SearchError> {
        if patterns.iter().any(|pattern| pattern.bytes.is_empty()) {
            return Err(SearchError::ExactMatcher);
        }
        let automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .build(patterns.iter().map(|pattern| pattern.bytes.as_slice()))
            .map_err(|_| SearchError::ExactMatcher)?;
        Ok(Self {
            patterns,
            automaton,
        })
    }

    fn has_fallback(&self) -> bool {
        self.patterns.iter().any(|pattern| pattern.fallback)
    }

    fn verify(
        &self,
        passage: Passage,
        deadline: Instant,
    ) -> Result<Option<ExactCandidate>, SearchError> {
        let mut matched = BTreeSet::new();
        self.collect_matches(passage.title.as_bytes(), &mut matched, deadline)?;
        self.collect_matches(passage.source_uri.as_bytes(), &mut matched, deadline)?;

        // The immutable chunk Bloom is a negative-only gate for raw quoted
        // patterns. `read_passage` verifies it against the fetched content
        // before this point; every Bloom-positive candidate is still
        // byte-verified below.
        let content_eligible = self
            .patterns
            .iter()
            .enumerate()
            .map(|(pattern_index, pattern)| {
                !matched.contains(&pattern_index)
                    && (!pattern.fallback
                        || pattern.bytes.len() < 3
                        || passage.quote_bloom.might_contain(&pattern.bytes))
            })
            .collect::<Vec<_>>();
        if content_eligible.iter().any(|eligible| *eligible) {
            let mut content_matches = BTreeSet::new();
            self.collect_matches(passage.content.as_bytes(), &mut content_matches, deadline)?;
            for pattern_index in content_matches {
                if content_eligible[pattern_index] {
                    matched.insert(pattern_index);
                }
            }
        }
        if matched.is_empty() {
            return Ok(None);
        }

        let matched_atoms = matched.len();
        let matched_bytes = matched
            .iter()
            .map(|index| self.patterns[*index].bytes.len())
            .sum();
        let longest_atom = matched
            .iter()
            .map(|index| self.patterns[*index].bytes.len())
            .max()
            .unwrap_or(0);
        let fallback = matched.iter().any(|index| self.patterns[*index].fallback);
        Ok(Some(ExactCandidate {
            passage,
            matched_atoms,
            matched_bytes,
            longest_atom,
            fallback,
        }))
    }

    fn collect_matches(
        &self,
        haystack: &[u8],
        matched: &mut BTreeSet<usize>,
        deadline: Instant,
    ) -> Result<(), SearchError> {
        if Instant::now() >= deadline {
            return Err(SearchError::DeadlineExceeded);
        }
        for (match_index, found) in self.automaton.find_overlapping_iter(haystack).enumerate() {
            if match_index % 256 == 0 && Instant::now() >= deadline {
                return Err(SearchError::DeadlineExceeded);
            }
            matched.insert(found.pattern().as_usize());
        }
        Ok(())
    }
}

enum ExactCandidateSource {
    Identifier {
        atoms: Vec<String>,
        offset: usize,
        exhausted: bool,
    },
    Phrase {
        phrase: String,
        offset: usize,
        exhausted: bool,
    },
    ProjectScan {
        offset: usize,
        exhausted: bool,
    },
}

impl ExactCandidateSource {
    fn exhausted(&self) -> bool {
        match self {
            Self::Identifier { exhausted, .. }
            | Self::Phrase { exhausted, .. }
            | Self::ProjectScan { exhausted, .. } => *exhausted,
        }
    }

    fn fetch(
        &mut self,
        connection: &Connection,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<ExactSourcePage, SearchError> {
        match self {
            Self::Identifier {
                atoms,
                offset,
                exhausted,
            } => {
                let passages =
                    fetch_identifier_hint_page(connection, project_id, atoms, *offset, limit)?;
                *offset += passages.len();
                *exhausted = passages.len() < limit;
                Ok(ExactSourcePage {
                    passages,
                    raw_scan: false,
                })
            }
            Self::Phrase {
                phrase,
                offset,
                exhausted,
            } => {
                let passages =
                    fetch_phrase_candidates(connection, project_id, phrase, *offset, limit)?;
                *offset += passages.len();
                *exhausted = passages.len() < limit;
                Ok(ExactSourcePage {
                    passages,
                    raw_scan: false,
                })
            }
            Self::ProjectScan { offset, exhausted } => {
                let passages = fetch_project_passages(connection, project_id, *offset, limit)?;
                *offset += passages.len();
                *exhausted = passages.len() < limit;
                Ok(ExactSourcePage {
                    passages,
                    raw_scan: true,
                })
            }
        }
    }
}

struct ExactSourcePage {
    passages: Vec<Passage>,
    raw_scan: bool,
}

fn fetch_identifier_hint_page(
    connection: &Connection,
    project_id: ProjectId,
    atoms: &[String],
    offset: usize,
    limit: usize,
) -> Result<Vec<Passage>, SearchError> {
    if atoms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    // Only the number of fixed "(?, ?)" value tuples is dynamic. User bytes
    // remain bound BLOB values and never become SQL or FTS syntax.
    let atom_values = std::iter::repeat_n("(?, ?)", atoms.len())
        .collect::<Vec<_>>()
        .join(", ");
    let passage_columns = passage_columns();
    let sql = format!(
        "WITH atoms(literal, byte_len) AS (
             VALUES {atom_values}
         ),
         matched(passage_id, literal, byte_len) AS (
             SELECT DISTINCT ap.id, atoms.literal, atoms.byte_len
             FROM atoms
             JOIN passage_literals AS pl ON pl.literal = atoms.literal
             JOIN active_passages AS ap ON ap.id = pl.passage_id
             JOIN project_sources AS ps ON ps.source_id = ap.source_id
             WHERE ps.project_id = ? AND ps.removed_at IS NULL
         ),
         ranked(passage_id, matched_atoms, matched_bytes, longest_atom) AS (
             SELECT passage_id, COUNT(*), SUM(byte_len), MAX(byte_len)
             FROM matched
             GROUP BY passage_id
         )
         SELECT {passage_columns},
                ranked.matched_atoms,
                ranked.matched_bytes,
                ranked.longest_atom
         FROM ranked
         JOIN active_passages AS ap ON ap.id = ranked.passage_id
         {PASSAGE_JOINS}
         WHERE ps.project_id = ?
         ORDER BY ranked.matched_atoms DESC,
                  ranked.matched_bytes DESC,
                  ranked.longest_atom DESC,
                  ap.source_id ASC,
                  ap.document_id ASC,
                  c.start_byte ASC
         LIMIT ? OFFSET ?"
    );
    let mut values = Vec::with_capacity(atoms.len() * 2 + 4);
    for atom in atoms {
        values.push(Value::Blob(atom.as_bytes().to_vec()));
        values.push(Value::Integer(
            i64::try_from(atom.len()).map_err(|_| SearchError::Corrupt("atom length overflow"))?,
        ));
    }
    values.push(Value::Blob(uuid_bytes(project_id).to_vec()));
    values.push(Value::Blob(uuid_bytes(project_id).to_vec()));
    values.push(Value::Integer(
        i64::try_from(limit).map_err(|_| SearchError::Corrupt("candidate limit overflow"))?,
    ));
    values.push(Value::Integer(
        i64::try_from(offset).map_err(|_| SearchError::Corrupt("candidate offset overflow"))?,
    ));

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| read_guarded_passage(row, 0))?;
    let mut passages = Vec::new();
    for row in rows {
        passages.push(row??);
    }
    Ok(passages)
}

fn fetch_phrase_candidates(
    connection: &Connection,
    project_id: ProjectId,
    phrase: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<Passage>, SearchError> {
    let expression = format!("\"{}\"", phrase.replace('"', "\"\""));
    let passage_columns = passage_columns();
    let mut statement = connection.prepare(&format!(
        "SELECT {passage_columns}, bm25(passages_fts) AS lexical_score
         FROM passages_fts
         JOIN active_passages AS ap ON ap.id = passages_fts.rowid
         {PASSAGE_JOINS}
         WHERE ps.project_id = ?1
           AND passages_fts MATCH ?2
         ORDER BY lexical_score ASC,
                  ap.source_id ASC,
                  ap.document_id ASC,
                  c.start_byte ASC
         LIMIT ?3 OFFSET ?4"
    ))?;
    let rows = statement.query_map(
        params![
            uuid_bytes(project_id).as_slice(),
            expression,
            i64::try_from(limit).map_err(|_| SearchError::Corrupt("candidate limit overflow"))?,
            i64::try_from(offset).map_err(|_| SearchError::Corrupt("candidate offset overflow"))?,
        ],
        |row| {
            Ok((
                read_guarded_passage(row, 0)?,
                row.get::<_, f64>(PASSAGE_COLUMN_COUNT)?,
            ))
        },
    )?;
    collect_scored_passages(rows)
}

fn fetch_project_passages(
    connection: &Connection,
    project_id: ProjectId,
    offset: usize,
    limit: usize,
) -> Result<Vec<Passage>, SearchError> {
    let passage_columns = passage_columns();
    let mut statement = connection.prepare(&format!(
        "SELECT {passage_columns}
         FROM active_passages AS ap
         {PASSAGE_JOINS}
         WHERE ps.project_id = ?1
         ORDER BY ap.source_id ASC,
                  ap.document_id ASC,
                  c.start_byte ASC
         LIMIT ?2 OFFSET ?3"
    ))?;
    let rows = statement.query_map(
        params![
            uuid_bytes(project_id).as_slice(),
            i64::try_from(limit).map_err(|_| SearchError::Corrupt("candidate limit overflow"))?,
            i64::try_from(offset).map_err(|_| SearchError::Corrupt("candidate offset overflow"))?,
        ],
        |row| read_guarded_passage(row, 0),
    )?;
    let mut passages = Vec::new();
    for row in rows {
        passages.push(row??);
    }
    Ok(passages)
}

fn compare_exact_candidates(left: &ExactCandidate, right: &ExactCandidate) -> Ordering {
    right
        .matched_atoms
        .cmp(&left.matched_atoms)
        .then_with(|| right.matched_bytes.cmp(&left.matched_bytes))
        .then_with(|| right.longest_atom.cmp(&left.longest_atom))
        .then_with(|| compare_passage_identity(&left.passage, &right.passage))
}

struct LexicalPage {
    candidates: Vec<LexicalCandidate>,
    exhausted: bool,
    deadline: bool,
}

#[derive(Clone)]
struct LexicalCandidate {
    passage: Passage,
    score: f64,
}

fn fetch_lexical_page(
    connection: &Connection,
    project_id: ProjectId,
    expression: &str,
    offset: usize,
    limit: usize,
    deadline: Instant,
) -> Result<LexicalPage, SearchError> {
    #[cfg(test)]
    SEARCH_TEST_HOOKS.with(|hooks| hooks.borrow_mut().lexical_fetches += 1);

    let passage_columns = passage_columns();
    let mut statement = connection.prepare(&format!(
        "SELECT {passage_columns}, bm25(passages_fts) AS lexical_score
         FROM passages_fts
         JOIN active_passages AS ap ON ap.id = passages_fts.rowid
         {PASSAGE_JOINS}
         WHERE ps.project_id = ?1
           AND passages_fts MATCH ?2
         ORDER BY lexical_score ASC,
                  ap.source_id ASC,
                  ap.document_id ASC,
                  c.start_byte ASC
         LIMIT ?3 OFFSET ?4"
    ))?;
    let mut rows = statement.query(params![
        uuid_bytes(project_id).as_slice(),
        expression,
        i64::try_from(limit).map_err(|_| SearchError::Corrupt("candidate limit overflow"))?,
        i64::try_from(offset).map_err(|_| SearchError::Corrupt("candidate offset overflow"))?,
    ])?;
    let mut candidates = Vec::new();
    let mut deadline_reached = false;
    while let Some(row) = rows.next()? {
        if Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
        let score = row.get::<_, f64>(PASSAGE_COLUMN_COUNT)?;
        if !score.is_finite() {
            return Err(SearchError::NonFiniteScore);
        }
        candidates.push(LexicalCandidate {
            passage: read_guarded_passage(row, 0)??,
            score,
        });
    }
    candidates.sort_by(compare_lexical_candidates);
    let exhausted = !deadline_reached && candidates.len() < limit;
    Ok(LexicalPage {
        candidates,
        exhausted,
        deadline: deadline_reached,
    })
}

fn compare_lexical_candidates(left: &LexicalCandidate, right: &LexicalCandidate) -> Ordering {
    left.score
        .total_cmp(&right.score)
        .then_with(|| compare_passage_identity(&left.passage, &right.passage))
}

fn collect_scored_passages(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&Row<'_>) -> rusqlite::Result<(Result<Passage, SearchError>, f64)>,
    >,
) -> Result<Vec<Passage>, SearchError> {
    let mut passages = Vec::new();
    for row in rows {
        let (passage, score) = row?;
        if !score.is_finite() {
            return Err(SearchError::NonFiniteScore);
        }
        passages.push(passage?);
    }
    Ok(passages)
}

const PASSAGE_COLUMN_COUNT: usize = 17;

fn passage_columns() -> String {
    format!(
        "
        (
            length(ap.source_id) != 16
            OR length(ap.document_id) != 16
            OR length(dv.revision_sha256) != 32
            OR length(CAST(dv.source_uri AS BLOB)) NOT BETWEEN 1 AND {MAX_SEARCH_SOURCE_URI_BYTES}
            OR length(CAST(COALESCE(dv.title, '') AS BLOB)) > {MAX_SEARCH_TITLE_BYTES}
            OR length(CAST(c.body_text AS BLOB)) NOT BETWEEN 1 AND {MAX_SEARCH_CONTENT_BYTES}
            OR length(c.content_sha256) != 32
            OR length(c.quote_bloom) != 512
            OR (
                dv.source_updated_at IS NOT NULL
                AND length(CAST(dv.source_updated_at AS BLOB)) > {MAX_SEARCH_TIMESTAMP_BYTES}
            )
            OR length(CAST(dv.indexed_at AS BLOB)) NOT BETWEEN 1 AND {MAX_SEARCH_TIMESTAMP_BYTES}
        ) AS passage_invalid,
        ap.id,
        CASE WHEN length(ap.source_id) = 16 THEN ap.source_id ELSE zeroblob(0) END,
        CASE WHEN length(ap.document_id) = 16 THEN ap.document_id ELSE zeroblob(0) END,
        CASE
            WHEN length(dv.revision_sha256) = 32 THEN dv.revision_sha256
            ELSE zeroblob(0)
        END,
        CASE
            WHEN length(CAST(dv.source_uri AS BLOB))
                 BETWEEN 1 AND {MAX_SEARCH_SOURCE_URI_BYTES}
            THEN dv.source_uri ELSE ''
        END,
        CASE
            WHEN length(CAST(COALESCE(dv.title, '') AS BLOB)) <= {MAX_SEARCH_TITLE_BYTES}
            THEN COALESCE(dv.title, '') ELSE ''
        END,
        c.start_byte,
        c.end_byte,
        c.start_line,
        c.end_line,
        CASE
            WHEN length(CAST(c.body_text AS BLOB))
                 BETWEEN 1 AND {MAX_SEARCH_CONTENT_BYTES}
            THEN c.body_text ELSE ''
        END,
        CASE
            WHEN length(c.content_sha256) = 32 THEN c.content_sha256
            ELSE zeroblob(0)
        END,
        CASE WHEN length(c.quote_bloom) = 512 THEN c.quote_bloom ELSE zeroblob(0) END,
        CASE
            WHEN dv.source_updated_at IS NULL
              OR length(CAST(dv.source_updated_at AS BLOB)) <= {MAX_SEARCH_TIMESTAMP_BYTES}
            THEN dv.source_updated_at ELSE NULL
        END,
        CASE
            WHEN length(CAST(dv.indexed_at AS BLOB))
                 BETWEEN 1 AND {MAX_SEARCH_TIMESTAMP_BYTES}
            THEN dv.indexed_at ELSE ''
        END,
        dh.generation_id"
    )
}
const PASSAGE_JOINS: &str = "
    JOIN project_sources AS ps
      ON ps.source_id = ap.source_id
     AND ps.removed_at IS NULL
    JOIN document_heads AS dh
      ON dh.document_id = ap.document_id
     AND dh.document_version_id = ap.document_version_id
     AND dh.state = 'active'
    JOIN document_versions AS dv
      ON dv.id = ap.document_version_id
     AND dv.document_id = ap.document_id
    JOIN chunks AS c ON c.id = ap.chunk_id";

#[derive(Clone)]
struct Passage {
    id: i64,
    source_id: SourceId,
    document_id: DocumentId,
    revision_sha256: Sha256Digest,
    source_uri: String,
    title: String,
    byte_span: ByteSpan,
    line_span: LineSpan,
    content: String,
    content_sha256: Sha256Digest,
    quote_bloom: QuoteBloom,
    source_updated_at: Option<String>,
    indexed_at: String,
    head_generation: i64,
}

fn read_guarded_passage(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<Result<Passage, SearchError>> {
    if row.get::<_, bool>(offset)? {
        return Ok(Err(SearchError::Corrupt(
            "stored search candidate exceeds alpha field bounds",
        )));
    }
    read_passage(row, offset + 1)
}

fn read_passage(row: &Row<'_>, offset: usize) -> rusqlite::Result<Result<Passage, SearchError>> {
    let source_id = row.get::<_, Vec<u8>>(offset + 1)?;
    let document_id = row.get::<_, Vec<u8>>(offset + 2)?;
    let revision = row.get::<_, Vec<u8>>(offset + 3)?;
    let start_byte = row.get::<_, i64>(offset + 6)?;
    let end_byte = row.get::<_, i64>(offset + 7)?;
    let start_line = row.get::<_, i64>(offset + 8)?;
    let end_line = row.get::<_, i64>(offset + 9)?;
    let content = row.get::<_, String>(offset + 10)?;
    let content_digest = row.get::<_, Vec<u8>>(offset + 11)?;
    let quote_bloom = row.get::<_, Vec<u8>>(offset + 12)?;

    let passage = (|| {
        let source_id = SourceId::from_uuid(uuid_from_blob(&source_id)?);
        let document_id = DocumentId::from_uuid(uuid_from_blob(&document_id)?);
        let revision_sha256 = digest_from_blob(&revision)?;
        let byte_span = ByteSpan::new(nonnegative_u64(start_byte)?, nonnegative_u64(end_byte)?)
            .map_err(|_| SearchError::Corrupt("stored byte span is invalid"))?;
        let line_span = LineSpan::new(nonnegative_u64(start_line)?, nonnegative_u64(end_line)?)
            .map_err(|_| SearchError::Corrupt("stored line span is invalid"))?;
        let content_sha256 = digest_from_blob(&content_digest)?;
        if Sha256Digest::of_bytes(content.as_bytes()) != content_sha256 {
            return Err(SearchError::Corrupt("passage content hash mismatch"));
        }
        let expected_length = byte_span
            .end()
            .checked_sub(byte_span.start())
            .ok_or(SearchError::Corrupt("stored byte span is reversed"))?;
        if usize::try_from(expected_length).ok() != Some(content.len()) {
            return Err(SearchError::Corrupt("passage byte span length mismatch"));
        }
        let quote_bloom: [u8; 512] = quote_bloom
            .try_into()
            .map_err(|_| SearchError::Corrupt("quote Bloom length is invalid"))?;
        let quote_bloom = QuoteBloom::from_bytes(quote_bloom);
        if quote_bloom != QuoteBloom::from_content(content.as_bytes()) {
            return Err(SearchError::Corrupt(
                "quote Bloom does not match passage content",
            ));
        }

        Ok(Passage {
            id: row.get(offset).map_err(SearchError::Sqlite)?,
            source_id,
            document_id,
            revision_sha256,
            source_uri: row.get(offset + 4).map_err(SearchError::Sqlite)?,
            title: row.get(offset + 5).map_err(SearchError::Sqlite)?,
            byte_span,
            line_span,
            content,
            content_sha256,
            quote_bloom,
            source_updated_at: row.get(offset + 13).map_err(SearchError::Sqlite)?,
            indexed_at: row.get(offset + 14).map_err(SearchError::Sqlite)?,
            head_generation: row.get(offset + 15).map_err(SearchError::Sqlite)?,
        })
    })();
    Ok(passage)
}

fn compile_lexical_expression(input: &str) -> Option<String> {
    let mut tokens = Vec::<String>::new();
    let mut current = String::new();

    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty()
            && current.chars().any(char::is_alphanumeric)
            && !tokens.iter().any(|token| token == current)
        {
            tokens.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };

    for character in input.chars() {
        if character.is_alphanumeric() || is_identifier_punctuation(character) {
            current.push(character);
        } else {
            flush(&mut current, &mut tokens);
        }
    }
    flush(&mut current, &mut tokens);

    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .into_iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

fn tokenizer_compatible_quote(value: &str) -> bool {
    value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || byte.is_ascii_whitespace()
            || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
    }) && value.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

fn is_identifier_punctuation(character: char) -> bool {
    matches!(character, '_' | '-' | '.' | ':' | '/')
}

#[derive(Clone)]
struct FusionCandidate {
    passage: Passage,
    exact_rank: Option<usize>,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    matched_exact_bytes: usize,
    exact_fallback: bool,
    lexical_score: Option<f64>,
    vector_distance: Option<f64>,
}

fn fuse_and_dedupe(
    context: &SearchContext,
    exact: &[ExactCandidate],
    lexical: &[LexicalCandidate],
    vector: &[VectorCandidate],
    explain: bool,
    deadline: Instant,
) -> Result<Vec<EvidencePassage>, SearchError> {
    let mut candidates = BTreeMap::<i64, FusionCandidate>::new();
    for (index, candidate) in exact.iter().enumerate() {
        check_materialization_deadline(index, deadline)?;
        candidates.insert(
            candidate.passage.id,
            FusionCandidate {
                passage: candidate.passage.clone(),
                exact_rank: Some(index + 1),
                lexical_rank: None,
                vector_rank: None,
                matched_exact_bytes: candidate.matched_bytes,
                exact_fallback: candidate.fallback,
                lexical_score: None,
                vector_distance: None,
            },
        );
    }
    for (index, candidate) in lexical.iter().enumerate() {
        check_materialization_deadline(index, deadline)?;
        let entry = candidates
            .entry(candidate.passage.id)
            .or_insert_with(|| FusionCandidate {
                passage: candidate.passage.clone(),
                exact_rank: None,
                lexical_rank: None,
                vector_rank: None,
                matched_exact_bytes: 0,
                exact_fallback: false,
                lexical_score: None,
                vector_distance: None,
            });
        entry.lexical_rank = Some(index + 1);
        entry.lexical_score = Some(candidate.score);
    }
    for (index, candidate) in vector.iter().enumerate() {
        check_materialization_deadline(index, deadline)?;
        let entry = candidates
            .entry(candidate.passage.id)
            .or_insert_with(|| FusionCandidate {
                passage: candidate.passage.clone(),
                exact_rank: None,
                lexical_rank: None,
                vector_rank: None,
                matched_exact_bytes: 0,
                exact_fallback: false,
                lexical_score: None,
                vector_distance: None,
            });
        entry.vector_rank = Some(index + 1);
        entry.vector_distance = Some(candidate.distance);
    }

    let mut candidates = candidates
        .into_values()
        .map(RankedFusionCandidate::from)
        .collect::<Vec<_>>();
    candidates.sort_by(compare_fused_candidates);

    let mut results = Vec::<EvidencePassage>::new();
    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        check_materialization_deadline(candidate_index, deadline)?;
        let duplicate = results.iter().enumerate().find_map(|(index, kept)| {
            if kept.content_sha256 == candidate.passage.content_sha256 {
                Some((index, DuplicateReason::SameContent))
            } else if kept.document_id == candidate.passage.document_id
                && spans_overlap_at_least_half(kept.byte_span, candidate.passage.byte_span)
            {
                Some((index, DuplicateReason::OverlappingSpan))
            } else {
                None
            }
        });
        if let Some((index, reason)) = duplicate {
            results[index].duplicate_citations.push(DuplicateCitation {
                citation: Citation {
                    index_id: context.index_id,
                    source_id: candidate.passage.source_id,
                    document_id: candidate.passage.document_id,
                    revision: candidate.passage.revision_sha256,
                    span: candidate.passage.byte_span,
                },
                reason,
            });
            continue;
        }

        let lists = if explain {
            candidate.lists
        } else {
            candidate
                .lists
                .into_iter()
                .map(|mut list| {
                    list.backend_score = None;
                    list
                })
                .collect()
        };
        results.push(EvidencePassage {
            index_id: context.index_id,
            source_id: candidate.passage.source_id,
            document_id: candidate.passage.document_id,
            revision_sha256: candidate.passage.revision_sha256,
            source_uri: candidate.passage.source_uri,
            title: candidate.passage.title,
            byte_span: candidate.passage.byte_span,
            line_span: candidate.passage.line_span,
            content: candidate.passage.content,
            content_sha256: candidate.passage.content_sha256,
            source_updated_at: candidate.passage.source_updated_at,
            indexed_at: candidate.passage.indexed_at,
            head_generation: candidate.passage.head_generation,
            untrusted_content: true,
            score: SearchScore {
                fused: candidate.fusion_units as f64 / RANK_FUSION_SCALE as f64,
                fusion_units: candidate.fusion_units,
                lists,
            },
            duplicate_citations: Vec::new(),
        });
    }
    Ok(results)
}

fn check_materialization_deadline(index: usize, deadline: Instant) -> Result<(), SearchError> {
    if index.is_multiple_of(256) && Instant::now() >= deadline {
        Err(SearchError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

struct RankedFusionCandidate {
    passage: Passage,
    fusion_units: u64,
    matched_exact_bytes: usize,
    lists: Vec<RankExplanation>,
}

impl From<FusionCandidate> for RankedFusionCandidate {
    fn from(candidate: FusionCandidate) -> Self {
        let mut fusion_units = 0_u64;
        let mut lists = Vec::new();
        if let Some(rank) = candidate.exact_rank {
            let contribution = exact_contribution(rank);
            fusion_units += contribution;
            lists.push(RankExplanation {
                retriever: if candidate.exact_fallback {
                    Retriever::ExactFallback
                } else {
                    Retriever::Exact
                },
                rank,
                contribution_units: contribution,
                backend_score: None,
            });
        }
        if let Some(rank) = candidate.lexical_rank {
            let contribution = lexical_contribution(rank);
            fusion_units += contribution;
            lists.push(RankExplanation {
                retriever: Retriever::Lexical,
                rank,
                contribution_units: contribution,
                backend_score: candidate.lexical_score,
            });
        }
        if let Some(rank) = candidate.vector_rank {
            let contribution = lexical_contribution(rank);
            fusion_units += contribution;
            lists.push(RankExplanation {
                retriever: Retriever::Vector,
                rank,
                contribution_units: contribution,
                backend_score: candidate.vector_distance,
            });
        }

        Self {
            passage: candidate.passage,
            fusion_units,
            matched_exact_bytes: candidate.matched_exact_bytes,
            lists,
        }
    }
}

fn exact_contribution(rank: usize) -> u64 {
    let denominator = 2 * (RANK_FUSION_K + rank as u64);
    round_half_up(3 * RANK_FUSION_SCALE, denominator)
}

fn lexical_contribution(rank: usize) -> u64 {
    round_half_up(RANK_FUSION_SCALE, RANK_FUSION_K + rank as u64)
}

const fn round_half_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator / 2) / denominator
}

fn compare_fused_candidates(
    left: &RankedFusionCandidate,
    right: &RankedFusionCandidate,
) -> Ordering {
    right
        .fusion_units
        .cmp(&left.fusion_units)
        .then_with(|| right.matched_exact_bytes.cmp(&left.matched_exact_bytes))
        .then_with(|| compare_passage_identity(&left.passage, &right.passage))
}

fn compare_passage_identity(left: &Passage, right: &Passage) -> Ordering {
    left.source_id
        .cmp(&right.source_id)
        .then_with(|| left.document_id.cmp(&right.document_id))
        .then_with(|| left.byte_span.start().cmp(&right.byte_span.start()))
}

fn spans_overlap_at_least_half(left: ByteSpan, right: ByteSpan) -> bool {
    let left_len = left.end().saturating_sub(left.start());
    let right_len = right.end().saturating_sub(right.start());
    let shorter = left_len.min(right_len);
    if shorter == 0 {
        return false;
    }
    let intersection = left
        .end()
        .min(right.end())
        .saturating_sub(left.start().max(right.start()));
    intersection.saturating_mul(2) >= shorter
}

fn uuid_bytes(project_id: ProjectId) -> [u8; 16] {
    *project_id.as_uuid().as_bytes()
}

fn uuid_from_blob(bytes: &[u8]) -> Result<Uuid, SearchError> {
    Uuid::from_slice(bytes).map_err(|_| SearchError::Corrupt("UUID length is invalid"))
}

fn digest_from_blob(bytes: &[u8]) -> Result<Sha256Digest, SearchError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SearchError::Corrupt("digest length is invalid"))?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn nonnegative_u64(value: i64) -> Result<u64, SearchError> {
    u64::try_from(value).map_err(|_| SearchError::Corrupt("stored integer is negative"))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[derive(Default)]
struct SearchTestHooks {
    inject_slow_exact_query: bool,
    lexical_fetches: usize,
}

#[cfg(test)]
thread_local! {
    static SEARCH_TEST_HOOKS: std::cell::RefCell<SearchTestHooks> =
        std::cell::RefCell::new(SearchTestHooks::default());
}

#[cfg(test)]
fn run_injected_exact_timeout_query(connection: &Connection) -> Result<(), SearchError> {
    let enabled = SEARCH_TEST_HOOKS.with(|hooks| hooks.borrow().inject_slow_exact_query);
    if !enabled {
        return Ok(());
    }

    connection.query_row(
        "WITH RECURSIVE counter(value) AS (
             VALUES(0)
             UNION ALL
             SELECT value + 1 FROM counter WHERE value < 100000000
         )
         SELECT SUM(value) FROM counter",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lexical_compiler_treats_fts_operators_as_quoted_terms() {
        assert_eq!(
            compile_lexical_expression("alpha) OR (beta* NEAR(title:gamma)"),
            Some("\"alpha\" AND \"OR\" AND \"beta\" AND \"NEAR\" AND \"title:gamma\"".to_owned())
        );
        assert_eq!(compile_lexical_expression("*(){}[]"), None);
    }

    #[test]
    fn fusion_rounding_is_integer_and_matches_the_frozen_example() {
        assert_eq!(
            exact_contribution(1) + lexical_contribution(3),
            40_463_179_807
        );
        assert_eq!(lexical_contribution(1), 16_393_442_623);
    }

    #[test]
    fn vector_knn_filters_by_source_before_applying_k() {
        let directory = private_test_directory();
        let database = IndexDb::create(
            &directory.path().join("index.sqlite"),
            IndexId::from_uuid(Uuid::new_v4()),
        )
        .unwrap();
        let selected = SourceId::from_uuid(Uuid::new_v4());
        let hidden = SourceId::from_uuid(Uuid::new_v4());
        let query = unit_vector_blob(0);
        let closer = unit_vector_blob(0);
        let selected_vector = unit_vector_blob(1);
        for rowid in 1..=51_i64 {
            database
                .connection()
                .execute(
                    "INSERT INTO passages_vec_a(rowid, embedding, source_id)
                     VALUES (?1, ?2, ?3)",
                    params![rowid, closer, hidden.to_string()],
                )
                .unwrap();
        }
        database
            .connection()
            .execute(
                "INSERT INTO passages_vec_a(rowid, embedding, source_id)
                 VALUES (100, ?1, ?2)",
                params![selected_vector, selected.to_string()],
            )
            .unwrap();

        let candidates = deterministic_source_knn(
            database.connection(),
            VectorSlot::A,
            &query,
            selected,
            VECTOR_CANDIDATES_PER_SOURCE,
        )
        .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rowid, 100);
    }

    #[test]
    fn vector_knn_boundary_ties_use_exact_rowid_order() {
        let directory = private_test_directory();
        let database = IndexDb::create(
            &directory.path().join("index.sqlite"),
            IndexId::from_uuid(Uuid::new_v4()),
        )
        .unwrap();
        let source = SourceId::from_uuid(Uuid::new_v4());
        let query = unit_vector_blob(0);
        for rowid in (1..=51_i64).rev() {
            database
                .connection()
                .execute(
                    "INSERT INTO passages_vec_a(rowid, embedding, source_id)
                     VALUES (?1, ?2, ?3)",
                    params![rowid, query, source.to_string()],
                )
                .unwrap();
        }

        let candidates = deterministic_source_knn(
            database.connection(),
            VectorSlot::A,
            &query,
            source,
            VECTOR_CANDIDATES_PER_SOURCE,
        )
        .unwrap();

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.rowid)
                .collect::<Vec<_>>(),
            (1..=50_i64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn overlap_threshold_is_inclusive_at_one_half() {
        assert!(spans_overlap_at_least_half(
            ByteSpan::new(0, 100).unwrap(),
            ByteSpan::new(50, 150).unwrap(),
        ));
        assert!(!spans_overlap_at_least_half(
            ByteSpan::new(0, 100).unwrap(),
            ByteSpan::new(51, 151).unwrap(),
        ));
    }

    #[test]
    fn deadline_guard_interrupts_a_running_sqlite_operation() {
        let connection = Connection::open_in_memory().unwrap();
        let deadline = Instant::now() + Duration::from_millis(20);
        let interrupt = DeadlineInterrupt::arm(&connection, deadline);
        let result = connection.query_row(
            "WITH RECURSIVE counter(value) AS (
                 VALUES(0)
                 UNION ALL
                 SELECT value + 1 FROM counter WHERE value < 100000000
             )
             SELECT SUM(value) FROM counter",
            [],
            |row| row.get::<_, i64>(0),
        );
        drop(interrupt);

        let error = result.expect_err("deadline must interrupt the recursive query");
        assert!(is_interrupted(&error), "{error:?}");
    }

    #[test]
    fn search_maps_exact_sqlite_timeout_and_skips_lexical_retrieval() {
        struct ResetHooks;

        impl Drop for ResetHooks {
            fn drop(&mut self) {
                SEARCH_TEST_HOOKS.with(|hooks| *hooks.borrow_mut() = SearchTestHooks::default());
            }
        }

        let directory = tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let database = IndexDb::create(
            &directory.path().join("index.sqlite"),
            IndexId::from_uuid(Uuid::new_v4()),
        )
        .unwrap();
        let project_id = ProjectId::from_uuid(Uuid::new_v4());
        database
            .connection()
            .execute(
                "INSERT INTO projects(id, name, scope_revision, created_at)
                 VALUES (?1, 'deadline-fixture', 0, '2026-07-20T00:00:00Z')",
                [project_id.as_uuid().as_bytes().as_slice()],
            )
            .unwrap();
        let request =
            SearchRequest::new("slow-token", SearchMode::Lexical, 10, 100, false).unwrap();
        let _reset = ResetHooks;
        SEARCH_TEST_HOOKS.with(|hooks| {
            let mut hooks = hooks.borrow_mut();
            hooks.inject_slow_exact_query = true;
            hooks.lexical_fetches = 0;
        });

        let result = database.search(project_id, &request);

        assert!(matches!(result, Err(SearchError::DeadlineExceeded)));
        let lexical_fetches = SEARCH_TEST_HOOKS.with(|hooks| hooks.borrow().lexical_fetches);
        assert_eq!(lexical_fetches, 0);
    }

    fn private_test_directory() -> tempfile::TempDir {
        let directory = tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        directory
    }

    fn unit_vector_blob(coordinate: usize) -> Vec<u8> {
        let mut vector = vec![0.0_f32; EMBEDDING_DIMENSION as usize];
        vector[coordinate] = 1.0;
        let mut blob = Vec::with_capacity(std::mem::size_of_val(vector.as_slice()));
        for component in vector {
            blob.extend_from_slice(&component.to_le_bytes());
        }
        blob
    }
}
