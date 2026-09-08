//! Restore authored settings and relationships from an available publication baseline.

use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::*;

/// Environment Node families with distinct configuration and restore semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentNodeType {
    Service,
    VariableGroup,
    Volume,
}

impl EnvironmentNodeType {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::VariableGroup => "variable_group",
            Self::Volume => "volume",
        }
    }
}

/// Restore authored owners and their edges; runtime observations are never a baseline.
///
/// # Errors
/// Returns ConfigError when a setting has no authored baseline, the requested change cannot be restored,
/// or restoration would leave invalid owner relationships.
pub fn restore_environment_node(
    mut current: SavedEnvironmentIntent,
    baseline: Option<&SavedEnvironmentIntent>,
    node_type: EnvironmentNodeType,
    node_id: &str,
    path: Option<&str>,
) -> Result<SavedEnvironmentIntent, ConfigError> {
    if let Some(path) = path {
        if node_type != EnvironmentNodeType::Service {
            return Err(ConfigError::at(
                "path",
                "Resource settings restore with their owner",
            ));
        }
        let service = current
            .services
            .iter_mut()
            .find(|s| s.id == node_id)
            .ok_or_else(|| ConfigError::at("service", "Current service is unavailable"))?;
        let prior = baseline
            .and_then(|b| b.services.iter().find(|s| s.id == node_id))
            .ok_or_else(|| ConfigError::at("service", "Authored baseline is unavailable"))?;
        if path == "variableGroupAttachments" {
            if service.variable_group_attachments == prior.variable_group_attachments {
                return Err(ConfigError::at(
                    "path",
                    "Requested setting is not a restorable change",
                ));
            }
            service
                .variable_group_attachments
                .clone_from(&prior.variable_group_attachments);
        } else {
            service
                .configuration
                .restore_setting(&prior.configuration, path)?;
        }
    } else {
        match node_type {
            EnvironmentNodeType::Service => {
                current.services.retain(|s| s.id != node_id);
                if let Some(prior) =
                    baseline.and_then(|b| b.services.iter().find(|s| s.id == node_id))
                {
                    current.services.push(prior.clone());
                }
            }
            EnvironmentNodeType::VariableGroup => {
                let old = current
                    .variable_groups
                    .iter()
                    .find(|g| g.resource_id == node_id)
                    .cloned();
                let prior = baseline
                    .and_then(|b| b.variable_groups.iter().find(|g| g.resource_id == node_id));
                current.variable_groups.retain(|g| g.resource_id != node_id);
                if let Some(prior) = prior {
                    current.variable_groups.push(prior.clone());
                }
                for service in &mut current.services {
                    match (&old, prior) {
                        (None, Some(prior)) => {
                            let attachment = baseline
                                .and_then(|b| b.services.iter().find(|s| s.id == service.id))
                                .and_then(|s| {
                                    s.variable_group_attachments
                                        .iter()
                                        .find(|a| a.variable_group_id == prior.variable_group_id)
                                });
                            if let Some(attachment) = attachment
                                && !service
                                    .variable_group_attachments
                                    .iter()
                                    .any(|a| a.variable_group_id == attachment.variable_group_id)
                            {
                                service.variable_group_attachments.push(attachment.clone());
                            }
                        }
                        (Some(old), Some(prior)) => {
                            for attachment in &mut service.variable_group_attachments {
                                if attachment.variable_group_id == old.variable_group_id {
                                    attachment.variable_group_id = prior.variable_group_id.clone();
                                }
                            }
                        }
                        (Some(old), None) => service
                            .variable_group_attachments
                            .retain(|a| a.variable_group_id != old.variable_group_id),
                        (None, None) => {}
                    }
                }
            }
            EnvironmentNodeType::Volume => {
                let existed = current.volumes.iter().any(|v| v.resource_id == node_id);
                let prior =
                    baseline.and_then(|b| b.volumes.iter().find(|v| v.resource_id == node_id));
                current.volumes.retain(|v| v.resource_id != node_id);
                if let Some(prior) = prior {
                    current.volumes.push(prior.clone());
                }
                for service in &mut current.services {
                    if prior.is_none() {
                        service
                            .volume_attachments
                            .retain(|a| a.volume_resource_id != node_id);
                    } else if !existed {
                        let attachment = baseline
                            .and_then(|b| b.services.iter().find(|s| s.id == service.id))
                            .and_then(|s| {
                                s.volume_attachments
                                    .iter()
                                    .find(|a| a.volume_resource_id == node_id)
                            });
                        if let Some(attachment) = attachment
                            && !service
                                .volume_attachments
                                .iter()
                                .any(|a| a.volume_resource_id == node_id)
                        {
                            service.volume_attachments.push(attachment.clone());
                        }
                    }
                }
            }
        }
    }
    Ok(canonicalize_environment_intent(parse_environment_intent(
        json!(current),
    )?))
}
