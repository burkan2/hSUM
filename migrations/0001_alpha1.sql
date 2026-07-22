CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    applied_at TEXT NOT NULL,
    checksum BLOB NOT NULL CHECK (length(checksum) = 32)
) STRICT;

CREATE TABLE index_meta (
    key TEXT PRIMARY KEY,
    value BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE generations (
    id INTEGER PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('building', 'committed', 'abandoned')),
    created_at TEXT NOT NULL,
    committed_at TEXT,
    owner_pid INTEGER,
    owner_host TEXT,
    lease_nonce BLOB CHECK (lease_nonce IS NULL OR length(lease_nonce) = 16),
    heartbeat_at TEXT,
    pipeline_fingerprint BLOB NOT NULL CHECK (length(pipeline_fingerprint) = 32),
    CHECK (
        (state = 'building' AND committed_at IS NULL)
        OR (state = 'committed' AND committed_at IS NOT NULL)
        OR state = 'abandoned'
    )
) STRICT;

CREATE TABLE sources (
    id BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind TEXT NOT NULL CHECK (kind = 'filesystem'),
    name TEXT NOT NULL UNIQUE,
    logical_uri TEXT NOT NULL UNIQUE,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_success_at TEXT,
    last_error_code TEXT,
    last_error_detail TEXT,
    last_error_at TEXT,
    CHECK (
        (last_error_code IS NULL AND last_error_detail IS NULL AND last_error_at IS NULL)
        OR (last_error_code IS NOT NULL AND last_error_detail IS NOT NULL AND last_error_at IS NOT NULL)
    )
) STRICT;

CREATE TABLE projects (
    id BLOB PRIMARY KEY CHECK (length(id) = 16),
    name TEXT NOT NULL UNIQUE,
    scope_revision INTEGER NOT NULL DEFAULT 0 CHECK (scope_revision >= 0),
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE project_sources (
    project_id BLOB NOT NULL CHECK (length(project_id) = 16),
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    PRIMARY KEY (project_id, source_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TABLE documents (
    id BLOB PRIMARY KEY CHECK (length(id) = 16),
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    connector_key BLOB NOT NULL CHECK (length(connector_key) BETWEEN 1 AND 4096),
    current_source_uri TEXT NOT NULL,
    tombstoned_at TEXT,
    UNIQUE (source_id, connector_key),
    UNIQUE (id, source_id),
    FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE content_blobs (
    id INTEGER PRIMARY KEY,
    body_sha256 BLOB NOT NULL UNIQUE CHECK (length(body_sha256) = 32),
    original_bytes BLOB NOT NULL
) STRICT;

CREATE TABLE chunk_layouts (
    id INTEGER PRIMARY KEY,
    content_blob_id INTEGER NOT NULL,
    chunker_fingerprint BLOB NOT NULL CHECK (length(chunker_fingerprint) = 32),
    UNIQUE (content_blob_id, chunker_fingerprint),
    FOREIGN KEY (content_blob_id) REFERENCES content_blobs(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE chunks (
    id INTEGER PRIMARY KEY,
    chunk_layout_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    start_byte INTEGER NOT NULL CHECK (start_byte >= 0),
    end_byte INTEGER NOT NULL CHECK (end_byte >= start_byte),
    start_line INTEGER NOT NULL CHECK (start_line >= 1),
    end_line INTEGER NOT NULL CHECK (end_line >= start_line),
    body_text TEXT NOT NULL,
    content_sha256 BLOB NOT NULL CHECK (length(content_sha256) = 32),
    quote_bloom BLOB NOT NULL CHECK (length(quote_bloom) = 512),
    UNIQUE (chunk_layout_id, ordinal),
    FOREIGN KEY (chunk_layout_id) REFERENCES chunk_layouts(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE document_versions (
    id INTEGER PRIMARY KEY,
    document_id BLOB NOT NULL CHECK (length(document_id) = 16),
    content_blob_id INTEGER NOT NULL,
    revision_sha256 BLOB NOT NULL CHECK (length(revision_sha256) = 32),
    source_uri TEXT NOT NULL,
    title TEXT,
    metadata_json TEXT NOT NULL,
    source_updated_at TEXT,
    indexed_at TEXT NOT NULL,
    UNIQUE (document_id, revision_sha256),
    UNIQUE (id, document_id),
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE RESTRICT,
    FOREIGN KEY (content_blob_id) REFERENCES content_blobs(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE document_heads (
    document_id BLOB PRIMARY KEY CHECK (length(document_id) = 16),
    document_version_id INTEGER,
    state TEXT NOT NULL CHECK (state IN ('active', 'tombstoned')),
    generation_id INTEGER NOT NULL,
    CHECK (
        (state = 'active' AND document_version_id IS NOT NULL)
        OR (state = 'tombstoned' AND document_version_id IS NULL)
    ),
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE RESTRICT,
    FOREIGN KEY (document_version_id, document_id)
        REFERENCES document_versions(id, document_id) ON DELETE RESTRICT,
    FOREIGN KEY (generation_id) REFERENCES generations(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE generation_changes (
    generation_id INTEGER NOT NULL,
    document_id BLOB NOT NULL CHECK (length(document_id) = 16),
    prior_version_id INTEGER,
    next_version_id INTEGER,
    next_state TEXT NOT NULL CHECK (next_state IN ('active', 'tombstoned')),
    PRIMARY KEY (generation_id, document_id),
    CHECK (
        (next_state = 'active' AND next_version_id IS NOT NULL)
        OR (next_state = 'tombstoned' AND next_version_id IS NULL)
    ),
    CHECK (prior_version_id IS NULL OR prior_version_id IS NOT next_version_id),
    FOREIGN KEY (generation_id) REFERENCES generations(id) ON DELETE CASCADE,
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE RESTRICT,
    FOREIGN KEY (prior_version_id, document_id)
        REFERENCES document_versions(id, document_id) ON DELETE RESTRICT,
    FOREIGN KEY (next_version_id, document_id)
        REFERENCES document_versions(id, document_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TABLE active_passages (
    id INTEGER PRIMARY KEY,
    document_id BLOB NOT NULL CHECK (length(document_id) = 16),
    document_version_id INTEGER NOT NULL,
    chunk_id INTEGER NOT NULL,
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    UNIQUE (document_id, chunk_id),
    FOREIGN KEY (document_id, source_id)
        REFERENCES documents(id, source_id) ON DELETE RESTRICT,
    FOREIGN KEY (document_version_id, document_id)
        REFERENCES document_versions(id, document_id) ON DELETE RESTRICT,
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE RESTRICT
) STRICT;

CREATE VIRTUAL TABLE passages_fts USING fts5(
    title,
    source_uri,
    body,
    tokenize = 'unicode61 remove_diacritics 0 tokenchars ''_-.:/'''
);

CREATE TABLE passage_literals (
    passage_id INTEGER NOT NULL,
    literal BLOB NOT NULL CHECK (length(literal) BETWEEN 2 AND 128),
    field TEXT NOT NULL CHECK (field IN ('title', 'source_uri', 'body')),
    PRIMARY KEY (passage_id, literal, field),
    FOREIGN KEY (passage_id) REFERENCES active_passages(id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE source_sync_errors (
    id INTEGER PRIMARY KEY,
    generation_id INTEGER NOT NULL,
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    connector_key BLOB CHECK (connector_key IS NULL OR length(connector_key) BETWEEN 1 AND 4096),
    code TEXT NOT NULL,
    detail TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (generation_id) REFERENCES generations(id) ON DELETE CASCADE,
    FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE RESTRICT
) STRICT;

CREATE INDEX document_versions_content_blob_idx
    ON document_versions(content_blob_id);
CREATE INDEX document_heads_generation_idx
    ON document_heads(generation_id);
CREATE INDEX generation_changes_document_idx
    ON generation_changes(document_id, generation_id);
CREATE INDEX active_passages_source_idx
    ON active_passages(source_id, document_id, chunk_id);
CREATE INDEX active_passages_version_idx
    ON active_passages(document_version_id);
CREATE INDEX passage_literals_lookup_idx
    ON passage_literals(literal, field, passage_id);
CREATE INDEX source_sync_errors_generation_idx
    ON source_sync_errors(generation_id, source_id);
