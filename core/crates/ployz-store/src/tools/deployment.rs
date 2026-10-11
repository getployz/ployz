//! Tools on Deployments: follow one to its end, cancel one.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::release::place;
use crate::catalog::NODE_NAME;
use crate::{
    Cancel, DeploymentView, DeploymentsQuery, EnvironmentName, EnvironmentRef, NodeOutcome,
    NumberedDeploymentQuery, Outcome, ProjectName, RowState,
};

/// The Deployment `number` names, or the latest when it names none.
fn find(
    cx: &mut Cx<'_>,
    environment: EnvironmentRef,
    number: Option<u64>,
) -> Result<NumberedDeploymentQuery, Stop> {
    let number = match number {
        Some(number) => number,
        None => {
            let page = cx.one(&DeploymentsQuery {
                environment: environment.clone(),
                limit: Some(1),
                cursor: None,
            })?;
            page.deployments
                .first()
                .map(|latest| latest.number)
                .ok_or_else(|| {
                    Stop::from(
                        Refusal::not_found("This Environment has no Deployment yet")
                            .with_next(Call::new("deploy", json!({}))),
                    )
                })?
        }
    };
    Ok(NumberedDeploymentQuery {
        environment,
        number,
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
        object.insert("log".into(), json!(log));
    }
    shown
}

fn show(view: &DeploymentView) -> Value {
    let deployment = &view.deployment;
    json!({
        "at": at(&view.environment),
        "deployment": deployment.number,
        "status": deployment.status,
        "in_flight": deployment.in_flight,
        "reason": reason(deployment.outcome.as_ref()),
        "message": deployment.message,
        "saved_revision": deployment.saved.0,
        "started_at": deployment.started_at,
        "ended_at": deployment.ended_at,
        "nodes": view.nodes.iter().map(node).collect::<Vec<_>>(),
    })
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
        if let (true, Value::Object(object)) = (view.deployment.in_flight, &mut shown) {
            let mut input = place(cx.call());
            input.insert("deployment".into(), view.deployment.number.into());
            input.insert("wait_seconds".into(), 120.into());
            object.insert(
                "next".into(),
                json!(Call::new(Self::NAME, Value::Object(input))),
            );
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
