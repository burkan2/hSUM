#![no_main]

use hsum::domain::{IndexId, ProjectId};
use hsum::protocol::{
    SearchCursorState, decode_search_cursor, encode_search_cursor, retrieval_config_fingerprint,
};
use hsum::search::SearchMode;
use libfuzzer_sys::fuzz_target;

const QUERY: &str = "cursor::fuzz";

fuzz_target!(|data: &[u8]| {
    let project_id: ProjectId = "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af".parse().unwrap();
    let state = SearchCursorState {
        index_id: "018f47f0-a032-7978-a41a-10d768f60755"
            .parse::<IndexId>()
            .unwrap(),
        scope_revision: 7,
        index_epoch: 11,
        generation: Some(13),
        config_fingerprint: retrieval_config_fingerprint(),
    };
    let canonical =
        encode_search_cursor(1, project_id, QUERY, SearchMode::Lexical, false, &state).unwrap();
    let decoded = decode_search_cursor(
        Some(&canonical),
        project_id,
        QUERY,
        SearchMode::Lexical,
        false,
    )
    .unwrap();
    assert_eq!(decoded.offset, 1);
    assert_eq!(decoded.state.as_ref(), Some(&state));

    let candidate = if data.first().is_some_and(|selector| selector & 1 == 1) {
        let mut mutated = canonical.into_bytes();
        for (byte, mutation) in mutated.iter_mut().zip(&data[1..]) {
            *byte ^= *mutation;
        }
        String::from_utf8(mutated).ok()
    } else {
        std::str::from_utf8(data).ok().map(str::to_owned)
    };
    if let Some(candidate) = candidate.as_deref() {
        let _ = decode_search_cursor(
            Some(candidate),
            project_id,
            QUERY,
            SearchMode::Lexical,
            false,
        );
    }
});
