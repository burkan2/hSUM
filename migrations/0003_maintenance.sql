CREATE TABLE prune_runs (
    plan_hash BLOB PRIMARY KEY CHECK (length(plan_hash) = 32),
    applied_at TEXT NOT NULL,
    history_floor_epoch INTEGER NOT NULL CHECK (history_floor_epoch > 0),
    affected_revision_count INTEGER NOT NULL CHECK (affected_revision_count >= 0),
    affected_citation_count INTEGER NOT NULL CHECK (affected_citation_count >= 0),
    logical_reclaimable_bytes INTEGER NOT NULL CHECK (logical_reclaimable_bytes >= 0),
    manifest_json TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE pruned_revision_namespaces (
    plan_hash BLOB NOT NULL CHECK (length(plan_hash) = 32),
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    document_id BLOB NOT NULL CHECK (length(document_id) = 16),
    revision_sha256 BLOB NOT NULL CHECK (length(revision_sha256) = 32),
    PRIMARY KEY (plan_hash, source_id, document_id, revision_sha256),
    FOREIGN KEY (plan_hash) REFERENCES prune_runs(plan_hash) ON DELETE RESTRICT,
    FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE RESTRICT,
    FOREIGN KEY (document_id, source_id)
        REFERENCES documents(id, source_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TABLE forget_runs (
    plan_hash BLOB PRIMARY KEY CHECK (length(plan_hash) = 32),
    applied_at TEXT NOT NULL,
    pre_index_epoch INTEGER NOT NULL CHECK (pre_index_epoch >= 0),
    post_index_epoch INTEGER NOT NULL CHECK (post_index_epoch = pre_index_epoch + 1),
    pre_replacement_epoch INTEGER NOT NULL CHECK (pre_replacement_epoch >= 0),
    post_replacement_epoch INTEGER NOT NULL CHECK (
        post_replacement_epoch = pre_replacement_epoch + 1
    ),
    recovery_backup_sha256 BLOB NOT NULL CHECK (length(recovery_backup_sha256) = 32),
    affected_revision_count INTEGER NOT NULL CHECK (affected_revision_count > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE forgotten_documents (
    source_id BLOB NOT NULL CHECK (length(source_id) = 16),
    document_id BLOB NOT NULL CHECK (length(document_id) = 16),
    connector_key_sha256 BLOB NOT NULL CHECK (length(connector_key_sha256) = 32),
    forget_plan_hash BLOB NOT NULL CHECK (length(forget_plan_hash) = 32),
    forgotten_at TEXT NOT NULL,
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    PRIMARY KEY (source_id, connector_key_sha256),
    FOREIGN KEY (forget_plan_hash) REFERENCES forget_runs(plan_hash) ON DELETE RESTRICT,
    FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE RESTRICT,
    FOREIGN KEY (document_id, source_id)
        REFERENCES documents(id, source_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TABLE restore_runs (
    plan_hash BLOB PRIMARY KEY CHECK (length(plan_hash) = 32),
    forget_plan_hash BLOB NOT NULL CHECK (length(forget_plan_hash) = 32),
    applied_at TEXT NOT NULL,
    pre_index_epoch INTEGER NOT NULL CHECK (pre_index_epoch >= 0),
    post_index_epoch INTEGER NOT NULL CHECK (post_index_epoch = pre_index_epoch + 1),
    pre_replacement_epoch INTEGER NOT NULL CHECK (pre_replacement_epoch >= 0),
    post_replacement_epoch INTEGER NOT NULL CHECK (
        post_replacement_epoch = pre_replacement_epoch + 1
    ),
    recovery_backup_sha256 BLOB NOT NULL CHECK (length(recovery_backup_sha256) = 32),
    safety_backup_sha256 BLOB NOT NULL CHECK (length(safety_backup_sha256) = 32)
) STRICT, WITHOUT ROWID;
