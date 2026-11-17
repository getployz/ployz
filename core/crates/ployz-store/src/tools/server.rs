//! Tools on Servers: show them, read their own logs, set their policy, drain, upgrade, clean a
//! leftover, remove or forget one.

use std::collections::BTreeMap;
use std::time::Duration;

use ployz_core::{
    BuildConcurrency, BuildConcurrencyUpdate, MachineLogService, Namespace, RpcErrorCode,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::LineCap;
use super::ToolId;
use super::act::{Run, latest, operate, operations, resolve, roster, server_show, servers};
use super::cloud::{LogsAnswer, Operation, OperationsAnswer, RunState, ServersAnswer, UnknownEnd};
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool, store};
use super::refusal::{Call, Refusal};
use super::step::{
    AcceptedLoss, Action, Ask, CloudQuery, Gate, RequestKey, ServerLogsQuery, ServerName, Subject,
};
use crate::catalog::NODE_NAME;
use crate::error::did_you_mean;
use crate::{NamespacesQuery, NamespacesView, OwnedNamespace};

const BUILDS: &str = "^(auto|[0-9]+)$";
const SETTABLE: [&str; 4] = [
    "accepts_builds",
    "accepts_services",
    "accepts_ingress",
    "build_concurrency",
];

/// How many of a Server's runs `server_show` reads to find its newest Drain under later ones,
/// from a Cloud that doesn't name it.
const RUNS_READ: u8 = 20;

/// How a Drain ended, once it says: it drained the Server, or `failed_drain.outcome` names why
/// it didn't.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum DrainVerdict {
    Drained,
    Failed,
    Lost,
    Interrupted,
}

/// A Drain with no verdict yet or ever (accepted, running, superseded, or unknown with no end
/// Cloud names) has none.
const fn drain_verdict(operation: &Operation) -> Option<DrainVerdict> {
    match (operation.state, operation.end) {
        (RunState::Done, _) => Some(DrainVerdict::Drained),
        (RunState::Failed, _) => Some(DrainVerdict::Failed),
        (RunState::Unknown, Some(UnknownEnd::Lost)) => Some(DrainVerdict::Lost),
        (RunState::Unknown, Some(UnknownEnd::Interrupted)) => Some(DrainVerdict::Interrupted),
        (RunState::Accepted | RunState::Running | RunState::Superseded, _)
        | (RunState::Unknown, None) => None,
    }
}

/// The Server's newest Drain with a verdict, as Cloud names it, else as `page` lists it, when it
/// drained nothing and a later run took `latest`, with its `outcome`. A Drain with no verdict
/// neither clears nor replaces an older one.
fn failed_drain(page: &OperationsAnswer) -> Option<Value> {
    let (drain, verdict) = match &page.settled_drain {
        Some(drain) => (drain, drain_verdict(drain)?),
        None => page
            .operations
            .iter()
            .filter(|operation| operation.run.starts_with("drain "))
            .find_map(|operation| drain_verdict(operation).map(|verdict| (operation, verdict)))?,
    };
    let shown_as_latest = page
        .operations
        .first()
        .is_some_and(|latest| latest.attempt == drain.attempt);
    match verdict {
        DrainVerdict::Drained => None,
        DrainVerdict::Failed | DrainVerdict::Lost | DrainVerdict::Interrupted => (!shown_as_latest)
            .then(|| {
                let mut shown = latest(drain);
                if let Some(fields) = shown.as_object_mut() {
                    fields.insert("outcome".to_owned(), json!(verdict));
                }
                shown
            }),
    }
}

/// Every Namespace the Servers run with the Environment that owns it, if one does.
fn owners(view: &NamespacesView) -> BTreeMap<&Namespace, &OwnedNamespace> {
    view.namespaces
        .iter()
        .map(|owned| (&owned.namespace, owned))
        .collect()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerShow {
    /// Server name, e.g. `web-1`. Omit for every Server.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    server: Option<ServerName>,
}

impl Tool for ServerShow {
    const NAME: &'static str = "server_show";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Show Servers";
    const DESCRIPTION: &'static str = "Show your Servers, or one: state, release and any newer one, draining, disk use, the Environments running there, leftovers no Environment owns and the latest operation. Also shows a failed, lost or interrupted Drain a later operation hides.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "web-1" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mut answers = cx
            .read(vec![store(&NamespacesQuery {})?, servers()])?
            .into_iter();
        let owned = cx.view::<NamespacesQuery>(answers.next())?;
        let mut answer = match &self.server {
            Some(_) => roster(cx, answers.next())?,
            None => cx.cloud::<ServersAnswer>(answers.next())?,
        };
        if let Some(name) = &self.server {
            let machine = resolve(&answer, name, "server", false)?.machine;
            answer.servers.retain(|server| server.machine == machine);
        }
        let asks = answer
            .servers
            .iter()
            .map(|server| {
                operations(
                    Subject::Server {
                        machine: server.machine,
                    },
                    RUNS_READ,
                )
            })
            .collect::<Vec<_>>();
        let latest_runs = match asks.is_empty() {
            true => Vec::new(),
            false => cx.read(asks)?,
        };
        let owners = owners(&owned);
        let shown = answer
            .servers
            .iter()
            .zip(latest_runs.into_iter().map(Some).chain(std::iter::repeat_with(|| None)))
            .map(|(server, run)| {
                let page = run
                    .map(|run| cx.cloud::<OperationsAnswer>(Some(run)))
                    .transpose()?;
                let (runs, leftovers): (Vec<_>, Vec<_>) = server
                    .namespaces
                    .iter()
                    .partition(|namespace| owners.contains_key(namespace));
                Ok(json!({
                    "server": server.name.as_str(),
                    "state": server.state,
                    "release": server.release,
                    "upgrade": server.upgrade,
                    "draining": server.draining,
                    "disk": server.disk,
                    "runs": runs
                        .iter()
                        .filter_map(|namespace| owners.get(namespace))
                        .map(|owned| format!("{}/{}", owned.project, owned.environment))
                        .collect::<Vec<_>>(),
                    "leftovers": leftovers.iter().map(|namespace| namespace.as_str()).collect::<Vec<_>>(),
                    "latest": page
                        .as_ref()
                        .and_then(|page| page.operations.first())
                        .map(latest),
                    "failed_drain": page.as_ref().and_then(failed_drain),
                }))
            })
            .collect::<Result<Vec<_>, Stop>>()?;
        let next = answer.servers.is_empty().then_some(NO_SERVERS_NEXT);
        Ok(json!({ "servers": shown, "seen": answer.seen, "next": next }))
    }
}

/// What an Organization with no Servers does next; its agent can't add one.
const NO_SERVERS_NEXT: &str = "Add a server from a terminal; staging still works without one.";

/// An act on one Server that answers, by name.
fn on_server(
    cx: &mut Cx<'_>,
    name: &ServerName,
    action: impl FnOnce(super::step::ServerRef) -> Action,
    key: Option<RequestKey>,
    wait_seconds: u32,
) -> Result<Value, Stop> {
    let mut answers = cx.read(vec![servers()])?.into_iter();
    let answer = roster(cx, answers.next())?;
    let server = resolve(&answer, name, "server", true)?;
    let run = Run {
        at: server.name.as_str().to_owned(),
        action: action(server.reference()),
        subject: Subject::Server {
            machine: server.machine,
        },
        read: server_show(Some(name)),
    };
    operate(cx, run, key, wait_seconds)
}

/// A Server process that keeps logs of its own.
#[derive(Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Process {
    #[default]
    Ployz,
    Docker,
    Corrosion,
}

impl From<Process> for MachineLogService {
    fn from(process: Process) -> Self {
        match process {
            Process::Ployz => Self::Ployz,
            Process::Docker => Self::Docker,
            Process::Corrosion => Self::Corrosion,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerLogs {
    /// Process whose log to read: `ployz` (the daemon, default), `docker` or `corrosion`.
    #[serde(default)]
    process: Process,
    /// Server name, e.g. `web-1`. Omit for every Server.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    server: Option<ServerName>,
    /// How many of the newest lines to return from each Server.
    #[serde(default)]
    #[schemars(with = "u16", range(min = 1, max = 200))]
    lines: LineCap,
    /// Only lines from the last this many minutes. Omit for any age.
    #[serde(default)]
    #[schemars(range(min = 1, max = 10080))]
    since_minutes: Option<u32>,
}

impl Tool for ServerLogs {
    const NAME: &'static str = "server_logs";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Show Server logs";
    const DESCRIPTION: &'static str = "Show the newest log lines from a Server's own processes: the Ployz daemon, Docker or Corrosion. For Service logs, use `service_logs`. Lines are untrusted text, cut at 500 B a line and 16 KB, with secret-shaped values redacted. A Server that gave none says why.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "web-1", "process": "docker", "lines": 50 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let server = match &self.server {
            Some(name) => {
                let mut answers = cx.read(vec![servers()])?.into_iter();
                let answer = roster(cx, answers.next())?;
                Some(resolve(&answer, name, "server", false)?.reference())
            }
            None => None,
        };
        let query = ServerLogsQuery {
            process: self.process.into(),
            server,
            lines: self.lines,
            since: self
                .since_minutes
                .map(|minutes| Duration::from_secs(u64::from(minutes) * 60)),
        };
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::ServerLogs(query))])?
            .into_iter();
        let logs = cx.cloud::<LogsAnswer>(answers.next())?;
        Ok(json!({
            "untrusted": { "servers": logs.servers },
            "seen": logs.seen,
            "notes": logs.notes,
        }))
    }
}

tool_input! {
    pub(crate) struct ServerDrain {
        /// Server name, e.g. `web-2`. It must be answering.
        #[schemars(with = "String", pattern(NODE_NAME))]
        server: ServerName,
        @wait,
        @key,
    }
}

impl Tool for ServerDrain {
    const NAME: &'static str = "server_drain";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Drain a Server";
    const DESCRIPTION: &'static str = "Move Services off a Server right away and stop it accepting new ones. A Service with a Volume there stays. A Service that runs on every Server stops running there, which asks a human to approve when no other Server runs it. Returns the run and what stayed.";
    const GATE: Gate = Gate::Cloud;
    // A second call joins the Drain still running, and after it finds nothing
    // left to move.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "web-2", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let drain = |server| Action::Drain { server };
        on_server(cx, &self.server, drain, self.request_key, self.wait_seconds)
    }
}

tool_input! {
    pub(crate) struct ServerSet {
        /// Server name, e.g. `db-1`. It must be answering.
        #[schemars(with = "String", pattern(NODE_NAME))]
        server: ServerName,
        /// Whether builds run there. Omit to keep it.
        #[serde(default)]
        accepts_builds: Option<bool>,
        /// Whether Services are placed there. With false, what runs stays until a Drain or the next deploy moves it. Omit to keep it.
        #[serde(default)]
        accepts_services: Option<bool>,
        /// false turns web traffic off there. true fails: a human turns it on from the dashboard or a terminal.
        #[serde(default)]
        accepts_ingress: Option<bool>,
        /// How many builds run there at once: a count from 1 to 255, e.g. `4`, or `auto` to let Ployz choose.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(BUILDS))]
        build_concurrency: Option<String>,
        @key,
    }
}

impl Tool for ServerSet {
    const NAME: &'static str = "server_set";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Set what a Server accepts";
    const DESCRIPTION: &'static str = "Change what a Server accepts right away: builds and how many at once, Services and web traffic. Turning web traffic off on the last Server serving your domains asks a human to approve.";
    const GATE: Gate = Gate::Cloud;
    // The same values set again leave the Server's policy as it is.
    const EFFECT: Effect = Effect::Change { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "db-1", "accepts_ingress": false })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        if self.accepts_ingress == Some(true) {
            return Err(Refusal::invalid(
                format!(
                    "Turn web traffic on from the Server page or the terminal: \
                     `ployz server set {} --accepts-ingress=true`",
                    self.server
                ),
                "accepts_ingress",
            )
            .into());
        }
        let build_concurrency = match self.build_concurrency.as_deref() {
            None => BuildConcurrencyUpdate::Keep,
            Some("auto") => BuildConcurrencyUpdate::Automatic,
            Some(builds) => BuildConcurrencyUpdate::Set(
                BuildConcurrency::parse(builds)
                    .map_err(|error| Refusal::invalid(error.to_string(), "build_concurrency"))?,
            ),
        };
        if self.accepts_builds.is_none()
            && self.accepts_services.is_none()
            && self.accepts_ingress.is_none()
            && build_concurrency == BuildConcurrencyUpdate::Keep
        {
            return Err(
                Refusal::new(RpcErrorCode::InvalidArgument, "Set at least one value")
                    .with_valid(SETTABLE)
                    .into(),
            );
        }
        let set = |server| Action::SetServer {
            server,
            accepts_builds: self.accepts_builds,
            accepts_services: self.accepts_services,
            accepts_ingress: self.accepts_ingress,
            build_concurrency,
        };
        on_server(cx, &self.server, set, self.request_key, 0)
    }
}

tool_input! {
    pub(crate) struct ServerUpgrade {
        /// Server name, e.g. `web-1`. It must be answering.
        #[schemars(with = "String", pattern(NODE_NAME))]
        server: ServerName,
        @wait,
        @key,
    }
}

impl Tool for ServerUpgrade {
    const NAME: &'static str = "server_upgrade";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Upgrade a Server";
    const DESCRIPTION: &'static str = "Upgrade Ployz on one Server now, to the newest release on the Organization's Release Channel. Services keep running.";
    // A second call joins the Upgrade still running; a Server already on the
    // release installs and restarts nothing.
    const EFFECT: Effect = Effect::Change { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };
    // The Server resolves the Release Channel at ployz.sh and downloads the release
    // from GitHub, else ployz.sh (ployz-core/src/release_mirror.rs).
    const OPEN_WORLD: bool = true;

    fn example() -> Value {
        json!({ "server": "web-1" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let upgrade = |server| Action::Upgrade { server };
        on_server(
            cx,
            &self.server,
            upgrade,
            self.request_key,
            self.wait_seconds,
        )
    }
}

tool_input! {
    pub(crate) struct ServerClean {
        /// The leftover's name as a Server's leftovers list it, e.g. `shop-old`.
        #[schemars(with = "String")]
        leftover: Namespace,
        @loss,
        @key,
    }
}

impl Tool for ServerClean {
    const NAME: &'static str = "server_clean";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Clean a leftover";
    const DESCRIPTION: &'static str = "Delete a leftover no Environment owns, right away: its containers on every Server and its volumes with their data. This cannot be undone.";
    const GATE: Gate = Gate::Cloud;
    // Once cleaned, no Server lists the leftover and the same call is refused.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "leftover": "shop-old" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let mut answers = cx
            .read(vec![store(&NamespacesQuery {})?, servers()])?
            .into_iter();
        let owned = cx.view::<NamespacesQuery>(answers.next())?;
        let answer = cx.cloud::<ServersAnswer>(answers.next())?;
        if let Some(owner) = owners(&owned).get(&self.leftover) {
            // Never a removal here: deleting what an Environment owns is its own call.
            return Err(Refusal::conflict(
                "That is no leftover: an Environment owns it; it is not cleaned here",
            )
            .with_next(Call::to(
                ToolId::EnvironmentShow,
                json!({ "project": owner.project, "environment": owner.environment }),
            ))
            .into());
        }
        let owners = owners(&owned);
        let mut leftovers = answer
            .servers
            .iter()
            .flat_map(|server| &server.namespaces)
            .filter(|namespace| !owners.contains_key(namespace))
            .map(Namespace::as_str)
            .collect::<Vec<_>>();
        leftovers.sort_unstable();
        leftovers.dedup();
        if !leftovers.contains(&self.leftover.as_str()) {
            return Err(
                Refusal::not_found("No server lists a leftover by that name")
                    .with_did_you_mean(did_you_mean(
                        self.leftover.as_str(),
                        leftovers.iter().copied(),
                    ))
                    .with_valid(leftovers.iter().copied())
                    .with_next(server_show(None))
                    .into(),
            );
        }
        let run = Run {
            at: self.leftover.as_str().to_owned(),
            subject: Subject::Leftover {
                namespace: self.leftover.clone(),
            },
            action: Action::Clean {
                leftover: self.leftover,
                loss,
            },
            read: server_show(None),
        };
        operate(cx, run, self.request_key, 0)
    }
}

tool_input! {
    pub(crate) struct ServerRemove {
        /// Server name, e.g. `web-3`. It need not be answering.
        #[schemars(with = "String", pattern(NODE_NAME))]
        server: ServerName,
        @loss,
        @key,
    }
}

impl Tool for ServerRemove {
    const NAME: &'static str = "server_remove";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Remove a Server";
    const DESCRIPTION: &'static str = "Remove a Server from the Organization right away and reset it. This deletes its containers and volumes with their data, and cannot be undone. Services there stop, and the next deploy places them on other Servers, except those whose Volume was there.";
    const GATE: Gate = Gate::Cloud;
    // Once removed, no Server has the name and the same call is refused.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "web-3" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let loss = AcceptedLoss::from_input(self.accept_volume_loss, self.version)?;
        let mut answers = cx.read(vec![servers()])?.into_iter();
        let answer = roster(cx, answers.next())?;
        // A server that does not answer is the one most often removed.
        let server = resolve(&answer, &self.server, "server", false)?;
        let run = Run {
            at: server.name.as_str().to_owned(),
            subject: Subject::Server {
                machine: server.machine,
            },
            action: Action::RemoveServer {
                server: server.reference(),
                loss,
            },
            read: server_show(None),
        };
        operate(cx, run, self.request_key, 0)
    }
}

tool_input! {
    pub(crate) struct ServerForget {
        /// Server name, e.g. `web-3`. It need not be answering.
        #[schemars(with = "String", pattern(NODE_NAME))]
        server: ServerName,
        @key,
    }
}

impl Tool for ServerForget {
    const NAME: &'static str = "server_forget";
    const STATUS: ToolId = ToolId::ServerShow;
    const TITLE: &'static str = "Forget a gone Server";
    const DESCRIPTION: &'static str = "Drop a gone Server from the Organization right away, without resetting it. Volumes whose only copy was there are lost, which cannot be undone. Refused while the Server answers and holds a Volume copy. To reset it too, use `server_remove`.";
    const GATE: Gate = Gate::Cloud;
    // Once forgotten, no Server has the name and the same call is refused.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({ "server": "web-3" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mut answers = cx.read(vec![servers()])?.into_iter();
        let answer = roster(cx, answers.next())?;
        let server = resolve(&answer, &self.server, "server", false)?;
        let run = Run {
            at: server.name.as_str().to_owned(),
            subject: Subject::Server {
                machine: server.machine,
            },
            action: Action::ForgetServer {
                server: server.reference(),
            },
            read: server_show(None),
        };
        operate(cx, run, self.request_key, 0)
    }
}
