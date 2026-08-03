use schemars::JsonSchema;
use serde::Serialize;

use crate::app::{StatusEvidenceOutcome, StatusHealthIssue};
use crate::protocol::{API_VERSION, EvidenceFreshnessOutput};
use crate::status::StatusReport;

#[derive(Clone, Debug, Serialize)]
pub struct CliStatusOutput {
    pub schema_version: String,
    pub request_id: String,
    pub index_id: String,
    pub index: String,
    pub project_id: String,
    pub project: String,
    pub problems_only: bool,
    #[serde(flatten)]
    pub report: StatusReport,
}

impl CliStatusOutput {
    pub fn from_outcome(
        request_id: String,
        index: String,
        project: String,
        problems_only: bool,
        outcome: StatusEvidenceOutcome,
    ) -> Self {
        let data = StatusPacketData::from_outcome(outcome);
        Self {
            schema_version: API_VERSION.to_owned(),
            request_id,
            index_id: data.index_id,
            index,
            project_id: data.project_id,
            project,
            problems_only,
            report: data.report,
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceStatusOutput {
    pub schema_version: String,
    pub request_id: String,
    pub index_id: String,
    pub project_id: String,
    pub active_generation: Option<i64>,
    pub index_epoch: u64,
    pub source_count: u64,
    pub document_count: u64,
    pub passage_count: u64,
    pub model_fingerprint: Option<String>,
    pub degraded_modes: Vec<String>,
    pub health_issues: Vec<HealthIssueOutput>,
    pub index_problems: Vec<StatusProblemOutput>,
    pub read_only: bool,
    pub query_only: bool,
    pub freshness: EvidenceFreshnessOutput,
}

impl EvidenceStatusOutput {
    pub fn from_outcome(
        request_id: String,
        outcome: StatusEvidenceOutcome,
        freshness: EvidenceFreshnessOutput,
    ) -> Self {
        let data = StatusPacketData::from_outcome(outcome);
        let active_generation = data.report.active_generation;
        let index_epoch = data.report.index_epoch;
        let read_only = data.report.database_read_only;
        let query_only = data.report.query_only;
        let index_problems = data
            .report
            .problems
            .into_iter()
            .map(|problem| StatusProblemOutput {
                code: problem.code.to_owned(),
                summary: problem.summary.to_owned(),
                repair_command: problem.repair_command.to_owned(),
            })
            .collect();
        Self {
            schema_version: API_VERSION.to_owned(),
            request_id,
            index_id: data.index_id,
            project_id: data.project_id,
            active_generation,
            index_epoch,
            source_count: data.source_count,
            document_count: data.document_count,
            passage_count: data.passage_count,
            model_fingerprint: None,
            degraded_modes: Vec::new(),
            health_issues: data
                .health_issues
                .into_iter()
                .map(|issue| HealthIssueOutput {
                    source_id: issue.source_id.to_string(),
                    code: issue.code,
                    detail: issue.detail,
                    observed_at: issue.observed_at,
                })
                .collect(),
            index_problems,
            read_only,
            query_only,
            freshness,
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthIssueOutput {
    pub source_id: String,
    pub code: String,
    pub detail: String,
    pub observed_at: String,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusProblemOutput {
    pub code: String,
    pub summary: String,
    pub repair_command: String,
}

struct StatusPacketData {
    index_id: String,
    project_id: String,
    report: StatusReport,
    source_count: u64,
    document_count: u64,
    passage_count: u64,
    health_issues: Vec<StatusHealthIssue>,
}

impl StatusPacketData {
    fn from_outcome(outcome: StatusEvidenceOutcome) -> Self {
        Self {
            index_id: outcome.index_id.to_string(),
            project_id: outcome.project_id.to_string(),
            report: outcome.report,
            source_count: outcome.source_count,
            document_count: outcome.document_count,
            passage_count: outcome.passage_count,
            health_issues: outcome.health_issues,
        }
    }
}
