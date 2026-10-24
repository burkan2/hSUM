use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::domain::{IndexId, ProjectId};
use crate::protocol::API_VERSION;
use crate::search::{MAX_SEARCH_LIMIT, SearchMode, SearchResponse};

pub const MAX_SEARCH_CURSOR_BYTES: usize = 256;

const CURSOR_VERSION: u8 = 1;
const CHECKSUM_OFFSET: usize = 107;
const PAYLOAD_BYTES: usize = 139;
// This historical domain separator is frozen so alpha.4 MCP cursors remain
// valid now that the same hsum.api.v1 cursor is also accepted by the CLI.
const CURSOR_CHECKSUM_DOMAIN: &[u8] = b"hsum.mcp.cursor.v1";
const RETRIEVAL_CONFIG_DESCRIPTOR: &str = concat!(
    "hsum.retrieval-config.v1\n",
    "modes=auto,lexical,hybrid,semantic:auto-capability-aware\n",
    "retrievers=identifier-literal,quoted-byte-bloom-aho,fts5-bm25,sqlite-vec-cosine\n",
    "candidates=initial-50:step-50:max-per-list-500:max-results-50\n",
    "fusion=integer-rrf:k-60:scale-1000000000000:exact-weight-3/2:lexical-weight-1:vector-weight-1\n",
    "rounding=half-up\n",
    "dedupe=same-content-sha256-or-same-document-overlap-at-least-half\n",
    "order=fusion-desc,matched-exact-bytes-desc,source-id,document-id,start-byte\n",
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCursorState {
    pub index_id: IndexId,
    pub scope_revision: u64,
    pub index_epoch: u64,
    pub generation: Option<i64>,
    pub config_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedSearchCursor {
    pub offset: usize,
    pub state: Option<SearchCursorState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCursorStaleCause {
    IndexEpoch,
    Generation,
    ScopeRevision,
    QueryFingerprint,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SearchCursorError {
    #[error("the search cursor is malformed")]
    Malformed,
    #[error("the search cursor is bound to different query state")]
    QueryFingerprint,
    #[error("the retrieval configuration fingerprint is invalid")]
    InvalidConfiguration,
}

pub fn retrieval_config_fingerprint() -> String {
    let descriptor = format!(
        "{RETRIEVAL_CONFIG_DESCRIPTOR}binary-version={}\npipeline-fingerprint={}\n",
        env!("CARGO_PKG_VERSION"),
        crate::store::pipeline_fingerprint(),
    );
    hex::encode(Sha256::digest(descriptor.as_bytes()))
}

pub fn retrieval_execution_fingerprint(response: &SearchResponse) -> String {
    let mut hasher = Sha256::new();
    for value in [
        b"hsum.retrieval-execution.v1".as_slice(),
        retrieval_config_fingerprint().as_bytes(),
        response.effective_mode.as_str().as_bytes(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    hasher.update((response.retrievers.len() as u64).to_be_bytes());
    for retriever in &response.retrievers {
        update_framed(&mut hasher, retriever.as_str().as_bytes());
    }
    for values in [&response.degraded_mode, &response.hints] {
        hasher.update((values.len() as u64).to_be_bytes());
        for value in values {
            update_framed(&mut hasher, value.as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

fn update_framed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub fn encode_search_cursor(
    offset: usize,
    project_id: ProjectId,
    query: &str,
    mode: SearchMode,
    explain: bool,
    state: &SearchCursorState,
) -> Result<String, SearchCursorError> {
    if !(1..MAX_SEARCH_LIMIT).contains(&offset) {
        return Err(SearchCursorError::Malformed);
    }
    let offset = u8::try_from(offset).map_err(|_| SearchCursorError::Malformed)?;
    let config_fingerprint = hex::decode(&state.config_fingerprint)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or(SearchCursorError::InvalidConfiguration)?;

    let mut payload = Vec::with_capacity(PAYLOAD_BYTES);
    payload.push(CURSOR_VERSION);
    payload.push(offset);
    payload.extend_from_slice(state.index_id.as_uuid().as_bytes());
    payload.extend_from_slice(&state.scope_revision.to_be_bytes());
    payload.extend_from_slice(&state.index_epoch.to_be_bytes());
    match state.generation {
        Some(generation) => {
            payload.push(1);
            payload.extend_from_slice(&generation.to_be_bytes());
        }
        None => {
            payload.push(0);
            payload.extend_from_slice(&0_i64.to_be_bytes());
        }
    }
    payload.extend_from_slice(&config_fingerprint);
    payload.extend_from_slice(&cursor_query_fingerprint(project_id, query, mode, explain));
    debug_assert_eq!(payload.len(), CHECKSUM_OFFSET);
    let mut checksum = Sha256::new();
    checksum.update(CURSOR_CHECKSUM_DOMAIN);
    checksum.update(&payload);
    payload.extend_from_slice(&checksum.finalize());
    Ok(format!("v1.{}", base64url_encode(&payload)))
}

pub fn decode_search_cursor(
    cursor: Option<&str>,
    project_id: ProjectId,
    query: &str,
    mode: SearchMode,
    explain: bool,
) -> Result<DecodedSearchCursor, SearchCursorError> {
    let Some(cursor) = cursor else {
        return Ok(DecodedSearchCursor {
            offset: 0,
            state: None,
        });
    };
    let encoded = cursor
        .strip_prefix("v1.")
        .filter(|value| cursor.len() <= MAX_SEARCH_CURSOR_BYTES && !value.is_empty())
        .ok_or(SearchCursorError::Malformed)?;
    let payload = base64url_decode(encoded).ok_or(SearchCursorError::Malformed)?;
    if payload.len() != PAYLOAD_BYTES || payload[0] != CURSOR_VERSION {
        return Err(SearchCursorError::Malformed);
    }
    let mut checksum = Sha256::new();
    checksum.update(CURSOR_CHECKSUM_DOMAIN);
    checksum.update(&payload[..CHECKSUM_OFFSET]);
    if checksum.finalize().as_slice() != &payload[CHECKSUM_OFFSET..] {
        return Err(SearchCursorError::Malformed);
    }

    let offset = usize::from(payload[1]);
    if !(1..MAX_SEARCH_LIMIT).contains(&offset) {
        return Err(SearchCursorError::Malformed);
    }
    let index_id = uuid::Uuid::from_slice(&payload[2..18])
        .map(IndexId::from_uuid)
        .map_err(|_| SearchCursorError::Malformed)?;
    let scope_revision = u64::from_be_bytes(
        payload[18..26]
            .try_into()
            .map_err(|_| SearchCursorError::Malformed)?,
    );
    let index_epoch = u64::from_be_bytes(
        payload[26..34]
            .try_into()
            .map_err(|_| SearchCursorError::Malformed)?,
    );
    let generation_bytes: [u8; 8] = payload[35..43]
        .try_into()
        .map_err(|_| SearchCursorError::Malformed)?;
    let generation = match payload[34] {
        0 if generation_bytes == [0; 8] => None,
        1 => {
            let value = i64::from_be_bytes(generation_bytes);
            Some(
                (value > 0)
                    .then_some(value)
                    .ok_or(SearchCursorError::Malformed)?,
            )
        }
        _ => return Err(SearchCursorError::Malformed),
    };
    let config_fingerprint = hex::encode(&payload[43..75]);
    let expected_query = cursor_query_fingerprint(project_id, query, mode, explain);
    if payload[75..107] != expected_query {
        return Err(SearchCursorError::QueryFingerprint);
    }
    Ok(DecodedSearchCursor {
        offset,
        state: Some(SearchCursorState {
            index_id,
            scope_revision,
            index_epoch,
            generation,
            config_fingerprint,
        }),
    })
}

pub fn search_cursor_stale_cause(
    expected: &SearchCursorState,
    actual: &SearchCursorState,
) -> SearchCursorStaleCause {
    if expected.index_id != actual.index_id || expected.index_epoch != actual.index_epoch {
        SearchCursorStaleCause::IndexEpoch
    } else if expected.generation != actual.generation {
        SearchCursorStaleCause::Generation
    } else if expected.scope_revision != actual.scope_revision {
        SearchCursorStaleCause::ScopeRevision
    } else {
        SearchCursorStaleCause::QueryFingerprint
    }
}

fn cursor_query_fingerprint(
    project_id: ProjectId,
    query: &str,
    mode: SearchMode,
    explain: bool,
) -> [u8; 32] {
    let explain = if explain { "1" } else { "0" };
    let mut hasher = Sha256::new();
    for value in [
        API_VERSION.as_bytes(),
        project_id.as_uuid().as_bytes().as_slice(),
        query.as_bytes(),
        mode.as_str().as_bytes(),
        explain.as_bytes(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    hasher.finalize().into()
}

fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(ALPHABET[usize::from(first >> 2)]));
        output.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            output.push(char::from(
                ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))],
            ));
        }
        if chunk.len() > 2 {
            output.push(char::from(ALPHABET[usize::from(third & 0x3f)]));
        }
    }
    output
}

fn base64url_decode(value: &str) -> Option<Vec<u8>> {
    if value.len() % 4 == 1 {
        return None;
    }
    let mut sextets = Vec::with_capacity(value.len());
    for byte in value.bytes() {
        sextets.push(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        });
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3 + 2);
    for chunk in sextets.chunks(4) {
        output.push((chunk[0] << 2) | (chunk[1] >> 4));
        if chunk.len() > 2 {
            output.push((chunk[1] << 4) | (chunk[2] >> 2));
        } else if chunk[1] & 0x0f != 0 {
            return None;
        }
        if chunk.len() > 3 {
            output.push((chunk[2] << 6) | chunk[3]);
        } else if chunk.len() == 3 && chunk[2] & 0x03 != 0 {
            return None;
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af";
    const PROJECT_UUID: &str = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a4402";
    const CONFIG_FINGERPRINT: &str =
        "ae2ba062b90b8f86574eb0d335fb2a416c05e3ebe49845823a52e9cc0a546c30";
    const FROZEN_CURSOR: &str = "v1.AQcBj0fwnZp6Y7TMjW8sikSvAAAAAAAAAAEAAAAAAAAAAQEAAAAAAAAAAa4roGK5C4-GV06w0zX7KkFsBePr5JhFgjpS6cwKVGwwm-1Qpc6OyJ47tNue0cMtssLWQ7kP8W3RfmMO8ttoPfvqihP0gAz2zy98OEVQrJ6EkKIOnOjMILyCRvkW6KRNzA";

    #[test]
    fn frozen_alpha_four_cursor_bytes_remain_compatible() {
        let project_id = ProjectId::from_uuid(uuid::Uuid::parse_str(PROJECT_UUID).unwrap());
        let state = SearchCursorState {
            index_id: IndexId::from_uuid(uuid::Uuid::parse_str(INDEX_UUID).unwrap()),
            scope_revision: 1,
            index_epoch: 1,
            generation: Some(1),
            config_fingerprint: CONFIG_FINGERPRINT.to_owned(),
        };
        assert_eq!(
            encode_search_cursor(7, project_id, "alpha", SearchMode::Auto, true, &state).unwrap(),
            FROZEN_CURSOR
        );
        assert_eq!(
            decode_search_cursor(
                Some(FROZEN_CURSOR),
                project_id,
                "alpha",
                SearchMode::Auto,
                true,
            )
            .unwrap(),
            DecodedSearchCursor {
                offset: 7,
                state: Some(state),
            }
        );
    }

    #[test]
    fn cursor_integrity_and_query_shape_fail_distinctly() {
        let project_id = ProjectId::from_uuid(uuid::Uuid::parse_str(PROJECT_UUID).unwrap());
        let mut tampered = FROZEN_CURSOR.as_bytes().to_vec();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert_eq!(
            decode_search_cursor(
                Some(std::str::from_utf8(&tampered).unwrap()),
                project_id,
                "alpha",
                SearchMode::Auto,
                true,
            ),
            Err(SearchCursorError::Malformed)
        );
        assert_eq!(
            decode_search_cursor(
                Some(FROZEN_CURSOR),
                project_id,
                "alpha",
                SearchMode::Lexical,
                true,
            ),
            Err(SearchCursorError::QueryFingerprint)
        );
    }

    #[test]
    fn execution_fingerprint_binds_effective_retrievers_and_degradation() {
        let mut response = SearchResponse {
            project_id: ProjectId::from_uuid(uuid::Uuid::parse_str(PROJECT_UUID).unwrap()),
            scope_revision: 1,
            generation: Some(1),
            index_epoch: 1,
            requested_mode: SearchMode::Auto,
            effective_mode: SearchMode::Lexical,
            retrievers: vec![
                crate::search::Retriever::Exact,
                crate::search::Retriever::Lexical,
            ],
            degraded_mode: Vec::new(),
            hints: Vec::new(),
            results: Vec::new(),
            stop_reason: crate::search::SearchStopReason::UniqueExhausted,
            examined: crate::search::CandidateCounts::default(),
            timing: crate::search::SearchTiming::default(),
        };
        let lexical = retrieval_execution_fingerprint(&response);
        response.effective_mode = SearchMode::Hybrid;
        response.retrievers.push(crate::search::Retriever::Vector);
        assert_ne!(retrieval_execution_fingerprint(&response), lexical);
        response.effective_mode = SearchMode::Lexical;
        response.retrievers.pop();
        response.hints.push("query_embedding_busy".to_owned());
        assert_ne!(retrieval_execution_fingerprint(&response), lexical);
    }
}
