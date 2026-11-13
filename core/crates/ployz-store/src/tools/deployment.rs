//! Tools on Deployments: list them, follow one to its end, retry, start or
//! cancel one.

use std::collections::BTreeMap;

use ployz_core::{MachineId, RpcErrorCode};
use serde_json::{Map, Value, json};

use super::ToolId;
use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool, store};
use super::refusal::{Call, Refusal};
use super::release::version_for_loss;
use super::step::Gate;
use crate::{
    Admit, Cancel, DeployedNode, DeploymentId, DeploymentQuery, DeploymentSummary, DeploymentView,
    DeploymentWarning, DeploymentsQuery, EnvironmentRef, EnvironmentSummary, NodeOutcome,
    NumberedDeploymentQuery, Outcome, Retry, RowState, Start, VolumeName,
};

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
fn diagnose(view: &DeploymentView, cx: &Cx<'_>) -> Option<[Call; 2]> {
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
    let mut input = cx.place_args();
    input.insert("service".into(), json!(service));
    input.insert("deployment".into(), view.deployment.number.into());
    let input = Value::Object(input);
    Some([
        Call::to(ToolId::ServiceLogs, input.clone()),
        Call::to(ToolId::ServiceMetrics, input),
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
        object.insert("at".into(), json!(view.environment.to_string()));
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

/// How long a call that follows a Deployment waits for its outcome.
pub(crate) const FOLLOW_WAIT_SECONDS: u32 = 120;

/// The `deployment_show` call that follows a Deployment in `place`: the one `number`
/// names, or the latest when none.
pub(crate) fn follow_call(mut place: Map<String, Value>, number: Option<u64>) -> Call {
    if let Some(number) = number {
        place.insert("deployment".into(), number.into());
    }
    place.insert("wait_seconds".into(), FOLLOW_WAIT_SECONDS.into());
    Call::to(ToolId::DeploymentShow, Value::Object(place))
}

tool_input! {
    pub(crate) struct DeploymentList {
        #[serde(default)]
        #[schemars(range(min = 1, max = 100))]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
        @place(here),
    }
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
        let scope = cx.scope(&self);
        let page = cx.one(&DeploymentsQuery {
            environment: scope,
            limit: self.limit,
            cursor: self.cursor,
        })?;
        let mut listed = json!({
            "at": page.environment.to_string(),
            "deployments": page.deployments.iter().map(summary).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        });
        if let (Some(cursor), Value::Object(object)) = (&page.next_cursor, &mut listed) {
            let mut input = cx.place_args();
            input.insert("cursor".into(), cursor.as_str().into());
            if let Some(limit) = self.limit {
                input.insert("limit".into(), limit.into());
            }
            object.insert(
                "next".into(),
                json!([Call::to(ToolId::DeploymentList, Value::Object(input))]),
            );
        }
        Ok(listed)
    }
}

tool_input! {
    pub(crate) struct DeploymentShow {
        /// The Deployment's number. Omit for the latest.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @wait["Wait up to this long for the outcome."],
        @place(named),
    }
}

impl Tool for DeploymentShow {
    const NAME: &'static str = "deployment_show";
    const DESCRIPTION: &'static str = "One Deployment, the latest unless you name its number: status, each Service's outcome, variables left out or Config-breaking, server warnings, the step and server that failed, and a short log excerpt with secrets replaced by their names. Can wait for it to end.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "deployment": 12, "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let (query, view) = find(cx, scope, self.deployment)?;
        let view = cx.follow(
            self.wait_seconds,
            view,
            |view| view.deployment.in_flight.then(|| store(&query)).transpose(),
            |cx, _, answer| cx.view::<NumberedDeploymentQuery>(answer),
        )?;
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
                        .map(|number| follow_call(cx.place_args(), Some(number)))
                        .into_iter()
                        .collect::<Vec<_>>()
                ),
            );
        } else if let Some(calls) = diagnose(&view, cx) {
            object.insert("next".into(), json!(calls));
        }
        Ok(shown)
    }
}

tool_input! {
    pub(crate) struct DeploymentCancel {
        /// The Deployment's number. Omit for the latest.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @place(named),
    }
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
        let scope = cx.scope(&self);
        let (_, view) = find(cx, scope, self.deployment)?;
        if !view.deployment.in_flight {
            return Err(Refusal::conflict("That Deployment already ended").into());
        }
        let cancelled = cx.write(&Cancel {
            deployment: view.deployment.id.clone(),
        })?;
        Ok(json!({
            "at": view.environment.to_string(),
            "deployment": cancelled.number,
            "status": cancelled.status,
        }))
    }
}

tool_input! {
    pub(crate) struct DeploymentRetry {
        /// The Deployment's number. Omit for the latest.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @key,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        #[schemars(with = "Vec<String>")]
        accept_volume_loss: Vec<VolumeName>,
        @place(named),
    }
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
        let scope = cx.scope(&self);
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
            teardown: None,
            id,
            deployment: source.deployment.id.clone(),
            rolls_back: None,
            version: self.version,
            accept_volume_loss: self.accept_volume_loss,
            approvals: Vec::new(),
            awaits_approval: false,
            approval: None,
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
        "at": environment.to_string(),
        "deployment": retried.number,
        "retries": retries,
        "rolls_back": rolls_back,
        "status": retried.status,
        "saved_revision": retried.saved.0,
        "next": retried.number.map(|number| follow_call(cx.place_args(), Some(number))),
    })
}

tool_input! {
    pub(crate) struct DeploymentStart {
        /// The Deployment's number. Omit for the latest.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @place(here),
    }
}

impl Tool for DeploymentStart {
    const NAME: &'static str = "deployment_start";
    const DESCRIPTION: &'static str = "Hand a queued Deployment to a runner now.";
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "deployment": 12 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let (_, queued) = find(cx, scope, self.deployment)?;
        let started = cx.write(&Start {
            deployment: queued.deployment.id.clone(),
        })?;
        Ok(json!({
            "at": queued.environment.to_string(),
            "deployment": started.number,
            "status": started.status,
            "next": started.number.map(|number| follow_call(cx.place_args(), Some(number))),
        }))
    }
}
