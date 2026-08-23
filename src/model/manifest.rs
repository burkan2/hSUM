use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{SafeSlug, Sha256Digest};

pub const MODEL_MANIFEST_SCHEMA: &str = "hsum.model-manifest.v1";
pub const MODEL_RECEIPT_FILE: &str = "hsum-model.json";

const BGE_SMALL_EN_V15_FP32: &str = include_str!("../../assets/models/bge-small-en-v1.5-fp32.json");

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub schema_version: String,
    pub id: String,
    pub kind: ModelKind,
    pub upstream_repository: String,
    pub upstream_revision: String,
    pub dimension: u32,
    pub license_id: String,
    pub source_url: String,
    pub files: Vec<ModelFile>,
}

impl ModelManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MODEL_MANIFEST_SCHEMA {
            return Err(ManifestError::Schema(self.schema_version.clone()));
        }
        SafeSlug::new(self.id.clone()).map_err(ManifestError::Id)?;
        if self.upstream_repository.is_empty()
            || self.upstream_repository.len() > 256
            || !self.upstream_repository.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            return Err(ManifestError::Repository);
        }
        if self.upstream_revision.len() != 40
            || !self
                .upstream_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ManifestError::Revision);
        }
        if self.dimension == 0 {
            return Err(ManifestError::Dimension);
        }
        if self.license_id.is_empty()
            || self.license_id.len() > 64
            || !self.license_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'+' | b' ')
            })
        {
            return Err(ManifestError::License);
        }
        if !self.source_url.starts_with("https://")
            || self.source_url.len() > 2_048
            || self.source_url.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ManifestError::SourceUrl);
        }
        if self.files.is_empty() || self.files.len() > 64 {
            return Err(ManifestError::FileCount);
        }

        let mut paths = BTreeSet::new();
        for file in &self.files {
            file.validate()?;
            if file.path == MODEL_RECEIPT_FILE || !paths.insert(file.path.clone()) {
                return Err(ManifestError::DuplicateFile(file.path.clone()));
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<Sha256Digest, ManifestError> {
        self.validate()?;
        let canonical = serde_json_canonicalizer::to_vec(self)
            .map_err(|error| ManifestError::Serialize(error.to_string()))?;
        Ok(Sha256Digest::of_bytes(&canonical))
    }

    pub fn download_url(&self, file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}?download=true",
            self.upstream_repository, self.upstream_revision, file.path
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Embedding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: Sha256Digest,
}

impl ModelFile {
    pub(crate) fn validate(&self) -> Result<(), ManifestError> {
        if self.bytes == 0 {
            return Err(ManifestError::EmptyFile(self.path.clone()));
        }
        let path = Path::new(&self.path);
        if self.path.is_empty()
            || self.path.len() > 512
            || path.is_absolute()
            || path.components().any(
                |component| !matches!(component, Component::Normal(value) if !value.is_empty()),
            )
            || self.path.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
            })
        {
            return Err(ManifestError::FilePath(self.path.clone()));
        }
        Ok(())
    }
}

pub fn builtin_manifests() -> &'static [ModelManifest] {
    static MANIFESTS: OnceLock<Vec<ModelManifest>> = OnceLock::new();
    MANIFESTS.get_or_init(|| {
        let manifest: ModelManifest = serde_json::from_str(BGE_SMALL_EN_V15_FP32)
            .expect("the embedded BGE manifest must be valid JSON");
        manifest
            .validate()
            .expect("the embedded BGE manifest must satisfy the model contract");
        vec![manifest]
    })
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ManifestError {
    #[error("unsupported model manifest schema {0}")]
    Schema(String),
    #[error("invalid model ID: {0}")]
    Id(crate::domain::SlugError),
    #[error("invalid upstream repository")]
    Repository,
    #[error("upstream revision must be a full lowercase 40-byte Git commit")]
    Revision,
    #[error("embedding dimension must be nonzero")]
    Dimension,
    #[error("invalid SPDX license identifier")]
    License,
    #[error("model source URL must be bounded HTTPS")]
    SourceUrl,
    #[error("model manifest file count is outside the supported range")]
    FileCount,
    #[error("invalid model file path {0}")]
    FilePath(String),
    #[error("model file {0} must be nonempty")]
    EmptyFile(String),
    #[error("duplicate or reserved model file path {0}")]
    DuplicateFile(String),
    #[error("could not serialize the model manifest: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_is_exact_and_fastembed_compatible() {
        let [manifest] = builtin_manifests() else {
            panic!("expected exactly one initial manifest");
        };
        assert_eq!(manifest.id, "bge-small-en-v1-5-fp32");
        assert_eq!(manifest.dimension, 384);
        assert_eq!(manifest.license_id, "MIT");
        assert_eq!(manifest.files.len(), 5);
        for required in [
            "config.json",
            "onnx/model.onnx",
            "special_tokens_map.json",
            "tokenizer.json",
            "tokenizer_config.json",
        ] {
            assert!(manifest.files.iter().any(|file| file.path == required));
        }
        assert_eq!(
            manifest.files.iter().map(|file| file.bytes).sum::<u64>(),
            133_806_060
        );
        assert_eq!(manifest.fingerprint().unwrap().to_string().len(), 64);
    }

    #[test]
    fn traversal_and_insecure_sources_are_rejected() {
        let mut manifest = builtin_manifests()[0].clone();
        manifest.files[0].path = "../model.onnx".to_owned();
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::FilePath(_))
        ));

        let mut manifest = builtin_manifests()[0].clone();
        manifest.source_url = "http://example.test/model".to_owned();
        assert_eq!(manifest.validate(), Err(ManifestError::SourceUrl));
    }
}
