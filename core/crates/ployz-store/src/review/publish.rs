//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::{
    At, ConfigAttachment, EnvironmentNodeType, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVariableIntent, Setting, VolumeAttachment, canonicalize_environment_intent,
    restore_environment_node_into,
};
use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{Revision, VolumeName};
use crate::review::{self, Review};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{NodeName, SettingPath, Target};
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
    /// What this saved revision changes, in the saver's words.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub message: Option<String>,
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
    /// The comparison to return to. Head preserves the deployment baseline; Saved
    /// abandons a draft reversal; Review discards the server-derived visible rows.
    #[serde(default)]
    #[ts(as = "Option<DiscardTarget>", optional)]
    pub target: DiscardTarget,
    /// `SERVICE`, `volumes.VOLUME`, `configs.CONFIG`, `SERVICE.SETTING`,
    /// `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME` or `SERVICE.configs.CONFIG`; none
    /// discards everything.
    #[serde(default)]
    pub path: Option<SettingPath>,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The reviewed baseline a Discard restores into the draft.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiscardTarget {
    #[default]
    Head,
    Saved,
    Review,
}

/// The Environment after a discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Discarded {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// The latest Saved revision, unchanged by Discard.
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
        publish.message.as_deref(),
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
    let saved = review.saved.as_ref();
    let runs = review.head.intent.clone();
    let baseline = match discard.target {
        DiscardTarget::Head | DiscardTarget::Review => &review.head.intent,
        DiscardTarget::Saved => saved.map_or(&environment.working, |saved| &saved.intent),
    };
    let working = if discard.target == DiscardTarget::Review {
        if discard.path.is_some() {
            return Err(error::invalid(
                "Review discard takes the whole reviewed list",
                json!({}),
            ));
        }
        discard_review(tx, &environment, &review)?
    } else {
        let mut working = environment.working.clone();
        match &discard.path {
            None => working = baseline.clone(),
            Some(path) => restore_path_into(
                tx,
                &environment,
                path,
                baseline,
                discard.target == DiscardTarget::Head,
                &mut working,
            )?,
        }
        working
    };
    review::history::prevent_plaintext(&environment.working, &working)?;
    let working =
        ployz_core::config::parse_environment_intent(json!(working)).map_err(|failure| {
            error::conflict(
                format!(
                    "Discard would leave an invalid Environment: {}",
                    failure.message
                ),
                json!({ "path": failure.path }),
            )
        })?;
    let revision = saved.map(|saved| saved.revision);
    let mut changed = false;
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

fn restore_path_into(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    path: &SettingPath,
    head: &SavedEnvironmentIntent,
    introductions: bool,
    current: &mut SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let working = &environment.working;
    let node = match path.node() {
        NodeName::Service(name) => [working, head]
            .into_iter()
            .flat_map(|intent| &intent.services)
            .find(|service| service.slug == name.as_str())
            .map(|service| (EnvironmentNodeType::Service, service.id.clone()))
            .ok_or_else(|| scope::no_service(&name, &environment.summary.name, working)),
        NodeName::Volume(name) => [working, head]
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
        NodeName::Config(selector) => selector
            .resolve([working, head])
            .map(|config| (EnvironmentNodeType::Config, config.resource_id.clone())),
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
    let field = path.node_field();
    let holds = |intent: &SavedEnvironmentIntent| match node_type {
        EnvironmentNodeType::Service => intent.services.iter().any(|service| service.id == id),
        EnvironmentNodeType::Volume => intent.volumes.iter().any(|volume| volume.resource_id == id),
        EnvironmentNodeType::Config => intent.configs.iter().any(|config| config.resource_id == id),
    };
    let introduction = introductions && (part.is_some() || field.is_some()) && !holds(head);
    let baseline = if introduction {
        review::introductions(tx, environment)?
    } else {
        head.clone()
    };
    let resource = part
        .map(|part| mounted(working, &baseline, part))
        .transpose()?
        .flatten();
    let resource = resource.as_deref();
    let restored = match field {
        Some(field) => field.restore(current, &baseline, &id),
        None => restore_into(current, &baseline, (node_type, &id), part, resource),
    };
    restored.map_err(|message| {
        error::conflict(
            format!("Discard would leave an invalid Environment: {message}"),
            json!({ "path": path }),
        )
    })
}

pub(crate) fn restore_into(
    current: &mut SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    (node_type, id): (EnvironmentNodeType, &str),
    part: Option<&Target>,
    resource: Option<&str>,
) -> Result<(), String> {
    let field = match part {
        None => None,
        Some(Target::Setting(setting)) => Some(setting.field()),
        Some(Target::Source) => Some("source"),
        Some(part) => {
            let service = current
                .services
                .iter_mut()
                .find(|service| service.id == id)
                .ok_or_else(|| "its Service is gone".to_owned())?;
            return restore_part(service, baseline, (id, resource), part);
        }
    };
    restore_environment_node_into(current, Some(baseline), node_type, id, field)
        .map_err(|error| error.message)
}

fn restore_part(
    service: &mut SavedServiceIntent,
    baseline: &SavedEnvironmentIntent,
    (id, resource): (&str, Option<&str>),
    part: &Target,
) -> Result<(), String> {
    let was = part_of(baseline, (id, resource), part);
    match part {
        Target::Variable(key) | Target::Exported(key) | Target::Description(key) => {
            let was = match was {
                Some(Part::Variable(was)) => Some(was),
                Some(Part::Mount(_) | Part::ConfigMount(_)) | None => None,
            };
            let variables = &mut service.variables;
            let at = variables.iter().position(|v| v.key == key.as_str());
            let exported = matches!(part, Target::Exported(_));
            match (at.and_then(|at| variables.get_mut(at)), was) {
                (Some(variable), Some(was)) if matches!(part, Target::Description(_)) => {
                    variable.description.clone_from(&was.description)
                }
                (Some(variable), None) if matches!(part, Target::Description(_)) => {
                    variable.description = None
                }
                (Some(variable), Some(was)) if exported => variable.exported = was.exported,
                (Some(variable), None) if exported => variable.exported = false,
                (Some(variable), Some(was)) => {
                    variable.id.clone_from(&was.id);
                    variable.value.clone_from(&was.value);
                    variable
                        .value_fingerprint
                        .clone_from(&was.value_fingerprint);
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
            service
                .config_attachments
                .retain(|mount| mount.config_resource_id != config);
            if let Some(Part::ConfigMount(mount)) = was {
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
fn part_of<'a>(
    intent: &'a SavedEnvironmentIntent,
    (id, resource): (&str, Option<&str>),
    part: &Target,
) -> Option<Part<'a>> {
    let service = intent.services.iter().find(|service| service.id == id)?;
    match part {
        Target::Variable(key) | Target::Exported(key) | Target::Description(key) => service
            .variables
            .iter()
            .find(|variable| variable.key == key.as_str())
            .map(Part::Variable),
        Target::Mount(_) => {
            let volume = resource?;
            service
                .volume_attachments
                .iter()
                .find(|mount| mount.volume_resource_id == volume)
                .map(Part::Mount)
        }
        Target::ConfigMount(_) => {
            let config = resource?;
            service
                .config_attachments
                .iter()
                .find(|mount| mount.config_resource_id == config)
                .map(Part::ConfigMount)
        }
        // A Setting compares through core's rows (see `setting_follows`).
        Target::Setting(_) | Target::Source => None,
    }
}

pub(crate) fn mounted(
    current: &SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    part: &Target,
) -> Result<Option<String>, RpcError> {
    let intents = [current, baseline];
    Ok(match part {
        Target::Mount(volume) => intents
            .into_iter()
            .flat_map(|intent| &intent.volumes)
            .find(|node| node.name == volume.as_str())
            .map(|node| node.resource_id.clone()),
        Target::ConfigMount(config) => Some(config.resolve(intents)?.resource_id.clone()),
        Target::Setting(_)
        | Target::Source
        | Target::Variable(_)
        | Target::Exported(_)
        | Target::Description(_) => None,
    })
}

fn discard_review(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    reviewed: &Review,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let mut working = environment.working.clone();
    let introductions = review::introductions(tx, environment)?;
    for change in &reviewed.view.changes {
        let exists = match change.node.node_type {
            EnvironmentNodeType::Service => reviewed
                .head
                .intent
                .services
                .iter()
                .any(|node| node.id == change.node.id),
            EnvironmentNodeType::Volume => reviewed
                .head
                .intent
                .volumes
                .iter()
                .any(|node| node.resource_id == change.node.id),
            EnvironmentNodeType::Config => reviewed
                .head
                .intent
                .configs
                .iter()
                .any(|node| node.resource_id == change.node.id),
        };
        let baseline =
            if change.lifecycle == ployz_core::config::ReviewLifecycleKind::Update && !exists {
                &introductions
            } else {
                &reviewed.head.intent
            };
        restore_change_into(&mut working, baseline, &environment.working, change)?;
    }
    if let Some(saved) = &reviewed.saved {
        for draft in &reviewed.view.draft_changes {
            let runtime = reviewed
                .view
                .changes
                .iter()
                .find(|runtime| runtime.node == draft.node);
            if runtime.is_some_and(|runtime| {
                runtime.lifecycle != ployz_core::config::ReviewLifecycleKind::Update
            }) {
                continue;
            }
            let mut only_draft = draft.clone();
            if let Some(runtime) = runtime {
                only_draft.settings.retain(|row| {
                    !runtime
                        .settings
                        .iter()
                        .any(|runtime| review::same_setting(runtime, row))
                });
                if only_draft.lifecycle == ployz_core::config::ReviewLifecycleKind::Update
                    && only_draft.settings.is_empty()
                {
                    continue;
                }
            }
            restore_change_into(
                &mut working,
                &saved.intent,
                &environment.working,
                &only_draft,
            )?;
        }
    }
    Ok(working)
}

pub(crate) fn restore_change_into(
    current: &mut SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
    source: &SavedEnvironmentIntent,
    change: &review::NodeChange,
) -> Result<(), RpcError> {
    let invalid = |message: String| {
        error::conflict(
            format!("This change cannot be restored: {message}"),
            json!({}),
        )
    };
    if change.lifecycle != ployz_core::config::ReviewLifecycleKind::Update {
        return restore_into(
            current,
            baseline,
            (change.node.node_type, &change.node.id),
            None,
            None,
        )
        .map_err(invalid);
    }
    for row in &change.settings {
        if change.node.node_type == EnvironmentNodeType::Service
            && row.path == format!("{}.name", change.name)
        {
            let name = baseline
                .services
                .iter()
                .find(|service| service.id == change.node.id)
                .ok_or_else(|| error::corrupt("History Service"))?;
            let service = current
                .services
                .iter_mut()
                .find(|service| service.id == change.node.id)
                .ok_or_else(|| {
                    error::conflict(
                        "The Service this change belongs to was removed",
                        json!({ "path": row.path }),
                    )
                })?;
            service.slug.clone_from(&name.slug);
            continue;
        }
        if change.node.node_type == EnvironmentNodeType::Service
            && let Some(At::Setting(setting)) = row.row.as_ref().map(ployz_core::config::RowId::at)
        {
            let route;
            let path = if *setting == Setting::Routes {
                let id = row
                    .before
                    .get("id")
                    .or_else(|| row.after.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| invalid("Route identity is unavailable".into()))?;
                route = format!("routes.{id}");
                route.as_str()
            } else {
                setting.path()
            };
            restore_environment_node_into(
                current,
                Some(baseline),
                change.node.node_type,
                &change.node.id,
                Some(path),
            )
            .map_err(|failure| invalid(failure.message))?;
            continue;
        }
        if change.node.node_type == EnvironmentNodeType::Service {
            let service = current
                .services
                .iter_mut()
                .find(|service| service.id == change.node.id)
                .ok_or_else(|| invalid("Current Service is unavailable".into()))?;
            let prior = baseline
                .services
                .iter()
                .find(|service| service.id == change.node.id)
                .ok_or_else(|| invalid("Authored Service baseline is unavailable".into()))?;
            match row.row.as_ref().map(ployz_core::config::RowId::at) {
                Some(At::Variable(key)) => {
                    let was = prior.variables.iter().find(|variable| variable.key == *key);
                    let now = service
                        .variables
                        .iter_mut()
                        .find(|variable| variable.key == *key);
                    match (now, was) {
                        (Some(now), Some(was)) if row.path.ends_with(".exported") => {
                            now.exported = was.exported
                        }
                        (Some(now), Some(was)) if row.path.ends_with(".description") => {
                            now.description.clone_from(&was.description)
                        }
                        (Some(now), Some(was)) => {
                            let changed = source
                                .services
                                .iter()
                                .find(|service| service.id == change.node.id)
                                .and_then(|service| {
                                    service
                                        .variables
                                        .iter()
                                        .find(|variable| variable.key == *key)
                                });
                            if changed.is_none_or(|changed| changed.id != was.id) {
                                now.id.clone_from(&was.id);
                            }
                            if changed.is_none_or(|changed| {
                                changed.value != was.value
                                    || changed.value_fingerprint != was.value_fingerprint
                            }) {
                                now.value.clone_from(&was.value);
                                now.value_fingerprint.clone_from(&was.value_fingerprint);
                            }
                        }
                        (Some(_), None) => {
                            service.variables.retain(|variable| variable.key != *key)
                        }
                        (None, Some(was)) => service.variables.push(was.clone()),
                        (None, None) => {}
                    }
                    continue;
                }
                Some(At::Mount(lineage)) => {
                    let resource = [baseline, source]
                        .into_iter()
                        .flat_map(|intent| &intent.volumes)
                        .find(|volume| volume.resource_lineage_id == *lineage)
                        .ok_or_else(|| invalid("Volume identity is unavailable".into()))?;
                    service
                        .volume_attachments
                        .retain(|mount| mount.volume_resource_id != resource.resource_id);
                    if let Some(mount) = prior
                        .volume_attachments
                        .iter()
                        .find(|mount| mount.volume_resource_id == resource.resource_id)
                    {
                        service.volume_attachments.push(mount.clone());
                    }
                    continue;
                }
                Some(At::ConfigMount(lineage)) => {
                    let resource = [baseline, source]
                        .into_iter()
                        .flat_map(|intent| &intent.configs)
                        .find(|config| config.resource_lineage_id == *lineage)
                        .ok_or_else(|| invalid("Config identity is unavailable".into()))?;
                    service
                        .config_attachments
                        .retain(|mount| mount.config_resource_id != resource.resource_id);
                    if let Some(mount) = prior
                        .config_attachments
                        .iter()
                        .find(|mount| mount.config_resource_id == resource.resource_id)
                    {
                        service.config_attachments.push(mount.clone());
                    }
                    continue;
                }
                _ if row.path == format!("{}.template", change.name) => {
                    service.config.template.clone_from(&prior.config.template);
                    continue;
                }
                _ => {}
            }
        }
        let path = SettingPath::parse(&row.path)?;
        if let Some(field) = path.node_field() {
            if field.of(current, &change.node.id) != field.of(baseline, &change.node.id) {
                field
                    .restore(current, baseline, &change.node.id)
                    .map_err(invalid)?;
            }
        } else {
            let resource = path
                .target()
                .map(|part| mounted(source, baseline, part))
                .transpose()?
                .flatten();
            restore_into(
                current,
                baseline,
                (change.node.node_type, &change.node.id),
                path.target(),
                resource.as_deref(),
            )
            .map_err(invalid)?;
        }
    }
    Ok(())
}
