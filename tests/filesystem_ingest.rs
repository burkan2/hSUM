use hsum::ingest::{
    DiscoveryError, DiscoveryOptions, FileIssueKind, HARD_MAX_FILE_BYTES, discover_files,
    discover_files_spooled, repo_uri,
};
use std::fs;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;

fn keys(snapshot: &hsum::ingest::FilesystemSnapshot) -> Vec<Vec<u8>> {
    snapshot
        .files()
        .iter()
        .map(|file| file.connector_key().to_vec())
        .collect()
}

#[test]
fn discovery_is_sorted_and_applies_supported_hidden_build_sensitive_and_gitignore_rules() {
    let directory = tempdir().unwrap();
    fs::create_dir_all(directory.path().join("nested")).unwrap();
    fs::create_dir_all(directory.path().join(".git")).unwrap();
    fs::create_dir_all(directory.path().join("target")).unwrap();
    fs::create_dir_all(directory.path().join("node_modules")).unwrap();
    fs::write(directory.path().join("zeta.rs"), b"fn zeta() {}\r\n").unwrap();
    fs::write(directory.path().join("alpha.md"), b"# Alpha\n").unwrap();
    fs::write(directory.path().join("nested/keep.txt"), b"keep\n").unwrap();
    fs::write(directory.path().join("nested/ignored.txt"), b"ignored\n").unwrap();
    fs::write(directory.path().join("unsupported.json"), b"{}\n").unwrap();
    fs::write(directory.path().join(".hidden.md"), b"hidden\n").unwrap();
    fs::write(directory.path().join(".env"), b"TOKEN=secret\n").unwrap();
    fs::write(directory.path().join("private.pem"), b"PRIVATE KEY\n").unwrap();
    fs::write(directory.path().join(".git/config"), b"secret\n").unwrap();
    fs::write(directory.path().join("target/generated.rs"), b"generated\n").unwrap();
    fs::write(
        directory.path().join("node_modules/dependency.js"),
        b"dependency\n",
    )
    .unwrap();
    fs::write(
        directory.path().join(".gitignore"),
        b"nested/*.txt\n!nested/keep.txt\n",
    )
    .unwrap();

    let snapshot = discover_files(directory.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(
        keys(&snapshot),
        vec![
            b"alpha.md".to_vec(),
            b"nested/keep.txt".to_vec(),
            b"zeta.rs".to_vec(),
        ]
    );
    assert_eq!(snapshot.files()[2].original_bytes(), b"fn zeta() {}\r\n");
}

#[test]
fn invalid_utf8_nul_and_oversized_files_are_present_failed_not_absent() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("bad-utf8.txt"), [0xff, 0xfe]).unwrap();
    fs::write(directory.path().join("nul.txt"), b"a\0b").unwrap();
    fs::write(directory.path().join("too-large.txt"), b"12345").unwrap();
    fs::write(directory.path().join("valid.txt"), b"ok\r\n").unwrap();
    let options = DiscoveryOptions::default().with_max_file_bytes(4).unwrap();

    let snapshot = discover_files(directory.path(), &options).unwrap();

    assert_eq!(keys(&snapshot), vec![b"valid.txt".to_vec()]);
    let issues: Vec<_> = snapshot
        .issues()
        .iter()
        .map(|issue| (issue.connector_key().to_vec(), issue.kind()))
        .collect();
    assert_eq!(
        issues,
        vec![
            (b"bad-utf8.txt".to_vec(), FileIssueKind::InvalidUtf8),
            (b"nul.txt".to_vec(), FileIssueKind::NulContent),
            (b"too-large.txt".to_vec(), FileIssueKind::FileTooLarge),
        ]
    );
}

#[test]
fn file_limit_configuration_is_bounded_by_the_hard_ceiling() {
    assert!(
        DiscoveryOptions::default()
            .with_max_file_bytes(HARD_MAX_FILE_BYTES)
            .is_ok()
    );
    assert!(
        DiscoveryOptions::default()
            .with_max_file_bytes(HARD_MAX_FILE_BYTES + 1)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn intermediate_and_final_symlinks_are_never_discovered() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("outside.md"), b"outside\n").unwrap();
    fs::write(root.path().join("inside.md"), b"inside\n").unwrap();
    symlink(
        outside.path().join("outside.md"),
        root.path().join("file-link.md"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("directory-link")).unwrap();

    let snapshot = discover_files(root.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(keys(&snapshot), vec![b"inside.md".to_vec()]);
}

#[cfg(unix)]
#[test]
fn hard_links_remain_indistinguishable_from_ordinary_regular_files() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let original = outside.path().join("original.txt");
    fs::write(&original, b"hard-linked content\n").unwrap();
    fs::hard_link(&original, root.path().join("linked.txt")).unwrap();

    let snapshot = discover_files(root.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(keys(&snapshot), vec![b"linked.txt".to_vec()]);
    assert_eq!(
        snapshot.files()[0].original_bytes(),
        b"hard-linked content\n"
    );
    assert_eq!(fs::metadata(original).unwrap().nlink(), 2);
}

#[cfg(unix)]
#[test]
fn a_symlink_cannot_be_used_as_the_source_root() {
    use std::os::unix::fs::symlink;

    let parent = tempdir().unwrap();
    let real_root = tempdir().unwrap();
    symlink(real_root.path(), parent.path().join("root-link")).unwrap();

    let error = discover_files(
        &parent.path().join("root-link"),
        &DiscoveryOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, DiscoveryError::RootIsSymlink { .. }));
}

#[cfg(target_os = "linux")]
#[test]
fn connector_keys_preserve_raw_non_utf8_path_bytes_and_sort_by_them() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = tempdir().unwrap();
    let first = OsString::from_vec(vec![b'a', 0x80, b'.', b't', b'x', b't']);
    let second = OsString::from_vec(vec![b'a', 0x81, b'.', b't', b'x', b't']);
    fs::write(directory.path().join(first), b"first\n").unwrap();
    fs::write(directory.path().join(second), b"second\n").unwrap();

    let snapshot = discover_files(directory.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(
        keys(&snapshot),
        vec![
            vec![b'a', 0x80, b'.', b't', b'x', b't'],
            vec![b'a', 0x81, b'.', b't', b'x', b't'],
        ]
    );
}

#[test]
fn repo_uri_percent_encodes_raw_bytes_with_uppercase_hex() {
    assert_eq!(repo_uri(b"src/a b%\x80.rs"), "repo://src/a%20b%25%80.rs");
    assert_eq!(
        repo_uri(b"AZaz09-._~/nested/file.rs"),
        "repo://AZaz09-._~/nested/file.rs"
    );
}

#[test]
fn accepted_file_carries_the_post_read_source_timestamp() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("notes.md"), b"stable bytes").unwrap();

    let snapshot = discover_files(directory.path(), &DiscoveryOptions::default()).unwrap();
    let timestamp = snapshot.files()[0]
        .source_timestamp()
        .expect("ordinary files expose a representable mtime");

    assert!(timestamp.unix_seconds() > 0);
    assert!(timestamp.nanoseconds() < 1_000_000_000);
}

#[test]
fn explicit_include_and_exclude_filters_are_deterministic_and_exclude_wins() {
    let directory = tempdir().unwrap();
    fs::create_dir_all(directory.path().join("docs/private")).unwrap();
    fs::write(directory.path().join("docs/public.md"), b"public\n").unwrap();
    fs::write(directory.path().join("docs/private/secret.md"), b"secret\n").unwrap();
    fs::write(directory.path().join("root.md"), b"root\n").unwrap();
    fs::write(directory.path().join(".gitignore"), b"docs/\n").unwrap();
    let options = DiscoveryOptions::default()
        .include("docs/**")
        .exclude("docs/private/**");

    let snapshot = discover_files(directory.path(), &options).unwrap();

    assert_eq!(keys(&snapshot), vec![b"docs/public.md".to_vec()]);
}

#[test]
fn nested_gitignore_rules_apply_relative_to_their_directory() {
    let directory = tempdir().unwrap();
    fs::create_dir_all(directory.path().join("nested")).unwrap();
    fs::write(
        directory.path().join("nested/.gitignore"),
        b"*.md\n!keep.md\n",
    )
    .unwrap();
    fs::write(directory.path().join("nested/drop.md"), b"drop\n").unwrap();
    fs::write(directory.path().join("nested/keep.md"), b"keep\n").unwrap();

    let snapshot = discover_files(directory.path(), &DiscoveryOptions::default()).unwrap();

    assert_eq!(keys(&snapshot), vec![b"nested/keep.md".to_vec()]);
}

#[test]
fn sensitive_directories_require_both_explicit_include_and_allowance() {
    let directory = tempdir().unwrap();
    fs::create_dir_all(directory.path().join(".ssh")).unwrap();
    fs::write(directory.path().join(".ssh/notes.txt"), b"private\n").unwrap();

    let include_only = DiscoveryOptions::default().include(".ssh/**");
    assert!(
        discover_files(directory.path(), &include_only)
            .unwrap()
            .files()
            .is_empty()
    );

    let explicitly_allowed = DiscoveryOptions::default()
        .include(".ssh/**")
        .allow_sensitive(true);
    assert_eq!(
        keys(&discover_files(directory.path(), &explicitly_allowed).unwrap()),
        vec![b".ssh/notes.txt".to_vec()]
    );
}

#[test]
fn source_budget_refuses_large_discovery_before_returning_a_partial_snapshot() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("one.txt"), b"one\n").unwrap();
    fs::write(directory.path().join("two.txt"), b"two\n").unwrap();
    let options = DiscoveryOptions::default()
        .with_source_limits(1, 1024)
        .unwrap();

    let error = discover_files(directory.path(), &options).unwrap_err();

    assert!(matches!(
        error,
        DiscoveryError::SourceLimitExceeded {
            files: 2,
            max_files: 1,
            ..
        }
    ));
}

#[test]
fn irrelevant_entries_are_bounded_before_per_directory_sorting() {
    let directory = tempdir().unwrap();
    for index in 0..5 {
        fs::write(
            directory.path().join(format!("irrelevant-{index}.bin")),
            b"x",
        )
        .unwrap();
    }
    let options = DiscoveryOptions::default()
        .with_traversal_limits(4, 10, 10, 4)
        .unwrap();

    let error = discover_files(directory.path(), &options).unwrap_err();

    assert!(matches!(
        error,
        DiscoveryError::TraversalLimitExceeded { .. }
    ));
}

#[test]
fn spooled_discovery_keeps_bodies_out_of_the_manifest_and_unlinks_staging() {
    let source = tempdir().unwrap();
    let staging = tempdir().unwrap();
    fs::write(source.path().join("a.md"), b"# Alpha\n").unwrap();
    fs::write(source.path().join("b.rs"), b"fn beta() {}\n").unwrap();

    let mut spool =
        discover_files_spooled(source.path(), &DiscoveryOptions::default(), staging.path())
            .unwrap();

    assert_eq!(spool.entries().len(), 2);
    assert!(spool.issues().is_empty());
    assert_eq!(spool.entries()[0].connector_key(), b"a.md");
    assert_eq!(spool.entries()[0].body_len(), 8);
    assert_eq!(spool.read_body(0).unwrap(), b"# Alpha\n");
    assert_eq!(spool.read_body(1).unwrap(), b"fn beta() {}\n");
    assert_eq!(
        fs::read_dir(staging.path()).unwrap().count(),
        0,
        "the private spool must be unlinked while its descriptor is open"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_ancestor_cannot_be_used_as_source_root() {
    let fixture = tempdir().unwrap();
    let physical_parent = fixture.path().join("physical");
    let source = physical_parent.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("inside.md"), b"inside\n").unwrap();
    let alias = fixture.path().join("alias");
    symlink(&physical_parent, &alias).unwrap();

    let error = discover_files(&alias.join("source"), &DiscoveryOptions::default()).unwrap_err();

    assert!(matches!(error, DiscoveryError::RootIsSymlink { .. }));
}
