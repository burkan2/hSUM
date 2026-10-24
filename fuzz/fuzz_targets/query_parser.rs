#![no_main]

use hsum::search::query::parse_query;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(parsed) = parse_query(input) {
        assert_eq!(parsed.original(), input);
        for span in parsed.quoted_spans() {
            assert!(span.start_byte() <= span.end_byte());
            assert!(span.end_byte() <= input.len());
            assert_eq!(&input[span.start_byte()..span.end_byte()], span.text());
        }
    }
});
