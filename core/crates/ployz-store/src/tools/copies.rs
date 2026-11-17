//! Tools on where a Volume's data is: show it, mirror and sync it, remove the
//! mirror, move it, release a move that stopped, and restore it from a copy.

use serde_json::{Map, Value, json};

use super::ToolId;
use super::act::{Run, latest, operate, operations, resolve, roster, servers};
use super::cloud::{CopiesAnswer, OperationsAnswer, VolumeCopies};
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool, WAIT_CAP_SECONDS, store};
use super::refusal::{Call, Refusal};
use super::step::{
    AcceptedLoss, Action, Ask, CloudQuery, Gate, RequestKey, ServerName, Subject, VolumeRun,
};
use crate::catalog::NODE_NAME;
use crate::{EnvironmentRef, NamespaceQuery, VolumeName, VolumeQuery, VolumeView, VolumesQuery};

/// `volume_show` for `volume` where the call is.
fn volume_show(cx: &Cx<'_>, volume: &VolumeName) -> Call {
    let mut input = cx.place_args();
    input.insert("volume".into(), volume.as_str().into());
    Call::to(ToolId::VolumeShow, Value::Object(input))
}

/// The Volume the call names, read alone.
fn volume(
    cx: &mut Cx<'_>,
    environment: EnvironmentRef,
    volume: VolumeName,
    also: Option<Ask>,
) -> Result<(VolumeView, Option<Result<Value, ployz_core::RpcError>>), Stop> {
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
        at: view.environment.to_string(),
        subject: Subject::Volume { volume: id.clone() },
        action: Action::Volume { volume: id, run },
        read: volume_show(cx, &view.volume.volume.name),
    };
    operate(cx, run, key, wait_seconds)
}

tool_input! {
    pub(crate) struct VolumeShow {
        /// The Volume's name, e.g. `pgdata`. Omit for every Volume in the Environment.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        volume: Option<VolumeName>,
        @place,
    }
}

impl Tool for VolumeShow {
    const NAME: &'static str = "volume_show";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Show where a Volume's data is";
    const DESCRIPTION: &'static str = "Show where each Volume's data lives: which Server has the writable copy, which has a read-only one (a Mirror or a move's old copy), and whether it is deployed. When exactly one Volume is shown, it lists recent runs and how each ended.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(&self);
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
        if volumes.is_empty() {
            return Ok(json!({ "at": place.to_string(), "volumes": [], "seen": null }));
        }
        let mut asks = vec![Ask::Cloud(CloudQuery::Copies {
            namespace: namespace.namespace,
            volume: self.volume,
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
        Ok(json!({ "at": place.to_string(), "volumes": shown, "seen": copies.seen }))
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

tool_input! {
    pub(crate) struct VolumeMirror {
        /// The Volume's name, e.g. `pgdata`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        /// Server to hold the Mirror, e.g. `db-2`, not the one with the writable copy. Omit to sync the existing Mirror.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        server: Option<ServerName>,
        /// Set true to recopy the whole Volume to a Mirror that diverged, as a run, instead of sending what changed.
        #[serde(default)]
        full: bool,
        @wait,
        @key,
        @place,
    }
}

impl Tool for VolumeMirror {
    const NAME: &'static str = "volume_mirror";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Mirror or sync a Volume";
    const DESCRIPTION: &'static str = "Give a Volume its one Mirror on another Server, or bring that Mirror up to date. A new Mirror is copied as a run. An existing one is sent what changed since its last sync, and the call returns before the download ends. Check the run with `volume_show`.";
    const GATE: Gate = Gate::Cloud;
    // Every call sends the Mirror a fresh sync, or starts a new run.
    const EFFECT: Effect = Effect::Remove { idempotent: false };
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata", "server": "db-2", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
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
                let answer = roster(cx, answers.next())?;
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
                let mut input = cx.place_args();
                input.insert("volume".into(), self.volume.as_str().into());
                return Err(Refusal::conflict(format!(
                    "It already has a mirror on {}; remove it first",
                    mirror.name.as_str()
                ))
                .with_next(Call::to(ToolId::VolumeMirrorRemove, Value::Object(input)))
                .into());
            }
            (Some(to), None) => VolumeRun::Mirror { to },
        };
        on_volume(cx, &view, run, self.request_key, self.wait_seconds)
    }
}

tool_input! {
    pub(crate) struct VolumeMirrorRemove {
        /// The Volume's name, e.g. `pgdata`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        @loss,
        @key,
        @place,
    }
}

impl Tool for VolumeMirrorRemove {
    const NAME: &'static str = "volume_mirror_remove";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Remove a Volume's read-only copies";
    const DESCRIPTION: &'static str = "Delete every read-only copy of a Volume: its Mirror and any old copy a move left, on every Server that holds one, right away. This cannot be undone. If no Server has a writable copy, the Volume's data goes with them. Returns the run.";
    const GATE: Gate = Gate::Cloud;
    // Once removed, the Volume has no read-only copy and the same call is refused.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let (view, _) = volume(cx, scope, self.volume, None)?;
        on_volume(
            cx,
            &view,
            VolumeRun::RemoveMirror { loss },
            self.request_key,
            0,
        )
    }
}

tool_input! {
    pub(crate) struct VolumeMove {
        /// The Volume's name, e.g. `pgdata`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        /// Server to move the writable copy and its Service to, e.g. `db-2`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        to: ServerName,
        @wait,
        @key,
        @place,
    }
}

impl Tool for VolumeMove {
    const NAME: &'static str = "volume_move";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Move a Volume to another Server";
    const DESCRIPTION: &'static str = "Move a Volume and the Service using it to another Server, right away. The Service is down only for the final copy. The old Server keeps a read-only old copy that never refreshes, which `volume_mirror_remove` deletes. Make the same call again to continue a move that stopped. Returns the run.";
    const GATE: Gate = Gate::Cloud;
    // The same call continues a move that stopped, and is refused once the
    // writable copy is on that Server.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata", "to": "db-2", "wait_seconds": WAIT_CAP_SECONDS })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let (view, answer) = volume(cx, scope, self.volume, Some(servers()))?;
        let answer = roster(cx, answer)?;
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

tool_input! {
    pub(crate) struct VolumeRelease {
        /// The Volume's name, e.g. `pgdata`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        @loss,
        @wait,
        @key,
        @place,
    }
}

impl Tool for VolumeRelease {
    const NAME: &'static str = "volume_release";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Release a Volume from its run";
    const DESCRIPTION: &'static str = "Stop the run under way on a Volume and start its Service again on the old Server with its data, right away. A move already past its handover finishes on the new Server instead: continue it with `volume_move`. Returns the run.";
    const GATE: Gate = Gate::Cloud;
    // Each call is a new release run, even with nothing left to stop.
    const EFFECT: Effect = Effect::Remove { idempotent: false };
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let (view, _) = volume(cx, scope, self.volume, None)?;
        let mut moving = cx.place_args();
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

tool_input! {
    pub(crate) struct VolumeRestore {
        /// The Volume's name, e.g. `pgdata`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        /// Server whose copy becomes the writable copy, e.g. `db-2`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        from: ServerName,
        @wait,
        @key,
        @place,
    }
}

impl Tool for VolumeRestore {
    const NAME: &'static str = "volume_restore";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Restore a Volume from a Server's copy";
    const DESCRIPTION: &'static str = "Make one Server's copy the Volume's writable copy when no Server has one, right away. It must be the only copy left, and a Mirror's copy lacks anything written after its last sync. The Service stays down until the next deploy. Returns the run.";
    const GATE: Gate = Gate::Cloud;
    // Once restored, that Server has the writable copy and the same call is
    // refused.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "volume": "pgdata", "from": "db-2", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let (view, answer) = volume(cx, scope, self.volume, Some(servers()))?;
        let answer = roster(cx, answer)?;
        let from = resolve(&answer, &self.from, "from", true)?.reference();
        on_volume(
            cx,
            &view,
            VolumeRun::Restore { from },
            self.request_key,
            self.wait_seconds,
        )
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
        refusal.retry = Call::to(ToolId::VolumeMove, Value::Object(moving));
    }
    if refusal.details.as_object().is_some_and(Map::is_empty) {
        refusal.details = Value::Null;
    }
    Stop::Refuse(refusal)
}
