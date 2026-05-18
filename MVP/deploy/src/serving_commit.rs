use mvp_bus::{BusActorHandle, BusSession};
use mvp_routing::{ServingCommitFacts, ServingCommitPlan};

use crate::{DeployError, DeployResult};

pub async fn write_serving_commit(
    bus: &BusActorHandle,
    session: &BusSession,
    commit: &ServingCommitPlan,
) -> DeployResult<ServingCommitFacts> {
    mvp_routing::write_serving_commit(bus, session, commit)
        .await
        .map_err(DeployError::from)
}
