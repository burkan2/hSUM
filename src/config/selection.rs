use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::domain::{IndexId, ProjectId, SafeSlug, SlugError};

use super::{BindingId, RepositoryPointer, TrustBinding, TrustRegistry};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalSelection {
    index_name: SafeSlug,
    project_name: SafeSlug,
}

impl LogicalSelection {
    pub fn new(index_name: SafeSlug, project_name: SafeSlug) -> Self {
        Self {
            index_name,
            project_name,
        }
    }

    pub fn parse(index_name: &str, project_name: &str) -> Result<Self, LogicalSelectionError> {
        Ok(Self::new(
            SafeSlug::new(index_name).map_err(LogicalSelectionError::InvalidIndexName)?,
            SafeSlug::new(project_name).map_err(LogicalSelectionError::InvalidProjectName)?,
        ))
    }

    pub fn index_name(&self) -> &SafeSlug {
        &self.index_name
    }

    pub fn project_name(&self) -> &SafeSlug {
        &self.project_name
    }
}

#[derive(Debug, Error)]
pub enum LogicalSelectionError {
    #[error("index name is invalid: {0}")]
    InvalidIndexName(#[source] SlugError),
    #[error("project name is invalid: {0}")]
    InvalidProjectName(#[source] SlugError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionMode {
    DirectCli,
    Mcp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplicitSelection {
    Logical(LogicalSelection),
    Binding(BindingId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionSource {
    Explicit,
    Environment,
    TrustedRoot,
    ConfiguredDefault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionRequest {
    pub mode: SelectionMode,
    pub explicit: Option<ExplicitSelection>,
    pub environment: Option<LogicalSelection>,
    pub canonical_root: Option<PathBuf>,
    pub configured_default: Option<LogicalSelection>,
    pub pointer: Option<RepositoryPointer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedContext {
    index_name: SafeSlug,
    project_name: SafeSlug,
    index_id: Option<IndexId>,
    project_id: Option<ProjectId>,
    binding_id: Option<BindingId>,
    canonical_root: Option<PathBuf>,
    source: SelectionSource,
}

impl SelectedContext {
    pub fn index_name(&self) -> &SafeSlug {
        &self.index_name
    }

    pub fn project_name(&self) -> &SafeSlug {
        &self.project_name
    }

    pub const fn index_id(&self) -> Option<IndexId> {
        self.index_id
    }

    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }

    pub const fn binding_id(&self) -> Option<BindingId> {
        self.binding_id
    }

    pub fn canonical_root(&self) -> Option<&Path> {
        self.canonical_root.as_deref()
    }

    pub const fn source(&self) -> SelectionSource {
        self.source
    }

    fn logical(selection: LogicalSelection, source: SelectionSource) -> Self {
        Self {
            index_name: selection.index_name,
            project_name: selection.project_name,
            index_id: None,
            project_id: None,
            binding_id: None,
            canonical_root: None,
            source,
        }
    }

    fn trusted(binding: &TrustBinding, source: SelectionSource) -> Self {
        Self {
            index_name: binding.index_name().clone(),
            project_name: binding.project_name().clone(),
            index_id: Some(binding.index_id()),
            project_id: Some(binding.project_id()),
            binding_id: Some(binding.binding_id()),
            canonical_root: Some(binding.canonical_root().to_path_buf()),
            source,
        }
    }
}

impl TrustRegistry {
    pub fn select(&self, request: SelectionRequest) -> Result<SelectedContext, SelectionError> {
        if let Some(explicit) = request.explicit {
            return self.select_explicit(request.mode, explicit);
        }

        if request.mode == SelectionMode::DirectCli
            && let Some(environment) = request.environment
        {
            return Ok(SelectedContext::logical(
                environment,
                SelectionSource::Environment,
            ));
        }

        if let Some(root) = request.canonical_root.as_deref() {
            let matches = self
                .bindings()
                .iter()
                .filter(|binding| binding.canonical_root() == root)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => {}
                [binding] => {
                    return Ok(SelectedContext::trusted(
                        binding,
                        SelectionSource::TrustedRoot,
                    ));
                }
                _ => {
                    return Err(SelectionError::AmbiguousTrustedRoot {
                        matches: matches.len(),
                    });
                }
            }
        }

        if request.mode == SelectionMode::DirectCli
            && let Some(default) = request.configured_default
        {
            return Ok(SelectedContext::logical(
                default,
                SelectionSource::ConfiguredDefault,
            ));
        }

        if request.pointer.is_some() {
            return Err(SelectionError::PointerIsOnlyHint);
        }
        if request.mode == SelectionMode::Mcp {
            Err(SelectionError::TrustRequired)
        } else {
            Err(SelectionError::NotConfigured)
        }
    }

    fn select_explicit(
        &self,
        mode: SelectionMode,
        explicit: ExplicitSelection,
    ) -> Result<SelectedContext, SelectionError> {
        match explicit {
            ExplicitSelection::Logical(selection) if mode == SelectionMode::DirectCli => Ok(
                SelectedContext::logical(selection, SelectionSource::Explicit),
            ),
            ExplicitSelection::Logical(_) => Err(SelectionError::TrustRequired),
            ExplicitSelection::Binding(binding_id) => {
                let matches = self
                    .bindings()
                    .iter()
                    .filter(|binding| binding.binding_id() == binding_id)
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [] => Err(SelectionError::BindingNotTrusted { binding_id }),
                    [binding] => Ok(SelectedContext::trusted(binding, SelectionSource::Explicit)),
                    _ => Err(SelectionError::AmbiguousBinding {
                        binding_id,
                        matches: matches.len(),
                    }),
                }
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum SelectionError {
    #[error("repository pointer is a hint and no user trust binding exists")]
    PointerIsOnlyHint,
    #[error("MCP selection requires a trusted root or binding")]
    TrustRequired,
    #[error("no hSUM index and project are configured")]
    NotConfigured,
    #[error("binding {binding_id} is not trusted")]
    BindingNotTrusted { binding_id: BindingId },
    #[error("binding {binding_id} has {matches} registry matches")]
    AmbiguousBinding {
        binding_id: BindingId,
        matches: usize,
    },
    #[error("repository root has {matches} trusted bindings")]
    AmbiguousTrustedRoot { matches: usize },
}
