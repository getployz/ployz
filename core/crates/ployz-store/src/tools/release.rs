//! Tools that move Staged changes on: review them, discard them, deploy them
//! or save them for the next deploy.

use std::collections::BTreeMap;

use ployz_core::config::{ChangeKind, EnvironmentNodeType, ReviewLifecycleKind};
use ployz_core::{ConfigName, ServiceName};
use serde_json::{Value, json};

use super::ToolId;
use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool, written};
use super::refusal::{Call, Refusal};
use super::step::{AcceptedLoss, Gate};
use crate::catalog::NODE_NAME;
use crate::{
    Admit, BatchCommand, Deploy, DestructiveKind, DiffQuery, Discard, DomainChange, NodeChange,
    Publish, ServicesQuery, SettingPath, VolumeName, Written,
};

/// The longest value a review line shows before it cuts.
const VALUE_CHARS: usize = 80;

/// Refuse loss accepted without the version a confirmation handed back.
pub(crate) fn version_for_loss(
    accept: &[VolumeName],
    version: Option<&String>,
) -> Result<(), Stop> {
    AcceptedLoss::from_input(
        accept.iter().map(ToString::to_string).collect(),
        version.cloned(),
    )
    .map(drop)
}

tool_input! {
    pub(crate) struct Review {
        @place(named),
    }
}

impl Tool for Review {
    const NAME: &'static str = "review";
    const DESCRIPTION: &'static str = "What the next deploy applies, in plain lines: the version deploy takes, what a human must approve, and broken references.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let diff = cx.one(&DiffQuery { environment: scope })?;
        let destroys = diff
            .effects
            .iter()
            .map(|effect| {
                let kind = match effect.kind {
                    DestructiveKind::RemovesService => "removes service",
                    DestructiveKind::DeletesVolume => "deletes volume data",
                    DestructiveKind::DetachesVolume => "detaches volume",
                    DestructiveKind::RemovesDomain => "removes domain",
                    DestructiveKind::RemovesServer => "removes server",
                    DestructiveKind::RemovesMember => "removes member",
                    DestructiveKind::ChangesRole => "changes role",
                    DestructiveKind::RevokesInvite => "revokes invite",
                    DestructiveKind::StopsService => "stops service",
                    DestructiveKind::RestartsService => "restarts service",
                    DestructiveKind::UnprotectsEnvironment => "unprotects environment",
                    DestructiveKind::RemovesOrganization => "removes organization",
                    DestructiveKind::RestoresVolume => "restores volume",
                };
                format!("{kind} {}", effect.name)
            })
            .collect::<Vec<_>>();
        let labels = NodeChange::labels(&diff.changes);
        Ok(json!({
            "at": diff.environment.to_string(),
            "revision": diff.environment.revision.0,
            "version": diff.version,
            "saved": diff.saved.map(|saved| json!({
                "revision": saved.0,
                "holds_staged": diff.published,
            })),
            "staged": diff.total_count,
            "lines": diff.changes.iter().flat_map(|change| lines(change, &labels)).collect::<Vec<_>>(),
            "asks_approval": !cx.is_local(),
            "destroys": destroys,
            "broken": diff.broken.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }))
    }
}

/// One node's change as plain lines: the node, then each Setting.
fn lines(change: &NodeChange, labels: &BTreeMap<String, String>) -> Vec<String> {
    let kind = match change.node.node_type {
        EnvironmentNodeType::Service => "service",
        EnvironmentNodeType::Volume => "volume",
        EnvironmentNodeType::Config => "config",
    };
    let verb = match change.lifecycle {
        ReviewLifecycleKind::Create => "create",
        ReviewLifecycleKind::Update => "update",
        ReviewLifecycleKind::Delete => "delete",
    };
    let mut head = format!("{verb} {kind} {}", change.name);
    if matches!(change.data, Some(crate::DataEffect::Deleted)) {
        head.push_str(", deleting its data");
    }
    if !change.restarts.is_empty() {
        let restarts = change
            .restarts
            .iter()
            .map(ServiceName::as_str)
            .collect::<Vec<_>>();
        head.push_str(&format!(", restarting {}", restarts.join(", ")));
    }
    let mut lines = vec![head];
    for setting in &change.settings {
        if let Some(domains) = DomainChange::of(setting) {
            let service = setting.path.split('.').next().unwrap_or_default();
            lines.extend(
                domains
                    .iter()
                    .map(|domain| format!("  {service}.domains: {domain}")),
            );
            continue;
        }
        let path = labels.get(&setting.path).unwrap_or(&setting.path);
        lines.push(match setting.kind {
            ChangeKind::Add => format!("  set {path} = {}", shown(&setting.after)),
            ChangeKind::Remove => format!("  unset {path}"),
            ChangeKind::Update => format!(
                "  {path}: {} -> {}",
                shown(&setting.before),
                shown(&setting.after)
            ),
        });
    }
    lines
}

/// A value on one line, cut long.
fn shown(value: &Value) -> String {
    let text = match value {
        Value::String(text) => format!("{text:?}"),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            value.to_string()
        }
    };
    match text.char_indices().nth(VALUE_CHARS) {
        Some((cut, _)) => format!("{}...", text.get(..cut).unwrap_or_default()),
        None => text,
    }
}

tool_input! {
    pub(crate) struct DiscardInput {
        /// Only this Service. Omit service, volume and config for everything.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        service: Option<ServiceName>,
        /// Only this Volume.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        volume: Option<VolumeName>,
        /// Only this Config.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        config: Option<ConfigName>,
        @revision,
        @place(named),
    }
}

impl Tool for DiscardInput {
    const NAME: &'static str = "discard";
    const DESCRIPTION: &'static str = "Return all Staged changes, or one Service, Volume or Config, to deployed. A named one differing only from saved returns to saved.";
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let (path, what) = match (&self.service, &self.volume, &self.config) {
            (None, None, None) => (None, "everything".to_owned()),
            (Some(service), None, None) => {
                (Some(service.to_string()), format!("service {service}"))
            }
            (None, Some(volume), None) => (
                Some(format!("volumes.{volume}")),
                format!("volume {volume}"),
            ),
            (None, None, Some(config)) => (
                Some(format!("configs.{config}")),
                format!("config {config}"),
            ),
            (Some(_) | None, Some(_) | None, Some(_) | None) => {
                return Err(
                    Refusal::invalid("Give one of service, volume or config", "service").into(),
                );
            }
        };
        let path = path
            .map(|path| SettingPath::parse(&path))
            .transpose()
            .map_err(|_| Refusal::internal("A discard path did not parse"))?;
        let scope = cx.scope(&self);
        let discard = Discard {
            environment: scope.clone(),
            target: None,
            path,
            version: None,
        };
        let results = cx.batch(
            &scope,
            self.expect_revision,
            vec![BatchCommand::Discard(discard)],
        )?;
        let discarded = written(results, |one| {
            if let Written::Discarded(discarded) = one {
                Some(discarded)
            } else {
                None
            }
        })?;
        Ok(json!({
            "at": discarded.environment.to_string(),
            "discarded": what,
            "changed": discarded.changed,
            "dropped": discarded.dropped,
            "saved_not_staged": discarded.reversed.iter().map(|reversal| json!({
                "name": reversal.change.name,
                "way_back": reversal.way_back,
            })).collect::<Vec<_>>(),
        }))
    }
}

tool_input! {
    pub(crate) struct DeployInput {
        /// Deploy only these Services. Omit to deploy every Service.
        #[serde(default)]
        #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
        services: Vec<ServiceName>,
        /// What this ships, in a few words. Shown on the Deployment.
        #[serde(default)]
        #[schemars(length(max = 200))]
        message: Option<String>,
        /// The reviewed version; refused if what is staged changed since. Required with accept_volume_loss.
        #[serde(default)]
        version: Option<String>,
        /// Volumes whose data this may delete.
        #[serde(default)]
        #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
        accept_volume_loss: Vec<VolumeName>,
        @key["Any id you choose. A call that repeats a key gets the first call's result, not a second deploy."],
        /// Save everything staged for the next deploy instead of deploying it.
        #[serde(default)]
        skip_deploy: bool,
        /// Keep running Services the Environment doesn't hold.
        #[serde(default)]
        no_prune: bool,
        @place(named),
    }
}

impl Tool for DeployInput {
    const NAME: &'static str = "deploy";
    const DESCRIPTION: &'static str = "Deploys everything staged. Deploying every Service removes other Services running in the Environment, unless no_prune. With skip_deploy, saves it for the next deploy instead. A deploy queues a Deployment and returns without waiting for it to finish. Every deploy waits for a human to approve; a save waits only if the next deploy removes a Service, a Volume, or a live domain.";
    const GATE: Gate = Gate::Store;
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "services": ["web"], "message": "Raise web's log level", "request_key": "h4Rv9sNb6YeD" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        version_for_loss(&self.accept_volume_loss, self.version.as_ref())?;
        let scope = cx.scope(&self);
        if self.skip_deploy {
            if !self.services.is_empty() {
                return Err(Refusal::invalid(
                    "skip_deploy saves everything staged; omit services",
                    "services",
                )
                .into());
            }
            let saved = cx.write(&Publish {
                environment: scope,
                version: self.version,
                message: self.message,
                accept_volume_loss: self.accept_volume_loss,
                id: Some(cx.mint(self.request_key)?),
            })?;
            return Ok(json!({
                "at": saved.environment.to_string(),
                "saved": {
                    "revision": saved.saved.map(|saved| saved.0),
                    "created": saved.created,
                },
            }));
        }
        let here = cx.one(&ServicesQuery {
            environment: scope.clone(),
        })?;
        let deployment = cx.write(&Admit::Deploy(Deploy {
            teardown: None,
            id: cx.mint(self.request_key)?,
            environment: scope,
            services: self.services,
            version: self.version,
            upload: None,
            accept_volume_loss: self.accept_volume_loss,
            message: self.message,
            prune: false,
            no_prune: self.no_prune,
            approval: None,
        }))?;
        let mut next = cx.place_args();
        next.insert("deployment".into(), deployment.number.into());
        next.insert("wait_seconds".into(), 120.into());
        Ok(json!({
            "at": here.environment.to_string(),
            "deployment": deployment.number,
            "status": deployment.status,
            "saved_revision": deployment.saved.0,
            "services": deployment.services,
            "prunes": deployment.prune,
            "next": Call::to(ToolId::DeploymentShow, next.into()),
        }))
    }
}
