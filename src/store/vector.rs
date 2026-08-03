use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use serde_json_canonicalizer::to_string as to_canonical_json;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::domain::{SafeSlug, Sha256Digest, SlugError, SourceId};
use crate::model::ModelFile;
use crate::store::doctor::Doctor;
use crate::store::lock::WriterLock;
use crate::store::open::{IndexDb, StoreError};
use crate::store::schema::pipeline_fingerprint_for;

pub const EMBEDDING_DIMENSION: u32 = 384;
pub const SQLITE_VEC_VERSION: &str = "0.1.7";
pub const EMBEDDING_PROVENANCE_SCHEMA: &str = "hsum.embedding-provenance.v1";
const NORMALIZED_NORM_TOLERANCE: f32 = 0.001;
const MAX_PROVENANCE_BYTES: usize = 64 * 1024;
pub const MAX_EMBEDDING_INPUT_BATCH: u8 = 8;
const DEFAULT_REEMBED_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const PROVENANCE_FIELDS: [&str; 25] = [
    "component_type",
    "execution_provider",
    "execution_provider_configuration",
    "fastembed_version",
    "files",
    "graph_optimization_level",
    "intra_threads",
    "manifest_sha256",
    "max_length",
    "model_id",
    "normalization",
    "normalization_implementation",
    "onnx_runtime_build_info",
    "onnx_runtime_version",
    "ort_crate_version",
    "output_selection",
    "pooling",
    "quantization",
    "schema_version",
    "target_arch",
    "target_endianness",
    "target_os",
    "upstream_repository",
    "upstream_revision",
    "vector_dimension",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingModelPin {
    model_id: SafeSlug,
    upstream_revision: String,
    model_fingerprint: Sha256Digest,
    dimension: u32,
}

impl EmbeddingModelPin {
    pub fn new(
        model_id: impl Into<String>,
        upstream_revision: impl Into<String>,
        model_fingerprint: Sha256Digest,
        dimension: u32,
    ) -> Result<Self, EmbeddingProfileError> {
        let model_id = SafeSlug::new(model_id).map_err(EmbeddingProfileError::ModelId)?;
        let upstream_revision = upstream_revision.into();
        if upstream_revision.len() != 40
            || !upstream_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(EmbeddingProfileError::UpstreamRevision);
        }
        if dimension != EMBEDDING_DIMENSION {
            return Err(EmbeddingProfileError::Dimension {
                expected: EMBEDDING_DIMENSION,
                actual: dimension,
            });
        }
        Ok(Self {
            model_id,
            upstream_revision,
            model_fingerprint,
            dimension,
        })
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        self.model_id.as_str()
    }

    #[must_use]
    pub fn upstream_revision(&self) -> &str {
        &self.upstream_revision
    }

    #[must_use]
    pub const fn model_fingerprint(&self) -> Sha256Digest {
        self.model_fingerprint
    }

    #[must_use]
    pub const fn dimension(&self) -> u32 {
        self.dimension
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum IndexEmbeddingProfile {
    #[default]
    LexicalOnly,
    Pinned(EmbeddingModelPin),
}

impl IndexEmbeddingProfile {
    pub(crate) fn descriptor(&self) -> String {
        match self {
            Self::LexicalOnly => "embedding=none\n".to_owned(),
            Self::Pinned(pin) => format!(
                concat!(
                    "embedding=pinned\n",
                    "embedding_model_id={}\n",
                    "embedding_upstream_revision={}\n",
                    "embedding_model_fingerprint={}\n",
                    "embedding_dimension={}\n",
                    "embedding_input=nfc-lf:title+source-uri+chunk-text\n",
                    "embedding_pooling=cls\n",
                    "embedding_normalization=l2-after-pooling\n",
                    "embedding_component=ieee754-binary32\n",
                    "embedding_quantization=none\n",
                ),
                pin.model_id(),
                pin.upstream_revision(),
                pin.model_fingerprint(),
                pin.dimension(),
            ),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EmbeddingProfileError {
    #[error("invalid embedding model ID: {0}")]
    ModelId(SlugError),
    #[error("embedding upstream revision must be a full lowercase 40-byte Git commit")]
    UpstreamRevision,
    #[error("unsupported embedding dimension: expected {expected}, found {actual}")]
    Dimension { expected: u32, actual: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingProvenanceRecord {
    canonical_json: String,
    fingerprint: Sha256Digest,
    compatibility_fingerprint: Sha256Digest,
    model_id: String,
    upstream_revision: String,
    manifest_fingerprint: Sha256Digest,
    dimension: u32,
}

impl EmbeddingProvenanceRecord {
    pub fn from_json(json: &str) -> Result<Self, EmbeddingProvenanceError> {
        if json.len() > MAX_PROVENANCE_BYTES {
            return Err(EmbeddingProvenanceError::TooLarge);
        }
        let value: Value = serde_json::from_str(json)
            .map_err(|error| EmbeddingProvenanceError::Json(error.to_string()))?;
        let object = value.as_object().ok_or(EmbeddingProvenanceError::Object)?;
        let fields = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if fields != PROVENANCE_FIELDS.into_iter().collect() {
            return Err(EmbeddingProvenanceError::Fields);
        }
        require_string(object, "schema_version", Some(EMBEDDING_PROVENANCE_SCHEMA))?;
        let model_id = require_string(object, "model_id", None)?.to_owned();
        SafeSlug::new(model_id.clone()).map_err(EmbeddingProvenanceError::ModelId)?;
        let upstream_repository = require_string(object, "upstream_repository", None)?;
        if upstream_repository.is_empty()
            || upstream_repository.len() > 256
            || !upstream_repository.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            return Err(EmbeddingProvenanceError::Field("upstream_repository"));
        }
        let upstream_revision = require_string(object, "upstream_revision", None)?.to_owned();
        if upstream_revision.len() != 40
            || !upstream_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(EmbeddingProvenanceError::Revision);
        }
        let manifest_fingerprint = require_string(object, "manifest_sha256", None)?
            .parse()
            .map_err(|_| EmbeddingProvenanceError::ManifestFingerprint)?;
        let files = serde_json::from_value::<Vec<ModelFile>>(
            object
                .get("files")
                .cloned()
                .ok_or(EmbeddingProvenanceError::Files)?,
        )
        .map_err(|_| EmbeddingProvenanceError::Files)?;
        if files.is_empty() || files.len() > 64 {
            return Err(EmbeddingProvenanceError::Files);
        }
        let mut file_paths = BTreeSet::new();
        for file in &files {
            file.validate()
                .map_err(|_| EmbeddingProvenanceError::Files)?;
            if !file_paths.insert(&file.path) {
                return Err(EmbeddingProvenanceError::Files);
            }
        }
        let dimension = require_u32(object, "vector_dimension")?;
        if dimension != EMBEDDING_DIMENSION {
            return Err(EmbeddingProvenanceError::Dimension(dimension));
        }
        for (field, expected) in [
            ("pooling", "cls"),
            ("normalization", "l2_after_pooling"),
            (
                "normalization_implementation",
                "fastembed::common::normalize",
            ),
            ("component_type", "ieee754_binary32"),
            ("quantization", "none"),
            ("output_selection", "fastembed_default_precedence"),
            ("fastembed_version", "5.17.4"),
            ("ort_crate_version", "2.0.0-rc.13"),
            ("onnx_runtime_version", "1.28.0"),
            ("execution_provider", "CPUExecutionProvider"),
            ("execution_provider_configuration", "default_cpu_fallback"),
            ("graph_optimization_level", "level3"),
            ("target_endianness", "little"),
        ] {
            require_string(object, field, Some(expected))?;
        }
        require_nonempty_string(object, "onnx_runtime_build_info", 4096)?;
        match (
            require_string(object, "target_os", None)?,
            require_string(object, "target_arch", None)?,
        ) {
            ("linux", "x86_64") | ("macos", "aarch64") => {}
            _ => return Err(EmbeddingProvenanceError::Target),
        }
        if require_u64(object, "max_length")? != 512 || require_u64(object, "intra_threads")? != 2 {
            return Err(EmbeddingProvenanceError::Configuration);
        }

        let canonical_json = to_canonical_json(&value)
            .map_err(|error| EmbeddingProvenanceError::Json(error.to_string()))?;
        let fingerprint = Sha256Digest::of_bytes(canonical_json.as_bytes());
        let mut compatible = value;
        let compatible_object = compatible
            .as_object_mut()
            .expect("validated provenance remains an object");
        compatible_object.remove("target_os");
        compatible_object.remove("target_arch");
        let compatible_json = to_canonical_json(&compatible)
            .map_err(|error| EmbeddingProvenanceError::Json(error.to_string()))?;
        let compatibility_fingerprint = Sha256Digest::of_bytes(compatible_json.as_bytes());
        Ok(Self {
            canonical_json,
            fingerprint,
            compatibility_fingerprint,
            model_id,
            upstream_revision,
            manifest_fingerprint,
            dimension,
        })
    }

    #[must_use]
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    #[must_use]
    pub const fn fingerprint(&self) -> Sha256Digest {
        self.fingerprint
    }

    #[must_use]
    pub const fn compatibility_fingerprint(&self) -> Sha256Digest {
        self.compatibility_fingerprint
    }

    pub(crate) fn model_id(&self) -> &str {
        &self.model_id
    }

    pub(crate) fn upstream_revision(&self) -> &str {
        &self.upstream_revision
    }

    pub(crate) const fn manifest_fingerprint(&self) -> Sha256Digest {
        self.manifest_fingerprint
    }

    pub(crate) const fn dimension(&self) -> u32 {
        self.dimension
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EmbeddingProvenanceError {
    #[error("embedding provenance exceeds 64 KiB")]
    TooLarge,
    #[error("embedding provenance is invalid JSON: {0}")]
    Json(String),
    #[error("embedding provenance must be an object")]
    Object,
    #[error("embedding provenance has a missing or unknown field")]
    Fields,
    #[error("embedding provenance field {0} is invalid")]
    Field(&'static str),
    #[error("embedding provenance model ID is invalid: {0}")]
    ModelId(SlugError),
    #[error("embedding provenance upstream revision is invalid")]
    Revision,
    #[error("embedding provenance manifest fingerprint is invalid")]
    ManifestFingerprint,
    #[error("embedding provenance file inventory is invalid")]
    Files,
    #[error("embedding provenance dimension {0} is unsupported")]
    Dimension(u32),
    #[error("embedding provenance target is unsupported")]
    Target,
    #[error("embedding provenance worker configuration is invalid")]
    Configuration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingInput {
    passage_id: i64,
    chunk_id: i64,
    source_id: SourceId,
    text: String,
    sha256: Sha256Digest,
}

impl EmbeddingInput {
    #[must_use]
    pub const fn passage_id(&self) -> i64 {
        self.passage_id
    }

    #[must_use]
    pub const fn chunk_id(&self) -> i64 {
        self.chunk_id
    }

    #[must_use]
    pub const fn source_id(&self) -> SourceId {
        self.source_id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn sha256(&self) -> Sha256Digest {
        self.sha256
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReembedOutcome {
    pub generation_id: i64,
    pub index_epoch: u64,
    pub passages: u64,
    pub active_vector_slot: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReembedPlan {
    pub passages: u64,
    pub cached_inputs: u64,
    pub missing_inputs: u64,
    pub estimated_write_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct PreparedChunkEmbedding {
    chunk_id: i64,
    input_sha256: Sha256Digest,
    vector: Vec<f32>,
    provenance: EmbeddingProvenanceRecord,
}

impl PreparedChunkEmbedding {
    pub fn from_input(
        input: &EmbeddingInput,
        vector: Vec<f32>,
        provenance: EmbeddingProvenanceRecord,
    ) -> Result<Self, StoreError> {
        Self::new(input.chunk_id, input.sha256, vector, provenance)
    }

    pub fn new(
        chunk_id: i64,
        input_sha256: Sha256Digest,
        vector: Vec<f32>,
        provenance: EmbeddingProvenanceRecord,
    ) -> Result<Self, StoreError> {
        if chunk_id <= 0 {
            return Err(StoreError::InvalidEmbedding("chunk identifier"));
        }
        validate_vector(&vector)?;
        Ok(Self {
            chunk_id,
            input_sha256,
            vector,
            provenance,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingCacheOutcome {
    Inserted { embedding_id: i64 },
    Reused { embedding_id: i64 },
}

impl IndexDb {
    /// Returns whether the active generation has exact, complete membership in
    /// the selected sqlite-vec slot for this index's immutable model pin.
    pub fn has_complete_vector_membership(&self) -> Result<bool, StoreError> {
        validate_active_vector_membership(self.connection())
    }

    pub fn has_completed_vector_generation(&self) -> Result<bool, StoreError> {
        Ok(self.connection().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM generations
                 WHERE state = 'committed' AND vector_state = 'complete'
             )",
            [],
            |row| row.get(0),
        )?)
    }

    /// Loads the next bounded batch of deterministic active embedding inputs.
    /// Callers advance with the last returned passage ID; the hard batch cap
    /// matches the canonical two-worker/eight-item inference queue.
    pub fn load_embedding_input_batch(
        &self,
        after_passage_id: i64,
        limit: u8,
    ) -> Result<Vec<EmbeddingInput>, StoreError> {
        if after_passage_id < 0 || limit == 0 || limit > MAX_EMBEDDING_INPUT_BATCH {
            return Err(StoreError::InvalidEmbedding("embedding input batch"));
        }
        if self.embedding_profile()? == IndexEmbeddingProfile::LexicalOnly {
            return Err(StoreError::InvalidEmbedding("lexical-only index"));
        }
        let mut statement = self.connection().prepare(
            "SELECT ap.id, ap.chunk_id, ap.source_id,
                    COALESCE(dv.title, ''), dv.source_uri, c.body_text
             FROM active_passages AS ap
             JOIN document_versions AS dv ON dv.id = ap.document_version_id
             JOIN chunks AS c ON c.id = ap.chunk_id
             WHERE ap.id > ?1
             ORDER BY ap.id
             LIMIT ?2",
        )?;
        let mut rows = statement.query(params![after_passage_id, i64::from(limit)])?;
        let mut inputs = Vec::with_capacity(usize::from(limit));
        while let Some(row) = rows.next()? {
            let passage_id = row.get::<_, i64>(0)?;
            let chunk_id = row.get::<_, i64>(1)?;
            let source_id = source_id_from_blob(&row.get::<_, Vec<u8>>(2)?)?;
            let title = row.get::<_, String>(3)?;
            let source_uri = row.get::<_, String>(4)?;
            let chunk_text = row.get::<_, String>(5)?;
            let text = prepare_embedding_input(&title, &source_uri, &chunk_text);
            let sha256 = Sha256Digest::of_bytes(text.as_bytes());
            inputs.push(EmbeddingInput {
                passage_id,
                chunk_id,
                source_id,
                text,
                sha256,
            });
        }
        Ok(inputs)
    }

    pub fn plan_reembedding(&self) -> Result<ReembedPlan, StoreError> {
        let profile = self.embedding_profile()?;
        let IndexEmbeddingProfile::Pinned(pin) = profile else {
            return Err(StoreError::InvalidEmbedding("lexical-only index"));
        };
        let mut after_passage_id = 0_i64;
        let mut passages = 0_u64;
        let mut cached_inputs = 0_u64;
        loop {
            let inputs =
                self.load_embedding_input_batch(after_passage_id, MAX_EMBEDDING_INPUT_BATCH)?;
            if inputs.is_empty() {
                break;
            }
            for input in &inputs {
                passages = passages.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
                if embedding_is_cached(self.connection(), &pin, input)? {
                    cached_inputs = cached_inputs
                        .checked_add(1)
                        .ok_or(StoreError::IntegerOverflow)?;
                }
            }
            after_passage_id = inputs
                .last()
                .expect("a nonempty input batch has a last item")
                .passage_id();
        }
        let missing_inputs = passages
            .checked_sub(cached_inputs)
            .ok_or(StoreError::IntegerOverflow)?;
        let cache_bytes = missing_inputs
            .checked_mul(u64::from(EMBEDDING_DIMENSION) * 4 + 8 * 1024)
            .ok_or(StoreError::IntegerOverflow)?;
        let shadow_bytes = passages
            .checked_mul(u64::from(EMBEDDING_DIMENSION) * 4 + 4 * 1024)
            .ok_or(StoreError::IntegerOverflow)?;
        let estimated_write_bytes = cache_bytes
            .checked_add(shadow_bytes)
            .and_then(|value| value.checked_add(64 * 1024))
            .ok_or(StoreError::IntegerOverflow)?;
        Ok(ReembedPlan {
            passages,
            cached_inputs,
            missing_inputs,
            estimated_write_bytes,
        })
    }

    /// Returns whether this exact canonical input already has a vector for
    /// the index's immutable model fingerprint. A true result lets inference
    /// callers skip unchanged content rather than merely deduplicating after
    /// an expensive model invocation.
    pub fn has_cached_embedding(&self, input: &EmbeddingInput) -> Result<bool, StoreError> {
        let profile = self.embedding_profile()?;
        let IndexEmbeddingProfile::Pinned(pin) = profile else {
            return Err(StoreError::InvalidEmbedding("lexical-only index"));
        };
        embedding_is_cached(self.connection(), &pin, input)
    }

    pub fn commit_cached_reembedding(&mut self) -> Result<ReembedOutcome, StoreError> {
        self.commit_cached_reembedding_with_timeout(DEFAULT_REEMBED_LOCK_TIMEOUT)
    }

    pub fn commit_cached_reembedding_with_timeout(
        &mut self,
        lock_timeout: Duration,
    ) -> Result<ReembedOutcome, StoreError> {
        let path = self.path().to_path_buf();
        let writer_lock = WriterLock::acquire(&path, lock_timeout)?;
        self.commit_cached_reembedding_with_lock(&writer_lock)
    }

    pub(crate) fn commit_cached_reembedding_with_lock(
        &mut self,
        writer_lock: &WriterLock,
    ) -> Result<ReembedOutcome, StoreError> {
        let path = self.path().to_path_buf();
        if !writer_lock.protects(&path) {
            return Err(StoreError::WriterLockMismatch);
        }
        Doctor::run(&path)?;
        let profile = self.embedding_profile()?;
        let IndexEmbeddingProfile::Pinned(pin) = profile else {
            return Err(StoreError::InvalidEmbedding("lexical-only index"));
        };
        let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_slot = metadata_value(&transaction, "active_vector_slot")?;
        let (prior_table, next_table, next_slot) = match raw_slot.as_slice() {
            b"0" => ("passages_vec_a", "passages_vec_b", 1_u8),
            b"1" => ("passages_vec_b", "passages_vec_a", 0_u8),
            _ => return Err(StoreError::InvalidMetadata("active_vector_slot")),
        };
        transaction.execute(&format!("DELETE FROM {next_table}"), [])?;

        let mut passage_statement = transaction.prepare(
            "SELECT ap.id, ap.source_id, COALESCE(dv.title, ''),
                    dv.source_uri, c.body_text
             FROM active_passages AS ap
             JOIN document_versions AS dv ON dv.id = ap.document_version_id
             JOIN chunks AS c ON c.id = ap.chunk_id
             ORDER BY ap.id",
        )?;
        let mut passage_rows = passage_statement.query([])?;
        let mut passages = 0_u64;
        while let Some(row) = passage_rows.next()? {
            let passage_id = row.get::<_, i64>(0)?;
            let source_id = source_id_from_blob(&row.get::<_, Vec<u8>>(1)?)?;
            let title = row.get::<_, String>(2)?;
            let source_uri = row.get::<_, String>(3)?;
            let chunk_text = row.get::<_, String>(4)?;
            let input = prepare_embedding_input(&title, &source_uri, &chunk_text);
            let input_sha256 = Sha256Digest::of_bytes(input.as_bytes());
            let vector_blob = transaction
                .query_row(
                    "SELECT vector_blob
                     FROM chunk_embeddings
                     WHERE model_fingerprint = ?1 AND input_sha256 = ?2",
                    params![
                        pin.model_fingerprint().as_bytes().as_slice(),
                        input_sha256.as_bytes().as_slice(),
                    ],
                    |cache_row| cache_row.get::<_, Vec<u8>>(0),
                )
                .optional()?
                .ok_or(StoreError::InvalidEmbedding("embedding cache incomplete"))?;
            validate_stored_vector_blob(&vector_blob)?;
            transaction.execute(
                &format!(
                    "INSERT INTO {next_table}(rowid, embedding, source_id)
                     VALUES (?1, ?2, ?3)"
                ),
                params![passage_id, vector_blob, source_id.to_string()],
            )?;
            passages = passages.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
        }
        drop(passage_rows);
        drop(passage_statement);

        let stored_rows =
            transaction.query_row(&format!("SELECT COUNT(*) FROM {next_table}"), [], |row| {
                row.get::<_, i64>(0)
            })?;
        if stored_rows != i64::try_from(passages).map_err(|_| StoreError::IntegerOverflow)? {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "shadow vector membership",
            ));
        }
        let lease_nonce = *Uuid::new_v4().as_bytes();
        let pipeline_fingerprint =
            pipeline_fingerprint_for(&IndexEmbeddingProfile::Pinned(pin.clone()));
        transaction.execute(
            "INSERT INTO generations(
                state, created_at, owner_pid, lease_nonce, heartbeat_at,
                pipeline_fingerprint, embedding_model_id, embedding_revision,
                embedding_model_fingerprint, embedding_dimension, vector_state
             ) VALUES (
                'building', ?1, ?2, ?3, ?1, ?4, ?5, ?6, ?7, ?8, 'complete'
             )",
            params![
                now,
                i64::from(std::process::id()),
                lease_nonce.as_slice(),
                pipeline_fingerprint.as_bytes().as_slice(),
                pin.model_id(),
                pin.upstream_revision(),
                pin.model_fingerprint().as_bytes().as_slice(),
                i64::from(pin.dimension()),
            ],
        )?;
        let generation_id = transaction.last_insert_rowid();
        let changed = transaction.execute(
            "UPDATE generations
             SET state = 'committed', committed_at = ?1
             WHERE id = ?2 AND state = 'building'",
            params![now, generation_id],
        )?;
        if changed != 1 {
            return Err(StoreError::GenerationInvariant(
                "reembedding generation commit",
            ));
        }
        transaction.execute(&format!("DELETE FROM {prior_table}"), [])?;
        let index_epoch = metadata_u64(&transaction, "index_epoch")?
            .checked_add(1)
            .ok_or(StoreError::IntegerOverflow)?;
        set_metadata(
            &transaction,
            "index_epoch",
            index_epoch.to_string().as_bytes(),
        )?;
        set_metadata(
            &transaction,
            "active_generation",
            generation_id.to_string().as_bytes(),
        )?;
        set_metadata(
            &transaction,
            "active_vector_slot",
            next_slot.to_string().as_bytes(),
        )?;
        transaction.commit()?;
        Doctor::run(&path)?;
        Ok(ReembedOutcome {
            generation_id,
            index_epoch,
            passages,
            active_vector_slot: next_slot,
        })
    }

    pub fn cache_chunk_embedding(
        &mut self,
        embedding: &PreparedChunkEmbedding,
    ) -> Result<EmbeddingCacheOutcome, StoreError> {
        self.cache_chunk_embedding_with_timeout(embedding, DEFAULT_REEMBED_LOCK_TIMEOUT)
    }

    pub fn cache_chunk_embedding_with_timeout(
        &mut self,
        embedding: &PreparedChunkEmbedding,
        lock_timeout: Duration,
    ) -> Result<EmbeddingCacheOutcome, StoreError> {
        let path = self.path().to_path_buf();
        let writer_lock = WriterLock::acquire(&path, lock_timeout)?;
        self.cache_chunk_embedding_with_lock(&writer_lock, embedding)
    }

    pub(crate) fn cache_chunk_embedding_with_lock(
        &mut self,
        writer_lock: &WriterLock,
        embedding: &PreparedChunkEmbedding,
    ) -> Result<EmbeddingCacheOutcome, StoreError> {
        if !writer_lock.protects(self.path()) {
            return Err(StoreError::WriterLockMismatch);
        }
        let profile = self.embedding_profile()?;
        let IndexEmbeddingProfile::Pinned(pin) = profile else {
            return Err(StoreError::InvalidEmbedding("lexical-only index"));
        };
        if pin.model_id() != embedding.provenance.model_id
            || pin.upstream_revision() != embedding.provenance.upstream_revision
            || pin.model_fingerprint() != embedding.provenance.manifest_fingerprint
            || pin.dimension() != embedding.provenance.dimension
        {
            return Err(StoreError::InvalidEmbedding("embedding model pin"));
        }
        let vector_blob = vector_blob(&embedding.vector);
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO embedding_provenance(
                 fingerprint, compatibility_fingerprint, schema_version, canonical_json
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(fingerprint) DO NOTHING",
            params![
                embedding.provenance.fingerprint().as_bytes().as_slice(),
                embedding
                    .provenance
                    .compatibility_fingerprint()
                    .as_bytes()
                    .as_slice(),
                EMBEDDING_PROVENANCE_SCHEMA,
                embedding.provenance.canonical_json(),
            ],
        )?;
        let stored_provenance: (Vec<u8>, String, String) = transaction.query_row(
            "SELECT compatibility_fingerprint, schema_version, canonical_json
             FROM embedding_provenance WHERE fingerprint = ?1",
            [embedding.provenance.fingerprint().as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if stored_provenance.0.as_slice()
            != embedding.provenance.compatibility_fingerprint().as_bytes()
            || stored_provenance.1 != EMBEDDING_PROVENANCE_SCHEMA
            || stored_provenance.2 != embedding.provenance.canonical_json()
        {
            return Err(StoreError::HashCollision);
        }

        let changed = transaction.execute(
            "INSERT INTO chunk_embeddings(
                 chunk_id, model_fingerprint, input_sha256, vector_dimension,
                 vector_blob, provenance_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(model_fingerprint, input_sha256) DO NOTHING",
            params![
                embedding.chunk_id,
                pin.model_fingerprint().as_bytes().as_slice(),
                embedding.input_sha256.as_bytes().as_slice(),
                i64::from(pin.dimension()),
                vector_blob,
                embedding.provenance.fingerprint().as_bytes().as_slice(),
            ],
        )?;
        let stored: (i64, Vec<u8>, Vec<u8>) = transaction.query_row(
            "SELECT id, vector_blob, provenance_fingerprint
             FROM chunk_embeddings
             WHERE model_fingerprint = ?1 AND input_sha256 = ?2",
            params![
                pin.model_fingerprint().as_bytes().as_slice(),
                embedding.input_sha256.as_bytes().as_slice(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if stored.2.as_slice() == embedding.provenance.fingerprint().as_bytes() {
            if stored.1 != vector_blob {
                return Err(StoreError::HashCollision);
            }
        } else {
            let stored_compatibility = transaction.query_row(
                "SELECT compatibility_fingerprint
                 FROM embedding_provenance WHERE fingerprint = ?1",
                [stored.2.as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )?;
            if stored_compatibility.as_slice()
                != embedding.provenance.compatibility_fingerprint().as_bytes()
            {
                return Err(StoreError::HashCollision);
            }
        }
        transaction.commit()?;
        if changed == 0 {
            Ok(EmbeddingCacheOutcome::Reused {
                embedding_id: stored.0,
            })
        } else {
            Ok(EmbeddingCacheOutcome::Inserted {
                embedding_id: stored.0,
            })
        }
    }
}

fn embedding_is_cached(
    connection: &Connection,
    pin: &EmbeddingModelPin,
    input: &EmbeddingInput,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM chunk_embeddings
             WHERE model_fingerprint = ?1 AND input_sha256 = ?2
         )",
        params![
            pin.model_fingerprint().as_bytes().as_slice(),
            input.sha256().as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?)
}

pub(crate) fn validate_active_vector_membership(
    connection: &Connection,
) -> Result<bool, StoreError> {
    let profile = read_embedding_profile(connection)?;
    if profile == IndexEmbeddingProfile::LexicalOnly {
        return Ok(false);
    }
    let raw_active_generation = metadata_value(connection, "active_generation")?;
    let active_generation = if raw_active_generation.is_empty() {
        0
    } else {
        std::str::from_utf8(&raw_active_generation)
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or(StoreError::InvalidMetadata("active_generation"))?
    };
    let table = match metadata_value(connection, "active_vector_slot")?.as_slice() {
        b"0" => "passages_vec_a",
        b"1" => "passages_vec_b",
        _ => return Err(StoreError::InvalidMetadata("active_vector_slot")),
    };
    let vector_rows =
        connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })?;
    if active_generation == 0 {
        if vector_rows != 0 {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "active vector membership",
            ));
        }
        return Ok(false);
    }
    let vector_state = connection.query_row(
        "SELECT vector_state FROM generations WHERE id = ?1 AND state = 'committed'",
        [active_generation],
        |row| row.get::<_, String>(0),
    )?;
    if vector_state == "absent" {
        if vector_rows != 0 {
            return Err(StoreError::ImmutableEvidenceMismatch(
                "active vector membership",
            ));
        }
        return Ok(false);
    }
    if vector_state != "complete" {
        return Err(StoreError::GenerationInvariant("generation vector state"));
    }
    let active_passages =
        connection.query_row("SELECT COUNT(*) FROM active_passages", [], |row| {
            row.get::<_, i64>(0)
        })?;
    if vector_rows != active_passages {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "active vector membership",
        ));
    }
    let mismatch: bool = connection.query_row(
        &format!(
            "SELECT EXISTS(
                 SELECT 1
                 FROM active_passages AS ap
                 LEFT JOIN {table} AS vec ON vec.rowid = ap.id
                 WHERE vec.rowid IS NULL
                    OR vec.source_id != lower(
                         substr(hex(ap.source_id), 1, 8) || '-' ||
                         substr(hex(ap.source_id), 9, 4) || '-' ||
                         substr(hex(ap.source_id), 13, 4) || '-' ||
                         substr(hex(ap.source_id), 17, 4) || '-' ||
                         substr(hex(ap.source_id), 21, 12)
                    )
             )"
        ),
        [],
        |row| row.get(0),
    )?;
    if mismatch {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "active vector membership",
        ));
    }
    Ok(true)
}

pub(crate) fn clear_active_vector_membership(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let table = match metadata_value(transaction, "active_vector_slot")?.as_slice() {
        b"0" => "passages_vec_a",
        b"1" => "passages_vec_b",
        _ => return Err(StoreError::InvalidMetadata("active_vector_slot")),
    };
    transaction.execute(&format!("DELETE FROM {table}"), [])?;
    Ok(())
}

pub(crate) fn delete_active_vector_membership_for_document(
    transaction: &Transaction<'_>,
    document_id: &[u8],
) -> Result<(), StoreError> {
    let table = match metadata_value(transaction, "active_vector_slot")?.as_slice() {
        b"0" => "passages_vec_a",
        b"1" => "passages_vec_b",
        _ => return Err(StoreError::InvalidMetadata("active_vector_slot")),
    };
    transaction.execute(
        &format!(
            "DELETE FROM {table}
             WHERE rowid IN (
                 SELECT id FROM active_passages WHERE document_id = ?1
             )"
        ),
        [document_id],
    )?;
    Ok(())
}

#[must_use]
pub fn prepare_embedding_input(title: &str, source_uri: &str, chunk_text: &str) -> String {
    let title = normalize_embedding_component(title);
    let source_uri = normalize_embedding_component(source_uri);
    let chunk_text = normalize_embedding_component(chunk_text);
    format!("Title: {title}\nSource: {source_uri}\n\n{chunk_text}")
}

fn normalize_embedding_component(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn source_id_from_blob(bytes: &[u8]) -> Result<SourceId, StoreError> {
    let bytes = <[u8; 16]>::try_from(bytes)
        .map_err(|_| StoreError::ImmutableEvidenceMismatch("vector source UUID"))?;
    Ok(SourceId::from_uuid(Uuid::from_bytes(bytes)))
}

fn metadata_u64(connection: &Connection, key: &'static str) -> Result<u64, StoreError> {
    std::str::from_utf8(&metadata_value(connection, key)?)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(StoreError::InvalidMetadata(key))
}

fn set_metadata(
    transaction: &Transaction<'_>,
    key: &'static str,
    value: &[u8],
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE index_meta SET value = ?1 WHERE key = ?2",
        params![value, key],
    )?;
    if changed != 1 {
        return Err(StoreError::InvalidMetadata(key));
    }
    Ok(())
}

pub(crate) fn validate_stored_provenance(
    canonical_json: &str,
    fingerprint: &[u8],
    compatibility_fingerprint: &[u8],
    schema_version: &str,
) -> Result<EmbeddingProvenanceRecord, StoreError> {
    let provenance = EmbeddingProvenanceRecord::from_json(canonical_json)
        .map_err(|_| StoreError::ImmutableEvidenceMismatch("embedding provenance"))?;
    if fingerprint != provenance.fingerprint().as_bytes()
        || compatibility_fingerprint != provenance.compatibility_fingerprint().as_bytes()
        || schema_version != EMBEDDING_PROVENANCE_SCHEMA
        || canonical_json != provenance.canonical_json()
    {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "embedding provenance",
        ));
    }
    Ok(provenance)
}

pub(crate) fn validate_stored_vector_blob(blob: &[u8]) -> Result<(), StoreError> {
    if blob.len() != EMBEDDING_DIMENSION as usize * std::mem::size_of::<f32>() {
        return Err(StoreError::ImmutableEvidenceMismatch(
            "embedding vector dimension",
        ));
    }
    let vector = blob
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("exact four-byte chunk")))
        .collect::<Vec<_>>();
    validate_vector(&vector).map_err(|_| StoreError::ImmutableEvidenceMismatch("embedding vector"))
}

fn validate_vector(vector: &[f32]) -> Result<(), StoreError> {
    if vector.len() != EMBEDDING_DIMENSION as usize {
        return Err(StoreError::InvalidEmbedding("vector dimension"));
    }
    if vector.iter().any(|component| !component.is_finite()) {
        return Err(StoreError::InvalidEmbedding("non-finite vector"));
    }
    let squared_norm = vector
        .iter()
        .map(|component| component * component)
        .sum::<f32>();
    let norm = squared_norm.sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > NORMALIZED_NORM_TOLERANCE {
        return Err(StoreError::InvalidEmbedding("vector normalization"));
    }
    Ok(())
}

fn vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for component in vector {
        bytes.extend_from_slice(&component.to_le_bytes());
    }
    bytes
}

fn require_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &'static str,
    expected: Option<&str>,
) -> Result<&'a str, EmbeddingProvenanceError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(EmbeddingProvenanceError::Field(field))?;
    if expected.is_some_and(|expected| value != expected) {
        return Err(EmbeddingProvenanceError::Field(field));
    }
    Ok(value)
}

fn require_nonempty_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    max_bytes: usize,
) -> Result<(), EmbeddingProvenanceError> {
    let value = require_string(object, field, None)?;
    if value.is_empty()
        || value.len() > max_bytes
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(EmbeddingProvenanceError::Field(field));
    }
    Ok(())
}

fn require_u32(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<u32, EmbeddingProvenanceError> {
    u32::try_from(require_u64(object, field)?).map_err(|_| EmbeddingProvenanceError::Field(field))
}

fn require_u64(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<u64, EmbeddingProvenanceError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(EmbeddingProvenanceError::Field(field))
}

pub(crate) fn read_embedding_profile(
    connection: &Connection,
) -> Result<IndexEmbeddingProfile, StoreError> {
    let profile = metadata_value(connection, "embedding_profile")?;
    let model_id = metadata_value(connection, "embedding_model_id")?;
    let revision = metadata_value(connection, "embedding_revision")?;
    let fingerprint = metadata_value(connection, "embedding_model_fingerprint")?;
    let dimension = metadata_value(connection, "embedding_dimension")?;
    match profile.as_slice() {
        b"none"
            if model_id.is_empty()
                && revision.is_empty()
                && fingerprint.is_empty()
                && dimension == b"0" =>
        {
            Ok(IndexEmbeddingProfile::LexicalOnly)
        }
        b"pinned" => {
            let model_id = String::from_utf8(model_id)
                .map_err(|_| StoreError::InvalidMetadata("embedding_model_id"))?;
            let revision = String::from_utf8(revision)
                .map_err(|_| StoreError::InvalidMetadata("embedding_revision"))?;
            let fingerprint = <[u8; 32]>::try_from(fingerprint)
                .map(Sha256Digest::from_bytes)
                .map_err(|_| StoreError::InvalidMetadata("embedding_model_fingerprint"))?;
            let dimension = std::str::from_utf8(&dimension)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or(StoreError::InvalidMetadata("embedding_dimension"))?;
            let pin = EmbeddingModelPin::new(model_id, revision, fingerprint, dimension)
                .map_err(|_| StoreError::InvalidMetadata("embedding profile"))?;
            Ok(IndexEmbeddingProfile::Pinned(pin))
        }
        _ => Err(StoreError::InvalidMetadata("embedding profile")),
    }
}

pub(crate) fn insert_embedding_metadata(
    transaction: &Transaction<'_>,
    profile: &IndexEmbeddingProfile,
) -> Result<(), StoreError> {
    let values: [(&str, Vec<u8>); 6] = match profile {
        IndexEmbeddingProfile::LexicalOnly => [
            ("embedding_profile", b"none".to_vec()),
            ("embedding_model_id", Vec::new()),
            ("embedding_revision", Vec::new()),
            ("embedding_model_fingerprint", Vec::new()),
            ("embedding_dimension", b"0".to_vec()),
            ("active_vector_slot", b"0".to_vec()),
        ],
        IndexEmbeddingProfile::Pinned(pin) => [
            ("embedding_profile", b"pinned".to_vec()),
            ("embedding_model_id", pin.model_id().as_bytes().to_vec()),
            (
                "embedding_revision",
                pin.upstream_revision().as_bytes().to_vec(),
            ),
            (
                "embedding_model_fingerprint",
                pin.model_fingerprint().as_bytes().to_vec(),
            ),
            (
                "embedding_dimension",
                pin.dimension().to_string().into_bytes(),
            ),
            ("active_vector_slot", b"0".to_vec()),
        ],
    };
    for (key, value) in values {
        transaction.execute(
            "INSERT INTO index_meta(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

pub(crate) fn insert_embedding_metadata_for_migration(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    for (key, value) in [
        ("embedding_model_id", b"".as_slice()),
        ("embedding_revision", b"".as_slice()),
        ("embedding_model_fingerprint", b"".as_slice()),
        ("embedding_dimension", b"0".as_slice()),
        ("active_vector_slot", b"0".as_slice()),
    ] {
        transaction.execute(
            "INSERT INTO index_meta(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn metadata_value(connection: &Connection, key: &str) -> Result<Vec<u8>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn ensure_sqlite_vec_registered() -> Result<(), StoreError> {
    static REGISTRATION: OnceLock<i32> = OnceLock::new();
    let result = *REGISTRATION.get_or_init(register_sqlite_vec);
    if result == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(StoreError::SqliteVecRegistration(result))
    }
}

#[allow(unsafe_code)]
fn register_sqlite_vec() -> i32 {
    // SAFETY: sqlite-vec exports this SQLite extension initializer for static
    // registration. The signature conversion is the upstream rusqlite
    // integration contract, and OnceLock guarantees one process-wide call
    // before any hSUM database connection is opened.
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    }
}
