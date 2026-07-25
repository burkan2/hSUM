use hsum::ingest::{ChunkKind, DiscoveryOptions, discover_files};
use std::fs;
use tempfile::tempdir;

/// Every extension the discovery gate admits must resolve to a chunk kind,
/// the hashed pipeline descriptor must name exactly that same set, and the
/// real discovery gate (`discover_files`, backed by the private
/// `is_supported_path`) must actually admit every extension in the set and
/// reject what is out of scope. Without this test a future language addition
/// can update some sites and not others: `expected` here, `ChunkKind::from_path`,
/// `PIPELINE_DESCRIPTOR`, and the private `is_supported_path` gate itself are
/// four independent copies of the same list that must all agree.
#[test]
fn discovery_chunking_and_pipeline_descriptor_agree_on_the_extension_set() {
    let expected = [
        "md", "markdown", "txt", "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "kt", "kts",
        "c", "h", "cpp", "hpp", "cc", "hh", "cxx", "rb", "cs", "swift", "php", "scala", "sh",
        "bash", "sql",
    ];

    // expected -> ChunkKind: everything in `expected` has a chunker.
    for extension in expected {
        let path = std::path::PathBuf::from(format!("sample.{extension}"));
        assert!(
            ChunkKind::from_path(&path).is_some(),
            "{extension} is admitted but has no chunk kind"
        );
    }

    // A format we deliberately do not index yet.
    assert!(
        ChunkKind::from_path(std::path::Path::new("sample.json")).is_none(),
        "json is out of scope until extensions are configurable"
    );

    // expected -> PIPELINE_DESCRIPTOR: the hashed descriptor names the same set in the same order.
    let descriptor = hsum::store::pipeline_descriptor();
    let line = descriptor
        .lines()
        .find(|line| line.starts_with("filesystem=v1:extensions="))
        .expect("descriptor names the extension set");
    let listed = line
        .trim_start_matches("filesystem=v1:extensions=")
        .trim_end_matches(":symlinks=never");
    assert_eq!(listed, expected.join(","));

    // expected -> the real discovery gate: `discover_files` must admit exactly
    // one file per extension in `expected`, and must reject the negative
    // controls. This exercises `is_supported_path` itself (it is private to
    // `src/ingest/filesystem.rs`, so this is the only way an integration test
    // can observe its behavior), closing the gap where `is_supported_path`
    // could drift from `expected` while every other site stayed in sync.
    let directory = tempdir().unwrap();
    for extension in expected {
        // `md` and `markdown` (and any other pair sharing a stem) would
        // collide on filename if named identically, so each file uses its
        // extension as a distinct stem too.
        let file_name = format!("sample-{extension}.{extension}");
        fs::write(
            directory.path().join(&file_name),
            format!("sample content for .{extension}\n"),
        )
        .unwrap();
    }
    // Negative controls: extensions the gate must continue to reject.
    fs::write(directory.path().join("sample.json"), b"{}\n").unwrap();
    fs::write(directory.path().join("sample.yaml"), b"key: value\n").unwrap();

    let snapshot = discover_files(directory.path(), &DiscoveryOptions::default()).unwrap();
    let mut discovered: Vec<String> = snapshot
        .files()
        .iter()
        .map(|file| String::from_utf8(file.connector_key().to_vec()).unwrap())
        .collect();
    discovered.sort();

    let mut expected_names: Vec<String> = expected
        .iter()
        .map(|extension| format!("sample-{extension}.{extension}"))
        .collect();
    expected_names.sort();

    assert_eq!(
        discovered, expected_names,
        "discover_files must admit exactly the 28 supported extensions and \
         exclude sample.json and sample.yaml"
    );
}
