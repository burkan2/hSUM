//! Stable `hsum.api.v1` wire types shared by CLI JSON and MCP.

mod cursor;
mod evidence;
mod status;

pub use cursor::{
    DecodedSearchCursor, MAX_SEARCH_CURSOR_BYTES, SearchCursorError, SearchCursorStaleCause,
    SearchCursorState, decode_search_cursor, encode_search_cursor, retrieval_config_fingerprint,
    search_cursor_stale_cause,
};
pub use evidence::{
    ByteSpanOutput, CandidateCountsOutput, CliDuplicateCitationOutput, CliRankExplanationOutput,
    CliSearchOutput, CliSearchPassageOutput, CliSearchScoreOutput, CliSpanOutput,
    DuplicateCitationOutput, EvidenceFreshnessOutput, EvidenceGetOutput, EvidencePassageOutput,
    EvidenceSearchOutput, GetPacketError, LineSpanOutput, RankExplanationOutput, SearchScoreOutput,
    SearchTimingOutput,
};
pub use status::{CliStatusOutput, EvidenceStatusOutput, HealthIssueOutput, StatusProblemOutput};

pub const API_VERSION: &str = "hsum.api.v1";
