use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use super::{
    FilesystemIngestError, FilesystemSourceConfig, estimate_filesystem_spool,
    prepare_filesystem_spool_with_estimate,
};
use crate::config::{
    AtomicSaveOutcome, BindingId, ManagedPaths, PointerError, RegistrationOutcome,
    RepositoryPointer, TrustBinding, TrustError, TrustRegistration, TrustRegistry,
    canonicalize_repository_root,
};
use crate::domain::{IndexId, ProjectId, SafeSlug, Sha256Digest, SlugError, SourceId};
use crate::ingest::{
    DiscoveryError, DiscoveryOptions, FilesystemDiscoveryEstimate, HARD_MAX_SOURCE_BYTES,
    HARD_MAX_SOURCE_FILES, extract_identifier_literals,
};
use crate::search::SearchRequest;
use crate::store::{
    DeleteConfirmations, Doctor, FilesystemScope, FingerprintPolicy, IndexDb, IngestOutcome,
    OpenMode, StoragePreflight, StoragePreflightError, StoreError, WriterLock,
};

const TRUST_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PROJECT_NAME: &str = "default";
const DEFAULT_SOURCE_NAME: &str = "workspace";

#[derive(Clone, Debug)]
pub struct InitRequest {
    pub current_dir: PathBuf,
    pub requested_root: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
    pub managed_paths: ManagedPaths,
    pub index_name: Option<SafeSlug>,
    pub project_name: Option<SafeSlug>,
    pub rebuild: bool,
    pub no_ingest: bool,
    pub dry_run: bool,
    pub write_pointer: bool,
    pub force_pointer: bool,
    pub allow_broad_root: bool,
    pub allow_large_source: bool,
    pub index_quota_bytes: Option<u64>,
}

impl InitRequest {
    pub fn new(current_dir: PathBuf, managed_paths: ManagedPaths) -> Self {
        Self {
            current_dir,
            requested_root: None,
            home_dir: env::var_os("HOME").map(PathBuf::from),
            managed_paths,
            index_name: None,
            project_name: None,
            rebuild: false,
            no_ingest: false,
            dry_run: false,
            write_pointer: false,
            force_pointer: false,
            allow_broad_root: false,
            allow_large_source: false,
            index_quota_bytes: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerOutcome {
    NotRequested,
    WouldWrite,
    Written,
    WrittenDurabilityUnknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceEstimate {
    pub eligible_files: usize,
    pub eligible_bytes: u64,
    pub skipped_files: usize,
}

#[derive(Clone, Debug)]
pub struct InitOutcome {
    pub canonical_root: PathBuf,
    pub index_name: SafeSlug,
    pub project_name: SafeSlug,
    pub database_path: PathBuf,
    pub index_id: IndexId,
    pub project_id: ProjectId,
    pub source_id: SourceId,
    pub binding_id: Option<BindingId>,
    pub rebuild: Option<RebuildSummary>,
    pub reused: bool,
    pub dry_run: bool,
    pub pointer: PointerOutcome,
    pub trust_durability: Option<AtomicSaveOutcome>,
    pub source_estimate: SourceEstimate,
    pub ingest: Option<IngestOutcome>,
    pub storage_preflight: Option<StoragePreflight>,
    pub next_step: InitNextStep,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebuildSummary {
    pub previous_binding_id: BindingId,
    pub previous_index_id: IndexId,
    pub previous_pipeline_fingerprint: Sha256Digest,
    pub active_documents: u64,
    pub active_passages: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InitNextStep {
    Search { query: String },
    Status,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustTarget {
    pub index_id: IndexId,
    pub index_name: SafeSlug,
    pub project_id: ProjectId,
    pub project_name: SafeSlug,
}

#[derive(Clone, Debug)]
pub struct TrustRequest {
    pub root: PathBuf,
    pub managed_paths: ManagedPaths,
    pub target: TrustTarget,
    pub confirm: bool,
}

#[derive(Clone, Debug)]
pub struct TrustOutcome {
    pub binding: TrustBinding,
    pub created: bool,
    pub durability: Option<AtomicSaveOutcome>,
}

pub fn initialize(request: &InitRequest) -> Result<InitOutcome, InitError> {
    initialize_with_rebuild_observer(request, |_| {})
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RebuildCheckpoint {
    TrustBindingRemoved,
    DatabaseRemoved,
    ReplacementDatabaseCreated,
    ReplacementBindingRegistered,
}

fn initialize_with_rebuild_observer(
    request: &InitRequest,
    mut observe_rebuild: impl FnMut(RebuildCheckpoint),
) -> Result<InitOutcome, InitError> {
    if request.force_pointer && !request.write_pointer {
        return Err(InitError::ForcePointerWithoutWrite);
    }
    if request.rebuild && request.no_ingest {
        return Err(InitError::RebuildWithoutIngest);
    }

    let canonical_current = canonicalize_repository_root(&request.current_dir)?;
    let canonical_root = select_root(request, &canonical_current)?;
    enforce_safe_root(
        &canonical_root,
        &canonical_current,
        request.home_dir.as_deref(),
        request.allow_broad_root,
    )?;
    let root_text = canonical_root
        .to_str()
        .ok_or_else(|| InitError::NonUtf8Root {
            root: canonical_root.clone(),
        })?
        .to_owned();

    let trust_path = request.managed_paths.trust_registry_file();
    let registry = load_registry(&trust_path)?;
    let existing_binding = binding_for_root(&registry, &canonical_root)?;
    let (index_name, project_name) = select_logical_names(
        request,
        &canonical_root,
        existing_binding.as_ref(),
        &registry,
    )?;
    let database_path = request.managed_paths.index_database(&index_name);
    let pointer_plan = PointerPlan::preflight(
        &canonical_root,
        request.write_pointer,
        request.force_pointer,
        &index_name,
        &project_name,
    )?;

    let mut rebuild_lock = None;
    let rebuild = if let Some(binding) = existing_binding.as_ref() {
        if !database_path.is_file() {
            return Err(InitError::TrustedIndexMissing {
                path: database_path,
            });
        }
        if request.rebuild {
            let writer_lock = if request.dry_run {
                None
            } else {
                Some(WriterLock::acquire(
                    &database_path,
                    crate::store::DEFAULT_WRITER_LOCK_TIMEOUT,
                )?)
            };
            let inspection = validate_trusted_database_with_policy(
                &database_path,
                binding.index_id(),
                binding.project_id(),
                binding.project_name(),
                &canonical_root,
                FingerprintPolicy::Tolerate,
            )?;
            rebuild_lock = writer_lock;
            Some(RebuildSummary {
                previous_binding_id: binding.binding_id(),
                previous_index_id: binding.index_id(),
                previous_pipeline_fingerprint: inspection.pipeline_fingerprint,
                active_documents: inspection.active_documents,
                active_passages: inspection.active_passages,
            })
        } else {
            validate_trusted_database(
                &database_path,
                binding.index_id(),
                binding.project_id(),
                binding.project_name(),
                &canonical_root,
            )?;

            let pointer = if request.dry_run {
                pointer_plan.dry_run_outcome()
            } else {
                pointer_plan.apply()?
            };
            return Ok(InitOutcome {
                canonical_root,
                index_name,
                project_name,
                database_path,
                index_id: binding.index_id(),
                project_id: binding.project_id(),
                source_id: derive_source_id(binding.index_id(), binding.project_id()),
                binding_id: Some(binding.binding_id()),
                rebuild: None,
                reused: true,
                dry_run: request.dry_run,
                pointer,
                trust_durability: None,
                source_estimate: SourceEstimate::default(),
                ingest: None,
                storage_preflight: None,
                next_step: InitNextStep::Status,
            });
        }
    } else if request.rebuild {
        return Err(InitError::RebuildBindingRequired {
            root: canonical_root,
        });
    } else {
        None
    };

    if !request.rebuild && request.index_name.is_some() && database_path.try_exists()? {
        return Err(InitError::IndexPathOccupied {
            path: database_path,
        });
    }

    let discovery_options = initial_discovery_options(request.allow_large_source);
    let discovery_estimate = if request.no_ingest {
        None
    } else {
        Some(estimate_initial_source(
            &canonical_root,
            &discovery_options,
            request.allow_large_source,
        )?)
    };
    let source_estimate = match discovery_estimate {
        None => SourceEstimate::default(),
        Some(estimate) => SourceEstimate {
            eligible_files: estimate
                .eligible_files
                .checked_add(estimate.skipped_files)
                .ok_or(InitError::SourceEstimateOverflow)?,
            eligible_bytes: estimate.eligible_bytes,
            skipped_files: estimate.skipped_files,
        },
    };

    let index_id = IndexId::new_v4();
    let project_id = ProjectId::new_v4();
    let source_id = derive_source_id(index_id, project_id);

    if request.dry_run {
        return Ok(InitOutcome {
            canonical_root,
            index_name,
            project_name,
            database_path,
            index_id,
            project_id,
            source_id,
            binding_id: None,
            rebuild,
            reused: false,
            dry_run: true,
            pointer: pointer_plan.dry_run_outcome(),
            trust_durability: None,
            source_estimate,
            ingest: None,
            storage_preflight: None,
            next_step: InitNextStep::Status,
        });
    }

    let parent = database_path
        .parent()
        .ok_or_else(|| InitError::DatabasePathHasNoParent {
            path: database_path.clone(),
        })?;
    create_private_directory(parent)?;

    let estimated_write_bytes = source_estimate
        .eligible_bytes
        .checked_mul(6)
        .ok_or(InitError::SourceEstimateOverflow)?;
    let mut prepared = if let Some(estimate) = discovery_estimate {
        let estimated_peak_bytes = estimated_write_bytes
            .checked_add(estimate.eligible_bytes)
            .ok_or(InitError::SourceEstimateOverflow)?;
        StoragePreflight::run(
            &database_path,
            estimated_peak_bytes,
            request.index_quota_bytes,
        )?;
        Some(prepare_filesystem_spool_with_estimate(
            &canonical_root,
            &discovery_options,
            parent,
            estimate,
        )?)
    } else {
        None
    };
    let storage_preflight = StoragePreflight::run(
        &database_path,
        estimated_write_bytes,
        request.index_quota_bytes,
    )?;
    if request.rebuild {
        let binding = existing_binding
            .as_ref()
            .expect("rebuild requires the existing binding validated above");
        unregister_and_save(&trust_path, binding)?;
        observe_rebuild(RebuildCheckpoint::TrustBindingRemoved);
        let database = IndexDb::open_existing_with_policy(
            &database_path,
            OpenMode::ReadWrite,
            FingerprintPolicy::Tolerate,
        )?;
        database.remove()?;
        observe_rebuild(RebuildCheckpoint::DatabaseRemoved);
        drop(rebuild_lock.take());
    }
    let mut database = IndexDb::create(&database_path, index_id)?;
    if request.rebuild {
        observe_rebuild(RebuildCheckpoint::ReplacementDatabaseCreated);
    }
    let mut rollback = NewIndexRollback::new(database_path.clone());
    if let Err(error) = set_private_file(&database_path) {
        drop(database);
        return Err(error);
    }
    let scope = FilesystemScope {
        source_id,
        source_name: SafeSlug::new(DEFAULT_SOURCE_NAME)
            .expect("the built-in source name is a safe slug"),
        source_logical_uri: root_text.clone(),
        source_config_json: FilesystemSourceConfig::new(canonical_root.clone(), discovery_options)?
            .with_index_quota_bytes(request.index_quota_bytes)?
            .to_canonical_json()?,
        project_id,
        project_name: project_name.clone(),
    };
    let ingest = if request.no_ingest {
        if let Err(error) = database.configure_filesystem_scope(&scope) {
            drop(database);
            return Err(error.into());
        }
        None
    } else {
        let prepared = prepared
            .as_mut()
            .expect("an ingesting init always has a prepared spool");
        let summaries = prepared.summaries.clone();
        let failures = prepared.failures.clone();
        let entry_by_key = prepared.entry_index();
        let writer_lock =
            WriterLock::acquire(&database_path, crate::store::DEFAULT_WRITER_LOCK_TIMEOUT)?;
        let ingest_result = database.apply_filesystem_summaries_under_lock(
            &writer_lock,
            &scope,
            &summaries,
            &failures,
            DeleteConfirmations::default(),
            |summary| prepared.load_document(summary, &entry_by_key),
        );
        match ingest_result {
            Ok(outcome) => Some(outcome),
            Err(error) => {
                drop(database);
                return Err(error.into());
            }
        }
    };
    let next_step = if request.no_ingest {
        InitNextStep::Status
    } else {
        verified_next_step(&database, project_id)
    };
    drop(database);

    let (pointer_rollback, pointer_outcome) = pointer_plan.apply_reversibly()?;
    let registration = TrustRegistration {
        canonical_root: canonical_root.clone(),
        index_id,
        index_name: index_name.clone(),
        project_id,
        project_name: project_name.clone(),
    };
    let registration_result = register_and_save(&trust_path, registration);
    let registration = match registration_result {
        Ok(outcome) => outcome,
        Err(error) => {
            pointer_rollback.rollback()?;
            return Err(error);
        }
    };
    let binding = outcome_binding(registration.outcome);
    if request.rebuild {
        observe_rebuild(RebuildCheckpoint::ReplacementBindingRegistered);
    }

    pointer_rollback.commit();
    rollback.commit();
    Ok(InitOutcome {
        canonical_root,
        index_name,
        project_name,
        database_path,
        index_id,
        project_id,
        source_id,
        binding_id: Some(binding.binding_id()),
        rebuild,
        reused: false,
        dry_run: false,
        pointer: pointer_outcome,
        trust_durability: registration.durability,
        source_estimate,
        ingest,
        storage_preflight: Some(storage_preflight),
        next_step,
    })
}

fn verified_next_step(database: &IndexDb, project_id: ProjectId) -> InitNextStep {
    for candidate in suggested_queries(database).into_iter().take(32) {
        let Ok(request) = SearchRequest::with_defaults(&candidate.query) else {
            continue;
        };
        if database.search(project_id, &request).is_ok_and(|response| {
            response.results.iter().any(|result| {
                result.source_uri == candidate.source_uri
                    && result.content_sha256 == candidate.content_sha256
            })
        }) {
            return InitNextStep::Search {
                query: candidate.query,
            };
        }
    }
    InitNextStep::Status
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SuggestedQuery {
    query: String,
    source_uri: String,
    content_sha256: crate::domain::Sha256Digest,
}

fn suggested_queries(database: &IndexDb) -> Vec<SuggestedQuery> {
    let mut headings = Vec::new();
    let mut identifiers = Vec::new();
    let Ok(mut statement) = database.connection().prepare(
        "SELECT dv.source_uri, c.body_text
         FROM active_passages AS ap
         JOIN document_versions AS dv ON dv.id = ap.document_version_id
         JOIN chunks AS c ON c.id = ap.chunk_id
         ORDER BY ap.id
         LIMIT 64",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return Vec::new();
    };
    for row in rows {
        let Ok((source_uri, body_text)) = row else {
            return Vec::new();
        };
        let content_sha256 = crate::domain::Sha256Digest::of_bytes(body_text.as_bytes());
        for line in body_text.trim_start_matches('\u{feff}').lines() {
            let heading = line.trim_start();
            let marker_bytes = heading.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&marker_bytes)
                && heading.as_bytes().get(marker_bytes) == Some(&b' ')
            {
                let candidate = heading[marker_bytes + 1..].trim();
                if printable_query(candidate) {
                    headings.push(SuggestedQuery {
                        query: candidate.to_owned(),
                        source_uri: source_uri.clone(),
                        content_sha256,
                    });
                }
            }
        }
        for literal in extract_identifier_literals(body_text.as_bytes()) {
            if let Ok(candidate) = std::str::from_utf8(&literal)
                && printable_query(candidate)
            {
                identifiers.push(SuggestedQuery {
                    query: candidate.to_owned(),
                    source_uri: source_uri.clone(),
                    content_sha256,
                });
            }
        }
    }
    headings.extend(identifiers);
    headings
}

fn printable_query(value: &str) -> bool {
    (2..=256).contains(&value.len())
        && value
            .chars()
            .all(|character| character == ' ' || character.is_ascii_graphic())
}

pub fn trust_repository(request: &TrustRequest) -> Result<TrustOutcome, InitError> {
    if !request.confirm {
        return Err(InitError::TrustConfirmationRequired);
    }

    let canonical_root = canonicalize_repository_root(&request.root)?;
    let database_path = request
        .managed_paths
        .index_database(&request.target.index_name);
    validate_trusted_database(
        &database_path,
        request.target.index_id,
        request.target.project_id,
        &request.target.project_name,
        &canonical_root,
    )?;

    let registration = TrustRegistration {
        canonical_root,
        index_id: request.target.index_id,
        index_name: request.target.index_name.clone(),
        project_id: request.target.project_id,
        project_name: request.target.project_name.clone(),
    };
    let registration =
        register_and_save(&request.managed_paths.trust_registry_file(), registration)?;
    Ok(match registration.outcome {
        RegistrationOutcome::Created(binding) => TrustOutcome {
            binding,
            created: true,
            durability: registration.durability,
        },
        RegistrationOutcome::Existing(binding) => TrustOutcome {
            binding,
            created: false,
            durability: registration.durability,
        },
    })
}

fn validate_trusted_database(
    database_path: &Path,
    expected_index_id: IndexId,
    expected_project_id: ProjectId,
    expected_project_name: &SafeSlug,
    expected_root: &Path,
) -> Result<TrustedIndexInspection, InitError> {
    validate_trusted_database_with_policy(
        database_path,
        expected_index_id,
        expected_project_id,
        expected_project_name,
        expected_root,
        FingerprintPolicy::Reject,
    )
}

fn validate_trusted_database_with_policy(
    database_path: &Path,
    expected_index_id: IndexId,
    expected_project_id: ProjectId,
    expected_project_name: &SafeSlug,
    expected_root: &Path,
    fingerprint_policy: FingerprintPolicy,
) -> Result<TrustedIndexInspection, InitError> {
    let report = Doctor::run_with_policy(database_path, fingerprint_policy)?;
    if report.index_id != expected_index_id {
        return Err(InitError::TrustedIndexIdentityMismatch {
            expected: expected_index_id,
            actual: report.index_id,
        });
    }

    let database =
        IndexDb::open_existing_with_policy(database_path, OpenMode::ReadOnly, fingerprint_policy)?;
    let connection = database.connection();
    let project_matches: bool = connection
        .query_row(
            "SELECT EXISTS(
            SELECT 1 FROM projects WHERE id = ?1 AND name = ?2
        )",
            params![
                expected_project_id.as_uuid().as_bytes().as_slice(),
                expected_project_name.as_str(),
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    if !project_matches {
        return Err(InitError::TrustedProjectIdentityMismatch {
            expected_id: expected_project_id,
            expected_name: expected_project_name.clone(),
        });
    }

    let linked_sources: i64 = connection
        .query_row(
            "SELECT COUNT(*)
         FROM project_sources AS ps
         JOIN sources AS s ON s.id = ps.source_id
         WHERE ps.project_id = ?1",
            [expected_project_id.as_uuid().as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    let linked_sources =
        usize::try_from(linked_sources).map_err(|_| InitError::InvalidTrustedCardinality)?;
    if linked_sources != 1 {
        return Err(InitError::AlphaSourceCardinality {
            found: linked_sources,
        });
    }

    let expected_source_id = derive_source_id(expected_index_id, expected_project_id);
    let source_matches: bool = connection
        .query_row(
            "SELECT EXISTS(
            SELECT 1
            FROM project_sources AS ps
            JOIN sources AS s ON s.id = ps.source_id
            WHERE ps.project_id = ?1 AND s.id = ?2 AND s.kind = 'filesystem'
        )",
            [
                expected_project_id.as_uuid().as_bytes().as_slice(),
                expected_source_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    if !source_matches {
        return Err(InitError::TrustedSourceIdentityMismatch {
            expected: expected_source_id,
        });
    }

    let (source_logical_uri, source_config_json): (String, String) = connection
        .query_row(
            "SELECT s.logical_uri, s.config_json
             FROM project_sources AS ps
             JOIN sources AS s ON s.id = ps.source_id
             WHERE ps.project_id = ?1 AND s.id = ?2",
            [
                expected_project_id.as_uuid().as_bytes().as_slice(),
                expected_source_id.as_uuid().as_bytes().as_slice(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StoreError::from)?;
    let expected_root_text = expected_root
        .to_str()
        .ok_or_else(|| InitError::NonUtf8Root {
            root: expected_root.to_path_buf(),
        })?;
    let source_config = FilesystemSourceConfig::parse(&source_config_json)?;
    if source_logical_uri != expected_root_text || source_config.root() != expected_root {
        return Err(InitError::TrustedSourceRootMismatch {
            expected: expected_root.to_path_buf(),
        });
    }
    let active_documents: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM document_heads WHERE state = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    let active_passages: i64 = connection
        .query_row("SELECT COUNT(*) FROM active_passages", [], |row| row.get(0))
        .map_err(StoreError::from)?;
    Ok(TrustedIndexInspection {
        pipeline_fingerprint: report.pipeline_fingerprint,
        active_documents: u64::try_from(active_documents)
            .map_err(|_| StoreError::IntegerOverflow)?,
        active_passages: u64::try_from(active_passages).map_err(|_| StoreError::IntegerOverflow)?,
    })
}

struct TrustedIndexInspection {
    pipeline_fingerprint: Sha256Digest,
    active_documents: u64,
    active_passages: u64,
}

fn select_root(request: &InitRequest, canonical_current: &Path) -> Result<PathBuf, InitError> {
    if let Some(root) = &request.requested_root {
        return canonicalize_repository_root(root).map_err(Into::into);
    }
    Ok(find_enclosing_git_root(canonical_current)
        .unwrap_or_else(|| canonical_current.to_path_buf()))
}

fn find_enclosing_git_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        let marker = candidate.join(".git");
        match fs::symlink_metadata(marker) {
            Ok(metadata) if !metadata.file_type().is_symlink() => {
                let kind = metadata.file_type();
                (kind.is_dir() || kind.is_file()).then(|| candidate.to_path_buf())
            }
            _ => None,
        }
    })
}

fn enforce_safe_root(
    root: &Path,
    current: &Path,
    environment_home: Option<&Path>,
    allow_broad_root: bool,
) -> Result<(), InitError> {
    if allow_broad_root {
        return Ok(());
    }
    if root.parent().is_none() {
        return Err(InitError::BroadRootConfirmationRequired {
            root: root.to_path_buf(),
            reason: BroadRootReason::FilesystemRoot,
        });
    }
    let account_home = canonical_account_home().map_err(InitError::AccountHomeUnavailable)?;
    if root == account_home {
        return Err(InitError::BroadRootConfirmationRequired {
            root: root.to_path_buf(),
            reason: BroadRootReason::HomeDirectory,
        });
    }
    if let Some(environment_home) = environment_home
        && fs::canonicalize(environment_home).is_ok_and(|home| root == home)
    {
        return Err(InitError::BroadRootConfirmationRequired {
            root: root.to_path_buf(),
            reason: BroadRootReason::HomeDirectory,
        });
    }
    if let Some(git_root) = find_enclosing_git_root(current)
        && root != git_root
        && git_root.starts_with(root)
    {
        return Err(InitError::BroadRootConfirmationRequired {
            root: root.to_path_buf(),
            reason: BroadRootReason::AboveGitWorktree { git_root },
        });
    }
    Ok(())
}

#[cfg(unix)]
fn canonical_account_home() -> io::Result<PathBuf> {
    use uzers::os::unix::UserExt;

    let user = uzers::get_user_by_uid(uzers::get_current_uid()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "current account is absent from the account database",
        )
    })?;
    if user.home_dir().as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "current account home directory is empty",
        ));
    }
    fs::canonicalize(user.home_dir())
}

#[cfg(not(unix))]
fn canonical_account_home() -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OS account home lookup is unsupported on this platform",
    ))
}

fn select_logical_names(
    request: &InitRequest,
    canonical_root: &Path,
    existing: Option<&TrustBinding>,
    registry: &TrustRegistry,
) -> Result<(SafeSlug, SafeSlug), InitError> {
    if let Some(binding) = existing {
        if request
            .index_name
            .as_ref()
            .is_some_and(|name| name != binding.index_name())
            || request
                .project_name
                .as_ref()
                .is_some_and(|name| name != binding.project_name())
        {
            return Err(InitError::ExistingBindingConflict {
                root: binding.canonical_root().to_path_buf(),
                index_name: binding.index_name().clone(),
                project_name: binding.project_name().clone(),
            });
        }
        return Ok((binding.index_name().clone(), binding.project_name().clone()));
    }

    let project_name = request
        .project_name
        .clone()
        .unwrap_or_else(|| SafeSlug::new(DEFAULT_PROJECT_NAME).expect("built-in slug is valid"));
    let index_name = match &request.index_name {
        Some(name) => name.clone(),
        None => collision_safe_index_name(canonical_root, &request.managed_paths, registry)?,
    };
    Ok((index_name, project_name))
}

fn collision_safe_index_name(
    source: &Path,
    paths: &ManagedPaths,
    registry: &TrustRegistry,
) -> Result<SafeSlug, InitError> {
    let base = slugify_path_name(source);
    for ordinal in 1_u32..=10_000 {
        let candidate_text = if ordinal == 1 {
            base.clone()
        } else {
            append_slug_suffix(&base, ordinal)
        };
        let candidate = SafeSlug::new(candidate_text)?;
        let name_is_bound = registry
            .bindings()
            .iter()
            .any(|binding| binding.index_name() == &candidate);
        if !name_is_bound && !paths.index_database(&candidate).try_exists()? {
            return Ok(candidate);
        }
    }
    Err(InitError::IndexNameSpaceExhausted { base })
}

fn slugify_path_name(path: &Path) -> String {
    let source = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("index");
    let mut output = String::with_capacity(source.len().min(64));
    let mut prior_separator = false;
    for character in source.chars().flat_map(char::to_lowercase) {
        if output.len() == 64 {
            break;
        }
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            output.push(character);
            prior_separator = false;
        } else if !prior_separator && !output.is_empty() {
            output.push('-');
            prior_separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        output.push_str("index");
    }
    output
}

fn append_slug_suffix(base: &str, ordinal: u32) -> String {
    let suffix = format!("-{ordinal}");
    let keep = 64_usize.saturating_sub(suffix.len());
    let mut truncated = base[..base.len().min(keep)]
        .trim_end_matches('-')
        .to_owned();
    truncated.push_str(&suffix);
    truncated
}

fn estimate_initial_source(
    root: &Path,
    options: &DiscoveryOptions,
    allow_large_source: bool,
) -> Result<FilesystemDiscoveryEstimate, InitError> {
    match estimate_filesystem_spool(root, options) {
        Err(FilesystemIngestError::Discovery(DiscoveryError::SourceLimitExceeded {
            files,
            bytes,
            max_files,
            max_bytes,
        })) if !allow_large_source => Err(InitError::LargeSourceConfirmationRequired {
            files,
            bytes,
            max_files,
            max_bytes,
        }),
        result => result.map_err(Into::into),
    }
}

fn initial_discovery_options(allow_large_source: bool) -> DiscoveryOptions {
    if allow_large_source {
        DiscoveryOptions::default()
            .with_source_limits(HARD_MAX_SOURCE_FILES, HARD_MAX_SOURCE_BYTES)
            .expect("built-in confirmed source limits are within the hard ceilings")
    } else {
        DiscoveryOptions::default()
    }
}

fn derive_source_id(index_id: IndexId, project_id: ProjectId) -> SourceId {
    let mut hasher = Sha256::new();
    hasher.update(b"hsum.alpha1.filesystem-source\0");
    hasher.update(index_id.as_uuid().as_bytes());
    hasher.update(project_id.as_uuid().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    SourceId::from_uuid(Uuid::from_bytes(bytes))
}

fn binding_for_root(
    registry: &TrustRegistry,
    canonical_root: &Path,
) -> Result<Option<TrustBinding>, InitError> {
    let matches = registry
        .bindings()
        .iter()
        .filter(|binding| binding.canonical_root() == canonical_root)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [binding] => Ok(Some((*binding).clone())),
        _ => Err(InitError::AmbiguousRootBindings {
            root: canonical_root.to_path_buf(),
            matches: matches.len(),
        }),
    }
}

fn load_registry(path: &Path) -> Result<TrustRegistry, InitError> {
    match TrustRegistry::load(path) {
        Ok(registry) => Ok(registry),
        Err(TrustError::Read(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok(TrustRegistry::new())
        }
        Err(error) => Err(error.into()),
    }
}

fn unregister_and_save(
    path: &Path,
    expected_binding: &TrustBinding,
) -> Result<AtomicSaveOutcome, InitError> {
    let parent = path
        .parent()
        .ok_or_else(|| InitError::TrustPathHasNoParent {
            path: path.to_path_buf(),
        })?;
    create_private_directory(parent)?;
    let _lock = WriterLock::acquire(path, TRUST_LOCK_TIMEOUT)?;
    let registry = load_registry(path)?;
    let Some(current_binding) = registry
        .bindings()
        .iter()
        .find(|binding| binding.binding_id() == expected_binding.binding_id())
    else {
        return Err(InitError::RebuildBindingChanged {
            binding_id: expected_binding.binding_id(),
        });
    };
    if current_binding != expected_binding {
        return Err(InitError::RebuildBindingChanged {
            binding_id: expected_binding.binding_id(),
        });
    }
    let retained = registry
        .bindings()
        .iter()
        .filter(|binding| binding.binding_id() != expected_binding.binding_id())
        .cloned()
        .collect();
    Ok(TrustRegistry::from_bindings(retained)?.save_atomic(path)?)
}

fn register_and_save(
    path: &Path,
    registration: TrustRegistration,
) -> Result<SavedRegistration, InitError> {
    let parent = path
        .parent()
        .ok_or_else(|| InitError::TrustPathHasNoParent {
            path: path.to_path_buf(),
        })?;
    create_private_directory(parent)?;
    let _lock = WriterLock::acquire(path, TRUST_LOCK_TIMEOUT)?;
    let mut registry = load_registry(path)?;

    if registry.bindings().iter().any(|binding| {
        binding.index_name() == &registration.index_name
            && binding.index_id() != registration.index_id
    }) {
        return Err(InitError::LogicalIndexConflict {
            index_name: registration.index_name,
        });
    }
    let outcome = registry.register(registration)?;
    let durability = if matches!(outcome, RegistrationOutcome::Created(_)) {
        Some(registry.save_atomic(path)?)
    } else {
        None
    };
    Ok(SavedRegistration {
        outcome,
        durability,
    })
}

struct SavedRegistration {
    outcome: RegistrationOutcome,
    durability: Option<AtomicSaveOutcome>,
}

fn outcome_binding(outcome: RegistrationOutcome) -> TrustBinding {
    match outcome {
        RegistrationOutcome::Created(binding) | RegistrationOutcome::Existing(binding) => binding,
    }
}

// The managed index directory holds the indexed source bodies themselves, so
// `README.md` requires its parent to be user-only (`0700`) and the database and
// its WAL/SHM sidecars to be user-only (`0600`). On a target with no user-only
// permission primitive these two functions used to fall through to `Ok(())`,
// creating the directory and reporting success while leaving whatever
// permissions it inherited. Refuse instead, for the same reason the trust
// registry refuses: a guarantee that silently does nothing is worse than one
// that is absent, because callers and documentation both still believe it.
fn create_private_directory(path: &Path) -> Result<(), InitError> {
    #[cfg(not(unix))]
    {
        Err(InitError::PrivatePermissionsUnsupported {
            path: path.to_path_buf(),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}

fn set_private_file(path: &Path) -> Result<(), InitError> {
    #[cfg(not(unix))]
    {
        Err(InitError::PrivatePermissionsUnsupported {
            path: path.to_path_buf(),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

struct PointerPlan {
    path: PathBuf,
    contents: Option<Vec<u8>>,
    prior: Option<Vec<u8>>,
    force: bool,
}

impl PointerPlan {
    fn preflight(
        root: &Path,
        write: bool,
        force: bool,
        index_name: &SafeSlug,
        project_name: &SafeSlug,
    ) -> Result<Self, InitError> {
        let path = root.join(crate::config::POINTER_FILE_NAME);
        if !write {
            return Ok(Self {
                path,
                contents: None,
                prior: None,
                force: false,
            });
        }

        let prior = match RepositoryPointer::read_bytes(root)? {
            Some(bytes) => {
                if !force {
                    return Err(InitError::PointerExists { path });
                }
                Some(bytes)
            }
            None => None,
        };
        let contents =
            RepositoryPointer::new(index_name.clone(), project_name.clone()).to_toml()?;
        Ok(Self {
            path,
            contents: Some(contents.into_bytes()),
            prior,
            force,
        })
    }

    const fn dry_run_outcome(&self) -> PointerOutcome {
        if self.contents.is_some() {
            PointerOutcome::WouldWrite
        } else {
            PointerOutcome::NotRequested
        }
    }

    fn apply(&self) -> Result<PointerOutcome, InitError> {
        if let Some(contents) = &self.contents {
            Ok(pointer_outcome(write_atomic_pointer(
                &self.path, contents, self.force,
            )?))
        } else {
            Ok(PointerOutcome::NotRequested)
        }
    }

    fn apply_reversibly(&self) -> Result<(PointerRollback, PointerOutcome), InitError> {
        if let Some(contents) = &self.contents {
            let durability = write_atomic_pointer(&self.path, contents, self.force)?;
            Ok((
                PointerRollback {
                    path: self.path.clone(),
                    prior: self.prior.clone(),
                    written: Some(contents.clone()),
                    armed: true,
                },
                pointer_outcome(durability),
            ))
        } else {
            Ok((
                PointerRollback {
                    path: self.path.clone(),
                    prior: None,
                    written: None,
                    armed: false,
                },
                PointerOutcome::NotRequested,
            ))
        }
    }
}

fn pointer_outcome(durability: AtomicSaveOutcome) -> PointerOutcome {
    match durability {
        AtomicSaveOutcome::Committed => PointerOutcome::Written,
        AtomicSaveOutcome::DurabilityUnknown => PointerOutcome::WrittenDurabilityUnknown,
    }
}

fn write_atomic_pointer(
    path: &Path,
    contents: &[u8],
    replace: bool,
) -> Result<AtomicSaveOutcome, InitError> {
    let parent = path
        .parent()
        .ok_or_else(|| InitError::PointerPathHasNoParent {
            path: path.to_path_buf(),
        })?;
    let temporary = parent.join(format!(".hsum.toml.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);

        let mut durability_unknown = false;
        if replace {
            if let Err(error) = fs::rename(&temporary, path) {
                let visible = path
                    .parent()
                    .and_then(|root| RepositoryPointer::read_bytes(root).ok().flatten())
                    .is_some_and(|stored| stored == contents);
                if !visible {
                    return Err(error.into());
                }
                durability_unknown = true;
            }
        } else {
            match rename_without_replacement(&temporary, path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(InitError::PointerExists {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }
        if File::open(parent)
            .and_then(|directory| directory.sync_all())
            .is_err()
        {
            durability_unknown = true;
        }
        Ok(if durability_unknown {
            AtomicSaveOutcome::DurabilityUnknown
        } else {
            AtomicSaveOutcome::Committed
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_without_replacement(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};

    let parent = source
        .parent()
        .filter(|parent| destination.parent() == Some(*parent))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename parents differ"))?;
    let source_name = source
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source name is absent"))?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination name is absent"))?;
    let directory = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    renameat_with(
        &directory,
        source_name,
        &directory,
        destination_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_without_replacement(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

struct PointerRollback {
    path: PathBuf,
    prior: Option<Vec<u8>>,
    written: Option<Vec<u8>>,
    armed: bool,
}

impl PointerRollback {
    fn rollback(mut self) -> Result<(), InitError> {
        if self.armed {
            let parent = self
                .path
                .parent()
                .ok_or_else(|| InitError::PointerPathHasNoParent {
                    path: self.path.clone(),
                })?;
            let current = RepositoryPointer::read_bytes(parent)?;
            if current.as_deref() != self.written.as_deref() {
                return Err(InitError::PointerRollbackConflict {
                    path: self.path.clone(),
                });
            }
            match &self.prior {
                Some(contents) => {
                    let _ = write_atomic_pointer(&self.path, contents, true)?;
                }
                None => {
                    match fs::remove_file(&self.path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    if let Some(parent) = self.path.parent() {
                        let _ = File::open(parent).and_then(|directory| directory.sync_all());
                    }
                }
            }
            self.armed = false;
        }
        Ok(())
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

struct NewIndexRollback {
    path: PathBuf,
    armed: bool,
}

impl NewIndexRollback {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn commit(&mut self) {
        self.armed = false;
    }
}

impl Drop for NewIndexRollback {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = fs::remove_file(&self.path);
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = self.path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let _ = fs::remove_file(PathBuf::from(sidecar));
        }
        let _ = fs::remove_file(WriterLock::sidecar_path(&self.path));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BroadRootReason {
    FilesystemRoot,
    HomeDirectory,
    AboveGitWorktree { git_root: PathBuf },
}

#[derive(Debug, Error)]
pub enum InitError {
    #[error("repository root is too broad without --allow-broad-root: {root}")]
    BroadRootConfirmationRequired {
        root: PathBuf,
        reason: BroadRootReason,
    },
    #[error(
        "source exceeds the default init budget ({files} files, {bytes} bytes; limits are {max_files} files and {max_bytes} bytes); use --allow-large-source"
    )]
    LargeSourceConfirmationRequired {
        files: usize,
        bytes: u64,
        max_files: usize,
        max_bytes: u64,
    },
    #[error("the operating-system account home could not be resolved safely")]
    AccountHomeUnavailable(#[source] io::Error),
    #[error(
        "this platform has no user-only permission primitive, so {path} cannot be made private; \
         hSUM refuses to create managed storage it cannot protect"
    )]
    PrivatePermissionsUnsupported { path: PathBuf },
    #[error("--force-pointer requires --write-pointer")]
    ForcePointerWithoutWrite,
    #[error("--rebuild cannot be combined with --no-ingest")]
    RebuildWithoutIngest,
    #[error("repository root has no existing trust binding to rebuild: {root}")]
    RebuildBindingRequired { root: PathBuf },
    #[error("trust binding changed while rebuild was preparing: {binding_id}")]
    RebuildBindingChanged { binding_id: BindingId },
    #[error("repository pointer already exists: {path}; use --force-pointer to replace it")]
    PointerExists { path: PathBuf },
    #[error("repository pointer path is not a regular non-symlink file: {path}")]
    UnsafePointerPath { path: PathBuf },
    #[error("managed index path is already occupied: {path}")]
    IndexPathOccupied { path: PathBuf },
    #[error("trusted index database is missing: {path}")]
    TrustedIndexMissing { path: PathBuf },
    #[error("trusted index identity mismatch: expected {expected}, found {actual}")]
    TrustedIndexIdentityMismatch { expected: IndexId, actual: IndexId },
    #[error(
        "trusted project identity mismatch: expected project {expected_id} named {expected_name}"
    )]
    TrustedProjectIdentityMismatch {
        expected_id: ProjectId,
        expected_name: SafeSlug,
    },
    #[error("alpha.4 requires exactly one source in the trusted project; found {found}")]
    AlphaSourceCardinality { found: usize },
    #[error("trusted project is not linked to its expected filesystem source {expected}")]
    TrustedSourceIdentityMismatch { expected: SourceId },
    #[error("trusted filesystem source root does not match requested repository root: {expected}")]
    TrustedSourceRootMismatch { expected: PathBuf },
    #[error("trusted source cardinality is not representable")]
    InvalidTrustedCardinality,
    #[error(
        "repository root {root} is already bound to index {index_name} and project {project_name}"
    )]
    ExistingBindingConflict {
        root: PathBuf,
        index_name: SafeSlug,
        project_name: SafeSlug,
    },
    #[error("repository root {root} has {matches} ambiguous trust bindings")]
    AmbiguousRootBindings { root: PathBuf, matches: usize },
    #[error("logical index name is already bound to a different index: {index_name}")]
    LogicalIndexConflict { index_name: SafeSlug },
    #[error("could not generate a free managed index name from {base}")]
    IndexNameSpaceExhausted { base: String },
    #[error("trust requires an explicit confirmation")]
    TrustConfirmationRequired,
    #[error("repository root cannot be represented as UTF-8: {root:?}")]
    NonUtf8Root { root: PathBuf },
    #[error("source estimate overflowed")]
    SourceEstimateOverflow,
    #[error("managed database path has no parent: {path}")]
    DatabasePathHasNoParent { path: PathBuf },
    #[error("trust registry path has no parent: {path}")]
    TrustPathHasNoParent { path: PathBuf },
    #[error("repository pointer path has no parent: {path}")]
    PointerPathHasNoParent { path: PathBuf },
    #[error("repository pointer changed concurrently and could not be rolled back safely: {path}")]
    PointerRollbackConflict { path: PathBuf },
    #[error(transparent)]
    FilesystemIngest(#[from] FilesystemIngestError),
    #[error(transparent)]
    StoragePreflight(#[from] StoragePreflightError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Pointer(#[from] PointerError),
    #[error(transparent)]
    Slug(#[from] SlugError),
    #[error(transparent)]
    SourceConfig(#[from] super::SourceConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(all(test, unix))]
mod tests {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::tempdir;

    use super::{InitRequest, RebuildCheckpoint, initialize, initialize_with_rebuild_observer};
    use crate::config::{ManagedPaths, TrustRegistry};
    use crate::status::Status;
    use crate::store::Doctor;

    const FAULT_ENV: &str = "HSUM_TEST_REBUILD_CHECKPOINT";
    const ROOT_ENV: &str = "HSUM_TEST_REBUILD_ROOT";
    const MANAGED_HOME_ENV: &str = "HSUM_TEST_REBUILD_MANAGED_HOME";
    const CRASH_EXIT: i32 = 86;

    #[test]
    fn interrupted_rebuilds_leave_plain_init_recoverable() {
        for checkpoint in [
            RebuildCheckpoint::TrustBindingRemoved,
            RebuildCheckpoint::DatabaseRemoved,
            RebuildCheckpoint::ReplacementDatabaseCreated,
            RebuildCheckpoint::ReplacementBindingRegistered,
        ] {
            let root = tempdir().unwrap();
            let managed_home = tempdir().unwrap();
            fs::write(
                root.path().join("README.md"),
                b"# Rebuild crash fixture\nCrashRecoveryIdentifier\n",
            )
            .unwrap();
            let initial = initialize(&request_for_paths(root.path(), managed_home.path())).unwrap();
            assert!(Doctor::run(&initial.database_path).is_ok());

            let status = Command::new(env::current_exe().unwrap())
                .arg("--exact")
                .arg("app::init::tests::rebuild_fault_helper")
                .arg("--nocapture")
                .env(FAULT_ENV, checkpoint.as_str())
                .env(ROOT_ENV, root.path())
                .env(MANAGED_HOME_ENV, managed_home.path())
                .status()
                .unwrap();
            assert_eq!(
                status.code(),
                Some(CRASH_EXIT),
                "checkpoint {checkpoint:?} did not terminate at the fault boundary"
            );

            let recovered =
                initialize(&request_for_paths(root.path(), managed_home.path())).unwrap();
            assert!(Doctor::run(&recovered.database_path).is_ok());
            assert_eq!(
                Status::read(&recovered.database_path)
                    .unwrap()
                    .active_documents,
                1
            );
            let registry =
                TrustRegistry::load(&managed_home.path().join("config/trusted-projects.toml"))
                    .unwrap();
            let canonical_root = fs::canonicalize(root.path()).unwrap();
            assert_eq!(
                registry
                    .bindings()
                    .iter()
                    .filter(|binding| binding.canonical_root() == canonical_root)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn rebuild_fault_helper() {
        let Ok(checkpoint) = env::var(FAULT_ENV) else {
            return;
        };
        let checkpoint = RebuildCheckpoint::parse(&checkpoint).expect("known rebuild checkpoint");
        let root = PathBuf::from(env::var_os(ROOT_ENV).expect("rebuild root"));
        let managed_home = PathBuf::from(env::var_os(MANAGED_HOME_ENV).expect("managed test home"));
        let mut request = request_for_paths(&root, &managed_home);
        request.rebuild = true;

        let result = initialize_with_rebuild_observer(&request, |observed| {
            if observed == checkpoint {
                std::process::exit(CRASH_EXIT);
            }
        });
        panic!("rebuild returned before checkpoint {checkpoint:?}: {result:?}");
    }

    fn request_for_paths(root: &Path, managed_home: &Path) -> InitRequest {
        let managed_paths = ManagedPaths::resolve(Some(managed_home)).unwrap();
        let mut request = InitRequest::new(root.to_path_buf(), managed_paths);
        request.requested_root = Some(root.to_path_buf());
        request.home_dir = Some(managed_home.join("not-the-source-home"));
        request
    }

    impl RebuildCheckpoint {
        const fn as_str(self) -> &'static str {
            match self {
                Self::TrustBindingRemoved => "trust-binding-removed",
                Self::DatabaseRemoved => "database-removed",
                Self::ReplacementDatabaseCreated => "replacement-database-created",
                Self::ReplacementBindingRegistered => "replacement-binding-registered",
            }
        }

        fn parse(value: &str) -> Option<Self> {
            match value {
                "trust-binding-removed" => Some(Self::TrustBindingRemoved),
                "database-removed" => Some(Self::DatabaseRemoved),
                "replacement-database-created" => Some(Self::ReplacementDatabaseCreated),
                "replacement-binding-registered" => Some(Self::ReplacementBindingRegistered),
                _ => None,
            }
        }
    }
}
