mod cache;
mod download;
#[cfg(feature = "model-portability")]
mod inference;
mod manifest;

pub use cache::{
    ModelArtifactState, ModelError, ModelInventoryItem, ModelMutation, ModelRemoval, ModelStore,
    ModelVerification, discover_model_pins,
};
#[cfg(feature = "model-portability")]
pub use inference::{
    EmbeddingInferenceError, EmbeddingOptions, LocalTextEmbedding, VerifiedEmbeddingArtifact,
    VerifiedEmbeddingBytes,
};
pub use manifest::{
    MODEL_MANIFEST_SCHEMA, MODEL_RECEIPT_FILE, ManifestError, ModelFile, ModelKind, ModelManifest,
    builtin_manifests,
};
