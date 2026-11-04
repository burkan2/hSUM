#![no_main]

use hsum::domain::Citation;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    for candidate in [Some(input), input.strip_suffix('\n')]
        .into_iter()
        .flatten()
    {
        if let Ok(citation) = candidate.parse::<Citation>() {
            let canonical = citation.to_string();
            assert_eq!(canonical, candidate);
            assert_eq!(canonical.parse::<Citation>().unwrap(), citation);
        }
    }
});
