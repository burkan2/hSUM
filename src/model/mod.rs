mod cache;
mod download;
mod inference;
mod manifest;
mod worker;

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
pub(crate) use worker::run_private_model_worker_from_env;
pub use worker::{
    MAX_QUERY_EMBEDDING_BYTES, QUERY_MODEL_QUEUE, QUERY_MODEL_WORKERS, QueryEmbeddingControl,
    QueryEmbeddingError, QueryEmbeddingRequest, QueryEmbeddingService,
};
