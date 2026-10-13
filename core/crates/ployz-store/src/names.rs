use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;
use serde_json::json;

use crate::error;
use crate::scope::Environment;
use crate::storage::Tx;

/// A name one node holds. No other node may hold it: a Service name or Private
/// DNS name within its Environment, a Volume or Config name within its
/// Environment, and a custom hostname or generated domain prefix across the
/// whole Organization.
#[derive(Clone, Copy)]
pub(crate) enum Name<'a> {
    Service(&'a str),
    Volume(&'a str),
    Config(&'a str),
    Hostname(&'a str),
    Prefix(&'a str),
}

/// Refuse `name` when a node of `environment` other than `except` holds it, or,
/// for a hostname, when any Service of another Environment of the Organization does.
pub(crate) fn refuse_taken(
    tx: &mut dyn Tx,
    environment: &Environment,
    name: Name<'_>,
    except: Option<&str>,
) -> Result<(), RpcError> {
    refuse_taken_among(tx, environment, name, except, &mut None)
}

/// [`refuse_taken`], loading the other Environments into `others` once for
/// every name checked against them.
fn refuse_taken_among(
    tx: &mut dyn Tx,
    environment: &Environment,
    name: Name<'_>,
    except: Option<&str>,
    others: &mut Option<Vec<SavedEnvironmentIntent>>,
) -> Result<(), RpcError> {
    let other = |id: &str| Some(id) != except;
    let working = &environment.working;
    let (kind, name, taken) = match name {
        Name::Service(name) => {
            if name == "volumes" || name == "configs" {
                return Err(error::invalid(
                    format!(
                        "{name} is reserved: paths name {} as {name}.NAME",
                        if name == "volumes" {
                            "Volumes"
                        } else {
                            "Configs"
                        }
                    ),
                    json!({ "service": name }),
                ));
            }
            let taken = working.services.iter().any(|service| {
                other(&service.id)
                    && (service.slug == name || service.config.private_dns.as_str() == name)
            });
            ("Service", name, taken)
        }
        Name::Volume(name) => {
            let taken = working
                .volumes
                .iter()
                .any(|volume| other(&volume.resource_id) && volume.name == name);
            ("Volume", name, taken)
        }
        Name::Config(name) => {
            let taken = working
                .configs
                .iter()
                .any(|config| other(&config.resource_id) && config.name.as_str() == name);
            ("Config", name, taken)
        }
        Name::Hostname(hostname) => {
            let routed = |intent: &SavedEnvironmentIntent| {
                intent.services.iter().any(|service| {
                    other(&service.id)
                        && service
                            .config
                            .routes
                            .iter()
                            .any(|route| route.hostname == hostname)
                })
            };
            if routed(working) || loaded(tx, environment, others)?.iter().any(routed) {
                return Err(error::conflict(
                    format!("Another Service already has {hostname}"),
                    json!({ "hostname": hostname }),
                ));
            }
            return Ok(());
        }
        Name::Prefix(prefix) => {
            let generated = |intent: &SavedEnvironmentIntent| {
                intent.services.iter().any(|service| {
                    other(&service.id)
                        && service
                            .config
                            .managed_hostnames
                            .iter()
                            .any(|managed| managed.prefix == prefix)
                })
            };
            if generated(working) || loaded(tx, environment, others)?.iter().any(generated) {
                return Err(error::conflict(
                    format!("Another generated domain of this Organization is {prefix}"),
                    json!({ "prefix": prefix }),
                ));
            }
            return Ok(());
        }
    };
    if taken {
        return Err(error::conflict(
            format!(
                "Environment {} already has a {kind} named {name}",
                environment.summary.name
            ),
            serde_json::Value::Object([(kind.to_lowercase(), json!(name))].into_iter().collect()),
        ));
    }
    Ok(())
}

/// Refuse each name `target` gives a node that the node doesn't hold in
/// `environment` now, when another node holds it.
pub(crate) fn refuse_new_taken(
    tx: &mut dyn Tx,
    environment: &Environment,
    target: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let now = &environment.working;
    let mut claims: Vec<(&str, Name<'_>)> = Vec::new();
    for service in &target.services {
        let held = now.services.iter().find(|held| held.id == service.id);
        for name in [service.slug.as_str(), service.config.private_dns.as_str()] {
            if held.is_none_or(|held| held.slug != name && held.config.private_dns.as_str() != name)
            {
                claims.push((&service.id, Name::Service(name)));
            }
        }
        for route in &service.config.routes {
            if held.is_none_or(|held| {
                held.config
                    .routes
                    .iter()
                    .all(|was| was.hostname != route.hostname)
            }) {
                claims.push((&service.id, Name::Hostname(&route.hostname)));
            }
        }
        for managed in &service.config.managed_hostnames {
            if held.is_none_or(|held| {
                held.config
                    .managed_hostnames
                    .iter()
                    .all(|was| was.prefix != managed.prefix)
            }) {
                claims.push((&service.id, Name::Prefix(&managed.prefix)));
            }
        }
    }
    for volume in &target.volumes {
        if !now
            .volumes
            .iter()
            .any(|held| held.resource_id == volume.resource_id && held.name == volume.name)
        {
            claims.push((&volume.resource_id, Name::Volume(&volume.name)));
        }
    }
    for config in &target.configs {
        if !now
            .configs
            .iter()
            .any(|held| held.resource_id == config.resource_id && held.name == config.name)
        {
            claims.push((&config.resource_id, Name::Config(config.name.as_str())));
        }
    }
    if claims.is_empty() {
        return Ok(());
    }
    let target = Environment {
        summary: environment.summary.clone(),
        working: target.clone(),
        live: environment.live.clone(),
    };
    let mut others = None;
    for (owner, name) in claims {
        refuse_taken_among(tx, &target, name, Some(owner), &mut others)?;
    }
    Ok(())
}

/// [`other_environments`], loaded into `others` on first use.
fn loaded<'a>(
    tx: &mut dyn Tx,
    environment: &Environment,
    others: &'a mut Option<Vec<SavedEnvironmentIntent>>,
) -> Result<&'a [SavedEnvironmentIntent], RpcError> {
    if others.is_none() {
        *others = Some(other_environments(tx, environment)?);
    }
    Ok(others.get_or_insert_with(Vec::new))
}

/// Working State of every other Environment of the Organization.
pub(crate) fn other_environments(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<Vec<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT working FROM config_environment WHERE id <> ?1 AND organization_id = \
         (SELECT organization_id FROM config_environment WHERE id = ?1)",
        &[environment.summary.id.as_str().into()],
    )?
    .iter()
    .map(|row| row.intent(0, "Working State"))
    .collect()
}
