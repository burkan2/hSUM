mod paths;
mod pointer;
mod safe_file;
mod selection;
mod trust;

pub use paths::{ManagedPaths, ManagedPathsError};
pub use pointer::{POINTER_FILE_NAME, PointerError, RepositoryPointer};
pub(crate) use safe_file::{BoundedReadError, read_bounded_file};
pub use selection::{
    ExplicitSelection, LogicalSelection, LogicalSelectionError, SelectedContext, SelectionError,
    SelectionMode, SelectionRequest, SelectionSource,
};
pub use trust::{
    AtomicSaveOutcome, BindingId, BindingIdParseError, RegistrationOutcome, TrustBinding,
    TrustError, TrustRegistration, TrustRegistry, canonicalize_repository_root,
};
