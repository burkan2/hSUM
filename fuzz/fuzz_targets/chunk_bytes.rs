#![no_main]

use hsum::ingest::{ChunkKind, ChunkSettings, chunk_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let selector = data.first().copied().unwrap_or_default();
    let target = 1 + usize::from(u16::from_le_bytes([
        data.get(1).copied().unwrap_or_default(),
        data.get(2).copied().unwrap_or_default(),
    ])) % 4_096;
    let maximum = target
        + usize::from(u16::from_le_bytes([
            data.get(3).copied().unwrap_or_default(),
            data.get(4).copied().unwrap_or_default(),
        ])) % 4_096;
    let overlap = usize::from(u16::from_le_bytes([
        data.get(5).copied().unwrap_or_default(),
        data.get(6).copied().unwrap_or_default(),
    ])) % target;
    let settings = ChunkSettings::new(target, maximum, overlap).unwrap();
    let content = data.get(7..).unwrap_or_default();
    let kind = ChunkKind::ALL[usize::from(selector) % ChunkKind::ALL.len()];

    if let Ok(chunks) = chunk_bytes(content, kind, settings) {
        for (ordinal, chunk) in chunks.iter().enumerate() {
            assert_eq!(usize::try_from(chunk.ordinal()).unwrap(), ordinal);
            assert!(chunk.span().start() < chunk.span().end());
            assert!(chunk.span().end() <= content.len() as u64);
            assert!(chunk.span().end() - chunk.span().start() <= maximum as u64);
        }
    }
});
