use hsum::ingest::{
    ChunkKind, ChunkSettings, DEFAULT_CHUNK_MAX_BYTES, DEFAULT_CHUNK_OVERLAP_BYTES,
    DEFAULT_CHUNK_TARGET_BYTES, chunk_bytes,
};
use proptest::prelude::*;

#[test]
fn approved_default_chunk_settings_are_frozen() {
    assert_eq!(DEFAULT_CHUNK_TARGET_BYTES, 1_200);
    assert_eq!(DEFAULT_CHUNK_MAX_BYTES, 1_800);
    assert_eq!(DEFAULT_CHUNK_OVERLAP_BYTES, 180);
    assert_eq!(ChunkSettings::default().target_bytes(), 1_200);
    assert_eq!(ChunkSettings::default().max_bytes(), 1_800);
    assert_eq!(ChunkSettings::default().overlap_bytes(), 180);
}

#[test]
fn bom_is_stored_but_the_first_searchable_span_starts_at_byte_three() {
    let original = b"\xEF\xBB\xBFalpha\r\nbeta\r\ngamma\r\n";
    let settings = ChunkSettings::new(8, 12, 2).unwrap();

    let chunks = chunk_bytes(original, ChunkKind::PlainText, settings).unwrap();

    assert!(!chunks.is_empty());
    assert_eq!(chunks[0].span().start(), 3);
    assert_eq!(chunks.last().unwrap().span().end(), original.len() as u64);
    for (ordinal, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal(), ordinal as u32);
        let bytes = chunk.span().slice_bytes(original).unwrap();
        assert_eq!(std::str::from_utf8(bytes).unwrap().as_bytes(), bytes);
        assert!(!bytes.starts_with(b"\xEF\xBB\xBF"));
    }
}

#[test]
fn crlf_bytes_are_never_normalized_or_rewritten() {
    let original = b"alpha\r\nbeta\r\n\r\ngamma\r\ndelta\r\n";
    let settings = ChunkSettings::new(11, 18, 3).unwrap();

    let chunks = chunk_bytes(original, ChunkKind::PlainText, settings).unwrap();

    assert!(chunks.len() > 1);
    for chunk in &chunks {
        let selected = chunk.span().slice_bytes(original).unwrap();
        assert_eq!(chunk.text(), std::str::from_utf8(selected).unwrap());
    }
}

#[test]
fn line_spans_are_derived_from_original_crlf_coordinates() {
    let original = b"one\r\ntwo\r\nthree\r\nfour";
    let settings = ChunkSettings::new(5, 8, 0).unwrap();

    let chunks = chunk_bytes(original, ChunkKind::PlainText, settings).unwrap();

    assert_eq!(chunks[0].line_span().start(), 1);
    assert_eq!(chunks[0].line_span().end(), 1);
    assert_eq!(chunks[1].line_span().start(), 2);
    assert_eq!(chunks[1].line_span().end(), 2);
    assert_eq!(chunks.last().unwrap().line_span().end(), 4);
}

#[test]
fn content_kinds_prefer_their_approved_structural_boundaries() {
    let settings = ChunkSettings::new(28, 96, 0).unwrap();
    let cases = [
        (
            ChunkKind::Markdown,
            "opening paragraph with enough words\n\n# Next section\nbody\n",
            "# Next section",
        ),
        (
            ChunkKind::PlainText,
            "opening paragraph with enough words\n\nSecond paragraph\n",
            "Second paragraph",
        ),
        (
            ChunkKind::Rust,
            "const PRELUDE: &str = \"opening words\";\n\npub fn next_item() {}\n",
            "pub fn next_item",
        ),
        (
            ChunkKind::Python,
            "PRELUDE = 'opening words for this module'\n\ndef next_item():\n    pass\n",
            "def next_item",
        ),
        (
            ChunkKind::TypeScript,
            "const prelude = 'opening words for this module';\n\nexport function nextItem() {}\n",
            "export function nextItem",
        ),
        (
            ChunkKind::JavaScript,
            "const prelude = 'opening words for this module';\n\nfunction nextItem() {}\n",
            "function nextItem",
        ),
        (
            ChunkKind::Go,
            "package sample\n\nvar prelude = \"opening words for module\"\n\nfunc nextItem() {}\n",
            "func nextItem",
        ),
        (
            ChunkKind::Java,
            "package sample;\n\nclass NextItem {}\n",
            "class NextItem",
        ),
        (
            ChunkKind::Kotlin,
            "val prelude = \"opening words for this module\"\n\nfun nextItem() {}\n",
            "fun nextItem",
        ),
        (
            ChunkKind::C,
            "static const char *prelude = \"opening\";\n\nstruct next_item {};\n",
            "struct next_item",
        ),
        (
            ChunkKind::Cpp,
            "static const char *prelude = \"opening\";\n\nclass NextItem {};\n",
            "class NextItem",
        ),
        (
            ChunkKind::Ruby,
            "PRELUDE = 'opening words for this module'\n\ndef next_item\nend\n",
            "def next_item",
        ),
        (
            ChunkKind::CSharp,
            "namespace Sample;\n\nclass NextItem {}\n",
            "class NextItem",
        ),
        (
            ChunkKind::Swift,
            "let prelude = \"opening words for this module\"\n\nfunc nextItem() {}\n",
            "func nextItem",
        ),
        (
            ChunkKind::Php,
            "$prelude = 'opening words for this module';\n\nfunction nextItem() {}\n",
            "function nextItem",
        ),
        (
            ChunkKind::Scala,
            "val prelude = \"opening words for this module\"\n\ndef nextItem() = {}\n",
            "def nextItem",
        ),
    ];

    for (kind, text, boundary) in cases {
        let chunks = chunk_bytes(text.as_bytes(), kind, settings).unwrap();
        assert!(
            chunks.len() > 1,
            "{kind:?} should split at a preferred boundary"
        );
        let expected_end = text.find(boundary).unwrap() as u64;
        assert_eq!(
            chunks[0].span().end(),
            expected_end,
            "{kind:?} did not choose its structural boundary"
        );
    }
}

#[test]
fn shell_and_sql_chunk_at_paragraph_boundaries_without_declaration_keywords() {
    let settings = ChunkSettings::new(28, 96, 0).unwrap();
    let cases = [
        (
            ChunkKind::Shell,
            "echo \"opening words for this script\"\n\nnext_item() {\n  echo hi\n}\n",
            "next_item() {",
        ),
        (
            ChunkKind::Sql,
            "SELECT one FROM opening_words_table;\n\nCREATE TABLE next_item (id INT);\n",
            "CREATE TABLE next_item",
        ),
    ];

    for (kind, text, boundary) in cases {
        let chunks = chunk_bytes(text.as_bytes(), kind, settings).unwrap();
        assert!(
            chunks.len() > 1,
            "{kind:?} should split at a paragraph boundary"
        );
        let expected_end = text.find(boundary).unwrap() as u64;
        assert_eq!(
            chunks[0].span().end(),
            expected_end,
            "{kind:?} did not choose its paragraph boundary"
        );
    }
}

#[test]
fn every_chunk_kind_has_a_distinct_layout_fingerprint() {
    use std::collections::HashSet;

    let mut names = HashSet::new();
    let mut fingerprints = HashSet::new();
    for kind in ChunkKind::ALL {
        assert!(
            names.insert(kind.as_str()),
            "{kind:?} reuses the as_str value {:?}",
            kind.as_str()
        );
        assert!(
            fingerprints.insert(hsum::store::chunker_fingerprint(kind)),
            "{kind:?} collides with another chunk layout fingerprint"
        );
    }
    assert_eq!(fingerprints.len(), 18);
}

#[test]
fn a_long_indivisible_line_splits_at_utf8_safe_hard_boundaries() {
    let text = "é".repeat(1_001);
    let chunks = chunk_bytes(
        text.as_bytes(),
        ChunkKind::PlainText,
        ChunkSettings::default(),
    )
    .unwrap();

    assert!(chunks.len() >= 2);
    assert_eq!(chunks[0].span().end(), 1_800);
    for chunk in chunks {
        let span = chunk.span();
        assert!(span.end() - span.start() <= DEFAULT_CHUNK_MAX_BYTES as u64);
        assert!(span.slice_utf8(&text).is_ok());
    }
}

#[test]
fn empty_and_bom_only_documents_have_no_searchable_chunks() {
    assert!(
        chunk_bytes(b"", ChunkKind::PlainText, ChunkSettings::default())
            .unwrap()
            .is_empty()
    );
    assert!(
        chunk_bytes(
            b"\xEF\xBB\xBF",
            ChunkKind::PlainText,
            ChunkSettings::default()
        )
        .unwrap()
        .is_empty()
    );
}

proptest! {
    #[test]
    fn chunks_cover_original_utf8_without_gaps_or_excess_overlap(text in "\\PC{0,5000}") {
        let bytes = text.as_bytes();
        let chunks = chunk_bytes(bytes, ChunkKind::PlainText, ChunkSettings::default()).unwrap();

        if bytes.is_empty() {
            prop_assert!(chunks.is_empty());
        } else {
            prop_assert!(!chunks.is_empty());
            prop_assert_eq!(chunks[0].span().start(), 0);
            prop_assert_eq!(chunks.last().unwrap().span().end(), bytes.len() as u64);

            for (ordinal, chunk) in chunks.iter().enumerate() {
                let span = chunk.span();
                prop_assert_eq!(chunk.ordinal(), ordinal as u32);
                prop_assert!(span.start() < span.end());
                prop_assert!(span.end() - span.start() <= DEFAULT_CHUNK_MAX_BYTES as u64);
                prop_assert!(span.slice_utf8(&text).is_ok());

                if ordinal > 0 {
                    let previous = chunks[ordinal - 1].span();
                    prop_assert!(span.start() <= previous.end());
                    prop_assert!(
                        previous.end() - span.start() <= DEFAULT_CHUNK_OVERLAP_BYTES as u64
                    );
                }
            }
        }
    }
}
