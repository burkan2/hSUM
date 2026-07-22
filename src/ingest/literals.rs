pub const MIN_IDENTIFIER_LITERAL_BYTES: usize = 2;
pub const MAX_IDENTIFIER_LITERAL_BYTES: usize = 128;
pub const MAX_IDENTIFIER_LITERALS_PER_PASSAGE: usize = 64;

/// Extracts case-sensitive identifier-like literals in first-occurrence order.
///
/// A literal is an ASCII run made from letters, digits, and the punctuation
/// bytes `_`, `-`, `.`, `:`, and `/`. It must contain both an alphanumeric byte
/// and one of those five punctuation bytes. Overlong runs are rejected as a
/// whole rather than truncated because truncation would create postings for
/// bytes that were never an identifier in the source.
pub fn extract_identifier_literals(input: &[u8]) -> Vec<Vec<u8>> {
    let mut literals: Vec<Vec<u8>> = Vec::with_capacity(MAX_IDENTIFIER_LITERALS_PER_PASSAGE);
    let mut cursor = 0;

    while cursor < input.len() && literals.len() < MAX_IDENTIFIER_LITERALS_PER_PASSAGE {
        if !is_literal_byte(input[cursor]) {
            cursor += 1;
            continue;
        }

        let start = cursor;
        while cursor < input.len() && is_literal_byte(input[cursor]) {
            cursor += 1;
        }
        let candidate = &input[start..cursor];

        if !(MIN_IDENTIFIER_LITERAL_BYTES..=MAX_IDENTIFIER_LITERAL_BYTES).contains(&candidate.len())
            || !candidate.iter().any(u8::is_ascii_alphanumeric)
            || !candidate.iter().copied().any(is_identifier_punctuation)
            || literals
                .iter()
                .any(|literal| literal.as_slice() == candidate)
        {
            continue;
        }

        literals.push(candidate.to_vec());
    }

    literals
}

fn is_literal_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || is_identifier_punctuation(byte)
}

fn is_identifier_punctuation(byte: u8) -> bool {
    matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
}
