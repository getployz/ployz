//! Current SDK outcome admission and per-Service completion evidence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use ts_rs::TS;

use super::ConfigError;
use crate::{
    DeployOperation, DeployOutcome, DeployPreview, ExecutionError, FailedOperation, ServiceName,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeOutcomeEvidence {
    version: u8,
    outcome: DeployOutcome<ExecutionError>,
}

/// Sanitized failure kinds; provider details remain inside encrypted evidence.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFailureKind {
    /// A Machine request failed.
    Machine,
    /// A replacement or new container failed health monitoring.
    Health,
    /// A dependency did not become healthy.
    DependencyHealth,
    /// A lifecycle hook failed.
    Hook,
    /// Execution was cancelled.
    Cancelled,
}

/// Public counts contain no operation inputs or provider error messages.
#[derive(Debug, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeOutcomeSummary {
    /// Every operation completed.
    Success {
        /// Number of completed operations.
        completed: usize,
    },
    /// Execution or preflight stopped before the whole plan completed.
    Failed {
        /// Number of completed operations.
        completed: usize,
        /// Number of operations never attempted, excluding the failed operation.
        unexecuted: usize,
        /// The failed operation's sanitized failure kind.
        reason: RuntimeFailureKind,
    },
}

/// Services whose complete set of planned operations is confirmed by this outcome.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOutcomeProjection {
    /// Sanitized whole-attempt counts and disposition.
    pub summary: RuntimeOutcomeSummary,
    /// Services whose every planned operation completed, ordered by name.
    pub confirmed_services: Vec<ServiceName>,
}

fn invalid() -> ConfigError {
    ConfigError::at("runtimeOutcome", "Invalid current SDK deployment evidence")
}

// Serde owns the wire types. Reject keys it would otherwise silently discard,
// including nested operation fields, while allowing its documented defaults.
fn preserves_input(input: &Value, decoded: &Value) -> bool {
    match (input, decoded) {
        (Value::Object(input), Value::Object(decoded)) => input.iter().all(|(key, value)| {
            decoded
                .get(key)
                .is_some_and(|decoded| preserves_input(value, decoded))
        }),
        (Value::Array(input), Value::Array(decoded)) => {
            input.len() == decoded.len()
                && input
                    .iter()
                    .zip(decoded)
                    .all(|(input, decoded)| preserves_input(input, decoded))
        }
        _ => input == decoded,
    }
}

fn decode<T: DeserializeOwned + Serialize>(value: Value) -> Result<T, ConfigError> {
    let decoded: T = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    if !preserves_input(
        &value,
        &serde_json::to_value(&decoded).map_err(|_| invalid())?,
    ) {
        return Err(invalid());
    }
    Ok(decoded)
}

fn redact_operation(operation: &mut DeployOperation) -> Result<(), ConfigError> {
    let spec = match operation {
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => spec,
        DeployOperation::ReplaceContainer(replacement) => &mut replacement.spec,
        DeployOperation::PrepareVolumes { .. }
        | DeployOperation::WaitHealthy { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. }
        | DeployOperation::StopHook { .. }
        | DeployOperation::RemoveVolume { .. } => return Ok(()),
    };
    spec.container.environment.clear();
    let mut configs = spec.configs().to_vec();
    for config in &mut configs {
        config.content.clear();
    }
    let mut requested = spec.to_requested();
    requested
        .set_config_graph(
            crate::ServiceConfigGraph::parse(configs, spec.config_mounts().to_vec())
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
    *spec = requested
        .to_resolved(spec.service_id, spec.update.clone())
        .map_err(|_| invalid())?;
    Ok(())
}

/// Validate the current SDK preview and remove resolved environment/config values.
///
/// # Errors
/// Rejects malformed SDK values and unknown fields without echoing their contents.
pub fn parse_runtime_preview(value: Value) -> Result<DeployPreview, ConfigError> {
    let mut preview: DeployPreview = decode(value)?;
    for row in &mut preview.operations {
        redact_operation(&mut row.operation)?;
    }
    Ok(preview)
}

/// Validate one version-1 SDK outcome against its exact preview, then identify
/// Services whose every planned operation completed. A partial replica update,
/// failed replacement, or skipped hook cannot advance a whole Service.
///
/// # Errors
/// Rejects unsupported versions, malformed evidence, missing Service identities,
/// and outcomes whose operations do not match the preview.
pub fn project_runtime_outcome(
    preview: Value,
    value: Value,
) -> Result<RuntimeOutcomeProjection, ConfigError> {
    let preview = parse_runtime_preview(preview)?;
    let evidence: RuntimeOutcomeEvidence = decode(value)?;
    if evidence.version != 1 {
        return Err(invalid());
    }
    let (summary, completed, pending) = match evidence.outcome {
        DeployOutcome::Success { completed } => (
            RuntimeOutcomeSummary::Success {
                completed: completed.len(),
            },
            completed,
            Vec::new(),
        ),
        DeployOutcome::Failed {
            completed,
            failed,
            mut unexecuted,
        } => {
            let (operation, error) = match failed {
                FailedOperation::Operation { operation, error } => (operation, error),
                FailedOperation::ReplacementHealth {
                    operation, error, ..
                } => (DeployOperation::ReplaceContainer(operation), error),
            };
            let reason = match error {
                ExecutionError::Machine { .. } => RuntimeFailureKind::Machine,
                ExecutionError::Health { .. } => RuntimeFailureKind::Health,
                ExecutionError::DependencyHealth { .. } => RuntimeFailureKind::DependencyHealth,
                ExecutionError::Hook { .. } => RuntimeFailureKind::Hook,
                ExecutionError::Cancelled => RuntimeFailureKind::Cancelled,
            };
            let summary = RuntimeOutcomeSummary::Failed {
                completed: completed.len(),
                unexecuted: unexecuted.len(),
                reason,
            };
            unexecuted.push(operation);
            (summary, completed, unexecuted)
        }
    };
    if completed.len() + pending.len() != preview.operations.len() {
        return Err(invalid());
    }
    let mut matched = vec![false; preview.operations.len()];
    let mut services = BTreeMap::new();
    for (mut operation, completed) in completed
        .into_iter()
        .map(|operation| (operation, true))
        .chain(pending.into_iter().map(|operation| (operation, false)))
    {
        redact_operation(&mut operation)?;
        // ponytail: quadratic matching for bounded plans; index operation identities if large plans make this measurable.
        let (index, row) = preview
            .operations
            .iter()
            .enumerate()
            .find(|(index, row)| {
                !matched.get(*index).copied().unwrap_or(true) && row.operation == operation
            })
            .ok_or_else(invalid)?;
        *matched.get_mut(index).ok_or_else(invalid)? = true;
        if row.machine_id != operation.machine_id() {
            return Err(invalid());
        }
        if let (Some(declared), Some(carried)) = (&row.service_name, operation.service_name())
            && declared != carried
        {
            return Err(invalid());
        }
        let service = row
            .service_name
            .as_ref()
            .or_else(|| operation.service_name());
        match service {
            Some(service) => {
                *services.entry(service.clone()).or_insert(true) &= completed;
            }
            None if matches!(
                operation,
                DeployOperation::PrepareVolumes { .. } | DeployOperation::RemoveVolume { .. }
            ) => {}
            None => return Err(invalid()),
        }
    }
    Ok(RuntimeOutcomeProjection {
        summary,
        confirmed_services: services
            .into_iter()
            .filter_map(|(service, complete)| complete.then_some(service))
            .collect(),
    })
}
