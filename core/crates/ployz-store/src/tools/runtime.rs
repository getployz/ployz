//! Tools on a Service's containers: their state, their logs, and restarting,
//! starting or stopping them.

use std::collections::BTreeMap;
use std::time::Duration;

use ployz_core::ServiceName;
use serde_json::{Value, json};

use super::LineCap;
use super::ToolId;
use super::act::{Run, latest, operate, operations, resolve, roster, servers};
use super::cloud::{
    ContainerReport, ContainerState, ContainersAnswer, Health, LogsAnswer, OperationsAnswer,
};
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool, place_of, store};
use super::refusal::{Call, Refusal};
use super::status::{Container, ServiceFacts, Watch, service_status};
use super::step::{
    Action, Ask, CloudQuery, Gate, LogsQuery, RequestKey, ServerName, ServiceVerb, Subject,
};
use crate::catalog::NODE_NAME;
use crate::error::did_you_mean;
use crate::{
    DeploymentsQuery, EnvironmentName, NamespaceQuery, NamespaceView, NumberedDeploymentQuery,
    ProjectName, ServicesQuery, SourceKind,
};

/// The deployed Service `service` names, by the runtime name its containers carry.
pub(super) fn deployed<'a>(
    namespace: &'a NamespaceView,
    service: &ServiceName,
) -> Result<&'a ServiceName, Stop> {
    namespace.services.get(service).ok_or_else(|| {
        let names = || namespace.services.keys().map(ServiceName::as_str);
        Refusal::not_found("No deployed Service by that name here")
            .with_did_you_mean(did_you_mean(service.as_str(), names()))
            .with_valid(names())
            .with_next(Call::to(
                ToolId::EnvironmentShow,
                place_of(&namespace.environment),
            ))
            .into()
    })
}

/// A container as the canvas counts it.
const fn container(report: &ContainerReport) -> Container {
    match (report.state, report.health, report.exit_code) {
        (ContainerState::Running, None | Some(Health::Healthy), _) => Container::Serving,
        (ContainerState::Running, Some(Health::Starting | Health::Unhealthy), _) => {
            Container::Running
        }
        (ContainerState::Exited, _, _) if report.stopped => Container::Stopped,
        (ContainerState::Exited, _, Some(0))
        | (ContainerState::Created | ContainerState::Paused, _, _) => Container::Idle,
        (ContainerState::Exited | ContainerState::Restarting | ContainerState::Dead, _, _) => {
            Container::Down
        }
    }
}

tool_input! {
    pub(crate) struct ServiceState {
        /// Service to show, e.g. `web`. Leave out to show every Service in the Environment.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        service: Option<ServiceName>,
        @place,
    }
}

impl Tool for ServiceState {
    const NAME: &'static str = "service_status";
    const TITLE: &'static str = "Show Service status";
    const STATUS: ToolId = ToolId::ServiceState;
    const DESCRIPTION: &'static str = "Show how each Service is running, with the status the dashboard shows. Each container is listed by Server with its state, health, exit code, restarts, image and Deployment number. Naming one Service also shows its latest restart, start or stop.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let mut answers = cx
            .read(vec![
                store(&ServicesQuery {
                    environment: scope.clone(),
                })?,
                store(&NamespaceQuery {
                    environment: scope.clone(),
                })?,
                store(&DeploymentsQuery {
                    environment: scope,
                    limit: Some(20),
                    cursor: None,
                })?,
            ])?
            .into_iter();
        let mut listed = cx.view::<ServicesQuery>(answers.next())?;
        let namespace = cx.view::<NamespaceQuery>(answers.next())?;
        let page = cx.view::<DeploymentsQuery>(answers.next())?;
        if let Some(service) = &self.service {
            let names = listed
                .services
                .iter()
                .map(|listing| listing.service.name.as_str().to_owned())
                .collect::<Vec<_>>();
            if !names.iter().any(|name| name == service.as_str()) {
                return Err(Refusal::not_found("No Service by that name here")
                    .with_did_you_mean(did_you_mean(
                        service.as_str(),
                        names.iter().map(String::as_str),
                    ))
                    .with_valid(names.iter().map(String::as_str))
                    .with_next(Call::to(
                        ToolId::EnvironmentShow,
                        place_of(&namespace.environment),
                    ))
                    .into());
            }
            listed
                .services
                .retain(|listing| listing.service.name == *service);
        }
        let runtime = |name: &ServiceName| namespace.services.get(name).cloned();
        let mut asks = vec![Ask::Cloud(CloudQuery::Containers {
            namespace: namespace.namespace.clone(),
            service: self.service.as_ref().and_then(runtime),
        })];
        if let Some(service) = &self.service {
            asks.push(operations(
                Subject::Service {
                    environment: namespace.environment.id.clone(),
                    service: service.clone(),
                },
                1,
            ));
        }
        let mut answers = cx.read(asks)?.into_iter();
        let seen = cx.cloud::<ContainersAnswer>(answers.next())?;
        let last = answers
            .next()
            .map(|answer| cx.cloud::<OperationsAnswer>(Some(answer)))
            .transpose()?;
        let watch = match (
            seen.servers.is_empty(),
            seen.servers.len() == seen.seen.unanswered.len(),
        ) {
            (true, _) => Watch::NoServers,
            (false, true) => Watch::Unreachable,
            (false, false) => Watch::Observed {
                whole: seen.seen.unanswered.is_empty(),
            },
        };
        let numbers = page
            .deployments
            .iter()
            .filter_map(|deployment| Some((&deployment.id, deployment.number?)))
            .collect::<BTreeMap<_, _>>();
        let deploying = page
            .deployments
            .iter()
            .any(|deployment| deployment.in_flight);
        let services = listed
            .services
            .iter()
            .map(|listing| {
                let name = &listing.service.name;
                let carried = runtime(name);
                let reports = seen
                    .containers
                    .iter()
                    .filter(|report| carried.as_ref() == Some(&report.service))
                    .collect::<Vec<_>>();
                let counted = reports
                    .iter()
                    .map(|report| container(report))
                    .collect::<Vec<_>>();
                let status = service_status(&ServiceFacts {
                    new: carried.is_none(),
                    empty_source: listing.source == SourceKind::Empty,
                    watch,
                    containers: (!counted.is_empty()).then_some(counted.as_slice()),
                    desired_replicas: None,
                    deploying,
                });
                json!({
                    "service": name,
                    "status": status,
                    "containers": reports
                        .iter()
                        .map(|report| json!({
                            "server": report.server.as_str(),
                            "state": report.state,
                            "health": report.health,
                            "exit_code": report.exit_code,
                            "restarts": report.restarts,
                            "image": report.image,
                            "deployment": report.deployment.as_ref().and_then(|id| numbers.get(id)),
                        }))
                        .collect::<Vec<_>>(),
                    "latest": last
                        .as_ref()
                        .and_then(|page| page.operations.first())
                        .map(latest),
                })
            })
            .collect::<Vec<_>>();
        Ok(
            json!({ "at": namespace.environment.to_string(), "services": services, "seen": seen.seen }),
        )
    }
}

tool_input! {
    pub(crate) struct ServiceLogs {
        /// Service to read, e.g. `web`, as deployed now or in the Deployment given.
        #[schemars(with = "String", pattern(NODE_NAME))]
        service: ServiceName,
        /// Deployment number, e.g. `12`, to read only the containers that Deployment created. Leave out for every container of the Service.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        /// Server name, to read only its containers. Leave out for every Server.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        server: Option<ServerName>,
        /// How many of the newest lines to keep from each Server (default 100).
        #[serde(default)]
        #[schemars(with = "u16", range(min = 1, max = 200))]
        lines: LineCap,
        /// Only lines from the last this many minutes, e.g. `60`. Leave out for no time limit.
        #[serde(default)]
        #[schemars(range(min = 1, max = 10080))]
        since_minutes: Option<u32>,
        @place,
    }
}

impl Tool for ServiceLogs {
    const NAME: &'static str = "service_logs";
    const TITLE: &'static str = "Show Service logs";
    const STATUS: ToolId = ToolId::ServiceState;
    const DESCRIPTION: &'static str = "Show a Service's recent log lines from each Server, including containers that were removed. Each Server returns its newest lines, up to 16 KB in total and 500 B a line. Secrets are replaced by their names or [redacted].";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::DeploymentShow),
    };

    fn example() -> Value {
        json!({ "service": "web", "deployment": 12, "lines": 50 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let mut asks = vec![store(&NamespaceQuery {
            environment: scope.clone(),
        })?];
        let numbered = self.deployment.map(|number| NumberedDeploymentQuery {
            environment: scope,
            number: Some(number),
        });
        if let Some(query) = &numbered {
            asks.push(store(query)?);
        }
        if self.server.is_some() {
            asks.push(servers());
        }
        let mut answers = cx.read(asks)?.into_iter();
        let namespace = cx.view::<NamespaceQuery>(answers.next())?;
        let deployment = match numbered {
            Some(_) => Some(cx.view::<NumberedDeploymentQuery>(answers.next())?),
            None => None,
        };
        let only = match &self.server {
            Some(name) => {
                let answer = roster(cx, answers.next())?;
                Some(resolve(&answer, name, "server", false)?.reference())
            }
            None => None,
        };
        // A Deployment's containers carry the name it ran them under.
        let carried = deployment
            .as_ref()
            .and_then(|view| view.runtime_names.get(&self.service))
            .map_or_else(
                || deployed(&namespace, &self.service).cloned(),
                |name| Ok(name.clone()),
            )?;
        let query = LogsQuery {
            namespace: namespace.namespace.clone(),
            service: carried,
            deployment: deployment.map(|view| view.deployment.id),
            server: only,
            lines: self.lines,
            since: self
                .since_minutes
                .map(|minutes| Duration::from_secs(u64::from(minutes) * 60)),
        };
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::Logs(query))])?
            .into_iter();
        let logs = cx.cloud::<LogsAnswer>(answers.next())?;
        Ok(json!({
            "at": namespace.environment.to_string(),
            "untrusted": { "servers": logs.servers },
            "seen": logs.seen,
            "notes": logs.notes,
        }))
    }
}

/// Restart, start or stop `service`'s containers, on one server or all.
#[expect(
    clippy::too_many_arguments,
    reason = "each argument is one field of the agent tool call"
)]
fn service_act(
    cx: &mut Cx<'_>,
    verb: ServiceVerb,
    service: ServiceName,
    server: Option<ServerName>,
    wait_seconds: u32,
    key: Option<RequestKey>,
    project: Option<&ProjectName>,
    environment: Option<&EnvironmentName>,
) -> Result<Value, Stop> {
    let scope = cx.scope_in(project, environment);
    let mut asks = vec![store(&NamespaceQuery { environment: scope })?];
    if server.is_some() {
        asks.push(servers());
    }
    let mut answers = cx.read(asks)?.into_iter();
    let namespace = cx.view::<NamespaceQuery>(answers.next())?;
    deployed(&namespace, &service)?;
    let only = match &server {
        Some(name) => {
            let answer = roster(cx, answers.next())?;
            Some(resolve(&answer, name, "server", true)?.reference())
        }
        None => None,
    };
    let mut read = cx.place_args();
    read.insert("service".into(), service.as_str().into());
    let environment = namespace.environment.id.clone();
    let run = Run {
        at: namespace.environment.to_string(),
        subject: Subject::Service {
            environment: environment.clone(),
            service: service.clone(),
        },
        action: Action::Service {
            environment,
            service,
            only,
            verb,
        },
        read: Call::to(ToolId::ServiceState, Value::Object(read)),
    };
    operate(cx, run, key, wait_seconds)
}

macro_rules! service_act_tool {
    (
        $tool:ident,
        $name:literal,
        $title:literal,
        $verb:expr,
        $gate:expr,
        idempotent = $idempotent:literal,
        $description:literal
    ) => {
        tool_input! {
            pub(crate) struct $tool {
                /// Service to act on, e.g. `web`. It must be deployed in the Environment.
                #[schemars(with = "String", pattern(NODE_NAME))]
                service: ServiceName,
                /// Server name, to act only on the containers there. Leave out for every Server.
                #[serde(default)]
                #[schemars(with = "Option<String>", pattern(NODE_NAME))]
                server: Option<ServerName>,
                @wait,
                @key,
                @place,
            }
        }

        impl Tool for $tool {
            const NAME: &'static str = $name;
            const TITLE: &'static str = $title;
            const STATUS: ToolId = ToolId::ServiceState;
            const DESCRIPTION: &'static str = $description;
            const EFFECT: Effect = Effect::Change {
                idempotent: $idempotent,
            };
            const GATE: Gate = $gate;
            const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

            fn example() -> Value {
                json!({ "service": "web", "wait_seconds": 60 })
            }

            fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
                service_act(
                    cx,
                    $verb,
                    self.service,
                    self.server,
                    self.wait_seconds,
                    self.request_key,
                    self.project.as_ref(),
                    self.environment.as_ref(),
                )
            }
        }
    };
}

// Cloud skips a container already where a start or stop puts it, so repeating
// either changes nothing more; a restart stops and starts each container anew.
service_act_tool!(
    ServiceRestart,
    "service_restart",
    "Restart a Service",
    ServiceVerb::Restart,
    Gate::Cloud,
    idempotent = false,
    "Restart a Service's containers one at a time, so the rest keep serving. If one does not serve again, the rest are left untouched."
);
service_act_tool!(
    ServiceStart,
    "service_start",
    "Start a Service",
    ServiceVerb::Start,
    Gate::Never,
    idempotent = true,
    "Start a Service's stopped containers. Running ones are left as they are."
);
service_act_tool!(
    ServiceStop,
    "service_stop",
    "Stop a Service",
    ServiceVerb::Stop,
    Gate::Cloud,
    idempotent = true,
    "Stop a Service's containers. Nothing is deleted, and they stay stopped until you start them again with `service_start`."
);

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
