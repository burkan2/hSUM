use hsum::ingest::{
    MAX_IDENTIFIER_LITERAL_BYTES, MAX_IDENTIFIER_LITERALS_PER_PASSAGE,
    MIN_IDENTIFIER_LITERAL_BYTES, extract_identifier_literals,
};
use proptest::prelude::*;

fn bytes(values: &[&str]) -> Vec<Vec<u8>> {
    values
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect()
}

fn reference_extract(input: &[u8]) -> Vec<Vec<u8>> {
    let mut literals = Vec::new();

    for candidate in input.split(|byte| {
        !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/'))
    }) {
        if literals.len() == MAX_IDENTIFIER_LITERALS_PER_PASSAGE {
            break;
        }
        if !(MIN_IDENTIFIER_LITERAL_BYTES..=MAX_IDENTIFIER_LITERAL_BYTES).contains(&candidate.len())
            || !candidate.iter().any(u8::is_ascii_alphanumeric)
            || !candidate
                .iter()
                .copied()
                .any(|byte| matches!(byte, b'_' | b'-' | b'.' | b':' | b'/'))
            || literals
                .iter()
                .any(|literal: &Vec<u8>| literal.as_slice() == candidate)
        {
            continue;
        }
        literals.push(candidate.to_vec());
    }

    literals
}

#[test]
fn extracts_only_case_sensitive_identifier_like_ascii_runs() {
    let input = b"HTTPServer::handle_request repo://src/lib.rs snake_case \
                  kebab-case dot.name key:value plain + also@plain";

    assert_eq!(
        extract_identifier_literals(input),
        bytes(&[
            "HTTPServer::handle_request",
            "repo://src/lib.rs",
            "snake_case",
            "kebab-case",
            "dot.name",
            "key:value",
        ])
    );
}

#[test]
fn preserves_first_occurrence_order_and_deduplicates_exact_bytes() {
    let input = b"Alpha_Beta alpha_beta Alpha_Beta path/to alpha_beta";

    assert_eq!(
        extract_identifier_literals(input),
        bytes(&["Alpha_Beta", "alpha_beta", "path/to"])
    );
}

#[test]
fn rejects_literals_outside_the_inclusive_byte_bounds_without_truncating() {
    let minimum = b"a_";
    let mut maximum = b"x_".to_vec();
    maximum.extend(std::iter::repeat_n(b'a', MAX_IDENTIFIER_LITERAL_BYTES - 2));
    let mut oversized = b"y_".to_vec();
    oversized.extend(std::iter::repeat_n(b'b', MAX_IDENTIFIER_LITERAL_BYTES - 1));

    let mut input = b"_ ".to_vec();
    input.extend_from_slice(minimum);
    input.push(b' ');
    input.extend_from_slice(&maximum);
    input.push(b' ');
    input.extend_from_slice(&oversized);

    let literals = extract_identifier_literals(&input);
    assert_eq!(literals, vec![minimum.to_vec(), maximum]);
    assert_eq!(literals[0].len(), MIN_IDENTIFIER_LITERAL_BYTES);
    assert_eq!(literals[1].len(), MAX_IDENTIFIER_LITERAL_BYTES);
}

#[test]
fn caps_each_passage_at_the_first_sixty_four_unique_literals() {
    let input = (0..MAX_IDENTIFIER_LITERALS_PER_PASSAGE + 5)
        .map(|number| format!("item_{number:02}"))
        .collect::<Vec<_>>()
        .join(" ");

    let literals = extract_identifier_literals(input.as_bytes());
    assert_eq!(literals.len(), MAX_IDENTIFIER_LITERALS_PER_PASSAGE);
    assert_eq!(literals.first(), Some(&b"item_00".to_vec()));
    assert_eq!(literals.last(), Some(&b"item_63".to_vec()));
}

proptest! {
    #[test]
    fn extraction_is_deterministic_bounded_and_uses_only_the_frozen_alphabet(
        input in proptest::collection::vec(any::<u8>(), 0..4096)
    ) {
        let first = extract_identifier_literals(&input);
        let second = extract_identifier_literals(&input);
        let reference = reference_extract(&input);

        prop_assert_eq!(&first, &second);
        prop_assert_eq!(&first, &reference);
        prop_assert!(first.len() <= MAX_IDENTIFIER_LITERALS_PER_PASSAGE);

        for literal in &first {
            prop_assert!(
                (MIN_IDENTIFIER_LITERAL_BYTES..=MAX_IDENTIFIER_LITERAL_BYTES)
                    .contains(&literal.len())
            );
            let uses_only_frozen_alphabet = literal.iter().copied().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
            });
            prop_assert!(uses_only_frozen_alphabet);
            prop_assert!(literal.iter().any(u8::is_ascii_alphanumeric));
            prop_assert!(literal
                .iter()
                .copied()
                .any(|byte| matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')));
        }

        for (index, literal) in first.iter().enumerate() {
            prop_assert!(!first[..index].contains(literal));
        }
    }
}
