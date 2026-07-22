use std::path::PathBuf;

use directories::ProjectDirs;
use hsum::config::{
    LogicalSelection, ManagedPaths, PointerError, RepositoryPointer, SelectionError, SelectionMode,
    SelectionRequest, TrustRegistry,
};
use hsum::domain::SafeSlug;
use tempfile::tempdir;

#[test]
fn hsum_home_places_config_data_and_cache_under_one_root() {
    let temporary = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(temporary.path())).unwrap();

    assert_eq!(paths.config_dir(), temporary.path().join("config"));
    assert_eq!(paths.data_dir(), temporary.path().join("data"));
    assert_eq!(paths.cache_dir(), temporary.path().join("cache"));
    assert_eq!(
        paths.config_file(),
        temporary.path().join("config/config.toml")
    );
    assert_eq!(
        paths.trust_registry_file(),
        temporary.path().join("config/trusted-projects.toml")
    );
}

#[test]
fn ordinary_locations_are_the_directories_crate_project_locations() {
    let expected = ProjectDirs::from("", "", "hsum").unwrap();
    let paths = ManagedPaths::resolve(None).unwrap();

    assert_eq!(paths.config_dir(), expected.config_dir());
    assert_eq!(paths.data_dir(), expected.data_dir());
    assert_eq!(paths.cache_dir(), expected.cache_dir());
}

#[test]
fn managed_index_database_is_never_inside_the_repository() {
    let temporary = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(temporary.path())).unwrap();
    let name = SafeSlug::new("team-memory").unwrap();

    assert_eq!(
        paths.index_database(&name),
        temporary
            .path()
            .join("data/indexes/team-memory/index.sqlite")
    );
}

#[test]
fn pointer_accepts_only_versioned_portable_logical_names() {
    let pointer = RepositoryPointer::parse(
        r#"
schema_version = 1
index = "team-memory"
project = "compiler_42"
"#,
    )
    .unwrap();

    assert_eq!(pointer.schema_version(), 1);
    assert_eq!(pointer.index_name().as_str(), "team-memory");
    assert_eq!(pointer.project_name().as_str(), "compiler_42");
}

#[test]
fn pointer_rejects_authorizing_or_machine_local_fields() {
    for forbidden in [
        r#"
schema_version = 1
index = "team-memory"
project = "compiler"
binding_id = "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44af"
"#,
        r#"
schema_version = 1
index = "team-memory"
project = "compiler"
index_id = "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44af"
"#,
        r#"
schema_version = 1
index = "team-memory"
project = "compiler"
database = "/tmp/index.sqlite"
"#,
    ] {
        assert!(matches!(
            RepositoryPointer::parse(forbidden),
            Err(PointerError::Malformed(_))
        ));
    }
}

#[test]
fn pointer_rejects_unknown_schema_and_unsafe_names() {
    let unknown_version = r#"
schema_version = 2
index = "team-memory"
project = "compiler"
"#;
    assert!(matches!(
        RepositoryPointer::parse(unknown_version),
        Err(PointerError::UnsupportedSchema { found: 2 })
    ));

    let unsafe_name = r#"
schema_version = 1
index = "../foreign-index"
project = "compiler"
"#;
    assert!(matches!(
        RepositoryPointer::parse(unsafe_name),
        Err(PointerError::InvalidIndexName(_))
    ));
}

#[test]
fn pointer_alone_never_selects_or_authorizes_an_index() {
    let pointer = RepositoryPointer::new(
        SafeSlug::new("team-memory").unwrap(),
        SafeSlug::new("compiler").unwrap(),
    );
    let registry = TrustRegistry::new();
    let request = SelectionRequest {
        mode: SelectionMode::DirectCli,
        explicit: None,
        environment: None,
        canonical_root: Some(PathBuf::from("/definitely/not/a/trusted/root")),
        configured_default: None,
        pointer: Some(pointer),
    };

    assert!(matches!(
        registry.select(request),
        Err(SelectionError::PointerIsOnlyHint)
    ));
}

#[test]
fn logical_selection_requires_both_safe_names() {
    let logical = LogicalSelection::parse("main-index", "repo_project").unwrap();
    assert_eq!(logical.index_name().as_str(), "main-index");
    assert_eq!(logical.project_name().as_str(), "repo_project");

    assert!(LogicalSelection::parse("Main", "repo_project").is_err());
    assert!(LogicalSelection::parse("main-index", "../repo").is_err());
}
