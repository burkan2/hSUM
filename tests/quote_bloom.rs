use hsum::ingest::{QUOTE_BLOOM_BITS, QUOTE_BLOOM_BYTES, QUOTE_BLOOM_HASHES, QuoteBloom};
use proptest::prelude::*;

#[test]
fn frozen_abc_fixture_commits_hash_endianness_double_hashing_and_bit_order() {
    let bloom = QuoteBloom::from_content(b"abc");
    let bytes = bloom.as_bytes();

    assert_eq!(QUOTE_BLOOM_BITS, 4096);
    assert_eq!(QUOTE_BLOOM_BYTES, 512);
    assert_eq!(QUOTE_BLOOM_HASHES, 4);
    assert_eq!(bytes.len(), QUOTE_BLOOM_BYTES);
    assert_eq!(bytes.iter().map(|byte| byte.count_ones()).sum::<u32>(), 4);
    assert_eq!(bytes[207], 0x20);
    assert_eq!(bytes[210], 0x08);
    assert_eq!(bytes[213], 0x02);
    assert_eq!(bytes[215], 0x80);
    assert!(
        bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 207 | 210 | 213 | 215) || *byte == 0)
    );
}

#[test]
fn every_raw_byte_quote_from_the_content_survives_candidate_pruning() {
    let content = b"\xef\xbb\xbfAlpha::Beta\r\ncaf\xc3\xa9/path.rs\0tail";
    let bloom = QuoteBloom::from_content(content);

    for start in 0..content.len() {
        for end in start + 3..=content.len() {
            assert!(
                bloom.might_contain(&content[start..end]),
                "false negative for byte range {start}..{end}"
            );
        }
    }
}

#[test]
fn content_and_queries_shorter_than_a_trigram_are_vacuous() {
    assert_eq!(
        QuoteBloom::from_content(b"ab").as_bytes(),
        &[0; QUOTE_BLOOM_BYTES]
    );

    let bloom = QuoteBloom::from_content(b"unrelated content");
    assert!(bloom.might_contain(b""));
    assert!(bloom.might_contain(b"a"));
    assert!(bloom.might_contain(b"ab"));
}

#[test]
fn construction_is_deterministic_and_byte_sensitive() {
    let first = QuoteBloom::from_content(b"Alpha::Beta");
    let second = QuoteBloom::from_content(b"Alpha::Beta");
    let different_case = QuoteBloom::from_content(b"alpha::beta");

    assert_eq!(first, second);
    assert_ne!(first, different_case);
    assert_eq!(QuoteBloom::from_bytes(first.clone().into_bytes()), first);
}

proptest! {
    #[test]
    fn every_inserted_trigram_is_a_possible_member(
        content in proptest::collection::vec(any::<u8>(), 0..2048)
    ) {
        let bloom = QuoteBloom::from_content(&content);

        for trigram in content.windows(3) {
            prop_assert!(bloom.might_contain(trigram));
        }
    }

    #[test]
    fn every_contained_byte_slice_survives_pruning(
        content in proptest::collection::vec(any::<u8>(), 0..2048),
        start_seed in any::<usize>(),
        length_seed in any::<usize>(),
    ) {
        let start = start_seed % (content.len() + 1);
        let length = length_seed % (content.len() - start + 1);
        let query = &content[start..start + length];

        prop_assert!(QuoteBloom::from_content(&content).might_contain(query));
    }
}
