//! The writer: puts a row's cell into an intent, the inverse of its projection.

use serde_json::{Value, json};

use super::*;
use crate::config::service_changes::{default_value, keep_branch};
use crate::config::{
    ConfigError, SavedEnvironmentIntent as Intent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, ServiceGitBranch, ServiceImageCredentials, ServiceSource, VolumeAttachment,
};

/// Give `intent` `cell` at `row`, the inverse of the projection a plan's cells come
/// from. A node arrives only whole, through [`Plan::apply`]; `Absent` removes it.
///
/// # Errors
/// Returns ConfigError when `cell` can't be held at `row`: a value of the wrong shape, a
/// mount on a Volume `intent` lacks, a setting each Environment owns, or a new node.
pub fn put(intent: &Intent, row: &RowId, cell: &Cell) -> Result<Intent, ConfigError> {
    let mut intent = intent.clone();
    put_into(&mut intent, row, cell)?;
    Ok(intent)
}

/// [`put`], but a row whose node (or a mount's Volume) is gone from `intent` stays as
/// it is: what rewinds a base.
///
/// # Errors
/// Returns ConfigError where [`put`] does on a node that is there.
pub fn put_back(intent: &Intent, row: &RowId, cell: &Cell) -> Result<Intent, ConfigError> {
    // ponytail: so a discarded Follow removal is not offered again: the node it would
    // bring back reads as the receiver's own. Put it back whole if that matters.
    let nodes = nodes(intent);
    let gone = |lineage: &str| !nodes.contains_key(lineage);
    if gone(&row.lineage) || matches!(&row.at, At::Mount(volume) if gone(volume)) {
        return Ok(intent.clone());
    }
    put(intent, row, cell)
}

/// [`put_into`], a secret with its value.
pub(super) fn put_sealed(
    env: &mut Intent,
    row: &RowId,
    cell: &SealedCell,
) -> Result<(), ConfigError> {
    put_into(env, row, &cell.to_redacted())?;
    if let (SealedCell::Secret(secret), At::Variable(key)) = (cell, &row.at)
        && let Some(variable) = service_mut(env, &row.lineage)
            .and_then(|service| service.variables.iter_mut().find(|v| v.key == *key))
    {
        variable.value = SavedVariableValue::Secret {
            encrypted_value: Some(secret.value.clone()),
        };
    }
    Ok(())
}

pub(super) fn put_into(env: &mut Intent, row: &RowId, cell: &Cell) -> Result<(), ConfigError> {
    let invalid = |message: &str| ConfigError::at(&row.to_string(), message);
    let lineage = row.lineage.as_str();
    // Clearing anything on a node that isn't there is already done.
    let missing = || match cell {
        Cell::Absent => Ok(()),
        Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue => {
            Err(invalid("The node isn't in this configuration"))
        }
    };
    match &row.at {
        At::Node => match cell {
            Cell::Absent => {
                env.services.retain(|s| s.lineage_id != lineage);
                env.volumes.retain(|v| v.resource_lineage_id != lineage);
            }
            // ponytail: a node row only adds or removes its node; a rename doesn't move.
            Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue
                if nodes(env).contains_key(lineage) => {}
            Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue => {
                return Err(invalid("A new node arrives whole, through a Sync"));
            }
        },
        At::Data => return Err(invalid("A Volume's data never moves")),
        At::Name | At::Storage => {
            let Some(target) = env
                .volumes
                .iter_mut()
                .find(|v| v.resource_lineage_id == lineage)
            else {
                return missing();
            };
            let Cell::Value(value) = cell else {
                return Err(invalid("A Volume always has a name and storage"));
            };
            let wrong = |_| invalid("Not a value this holds");
            if row.at == At::Name {
                target.name = serde_json::from_value(value.clone()).map_err(wrong)?;
            } else {
                target.storage = serde_json::from_value(value.clone()).map_err(wrong)?;
            }
        }
        At::Mount(volume_lineage) => {
            let mounted = volume(env, volume_lineage).map(|v| v.resource_id.clone());
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let attachments = &mut service.volume_attachments;
            match (cell, mounted) {
                (Cell::Absent, Some(id)) => attachments.retain(|a| a.volume_resource_id != id),
                (Cell::Absent, None) => {}
                (Cell::Value(Value::String(path)), Some(id)) => {
                    attachments.retain(|a| a.volume_resource_id != id);
                    attachments.push(VolumeAttachment {
                        volume_resource_id: id,
                        mount_path: path.clone(),
                    });
                }
                (Cell::Value(_), None) => {
                    return Err(ConfigError::at(
                        "picks",
                        "Mounted Volume is not in the receiving configuration",
                    ));
                }
                (Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue, _) => {
                    return Err(invalid("Not a mount path"));
                }
            }
        }
        At::Variable(key) => {
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let (value, fingerprint) = match cell {
                Cell::Absent => {
                    service.variables.retain(|v| v.key != *key);
                    return Ok(());
                }
                Cell::Value(value) => {
                    plain(value).ok_or_else(|| invalid("Not a variable's value"))?
                }
                Cell::Secret { fingerprint } => (
                    SavedVariableValue::Secret {
                        encrypted_value: None,
                    },
                    fingerprint.clone(),
                ),
                Cell::SecretWithoutValue => (SavedVariableValue::SecretWithoutValue, String::new()),
            };
            match service.variables.iter_mut().find(|v| v.key == *key) {
                Some(variable) => {
                    variable.value = value;
                    variable.value_fingerprint = fingerprint;
                }
                None => service.variables.push(SavedVariableIntent {
                    id: uuid::Uuid::new_v4().to_string(),
                    key: key.clone(),
                    description: None,
                    exported: false,
                    value_fingerprint: fingerprint,
                    value,
                }),
            }
        }
        At::Setting(setting) => {
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let value = match cell {
                Cell::Absent => default_value(setting.path()),
                Cell::Value(value) => value.clone(),
                Cell::Secret { .. } | Cell::SecretWithoutValue => {
                    return Err(invalid("A setting is never secret"));
                }
            };
            put_setting(service, *setting, value)
                .map_err(|_| invalid("Not a value this setting takes"))?;
        }
    }
    Ok(())
}

/// Write `setting` of `service` as `value` says it. Settings each Environment owns
/// never move, so nothing writes their rows.
pub(super) fn put_setting(
    service: &mut SavedServiceIntent,
    setting: Setting,
    value: Value,
) -> Result<(), serde_json::Error> {
    use serde_json::from_value as parse;
    let id = service.id.clone();
    let config = &mut service.config;
    match (setting, value) {
        (Setting::Routes | Setting::ManagedHostnames, _) => {
            return Err(serde::de::Error::custom("never moves"));
        }
        (Setting::Source, mut value) => {
            let fields = value
                .as_object_mut()
                .ok_or_else(|| serde::de::Error::custom("not a source"))?;
            // A git source parses with the branch it carries, else none yet.
            if fields.get("type") == Some(&json!("git")) {
                fields.entry("branch").or_insert_with(|| {
                    json!(ServiceGitBranch::Disconnected {
                        previous_name: None
                    })
                });
            }
            // Each Service's credential is its own: it is bound to the Service's id.
            if let Some(credentials) = fields.get_mut("credentials") {
                *credentials = match parse(credentials.take())? {
                    true => json!(ServiceImageCredentials::Configured { credential_id: id }),
                    false => json!(ServiceImageCredentials::None),
                };
            }
            config.source = keep_branch(&config.source, parse(value)?);
        }
        (Setting::Branch, value) => {
            if let ServiceSource::Git { branch, .. } = &mut config.source
                && !value.is_null()
            {
                *branch = parse(value)?;
            }
        }
        (Setting::PrivateDns, value) => config.private_dns = parse(value)?,
        (Setting::PreDeployCommand, value) => config.pre_deploy_command = parse(value)?,
        (Setting::StartCommand, value) => config.start_command = parse(value)?,
        (Setting::Healthcheck, value) => config.healthcheck = parse(value)?,
        (Setting::RestartPolicy, value) => config.restart_policy = parse(value)?,
        (Setting::MaxRetries, value) => config.max_retries = parse(value)?,
        (Setting::Replicas, value) => config.replicas = parse(value)?,
        (Setting::CpuLimit, value) => config.cpu_limit = parse(value)?,
        (Setting::MemLimit, value) => config.mem_limit = parse(value)?,
        (Setting::BuildMethod, value) => config.build.build_method = parse(value)?,
        (Setting::DockerfilePath, value) => config.build.dockerfile_path = parse(value)?,
        (Setting::BuildCommand, value) => config.build.command = parse(value)?,
    }
    Ok(())
}

/// A plain variable's cell back as its value and fingerprint.
fn plain(cell: &Value) -> Option<(SavedVariableValue, String)> {
    let mut value = cell.clone();
    let fingerprint = value.as_object_mut()?.remove("fingerprint")?;
    let value: SavedVariableValue = serde_json::from_value(value).ok()?;
    match value {
        SavedVariableValue::Literal { .. } | SavedVariableValue::Template { .. } => {
            Some((value, fingerprint.as_str()?.to_owned()))
        }
        SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue => None,
    }
}
