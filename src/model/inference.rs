use std::fs::File;
use std::io::{Read, Take};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use serde::Serialize;
use thiserror::Error;

use crate::domain::Sha256Digest;

use super::{ModelError, ModelFile, ModelKind, ModelManifest, ModelStore};

const ONNX_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const CONFIG_FILE: &str = "config.json";
const SPECIAL_TOKENS_MAP_FILE: &str = "special_tokens_map.json";
const TOKENIZER_CONFIG_FILE: &str = "tokenizer_config.json";
const NORMALIZED_NORM_TOLERANCE: f32 = 0.001;

pub const EMBEDDING_PROVENANCE_SCHEMA: &str = "hsum.embedding-provenance.v1";
pub const FASTEMBED_VERSION: &str = "5.17.4";
pub const ORT_CRATE_VERSION: &str = "2.0.0-rc.13";
pub const ONNX_RUNTIME_VERSION: &str = "1.28.0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmbeddingProvenance {
    pub schema_version: &'static str,
    pub model_id: String,
    pub upstream_repository: String,
    pub upstream_revision: String,
    pub manifest_sha256: Sha256Digest,
    pub files: Vec<ModelFile>,
    pub vector_dimension: u32,
    pub pooling: &'static str,
    pub normalization: &'static str,
    pub normalization_implementation: &'static str,
    pub component_type: &'static str,
    pub quantization: &'static str,
    pub output_selection: &'static str,
    pub fastembed_version: &'static str,
    pub ort_crate_version: &'static str,
    pub onnx_runtime_version: &'static str,
    pub onnx_runtime_build_info: String,
    pub execution_provider: &'static str,
    pub execution_provider_configuration: &'static str,
    pub graph_optimization_level: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub target_endianness: &'static str,
    pub max_length: usize,
    pub intra_threads: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingOptions {
    max_length: NonZeroUsize,
    intra_threads: NonZeroUsize,
}

impl EmbeddingOptions {
    #[must_use]
    pub fn new(max_length: NonZeroUsize, intra_threads: NonZeroUsize) -> Self {
        Self {
            max_length,
            intra_threads,
        }
    }

    #[must_use]
    pub fn max_length(self) -> usize {
        self.max_length.get()
    }

    #[must_use]
    pub fn intra_threads(self) -> usize {
        self.intra_threads.get()
    }
}

#[derive(Debug)]
pub struct VerifiedEmbeddingArtifact {
    root: PathBuf,
    manifest: ModelManifest,
    fingerprint: Sha256Digest,
}

impl VerifiedEmbeddingArtifact {
    pub fn read(self) -> Result<VerifiedEmbeddingBytes, EmbeddingInferenceError> {
        let onnx = read_manifest_file(&self.root, required_file(&self.manifest, ONNX_FILE)?)?;
        let tokenizer_file =
            read_manifest_file(&self.root, required_file(&self.manifest, TOKENIZER_FILE)?)?;
        let config_file =
            read_manifest_file(&self.root, required_file(&self.manifest, CONFIG_FILE)?)?;
        let special_tokens_map_file = read_manifest_file(
            &self.root,
            required_file(&self.manifest, SPECIAL_TOKENS_MAP_FILE)?,
        )?;
        let tokenizer_config_file = read_manifest_file(
            &self.root,
            required_file(&self.manifest, TOKENIZER_CONFIG_FILE)?,
        )?;

        let bytes = self.manifest.files.iter().map(|file| file.bytes).sum();
        Ok(VerifiedEmbeddingBytes {
            manifest: self.manifest,
            fingerprint: self.fingerprint,
            bytes,
            onnx,
            tokenizer_files: TokenizerFiles {
                tokenizer_file,
                config_file,
                special_tokens_map_file,
                tokenizer_config_file,
            },
        })
    }
}

#[derive(Debug)]
pub struct VerifiedEmbeddingBytes {
    manifest: ModelManifest,
    fingerprint: Sha256Digest,
    bytes: u64,
    onnx: Vec<u8>,
    tokenizer_files: TokenizerFiles,
}

impl VerifiedEmbeddingBytes {
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn initialize(
        self,
        options: EmbeddingOptions,
    ) -> Result<LocalTextEmbedding, EmbeddingInferenceError> {
        let provenance = embedding_provenance(
            &self.manifest,
            self.fingerprint,
            options,
            ort::info().to_owned(),
        );
        let model = UserDefinedEmbeddingModel::new(self.onnx, self.tokenizer_files)
            .with_pooling(Pooling::Cls);
        let options = InitOptionsUserDefined::new()
            .with_max_length(options.max_length())
            .with_intra_threads(options.intra_threads());
        let model = TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|error| EmbeddingInferenceError::FastEmbed(error.to_string()))?;
        Ok(LocalTextEmbedding { provenance, model })
    }
}

fn embedding_provenance(
    manifest: &ModelManifest,
    fingerprint: Sha256Digest,
    options: EmbeddingOptions,
    onnx_runtime_build_info: String,
) -> EmbeddingProvenance {
    EmbeddingProvenance {
        schema_version: EMBEDDING_PROVENANCE_SCHEMA,
        model_id: manifest.id.clone(),
        upstream_repository: manifest.upstream_repository.clone(),
        upstream_revision: manifest.upstream_revision.clone(),
        manifest_sha256: fingerprint,
        files: manifest.files.clone(),
        vector_dimension: manifest.dimension,
        pooling: "cls",
        normalization: "l2_after_pooling",
        normalization_implementation: "fastembed::common::normalize",
        component_type: "ieee754_binary32",
        quantization: "none",
        output_selection: "fastembed_default_precedence",
        fastembed_version: FASTEMBED_VERSION,
        ort_crate_version: ORT_CRATE_VERSION,
        onnx_runtime_version: ONNX_RUNTIME_VERSION,
        onnx_runtime_build_info,
        execution_provider: "CPUExecutionProvider",
        execution_provider_configuration: "default_cpu_fallback",
        graph_optimization_level: "level3",
        target_os: std::env::consts::OS,
        target_arch: std::env::consts::ARCH,
        target_endianness: if cfg!(target_endian = "little") {
            "little"
        } else {
            "big"
        },
        max_length: options.max_length(),
        intra_threads: options.intra_threads(),
    }
}

pub struct LocalTextEmbedding {
    provenance: EmbeddingProvenance,
    model: TextEmbedding,
}

impl LocalTextEmbedding {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.provenance.model_id
    }

    #[must_use]
    pub fn fingerprint(&self) -> Sha256Digest {
        self.provenance.manifest_sha256
    }

    #[must_use]
    pub fn dimension(&self) -> u32 {
        self.provenance.vector_dimension
    }

    #[must_use]
    pub fn provenance(&self) -> &EmbeddingProvenance {
        &self.provenance
    }

    pub fn embed(
        &mut self,
        texts: &[String],
        batch_size: NonZeroUsize,
    ) -> Result<Vec<Vec<f32>>, EmbeddingInferenceError> {
        if texts.is_empty() {
            return Err(EmbeddingInferenceError::EmptyInput);
        }
        let embeddings = self
            .model
            .embed(texts, Some(batch_size.get()))
            .map_err(|error| EmbeddingInferenceError::FastEmbed(error.to_string()))?;
        validate_embeddings(texts.len(), self.dimension(), &embeddings)?;
        Ok(embeddings)
    }
}

impl<'a> ModelStore<'a> {
    pub fn verify_embedding_artifact(
        &self,
        id: &str,
    ) -> Result<VerifiedEmbeddingArtifact, EmbeddingInferenceError> {
        let manifest = self.manifest(id)?;
        if manifest.kind != ModelKind::Embedding {
            return Err(EmbeddingInferenceError::WrongKind(id.to_owned()));
        }
        for path in [
            ONNX_FILE,
            TOKENIZER_FILE,
            CONFIG_FILE,
            SPECIAL_TOKENS_MAP_FILE,
            TOKENIZER_CONFIG_FILE,
        ] {
            required_file(manifest, path)?;
        }
        let verification = self.verify(id)?;
        Ok(VerifiedEmbeddingArtifact {
            root: verification.path,
            manifest: manifest.clone(),
            fingerprint: verification.fingerprint,
        })
    }
}

fn required_file<'a>(
    manifest: &'a ModelManifest,
    path: &'static str,
) -> Result<&'a ModelFile, EmbeddingInferenceError> {
    manifest
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or(EmbeddingInferenceError::ManifestFileMissing(path))
}

fn read_manifest_file(root: &Path, expected: &ModelFile) -> Result<Vec<u8>, ModelError> {
    let path = root.join(&expected.path);
    let file = open_nofollow(&path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
        return Err(ModelError::FileSize {
            path: expected.path.clone(),
            expected: expected.bytes,
            actual: metadata.len(),
        });
    }
    let capacity = usize::try_from(expected.bytes).map_err(|_| ModelError::IntegerOverflow)?;
    let read_limit = expected
        .bytes
        .checked_add(1)
        .ok_or(ModelError::IntegerOverflow)?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded: Take<File> = file.take(read_limit);
    bounded.read_to_end(&mut bytes)?;
    let actual_bytes = u64::try_from(bytes.len()).map_err(|_| ModelError::IntegerOverflow)?;
    let actual_sha256 = Sha256Digest::of_bytes(&bytes);
    if actual_bytes != expected.bytes || actual_sha256 != expected.sha256 {
        return Err(ModelError::Checksum {
            path: expected.path.clone(),
            expected_bytes: expected.bytes,
            actual_bytes,
            expected_sha256: expected.sha256,
            actual_sha256,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_nofollow(path: &Path) -> Result<File, std::io::Error> {
    use rustix::fs::{Mode, OFlags, open};

    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(descriptor.into())
}

#[cfg(not(unix))]
fn open_nofollow(path: &Path) -> Result<File, std::io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "model artifact path is not a regular file",
        ));
    }
    File::open(path)
}

fn validate_embeddings(
    expected_count: usize,
    expected_dimension: u32,
    embeddings: &[Vec<f32>],
) -> Result<(), EmbeddingInferenceError> {
    if embeddings.len() != expected_count {
        return Err(EmbeddingInferenceError::OutputCount {
            expected: expected_count,
            actual: embeddings.len(),
        });
    }
    let expected_dimension = usize::try_from(expected_dimension)
        .map_err(|_| EmbeddingInferenceError::DimensionOverflow)?;
    for (document, embedding) in embeddings.iter().enumerate() {
        if embedding.len() != expected_dimension {
            return Err(EmbeddingInferenceError::OutputDimension {
                document,
                expected: expected_dimension,
                actual: embedding.len(),
            });
        }
        if let Some(component) = embedding.iter().position(|value| !value.is_finite()) {
            return Err(EmbeddingInferenceError::NonFinite {
                document,
                component,
            });
        }
        let norm = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if (norm - 1.0).abs() > NORMALIZED_NORM_TOLERANCE {
            return Err(EmbeddingInferenceError::NotNormalized { document, norm });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum EmbeddingInferenceError {
    #[error(transparent)]
    Artifact(#[from] ModelError),
    #[error("model {0} is not an embedding model")]
    WrongKind(String),
    #[error("embedding manifest is missing required FastEmbed file {0}")]
    ManifestFileMissing(&'static str),
    #[error("embedding input must contain at least one document")]
    EmptyInput,
    #[error("FastEmbed CPU inference failed: {0}")]
    FastEmbed(String),
    #[error("embedding output count differs: expected {expected}, got {actual}")]
    OutputCount { expected: usize, actual: usize },
    #[error("embedding dimension does not fit this platform")]
    DimensionOverflow,
    #[error("embedding output {document} has dimension {actual}; expected dimension {expected}")]
    OutputDimension {
        document: usize,
        expected: usize,
        actual: usize,
    },
    #[error("embedding output {document} component {component} is not finite")]
    NonFinite { document: usize, component: usize },
    #[error("embedding output {document} has non-unit L2 norm {norm}")]
    NotNormalized { document: usize, norm: f32 },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::model::{MODEL_MANIFEST_SCHEMA, MODEL_RECEIPT_FILE};

    fn manifest(files: &[(&str, &[u8])]) -> ModelManifest {
        ModelManifest {
            schema_version: MODEL_MANIFEST_SCHEMA.to_owned(),
            id: "fixture-embedding".to_owned(),
            kind: ModelKind::Embedding,
            upstream_repository: "example/fixture".to_owned(),
            upstream_revision: "a".repeat(40),
            dimension: 3,
            license_id: "MIT".to_owned(),
            source_url: "https://example.test/fixture".to_owned(),
            files: files
                .iter()
                .map(|(path, bytes)| ModelFile {
                    path: (*path).to_owned(),
                    bytes: bytes.len() as u64,
                    sha256: Sha256Digest::of_bytes(bytes),
                })
                .collect(),
        }
    }

    fn install_fixture(root: &Path, manifest: &ModelManifest, files: &[(&str, &[u8])]) {
        for (path, bytes) in files {
            let destination = root.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, bytes).unwrap();
        }
        fs::write(
            root.join(MODEL_RECEIPT_FILE),
            serde_json_canonicalizer::to_vec(manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn verified_artifact_rereads_and_rehashes_every_fastembed_input() {
        let files: [(&str, &[u8]); 5] = [
            (CONFIG_FILE, br#"{"pad_token_id":0}"#),
            (ONNX_FILE, b"not a real ONNX model"),
            (SPECIAL_TOKENS_MAP_FILE, br#"{}"#),
            (TOKENIZER_FILE, br#"{}"#),
            (
                TOKENIZER_CONFIG_FILE,
                br#"{"model_max_length":512,"pad_token":"[PAD]"}"#,
            ),
        ];
        let manifest = manifest(&files);
        let manifests = [manifest.clone()];
        let directory = TempDir::new().unwrap();
        let cache = directory.path().join("models");
        let store = ModelStore::with_manifests(cache, &manifests);
        let source = directory.path().join("source");
        install_fixture(&source, &manifest, &files);
        store.import(&source).unwrap();

        let artifact = store.verify_embedding_artifact(&manifest.id).unwrap();
        let bytes = artifact.read().unwrap();
        assert_eq!(
            bytes.bytes(),
            files
                .iter()
                .map(|(_, bytes)| bytes.len() as u64)
                .sum::<u64>()
        );
    }

    #[test]
    fn output_contract_rejects_wrong_shape_nonfinite_and_nonunit_vectors() {
        assert!(matches!(
            validate_embeddings(1, 3, &[]),
            Err(EmbeddingInferenceError::OutputCount { .. })
        ));
        assert!(matches!(
            validate_embeddings(1, 3, &[vec![1.0, 0.0]]),
            Err(EmbeddingInferenceError::OutputDimension { .. })
        ));
        assert!(matches!(
            validate_embeddings(1, 3, &[vec![f32::NAN, 0.0, 0.0]]),
            Err(EmbeddingInferenceError::NonFinite { .. })
        ));
        assert!(matches!(
            validate_embeddings(1, 3, &[vec![0.5, 0.0, 0.0]]),
            Err(EmbeddingInferenceError::NotNormalized { .. })
        ));
        validate_embeddings(1, 3, &[vec![1.0, 0.0, 0.0]]).unwrap();
    }

    #[test]
    fn provenance_binds_exact_artifact_runtime_and_preprocessing_contract() {
        let files: [(&str, &[u8]); 5] = [
            (CONFIG_FILE, br#"{"pad_token_id":0}"#),
            (ONNX_FILE, b"not a real ONNX model"),
            (SPECIAL_TOKENS_MAP_FILE, br#"{}"#),
            (TOKENIZER_FILE, br#"{}"#),
            (
                TOKENIZER_CONFIG_FILE,
                br#"{"model_max_length":512,"pad_token":"[PAD]"}"#,
            ),
        ];
        let manifest = manifest(&files);
        let fingerprint = manifest.fingerprint().unwrap();
        let options = EmbeddingOptions::new(
            NonZeroUsize::new(512).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let provenance =
            embedding_provenance(&manifest, fingerprint, options, "fixture build".to_owned());

        assert_eq!(provenance.schema_version, EMBEDDING_PROVENANCE_SCHEMA);
        assert_eq!(provenance.model_id, manifest.id);
        assert_eq!(provenance.upstream_revision, manifest.upstream_revision);
        assert_eq!(provenance.manifest_sha256, fingerprint);
        assert_eq!(provenance.files, manifest.files);
        assert_eq!(provenance.vector_dimension, 3);
        assert_eq!(provenance.pooling, "cls");
        assert_eq!(provenance.normalization, "l2_after_pooling");
        assert_eq!(provenance.fastembed_version, "5.17.4");
        assert_eq!(provenance.ort_crate_version, "2.0.0-rc.13");
        assert_eq!(provenance.onnx_runtime_version, "1.28.0");
        assert_eq!(provenance.onnx_runtime_build_info, "fixture build");
        assert_eq!(provenance.execution_provider, "CPUExecutionProvider");
        assert_eq!(provenance.max_length, 512);
        assert_eq!(provenance.intra_threads, 4);
    }

    #[test]
    fn read_rejects_same_size_tampering_after_full_verification() {
        let files: [(&str, &[u8]); 5] = [
            (CONFIG_FILE, br#"{"pad_token_id":0}"#),
            (ONNX_FILE, b"not a real ONNX model"),
            (SPECIAL_TOKENS_MAP_FILE, br#"{}"#),
            (TOKENIZER_FILE, br#"{}"#),
            (
                TOKENIZER_CONFIG_FILE,
                br#"{"model_max_length":512,"pad_token":"[PAD]"}"#,
            ),
        ];
        let manifest = manifest(&files);
        let manifests = [manifest.clone()];
        let directory = TempDir::new().unwrap();
        let store = ModelStore::with_manifests(directory.path().join("models"), &manifests);
        let source = directory.path().join("source");
        install_fixture(&source, &manifest, &files);
        store.import(&source).unwrap();

        let artifact = store.verify_embedding_artifact(&manifest.id).unwrap();
        let installed = store.installation_path(&manifest).unwrap();
        fs::write(installed.join(SPECIAL_TOKENS_MAP_FILE), br#"[]"#).unwrap();

        assert!(matches!(
            artifact.read(),
            Err(EmbeddingInferenceError::Artifact(
                ModelError::Checksum { .. }
            ))
        ));
    }
}
