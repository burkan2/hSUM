use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hsum::config::{
    ConfigArtifactKind, ConfigMigrationError, ManagedPaths, TrustRegistry, apply_config_migration,
    plan_config_migration,
};
use hsum::domain::Sha256Digest;
use hsum::store::MaintenanceError;
use serde_json_canonicalizer::to_vec as to_canonical_vec;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _home: TempDir,
    _repository: TempDir,
    config_file: PathBuf,
    trust_file: PathBuf,
    backup_directory: PathBuf,
    config_v1: Vec<u8>,
    trust_v1: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let home = tempdir().unwrap();
        let repository = tempdir().unwrap();
        let paths = ManagedPaths::resolve(Some(home.path())).unwrap();
        fs::create_dir_all(paths.config_dir()).unwrap();
        let config_file = paths.config_file();
        let trust_file = paths.trust_registry_file();
        let backup_directory = home.path().join("config-migration-backup");
        let config_v1 = concat!(
            "schema_version = 1\n",
            "default_index = \"fixture\"\n",
            "default_project = \"default\"\n",
        )
        .as_bytes()
        .to_vec();
        let trust_v1 = format!(
            concat!(
                "schema_version = 1\n\n",
                "[[bindings]]\n",
                "root = \"{}\"\n",
                "binding_id = \"018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1\"\n",
                "index_id = \"018f47f0-9d9a-4a63-b4cc-8d6f2c8a44af\"\n",
                "index_name = \"fixture\"\n",
                "project_id = \"018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b0\"\n",
                "project_name = \"default\"\n",
            ),
            repository.path().display(),
        )
        .into_bytes();
        write_private(&config_file, &config_v1);
        write_private(&trust_file, &trust_v1);
        Self {
            _home: home,
            _repository: repository,
            config_file,
            trust_file,
            backup_directory,
            config_v1,
            trust_v1,
        }
    }

    fn plan(&self) -> hsum::store::PlanEnvelope<hsum::config::ConfigMigrationPlan> {
        plan_config_migration(
            &self.config_file,
            &self.trust_file,
            &self.backup_directory,
            Duration::from_secs(1),
        )
        .unwrap()
    }
}

#[test]
fn plan_binds_both_exact_sources_without_mutating_or_backing_up() {
    let fixture = Fixture::new();
    let plan = fixture.plan();

    assert_eq!(plan.plan.migrations_required(), 2);
    assert_eq!(plan.plan.artifacts.len(), 2);
    assert_eq!(plan.plan.artifacts[0].kind, ConfigArtifactKind::UserConfig);
    assert_eq!(
        plan.plan.artifacts[1].kind,
        ConfigArtifactKind::TrustRegistry
    );
    assert_eq!(fs::read(&fixture.config_file).unwrap(), fixture.config_v1);
    assert_eq!(fs::read(&fixture.trust_file).unwrap(), fixture.trust_v1);
    assert!(!fixture.backup_directory.exists());
}

#[test]
fn wrong_confirmation_and_stale_sources_refuse_before_backup_or_mutation() {
    let wrong_confirmation = Fixture::new();
    let plan = wrong_confirmation.plan();
    let error = apply_config_migration(
        &wrong_confirmation.config_file,
        &wrong_confirmation.trust_file,
        &plan,
        Sha256Digest::of_bytes(b"not-the-plan"),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigMigrationError::Maintenance(MaintenanceError::ConfirmationMismatch)
    ));
    assert_eq!(
        fs::read(&wrong_confirmation.config_file).unwrap(),
        wrong_confirmation.config_v1
    );
    assert_eq!(
        fs::read(&wrong_confirmation.trust_file).unwrap(),
        wrong_confirmation.trust_v1
    );
    assert!(!wrong_confirmation.backup_directory.exists());

    let stale = Fixture::new();
    let plan = stale.plan();
    let mut changed = stale.config_v1.clone();
    changed.push(b'\n');
    write_private(&stale.config_file, &changed);
    let error = apply_config_migration(
        &stale.config_file,
        &stale.trust_file,
        &plan,
        plan.plan_hash,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(matches!(error, ConfigMigrationError::PlanStale));
    assert_eq!(fs::read(&stale.config_file).unwrap(), changed);
    assert_eq!(fs::read(&stale.trust_file).unwrap(), stale.trust_v1);
    assert!(!stale.backup_directory.exists());
}

#[test]
fn apply_backs_up_exact_bytes_validates_targets_and_resumes_mixed_state() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let outcome = apply_config_migration(
        &fixture.config_file,
        &fixture.trust_file,
        &plan,
        plan.plan_hash,
        Duration::from_secs(1),
    )
    .unwrap();

    assert_eq!(outcome.migrated_artifacts, 2);
    assert_eq!(outcome.already_migrated_artifacts, 0);
    assert_eq!(
        fs::read(fixture.backup_directory.join("config.toml.bak")).unwrap(),
        fixture.config_v1
    );
    assert_eq!(
        fs::read(fixture.backup_directory.join("trusted-projects.toml.bak")).unwrap(),
        fixture.trust_v1
    );
    let migrated_config = fs::read_to_string(&fixture.config_file).unwrap();
    assert!(migrated_config.contains("schema_version = 2"));
    assert!(migrated_config.contains("config_epoch = 1"));
    let migrated_trust = fs::read_to_string(&fixture.trust_file).unwrap();
    let registry = TrustRegistry::parse(&migrated_trust).unwrap();
    assert_eq!(registry.config_epoch(), 1);

    let retry = apply_config_migration(
        &fixture.config_file,
        &fixture.trust_file,
        &plan,
        plan.plan_hash,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(retry.migrated_artifacts, 0);
    assert_eq!(retry.already_migrated_artifacts, 2);

    write_private(&fixture.trust_file, &fixture.trust_v1);
    let resumed = apply_config_migration(
        &fixture.config_file,
        &fixture.trust_file,
        &plan,
        plan.plan_hash,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(resumed.migrated_artifacts, 1);
    assert_eq!(resumed.already_migrated_artifacts, 1);
    TrustRegistry::load(&fixture.trust_file).unwrap();
}

#[test]
fn a_rehashed_plan_cannot_redirect_an_artifact_or_understate_capacity() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan();
    let victim = fixture._home.path().join("victim.toml");
    write_private(&victim, &fixture.config_v1);
    plan.plan.artifacts[0].path = victim.clone();
    plan.plan.estimated_peak_bytes = 1;
    plan.plan_hash = Sha256Digest::of_bytes(&to_canonical_vec(&plan.plan).unwrap());

    let error = apply_config_migration(
        &fixture.config_file,
        &fixture.trust_file,
        &plan,
        plan.plan_hash,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(matches!(error, ConfigMigrationError::PlanInvalid));
    assert_eq!(fs::read(&victim).unwrap(), fixture.config_v1);
    assert_eq!(fs::read(&fixture.config_file).unwrap(), fixture.config_v1);
    assert_eq!(fs::read(&fixture.trust_file).unwrap(), fixture.trust_v1);
    assert!(!fixture.backup_directory.exists());
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
