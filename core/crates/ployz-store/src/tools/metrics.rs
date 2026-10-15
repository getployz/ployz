//! Tools on what a Service or Server used over a range, and what Docker did to
//! its containers: the table `ployz metrics` prints.

use std::collections::BTreeMap;

use ployz_core::ServiceName;
use ployz_core::metrics::MarkerKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::act::{resolve, servers};
use super::cloud::{MetricEvent, MetricsAnswer, ServersAnswer};
use super::machine::{Cx, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::step::{
    Ask, CloudQuery, MetricsQuery, MetricsSince, MetricsSubject, ServerName, Timestamp,
};
use crate::catalog::NODE_NAME;
use crate::error::did_you_mean;
use crate::{
    DeploymentId, DeploymentsQuery, EnvironmentName, NamespaceQuery, NamespacesQuery,
    NumberedDeploymentQuery, ProjectName,
};

const DEFAULT_MINUTES: u32 = 60;

fn ask(cx: &mut Cx<'_>, query: MetricsQuery) -> Result<MetricsAnswer, Stop> {
    let mut answers = cx
        .read(vec![Ask::Cloud(CloudQuery::Metrics(query))])?
        .into_iter();
    cx.cloud::<MetricsAnswer>(answers.next())
}

/// `event` as a tool shows it: when, where, and the kind with its exit code.
/// An exit's OOM and stop evidence stays out, as in every other reader: the
/// OOM kill is its own event.
fn event(event: &MetricEvent) -> Map<String, Value> {
    let mut shown = Map::new();
    shown.insert("at".into(), json!(event.at));
    shown.insert("server".into(), json!(event.server.as_str()));
    match event.kind {
        MarkerKind::Oom => {
            shown.insert("kind".into(), json!("oom"));
        }
        MarkerKind::Restart => {
            shown.insert("kind".into(), json!("restart"));
        }
        MarkerKind::Exit { exit_code, .. } => {
            shown.insert("kind".into(), json!("exit"));
            if let Some(code) = exit_code {
                shown.insert("exit_code".into(), json!(code));
            }
        }
    }
    shown
}

fn shown(answer: MetricsAnswer, events: Vec<Map<String, Value>>) -> Map<String, Value> {
    let Value::Object(shown) = json!({
        "since": answer.since,
        "until": answer.until,
        "metrics": answer.metrics,
        "oom_kills": answer.oom_kills,
        "exits": answer.exits,
        "restarts": answer.restarts,
        "events": events,
        "notes": answer.notes,
        "seen": answer.seen,
    }) else {
        unreachable!("json! of an object literal is an object")
    };
    shown
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceMetrics {
    #[schemars(with = "String", pattern(NODE_NAME))]
    service: ServiceName,
    /// Deployment number: from its start, its events only.
    #[serde(default)]
    #[schemars(range(min = 1))]
    deployment: Option<u64>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 10080))]
    since_minutes: Option<u32>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ServiceMetrics {
    const NAME: &'static str = "service_metrics";
    const DESCRIPTION: &'static str = "A Service's CPU, memory, network and disk (now, avg, peak) and its OOM kills, exits and restarts. Default: the last hour.";
    const READ_ONLY: bool = true;
    const LOCAL_NEXT: Option<&'static str> = Some("deployment_show");

    fn example() -> Value {
        json!({ "service": "web", "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let mut asks = vec![
            store(&NamespaceQuery {
                environment: scope.clone(),
            })?,
            store(&DeploymentsQuery {
                environment: scope.clone(),
                limit: Some(20),
                cursor: None,
            })?,
        ];
        let numbered = self.deployment.map(|number| NumberedDeploymentQuery {
            environment: scope,
            number,
        });
        if let Some(query) = &numbered {
            asks.push(store(query)?);
        }
        let mut answers = cx.read(asks)?.into_iter();
        let namespace = cx.view::<NamespaceQuery>(answers.next())?;
        let page = cx.view::<DeploymentsQuery>(answers.next())?;
        let deployment = match numbered {
            Some(_) => Some(cx.view::<NumberedDeploymentQuery>(answers.next())?),
            None => None,
        };
        // A Deployment's containers carry the name it ran them under. Its events say nothing
        // about a Service it did not deploy.
        let carried = match &deployment {
            Some(view) => view
                .runtime_names
                .get(&self.service)
                .cloned()
                .ok_or_else(|| {
                    let names = || view.runtime_names.keys().map(ServiceName::as_str);
                    Stop::from(
                        Refusal::not_found("That Deployment did not deploy this Service")
                            .with_did_you_mean(did_you_mean(self.service.as_str(), names()))
                            .with_valid(names()),
                    )
                })?,
            None => namespace
                .services
                .get(&self.service)
                .cloned()
                .ok_or_else(|| {
                    let names = || namespace.services.keys().map(ServiceName::as_str);
                    Stop::from(
                        Refusal::not_found("No deployed Service by that name here")
                            .with_did_you_mean(did_you_mean(self.service.as_str(), names()))
                            .with_valid(names())
                            .with_next(Call::new(
                                "environment_show",
                                json!({
                                    "project": namespace.environment.project,
                                    "environment": namespace.environment.name,
                                }),
                            )),
                    )
                })?,
        };
        let since = match (self.since_minutes, &deployment) {
            (Some(minutes), _) => MetricsSince::MinutesAgo { minutes },
            (None, Some(view)) => MetricsSince::At {
                at: Timestamp(
                    view.deployment
                        .started_at
                        .unwrap_or(view.deployment.admitted_at),
                ),
            },
            (None, None) => MetricsSince::MinutesAgo {
                minutes: DEFAULT_MINUTES,
            },
        };
        let answer = ask(
            cx,
            MetricsQuery {
                subject: MetricsSubject::Service {
                    namespace: namespace.namespace.clone(),
                    service: carried,
                },
                since,
                deployment: deployment.map(|view| view.deployment.id),
            },
        )?;
        let numbers: BTreeMap<&DeploymentId, u64> = page
            .deployments
            .iter()
            .map(|deployment| (&deployment.id, deployment.number))
            .collect();
        let events = answer
            .events
            .iter()
            .map(|marker| {
                let mut shown = event(marker);
                shown.insert(
                    "deployment".into(),
                    json!(marker.deployment.as_ref().and_then(|id| numbers.get(id))),
                );
                shown
            })
            .collect();
        let mut shown = shown(answer, events);
        shown.insert("at".into(), json!(at(&namespace.environment)));
        Ok(Value::Object(shown))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerMetrics {
    #[schemars(with = "String", pattern(NODE_NAME))]
    server: ServerName,
    #[serde(default)]
    #[schemars(range(min = 1, max = 10080))]
    since_minutes: Option<u32>,
}

impl Tool for ServerMetrics {
    const NAME: &'static str = "server_metrics";
    const DESCRIPTION: &'static str = "A server's CPU, memory, network and disk (now, avg, peak) and its containers' OOM kills, exits and restarts. Default: the last hour.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "server": "web-1", "since_minutes": 180 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        cx.cloud_only()?;
        let mut answers = cx
            .read(vec![store(&NamespacesQuery {})?, servers()])?
            .into_iter();
        let owned = cx.view::<NamespacesQuery>(answers.next())?;
        let listed = cx.cloud::<ServersAnswer>(answers.next())?;
        let server = resolve(&listed, &self.server, "server", false)?.reference();
        let answer = ask(
            cx,
            MetricsQuery {
                subject: MetricsSubject::Server { server },
                since: MetricsSince::MinutesAgo {
                    minutes: self.since_minutes.unwrap_or(DEFAULT_MINUTES),
                },
                deployment: None,
            },
        )?;
        let owners: BTreeMap<_, _> = owned
            .namespaces
            .iter()
            .map(|owned| (&owned.namespace, owned))
            .collect();
        let events = answer
            .events
            .iter()
            .map(|marker| {
                let mut shown = event(marker);
                shown.insert("service".into(), json!(marker.service.as_str()));
                match owners.get(&marker.namespace) {
                    Some(owned) => {
                        shown.insert("project".into(), json!(owned.project));
                        shown.insert("environment".into(), json!(owned.environment));
                    }
                    None => {
                        shown.insert("leftover".into(), json!(marker.namespace.as_str()));
                    }
                }
                shown
            })
            .collect();
        Ok(Value::Object(shown(answer, events)))
    }
}
