use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::app::{EvidenceSourceState, GetEvidenceOutcome};
use crate::domain::{DocumentId, SourceId};
use crate::protocol::API_VERSION;
use crate::search::{
    CandidateCounts, DuplicateReason, EvidencePassage, SearchResponse, SearchStopReason,
    SearchTiming,
};
use crate::status::DocumentDrift;

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ByteSpanOutput {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineSpanOutput {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliSearchPassageOutput {
    pub citation_uri: String,
    pub index_id: String,
    pub source_id: String,
    pub document_id: String,
    pub revision_sha256: String,
    pub source_uri: String,
    pub title: String,
    pub span: CliSpanOutput,
    pub content: String,
    pub content_sha256: String,
    pub source_updated_at: Option<String>,
    pub indexed_at: String,
    pub head_generation: i64,
    pub source_state: String,
    pub untrusted_content: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<CliSearchScoreOutput>,
    pub duplicate_citations: Vec<CliDuplicateCitationOutput>,
}

impl CliSearchPassageOutput {
    pub fn from_passage(
        passage: EvidencePassage,
        explain: bool,
        source_state: EvidenceSourceState,
    ) -> Self {
        SearchPassageData::from_passage(passage, explain, source_state).into_cli()
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct CliSpanOutput {
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u64,
    pub end_line: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliSearchScoreOutput {
    pub fused: f64,
    pub fusion_units: u64,
    pub lists: Vec<CliRankExplanationOutput>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliRankExplanationOutput {
    pub name: String,
    pub rank: usize,
    pub contribution_units: u64,
    pub backend_score: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliDuplicateCitationOutput {
    pub citation_uri: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliSearchOutput {
    pub schema_version: &'static str,
    pub request_id: String,
    pub generation: Option<i64>,
    pub index_epoch: u64,
    pub project: String,
    pub project_id: String,
    pub scope_revision: u64,
    pub requested_mode: String,
    pub effective_mode: String,
    pub retrievers: Vec<String>,
    pub degraded_mode: Vec<String>,
    pub hints: Vec<String>,
    pub results: Vec<CliSearchPassageOutput>,
    pub next_cursor: Option<String>,
    pub stop_reason: &'static str,
    pub examined: CandidateCountsOutput,
    pub timing_ms: SearchTimingOutput,
}

impl CliSearchOutput {
    pub fn from_response(
        request_id: String,
        project: String,
        response: SearchResponse,
        drift: &[DocumentDrift],
        explain: bool,
        next_cursor: Option<String>,
    ) -> Self {
        SearchEnvelopeData::from_response(response, drift, explain).into_cli(
            request_id,
            project,
            next_cursor,
        )
    }
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
pub struct CandidateCountsOutput {
    pub exact: usize,
    pub exact_fallback: usize,
    pub lexical: usize,
    pub vector: usize,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
pub struct SearchTimingOutput {
    pub query_embedding: u64,
    pub exact: u64,
    pub exact_fallback: u64,
    pub lexical: u64,
    pub vector: u64,
    pub fusion: u64,
    pub total: u64,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePassageOutput {
    pub citation_uri: String,
    pub index_id: String,
    pub source_id: String,
    pub document_id: String,
    pub revision_sha256: String,
    pub source_uri: String,
    pub title: String,
    pub byte_span: ByteSpanOutput,
    pub line_span: LineSpanOutput,
    pub content: String,
    pub content_sha256: String,
    pub source_updated_at: Option<String>,
    pub indexed_at: String,
    pub head_generation: i64,
    pub source_state: String,
    pub untrusted_content: bool,
    pub score: Option<SearchScoreOutput>,
    pub duplicate_citations: Vec<DuplicateCitationOutput>,
}

impl EvidencePassageOutput {
    pub fn from_passage(
        passage: EvidencePassage,
        explain: bool,
        source_state: EvidenceSourceState,
    ) -> Self {
        SearchPassageData::from_passage(passage, explain, source_state).into_mcp()
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchScoreOutput {
    pub fused: f64,
    pub fusion_units: u64,
    pub lists: Vec<RankExplanationOutput>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RankExplanationOutput {
    pub retriever: String,
    pub rank: usize,
    pub contribution_units: u64,
    pub backend_score: Option<f64>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateCitationOutput {
    pub citation_uri: String,
    pub reason: String,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSearchOutput {
    pub schema_version: String,
    pub request_id: String,
    pub project_id: String,
    pub scope_revision: u64,
    pub generation: Option<i64>,
    pub index_epoch: u64,
    pub requested_mode: String,
    pub effective_mode: String,
    pub retrievers: Vec<String>,
    pub degraded_mode: Vec<String>,
    pub hints: Vec<String>,
    pub results: Vec<EvidencePassageOutput>,
    pub stop_reason: String,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub body_bytes: usize,
    pub examined: CandidateCountsOutput,
    pub timing_ms: SearchTimingOutput,
    pub freshness: EvidenceFreshnessOutput,
}

impl EvidenceSearchOutput {
    pub fn from_response(
        request_id: String,
        response: SearchResponse,
        drift: &[DocumentDrift],
        explain: bool,
        body_bytes: usize,
        freshness: EvidenceFreshnessOutput,
    ) -> Self {
        SearchEnvelopeData::from_response(response, drift, explain)
            .into_mcp(request_id, body_bytes, freshness)
    }
}

struct SearchEnvelopeData {
    project_id: String,
    scope_revision: u64,
    generation: Option<i64>,
    index_epoch: u64,
    requested_mode: String,
    effective_mode: String,
    retrievers: Vec<String>,
    degraded_mode: Vec<String>,
    hints: Vec<String>,
    results: Vec<SearchPassageData>,
    stop_reason: &'static str,
    examined: CandidateCounts,
    timing: SearchTiming,
}

impl SearchEnvelopeData {
    fn from_response(response: SearchResponse, drift: &[DocumentDrift], explain: bool) -> Self {
        Self {
            project_id: response.project_id.to_string(),
            scope_revision: response.scope_revision,
            generation: response.generation,
            index_epoch: response.index_epoch,
            requested_mode: response.requested_mode.as_str().to_owned(),
            effective_mode: response.effective_mode.as_str().to_owned(),
            retrievers: response
                .retrievers
                .into_iter()
                .map(|retriever| retriever.as_str().to_owned())
                .collect(),
            degraded_mode: response.degraded_mode,
            hints: response.hints,
            results: response
                .results
                .into_iter()
                .map(|passage| {
                    let source_state = EvidenceSourceState::from_observation(drift_for(
                        drift,
                        passage.source_id,
                        passage.document_id,
                    ));
                    SearchPassageData::from_passage(passage, explain, source_state)
                })
                .collect(),
            stop_reason: search_stop_reason(response.stop_reason),
            examined: response.examined,
            timing: response.timing,
        }
    }

    fn into_cli(
        self,
        request_id: String,
        project: String,
        next_cursor: Option<String>,
    ) -> CliSearchOutput {
        CliSearchOutput {
            schema_version: API_VERSION,
            request_id,
            generation: self.generation,
            index_epoch: self.index_epoch,
            project,
            project_id: self.project_id,
            scope_revision: self.scope_revision,
            requested_mode: self.requested_mode,
            effective_mode: self.effective_mode,
            retrievers: self.retrievers,
            degraded_mode: self.degraded_mode,
            hints: self.hints,
            results: self
                .results
                .into_iter()
                .map(SearchPassageData::into_cli)
                .collect(),
            next_cursor,
            stop_reason: self.stop_reason,
            examined: CandidateCountsOutput {
                exact: self.examined.exact,
                exact_fallback: self.examined.exact_fallback,
                lexical: self.examined.lexical,
                vector: self.examined.vector,
            },
            timing_ms: SearchTimingOutput {
                query_embedding: self.timing.query_embedding_ms,
                exact: self.timing.exact_ms,
                exact_fallback: self.timing.exact_fallback_ms,
                lexical: self.timing.lexical_ms,
                vector: self.timing.vector_ms,
                fusion: self.timing.fusion_ms,
                total: self.timing.total_ms,
            },
        }
    }

    fn into_mcp(
        self,
        request_id: String,
        body_bytes: usize,
        freshness: EvidenceFreshnessOutput,
    ) -> EvidenceSearchOutput {
        EvidenceSearchOutput {
            schema_version: API_VERSION.to_owned(),
            request_id,
            project_id: self.project_id,
            scope_revision: self.scope_revision,
            generation: self.generation,
            index_epoch: self.index_epoch,
            requested_mode: self.requested_mode,
            effective_mode: self.effective_mode,
            retrievers: self.retrievers,
            degraded_mode: self.degraded_mode,
            hints: self.hints,
            results: self
                .results
                .into_iter()
                .map(SearchPassageData::into_mcp)
                .collect(),
            stop_reason: self.stop_reason.to_owned(),
            next_cursor: None,
            truncated: false,
            body_bytes,
            examined: CandidateCountsOutput {
                exact: self.examined.exact,
                exact_fallback: self.examined.exact_fallback,
                lexical: self.examined.lexical,
                vector: self.examined.vector,
            },
            timing_ms: SearchTimingOutput {
                query_embedding: self.timing.query_embedding_ms,
                exact: self.timing.exact_ms,
                exact_fallback: self.timing.exact_fallback_ms,
                lexical: self.timing.lexical_ms,
                vector: self.timing.vector_ms,
                fusion: self.timing.fusion_ms,
                total: self.timing.total_ms,
            },
            freshness,
        }
    }
}

struct SearchPassageData {
    citation_uri: String,
    index_id: String,
    source_id: String,
    document_id: String,
    revision_sha256: String,
    source_uri: String,
    title: String,
    start_byte: u64,
    end_byte: u64,
    start_line: u64,
    end_line: u64,
    content: String,
    content_sha256: String,
    source_updated_at: Option<String>,
    indexed_at: String,
    head_generation: i64,
    source_state: String,
    untrusted_content: bool,
    score: Option<SearchScoreData>,
    duplicate_citations: Vec<DuplicateCitationData>,
}

impl SearchPassageData {
    fn from_passage(
        passage: EvidencePassage,
        explain: bool,
        source_state: EvidenceSourceState,
    ) -> Self {
        let citation_uri = passage.citation().to_string();
        let score = explain.then(|| SearchScoreData {
            fused: passage.score.fused,
            fusion_units: passage.score.fusion_units,
            lists: passage
                .score
                .lists
                .iter()
                .map(|rank| RankExplanationData {
                    retriever: rank.retriever.as_str().to_owned(),
                    rank: rank.rank,
                    contribution_units: rank.contribution_units,
                    backend_score: rank.backend_score,
                })
                .collect(),
        });
        let duplicate_citations = passage
            .duplicate_citations
            .iter()
            .map(|duplicate| DuplicateCitationData {
                citation_uri: duplicate.citation.to_string(),
                reason: duplicate_reason(duplicate.reason).to_owned(),
            })
            .collect();
        Self {
            citation_uri,
            index_id: passage.index_id.to_string(),
            source_id: passage.source_id.to_string(),
            document_id: passage.document_id.to_string(),
            revision_sha256: passage.revision_sha256.to_string(),
            source_uri: passage.source_uri,
            title: passage.title,
            start_byte: passage.byte_span.start(),
            end_byte: passage.byte_span.end(),
            start_line: passage.line_span.start(),
            end_line: passage.line_span.end(),
            content: passage.content,
            content_sha256: passage.content_sha256.to_string(),
            source_updated_at: passage.source_updated_at,
            indexed_at: passage.indexed_at,
            head_generation: passage.head_generation,
            source_state: source_state.as_str().to_owned(),
            untrusted_content: passage.untrusted_content,
            score,
            duplicate_citations,
        }
    }

    fn into_cli(self) -> CliSearchPassageOutput {
        CliSearchPassageOutput {
            citation_uri: self.citation_uri,
            index_id: self.index_id,
            source_id: self.source_id,
            document_id: self.document_id,
            revision_sha256: self.revision_sha256,
            source_uri: self.source_uri,
            title: self.title,
            span: CliSpanOutput {
                start_byte: self.start_byte,
                end_byte: self.end_byte,
                start_line: self.start_line,
                end_line: self.end_line,
            },
            content: self.content,
            content_sha256: self.content_sha256,
            source_updated_at: self.source_updated_at,
            indexed_at: self.indexed_at,
            head_generation: self.head_generation,
            source_state: self.source_state,
            untrusted_content: self.untrusted_content,
            score: self.score.map(SearchScoreData::into_cli),
            duplicate_citations: self
                .duplicate_citations
                .into_iter()
                .map(DuplicateCitationData::into_cli)
                .collect(),
        }
    }

    fn into_mcp(self) -> EvidencePassageOutput {
        EvidencePassageOutput {
            citation_uri: self.citation_uri,
            index_id: self.index_id,
            source_id: self.source_id,
            document_id: self.document_id,
            revision_sha256: self.revision_sha256,
            source_uri: self.source_uri,
            title: self.title,
            byte_span: ByteSpanOutput {
                start: self.start_byte,
                end: self.end_byte,
            },
            line_span: LineSpanOutput {
                start: self.start_line,
                end: self.end_line,
            },
            content: self.content,
            content_sha256: self.content_sha256,
            source_updated_at: self.source_updated_at,
            indexed_at: self.indexed_at,
            head_generation: self.head_generation,
            source_state: self.source_state,
            untrusted_content: self.untrusted_content,
            score: self.score.map(SearchScoreData::into_mcp),
            duplicate_citations: self
                .duplicate_citations
                .into_iter()
                .map(DuplicateCitationData::into_mcp)
                .collect(),
        }
    }
}

struct SearchScoreData {
    fused: f64,
    fusion_units: u64,
    lists: Vec<RankExplanationData>,
}

impl SearchScoreData {
    fn into_cli(self) -> CliSearchScoreOutput {
        CliSearchScoreOutput {
            fused: self.fused,
            fusion_units: self.fusion_units,
            lists: self
                .lists
                .into_iter()
                .map(RankExplanationData::into_cli)
                .collect(),
        }
    }

    fn into_mcp(self) -> SearchScoreOutput {
        SearchScoreOutput {
            fused: self.fused,
            fusion_units: self.fusion_units,
            lists: self
                .lists
                .into_iter()
                .map(RankExplanationData::into_mcp)
                .collect(),
        }
    }
}

struct RankExplanationData {
    retriever: String,
    rank: usize,
    contribution_units: u64,
    backend_score: Option<f64>,
}

impl RankExplanationData {
    fn into_cli(self) -> CliRankExplanationOutput {
        CliRankExplanationOutput {
            name: self.retriever,
            rank: self.rank,
            contribution_units: self.contribution_units,
            backend_score: self.backend_score,
        }
    }

    fn into_mcp(self) -> RankExplanationOutput {
        RankExplanationOutput {
            retriever: self.retriever,
            rank: self.rank,
            contribution_units: self.contribution_units,
            backend_score: self.backend_score,
        }
    }
}

struct DuplicateCitationData {
    citation_uri: String,
    reason: String,
}

impl DuplicateCitationData {
    fn into_cli(self) -> CliDuplicateCitationOutput {
        CliDuplicateCitationOutput {
            citation_uri: self.citation_uri,
            reason: self.reason,
        }
    }

    fn into_mcp(self) -> DuplicateCitationOutput {
        DuplicateCitationOutput {
            citation_uri: self.citation_uri,
            reason: self.reason,
        }
    }
}

const fn duplicate_reason(reason: DuplicateReason) -> &'static str {
    match reason {
        DuplicateReason::SameContent => "same_content",
        DuplicateReason::OverlappingSpan => "overlapping_span",
    }
}

fn drift_for(
    drift: &[DocumentDrift],
    source_id: SourceId,
    document_id: DocumentId,
) -> Option<&DocumentDrift> {
    drift.iter().find(|observation| {
        observation.source_id == source_id && observation.document_id == document_id
    })
}

const fn search_stop_reason(reason: SearchStopReason) -> &'static str {
    match reason {
        SearchStopReason::LimitReached => "limit_reached",
        SearchStopReason::UniqueExhausted => "unique_exhausted",
        SearchStopReason::WorkBudgetExhausted => "work_budget_exhausted",
        SearchStopReason::Deadline => "deadline",
    }
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFreshnessOutput {
    pub policy: String,
    pub state: String,
    pub problem: Option<String>,
}

impl EvidenceFreshnessOutput {
    pub fn manual() -> Self {
        Self {
            policy: "manual".to_owned(),
            state: "not_managed".to_owned(),
            problem: None,
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGetOutput {
    pub schema_version: String,
    pub request_id: String,
    pub requested_citation_uri: String,
    pub returned_citation_uri: String,
    pub requested_line_span: LineSpanOutput,
    pub returned_line_span: LineSpanOutput,
    pub source_uri: String,
    pub title: String,
    pub metadata: Value,
    pub source_updated_at: Option<String>,
    pub indexed_at: String,
    pub content: String,
    pub body_sha256: String,
    pub source_state: String,
    pub source_hash_verification: String,
    pub untrusted_content: bool,
    pub freshness: EvidenceFreshnessOutput,
}

impl EvidenceGetOutput {
    pub fn from_outcome(
        request_id: String,
        outcome: GetEvidenceOutcome,
    ) -> Result<Self, GetPacketError> {
        let response = outcome.evidence;
        let content = String::from_utf8(response.content)?;
        let metadata = serde_json::from_str(&response.metadata_json)?;
        Ok(Self {
            schema_version: API_VERSION.to_owned(),
            request_id,
            requested_citation_uri: response.requested_citation.to_string(),
            returned_citation_uri: response.returned_citation.to_string(),
            requested_line_span: LineSpanOutput {
                start: response.requested_line_span.start(),
                end: response.requested_line_span.end(),
            },
            returned_line_span: LineSpanOutput {
                start: response.returned_line_span.start(),
                end: response.returned_line_span.end(),
            },
            source_uri: response.source_uri,
            title: response.title,
            metadata,
            source_updated_at: response.source_updated_at,
            indexed_at: response.indexed_at,
            content,
            body_sha256: response.body_sha256.to_string(),
            source_state: outcome.source_state.as_str().to_owned(),
            source_hash_verification: outcome.source_hash_verification.as_str().to_owned(),
            untrusted_content: response.untrusted_content,
            freshness: EvidenceFreshnessOutput::manual(),
        })
    }
}

#[derive(Debug, Error)]
pub enum GetPacketError {
    #[error("stored evidence content is not UTF-8")]
    ContentUtf8(#[from] std::string::FromUtf8Error),
    #[error("stored evidence metadata is not valid JSON")]
    MetadataJson(#[from] serde_json::Error),
}
