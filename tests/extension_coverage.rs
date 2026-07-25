use hsum::ingest::ChunkKind;

/// Every extension the discovery gate admits must resolve to a chunk kind,
/// and the hashed pipeline descriptor must name exactly that same set.
/// Without this test a future language addition can update two of three sites.
#[test]
fn discovery_chunking_and_pipeline_descriptor_agree_on_the_extension_set() {
    let expected = [
        "md", "markdown", "txt", "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "kt", "kts",
        "c", "h", "cpp", "hpp", "cc", "hh", "cxx", "rb", "cs", "swift", "php", "scala", "sh",
        "bash", "sql",
    ];

    // Site 1 -> Site 2: everything admitted has a chunker.
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

    // Site 3: the hashed descriptor names the same set in the same order.
    let descriptor = hsum::store::pipeline_descriptor();
    let line = descriptor
        .lines()
        .find(|line| line.starts_with("filesystem=v1:extensions="))
        .expect("descriptor names the extension set");
    let listed = line
        .trim_start_matches("filesystem=v1:extensions=")
        .trim_end_matches(":symlinks=never");
    assert_eq!(listed, expected.join(","));
}
