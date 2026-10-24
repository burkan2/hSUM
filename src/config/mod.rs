mod migration;
mod paths;
mod pointer;
mod safe_file;
mod selection;
mod trust;

pub use migration::{
    CONFIG_SCHEMA_VERSION, ConfigArtifactKind, ConfigMigrationArtifact, ConfigMigrationError,
    ConfigMigrationOutcome, ConfigMigrationPlan, PREVIOUS_CONFIG_SCHEMA_VERSION,
    USER_CONFIG_MAX_BYTES, apply_config_migration, plan_config_migration,
};
pub use paths::{ManagedPaths, ManagedPathsError};
pub use pointer::{POINTER_FILE_NAME, PointerError, RepositoryPointer};
pub(crate) use safe_file::{BoundedReadError, read_bounded_file};
pub use selection::{
    ExplicitSelection, LogicalSelection, LogicalSelectionError, SelectedContext, SelectionError,
    SelectionMode, SelectionRequest, SelectionSource,
};
pub use trust::{
    AtomicSaveOutcome, BindingId, BindingIdParseError, RegistrationOutcome, RetargetOutcome,
    TRUST_PREVIOUS_SCHEMA_VERSION, TRUST_SCHEMA_VERSION, TrustBinding, TrustError,
    TrustRegistration, TrustRegistry, canonicalize_repository_root,
};
