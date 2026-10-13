//! Tools on Deployments: list them, follow one to its end, retry, start or
//! cancel one.

use std::collections::BTreeMap;

use ployz_core::{MachineId, RpcErrorCode};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::release::place;
use super::step::RequestKey;
use crate::catalog::NODE_NAME;
use crate::{
    Admit, Cancel, DeployedNode, DeploymentId, DeploymentQuery, DeploymentSummary, DeploymentView,
    DeploymentsQuery, DeploymentsView, EnvironmentName, EnvironmentRef, EnvironmentSummary,
    NodeOutcome, NumberedDeploymentQuery, Outcome, ProjectName, Retry, RowState, Start,
};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

/// The Deployment `number` names, or the latest when it names none.
fn find(
    cx: &mut Cx<'_>,
    environment: EnvironmentRef,
    number: Option<u64>,
) -> Result<NumberedDeploymentQuery, Stop> {
    let number = match number {
        Some(number) => number,
        None => latest(cx.one(&newest(&environment))?)?,
    };
    Ok(NumberedDeploymentQuery {
        environment,
        number,
    })
}

fn newest(environment: &EnvironmentRef) -> DeploymentsQuery {
    DeploymentsQuery {
        environment: environment.clone(),
        limit: Some(1),
        cursor: None,
    }
}

fn latest(page: DeploymentsView) -> Result<u64, Stop> {
    page.deployments
        .first()
        .map(|latest| latest.number)
        .ok_or_else(|| {
            Refusal::not_found("This Environment has no Deployment yet")
                .with_next(Call::new("deploy", json!({})))
                .into()
        })
}

/// Why the Deployment ended as it did, when its runner said.
fn reason(outcome: Option<&Outcome>) -> Option<&str> {
    match outcome? {
        Outcome::Executed { reason, .. } => reason.as_deref(),
        Outcome::NotExecuted { reason, .. } => Some(reason),
        Outcome::NeverRan | Outcome::Forgotten => None,
    }
}

/// One node's outcome, with the first Server its work failed on.
fn node(node: &NodeOutcome) -> Value {
    let failed = node.rows.iter().find_map(|row| match &row.state {
        RowState::Failed {
            reason, step, log, ..
        } => Some((row.server.as_str(), reason, step, log)),
        RowState::Pending
        | RowState::Running { .. }
        | RowState::Completed
        | RowState::NotAttempted
        | RowState::Unknown => None,
    });
    let mut shown = json!({ "name": node.node.name(), "outcome": node.outcome });
    if let (Some((server, reason, step, log)), Value::Object(object)) = (failed, &mut shown) {
        object.insert("server".into(), server.into());
        object.insert("reason".into(), reason.as_str().into());
        object.insert("step".into(), json!(step));
        object.insert("excerpt".into(), json!(log));
    }
    shown
}

/// How far a running Deployment got: the phase its first busy row is in, and
/// how many of its Servers finished every row.
fn progress(view: &DeploymentView) -> Value {
    let rows = || view.nodes.iter().flat_map(|node| &node.rows);
    let phase = rows().find_map(|row| match &row.state {
        RowState::Running { phase } => Some(*phase),
        RowState::Pending
        | RowState::Completed
        | RowState::Failed { .. }
        | RowState::NotAttempted
        | RowState::Unknown => None,
    });
    let mut servers = BTreeMap::<&MachineId, bool>::new();
    for row in rows() {
        let done = servers.entry(&row.machine_id).or_insert(true);
        *done &= row.state.finished();
    }
    json!({
        "phase": phase,
        "servers_done": servers.values().filter(|done| **done).count(),
        "servers_total": servers.len(),
    })
}

/// `service_logs` and `service_metrics` for the first Service whose work
/// failed, over this Deployment: an exit code shows in logs, an OOM kill only in
/// metrics.
fn diagnose(view: &DeploymentView, call: &Call) -> Option<[Call; 2]> {
    let service = view.nodes.iter().find_map(|node| match &node.node {
        DeployedNode::Service { name, .. }
            if node
                .rows
                .iter()
                .any(|row| matches!(row.state, RowState::Failed { .. })) =>
        {
            Some(name)
        }
        DeployedNode::Service { .. }
        | DeployedNode::Volume { .. }
        | DeployedNode::Config { .. } => None,
    })?;
    let mut input = place(call);
    input.insert("service".into(), json!(service));
    input.insert("deployment".into(), view.deployment.number.into());
    let input = Value::Object(input);
    Some([
        Call::new("service_logs", input.clone()),
        Call::new("service_metrics", input),
    ])
}

fn summary(deployment: &DeploymentSummary) -> Value {
    json!({
        "deployment": deployment.number,
        "status": deployment.status,
        "in_flight": deployment.in_flight,
        "reason": reason(deployment.outcome.as_ref()),
        "message": deployment.message,
        "saved_revision": deployment.saved.0,
        "started_at": deployment.started_at,
        "ended_at": deployment.ended_at,
    })
}

fn show(view: &DeploymentView) -> Value {
    let mut shown = summary(&view.deployment);
    if let Value::Object(object) = &mut shown {
        object.insert("at".into(), json!(at(&view.environment)));
        object.insert(
            "nodes".into(),
            json!(view.nodes.iter().map(node).collect::<Vec<_>>()),
        );
    }
    shown
}

fn follow(call: &Call, number: u64) -> Call {
    let mut input = place(call);
    input.insert("deployment".into(), number.into());
    input.insert("wait_seconds".into(), 120.into());
    Call::new("deployment_show", Value::Object(input))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeploymentList {
    #[serde(default)]
    #[schemars(range(min = 1, max = 100))]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentList {
    const NAME: &'static str = "deployment_list";
    const DESCRIPTION: &'static str =
        "An Environment's Deployments, newest first, with each one's status.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "environment": "staging", "limit": 10 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let page = cx.one(&DeploymentsQuery {
            environment: scope,
            limit: self.limit,
            cursor: self.cursor,
        })?;
        let mut listed = json!({
            "at": at(&page.environment),
            "deployments": page.deployments.iter().map(summary).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        });
        if let (Some(cursor), Value::Object(object)) = (&page.next_cursor, &mut listed) {
            let mut input = place(cx.call());
            input.insert("cursor".into(), cursor.as_str().into());
            if let Some(limit) = self.limit {
                input.insert("limit".into(), limit.into());
            }
            object.insert(
                "next".into(),
                json!([Call::new(Self::NAME, Value::Object(input))]),
            );
        }
        Ok(listed)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeploymentShow {
    /// The Deployment's number. Omit for the latest.
    #[serde(default)]
    #[schemars(range(min = 1))]
    deployment: Option<u64>,
    /// Wait up to this long for the outcome.
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentShow {
    const NAME: &'static str = "deployment_show";
    const DESCRIPTION: &'static str = "One Deployment, the latest unless you name its number: status, each Service's outcome, the step and server that failed, and a short log excerpt with secrets replaced by their names. Can wait for it to end.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "deployment": 12, "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let query = find(cx, scope, self.deployment)?;
        let mut view = cx.one(&query)?;
        let mut attempt = 0;
        while view.deployment.in_flight {
            let Some(answers) = cx.poll(self.wait_seconds, attempt, vec![store(&query)?])? else {
                break;
            };
            view = cx.view::<NumberedDeploymentQuery>(answers.into_iter().next())?;
            attempt += 1;
        }
        let mut shown = show(&view);
        let Value::Object(object) = &mut shown else {
            return Ok(shown);
        };
        if view.deployment.in_flight {
            object.insert("progress".into(), progress(&view));
            object.insert(
                "next".into(),
                json!([follow(cx.call(), view.deployment.number)]),
            );
        } else if let Some(calls) = diagnose(&view, cx.call()) {
            object.insert("next".into(), json!(calls));
        }
        Ok(shown)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeploymentCancel {
    /// The Deployment's number. Omit for the latest.
    #[serde(default)]
    #[schemars(range(min = 1))]
    deployment: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentCancel {
    const NAME: &'static str = "deployment_cancel";
    const DESCRIPTION: &'static str =
        "Cancel a queued or running Deployment. What already changed on servers stays.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let query = find(cx, scope, self.deployment)?;
        let view = cx.one(&query)?;
        if !view.deployment.in_flight {
            return Err(Refusal::conflict("That Deployment already ended").into());
        }
        let cancelled = cx.write(&Cancel {
            deployment: view.deployment.id.clone(),
        })?;
        Ok(json!({
            "at": at(&view.environment),
            "deployment": cancelled.number,
            "status": cancelled.status,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeploymentRetry {
    /// The Deployment's number. Omit for the latest.
    #[serde(default)]
    #[schemars(range(min = 1))]
    deployment: Option<u64>,
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

impl Tool for DeploymentRetry {
    const NAME: &'static str = "deployment_retry";
    const DESCRIPTION: &'static str = "Ship a failed, unknown or cancelled Deployment again as a new one; changes saved since wait for the next deploy.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "deployment": 12, "request_key": "r8Tq2wLm5XcV" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let id: DeploymentId = cx.mint(self.request_key)?;
        let number = match self.deployment {
            Some(number) => number,
            // A replay must not resolve the latest again: by now that is the retry itself.
            None => {
                let asks = vec![
                    store(&DeploymentQuery { id: id.clone() })?,
                    store(&newest(&scope))?,
                ];
                let mut answers = cx.read(asks)?.into_iter();
                let replay = answers.next();
                if !matches!(&replay, Some(Err(error)) if error.code == RpcErrorCode::NotFound) {
                    let recorded = cx.view::<DeploymentQuery>(replay)?;
                    let environment = &recorded.environment;
                    return Ok(retried(cx, environment, &recorded.deployment, None));
                }
                latest(cx.view::<DeploymentsQuery>(answers.next())?)?
            }
        };
        let query = NumberedDeploymentQuery {
            environment: scope,
            number,
        };
        let source = cx.one(&query)?;
        let retry = Admit::Retry(Retry {
            id,
            deployment: source.deployment.id.clone(),
        });
        let summary = cx.write(&retry)?;
        let retries = Some(source.deployment.number);
        Ok(retried(cx, &source.environment, &summary, retries))
    }
}

fn retried(
    cx: &Cx<'_>,
    environment: &EnvironmentSummary,
    retried: &DeploymentSummary,
    retries: Option<u64>,
) -> Value {
    json!({
        "at": at(environment),
        "deployment": retried.number,
        "retries": retries,
        "status": retried.status,
        "saved_revision": retried.saved.0,
        "next": follow(cx.call(), retried.number),
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeploymentStart {
    /// The Deployment's number. Omit for the latest.
    #[serde(default)]
    #[schemars(range(min = 1))]
    deployment: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentStart {
    const NAME: &'static str = "deployment_start";
    const DESCRIPTION: &'static str = "Hand a queued Deployment to a runner now.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let query = find(cx, scope, self.deployment)?;
        let queued = cx.one(&query)?;
        let started = cx.write(&Start {
            deployment: queued.deployment.id.clone(),
        })?;
        Ok(json!({
            "at": at(&queued.environment),
            "deployment": started.number,
            "status": started.status,
            "next": follow(cx.call(), started.number),
        }))
    }
}
