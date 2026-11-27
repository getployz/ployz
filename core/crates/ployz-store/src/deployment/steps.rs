//! When each step of a Deployment ran.

use ployz_core::RpcError;
use ployz_core::deploy_steps::{DeploymentStep, StepName};
use serde_json::json;

use crate::error;
use crate::id::DeploymentId;
use crate::storage::Tx;

pub(crate) fn record(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
    steps: &[DeploymentStep],
) -> Result<(), RpcError> {
    for step in steps {
        let service = step.service.as_ref().map_or("", |service| service.as_str());
        let written = tx.execute(
            "INSERT INTO config_deployment_step \
             (deployment_id, name, service, organization_id, started, ended) \
             SELECT id, ?2, ?3, organization_id, ?4, ?5 FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, name, service) DO UPDATE SET \
             started = excluded.started, ended = excluded.ended",
            &[
                deployment.as_str().into(),
                step.name.as_str().into(),
                service.into(),
                step.started_at.into(),
                step.ended_at.into(),
            ],
        )?;
        if written == 0 {
            return Err(error::not_found(
                "That Deployment doesn't exist.",
                json!({ "deployment": deployment }),
            ));
        }
    }
    Ok(())
}

/// Deployment `deployment`'s steps, by when each started, then in Deploy order.
pub(crate) fn of(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
) -> Result<Vec<DeploymentStep>, RpcError> {
    let rows = tx.query(
        "SELECT name, service, started, ended FROM config_deployment_step \
         WHERE deployment_id = ?1",
        &[deployment.as_str().into()],
    )?;
    let mut steps = Vec::with_capacity(rows.len());
    for row in &rows {
        // A step a newer writer named that this reader doesn't know.
        let Some(name) = StepName::parse(row.text(0)?) else {
            continue;
        };
        let service = match row.text(1)? {
            "" => None,
            _ => Some(row.parse(1, "Deployment step")?),
        };
        steps.push(DeploymentStep {
            name,
            service,
            started_at: row.int(2)?,
            ended_at: row.optional_int(3)?,
        });
    }
    steps.sort_by(|a, b| {
        (a.started_at, a.name, &a.service).cmp(&(b.started_at, b.name, &b.service))
    });
    Ok(steps)
}
