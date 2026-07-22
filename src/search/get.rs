use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use thiserror::Error;

use crate::domain::{ByteSpan, Citation, LineSpan, ProjectId, Sha256Digest};
use crate::ingest::{
    ChunkKind, DEFAULT_CHUNK_OVERLAP_BYTES, DEFAULT_CHUNK_TARGET_BYTES, HARD_MAX_FILE_BYTES,
};
use crate::store::{IndexDb, chunker_fingerprint};

pub const DEFAULT_GET_MAX_BYTES: usize = 16 * 1024;
pub const HARD_GET_MAX_BYTES: usize = 64 * 1024;
const MAX_GET_SOURCE_URI_BYTES: i64 = 16 * 1024;
const MAX_GET_TITLE_BYTES: i64 = 16 * 1024;
const MAX_GET_METADATA_BYTES: i64 = 64 * 1024;
const MAX_GET_TIMESTAMP_BYTES: i64 = 128;
const MIN_CHUNK_PROGRESS_BYTES: u64 =
    (DEFAULT_CHUNK_TARGET_BYTES / 2 - DEFAULT_CHUNK_OVERLAP_BYTES) as u64;
const MAX_CHUNKS_PER_LAYOUT: u64 = HARD_MAX_FILE_BYTES.div_ceil(MIN_CHUNK_PROGRESS_BYTES) + 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetRequest {
    pub project_id: ProjectId,
    pub citation: Citation,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetResponse {
    pub requested_citation: Citation,
    pub returned_citation: Citation,
    pub requested_line_span: LineSpan,
    pub returned_line_span: LineSpan,
    pub source_uri: String,
    pub title: String,
    pub metadata_json: String,
    pub source_updated_at: Option<String>,
    pub indexed_at: String,
    pub content: Vec<u8>,
    pub body_sha256: Sha256Digest,
    pub untrusted_content: bool,
}

#[derive(Debug, Error)]
pub enum GetError {
    #[error("citation is outside the bound project")]
    ScopeDenied,
    #[error("cited immutable evidence was not found")]
    EvidenceNotFound,
    #[error("max_bytes must be between 1 and {limit}; received {requested}")]
    InvalidBound { requested: usize, limit: usize },
    #[error(
        "max_bytes {requested} is smaller than the cited passage; at least {required} is required"
    )]
    BoundBelowPassage { requested: usize, required: usize },
    #[error("stored evidence invariant failed: {0}")]
    Corrupt(&'static str),
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
}

impl IndexDb {
    pub fn get_evidence(&self, request: &GetRequest) -> Result<GetResponse, GetError> {
        if !(1..=HARD_GET_MAX_BYTES).contains(&request.max_bytes) {
            return Err(GetError::InvalidBound {
                requested: request.max_bytes,
                limit: HARD_GET_MAX_BYTES,
            });
        }

        let transaction = self.connection().unchecked_transaction()?;
        let stored_index_id: Vec<u8> = transaction.query_row(
            "SELECT value FROM index_meta WHERE key = 'index_uuid'",
            [],
            |row| row.get(0),
        )?;
        if stored_index_id.as_slice() != request.citation.index_id.as_uuid().as_bytes() {
            return Err(GetError::ScopeDenied);
        }

        let project_id = *request.project_id.as_uuid().as_bytes();
        let source_id = *request.citation.source_id.as_uuid().as_bytes();
        let document_id = *request.citation.document_id.as_uuid().as_bytes();
        let in_scope: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM documents AS d
                JOIN project_sources AS ps
                  ON ps.source_id = d.source_id
                WHERE ps.project_id = ?1
                  AND d.id = ?2
                  AND d.source_id = ?3
            )",
            params![
                project_id.as_slice(),
                document_id.as_slice(),
                source_id.as_slice(),
            ],
            |row| row.get(0),
        )?;
        if !in_scope {
            return Err(GetError::ScopeDenied);
        }

        let start = integer_from_u64(request.citation.span.start())?;
        let end = integer_from_u64(request.citation.span.end())?;
        let oversized_version: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM document_versions AS dv
                 JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
                 WHERE dv.document_id = ?1
                   AND dv.revision_sha256 = ?2
                   AND EXISTS(
                       SELECT 1 FROM documents AS d
                       WHERE d.id = dv.document_id
                         AND d.source_id = ?3
                   )
                   AND (
                       length(CAST(dv.source_uri AS BLOB))
                           NOT BETWEEN 1 AND ?4
                       OR length(CAST(COALESCE(dv.title, '') AS BLOB)) > ?5
                       OR length(CAST(dv.metadata_json AS BLOB)) > ?6
                       OR (
                           dv.source_updated_at IS NOT NULL
                           AND length(CAST(dv.source_updated_at AS BLOB)) > ?7
                       )
                       OR length(CAST(dv.indexed_at AS BLOB))
                           NOT BETWEEN 1 AND ?7
                       OR length(cb.original_bytes) > ?8
                       OR length(cb.body_sha256) != 32
                   )
             )",
            params![
                document_id.as_slice(),
                request.citation.revision.as_bytes().as_slice(),
                source_id.as_slice(),
                MAX_GET_SOURCE_URI_BYTES,
                MAX_GET_TITLE_BYTES,
                MAX_GET_METADATA_BYTES,
                MAX_GET_TIMESTAMP_BYTES,
                i64::try_from(HARD_MAX_FILE_BYTES)
                    .expect("hard source file limit fits SQLite integer"),
            ],
            |row| row.get(0),
        )?;
        if oversized_version {
            return Err(GetError::Corrupt(
                "stored evidence exceeds alpha field bounds",
            ));
        }
        let stored = transaction
            .query_row(
                "SELECT dv.source_uri, COALESCE(dv.title, ''),
                        dv.metadata_json, dv.source_updated_at,
                        dv.indexed_at, cb.original_bytes,
                        cb.body_sha256, cb.id
                 FROM document_versions AS dv
                 JOIN content_blobs AS cb ON cb.id = dv.content_blob_id
                 WHERE dv.document_id = ?1
                   AND dv.revision_sha256 = ?2
                   AND EXISTS(
                       SELECT 1 FROM documents AS d
                       WHERE d.id = dv.document_id
                         AND d.source_id = ?3
                   )",
                params![
                    document_id.as_slice(),
                    request.citation.revision.as_bytes().as_slice(),
                    source_id.as_slice(),
                ],
                |row| {
                    Ok(StoredVersion {
                        source_uri: row.get(0)?,
                        title: row.get(1)?,
                        metadata_json: row.get(2)?,
                        source_updated_at: row.get(3)?,
                        indexed_at: row.get(4)?,
                        body: row.get(5)?,
                        body_sha256: row.get(6)?,
                        content_blob_id: row.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or(GetError::EvidenceNotFound)?;

        let chunk_kind = ChunkKind::from_path(Path::new(&stored.source_uri))
            .ok_or(GetError::Corrupt("stored source kind is unsupported"))?;
        let expected_fingerprint = chunker_fingerprint(chunk_kind);
        let (layout_id, cited_ordinal): (i64, i64) = transaction
            .query_row(
                "SELECT cl.id, c.ordinal
                 FROM chunk_layouts AS cl
                 JOIN chunks AS c ON c.chunk_layout_id = cl.id
                 WHERE cl.content_blob_id = ?1
                   AND cl.chunker_fingerprint = ?2
                   AND c.start_byte = ?3
                   AND c.end_byte = ?4",
                params![
                    stored.content_blob_id,
                    expected_fingerprint.as_bytes().as_slice(),
                    start,
                    end,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or(GetError::EvidenceNotFound)?;

        ensure_chunk_layout_cardinality(&transaction, layout_id, MAX_CHUNKS_PER_LAYOUT)?;
        if Sha256Digest::of_bytes(&stored.body) != digest_from_blob(&stored.body_sha256)? {
            return Err(GetError::Corrupt("body hash mismatch"));
        }
        std::str::from_utf8(&stored.body)
            .map_err(|_| GetError::Corrupt("stored body is not UTF-8"))?;

        let mut statement = transaction.prepare(
            "SELECT ordinal, start_byte, end_byte, start_line, end_line
             FROM chunks
             WHERE chunk_layout_id = ?1
             ORDER BY ordinal
             LIMIT ?2",
        )?;
        let chunks = statement
            .query_map(
                params![
                    layout_id,
                    i64::try_from(MAX_CHUNKS_PER_LAYOUT)
                        .expect("chunk layout cap fits SQLite integer"),
                ],
                |row| {
                    Ok(StoredChunk {
                        ordinal: row.get(0)?,
                        start_byte: row.get(1)?,
                        end_byte: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let cited_index = chunks
            .iter()
            .position(|chunk| chunk.ordinal == cited_ordinal)
            .ok_or(GetError::Corrupt("cited chunk ordinal is missing"))?;

        let cited_length = request
            .citation
            .span
            .end()
            .checked_sub(request.citation.span.start())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(GetError::Corrupt("cited span length is invalid"))?;
        if request.max_bytes < cited_length {
            return Err(GetError::BoundBelowPassage {
                requested: request.max_bytes,
                required: cited_length,
            });
        }

        let (left, right) = expand_chunk_window(&chunks, cited_index, request.max_bytes)?;
        let returned_start = u64_from_i64(chunks[left].start_byte)?;
        let returned_end = u64_from_i64(chunks[right].end_byte)?;
        let returned_span = ByteSpan::new(returned_start, returned_end)
            .map_err(|_| GetError::Corrupt("expanded span is reversed"))?;
        let content = returned_span
            .slice_bytes(&stored.body)
            .map_err(|_| GetError::Corrupt("expanded span is outside the body"))?
            .to_vec();
        let requested_line_span = line_span(&chunks[cited_index])?;
        let returned_line_span = LineSpan::new(
            u64_from_i64(chunks[left].start_line)?,
            u64_from_i64(chunks[right].end_line)?,
        )
        .map_err(|_| GetError::Corrupt("expanded line span is invalid"))?;
        let returned_citation = Citation {
            span: returned_span,
            ..request.citation.clone()
        };

        let response = GetResponse {
            requested_citation: request.citation.clone(),
            returned_citation,
            requested_line_span,
            returned_line_span,
            source_uri: stored.source_uri,
            title: stored.title,
            metadata_json: stored.metadata_json,
            source_updated_at: stored.source_updated_at,
            indexed_at: stored.indexed_at,
            content,
            body_sha256: digest_from_blob(&stored.body_sha256)?,
            untrusted_content: true,
        };
        drop(statement);
        transaction.rollback()?;
        Ok(response)
    }
}

fn ensure_chunk_layout_cardinality(
    connection: &Connection,
    layout_id: i64,
    max_chunks: u64,
) -> Result<(), GetError> {
    let oversized: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM chunks
             WHERE chunk_layout_id = ?1
             ORDER BY ordinal
             LIMIT 1 OFFSET ?2
         )",
        params![
            layout_id,
            i64::try_from(max_chunks)
                .map_err(|_| GetError::Corrupt("chunk layout bound exceeds SQLite range"))?,
        ],
        |row| row.get(0),
    )?;
    if oversized {
        return Err(GetError::Corrupt(
            "stored chunk layout exceeds the alpha cardinality bound",
        ));
    }
    Ok(())
}

struct StoredVersion {
    source_uri: String,
    title: String,
    metadata_json: String,
    source_updated_at: Option<String>,
    indexed_at: String,
    body: Vec<u8>,
    body_sha256: Vec<u8>,
    content_blob_id: i64,
}

#[derive(Clone, Copy)]
struct StoredChunk {
    ordinal: i64,
    start_byte: i64,
    end_byte: i64,
    start_line: i64,
    end_line: i64,
}

fn expand_chunk_window(
    chunks: &[StoredChunk],
    cited_index: usize,
    max_bytes: usize,
) -> Result<(usize, usize), GetError> {
    let mut left = cited_index;
    let mut right = cited_index;
    let mut left_blocked = false;
    let mut right_blocked = false;

    while !left_blocked || !right_blocked {
        if left == 0 {
            left_blocked = true;
        } else if !left_blocked {
            let candidate = left - 1;
            if window_length(chunks[candidate].start_byte, chunks[right].end_byte)? <= max_bytes {
                left = candidate;
            } else {
                left_blocked = true;
            }
        }

        if right + 1 >= chunks.len() {
            right_blocked = true;
        } else if !right_blocked {
            let candidate = right + 1;
            if window_length(chunks[left].start_byte, chunks[candidate].end_byte)? <= max_bytes {
                right = candidate;
            } else {
                right_blocked = true;
            }
        }
    }

    Ok((left, right))
}

fn window_length(start: i64, end: i64) -> Result<usize, GetError> {
    let length = end
        .checked_sub(start)
        .ok_or(GetError::Corrupt("chunk window is reversed"))?;
    usize::try_from(length).map_err(|_| GetError::Corrupt("chunk window is too large"))
}

fn line_span(chunk: &StoredChunk) -> Result<LineSpan, GetError> {
    LineSpan::new(
        u64_from_i64(chunk.start_line)?,
        u64_from_i64(chunk.end_line)?,
    )
    .map_err(|_| GetError::Corrupt("cited line span is invalid"))
}

fn digest_from_blob(bytes: &[u8]) -> Result<Sha256Digest, GetError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| GetError::Corrupt("digest length is invalid"))?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn integer_from_u64(value: u64) -> Result<i64, GetError> {
    i64::try_from(value).map_err(|_| GetError::Corrupt("span exceeds SQLite range"))
}

fn u64_from_i64(value: i64) -> Result<u64, GetError> {
    u64::try_from(value).map_err(|_| GetError::Corrupt("stored span is negative"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_layout_cardinality_is_rejected_before_collection() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE chunks (
                     chunk_layout_id INTEGER NOT NULL,
                     ordinal INTEGER NOT NULL
                 );
                 INSERT INTO chunks VALUES (7, 0), (7, 1), (7, 2);",
            )
            .unwrap();

        ensure_chunk_layout_cardinality(&connection, 7, 3).unwrap();
        assert!(matches!(
            ensure_chunk_layout_cardinality(&connection, 7, 2),
            Err(GetError::Corrupt(
                "stored chunk layout exceeds the alpha cardinality bound"
            ))
        ));
    }
}
