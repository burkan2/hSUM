use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use thiserror::Error;

use crate::domain::SourceId;
use crate::ingest::DiscoveryError;
use crate::store::{
    ConfiguredSource, ConfiguredSourceKind, DeleteConfirmations, FilesystemScope, IndexDb,
    IngestOutcome, IngestPlan, JsonlScope, OpenMode, PreparedDocumentSummary, PreparedSourceBatch,
    StoragePreflight, StoragePreflightError, StoreError, WriterLock, list_project_sources,
};

use super::jsonl_connector::{SOURCE_FAILURE_CODE, bounded_failure_detail, read_prepared_snapshot};
use super::{
    EffectiveContext, FAILURE_RECORD_ESTIMATED_WRITE_BYTES, FilesystemIngestError,
    JsonlFileIngestError, JsonlSourceConfig, JsonlSourceConfigError, SourceManagementError,
    discovery_error_code, prepare_filesystem_spool_for_ingest, resolve_source_selector,
};

struct PreparedJsonlSource {
    scope: JsonlScope,
    summaries: Vec<PreparedDocumentSummary>,
    explicit_deletions: Vec<Vec<u8>>,
}

enum PreparedWholeFailure {
    Filesystem {
        scope: FilesystemScope,
        code: String,
        detail: String,
    },
    Jsonl {
        scope: JsonlScope,
        code: String,
        detail: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectIngestPlan {
    pub aggregate: IngestPlan,
    pub targeted_sources: usize,
    pub failed_sources: usize,
}

pub fn plan_project_sources_with_timeout(
    context: &EffectiveContext,
    selectors: &[String],
    strict: bool,
    lock_timeout: Duration,
) -> Result<ProjectIngestPlan, ProjectIngestError> {
    let database = IndexDb::open_existing(&context.database_path, OpenMode::ReadOnly)?;
    let configured = list_project_sources(&database, context.project_id)?;
    let selected = select_sources(&configured, selectors)?;
    let targeted_sources = selected.len();
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    let staging_directory = database
        .path()
        .parent()
        .ok_or(FilesystemIngestError::StagingDirectoryMissing)?;
    let mut aggregate = empty_plan();
    let mut failed_sources = 0_usize;

    for source in selected {
        match source.kind {
            ConfiguredSourceKind::Filesystem => {
                let scope = filesystem_scope_from_context(context, source)?;
                match prepare_filesystem_spool_for_ingest(
                    &database,
                    &context.source_root,
                    &context.source_discovery_options,
                    staging_directory,
                    context.index_quota_bytes,
                ) {
                    Ok(spool) => {
                        if strict && !spool.failures.is_empty() {
                            return Err(ProjectIngestError::StrictFilesystemFailures {
                                source_id: source.source_id,
                                failures: spool.failures.len(),
                            });
                        }
                        let plan = database.plan_filesystem_summaries_under_lock(
                            &writer_lock,
                            &scope,
                            &spool.summaries,
                            &spool.failures,
                        )?;
                        if plan.failed_documents != 0 {
                            failed_sources = checked_add(failed_sources, 1)?;
                        }
                        merge_plan(&mut aggregate, &plan)?;
                    }
                    Err(FilesystemIngestError::Discovery(
                        error @ DiscoveryError::Staging { .. },
                    )) => return Err(ProjectIngestError::Filesystem(error.into())),
                    Err(FilesystemIngestError::Discovery(error)) => {
                        if strict {
                            return Err(ProjectIngestError::StrictFilesystemSource {
                                source_id: source.source_id,
                                detail: error.to_string(),
                            });
                        }
                        failed_sources = checked_add(failed_sources, 1)?;
                        merge_whole_failure(&mut aggregate, source)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            ConfiguredSourceKind::Jsonl => {
                let config = JsonlSourceConfig::parse(&source.config_json)?;
                if config.index_quota_bytes() != context.index_quota_bytes {
                    return Err(ProjectIngestError::InconsistentIndexQuota {
                        source_id: source.source_id,
                    });
                }
                let scope = JsonlScope {
                    source_id: source.source_id,
                    source_name: source.name.clone(),
                    source_logical_uri: source.logical_uri.clone(),
                    source_config_json: source.config_json.clone(),
                    project_id: context.project_id,
                    project_name: context.project_name.clone(),
                };
                match read_prepared_snapshot(&config) {
                    Ok(snapshot) => {
                        let plan = database.plan_jsonl_snapshot_under_lock(
                            &writer_lock,
                            &scope,
                            &snapshot.documents,
                            &snapshot.explicit_deletions,
                        )?;
                        merge_plan(&mut aggregate, &plan)?;
                    }
                    Err(error) => {
                        if strict {
                            return Err(ProjectIngestError::StrictJsonlSource {
                                source_id: source.source_id,
                                source: error,
                            });
                        }
                        failed_sources = checked_add(failed_sources, 1)?;
                        merge_whole_failure(&mut aggregate, source)?;
                    }
                }
            }
        }
    }
    Ok(ProjectIngestPlan {
        aggregate,
        targeted_sources,
        failed_sources,
    })
}

pub fn ingest_project_sources_with_timeout(
    context: &EffectiveContext,
    selectors: &[String],
    strict: bool,
    confirmations: DeleteConfirmations,
    lock_timeout: Duration,
) -> Result<IngestOutcome, ProjectIngestError> {
    let mut database = IndexDb::open_existing(&context.database_path, OpenMode::ReadWrite)?;
    let configured = list_project_sources(&database, context.project_id)?;
    let selected = select_sources(&configured, selectors)?;
    let writer_lock = WriterLock::acquire(database.path(), lock_timeout)?;
    let failure_budget = FAILURE_RECORD_ESTIMATED_WRITE_BYTES
        .checked_mul(u64::try_from(selected.len()).map_err(|_| StoreError::IntegerOverflow)?)
        .ok_or(StoreError::IntegerOverflow)?;
    StoragePreflight::run(database.path(), failure_budget, context.index_quota_bytes)?;

    let staging_directory = database
        .path()
        .parent()
        .ok_or(FilesystemIngestError::StagingDirectoryMissing)?;
    let mut filesystem_scope = None;
    let mut filesystem_spool = None;
    let mut filesystem_summaries = Vec::new();
    let mut filesystem_failures = Vec::new();
    let mut jsonl_sources = Vec::new();
    let mut jsonl_documents = BTreeMap::new();
    let mut whole_failures = Vec::new();
    let mut estimated_write_bytes = failure_budget;

    for source in selected {
        match source.kind {
            ConfiguredSourceKind::Filesystem => {
                let scope = filesystem_scope_from_context(context, source)?;
                let prepared = prepare_filesystem_spool_for_ingest(
                    &database,
                    &context.source_root,
                    &context.source_discovery_options,
                    staging_directory,
                    context.index_quota_bytes,
                );
                match prepared {
                    Ok(spool) => {
                        if strict && !spool.failures.is_empty() {
                            return Err(ProjectIngestError::StrictFilesystemFailures {
                                source_id: source.source_id,
                                failures: spool.failures.len(),
                            });
                        }
                        let plan = database.plan_filesystem_summaries_under_lock(
                            &writer_lock,
                            &scope,
                            &spool.summaries,
                            &spool.failures,
                        )?;
                        estimated_write_bytes = estimated_write_bytes
                            .checked_add(plan.estimated_write_bytes)
                            .ok_or(StoreError::IntegerOverflow)?;
                        filesystem_summaries = spool.summaries.clone();
                        filesystem_failures = spool.failures.clone();
                        filesystem_scope = Some(scope);
                        filesystem_spool = Some(spool);
                    }
                    Err(FilesystemIngestError::Discovery(
                        error @ DiscoveryError::Staging { .. },
                    )) => return Err(ProjectIngestError::Filesystem(error.into())),
                    Err(FilesystemIngestError::Discovery(error)) => {
                        if strict {
                            return Err(ProjectIngestError::StrictFilesystemSource {
                                source_id: source.source_id,
                                detail: error.to_string(),
                            });
                        }
                        whole_failures.push(PreparedWholeFailure::Filesystem {
                            scope,
                            code: discovery_error_code(&error).to_owned(),
                            detail: error.to_string(),
                        });
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            ConfiguredSourceKind::Jsonl => {
                let config = JsonlSourceConfig::parse(&source.config_json)?;
                if config.index_quota_bytes() != context.index_quota_bytes {
                    return Err(ProjectIngestError::InconsistentIndexQuota {
                        source_id: source.source_id,
                    });
                }
                let scope = JsonlScope {
                    source_id: source.source_id,
                    source_name: source.name.clone(),
                    source_logical_uri: source.logical_uri.clone(),
                    source_config_json: source.config_json.clone(),
                    project_id: context.project_id,
                    project_name: context.project_name.clone(),
                };
                match read_prepared_snapshot(&config) {
                    Ok(snapshot) => {
                        let plan = database.plan_jsonl_snapshot_under_lock(
                            &writer_lock,
                            &scope,
                            &snapshot.documents,
                            &snapshot.explicit_deletions,
                        )?;
                        estimated_write_bytes = estimated_write_bytes
                            .checked_add(plan.estimated_write_bytes)
                            .ok_or(StoreError::IntegerOverflow)?;
                        let summaries = snapshot
                            .documents
                            .iter()
                            .map(PreparedDocumentSummary::from_document)
                            .collect::<Result<Vec<_>, _>>()?;
                        for document in snapshot.documents {
                            jsonl_documents.insert(
                                (source.source_id, document.connector_key.clone()),
                                document,
                            );
                        }
                        jsonl_sources.push(PreparedJsonlSource {
                            scope,
                            summaries,
                            explicit_deletions: snapshot.explicit_deletions,
                        });
                    }
                    Err(error) => {
                        if strict {
                            return Err(ProjectIngestError::StrictJsonlSource {
                                source_id: source.source_id,
                                source: error,
                            });
                        }
                        whole_failures.push(PreparedWholeFailure::Jsonl {
                            scope,
                            code: SOURCE_FAILURE_CODE.to_owned(),
                            detail: bounded_failure_detail(&error),
                        });
                    }
                }
            }
        }
    }

    let storage_preflight = StoragePreflight::run(
        database.path(),
        estimated_write_bytes,
        context.index_quota_bytes,
    )?;
    let mut batch = Vec::new();
    if let Some(scope) = filesystem_scope.as_ref() {
        batch.push(PreparedSourceBatch::filesystem_snapshot(
            scope,
            &filesystem_summaries,
            &filesystem_failures,
        ));
    }
    for source in &jsonl_sources {
        batch.push(PreparedSourceBatch::jsonl_snapshot(
            &source.scope,
            &source.summaries,
            &source.explicit_deletions,
        ));
    }
    for failure in &whole_failures {
        match failure {
            PreparedWholeFailure::Filesystem {
                scope,
                code,
                detail,
            } => batch.push(PreparedSourceBatch::filesystem_failed(scope, code, detail)),
            PreparedWholeFailure::Jsonl {
                scope,
                code,
                detail,
            } => batch.push(PreparedSourceBatch::jsonl_failed(scope, code, detail)),
        }
    }

    let filesystem_entry_by_key = filesystem_spool
        .as_ref()
        .map(super::PreparedFilesystemSpool::entry_index)
        .unwrap_or_default();
    let filesystem_source_id = filesystem_scope.as_ref().map(|scope| scope.source_id);
    let mut outcome = database.apply_prepared_source_batch_under_lock(
        &writer_lock,
        &batch,
        confirmations,
        |source_id, summary| {
            if filesystem_source_id == Some(source_id) {
                return filesystem_spool
                    .as_mut()
                    .ok_or(StoreError::InvalidPreparedDocument(
                        "filesystem spool is unavailable",
                    ))?
                    .load_document(summary, &filesystem_entry_by_key);
            }
            jsonl_documents
                .remove(&(source_id, summary.connector_key.clone()))
                .ok_or(StoreError::InvalidPreparedDocument(
                    "JSONL prepared document is unavailable",
                ))
        },
    )?;
    outcome.storage_preflight = Some(storage_preflight);
    Ok(outcome)
}

fn select_sources<'a>(
    configured: &'a [ConfiguredSource],
    selectors: &[String],
) -> Result<Vec<&'a ConfiguredSource>, ProjectIngestError> {
    if configured.is_empty() {
        return Err(ProjectIngestError::NoSources);
    }
    if selectors.is_empty() {
        return Ok(configured.iter().collect());
    }
    let mut selected_ids = BTreeSet::new();
    for selector in selectors {
        selected_ids.insert(resolve_source_selector(configured, selector)?);
    }
    Ok(configured
        .iter()
        .filter(|source| selected_ids.contains(&source.source_id))
        .collect())
}

fn empty_plan() -> IngestPlan {
    IngestPlan {
        new_documents: 0,
        changed_documents: 0,
        renamed_documents: 0,
        unchanged_documents: 0,
        tombstoned_documents: 0,
        carried_forward_documents: 0,
        failed_documents: 0,
        prior_active_documents: 0,
        projected_active_documents: 0,
        would_create_generation: false,
        requires_empty_snapshot_confirmation: false,
        requires_mass_delete_confirmation: false,
        estimated_write_bytes: 0,
    }
}

fn merge_plan(aggregate: &mut IngestPlan, source: &IngestPlan) -> Result<(), StoreError> {
    aggregate.new_documents = checked_add(aggregate.new_documents, source.new_documents)?;
    aggregate.changed_documents =
        checked_add(aggregate.changed_documents, source.changed_documents)?;
    aggregate.renamed_documents =
        checked_add(aggregate.renamed_documents, source.renamed_documents)?;
    aggregate.unchanged_documents =
        checked_add(aggregate.unchanged_documents, source.unchanged_documents)?;
    aggregate.tombstoned_documents =
        checked_add(aggregate.tombstoned_documents, source.tombstoned_documents)?;
    aggregate.carried_forward_documents = checked_add(
        aggregate.carried_forward_documents,
        source.carried_forward_documents,
    )?;
    aggregate.failed_documents = checked_add(aggregate.failed_documents, source.failed_documents)?;
    aggregate.prior_active_documents = checked_add(
        aggregate.prior_active_documents,
        source.prior_active_documents,
    )?;
    aggregate.projected_active_documents = checked_add(
        aggregate.projected_active_documents,
        source.projected_active_documents,
    )?;
    aggregate.would_create_generation |= source.would_create_generation;
    aggregate.requires_empty_snapshot_confirmation |= source.requires_empty_snapshot_confirmation;
    aggregate.requires_mass_delete_confirmation |= source.requires_mass_delete_confirmation;
    aggregate.estimated_write_bytes = aggregate
        .estimated_write_bytes
        .checked_add(source.estimated_write_bytes)
        .ok_or(StoreError::IntegerOverflow)?;
    Ok(())
}

fn merge_whole_failure(
    aggregate: &mut IngestPlan,
    source: &ConfiguredSource,
) -> Result<(), StoreError> {
    aggregate.failed_documents = checked_add(aggregate.failed_documents, 1)?;
    aggregate.carried_forward_documents =
        checked_add(aggregate.carried_forward_documents, source.active_documents)?;
    aggregate.prior_active_documents =
        checked_add(aggregate.prior_active_documents, source.active_documents)?;
    aggregate.projected_active_documents = checked_add(
        aggregate.projected_active_documents,
        source.active_documents,
    )?;
    aggregate.estimated_write_bytes = aggregate
        .estimated_write_bytes
        .checked_add(FAILURE_RECORD_ESTIMATED_WRITE_BYTES)
        .ok_or(StoreError::IntegerOverflow)?;
    Ok(())
}

fn checked_add(left: usize, right: usize) -> Result<usize, StoreError> {
    left.checked_add(right).ok_or(StoreError::IntegerOverflow)
}

fn filesystem_scope_from_context(
    context: &EffectiveContext,
    source: &ConfiguredSource,
) -> Result<FilesystemScope, ProjectIngestError> {
    if source.source_id != context.source_id
        || source.name != context.source_name
        || source.logical_uri != context.source_root.to_string_lossy()
        || source.config_json != context.source_config_json
    {
        return Err(ProjectIngestError::FilesystemAuthorityMismatch);
    }
    Ok(FilesystemScope {
        source_id: source.source_id,
        source_name: source.name.clone(),
        source_logical_uri: source.logical_uri.clone(),
        source_config_json: source.config_json.clone(),
        project_id: context.project_id,
        project_name: context.project_name.clone(),
    })
}

#[derive(Debug, Error)]
pub enum ProjectIngestError {
    #[error("the selected project has no configured sources")]
    NoSources,
    #[error("the stored filesystem authority does not match the selected context")]
    FilesystemAuthorityMismatch,
    #[error("strict ingest rejected filesystem source {source_id}: {detail}")]
    StrictFilesystemSource { source_id: SourceId, detail: String },
    #[error("strict ingest rejected {failures} filesystem records in source {source_id}")]
    StrictFilesystemFailures {
        source_id: SourceId,
        failures: usize,
    },
    #[error("strict ingest rejected JSONL source {source_id}")]
    StrictJsonlSource {
        source_id: SourceId,
        #[source]
        source: JsonlFileIngestError,
    },
    #[error("source {source_id} has an index quota inconsistent with the selected index")]
    InconsistentIndexQuota { source_id: SourceId },
    #[error(transparent)]
    SourceManagement(#[from] SourceManagementError),
    #[error(transparent)]
    Filesystem(#[from] FilesystemIngestError),
    #[error(transparent)]
    JsonlConfig(#[from] JsonlSourceConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
}
