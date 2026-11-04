//! Restricted counter for the audited FTS5 single-byte position-list case.

pub(super) const SOURCE_ID: &[u8] =
    b"2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6d618";

pub(super) fn source_matches(source: &[u8]) -> bool {
    cfg!(hsum_verified_sqlite_positions) && source == SOURCE_ID
}

pub(super) fn safe_tail_bound(initial_offset: i32, bytes: usize) -> bool {
    initial_offset >= 0 && bytes <= ((i32::MAX - initial_offset) / 125) as usize
}

// This checks arithmetic only; allocation/lifetime validity comes from the
// independently gated SQLite implementation, not from integer addresses.
pub(super) fn tail_length(start: usize, end: usize, initial_offset: i32) -> Option<usize> {
    if start == 0 || end == 0 {
        return None;
    }
    let length = end.checked_sub(start)?;
    safe_tail_bound(initial_offset, length).then_some(length)
}

/// xPhraseFirst has consumed one occurrence. A tail consisting entirely of
/// bytes 2..=127 contains only one-byte positive position deltas. Column
/// markers and every multibyte encoding use the native iterator instead.
pub(super) fn single_byte_frequency(
    bytes: &[u8],
    initial_offset: i32,
    mut poll: impl FnMut() -> Result<(), i32>,
) -> Result<Option<u64>, i32> {
    poll()?;
    if !safe_tail_bound(initial_offset, bytes.len()) {
        return Ok(None);
    }
    for (index, block) in bytes.chunks(256).enumerate() {
        if index != 0 {
            poll()?;
        }
        // No early exit inside the bounded block: LLVM can vectorize this
        // validation without interpreting any unsupported position encoding.
        let mut invalid = 0_u8;
        for &byte in block {
            invalid |= byte & 0x80;
            invalid |= u8::from(byte < 2);
        }
        if invalid != 0 {
            return Ok(None);
        }
    }
    Ok(Some(bytes.len() as u64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn unknown_sqlite_source_never_enables_the_fast_path() {
        assert!(!source_matches(b"3.51.3"));
        assert!(!source_matches(b""));
        let mut altered = SOURCE_ID.to_vec();
        altered[0] ^= 1;
        assert!(!source_matches(&altered));
        assert_eq!(
            source_matches(SOURCE_ID),
            cfg!(hsum_verified_sqlite_positions)
        );
    }

    #[test]
    fn null_reversed_and_oversized_ranges_are_rejected_before_borrowing() {
        assert_eq!(tail_length(0, 0, 0), None);
        assert_eq!(tail_length(0, 1, 0), None);
        assert_eq!(tail_length(1, 0, 0), None);
        assert_eq!(tail_length(2, 1, 0), None);
        assert_eq!(tail_length(1, usize::MAX, 0), None);
        assert_eq!(tail_length(1, 1, 0), Some(0));
        assert_eq!(tail_length(1, 2, 0), Some(1));
        assert_eq!(tail_length(1, 2, i32::MAX), None);
    }

    #[test]
    fn empty_and_single_byte_tails_count_the_already_consumed_first_hit() {
        assert_eq!(single_byte_frequency(&[], 0, || Ok(())), Ok(Some(1)));
        assert_eq!(
            single_byte_frequency(&[2, 3, 127], 0, || Ok(())),
            Ok(Some(4))
        );
    }

    #[test]
    fn every_unsupported_byte_and_offset_bound_uses_native_fallback() {
        for byte in [0, 1, 128, 129, 255] {
            assert_eq!(single_byte_frequency(&[2, byte, 3], 0, || Ok(())), Ok(None));
        }
        assert!(!safe_tail_bound(-1, 0));
        assert!(safe_tail_bound(i32::MAX, 0));
        assert!(!safe_tail_bound(i32::MAX, 1));
        assert!(safe_tail_bound(i32::MAX - 125, 1));
        assert!(!safe_tail_bound(i32::MAX - 124, 1));
        assert!(!safe_tail_bound(0, usize::MAX));
    }

    #[test]
    fn cancellation_is_checked_before_empty_tails_and_every_256_bytes() {
        assert_eq!(single_byte_frequency(&[], 0, || Err(9)), Err(9));
        let mut polls = 0;
        let result = single_byte_frequency(&[2; 1024], 0, || {
            polls += 1;
            if polls == 3 { Err(9) } else { Ok(()) }
        });
        assert_eq!(result, Err(9));
        assert_eq!(polls, 3);
    }

    proptest! {
        #[test]
        fn all_supported_bytes_count_exactly_once(bytes in prop::collection::vec(2_u8..=127, 0..4096)) {
            prop_assert_eq!(single_byte_frequency(&bytes, 0, || Ok(())), Ok(Some(bytes.len() as u64 + 1)));
        }
    }
}
