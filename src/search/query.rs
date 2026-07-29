use std::collections::BTreeSet;

use thiserror::Error;

pub const MAX_QUERY_BYTES: usize = 4_096;
pub const MAX_EXACT_ATOMS: usize = 16;
pub const MIN_EXACT_ATOM_BYTES: usize = 2;
pub const MAX_EXACT_ATOM_BYTES: usize = 256;
pub const MIN_PUNCTUATION_QUOTED_SPAN_BYTES: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedQuery {
    original: String,
    exact_atoms: Vec<ExactAtom>,
    quoted_spans: Vec<QuotedSpan>,
}

impl ParsedQuery {
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        parse_query(input)
    }

    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn exact_atoms(&self) -> &[ExactAtom] {
        &self.exact_atoms
    }

    pub fn quoted_spans(&self) -> &[QuotedSpan] {
        &self.quoted_spans
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactAtom {
    text: String,
    kind: ExactAtomKind,
}

impl ExactAtom {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn kind(&self) -> ExactAtomKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactAtomKind {
    Quoted,
    Identifier,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuotedSpan {
    text: String,
    start_byte: usize,
    end_byte: usize,
}

impl QuotedSpan {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the first byte of the quoted content, excluding the quote.
    pub fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Returns the exclusive end byte of the quoted content.
    pub fn end_byte(&self) -> usize {
        self.end_byte
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum QueryError {
    #[error("query must contain at least one non-whitespace character")]
    Blank,
    #[error("query is {bytes} bytes; the limit is {limit}")]
    QueryTooLong { bytes: usize, limit: usize },
    #[error("query contains NUL at byte {byte_offset}")]
    Nul { byte_offset: usize },
    #[error("query contains an unmatched quote at byte {byte_offset}")]
    UnmatchedQuote { byte_offset: usize },
    #[error("quoted span at byte {byte_offset} is empty")]
    EmptyQuotedSpan { byte_offset: usize },
    #[error("exact atom is {bytes} bytes; the minimum is {minimum}")]
    AtomTooShort { bytes: usize, minimum: usize },
    #[error("exact atom is {bytes} bytes; the limit is {limit}")]
    AtomTooLong { bytes: usize, limit: usize },
    #[error("punctuation-only quoted span is {bytes} bytes; the minimum is {minimum}")]
    PunctuationQuotedSpanTooShort { bytes: usize, minimum: usize },
    #[error("query derives {atoms} exact atoms; the limit is {limit}")]
    TooManyExactAtoms { atoms: usize, limit: usize },
}

/// Parses and validates a query without compiling or executing retrieval.
///
/// Double quotes delimit exact spans. The alpha.4 grammar intentionally has no
/// escape syntax: every `"` byte opens or closes a span. Exact atoms preserve
/// their original UTF-8 bytes and are deduplicated in first-occurrence order.
pub fn parse_query(input: &str) -> Result<ParsedQuery, QueryError> {
    if input.len() > MAX_QUERY_BYTES {
        return Err(QueryError::QueryTooLong {
            bytes: input.len(),
            limit: MAX_QUERY_BYTES,
        });
    }

    if let Some(byte_offset) = input.as_bytes().iter().position(|byte| *byte == 0) {
        return Err(QueryError::Nul { byte_offset });
    }

    if input.trim().is_empty() {
        return Err(QueryError::Blank);
    }

    let (quoted_spans, mut candidates) = parse_quoted_spans(input)?;
    candidates.extend(parse_identifier_candidates(input)?);
    candidates.sort_by_key(|candidate| candidate.start_byte);

    let mut seen = BTreeSet::new();
    let mut exact_atoms = Vec::new();
    for candidate in candidates {
        if seen.insert(candidate.text.clone()) {
            exact_atoms.push(ExactAtom {
                text: candidate.text,
                kind: candidate.kind,
            });
        }
    }

    if exact_atoms.len() > MAX_EXACT_ATOMS {
        return Err(QueryError::TooManyExactAtoms {
            atoms: exact_atoms.len(),
            limit: MAX_EXACT_ATOMS,
        });
    }

    Ok(ParsedQuery {
        original: input.to_owned(),
        exact_atoms,
        quoted_spans,
    })
}

#[derive(Debug)]
struct AtomCandidate {
    text: String,
    kind: ExactAtomKind,
    start_byte: usize,
}

fn parse_quoted_spans(input: &str) -> Result<(Vec<QuotedSpan>, Vec<AtomCandidate>), QueryError> {
    let mut opening_quote = None;
    let mut quoted_spans = Vec::new();
    let mut candidates = Vec::new();

    for (byte_offset, byte) in input.bytes().enumerate() {
        if byte != b'"' {
            continue;
        }

        let Some(opening_byte) = opening_quote.take() else {
            opening_quote = Some(byte_offset);
            continue;
        };

        let start_byte = opening_byte + 1;
        let text = &input[start_byte..byte_offset];
        validate_quoted_atom(text, opening_byte)?;

        quoted_spans.push(QuotedSpan {
            text: text.to_owned(),
            start_byte,
            end_byte: byte_offset,
        });
        candidates.push(AtomCandidate {
            text: text.to_owned(),
            kind: ExactAtomKind::Quoted,
            start_byte,
        });
    }

    if let Some(byte_offset) = opening_quote {
        return Err(QueryError::UnmatchedQuote { byte_offset });
    }

    Ok((quoted_spans, candidates))
}

fn validate_quoted_atom(text: &str, opening_byte: usize) -> Result<(), QueryError> {
    if text.is_empty() || text.chars().all(char::is_whitespace) {
        return Err(QueryError::EmptyQuotedSpan {
            byte_offset: opening_byte,
        });
    }

    if text.len() > MAX_EXACT_ATOM_BYTES {
        return Err(QueryError::AtomTooLong {
            bytes: text.len(),
            limit: MAX_EXACT_ATOM_BYTES,
        });
    }

    let punctuation_only = text
        .chars()
        .all(|character| !character.is_alphanumeric() && !character.is_whitespace());
    if punctuation_only && text.len() < MIN_PUNCTUATION_QUOTED_SPAN_BYTES {
        return Err(QueryError::PunctuationQuotedSpanTooShort {
            bytes: text.len(),
            minimum: MIN_PUNCTUATION_QUOTED_SPAN_BYTES,
        });
    }

    if text.len() < MIN_EXACT_ATOM_BYTES {
        return Err(QueryError::AtomTooShort {
            bytes: text.len(),
            minimum: MIN_EXACT_ATOM_BYTES,
        });
    }

    Ok(())
}

fn parse_identifier_candidates(input: &str) -> Result<Vec<AtomCandidate>, QueryError> {
    let bytes = input.as_bytes();
    let mut candidates = Vec::new();
    let mut cursor = 0;
    let mut inside_quote = false;

    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            inside_quote = !inside_quote;
            cursor += 1;
            continue;
        }
        if inside_quote || !is_identifier_byte(bytes[cursor]) {
            cursor += 1;
            continue;
        }

        let start_byte = cursor;
        while cursor < bytes.len() && is_identifier_byte(bytes[cursor]) {
            cursor += 1;
        }
        let candidate = &input[start_byte..cursor];

        if !candidate.bytes().any(|byte| byte.is_ascii_alphanumeric())
            || !candidate.bytes().any(is_identifier_punctuation)
        {
            continue;
        }
        if candidate.len() > MAX_EXACT_ATOM_BYTES {
            return Err(QueryError::AtomTooLong {
                bytes: candidate.len(),
                limit: MAX_EXACT_ATOM_BYTES,
            });
        }
        if candidate.len() < MIN_EXACT_ATOM_BYTES {
            return Err(QueryError::AtomTooShort {
                bytes: candidate.len(),
                minimum: MIN_EXACT_ATOM_BYTES,
            });
        }

        candidates.push(AtomCandidate {
            text: candidate.to_owned(),
            kind: ExactAtomKind::Identifier,
            start_byte,
        });
    }

    Ok(candidates)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || is_identifier_punctuation(byte)
}

fn is_identifier_punctuation(byte: u8) -> bool {
    matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
}
