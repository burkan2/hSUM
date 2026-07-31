#[cfg(unix)]
use std::fs;

use hsum::config::{
    BindingId, ExplicitSelection, LogicalSelection, RegistrationOutcome, SelectionError,
    SelectionMode, SelectionRequest, SelectionSource, TrustBinding, TrustError, TrustRegistration,
    TrustRegistry, canonicalize_repository_root,
};
use hsum::domain::{IndexId, ProjectId, SafeSlug};
use tempfile::tempdir;
use uuid::{Uuid, Version};

fn registration(root: &std::path::Path) -> TrustRegistration {
    TrustRegistration {
        canonical_root: canonicalize_repository_root(root).unwrap(),
        index_id: IndexId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-4a63-b4cc-8d6f2c8a44af").unwrap(),
        ),
        index_name: SafeSlug::new("team-memory").unwrap(),
        project_id: ProjectId::from_uuid(
            Uuid::parse_str("018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b0").unwrap(),
        ),
        project_name: SafeSlug::new("compiler").unwrap(),
    }
}

#[test]
fn registration_is_randomly_bound_and_matching_reruns_are_idempotent() {
    let root = tempdir().unwrap();
    let mut registry = TrustRegistry::new();
    let request = registration(root.path());

    let created = registry.register(request.clone()).unwrap();
    let first = match created {
        RegistrationOutcome::Created(binding) => binding,
        RegistrationOutcome::Existing(_) => panic!("first registration must be created"),
    };
    assert_eq!(
        first.binding_id().as_uuid().get_version(),
        Some(Version::Random)
    );
    assert_eq!(first.canonical_root(), request.canonical_root);

    let repeated = registry.register(request).unwrap();
    let second = match repeated {
        RegistrationOutcome::Existing(binding) => binding,
        RegistrationOutcome::Created(_) => panic!("matching registration must be idempotent"),
    };
    assert_eq!(second, first);
    assert_eq!(registry.bindings().len(), 1);
}

#[test]
fn conflicting_root_registration_fails_without_mutating_the_registry() {
    let root = tempdir().unwrap();
    let mut registry = TrustRegistry::new();
    let first = registration(root.path());
    registry.register(first.clone()).unwrap();

    let mut conflict = first;
    conflict.project_name = SafeSlug::new("different-project").unwrap();

    assert!(matches!(
        registry.register(conflict),
        Err(TrustError::ConflictingRoot { .. })
    ));
    assert_eq!(registry.bindings().len(), 1);
}

#[test]
fn parsed_bindings_reject_duplicate_roots_and_identity_aliases() {
    let first_root = tempdir().unwrap();
    let second_root = tempdir().unwrap();
    let first_registration = registration(first_root.path());
    let first = TrustBinding::from_registration(
        BindingId::from_uuid(Uuid::new_v4()),
        first_registration.clone(),
    );

    let duplicate_root =
        TrustBinding::from_registration(BindingId::from_uuid(Uuid::new_v4()), first_registration);
    assert!(matches!(
        TrustRegistry::from_bindings(vec![first.clone(), duplicate_root]),
        Err(TrustError::ConflictingRoot { .. })
    ));

    let mut aliased_registration = registration(second_root.path());
    aliased_registration.index_name = SafeSlug::new("aliased-index").unwrap();
    let identity_alias =
        TrustBinding::from_registration(BindingId::from_uuid(Uuid::new_v4()), aliased_registration);
    assert!(matches!(
        TrustRegistry::from_bindings(vec![first, identity_alias]),
        Err(TrustError::ConflictingIdentity)
    ));
}

#[test]
fn registration_is_in_memory_until_explicit_atomic_save() {
    let root = tempdir().unwrap();
    let config = tempdir().unwrap();
    let path = config.path().join("nested/trusted-projects.toml");
    let mut registry = TrustRegistry::new();
    registry.register(registration(root.path())).unwrap();

    assert!(!path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        registry.save_atomic(&path).unwrap();
        assert!(path.is_file());

        let loaded = TrustRegistry::load(&path).unwrap();
        assert_eq!(loaded, registry);

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    // A target with no user-only permission primitive must refuse rather than
    // write a registry it cannot protect. The refusal happens before any bytes
    // are written, so the registry file must not exist afterwards.
    #[cfg(not(unix))]
    {
        assert!(matches!(
            registry.save_atomic(&path),
            Err(TrustError::PrivatePermissionsUnsupported { .. })
        ));
        assert!(!path.exists());
    }
}

#[test]
fn atomic_save_refuses_to_write_a_registry_its_loader_cannot_read() {
    let root = tempdir().unwrap();
    let prototype = registration(root.path());
    let bindings = (0..4096)
        .map(|ordinal| {
            let mut registration = prototype.clone();
            registration.canonical_root = format!("/{}-{ordinal:04}", "x".repeat(220)).into();
            TrustBinding::from_registration(BindingId::from_uuid(Uuid::new_v4()), registration)
        })
        .collect();
    let registry = TrustRegistry::from_bindings(bindings).unwrap();
    let config = tempdir().unwrap();
    let destination = config.path().join("nested/trusted-projects.toml");

    assert!(matches!(
        registry.save_atomic(&destination),
        Err(TrustError::TooLarge)
    ));
    assert!(
        !destination
            .parent()
            .expect("destination has a parent")
            .exists(),
        "size preflight must happen before creating filesystem state"
    );
}

#[test]
fn trust_toml_rejects_unknown_fields_noncanonical_ids_and_roots() {
    let root = tempdir().unwrap();
    let canonical = canonicalize_repository_root(root.path()).unwrap();
    let base = format!(
        r#"
schema_version = 1

[[bindings]]
root = "{}"
binding_id = "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1"
index_id = "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44af"
index_name = "team-memory"
project_id = "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b0"
project_name = "compiler"
"#,
        canonical.display()
    );

    assert!(TrustRegistry::parse(&base).is_ok());
    assert!(TrustRegistry::parse(&format!("{base}\nunknown = true\n")).is_err());
    assert!(
        TrustRegistry::parse(&base.replace(
            "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1",
            "018F47F0-9D9A-4A63-B4CC-8D6F2C8A44B1"
        ))
        .is_err()
    );
    assert!(
        TrustRegistry::parse(&base.replace(
            "018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1",
            "018f47f0-9d9a-1a63-b4cc-8d6f2c8a44b1"
        ))
        .is_err(),
        "binding identifiers must be random UUIDv4 values"
    );

    let noncanonical_root = root.path().join(".");
    let noncanonical = base.replace(
        canonical.to_str().unwrap(),
        noncanonical_root.to_str().unwrap(),
    );
    assert!(matches!(
        TrustRegistry::parse(&noncanonical),
        Err(TrustError::InvalidStoredRoot { .. })
    ));
}

// Persists a registry, which requires a user-only permission primitive this
// target may not have. Selection behaviour itself is platform-independent.
#[cfg(unix)]
#[test]
fn a_deleted_registered_root_does_not_invalidate_other_bindings() {
    let stale_root = tempdir().unwrap();
    let active_root = tempdir().unwrap();
    let config = tempdir().unwrap();
    let path = config.path().join("trusted-projects.toml");
    let mut registry = TrustRegistry::new();
    registry.register(registration(stale_root.path())).unwrap();
    let active_binding = match registry.register(registration(active_root.path())).unwrap() {
        RegistrationOutcome::Created(binding) => binding,
        RegistrationOutcome::Existing(_) => unreachable!(),
    };
    registry.save_atomic(&path).unwrap();

    drop(stale_root);

    let loaded = TrustRegistry::load(&path).unwrap();
    assert_eq!(loaded.bindings().len(), registry.bindings().len());
    let selected = loaded
        .select(SelectionRequest {
            mode: SelectionMode::DirectCli,
            explicit: None,
            environment: None,
            canonical_root: Some(canonicalize_repository_root(active_root.path()).unwrap()),
            configured_default: None,
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.binding_id(), Some(active_binding.binding_id()));
}

#[test]
fn stored_roots_reject_every_noncanonical_lexical_spelling() {
    let root = tempdir().unwrap();
    let canonical = canonicalize_repository_root(root.path()).unwrap();
    let registry = TrustRegistry::from_bindings(vec![TrustBinding::from_registration(
        BindingId::from_uuid(Uuid::parse_str("018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1").unwrap()),
        registration(root.path()),
    )])
    .unwrap();
    let base = registry.to_toml().unwrap();
    let canonical = canonical.to_str().unwrap();
    let spellings = [
        format!("{canonical}/."),
        format!("{canonical}/.."),
        format!("{canonical}//child"),
        format!("{canonical}/"),
    ];

    for spelling in spellings {
        let input = base.replace(canonical, &spelling);
        assert!(
            matches!(
                TrustRegistry::parse(&input),
                Err(TrustError::InvalidStoredRoot { .. })
            ),
            "accepted noncanonical stored root {spelling:?}"
        );
    }
}

#[test]
fn direct_selection_obeys_fixed_precedence() {
    let root = tempdir().unwrap();
    let canonical_root = canonicalize_repository_root(root.path()).unwrap();
    let mut registry = TrustRegistry::new();
    let trusted = match registry.register(registration(root.path())).unwrap() {
        RegistrationOutcome::Created(binding) => binding,
        RegistrationOutcome::Existing(_) => unreachable!(),
    };
    let explicit = LogicalSelection::parse("explicit", "explicit-project").unwrap();
    let environment = LogicalSelection::parse("environment", "environment-project").unwrap();
    let default = LogicalSelection::parse("default", "default-project").unwrap();

    let selected = registry
        .select(SelectionRequest {
            mode: SelectionMode::DirectCli,
            explicit: Some(ExplicitSelection::Logical(explicit.clone())),
            environment: Some(environment.clone()),
            canonical_root: Some(canonical_root.clone()),
            configured_default: Some(default.clone()),
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.source(), SelectionSource::Explicit);
    assert_eq!(selected.index_name(), explicit.index_name());

    let selected = registry
        .select(SelectionRequest {
            mode: SelectionMode::DirectCli,
            explicit: None,
            environment: Some(environment.clone()),
            canonical_root: Some(canonical_root.clone()),
            configured_default: Some(default.clone()),
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.source(), SelectionSource::Environment);
    assert_eq!(selected.index_name(), environment.index_name());

    let selected = registry
        .select(SelectionRequest {
            mode: SelectionMode::DirectCli,
            explicit: None,
            environment: None,
            canonical_root: Some(canonical_root),
            configured_default: Some(default.clone()),
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.source(), SelectionSource::TrustedRoot);
    assert_eq!(selected.binding_id(), Some(trusted.binding_id()));

    let selected = TrustRegistry::new()
        .select(SelectionRequest {
            mode: SelectionMode::DirectCli,
            explicit: None,
            environment: None,
            canonical_root: None,
            configured_default: Some(default.clone()),
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.source(), SelectionSource::ConfiguredDefault);
    assert_eq!(selected.index_name(), default.index_name());
}

#[test]
fn ambiguous_trusted_root_state_is_rejected_before_selection() {
    let root = tempdir().unwrap();
    let request = registration(root.path());
    let first = TrustBinding::from_registration(
        BindingId::from_uuid(Uuid::parse_str("018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b1").unwrap()),
        request.clone(),
    );
    let mut second_request = request;
    second_request.project_id = ProjectId::new_v4();
    second_request.project_name = SafeSlug::new("other-project").unwrap();
    let second = TrustBinding::from_registration(
        BindingId::from_uuid(Uuid::parse_str("018f47f0-9d9a-4a63-b4cc-8d6f2c8a44b2").unwrap()),
        second_request,
    );
    assert!(matches!(
        TrustRegistry::from_bindings(vec![first, second]),
        Err(TrustError::ConflictingRoot { root: actual })
            if actual == canonicalize_repository_root(root.path()).unwrap()
    ));
}

#[test]
fn mcp_requires_a_trusted_binding_and_ignores_direct_cli_sources() {
    let root = tempdir().unwrap();
    let mut registry = TrustRegistry::new();
    let binding = match registry.register(registration(root.path())).unwrap() {
        RegistrationOutcome::Created(binding) => binding,
        RegistrationOutcome::Existing(_) => unreachable!(),
    };
    let logical = LogicalSelection::parse("environment", "environment-project").unwrap();

    assert!(matches!(
        registry.select(SelectionRequest {
            mode: SelectionMode::Mcp,
            explicit: None,
            environment: Some(logical.clone()),
            canonical_root: None,
            configured_default: Some(logical),
            pointer: None,
        }),
        Err(SelectionError::TrustRequired)
    ));

    let selected = registry
        .select(SelectionRequest {
            mode: SelectionMode::Mcp,
            explicit: Some(ExplicitSelection::Binding(binding.binding_id())),
            environment: None,
            canonical_root: None,
            configured_default: None,
            pointer: None,
        })
        .unwrap();
    assert_eq!(selected.source(), SelectionSource::Explicit);
    assert_eq!(selected.binding_id(), Some(binding.binding_id()));
}
