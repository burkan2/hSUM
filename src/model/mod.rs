mod cache;
mod download;
mod inference;
mod manifest;

pub use cache::{
    IndexModelState, ModelArtifactState, ModelError, ModelInventoryItem, ModelMutation,
    ModelRemoval, ModelStore, ModelVerification, discover_model_pins,
};
pub use inference::{
    EMBEDDING_PROVENANCE_SCHEMA, EmbeddingInferenceError, EmbeddingOptions, EmbeddingProvenance,
    FASTEMBED_VERSION, LocalTextEmbedding, ONNX_RUNTIME_VERSION, ORT_CRATE_VERSION,
    VerifiedEmbeddingArtifact, VerifiedEmbeddingBytes,
};
pub use manifest::{
    MODEL_MANIFEST_SCHEMA, MODEL_RECEIPT_FILE, ManifestError, ModelFile, ModelKind, ModelManifest,
    builtin_manifests,
};
