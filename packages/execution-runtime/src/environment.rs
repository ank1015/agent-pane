use execution_contracts::{
    ARTIFACTS_CAPABILITY, BASIC_FILESYSTEM_CAPABILITY, CODE_INTELLIGENCE_CAPABILITY,
    EnvironmentDescriptor, MEDIA_PROCESSING_CAPABILITY, PROCESS_SESSION_CAPABILITY, Validate,
    ValidationError, WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY,
};
use thiserror::Error;

use crate::{ArtifactStore, BasicFileSystem, ProcessRuntime, WorkspaceMutation, WorkspaceQuery};

/// Optional code-intelligence capability boundary.
///
/// Concrete operations will be added when their serializable contracts are
/// defined. The marker lets an environment compose and negotiate the capability
/// without coupling the core runtime to an LSP implementation.
pub trait CodeIntelligence: Send + Sync {}

/// Optional media-processing capability boundary.
///
/// Concrete operations will be added alongside their serializable contracts.
pub trait MediaProcessing: Send + Sync {}

/// Composition root for one executable target.
///
/// A machine may expose several environments, each with its own roots,
/// operating system, shell, and capabilities.
pub trait ExecutionEnvironment: Send + Sync {
    fn descriptor(&self) -> &EnvironmentDescriptor;

    fn workspace_query(&self) -> &dyn WorkspaceQuery;

    fn workspace_mutation(&self) -> Option<&dyn WorkspaceMutation> {
        None
    }

    fn process_runtime(&self) -> Option<&dyn ProcessRuntime> {
        None
    }

    fn artifact_store(&self) -> Option<&dyn ArtifactStore> {
        None
    }

    fn filesystem(&self) -> Option<&dyn BasicFileSystem> {
        None
    }

    fn code_intelligence(&self) -> Option<&dyn CodeIntelligence> {
        None
    }

    fn media_processing(&self) -> Option<&dyn MediaProcessing> {
        None
    }

    /// Checks that the descriptor and executable capability composition agree.
    fn validate_capabilities(&self) -> Result<(), CapabilityConsistencyError>
    where
        Self: Sized,
    {
        validate_capability_consistency(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityConsistencyIssue {
    pub capability: String,
    pub message: String,
}

/// One or more descriptor/runtime composition mismatches.
#[derive(Debug, Error)]
#[error("runtime capability configuration is inconsistent: {issues:?}")]
pub struct CapabilityConsistencyError {
    pub issues: Vec<CapabilityConsistencyIssue>,
    pub descriptor_error: Option<ValidationError>,
}

/// Validates the descriptor and verifies all built-in capability declarations.
///
/// Unknown capability IDs are allowed so extension crates can negotiate their
/// own versioned interfaces. Built-in capabilities currently support major v1.
pub fn validate_capability_consistency(
    environment: &dyn ExecutionEnvironment,
) -> Result<(), CapabilityConsistencyError> {
    let descriptor_error = environment.descriptor().validate().err();
    let advertised = &environment.descriptor().capabilities;
    let mut issues = Vec::new();

    check_builtin(advertised, WORKSPACE_QUERY_CAPABILITY, true, &mut issues);
    check_builtin(
        advertised,
        WORKSPACE_MUTATION_CAPABILITY,
        environment.workspace_mutation().is_some(),
        &mut issues,
    );
    check_builtin(
        advertised,
        PROCESS_SESSION_CAPABILITY,
        environment.process_runtime().is_some(),
        &mut issues,
    );
    check_builtin(
        advertised,
        ARTIFACTS_CAPABILITY,
        environment.artifact_store().is_some(),
        &mut issues,
    );
    check_builtin(
        advertised,
        BASIC_FILESYSTEM_CAPABILITY,
        environment.filesystem().is_some(),
        &mut issues,
    );
    check_builtin(
        advertised,
        CODE_INTELLIGENCE_CAPABILITY,
        environment.code_intelligence().is_some(),
        &mut issues,
    );
    check_builtin(
        advertised,
        MEDIA_PROCESSING_CAPABILITY,
        environment.media_processing().is_some(),
        &mut issues,
    );

    if issues.is_empty() && descriptor_error.is_none() {
        Ok(())
    } else {
        Err(CapabilityConsistencyError {
            issues,
            descriptor_error,
        })
    }
}

fn check_builtin(
    advertised: &[execution_contracts::Capability],
    capability_id: &str,
    implemented: bool,
    issues: &mut Vec<CapabilityConsistencyIssue>,
) {
    let declarations: Vec<_> = advertised
        .iter()
        .filter(|capability| capability.id.as_str() == capability_id)
        .collect();

    if implemented && declarations.is_empty() {
        issues.push(CapabilityConsistencyIssue {
            capability: capability_id.to_owned(),
            message: "implemented but not advertised".to_owned(),
        });
        return;
    }

    if !implemented && !declarations.is_empty() {
        issues.push(CapabilityConsistencyIssue {
            capability: capability_id.to_owned(),
            message: "advertised without a runtime implementation".to_owned(),
        });
    }

    for declaration in declarations {
        if declaration.major != 1 {
            issues.push(CapabilityConsistencyIssue {
                capability: capability_id.to_owned(),
                message: format!(
                    "unsupported major version {}; runtime supports v1",
                    declaration.major
                ),
            });
        }
    }
}
