#![no_main]

use hsum::mcp::validate_frame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = validate_frame(data);
});
