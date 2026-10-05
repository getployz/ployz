//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::{
    At, ConfigAttachment, EnvironmentNodeType, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVariableIntent, ServiceConfig, Setting, VolumeAttachment, canonicalize_environment_intent,
    compare_service_settings, parse_environment_intent, restore_environment_node,
};
use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{Revision, VolumeName};
use crate::review::{self, Review};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{NodeName, ServiceSetting, SettingPath, Target, VolumeField};
use crate::storage::Tx;
use crate::{Actor, Trusted, deployment};

/// Put Working State in Saved State without deploying it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// The Environment to publish.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Refuse with `conflict` unless this is still the latest `diff` version, or the
    /// version a refusal to delete data handed back.
    #[serde(default)]
    pub version: Option<String>,
    /// Deployed Volumes whose removal it may publish, by name: the next full Deploy
    /// deletes their data. Publishing one refuses with `confirmation_required` unless
    /// it names each one and passes the `version` that refusal handed back.
    #[serde(default)]
    #[ts(as = "Option<Vec<VolumeName>>", optional)]
    pub accept_volume_loss: Vec<VolumeName>,
}

/// The Saved revision Working State is now in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Published {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// The Saved revision that now holds Working State; none when nothing was ever
    /// published and nothing is staged.
    pub saved: Option<Revision>,
    /// False when Saved State already held it, or nothing is staged.
    pub created: bool,
}

/// Undo staged changes: all of them, one node's, or one Setting, variable or mount's.
/// It names a [`SettingPath`] rather than a `RowId`, as the diff it undoes does:
/// it is a whole-setting action, not one of a Sync view's rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Discard {
    /// The Environment to discard in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// `SERVICE`, `volumes.VOLUME`, `configs.CONFIG`, `SERVICE.SETTING`,
    /// `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME` or `SERVICE.configs.CONFIG`; none
    /// discards everything.
    #[serde(default)]
    pub path: Option<SettingPath>,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The Environment after a discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Discarded {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// The latest Saved revision, which follows the discard so the next Deploy ships it.
    pub saved: Option<Revision>,
    /// False when nothing it names was staged.
    pub changed: bool,
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    publish: &Publish,
    trusted: &Trusted,
) -> Result<Published, RpcError> {
    let environment = scope::lock(tx, who, &publish.environment)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, publish.version.as_deref())?;
    if review.saved.is_none()
        && canonicalize_environment_intent(environment.working.clone())
            == canonicalize_environment_intent(review.head.intent.clone())
    {
        // Nothing staged: Head already is Working State.
        return Ok(Published {
            environment: environment.summary,
            saved: None,
            created: false,
        });
    }
    // Saved State then holds the removal, which the next full Deploy ships: the
    // same destructive review, before anything is saved.
    let target = canonicalize_environment_intent(environment.working.clone());
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    review::destructive(
        who,
        &review,
        (&target, &namespace, review::Shipping::Publish),
        (publish.version.as_deref(), &publish.accept_volume_loss),
        trusted.volumes.as_ref(),
    )?;
    let (saved, created) = review::publish(
        tx,
        who,
        &environment.summary.id,
        environment.working,
        review.saved.as_ref(),
    )?;
    Ok(Published {
        environment: environment.summary,
        saved: Some(saved),
        created,
    })
}

pub(crate) fn discard(
    tx: &mut dyn Tx,
    who: &Actor,
    discard: &Discard,
) -> Result<Discarded, RpcError> {
    let mut environment = scope::lock(tx, who, &discard.environment)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, discard.version.as_deref())?;
    let Review { saved, head, .. } = review;
    // Head is what runs: what it already breaks, a discard may bring back.
    let runs = head.intent.clone();
    let (working, restored) = match &discard.path {
        // Everything returns to Head, in Working and Saved State alike.
        None => (head.intent.clone(), saved.as_ref().map(|_| head.intent)),
        Some(path) => restore(
            tx,
            &environment,
            path,
            saved.as_ref().map(|saved| &saved.intent),
            head.intent,
        )?,
    };
    let mut revision = saved.as_ref().map(|saved| saved.revision);
    let mut changed = false;
    if let Some(restored) = restored {
        let (saved, created) =
            review::publish(tx, who, &environment.summary.id, restored, saved.as_ref())?;
        revision = Some(saved);
        changed = created;
    }
    if canonicalize_environment_intent(working.clone())
        != canonicalize_environment_intent(environment.working.clone())
    {
        changed = true;
        crate::branch::rewind(
            tx,
            &environment.summary.id,
            (&environment.working, &working),
        )?;
        environment.working = working;
        scope::save_working_from(tx, &mut environment, Some(&runs))?;
    }
    Ok(Discarded {
        environment: environment.summary,
        saved: revision,
        changed,
    })
}

/// Restore one node, or one Setting, variable or mount of a Service, in Working
/// State and, when Saved State follows, in Saved State too. Returns (Working, Saved
/// to publish).
fn restore(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    path: &SettingPath,
    saved: Option<&SavedEnvironmentIntent>,
    head: SavedEnvironmentIntent,
) -> Result<(SavedEnvironmentIntent, Option<SavedEnvironmentIntent>), RpcError> {
    let working = &environment.working;
    let node = match path.node() {
        NodeName::Service(name) => [working, &head]
            .into_iter()
            .flat_map(|intent| &intent.services)
            .find(|service| service.slug == name.as_str())
            .map(|service| (EnvironmentNodeType::Service, service.id.clone()))
            .ok_or_else(|| scope::no_service(&name, &environment.summary.name, working)),
        NodeName::Volume(name) => [working, &head]
            .into_iter()
            .flat_map(|intent| &intent.volumes)
            .find(|volume| volume.name == name.as_str())
            .map(|volume| (EnvironmentNodeType::Volume, volume.resource_id.clone()))
            .ok_or_else(|| {
                environment
                    .volume(&name)
                    .err()
                    .unwrap_or_else(|| error::corrupt("Volume"))
            }),
        NodeName::Config(name) => [working, &head]
            .into_iter()
            .flat_map(|intent| &intent.configs)
            .find(|config| config.name == name)
            .map(|config| (EnvironmentNodeType::Config, config.resource_id.clone()))
            .ok_or_else(|| {
                environment
                    .config(&name)
                    .err()
                    .unwrap_or_else(|| error::corrupt("Config"))
            }),
    };
    let (node_type, id) = node?;
    // A part of the source discards with it: the source is one change row.
    let part = path.target().map(|part| {
        if let Target::Setting(setting) = part {
            Cow::Owned(setting.covering())
        } else {
            Cow::Borrowed(part)
        }
    });
    let part = part.as_deref();
    let field = path.volume_field();
    let holds = |intent: &SavedEnvironmentIntent| match node_type {
        EnvironmentNodeType::Service => intent.services.iter().any(|service| service.id == id),
        EnvironmentNodeType::Volume => intent.volumes.iter().any(|volume| volume.resource_id == id),
        EnvironmentNodeType::Config => intent.configs.iter().any(|config| config.resource_id == id),
    };
    // A part of a node never deployed resets to its Introduction; Saved State follows
    // only where it holds the node already.
    let introduction = (part.is_some() || field.is_some()) && !holds(&head);
    let baseline = if introduction {
        review::introductions(tx, environment)?
    } else {
        head
    };
    let restore = |current: &SavedEnvironmentIntent| {
        let restored = match field {
            Some(field) => restore_field(current, &baseline, &id, field),
            None => restore_node(current, &baseline, (node_type, &id), part),
        };
        restored.map_err(|message| {
            error::conflict(
                format!("Discard would leave an invalid Environment: {message}"),
                json!({ "path": path }),
            )
        })
    };
    // Saved State follows, except for a part Saved State holds as it is at Head already.
    let saved_follows = match (part, saved) {
        (_, Some(saved)) if introduction && !holds(saved) => false,
        (None, saved) => field.is_none_or(|field| {
            let value = |intent: &SavedEnvironmentIntent| {
                intent
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == id)
                    .map(|volume| field.of(volume))
            };
            saved.is_some_and(|saved| value(saved) != value(&baseline))
        }),
        (Some(part @ (Target::Setting(_) | Target::Source)), Some(saved)) => {
            let config = |intent: &SavedEnvironmentIntent| {
                intent
                    .services
                    .iter()
                    .find(|service| service.id == id)
                    .map(|service| ServiceConfig::from(service.config.clone()))
            };
            config(saved)
                .zip(config(&baseline))
                .is_some_and(|(saved, head)| {
                    compare_service_settings(&saved, Some(&head))
                        .iter()
                        .any(|(row, at)| {
                            row.can_restore
                                && matches!(at, Some(At::Setting(changed))
                                if match ServiceSetting::of(*changed) {
                                    Some(setting) => Target::Setting(setting) == *part,
                                    None => *changed == Setting::Source && *part == Target::Source,
                                })
                        })
                })
        }
        (Some(part), Some(saved)) => part_of(saved, &id, part) != part_of(&baseline, &id, part),
        (Some(_), None) => false,
    };
    let saved = match saved {
        Some(saved) if saved_follows => Some(restore(saved)?),
        Some(_) | None => None,
    };
    Ok((restore(working)?, saved))
}

/// `current` with Volume `id`'s `field` as `baseline` has it.
fn restore_field(
    current: &SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    id: &str,
    field: VolumeField,
) -> Result<SavedEnvironmentIntent, String> {
    let gone = || "discard the whole Volume instead".to_owned();
    let mut restored = current.clone();
    let from = baseline
        .volumes
        .iter()
        .find(|volume| volume.resource_id == id)
        .ok_or_else(gone)?;
    let volume = restored
        .volumes
        .iter_mut()
        .find(|volume| volume.resource_id == id)
        .ok_or_else(gone)?;
    field.restore(volume, from);
    parse_environment_intent(serde_json::to_value(restored).expect("Working State is JSON"))
        .map_err(|error| error.message)
}

/// `current` with node `id`, or one part of it, as `baseline` has it.
fn restore_node(
    current: &SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    (node_type, id): (EnvironmentNodeType, &str),
    part: Option<&Target>,
) -> Result<SavedEnvironmentIntent, String> {
    let field = match part {
        None => None,
        Some(Target::Setting(setting)) => Some(setting.field()),
        Some(Target::Source) => Some("source"),
        Some(part) => {
            let mut restored = current.clone();
            let resource = mounted(current, baseline, part);
            let Some(service) = restored
                .services
                .iter_mut()
                .find(|service| service.id == id)
            else {
                return Err("its Service is gone".to_owned());
            };
            restore_part(service, baseline, (id, resource.as_deref()), part)?;
            return parse_environment_intent(
                serde_json::to_value(restored).expect("Working State is JSON"),
            )
            .map_err(|error| error.message);
        }
    };
    restore_environment_node(current.clone(), Some(baseline), node_type, id, field)
        .map_err(|error| error.message)
}

/// Give `service` the variable, its export, or the mount `part` names as Service
/// `id` in `baseline` has it: absent there, it goes. A mount names Volume or Config
/// `resource`.
fn restore_part(
    service: &mut SavedServiceIntent,
    baseline: &SavedEnvironmentIntent,
    (id, resource): (&str, Option<&str>),
    part: &Target,
) -> Result<(), String> {
    let was = part_of(baseline, id, part);
    match part {
        Target::Variable(key) | Target::Exported(key) => {
            let was = match was {
                Some(Part::Variable(was)) => Some(was),
                Some(Part::Mount(_) | Part::ConfigMount(_)) | None => None,
            };
            let variables = &mut service.variables;
            let at = variables.iter().position(|v| v.key == key.as_str());
            let exported = matches!(part, Target::Exported(_));
            match (at.and_then(|at| variables.get_mut(at)), was) {
                (Some(variable), Some(was)) if exported => variable.exported = was.exported,
                (Some(variable), None) if exported => variable.exported = false,
                (Some(variable), Some(was)) => {
                    *variable = SavedVariableIntent {
                        id: variable.id.clone(),
                        ..was.clone()
                    };
                }
                (Some(_), None) => variables.retain(|v| v.key != key.as_str()),
                (None, Some(was)) if !exported => variables.push(was.clone()),
                (None, Some(_) | None) => {}
            }
        }
        Target::Mount(_) => {
            let Some(volume) = resource else {
                return Err("no such Volume".to_owned());
            };
            service
                .volume_attachments
                .retain(|mount| mount.volume_resource_id != volume);
            if let Some(Part::Mount(mount)) = was {
                service.volume_attachments.push(mount.clone());
            }
        }
        Target::ConfigMount(_) => {
            let Some(config) = resource else {
                return Err("no such Config".to_owned());
            };
            // By ID: a renamed Config still mounts where it did.
            let was = baseline
                .services
                .iter()
                .find(|service| service.id == id)
                .and_then(|service| {
                    service
                        .config_attachments
                        .iter()
                        .find(|mount| mount.config_resource_id == config)
                });
            service
                .config_attachments
                .retain(|mount| mount.config_resource_id != config);
            if let Some(mount) = was {
                service.config_attachments.push(mount.clone());
            }
        }
        Target::Setting(_) | Target::Source => {}
    }
    Ok(())
}

/// One variable or mount of a Service, as some state holds it.
#[derive(PartialEq)]
enum Part<'a> {
    Variable(&'a SavedVariableIntent),
    Mount(&'a VolumeAttachment),
    ConfigMount(&'a ConfigAttachment),
}

/// The variable or mount `part` names of Service `id` in `intent`, compared by value.
fn part_of<'a>(intent: &'a SavedEnvironmentIntent, id: &str, part: &Target) -> Option<Part<'a>> {
    let service = intent.services.iter().find(|service| service.id == id)?;
    match part {
        Target::Variable(key) | Target::Exported(key) => service
            .variables
            .iter()
            .find(|variable| variable.key == key.as_str())
            .map(Part::Variable),
        Target::Mount(volume) => {
            let volume = intent
                .volumes
                .iter()
                .find(|node| node.name == volume.as_str())?;
            service
                .volume_attachments
                .iter()
                .find(|mount| mount.volume_resource_id == volume.resource_id)
                .map(Part::Mount)
        }
        Target::ConfigMount(config) => {
            let config = intent.configs.iter().find(|node| node.name == *config)?;
            service
                .config_attachments
                .iter()
                .find(|mount| mount.config_resource_id == config.resource_id)
                .map(Part::ConfigMount)
        }
        // A Setting compares through core's rows (see `setting_follows`).
        Target::Setting(_) | Target::Source => None,
    }
}

/// The Volume or Config a mount path names, by ID, as `current` or `baseline` has it.
fn mounted(
    current: &SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    part: &Target,
) -> Option<String> {
    let intents = [current, baseline];
    match part {
        Target::Mount(volume) => intents
            .into_iter()
            .flat_map(|intent| &intent.volumes)
            .find(|node| node.name == volume.as_str())
            .map(|node| node.resource_id.clone()),
        Target::ConfigMount(config) => intents
            .into_iter()
            .flat_map(|intent| &intent.configs)
            .find(|node| node.name == *config)
            .map(|node| node.resource_id.clone()),
        Target::Setting(_) | Target::Source | Target::Variable(_) | Target::Exported(_) => None,
    }
}
