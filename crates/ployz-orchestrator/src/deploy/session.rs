use crate::error::Result;
use crate::model::{DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineRecord, SlotId};
use ployz_types::spec::Namespace;

#[async_trait::async_trait]
pub trait DeploySessionFactory: Send + Sync {
    async fn open(
        &self,
        machine: &MachineRecord,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
    ) -> Result<(Box<dyn DeploySession>, Vec<InstanceStatusRecord>)>;
}

#[async_trait::async_trait]
pub trait DeploySession: Send {
    fn machine_id(&self) -> &MachineId;

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>>;

    async fn start_candidate(
        &mut self,
        request: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord>;

    async fn drain_instance(&mut self, instance_id: &InstanceId) -> Result<()>;

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()>;

    async fn close(self: Box<Self>) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct StartCandidateRequest {
    pub service: String,
    pub slot_id: SlotId,
    pub instance_id: InstanceId,
    pub spec_json: String,
}
