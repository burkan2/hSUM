#![no_main]

use std::collections::BTreeSet;

use hsum::ingest::{JsonlDeletion, JsonlRecord, parse_jsonl_snapshot};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(snapshot) = parse_jsonl_snapshot(data) {
        let ids = snapshot
            .records()
            .iter()
            .map(JsonlRecord::id)
            .chain(snapshot.deletions().iter().map(JsonlDeletion::id))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids.len(),
            snapshot.records().len() + snapshot.deletions().len()
        );
    }
});
