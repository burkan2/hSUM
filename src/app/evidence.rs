use std::path::PathBuf;
use std::time::Instant;

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::app::SourceConfigError;
use crate::app::stored_source::{StoredSource, StoredSourceRootError, stored_source};
use crate::domain::IndexId;
use crate::search::{GetError, GetRequest, GetResponse, get_evidence_snapshot};
use crate::status::{DocumentDrift, DriftOptions, DriftState, Status, StatusError};
use crate::store::{ForgetLedger, IndexDb, OpenMode, StoreError};

/// Transport-specific field ceilings applied before stored text is materialized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GetEvidenceFieldLimits {
    pub display_bytes: usize,
    pub metadata_bytes: usize,
    pub timestamp_bytes: usize,
}

impl GetEvidenceFieldLimits {
    pub const CLI: Self = Self {
        display_bytes: 16 * 1024,
        metadata_bytes: 64 * 1024,
        timestamp_bytes: 128,
    };

    pub const fn new(display_bytes: usize, metadata_bytes: usize, timestamp_bytes: usize) -> Self {
        Self {
            display_bytes,
            metadata_bytes,
            timestamp_bytes,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GetEvidenceRequest {
    pub index_path: PathBuf,
    pub request: GetRequest,
    pub verify_source_hash: bool,
    pub probe_deadline: Instant,
    pub field_limits: GetEvidenceFieldLimits,
    pub connection_observer: Option<fn(&Connection)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetEvidenceOutcome {
    pub evidence: GetResponse,
    pub source_state: EvidenceSourceState,
    pub source_hash_verification: SourceHashVerification,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceSourceState {
    MetadataUnchanged,
    ContentUnchanged,
    ChangedSinceIngest,
    MissingSinceIngest,
    SnapshotOnly,
    Unverifiable,
}

impl EvidenceSourceState {
    pub fn from_observation(observation: Option<&DocumentDrift>) -> Self {
        match observation.map(|value| (value.content_matches, value.state)) {
            Some((Some(true), _)) => Self::ContentUnchanged,
            Some((Some(false), _)) | Some((None, DriftState::MetadataChanged)) => {
                Self::ChangedSinceIngest
            }
            Some((None, DriftState::MetadataUnchanged)) => Self::MetadataUnchanged,
            Some((None, DriftState::Missing)) => Self::MissingSinceIngest,
            Some((None, DriftState::SnapshotOnly)) => Self::SnapshotOnly,
            Some((None, DriftState::Blocked | DriftState::Unknown)) | None => Self::Unverifiable,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataUnchanged => "metadata_unchanged",
            Self::ContentUnchanged => "content_unchanged",
            Self::ChangedSinceIngest => "changed_since_ingest",
            Self::MissingSinceIngest => "missing_since_ingest",
            Self::SnapshotOnly => "snapshot_only",
            Self::Unverifiable => "unverifiable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHashVerification {
    NotRequested,
    Unchanged,
    Changed,
    Missing,
    Blocked,
    SnapshotOnly,
    Unverifiable,
}

impl SourceHashVerification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
            Self::Missing => "missing",
            Self::Blocked => "blocked",
            Self::SnapshotOnly => "snapshot_only",
            Self::Unverifiable => "unverifiable",
        }
    }
}

#[derive(Debug, Error)]
pub enum GetEvidenceError {
    #[error("unable to open the immutable evidence store")]
    Store(#[from] StoreError),
    #[error(transparent)]
    Get(#[from] GetError),
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error("stored filesystem source configuration is invalid")]
    SourceConfig(#[from] SourceConfigError),
    #[error("SQLite evidence orchestration failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored evidence exceeds the transport field limits")]
    FieldLimit,
    #[error("the cited evidence has no source drift target")]
    SourceUnavailable,
}

/// Executes immutable get and live-source observation as one application use case.
#[derive(Debug)]
pub struct GetEvidence;

impl GetEvidence {
    pub fn execute(request: &GetEvidenceRequest) -> Result<GetEvidenceOutcome, GetEvidenceError> {
        let database = IndexDb::open_existing(&request.index_path, OpenMode::ReadOnly)?;
        if let Some(observer) = request.connection_observer {
            observer(database.connection());
        }
        let stored_index_id: Vec<u8> = database.connection().query_row(
            "SELECT value FROM index_meta WHERE key = 'index_uuid'",
            [],
            |row| row.get(0),
        )?;
        let index_id = IndexId::from_uuid(
            uuid::Uuid::from_slice(&stored_index_id)
                .map_err(|_| StoreError::InvalidMetadata("index_uuid"))?,
        );
        if ForgetLedger::read(&request.index_path, index_id)?.suppresses_document(
            request.request.citation.source_id,
            request.request.citation.document_id,
        ) {
            return Err(GetError::EvidenceForgotten.into());
        }
        let transaction = database.connection().unchecked_transaction()?;
        validate_get_fields(&transaction, &request.request, request.field_limits)?;
        let evidence = get_evidence_snapshot(&transaction, &request.request)?;
        let source = stored_source(
            &transaction,
            request.request.project_id,
            request.request.citation.source_id,
            request.field_limits.display_bytes,
        )
        .map_err(map_stored_source_root_error)?;
        let drift_target = Status::cited_drift_target(
            &transaction,
            request.request.citation.source_id,
            request.request.citation.document_id,
            request.request.citation.revision,
        )?
        .ok_or(GetEvidenceError::SourceUnavailable)?;
        transaction.rollback()?;

        let observation = match source {
            StoredSource::FilesystemRoot(source_root) => Status::probe_cited_target(
                &source_root,
                drift_target,
                DriftOptions {
                    verify_content_hash: request.verify_source_hash,
                    deadline: request
                        .probe_deadline
                        .saturating_duration_since(Instant::now()),
                },
            ),
            StoredSource::SnapshotOnly => Status::snapshot_only_target(drift_target),
        };
        database.verify_live_identity()?;
        Ok(GetEvidenceOutcome {
            source_state: EvidenceSourceState::from_observation(Some(&observation)),
            source_hash_verification: source_hash_verification(
                &observation,
                request.verify_source_hash,
            ),
            evidence,
        })
    }
}

fn validate_get_fields(
    connection: &Connection,
    request: &GetRequest,
    limits: GetEvidenceFieldLimits,
) -> Result<(), GetEvidenceError> {
    let oversized: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM document_versions AS dv
             JOIN documents AS d ON d.id = dv.document_id
             JOIN project_sources AS ps ON ps.source_id = d.source_id
             WHERE ps.project_id = ?1
               AND d.id = ?2
               AND d.source_id = ?3
               AND dv.revision_sha256 = ?4
               AND (
                 length(CAST(dv.source_uri AS BLOB)) > ?5
                 OR length(CAST(COALESCE(dv.title, '') AS BLOB)) > ?5
                 OR length(CAST(dv.metadata_json AS BLOB)) > ?6
                 OR length(CAST(COALESCE(dv.source_updated_at, '') AS BLOB)) > ?7
                 OR length(CAST(dv.indexed_at AS BLOB)) > ?7
               )
         )",
        params![
            request.project_id.as_uuid().as_bytes().as_slice(),
            request.citation.document_id.as_uuid().as_bytes().as_slice(),
            request.citation.source_id.as_uuid().as_bytes().as_slice(),
            request.citation.revision.as_bytes().as_slice(),
            sqlite_limit(limits.display_bytes)?,
            sqlite_limit(limits.metadata_bytes)?,
            sqlite_limit(limits.timestamp_bytes)?,
        ],
        |row| row.get(0),
    )?;
    if oversized {
        return Err(GetEvidenceError::FieldLimit);
    }
    Ok(())
}

fn source_hash_verification(
    observation: &DocumentDrift,
    requested: bool,
) -> SourceHashVerification {
    if !requested {
        return SourceHashVerification::NotRequested;
    }
    match (observation.content_matches, observation.state) {
        (Some(true), _) => SourceHashVerification::Unchanged,
        (Some(false), _) => SourceHashVerification::Changed,
        (None, DriftState::Missing) => SourceHashVerification::Missing,
        (None, DriftState::Blocked) => SourceHashVerification::Blocked,
        (None, DriftState::SnapshotOnly) => SourceHashVerification::SnapshotOnly,
        (
            None,
            DriftState::MetadataUnchanged | DriftState::MetadataChanged | DriftState::Unknown,
        ) => SourceHashVerification::Unverifiable,
    }
}

fn sqlite_limit(value: usize) -> Result<i64, GetEvidenceError> {
    i64::try_from(value).map_err(|_| GetEvidenceError::FieldLimit)
}

fn map_stored_source_root_error(error: StoredSourceRootError) -> GetEvidenceError {
    match error {
        StoredSourceRootError::Sqlite(error) => GetEvidenceError::Sqlite(error),
        StoredSourceRootError::SourceConfig(error) => GetEvidenceError::SourceConfig(error),
        StoredSourceRootError::JsonlSourceConfig(_) => GetEvidenceError::SourceUnavailable,
        StoredSourceRootError::FieldLimit => GetEvidenceError::FieldLimit,
        StoredSourceRootError::Unavailable => GetEvidenceError::SourceUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::FilesystemSourceConfig;
    use crate::domain::{DocumentId, SourceId};
    use crate::status::SafeDisplayText;

    fn observation(state: DriftState, content_matches: Option<bool>) -> DocumentDrift {
        DocumentDrift {
            source_id: SourceId::new_v4(),
            document_id: DocumentId::new_v4(),
            connector: SafeDisplayText::from_bytes(b"fixture"),
            state,
            content_matches,
        }
    }

    #[test]
    fn source_state_and_hash_verification_share_one_mapping() {
        let unchanged = observation(DriftState::MetadataUnchanged, Some(true));
        assert_eq!(
            EvidenceSourceState::from_observation(Some(&unchanged)),
            EvidenceSourceState::ContentUnchanged
        );
        assert_eq!(
            source_hash_verification(&unchanged, true),
            SourceHashVerification::Unchanged
        );
        assert_eq!(
            source_hash_verification(&unchanged, false),
            SourceHashVerification::NotRequested
        );

        let changed = observation(DriftState::MetadataChanged, Some(false));
        assert_eq!(
            EvidenceSourceState::from_observation(Some(&changed)),
            EvidenceSourceState::ChangedSinceIngest
        );
        assert_eq!(
            source_hash_verification(&changed, true),
            SourceHashVerification::Changed
        );
    }

    #[test]
    fn stored_root_reader_keeps_legacy_evidence_readable() {
        assert_eq!(
            FilesystemSourceConfig::parse_stored_root(r#"{"root":"/legacy-fixture"}"#).unwrap(),
            PathBuf::from("/legacy-fixture")
        );
        assert!(FilesystemSourceConfig::parse_stored_root(r#"{"root":"relative"}"#).is_err());
    }
}
