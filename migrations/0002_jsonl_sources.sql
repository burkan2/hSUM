CREATE TABLE sources_v2 (
    id BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind TEXT NOT NULL CHECK (kind IN ('filesystem', 'jsonl')),
    name TEXT NOT NULL UNIQUE,
    logical_uri TEXT NOT NULL UNIQUE,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    removed_at TEXT,
    last_success_at TEXT,
    last_error_code TEXT,
    last_error_detail TEXT,
    last_error_at TEXT,
    CHECK (
        (last_error_code IS NULL AND last_error_detail IS NULL AND last_error_at IS NULL)
        OR (last_error_code IS NOT NULL AND last_error_detail IS NOT NULL AND last_error_at IS NOT NULL)
    )
) STRICT;

INSERT INTO sources_v2(
    id, kind, name, logical_uri, config_json, created_at, removed_at,
    last_success_at, last_error_code, last_error_detail, last_error_at
)
SELECT
    id, kind, name, logical_uri, config_json, created_at, NULL,
    last_success_at, last_error_code, last_error_detail, last_error_at
FROM sources;

DROP TABLE sources;
ALTER TABLE sources_v2 RENAME TO sources;

ALTER TABLE project_sources ADD COLUMN removed_at TEXT;
