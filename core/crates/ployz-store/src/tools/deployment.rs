//! Tools on Deployments: list them, follow one to its end, retry, start or
//! cancel one.

use std::collections::BTreeMap;

use ployz_core::{MachineId, RpcErrorCode};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Effect, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::release::{place, version_for_loss};
use super::step::{Gate, RequestKey};
use crate::catalog::NODE_NAME;
use crate::{
    Admit, Cancel, DeployedNode, DeploymentId, DeploymentQuery, DeploymentSummary, DeploymentView,
    DeploymentWarning, DeploymentsQuery, EnvironmentName, EnvironmentRef, EnvironmentSummary,
    NodeOutcome, NumberedDeploymentQuery, Outcome, ProjectName, Retry, RowState, Start, VolumeName,
};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

/// The Deployment `number` names, or the latest numbered one when it names none,
/// with the query that reads it again by its number.
fn find(
    cx: &mut Cx<'_>,
    environment: EnvironmentRef,
    number: Option<u64>,
) -> Result<(NumberedDeploymentQuery, DeploymentView), Stop> {
    let view = cx.one(&NumberedDeploymentQuery {
        environment: environment.clone(),
        number,
    })?;
    let query = NumberedDeploymentQuery {
        environment,
        number: number.or(view.deployment.number),
    };
    Ok((query, view))
}

fn newest(environment: &EnvironmentRef) -> DeploymentsQuery {
    DeploymentsQuery {
        environment: environment.clone(),
        limit: Some(1),
        cursor: None,
    }
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
            reason,
            step,
            log,
            stopped_by: crate::StoppedBy::NOTHING,
            ..
        } => Some((row.server.as_str(), reason, step, log)),
        RowState::Pending
        | RowState::Running { .. }
        | RowState::Completed
        | RowState::Failed { .. }
        | RowState::NotAttempted
        | RowState::Unknown => None,
    });
    let outcome = if node.cancelled {
        json!("cancelled")
    } else {
        json!(node.outcome)
    };
    let mut shown = json!({ "name": node.node.name(), "outcome": outcome });
    if let (Some((server, reason, step, log)), Value::Object(object)) = (failed, &mut shown) {
        object.insert("server".into(), server.into());
        object.insert("reason".into(), reason.as_str().into());
        object.insert("step".into(), json!(step));
        object.insert("untrusted".into(), json!({ "excerpt": log }));
    }
    shown
}

/// How far a running Deployment got: the phase its first busy row is in, and
/// how many of its Servers finished every row.
fn progress(view: &DeploymentView) -> Value {
    let rows = || view.nodes.iter().flat_map(|node| &node.rows);
    let phase = rows().find_map(|row| match &row.state {
        RowState::Running { phase, .. } => Some(*phase),
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
            if node.rows.iter().any(|row| {
                matches!(
                    row.state,
                    RowState::Failed {
                        stopped_by: crate::StoppedBy::NOTHING,
                        ..
                    }
                )
            }) =>
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
    let mut summary = json!({
        "deployment": deployment.number,
        "status": deployment.status,
        "in_flight": deployment.in_flight,
        "reason": reason(deployment.outcome.as_ref()),
        "untrusted": { "message": deployment.message },
        "saved_revision": deployment.saved.0,
        "started_at": deployment.started_at,
        "ended_at": deployment.ended_at,
    });
    if let (true, Value::Object(object)) = (deployment.preparing, &mut summary) {
        object.insert("preparing".into(), true.into());
    }
    summary
}

/// Whether a warning holds only names the Config Store wrote, such as a variable
/// a Deploy left out, rather than text a Server reported.
fn named(warning: &DeploymentWarning) -> bool {
    match warning {
        DeploymentWarning::Fragile { .. }
        | DeploymentWarning::NotSet { .. }
        | DeploymentWarning::NotSetInFile { .. } => true,
        DeploymentWarning::IngressNotServing { .. }
        | DeploymentWarning::IngressUnchecked { .. }
        | DeploymentWarning::Deploy { .. } => false,
    }
}

fn show(view: &DeploymentView) -> Value {
    let mut shown = summary(&view.deployment);
    if let Value::Object(object) = &mut shown {
        object.insert("at".into(), json!(at(&view.environment)));
        object.insert(
            "nodes".into(),
            json!(view.nodes.iter().map(node).collect::<Vec<_>>()),
        );
        let (stored, reported): (Vec<_>, Vec<_>) = view
            .deployment
            .warnings
            .iter()
            .partition(|warning| named(warning));
        if !stored.is_empty() {
            object.insert("warnings".into(), json!(stored));
        }
        if let (false, Some(Value::Object(untrusted))) =
            (reported.is_empty(), object.get_mut("untrusted"))
        {
            untrusted.insert("warnings".into(), json!(reported));
        }
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
    const EFFECT: Effect = Effect::Read;

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
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentShow {
    const NAME: &'static str = "deployment_show";
    const DESCRIPTION: &'static str = "One Deployment, the latest unless you name its number: status, each Service's outcome, variables left out or Config-breaking, server warnings, the step and server that failed, and a short log excerpt with secrets replaced by their names. Can wait for it to end.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "deployment": 12, "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let (query, mut view) = find(cx, scope, self.deployment)?;
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
                json!(
                    view.deployment
                        .number
                        .map(|number| follow(cx.call(), number))
                        .into_iter()
                        .collect::<Vec<_>>()
                ),
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
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentCancel {
    const NAME: &'static str = "deployment_cancel";
    const DESCRIPTION: &'static str =
        "Cancel a queued or running Deployment. What already changed on servers stays.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let (_, view) = find(cx, scope, self.deployment)?;
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
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    accept_volume_loss: Vec<VolumeName>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DeploymentRetry {
    const NAME: &'static str = "deployment_retry";
    const DESCRIPTION: &'static str = "Ship an ended Deployment's commit and settings again; an older one rolls back newer ones. A human approves every redeploy.";
    const GATE: Gate = Gate::Store;
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "deployment": 12, "request_key": "r8Tq2wLm5XcV" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        version_for_loss(&self.accept_volume_loss, self.version.as_ref())?;
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let id: DeploymentId = cx.mint(self.request_key)?;
        let number = match self.deployment {
            Some(number) => Some(number),
            // A replay must not resolve the latest again: by now that is the retry itself.
            None => {
                let asks = vec![
                    store(&DeploymentQuery { id: id.clone() })?,
                    store(&newest(&scope))?,
                ];
                let mut answers = cx.read(asks)?.into_iter();
                let replay = answers.next();
                let page = cx.view::<DeploymentsQuery>(answers.next())?;
                let recorded = match replay {
                    Some(Err(error)) if error.code == RpcErrorCode::NotFound => None,
                    answer => Some(cx.view::<DeploymentQuery>(answer)?.deployment),
                };
                // The reads run together, so the original can land between them
                // and show up only as the latest; see
                // a_keyed_retry_that_lands_between_its_replays_reads_is_the_replay.
                let recorded = recorded
                    .or_else(|| page.deployments.first().cloned())
                    .filter(|recorded| recorded.id == id)
                    .filter(|recorded| recorded.environment_id == page.environment.id);
                if let Some(recorded) = recorded {
                    return Ok(retried(cx, &page.environment, &recorded, None, &[]));
                }
                None
            }
        };
        let query = NumberedDeploymentQuery {
            environment: scope,
            number,
        };
        let source = cx.one(&query)?;
        // A human approves the exact set it rolls back, so the agent confirms none.
        let retry = Admit::Retry(Retry {
            id,
            deployment: source.deployment.id.clone(),
            rolls_back: None,
            version: self.version,
            accept_volume_loss: self.accept_volume_loss,
        });
        let summary = cx.write(&retry)?;
        let retries = source.deployment.number;
        // On a replay the source's newer runs include this redeploy and any after it.
        let rolls_back: Vec<u64> = source
            .rolls_back
            .iter()
            .copied()
            .filter(|number| summary.number.is_some_and(|retried| *number < retried))
            .collect();
        Ok(retried(
            cx,
            &source.environment,
            &summary,
            retries,
            &rolls_back,
        ))
    }
}

fn retried(
    cx: &Cx<'_>,
    environment: &EnvironmentSummary,
    retried: &DeploymentSummary,
    retries: Option<u64>,
    rolls_back: &[u64],
) -> Value {
    json!({
        "at": at(environment),
        "deployment": retried.number,
        "retries": retries,
        "rolls_back": rolls_back,
        "status": retried.status,
        "saved_revision": retried.saved.0,
        "next": retried.number.map(|number| follow(cx.call(), number)),
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
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let (_, queued) = find(cx, scope, self.deployment)?;
        let started = cx.write(&Start {
            deployment: queued.deployment.id.clone(),
        })?;
        Ok(json!({
            "at": at(&queued.environment),
            "deployment": started.number,
            "status": started.status,
            "next": started.number.map(|number| follow(cx.call(), number)),
        }))
    }
}
