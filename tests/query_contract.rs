use hsum::search::query::{
    ExactAtomKind, MAX_EXACT_ATOM_BYTES, MAX_EXACT_ATOMS, MAX_QUERY_BYTES, QueryError, parse_query,
};

#[test]
fn rejects_blank_nul_empty_quotes_and_unmatched_quotes() {
    assert_eq!(parse_query(""), Err(QueryError::Blank));
    assert_eq!(parse_query(" \n\t"), Err(QueryError::Blank));
    assert_eq!(
        parse_query("alpha\0beta"),
        Err(QueryError::Nul { byte_offset: 5 })
    );
    assert_eq!(
        parse_query("alpha \"beta"),
        Err(QueryError::UnmatchedQuote { byte_offset: 6 })
    );
    assert_eq!(
        parse_query("alpha \"\""),
        Err(QueryError::EmptyQuotedSpan { byte_offset: 6 })
    );
    assert_eq!(
        parse_query("alpha \" \t\""),
        Err(QueryError::EmptyQuotedSpan { byte_offset: 6 })
    );
}

#[test]
fn enforces_the_original_utf8_byte_budget() {
    let exact_limit = "!".repeat(MAX_QUERY_BYTES);
    assert!(parse_query(&exact_limit).is_ok());

    let over_limit = format!("{}é", "!".repeat(MAX_QUERY_BYTES - 1));
    assert_eq!(
        parse_query(&over_limit),
        Err(QueryError::QueryTooLong {
            bytes: MAX_QUERY_BYTES + 1,
            limit: MAX_QUERY_BYTES,
        })
    );
}

#[test]
fn quoted_atom_lengths_are_measured_in_utf8_bytes_without_truncation() {
    assert_eq!(
        parse_query("\"a\""),
        Err(QueryError::AtomTooShort {
            bytes: 1,
            minimum: 2,
        })
    );

    let unicode = parse_query("\"é\"").expect("two UTF-8 bytes are valid");
    assert_eq!(unicode.exact_atoms()[0].text(), "é");

    let exact_limit = format!("\"{}\"", "a".repeat(MAX_EXACT_ATOM_BYTES));
    assert!(parse_query(&exact_limit).is_ok());

    let over_limit = format!("\"{}\"", "a".repeat(MAX_EXACT_ATOM_BYTES + 1));
    assert_eq!(
        parse_query(&over_limit),
        Err(QueryError::AtomTooLong {
            bytes: MAX_EXACT_ATOM_BYTES + 1,
            limit: MAX_EXACT_ATOM_BYTES,
        })
    );
}

#[test]
fn punctuation_only_quotes_need_at_least_three_bytes() {
    assert_eq!(
        parse_query("\"!!\""),
        Err(QueryError::PunctuationQuotedSpanTooShort {
            bytes: 2,
            minimum: 3,
        })
    );

    let parsed = parse_query("\"!!!\"").expect("three punctuation bytes are valid");
    assert_eq!(parsed.exact_atoms()[0].text(), "!!!");
    assert_eq!(parsed.exact_atoms()[0].kind(), ExactAtomKind::Quoted);
}

#[test]
fn unquoted_punctuation_only_input_is_valid_and_has_no_exact_atoms() {
    let parsed = parse_query("!@#$%^&*()").expect("valid query may produce no results");
    assert_eq!(parsed.original(), "!@#$%^&*()");
    assert!(parsed.exact_atoms().is_empty());
    assert!(parsed.quoted_spans().is_empty());
}

#[test]
fn extracts_quotes_and_identifier_tokens_in_first_occurrence_order() {
    let parsed =
        parse_query("plain foo_bar \"quoted span\" x/y foo_bar \"quoted span\" a.b:c/d suffix")
            .expect("query is valid");

    let atoms: Vec<_> = parsed
        .exact_atoms()
        .iter()
        .map(|atom| (atom.text(), atom.kind()))
        .collect();
    assert_eq!(
        atoms,
        vec![
            ("foo_bar", ExactAtomKind::Identifier),
            ("quoted span", ExactAtomKind::Quoted),
            ("x/y", ExactAtomKind::Identifier),
            ("a.b:c/d", ExactAtomKind::Identifier),
        ]
    );

    let spans: Vec<_> = parsed
        .quoted_spans()
        .iter()
        .map(|span| span.text())
        .collect();
    assert_eq!(spans, vec!["quoted span", "quoted span"]);
}

#[test]
fn quoted_identifiers_are_not_also_derived_as_identifier_atoms() {
    let parsed = parse_query("\"foo_bar\"").expect("query is valid");
    assert_eq!(parsed.exact_atoms().len(), 1);
    assert_eq!(parsed.exact_atoms()[0].text(), "foo_bar");
    assert_eq!(parsed.exact_atoms()[0].kind(), ExactAtomKind::Quoted);
}

#[test]
fn quoted_span_offsets_are_half_open_original_byte_offsets() {
    let parsed = parse_query("é \"beta\"").expect("query is valid");
    let span = &parsed.quoted_spans()[0];
    assert_eq!(span.start_byte(), 4);
    assert_eq!(span.end_byte(), 8);
    assert_eq!(&parsed.original().as_bytes()[4..8], b"beta");
}

#[test]
fn duplicate_atoms_do_not_consume_the_sixteen_atom_budget() {
    let sixteen = (0..MAX_EXACT_ATOMS)
        .map(|index| format!("name_{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let with_duplicates = format!("{sixteen} name_0 \"name_1\"");
    let parsed = parse_query(&with_duplicates).expect("duplicates are derived once");
    assert_eq!(parsed.exact_atoms().len(), MAX_EXACT_ATOMS);

    let seventeen = format!("{sixteen} name_{}", MAX_EXACT_ATOMS);
    assert_eq!(
        parse_query(&seventeen),
        Err(QueryError::TooManyExactAtoms {
            atoms: MAX_EXACT_ATOMS + 1,
            limit: MAX_EXACT_ATOMS,
        })
    );
}

#[test]
fn overlong_identifier_atom_is_rejected_as_a_whole() {
    let identifier = format!("a_{}", "b".repeat(MAX_EXACT_ATOM_BYTES));
    assert_eq!(
        parse_query(&identifier),
        Err(QueryError::AtomTooLong {
            bytes: MAX_EXACT_ATOM_BYTES + 2,
            limit: MAX_EXACT_ATOM_BYTES,
        })
    );
}
