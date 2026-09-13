//! Pure authored configuration rules used by native clients and Cloud's canvas.

mod change_set;
mod change_set_types;
mod environment;
mod environment_compile;
mod environment_restore;
mod lowering;
mod publication;
mod resource_changes;
mod runtime_outcome;
mod service;
mod service_changes;
mod validation;
mod variables;

pub use change_set::*;
pub use change_set_types::*;
pub use environment::*;
pub use environment_compile::*;
pub use environment_restore::*;
pub use lowering::*;
pub use publication::*;
pub use resource_changes::*;
pub use runtime_outcome::*;
pub use service::*;
pub use service_changes::*;
pub use validation::*;
pub use variables::*;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigRequest {
    ParseRuntimePreview {
        value: serde_json::Value,
    },
    ProjectRuntimeOutcome {
        preview: serde_json::Value,
        value: serde_json::Value,
    },
    RedactEnvironment {
        value: serde_json::Value,
    },
    LowerDeployment {
        value: LowerDeploymentInput,
    },
    ReusePublication {
        policy: PublicationRevisionPolicy,
        current: PublicationCandidate,
        latest: Option<PublicationCandidate>,
    },
    ParsePublicationBasis {
        value: serde_json::Value,
    },
    PublicationBasisMatches {
        basis: PublicationBasis,
        latest: Option<String>,
    },
    DestructivePublication {
        value: DestructivePublicationInput,
    },
    DestructivePublicationMismatch {
        expected: DestructivePublication,
        reviewed: DestructivePublication,
    },
    CanonicalWorkingReview {
        value: ReviewedWorkingState,
    },
    ProjectChanges {
        value: Box<ChangeSetInput>,
    },
    ParseResource {
        node_type: EnvironmentNodeType,
        value: serde_json::Value,
    },
    CompareResource {
        node_type: EnvironmentNodeType,
        current: serde_json::Value,
        baseline: Option<serde_json::Value>,
    },
    RestoreEnvironment {
        current: serde_json::Value,
        baseline: Option<serde_json::Value>,
        node_type: EnvironmentNodeType,
        node_id: String,
        path: Option<String>,
    },
    CompileEnvironment {
        environment_id: String,
        value: serde_json::Value,
    },
    RenderVariableParts {
        parts: Vec<ValuePart>,
        slugs: std::collections::BTreeMap<String, String>,
    },
    ParseSavedVariable {
        value: serde_json::Value,
    },
    ParseEnvironment {
        value: serde_json::Value,
    },
    CanonicalizeEnvironment {
        value: serde_json::Value,
    },
    ResolveVariables {
        value: ResolveVariablesInput,
    },
    ParseService {
        value: serde_json::Value,
    },
    ParseSetting {
        value: serde_json::Value,
    },
    CompareService {
        current: serde_json::Value,
        baseline: Option<serde_json::Value>,
    },
    RestoreService {
        current: serde_json::Value,
        baseline: serde_json::Value,
        path: String,
    },
}

/// The JSON ABI is shared by native and browser exports; both validate before policy.
///
/// # Errors
/// Returns ConfigError for an unknown request or any failed admission or restore operation.
/// Messages identify the rejected setting without echoing its value.
pub fn config_request(input: serde_json::Value) -> Result<serde_json::Value, ConfigError> {
    let input = serde_json::from_value(input)
        .map_err(|_| ConfigError::at("request", "Invalid configuration request"))?;
    Ok(match input {
        ConfigRequest::ParseRuntimePreview { value } => {
            serde_json::json!(parse_runtime_preview(value)?)
        }
        ConfigRequest::ProjectRuntimeOutcome { preview, value } => {
            serde_json::json!(project_runtime_outcome(preview, value)?)
        }
        ConfigRequest::RedactEnvironment { value } => {
            serde_json::json!(redact_environment_intent(parse_environment_intent(value)?))
        }
        ConfigRequest::LowerDeployment { value } => serde_json::json!(lower_deployment(value)?),
        ConfigRequest::ReusePublication {
            policy,
            current,
            latest,
        } => serde_json::json!(reuse_publication(policy, current, latest)?),
        ConfigRequest::ParsePublicationBasis { value } => {
            serde_json::json!(parse_publication_basis(value)?)
        }
        ConfigRequest::PublicationBasisMatches { basis, latest } => {
            serde_json::json!(publication_basis_matches(&basis, latest.as_deref()))
        }
        ConfigRequest::DestructivePublication { value } => {
            serde_json::json!(destructive_publication(value))
        }
        ConfigRequest::DestructivePublicationMismatch { expected, reviewed } => {
            serde_json::json!(destructive_publication_mismatch(expected, reviewed))
        }
        ConfigRequest::CanonicalWorkingReview { value } => {
            serde_json::json!(canonical_reviewed_working_state(value)?)
        }
        ConfigRequest::ProjectChanges { value } => {
            serde_json::json!(project_environment_changes(*value)?)
        }
        ConfigRequest::ParseResource { node_type, value } => {
            parse_resource_config(node_type, value)?
        }
        ConfigRequest::CompareResource {
            node_type,
            current,
            baseline,
        } => serde_json::json!(compare_resource_settings(node_type, current, baseline)?),
        ConfigRequest::RestoreEnvironment {
            current,
            baseline,
            node_type,
            node_id,
            path,
        } => {
            let current = parse_environment_intent(current)?;
            let baseline = baseline.map(parse_environment_intent).transpose()?;
            serde_json::json!(restore_environment_node(
                current,
                baseline.as_ref(),
                node_type,
                &node_id,
                path.as_deref()
            )?)
        }
        ConfigRequest::CompileEnvironment {
            environment_id,
            value,
        } => serde_json::json!(compile_environment_intent(
            &environment_id,
            parse_environment_intent(value)?
        )),
        ConfigRequest::RenderVariableParts { parts, slugs } => {
            serde_json::json!(render_variable_parts(&parts, &slugs))
        }
        ConfigRequest::ParseSavedVariable { value } => {
            serde_json::json!(parse_saved_variable(value)?)
        }
        ConfigRequest::ParseEnvironment { value } => {
            serde_json::json!(parse_environment_intent(value)?)
        }
        ConfigRequest::CanonicalizeEnvironment { value } => serde_json::json!(
            canonicalize_environment_intent(parse_environment_intent(value)?)
        ),
        ConfigRequest::ResolveVariables { value } => serde_json::json!(resolve_variables(&value)),
        ConfigRequest::ParseService { value } => serde_json::json!(parse_service_config(value)?),
        ConfigRequest::ParseSetting { value } => parse_service_setting(value)?,
        ConfigRequest::CompareService { current, baseline } => {
            let current = parse_service_config(current)?;
            let baseline = baseline.map(parse_service_config).transpose()?;
            serde_json::json!(compare_service_settings(&current, baseline.as_ref()))
        }
        ConfigRequest::RestoreService {
            current,
            baseline,
            path,
        } => {
            let current = parse_service_config(current)?;
            let baseline = parse_service_config(baseline)?;
            serde_json::json!(restore_service_setting(current, &baseline, &path)?)
        }
    })
}
