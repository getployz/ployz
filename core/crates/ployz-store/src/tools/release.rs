//! Tools that move Staged changes on: review them, discard them, publish them,
//! deploy them.

use ployz_core::config::{ChangeKind, EnvironmentNodeType, ReviewLifecycleKind};
use ployz_core::{ConfigName, ServiceName};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, written};
use super::refusal::{Call, Refusal, not_setting};
use super::step::{AcceptedLoss, Gate, RequestKey};
use crate::catalog::NODE_NAME;
use crate::{
    Admit, BatchCommand, Deploy, DestructiveKind, DiffQuery, Discard, EnvironmentName, NodeChange,
    ProjectName, Publish, ServicesQuery, SettingPath, VolumeName, Written,
};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

/// The longest value a review line shows before it cuts.
const VALUE_CHARS: usize = 80;

/// Refuse loss accepted without the version a confirmation handed back.
fn version_for_loss(accept: &[VolumeName], version: Option<&String>) -> Result<(), Stop> {
    AcceptedLoss::from_input(
        accept.iter().map(ToString::to_string).collect(),
        version.cloned(),
    )
    .map(drop)
}

/// `project` and `environment` as the input named them, for a `next` call.
pub(crate) fn place(call: &Call) -> serde_json::Map<String, Value> {
    ["project", "environment"]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), call.input.get(key)?.clone())))
        .collect()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Review {
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for Review {
    const NAME: &'static str = "review";
    const DESCRIPTION: &'static str = "What Publish would save and Deploy would apply, in plain lines, with the version publish and deploy take and what a human must approve.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
                };
                format!("{kind} {}", effect.name)
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "at": at(&diff.environment),
            "revision": diff.environment.revision.0,
            "version": diff.version,
            "saved_revision": diff.saved.map(|saved| saved.0),
            "published": diff.published,
            "staged": diff.total_count,
            "lines": diff.changes.iter().flat_map(lines).collect::<Vec<_>>(),
            "asks_approval": !destroys.is_empty(),
            "destroys": destroys,
        }))
    }
}

/// One node's change as plain lines: the node, then each Setting.
fn lines(change: &NodeChange) -> Vec<String> {
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
    lines.extend(change.settings.iter().map(|setting| {
        let path = label(&setting.path);
        match setting.kind {
            ChangeKind::Add => format!("  set {path} = {}", shown(&setting.after)),
            ChangeKind::Remove => format!("  unset {path}"),
            ChangeKind::Update => format!(
                "  {path}: {} -> {}",
                shown(&setting.before),
                shown(&setting.after)
            ),
        }
    }));
    lines
}

/// A Setting path as an agent names it: domains as `domains`, never the field
/// that stores them.
fn label(path: &str) -> String {
    path.split('.')
        .map(|part| match not_setting(part) {
            Some("domain_add") => "domains",
            Some(_) | None => part,
        })
        .collect::<Vec<_>>()
        .join(".")
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscardInput {
    /// Only this Service's Staged changes. Omit service, volume and config to discard everything Staged.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    service: Option<ServiceName>,
    /// Only this Volume's Staged changes.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    volume: Option<VolumeName>,
    /// Only this Config's Staged changes.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    config: Option<ConfigName>,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DiscardInput {
    const NAME: &'static str = "discard";
    const DESCRIPTION: &'static str =
        "Return every Staged change, or one Service's, Volume's or Config's, to what is deployed.";
    const READ_ONLY: bool = false;

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
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let discard = Discard {
            environment: scope.clone(),
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
            "at": at(&discarded.environment),
            "discarded": what,
            "changed": discarded.changed,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublishInput {
    /// Only if review still shows this version. Required with accept_volume_loss.
    #[serde(default)]
    version: Option<String>,
    /// Volumes whose data this may delete. Only when the member asked for exactly that loss.
    #[serde(default)]
    #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
    accept_volume_loss: Vec<VolumeName>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for PublishInput {
    const NAME: &'static str = "publish";
    const DESCRIPTION: &'static str = "Save everything Staged as the next Saved revision, without deploying. Usually call deploy instead: it publishes and deploys. If the change removes a Service, a Volume, or a live domain, this waits for a human to approve.";
    const GATE: Gate = Gate::Store;
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        version_for_loss(&self.accept_volume_loss, self.version.as_ref())?;
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let published = cx.write(&Publish {
            environment: scope,
            version: self.version,
            accept_volume_loss: self.accept_volume_loss,
        })?;
        Ok(json!({
            "at": at(&published.environment),
            "saved_revision": published.saved.map(|saved| saved.0),
            "published": published.created,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeployInput {
    /// Deploy only these Services. Omit to deploy every Service.
    #[serde(default)]
    #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
    services: Vec<ServiceName>,
    /// What this ships, in a few words. Shown on the Deployment.
    #[serde(default)]
    #[schemars(length(max = 200))]
    message: Option<String>,
    /// Only if review still shows this version. Required with accept_volume_loss.
    #[serde(default)]
    version: Option<String>,
    /// Volumes whose data this may delete. Only when the member asked for exactly that loss.
    #[serde(default)]
    #[schemars(with = "Vec<String>", inner(pattern(NODE_NAME)))]
    accept_volume_loss: Vec<VolumeName>,
    /// Any id you choose. Repeat the same call with the same key after a lost reply and you get the same result back, not a second one.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeployInput {
    const NAME: &'static str = "deploy";
    const DESCRIPTION: &'static str = "Deploy what is Staged and Saved: publishes first if needed, then queues a Deployment and returns. Follow it with deployment_show. If the change removes a Service, a Volume, or a live domain, this waits for a human to approve.";
    const GATE: Gate = Gate::Store;
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "services": ["web"], "message": "Raise web's log level", "request_key": "deploy-web-0001" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        version_for_loss(&self.accept_volume_loss, self.version.as_ref())?;
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
        }))?;
        let mut next = place(cx.call());
        next.insert("deployment".into(), deployment.number.into());
        next.insert("wait_seconds".into(), 120.into());
        Ok(json!({
            "at": at(&here.environment),
            "deployment": deployment.number,
            "status": deployment.status,
            "saved_revision": deployment.saved.0,
            "services": deployment.services,
            "next": Call::new("deployment_show", next.into()),
        }))
    }
}
