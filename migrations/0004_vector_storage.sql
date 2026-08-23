ALTER TABLE generations ADD COLUMN embedding_model_id TEXT
    CHECK (
        embedding_model_id IS NULL
        OR length(CAST(embedding_model_id AS BLOB)) BETWEEN 1 AND 64
    );
ALTER TABLE generations ADD COLUMN embedding_revision TEXT
    CHECK (
        embedding_revision IS NULL
        OR length(CAST(embedding_revision AS BLOB)) = 40
    );
ALTER TABLE generations ADD COLUMN embedding_model_fingerprint BLOB
    CHECK (
        embedding_model_fingerprint IS NULL
        OR length(embedding_model_fingerprint) = 32
    );
ALTER TABLE generations ADD COLUMN embedding_dimension INTEGER
    CHECK (embedding_dimension IS NULL OR embedding_dimension = 384);
ALTER TABLE generations ADD COLUMN vector_state TEXT NOT NULL DEFAULT 'absent'
    CHECK (vector_state IN ('absent', 'complete'));

CREATE TABLE embedding_provenance (
    fingerprint BLOB PRIMARY KEY CHECK (length(fingerprint) = 32),
    compatibility_fingerprint BLOB NOT NULL
        CHECK (length(compatibility_fingerprint) = 32),
    schema_version TEXT NOT NULL
        CHECK (length(CAST(schema_version AS BLOB)) BETWEEN 1 AND 64),
    canonical_json TEXT NOT NULL
        CHECK (length(CAST(canonical_json AS BLOB)) BETWEEN 2 AND 65536)
) STRICT, WITHOUT ROWID;

CREATE TABLE chunk_embeddings (
    id INTEGER PRIMARY KEY,
    chunk_id INTEGER NOT NULL,
    model_fingerprint BLOB NOT NULL CHECK (length(model_fingerprint) = 32),
    input_sha256 BLOB NOT NULL CHECK (length(input_sha256) = 32),
    vector_dimension INTEGER NOT NULL CHECK (vector_dimension = 384),
    vector_blob BLOB NOT NULL CHECK (length(vector_blob) = vector_dimension * 4),
    provenance_fingerprint BLOB NOT NULL CHECK (length(provenance_fingerprint) = 32),
    UNIQUE (model_fingerprint, input_sha256),
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE RESTRICT,
    FOREIGN KEY (provenance_fingerprint)
        REFERENCES embedding_provenance(fingerprint) ON DELETE RESTRICT
) STRICT;

CREATE INDEX chunk_embeddings_chunk_idx
    ON chunk_embeddings(chunk_id, model_fingerprint, input_sha256);

CREATE VIRTUAL TABLE passages_vec_a USING vec0(
    embedding float[384] distance_metric=cosine,
    source_id text partition key
);

CREATE VIRTUAL TABLE passages_vec_b USING vec0(
    embedding float[384] distance_metric=cosine,
    source_id text partition key
);
