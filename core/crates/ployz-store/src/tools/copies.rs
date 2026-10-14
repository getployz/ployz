//! Tools on where a Volume's data is: show it, mirror and sync it, remove the
//! mirror, move it, and release a move that stopped.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::act::{Run, latest, operate, operations, resolve, servers};
use super::cloud::{CopiesAnswer, OperationsAnswer, ServersAnswer, VolumeCopies};
use super::machine::{Cx, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::release::place;
use super::step::{
    AcceptedLoss, Action, Ask, CloudQuery, Gate, RequestKey, ServerName, Subject, VolumeRun,
};
use crate::catalog::NODE_NAME;
use crate::{
    EnvironmentName, NamespaceQuery, ProjectName, VolumeName, VolumeQuery, VolumeView, VolumesQuery,
};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

/// `volume_show` for `volume` where the call is.
fn volume_show(cx: &Cx<'_>, volume: &VolumeName) -> Call {
    let mut input = place(cx.call());
    input.insert("volume".into(), volume.as_str().into());
    Call::new("volume_show", Value::Object(input))
}

/// The Volume the call names, read alone.
fn volume(
    cx: &mut Cx<'_>,
    volume: VolumeName,
    project: Option<&ProjectName>,
    environment: Option<&EnvironmentName>,
    also: Option<Ask>,
) -> Result<(VolumeView, Option<Result<Value, ployz_core::RpcError>>), Stop> {
    let environment = cx.scope(project, environment);
    let mut asks = vec![store(&VolumeQuery {
        environment,
        volume,
    })?];
    asks.extend(also);
    let mut answers = cx.read(asks)?.into_iter();
    let view = cx.view::<VolumeQuery>(answers.next())?;
    Ok((view, answers.next()))
}

/// Run `run` on the Volume `view` shows.
fn on_volume(
    cx: &mut Cx<'_>,
    view: &VolumeView,
    run: VolumeRun,
    key: Option<RequestKey>,
    wait_seconds: u32,
) -> Result<Value, Stop> {
    let id = view.volume.volume.id.clone();
    let run = Run {
        at: at(&view.environment),
        subject: Subject::Volume { volume: id.clone() },
        action: Action::Volume { volume: id, run },
        read: volume_show(cx, &view.volume.volume.name),
    };
    operate(cx, run, key, wait_seconds)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeShow {
    /// Omit for every Volume here.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    volume: Option<VolumeName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeShow {
    const NAME: &'static str = "volume_show";
    const DESCRIPTION: &'static str = "Where a Volume's data is: its writable copy, its mirror and last sync, and recent runs with how each ended.";
    const READ_ONLY: bool = true;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "volume": "pgdata" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let environment = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let first = match &self.volume {
            Some(volume) => store(&VolumeQuery {
                environment: environment.clone(),
                volume: volume.clone(),
            })?,
            None => store(&VolumesQuery {
                environment: environment.clone(),
            })?,
        };
        let mut answers = cx
            .read(vec![first, store(&NamespaceQuery { environment })?])?
            .into_iter();
        let (place, volumes) = match &self.volume {
            Some(_) => {
                let view = cx.view::<VolumeQuery>(answers.next())?;
                (view.environment, vec![view.volume])
            }
            None => {
                let view = cx.view::<VolumesQuery>(answers.next())?;
                (view.environment, view.volumes)
            }
        };
        let namespace = cx.view::<NamespaceQuery>(answers.next())?;
        let mut asks = vec![Ask::Cloud(CloudQuery::Copies {
            namespace: namespace.namespace,
            volume: self.volume.clone(),
        })];
        if let [one] = volumes.as_slice() {
            asks.push(operations(
                Subject::Volume {
                    volume: one.volume.id.clone(),
                },
                5,
            ));
        }
        let mut answers = cx.read(asks)?.into_iter();
        let copies = cx.cloud::<CopiesAnswer>(answers.next())?;
        let runs = answers
            .next()
            .map(|answer| cx.cloud::<OperationsAnswer>(Some(answer)))
            .transpose()?;
        let shown = volumes
            .iter()
            .map(|listing| {
                let name = &listing.volume.name;
                let found = copies.volumes.iter().find(|copy| copy.volume == *name);
                json!({
                    "volume": name,
                    "deployed": listing.deployed,
                    "writable_on": found.and_then(|copy| copy.writer.as_ref()).map(|server| server.name.as_str()),
                    "mirror": found.and_then(|copy| copy.mirror.as_ref()).map(|mirror| json!({
                        "server": mirror.server.name.as_str(),
                        "synced_at": mirror.synced_at,
                    })),
                    "runs": runs
                        .as_ref()
                        .map(|page| page.operations.iter().map(latest).collect::<Vec<_>>()),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "at": at(&place), "volumes": shown, "seen": copies.seen }))
    }
}

/// The copies of the Volume `view` shows, or a refusal when it has none yet.
fn copies_of<'a>(
    cx: &Cx<'_>,
    answer: &'a CopiesAnswer,
    view: &VolumeView,
) -> Result<&'a VolumeCopies, Stop> {
    let name = &view.volume.volume.name;
    answer
        .volumes
        .iter()
        .find(|copy| copy.volume == *name && copy.writer.is_some())
        .ok_or_else(|| {
            Refusal::conflict("That Volume has no data on a server yet; deploy it first")
                .with_next(volume_show(cx, name))
                .into()
        })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeMirror {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    server: Option<ServerName>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeMirror {
    const NAME: &'static str = "volume_mirror";
    const DESCRIPTION: &'static str = "Make a Volume's one mirror current. With server, copy it there as <volume>-<server>; without, hand the mirror what changed since its last pull and answer without waiting for the download. full recopies a mirror that diverged, as a run.";
    const GATE: Gate = Gate::Cloud;
    const READ_ONLY: bool = false;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "volume": "pgdata", "server": "db-2", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let mut asks = vec![
            store(&VolumeQuery {
                environment: scope.clone(),
                volume: self.volume.clone(),
            })?,
            store(&NamespaceQuery { environment: scope })?,
        ];
        if self.server.is_some() {
            asks.push(servers());
        }
        let mut answers = cx.read(asks)?.into_iter();
        let view = cx.view::<VolumeQuery>(answers.next())?;
        let namespace = cx.view::<NamespaceQuery>(answers.next())?;
        let target = match &self.server {
            Some(name) => {
                let answer = cx.cloud::<ServersAnswer>(answers.next())?;
                Some(resolve(&answer, name, "server", true)?.reference())
            }
            None => None,
        };
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::Copies {
                namespace: namespace.namespace,
                volume: Some(self.volume.clone()),
            })])?
            .into_iter();
        let answer = cx.cloud::<CopiesAnswer>(answers.next())?;
        let copies = copies_of(cx, &answer, &view)?;
        let writer = copies.writer.as_ref().map(|server| server.machine);
        let mirror = copies.mirror.as_ref().map(|mirror| &mirror.server);
        let run = match (target, mirror) {
            (None, Some(_)) => VolumeRun::Sync { full: self.full },
            (None, None) => {
                return Err(Refusal::invalid(
                    "It has no mirror yet; name the server to copy it to",
                    "server",
                )
                .into());
            }
            (Some(to), _) if Some(to.machine) == writer => {
                return Err(Refusal::invalid(
                    "That server has its writable copy; name another",
                    "server",
                )
                .into());
            }
            (Some(to), Some(mirror)) if mirror.machine == to.machine => {
                VolumeRun::Sync { full: self.full }
            }
            (Some(_), Some(mirror)) => {
                // One mirror: another server's would replace it, deleting it.
                let mut input = place(cx.call());
                input.insert("volume".into(), self.volume.as_str().into());
                return Err(Refusal::conflict(format!(
                    "It already has a mirror on {}; remove it first",
                    mirror.name.as_str()
                ))
                .with_next(Call::new("volume_mirror_remove", Value::Object(input)))
                .into());
            }
            (Some(to), None) => VolumeRun::Mirror { to },
        };
        on_volume(cx, &view, run, self.request_key, self.wait_seconds)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeMirrorRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    #[serde(default)]
    accept_volume_loss: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeMirrorRemove {
    const NAME: &'static str = "volume_mirror_remove";
    const DESCRIPTION: &'static str =
        "Delete a Volume's mirror; if it is the last copy, its data goes too.";
    const GATE: Gate = Gate::Cloud;
    const READ_ONLY: bool = false;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "volume": "pgdata" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let (view, _) = volume(
            cx,
            self.volume,
            self.project.as_ref(),
            self.environment.as_ref(),
            None,
        )?;
        on_volume(
            cx,
            &view,
            VolumeRun::RemoveMirror { loss },
            self.request_key,
            0,
        )
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeMove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    #[schemars(with = "String", pattern(NODE_NAME))]
    to: ServerName,
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeMove {
    const NAME: &'static str = "volume_move";
    const DESCRIPTION: &'static str = "Put a Volume and the Service using it on server to. The Service is down only for the final copy; the old server keeps a mirror. If a move stopped, the same call continues it.";
    const GATE: Gate = Gate::Cloud;
    const READ_ONLY: bool = false;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "volume": "pgdata", "to": "db-2", "wait_seconds": 300 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let (view, answer) = volume(
            cx,
            self.volume,
            self.project.as_ref(),
            self.environment.as_ref(),
            Some(servers()),
        )?;
        let answer = cx.cloud::<ServersAnswer>(answer)?;
        let to = resolve(&answer, &self.to, "to", true)?.reference();
        on_volume(
            cx,
            &view,
            VolumeRun::Move { to },
            self.request_key,
            self.wait_seconds,
        )
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeRelease {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    #[serde(default)]
    accept_volume_loss: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeRelease {
    const NAME: &'static str = "volume_release";
    const DESCRIPTION: &'static str = "Stop what runs on a volume and start its Service again on the old server with its data. A move past its handover finishes on its new server instead.";
    const GATE: Gate = Gate::Cloud;
    const READ_ONLY: bool = false;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "volume": "pgdata", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let (view, _) = volume(
            cx,
            self.volume,
            self.project.as_ref(),
            self.environment.as_ref(),
            None,
        )?;
        let mut moving = place(cx.call());
        moving.insert("volume".into(), view.volume.volume.name.as_str().into());
        on_volume(
            cx,
            &view,
            VolumeRun::Release { loss },
            self.request_key,
            self.wait_seconds,
        )
        .map_err(|stop| handed_over(stop, moving))
    }
}

/// A release refused after the handover retries as the move it names.
fn handed_over(stop: Stop, mut moving: Map<String, Value>) -> Stop {
    let Stop::Refuse(mut refusal) = stop else {
        return stop;
    };
    let to = refusal
        .details
        .as_object_mut()
        .and_then(|details| details.remove("move_to"));
    if let Some(Value::String(to)) = to {
        moving.insert("to".into(), to.into());
        refusal.retry = Call::new("volume_move", Value::Object(moving));
    }
    if refusal.details.as_object().is_some_and(Map::is_empty) {
        refusal.details = Value::Null;
    }
    Stop::Refuse(refusal)
}
