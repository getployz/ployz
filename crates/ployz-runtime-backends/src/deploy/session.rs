use std::sync::Arc;

use crate::error::Result;
use crate::model::{InstanceId, InstanceStatusRecord, MachineId};
pub use ployz_orchestrator::deploy::session::{
    DeploySession, DeploySessionFactory, StartCandidateRequest,
};

use super::remote::{DeployAgent, SessionState};

// ---------------------------------------------------------------------------
// InProcessDeploySession — local participant
// ---------------------------------------------------------------------------

/// Deploy session that runs in-process against the local DeployAgent.
pub struct InProcessDeploySession {
    agent: Arc<DeployAgent>,
    state: SessionState,
    machine_id: MachineId,
}

// TODO: remove async_trait when RPITIT is sufficient for dyn dispatch
#[async_trait::async_trait]
impl DeploySession for InProcessDeploySession {
    fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>> {
        self.agent.inspect_namespace(&self.state).await
    }

    async fn start_candidate(
        &mut self,
        req: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        self.agent
            .start_candidate(
                &self.state,
                &req.service,
                &req.slot_id,
                &req.instance_id,
                self.state.deploy_id(),
                &req.spec_json,
                &req.volumes_json,
            )
            .await
    }

    async fn drain_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.agent.drain_instance(&self.state, instance_id).await
    }

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.agent.remove_instance(&self.state, instance_id).await
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}
