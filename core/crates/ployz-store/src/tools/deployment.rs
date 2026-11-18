//! Tools on Deployments: list them, follow one to its end, retry, start or
//! cancel one.

use std::collections::BTreeMap;

use ployz_core::{MachineId, RpcErrorCode};
use serde_json::{Map, Value, json};

use super::ToolId;
use super::input::tool_input;
use super::machine::{Cx, DiffVerb, Effect, Stop, Tool, store};
use super::refusal::{Call, Refusal};
use super::step::{Gate, version_for_loss};
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
    if let Value::Object(object) = &mut summary {
        if deployment.preparing {
            object.insert("preparing".into(), true.into());
        }
        say_waiting(object, deployment);
    }
    summary
}

/// Add to `shown` the Services `deployment` left waiting: their names, and what
/// each waits on under `untrusted`, since the reason names what a Server reported.
/// `all_waiting` marks one that deployed or removed no Service.
pub(super) fn say_waiting(shown: &mut Map<String, Value>, deployment: &DeploymentSummary) {
    let Some(Outcome::Executed { waiting, .. }) = &deployment.outcome else {
        return;
    };
    if waiting.is_empty() {
        return;
    }
    let names: Vec<_> = waiting.iter().map(|waiting| &waiting.service).collect();
    shown.insert("waiting".into(), json!(names));
    if deployment.all_waiting {
        shown.insert("all_waiting".into(), true.into());
    }
    let untrusted = shown.entry("untrusted").or_insert_with(|| json!({}));
    if let Value::Object(untrusted) = untrusted {
        untrusted.insert("waiting".into(), json!(waiting));
    }
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
        /// How many to return (default 20).
        #[serde(default)]
        #[schemars(range(min = 1, max = 100))]
        limit: Option<usize>,
        /// The `next_cursor` from the previous page, to read the next one.
        #[serde(default)]
        cursor: Option<String>,
        @place(here),
    }
}

impl Tool for DeploymentList {
    const NAME: &'static str = "deployment_list";
    const TITLE: &'static str = "List Deployments";
    const DESCRIPTION: &'static str = "List an Environment's Deployments, newest to oldest. Each shows its number, status, whether it is still in flight, why it ended, what waits, its message, Saved revision, and start and end times.";
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
        /// Deployment number, e.g. `12`. Leave out for the latest numbered one.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @wait["Seconds to wait for a Deployment still in flight to end, e.g. `120`. By default it answers at once."],
        @place(named),
    }
}

impl Tool for DeploymentShow {
    const NAME: &'static str = "deployment_show";
    const TITLE: &'static str = "Show a Deployment";
    const DESCRIPTION: &'static str = "Show one Deployment, the latest unless you give a number: its status, each node's outcome, and warnings such as variables left out. `waiting` names each Service left waiting and why; `all_waiting` means nothing deployed. A failure comes with its Server, step, reason and a short scrubbed log excerpt. One still in flight also shows its progress, and the call can wait for it to end.";
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
        /// Deployment number, e.g. `12`. Leave out for the latest numbered one.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @place(named),
    }
}

impl Tool for DeploymentCancel {
    const NAME: &'static str = "deployment_cancel";
    const TITLE: &'static str = "Cancel a Deployment";
    const DESCRIPTION: &'static str = "Cancel a queued or running Deployment, the latest unless you give a number. A queued one never runs. A running one stops, but what it already changed on Servers stays. Fails if it already ended.";
    // Sent again, one still stopping comes back unchanged and one that ended is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };
    // On Cloud, a committed cancel also cancels the GitHub Actions runs the
    // Deployment's builds were dispatched to, through GitHub's API.
    const OPEN_WORLD: bool = true;

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
        /// Deployment number, e.g. `12`. Leave out for the latest numbered one.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @key,
        /// The version the refusal gave. Fails if what it covers changed since.
        #[serde(default)]
        version: Option<String>,
        /// Volumes whose data the member agreed to lose, named as the refusal listed them.
        #[serde(default)]
        #[schemars(with = "Vec<String>")]
        accept_volume_loss: Vec<VolumeName>,
        @place(named),
    }
}

impl Tool for DeploymentRetry {
    const NAME: &'static str = "deployment_retry";
    const TITLE: &'static str = "Redeploy an ended Deployment";
    const DESCRIPTION: &'static str = "Redeploy an ended Deployment. A new Deployment ships its Saved revision and pinned commits again, whatever was saved since. Redeploying an older one rolls back the newer ones, and fails until you accept the loss of any Volume data it would delete. Check progress with `deployment_show`.";
    const GATE: Gate = Gate::Store;
    // Without a request_key every call queues another Deployment.
    const EFFECT: Effect = Effect::Remove { idempotent: false };
    // The Deployment it queues builds on GitHub Actions and pulls images from
    // public registries.
    const OPEN_WORLD: bool = true;

    fn verb(_input: &Value) -> DiffVerb {
        DiffVerb::Retry
    }

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
            id,
            deployment: source.deployment.id.clone(),
            rolls_back: None,
            version: self.version,
            accept_volume_loss: self.accept_volume_loss,
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
        /// Deployment number, e.g. `12`. Leave out for the latest numbered one.
        #[serde(default)]
        #[schemars(range(min = 1))]
        deployment: Option<u64>,
        @place(here),
    }
}

impl Tool for DeploymentStart {
    const NAME: &'static str = "deployment_start";
    const TITLE: &'static str = "Start a queued Deployment";
    const DESCRIPTION: &'static str = "Hand a queued Deployment to a runner now, the latest unless you give a number. Nothing stored changes. Fails if it is already running or has ended.";
    // Start stores nothing: sent again it hands over the same queued Deployment,
    // or is refused once that runs.
    const EFFECT: Effect = Effect::Remove { idempotent: true };
    // The Deployment it queues builds on GitHub Actions and pulls images from
    // public registries.
    const OPEN_WORLD: bool = true;

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
