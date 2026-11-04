use std::fs;
use std::path::Path;
use std::process::Command;

use hsum::app::{
    ContextError, ContextRequest, InitRequest, MAX_FILESYSTEM_SOURCE_CONFIG_BYTES, initialize,
    resolve_context, resolve_trust_target,
};
use hsum::config::{ManagedPaths, SelectionError, SelectionMode};
use hsum::domain::{SafeSlug, SourceId};
use tempfile::tempdir;

fn write_private_config(path: &Path, contents: impl AsRef<[u8]>) {
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn initialized_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    ManagedPaths,
    hsum::app::InitOutcome,
) {
    let repository = tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    let portable_home = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(portable_home.path())).unwrap();
    let mut request = InitRequest::new(repository.path().to_path_buf(), paths.clone());
    request.index_name = Some(SafeSlug::new("fixture").unwrap());
    request.project_name = Some(SafeSlug::new("default").unwrap());
    request.no_ingest = true;
    let outcome = initialize(&request).unwrap();
    (repository, portable_home, paths, outcome)
}

fn fixture_database_path(paths: &ManagedPaths) -> std::path::PathBuf {
    paths.index_database(&SafeSlug::new("fixture").unwrap())
}

#[test]
fn trusted_root_and_explicit_binding_materialize_the_same_scope() {
    let (repository, _portable_home, paths, initialized) = initialized_fixture();
    fs::write(repository.path().join(".hsum.toml"), "not strict TOML").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            repository.path().join(".hsum.toml"),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
    }

    let direct = resolve_context(&ContextRequest::direct(
        repository.path().to_path_buf(),
        paths.clone(),
    ))
    .unwrap();
    let mut mcp = ContextRequest::direct(repository.path().to_path_buf(), paths);
    mcp.mode = SelectionMode::Mcp;
    mcp.binding = initialized.binding_id;
    let bound = resolve_context(&mcp).unwrap();

    assert_eq!(direct.index_id, initialized.index_id);
    assert_eq!(direct.project_id, initialized.project_id);
    assert_eq!(direct.source_id, initialized.source_id);
    assert_eq!(
        direct.source_discovery_options.max_file_bytes(),
        hsum::ingest::DEFAULT_MAX_FILE_BYTES
    );
    assert_eq!(direct.index_id, bound.index_id);
    assert_eq!(direct.project_id, bound.project_id);
    assert_eq!(direct.source_id, bound.source_id);
    assert_eq!(bound.binding_id, initialized.binding_id);
}

#[test]
fn explicit_trust_names_do_not_read_a_malformed_pointer() {
    let (repository, _portable_home, paths, initialized) = initialized_fixture();
    fs::write(repository.path().join(".hsum.toml"), "not strict TOML").unwrap();

    let (_, target) = resolve_trust_target(
        repository.path(),
        &paths,
        Some(SafeSlug::new("fixture").unwrap()),
        Some(SafeSlug::new("default").unwrap()),
    )
    .unwrap();

    assert_eq!(target.index_id, initialized.index_id);
    assert_eq!(target.project_id, initialized.project_id);
}

#[test]
fn a_copied_pointer_is_a_hint_for_trust_but_never_authority() {
    let (repository, _portable_home, paths, initialized) = initialized_fixture();
    let pointer_source = repository.path().join(".hsum.toml");
    let mut pointer_request = InitRequest::new(repository.path().to_path_buf(), paths.clone());
    pointer_request.write_pointer = true;
    let pointer_outcome = initialize(&pointer_request).unwrap();
    assert_eq!(pointer_outcome.index_id, initialized.index_id);

    let clone = tempdir().unwrap();
    fs::create_dir(clone.path().join(".git")).unwrap();
    fs::copy(pointer_source, clone.path().join(".hsum.toml")).unwrap();

    let error = resolve_context(&ContextRequest::direct(
        clone.path().to_path_buf(),
        paths.clone(),
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        ContextError::Selection(SelectionError::PointerIsOnlyHint)
    ));

    assert!(matches!(
        resolve_trust_target(clone.path(), &paths, None, None),
        Err(ContextError::TrustedSourceRootMismatch)
    ));
}

#[test]
fn a_strict_configured_default_selects_direct_cli_from_an_unrelated_directory() {
    let (_repository, portable_home, paths, initialized) = initialized_fixture();
    fs::create_dir_all(paths.config_dir()).unwrap();
    write_private_config(
        &paths.config_file(),
        concat!(
            "schema_version = 2\n",
            "config_epoch = 1\n",
            "default_index = \"fixture\"\n",
            "default_project = \"default\"\n"
        ),
    );
    let unrelated = tempdir().unwrap();
    fs::write(unrelated.path().join(".hsum.toml"), "not strict TOML").unwrap();

    let context = resolve_context(&ContextRequest::direct(
        unrelated.path().to_path_buf(),
        paths,
    ))
    .unwrap();

    assert_eq!(context.index_id, initialized.index_id);
    assert_eq!(context.project_id, initialized.project_id);
    drop(portable_home);
}

#[test]
fn n_minus_one_user_config_is_diagnosed_before_v2_fields_are_required() {
    let (_repository, _portable_home, paths, _initialized) = initialized_fixture();
    let bytes = concat!(
        "schema_version = 1\n",
        "default_index = \"fixture\"\n",
        "default_project = \"default\"\n",
    );
    write_private_config(&paths.config_file(), bytes);
    let unrelated = tempdir().unwrap();

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            unrelated.path().to_path_buf(),
            paths.clone(),
        )),
        Err(ContextError::ConfigSchema { found: 1 })
    ));
    assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), bytes);
}

#[test]
fn environment_selection_does_not_read_a_malformed_pointer() {
    let (repository, portable_home, _paths, initialized) = initialized_fixture();
    fs::write(repository.path().join(".hsum.toml"), "not strict TOML").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_hsum"))
        .current_dir(repository.path())
        .env("HSUM_HOME", portable_home.path())
        .env("HSUM_INDEX", "fixture")
        .env("HSUM_PROJECT", "default")
        .args(["context", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["index_id"], initialized.index_id.to_string());
    assert_eq!(json["project_id"], initialized.project_id.to_string());
}

#[test]
fn a_trusted_binding_rejects_a_tampered_source_logical_root() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let redirected = tempdir().unwrap();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    database
        .execute(
            "UPDATE sources SET logical_uri = ?1",
            [redirected.path().to_str().unwrap()],
        )
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::SourceConfigurationRootMismatch)
    ));
}

#[test]
fn a_trusted_binding_rejects_a_tampered_source_config_root() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let redirected = tempdir().unwrap();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    let original: String = database
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&original).unwrap();
    config["root"] = redirected.path().to_str().unwrap().into();
    database
        .execute(
            "UPDATE sources SET config_json = ?1",
            [serde_json::to_string(&config).unwrap()],
        )
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::SourceConfigurationRootMismatch)
    ));
}

#[test]
fn a_trusted_binding_rejects_matching_source_roots_that_redirect_away_from_the_binding() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let redirected = tempdir().unwrap();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    let original: String = database
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&original).unwrap();
    config["root"] = redirected.path().to_str().unwrap().into();
    database
        .execute(
            "UPDATE sources SET logical_uri = ?1, config_json = ?2",
            rusqlite::params![
                redirected.path().to_str().unwrap(),
                serde_json::to_string(&config).unwrap()
            ],
        )
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::TrustedSourceRootMismatch)
    ));
}

#[test]
fn a_trusted_binding_rejects_malformed_source_configuration() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    database
        .execute("UPDATE sources SET config_json = '{\"root\":42}'", [])
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::InvalidFilesystemSourceConfig(_))
    ));
}

#[test]
fn a_tampered_index_rejects_a_second_project_source_before_loading_rows() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    let project_id: Vec<u8> = database
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let second_source_id = SourceId::new_v4().as_uuid().as_bytes().to_vec();
    database
        .execute(
            "INSERT INTO sources (
                 id, kind, name, logical_uri, config_json, created_at
             )
             SELECT ?1, 'filesystem', 'second', '/second', config_json, created_at
             FROM sources
             LIMIT 1",
            [&second_source_id],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO project_sources (project_id, source_id) VALUES (?1, ?2)",
            rusqlite::params![project_id, second_source_id],
        )
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::AlphaSourceCardinality { found: 2 })
    ));
}

#[test]
fn a_tampered_index_rejects_oversized_source_fields_before_loading_them() {
    let cases = [
        ("name", "x".repeat(65)),
        (
            "logical_uri",
            format!("/{}", "x".repeat(MAX_FILESYSTEM_SOURCE_CONFIG_BYTES)),
        ),
        (
            "config_json",
            "x".repeat(MAX_FILESYSTEM_SOURCE_CONFIG_BYTES + 1),
        ),
    ];

    for (column, oversized_value) in cases {
        let (repository, _portable_home, paths, _initialized) = initialized_fixture();
        let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
        database
            .execute(
                &format!("UPDATE sources SET {column} = ?1"),
                [&oversized_value],
            )
            .unwrap();
        drop(database);

        assert!(
            matches!(
                resolve_context(&ContextRequest::direct(
                    repository.path().to_path_buf(),
                    paths
                )),
                Err(ContextError::InvalidDatabaseValue(
                    "bounded filesystem source fields"
                ))
            ),
            "oversized {column} must be rejected"
        );
    }
}

#[test]
fn a_tampered_index_rejects_a_bounded_but_unsafe_source_name() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    database
        .execute("UPDATE sources SET name = 'Not-Safe'", [])
        .unwrap();
    drop(database);

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            repository.path().to_path_buf(),
            paths
        )),
        Err(ContextError::InvalidSourceName(_))
    ));
}

#[test]
fn effective_context_carries_persisted_discovery_limits() {
    let (repository, _portable_home, paths, _initialized) = initialized_fixture();
    let database = rusqlite::Connection::open(fixture_database_path(&paths)).unwrap();
    let original: String = database
        .query_row("SELECT config_json FROM sources", [], |row| row.get(0))
        .unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&original).unwrap();
    config["max_file_bytes"] = 1024_u64.into();
    database
        .execute(
            "UPDATE sources SET config_json = ?1",
            [serde_json::to_string(&config).unwrap()],
        )
        .unwrap();
    drop(database);

    let context = resolve_context(&ContextRequest::direct(
        repository.path().to_path_buf(),
        paths,
    ))
    .unwrap();
    assert_eq!(context.source_discovery_options.max_file_bytes(), 1024);
}

#[cfg(unix)]
#[test]
fn user_config_rejects_symlinks_and_nonprivate_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let portable_home = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(portable_home.path())).unwrap();
    fs::create_dir_all(paths.config_dir()).unwrap();
    let target = portable_home.path().join("config-target.toml");
    write_private_config(
        &target,
        "schema_version = 1\ndefault_index = \"fixture\"\ndefault_project = \"default\"\n",
    );
    symlink(&target, paths.config_file()).unwrap();
    let current_dir = tempdir().unwrap();

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            current_dir.path().to_path_buf(),
            paths.clone()
        )),
        Err(ContextError::ConfigUnsafe)
    ));

    fs::remove_file(paths.config_file()).unwrap();
    fs::copy(&target, paths.config_file()).unwrap();
    fs::set_permissions(paths.config_file(), fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            current_dir.path().to_path_buf(),
            paths.clone()
        )),
        Err(ContextError::ConfigUnsafe)
    ));

    fs::remove_file(paths.config_file()).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&target, paths.config_file()).unwrap();
    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            current_dir.path().to_path_buf(),
            paths
        )),
        Err(ContextError::ConfigUnsafe)
    ));
}

#[test]
fn user_config_is_bounded_before_toml_deserialization() {
    let portable_home = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(portable_home.path())).unwrap();
    fs::create_dir_all(paths.config_dir()).unwrap();
    write_private_config(paths.config_file().as_path(), vec![b'x'; 64 * 1024 + 1]);
    let current_dir = tempdir().unwrap();

    assert!(matches!(
        resolve_context(&ContextRequest::direct(
            current_dir.path().to_path_buf(),
            paths
        )),
        Err(ContextError::ConfigTooLarge)
    ));
}

#[test]
fn managed_data_and_cache_overrides_must_be_absolute() {
    let portable_home = tempdir().unwrap();
    let paths = ManagedPaths::resolve(Some(portable_home.path())).unwrap();
    assert!(
        paths
            .clone()
            .with_overrides(Some("relative-data".into()), None)
            .is_err()
    );
    assert!(
        paths
            .with_overrides(None, Some("relative-cache".into()))
            .is_err()
    );
}
