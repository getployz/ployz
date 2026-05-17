use mvp_bus::{BusActorHandle, BusSession, FactKey, FactWriteOutcome};
use mvp_projection::{ProjectionFactPayload, ServingCommitFact};

use crate::wire::encode;
use crate::{DeployError, DeployResult, ServingCommitPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommitFacts {
    pub serving: FactWriteOutcome,
}

pub async fn write_serving_commit(
    bus: &BusActorHandle,
    session: &BusSession,
    commit: &ServingCommitPlan,
) -> DeployResult<ServingCommitFacts> {
    let serving = write_projection_fact(
        bus,
        session,
        &format!("/facts/serving/{}", commit.serving_commit_id),
        ProjectionFactPayload::ServingCommit(ServingCommitFact {
            serving_commit_id: commit.serving_commit_id.to_string(),
            route_commit_id: commit.route_commit_id.to_string(),
            gateway_commit_id: commit.gateway_commit_id.to_string(),
            dns_commit_id: commit.dns_commit_id.to_string(),
            route_id: commit.route_id.clone(),
            hostnames: commit.hostnames.clone(),
            backends: commit.active_backends.clone(),
            old_backends_to_drain: commit.old_backends_to_drain.clone(),
            dns_records: commit.dns_records.clone(),
            epoch: commit.epoch,
        }),
    )
    .await?;
    Ok(ServingCommitFacts { serving })
}

async fn write_projection_fact(
    bus: &BusActorHandle,
    session: &BusSession,
    key: &str,
    payload: ProjectionFactPayload,
) -> DeployResult<FactWriteOutcome> {
    let key = FactKey::parse(key)?;
    let payload = encode(&payload, "projection fact")?;
    let outcome = bus
        .write_fact_payload(session, key.clone(), payload)
        .await
        .map_err(DeployError::from)?;
    match outcome {
        FactWriteOutcome::Inserted(_) | FactWriteOutcome::AlreadyPresent(_) => Ok(outcome),
        FactWriteOutcome::Conflict(_) => Err(DeployError::ServingFactConflict { key }),
    }
}
