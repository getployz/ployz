//! Tools that move Staged changes on: review them, discard them, deploy them
//! or save them for the next deploy.

use std::collections::BTreeMap;

use ployz_core::config::{ChangeKind, EnvironmentNodeType, ReviewLifecycleKind};
use ployz_core::{ConfigName, ServiceName};
use serde_json::{Value, json};

use super::deployment::follow_call;
use super::input::tool_input;
use super::machine::{Cx, DiffVerb, Effect, Stop, Tool, written};
use super::refusal::Refusal;
use super::step::{Gate, version_for_loss};
use crate::catalog::NODE_NAME;
use crate::{
    Admit, BatchCommand, Deploy, DiffQuery, Discard, DomainChange, NodeChange, Publish,
    ServicesQuery, SettingPath, VolumeName, Written,
};

/// The longest value a review line shows before it cuts.
const VALUE_CHARS: usize = 80;

tool_input! {
    pub(crate) struct Review {
        @place(named),
    }
}

impl Tool for Review {
    const NAME: &'static str = "review";
    const TITLE: &'static str = "Review staged changes";
    const DESCRIPTION: &'static str = "Show what the next deploy would apply: each staged change as a plain line, what it destroys, broken references and the latest Saved revision. Also reports whether a deploy here can stop to ask a human, and the version that pins a deploy to this review.";
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
            .map(|effect| format!("{} {}", effect.kind.verb(), effect.name))
            .collect::<Vec<_>>();
        let labels = NodeChange::labels(&diff.changes);
        let region = diff.region.as_ref().map(|region| {
            let [before, after] = region.sides();
            format!("{}: {before} -> {after}", SettingPath::REGION)
        });
        Ok(json!({
            "at": diff.environment.to_string(),
            "revision": diff.environment.revision.0,
            "version": diff.version,
            "saved": diff.saved.map(|saved| json!({
                "revision": saved.0,
                "holds_staged": diff.published,
            })),
            "staged": diff.total_count,
            "lines": region.into_iter().chain(diff.changes.iter().flat_map(|change| lines(change, &labels))).collect::<Vec<_>>(),
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
        /// Discard only this Service's changes, e.g. `web`. Give at most one of service, volume and config. With none, everything staged is discarded.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        service: Option<ServiceName>,
        /// Discard only this Volume's changes, e.g. `data`.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        volume: Option<VolumeName>,
        /// Discard only this Config's changes.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        config: Option<ConfigName>,
        @revision,
        @place(named),
    }
}

impl Tool for DiscardInput {
    const NAME: &'static str = "discard";
    const TITLE: &'static str = "Discard staged changes";
    const DESCRIPTION: &'static str = "Discard all staged changes, or only one Service's, Volume's or Config's, putting it back to what is deployed. A named one that differs only from Saved State goes back to Saved. Unsaved changes are lost.";
    // Sent again for one name, it can go a step further: the first puts a removed Service back to
    // Saved, the second takes that Saved Service away because it never deployed.
    const EFFECT: Effect = Effect::Remove { idempotent: false };

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
        /// Deploy only these Services, e.g. `["web"]`. Leave out to deploy every Service.
        #[serde(default)]
        #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
        services: Vec<ServiceName>,
        /// What this ships, in a few words. Shown on the Deployment.
        #[serde(default)]
        #[schemars(length(max = 200))]
        message: Option<String>,
        // Not the @loss wording: a review hands this version out too, not only a refusal.
        /// The version a review or a refusal gave. Fails if what is staged changed since. Leave out to ship whatever is staged now. Required with accept_volume_loss.
        #[serde(default)]
        version: Option<String>,
        /// Volumes whose data the member agreed to lose, named as the refusal listed them.
        #[serde(default)]
        #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
        accept_volume_loss: Vec<VolumeName>,
        @key,
        /// Save everything staged as a Saved revision for a later deploy, without starting a Deployment. Fails if services is given.
        #[serde(default)]
        skip_deploy: bool,
        /// When every Service deploys, keep running Services that are not in the Environment.
        #[serde(default)]
        no_prune: bool,
        @place(named),
    }
}

impl Tool for DeployInput {
    const NAME: &'static str = "deploy";
    const TITLE: &'static str = "Deploy an Environment";
    const DESCRIPTION: &'static str = "Deploy an Environment: save everything staged and queue a Deployment of every Service, or only the ones named. Returns once the Deployment is queued, so check progress with `deployment_show`. Deploying every Service removes running Services that are not in the Environment, unless no_prune. With skip_deploy it only saves. Where approval is asked, every deploy waits for it; a save alone waits only if deploying it would destroy something.";
    const GATE: Gate = Gate::Store;
    // Without a request_key, each call queues a new Deployment.
    const EFFECT: Effect = Effect::Remove { idempotent: false };
    // The Deployment it queues builds on GitHub Actions and pulls images from
    // public registries.
    const OPEN_WORLD: bool = true;

    fn verb(input: &Value) -> DiffVerb {
        match input.get("skip_deploy").and_then(Value::as_bool) {
            Some(true) => DiffVerb::Publish,
            _ => DiffVerb::Deploy,
        }
    }

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
            id: cx.mint(self.request_key)?,
            environment: scope,
            services: self.services,
            version: self.version,
            upload: None,
            accept_volume_loss: self.accept_volume_loss,
            message: self.message,
            prune: false,
            no_prune: self.no_prune,
            versions: Vec::new(),
            recreate: false,
        }))?;
        Ok(json!({
            "at": here.environment.to_string(),
            "deployment": deployment.number,
            "status": deployment.status,
            "saved_revision": deployment.saved.0,
            "services": deployment.services,
            "prunes": deployment.prune,
            "next": follow_call(cx.place_args(), deployment.number),
        }))
    }
}
