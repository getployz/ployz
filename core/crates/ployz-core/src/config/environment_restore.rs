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
    Volume,
}

impl EnvironmentNodeType {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
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
        service.config = restore_service_setting(
            ServiceConfig::from(service.config.clone()),
            &ServiceConfig::from(prior.config.clone()),
            path,
        )?
        .settings;
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
