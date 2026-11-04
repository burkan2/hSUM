use std::time::Instant;

use rusqlite::{Connection, params};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{IndexId, ProjectId, SourceId};
use crate::status::{Status, StatusError, StatusReport};
use crate::store::{IndexDb, OpenMode, StoreError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusEvidenceFieldLimits {
    pub health_issues: usize,
    pub health_detail_bytes: usize,
}

impl StatusEvidenceFieldLimits {
    pub const CLI: Self = Self {
        health_issues: 64,
        health_detail_bytes: 64 * 1024,
    };

    pub const fn new(health_issues: usize, health_detail_bytes: usize) -> Self {
        Self {
            health_issues,
            health_detail_bytes,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StatusEvidenceRequest {
    pub index_path: std::path::PathBuf,
    pub project_id: ProjectId,
    pub field_limits: StatusEvidenceFieldLimits,
    pub operation_deadline: Option<Instant>,
    pub connection_observer: Option<fn(&Connection)>,
    pub cancelled: Option<fn() -> bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusHealthIssue {
    pub source_id: SourceId,
    pub code: String,
    pub detail: String,
    pub observed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusEvidenceOutcome {
    pub index_id: IndexId,
    pub project_id: ProjectId,
    pub report: StatusReport,
    pub source_count: u64,
    pub document_count: u64,
    pub passage_count: u64,
    pub health_issues: Vec<StatusHealthIssue>,
}

#[derive(Debug, Error)]
pub enum StatusEvidenceError {
    #[error("unable to open the immutable evidence store")]
    Store(#[from] StoreError),
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error("SQLite status orchestration failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the bound project does not exist")]
    ProjectNotFound,
    #[error("stored status evidence exceeds the transport field limits")]
    FieldLimit,
    #[error("stored status invariant failed: {0}")]
    Corrupt(&'static str),
    #[error("the status request was cancelled")]
    Cancelled,
    #[error("the status request deadline expired")]
    Deadline,
}

#[derive(Debug)]
pub struct StatusEvidence;

impl StatusEvidence {
    pub fn execute(
        request: &StatusEvidenceRequest,
    ) -> Result<StatusEvidenceOutcome, StatusEvidenceError> {
        checkpoint(request)?;
        let database = IndexDb::open_existing(&request.index_path, OpenMode::ReadOnly)?;
        if let Some(observer) = request.connection_observer {
            observer(database.connection());
        }
        let database_read_only = database.is_read_only()?;
        let transaction = database.connection().unchecked_transaction()?;
        let project_bytes = *request.project_id.as_uuid().as_bytes();
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_bytes.as_slice()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StatusEvidenceError::ProjectNotFound);
        }

        let index_id = read_index_id(&transaction)?;
        let mut report = Status::read_snapshot(&transaction, database_read_only)?;
        let source_count = count_for_project(
            &transaction,
            "SELECT COUNT(*)
             FROM project_sources AS ps
             JOIN sources AS s ON s.id = ps.source_id
             WHERE ps.project_id = ?1
               AND ps.removed_at IS NULL
               AND s.removed_at IS NULL",
            project_bytes.as_slice(),
        )?;
        let document_count = count_for_project(
            &transaction,
            "SELECT COUNT(*)
             FROM document_heads AS dh
             JOIN documents AS d ON d.id = dh.document_id
             JOIN project_sources AS ps ON ps.source_id = d.source_id
             WHERE ps.project_id = ?1
               AND ps.removed_at IS NULL
               AND dh.state = 'active'",
            project_bytes.as_slice(),
        )?;
        let passage_count = count_for_project(
            &transaction,
            "SELECT COUNT(*)
             FROM active_passages AS ap
             JOIN project_sources AS ps ON ps.source_id = ap.source_id
             WHERE ps.project_id = ?1 AND ps.removed_at IS NULL",
            project_bytes.as_slice(),
        )?;
        let health_issues = load_health_issues(
            &transaction,
            project_bytes.as_slice(),
            request.field_limits,
            request,
        )?;
        transaction.rollback()?;

        checkpoint(request)?;
        Status::attach_storage_status(&request.index_path, &mut report);
        checkpoint(request)?;
        database.verify_live_identity()?;
        Ok(StatusEvidenceOutcome {
            index_id,
            project_id: request.project_id,
            report,
            source_count,
            document_count,
            passage_count,
            health_issues,
        })
    }
}

fn load_health_issues(
    connection: &Connection,
    project_bytes: &[u8],
    limits: StatusEvidenceFieldLimits,
    request: &StatusEvidenceRequest,
) -> Result<Vec<StatusHealthIssue>, StatusEvidenceError> {
    let row_limit = limits
        .health_issues
        .checked_add(1)
        .and_then(|limit| i64::try_from(limit).ok())
        .ok_or(StatusEvidenceError::FieldLimit)?;
    let mut statement = connection.prepare(
        "SELECT s.id, s.last_error_code, s.last_error_detail, s.last_error_at
         FROM project_sources AS ps
         JOIN sources AS s ON s.id = ps.source_id
         WHERE ps.project_id = ?1
           AND ps.removed_at IS NULL
           AND s.removed_at IS NULL
           AND s.last_error_code IS NOT NULL
         ORDER BY s.id
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_bytes, row_limit], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut issues = Vec::new();
    for row in rows {
        checkpoint(request)?;
        if issues.len() == limits.health_issues {
            return Err(StatusEvidenceError::FieldLimit);
        }
        let (source_id, code, mut detail, observed_at) = row?;
        truncate_utf8(&mut detail, limits.health_detail_bytes);
        let source_id = Uuid::from_slice(&source_id)
            .map(SourceId::from_uuid)
            .map_err(|_| StatusEvidenceError::Corrupt("invalid health source identity"))?;
        issues.push(StatusHealthIssue {
            source_id,
            code,
            detail,
            observed_at,
        });
    }
    Ok(issues)
}

fn truncate_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn count_for_project(
    connection: &Connection,
    sql: &str,
    project_bytes: &[u8],
) -> Result<u64, StatusEvidenceError> {
    let count: i64 = connection.query_row(sql, [project_bytes], |row| row.get(0))?;
    u64::try_from(count).map_err(|_| StatusEvidenceError::Corrupt("negative status count"))
}

fn read_index_id(connection: &Connection) -> Result<IndexId, StatusEvidenceError> {
    let value: Vec<u8> = connection.query_row(
        "SELECT value FROM index_meta WHERE key = 'index_uuid'",
        [],
        |row| row.get(0),
    )?;
    let uuid = Uuid::from_slice(&value)
        .map_err(|_| StatusEvidenceError::Corrupt("invalid index identity"))?;
    Ok(IndexId::from_uuid(uuid))
}

fn checkpoint(request: &StatusEvidenceRequest) -> Result<(), StatusEvidenceError> {
    if request.cancelled.is_some_and(|cancelled| cancelled()) {
        return Err(StatusEvidenceError::Cancelled);
    }
    if request
        .operation_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(StatusEvidenceError::Deadline);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_detail_truncation_preserves_utf8_boundaries() {
        let mut detail = "é".repeat(4);
        truncate_utf8(&mut detail, 5);
        assert_eq!(detail, "éé");
        assert!(detail.is_char_boundary(detail.len()));
    }
}
