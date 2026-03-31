use std::net::Ipv4Addr;
use std::sync::Arc;

use ployz_runtime_api::{
    DeploySession, DeploySessionFactory, Result, RuntimeError, StartCandidateRequest,
};
use ployz_store_api::{DeployReadStore, DeployWriteStore};
use ployz_types::model::{DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineRecord};
use ployz_types::spec::Namespace;

use super::locks::NamespaceLockManager;
use super::remote::{DeployAgent, DeployStoreHandle, SessionState, TcpDeploySession};

pub struct InProcessDeploySession {
    agent: Arc<DeployAgent>,
    state: SessionState,
    machine_id: MachineId,
}

#[async_trait::async_trait]
impl DeploySession for InProcessDeploySession {
    fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>> {
        self.agent
            .inspect_namespace(&self.state)
            .await
            .map_err(RuntimeError::from)
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
            )
            .await
            .map_err(RuntimeError::from)
    }

    async fn drain_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.agent
            .drain_instance(&self.state, instance_id)
            .await
            .map_err(RuntimeError::from)
    }

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.agent
            .remove_instance(&self.state, instance_id)
            .await
            .map_err(RuntimeError::from)
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

pub struct DefaultDeploySessionFactory {
    agent: Arc<DeployAgent>,
    local_machine_id: MachineId,
    remote_control_port: u16,
}

impl DefaultDeploySessionFactory {
    #[must_use]
    pub(crate) fn new(
        agent: Arc<DeployAgent>,
        local_machine_id: MachineId,
        remote_control_port: u16,
    ) -> Self {
        Self {
            agent,
            local_machine_id,
            remote_control_port,
        }
    }

    #[must_use]
    pub fn for_local_machine(
        deploy_read: Arc<dyn DeployReadStore>,
        deploy_write: Arc<dyn DeployWriteStore>,
        namespace_locks: NamespaceLockManager,
        local_machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
        remote_control_port: u16,
    ) -> Self {
        let agent = Arc::new(DeployAgent::new(
            DeployStoreHandle::new(deploy_read, deploy_write),
            namespace_locks,
            local_machine_id.clone(),
            overlay_network_name,
            overlay_dns_server,
        ));
        Self::new(agent, local_machine_id, remote_control_port)
    }
}

#[async_trait::async_trait]
impl DeploySessionFactory for DefaultDeploySessionFactory {
    async fn open(
        &self,
        machine: &MachineRecord,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
    ) -> Result<(Box<dyn DeploySession>, Vec<InstanceStatusRecord>)> {
        if machine.id == self.local_machine_id {
            let (state, instances) = self.agent.open_session(namespace, deploy_id).await?;
            let session = InProcessDeploySession {
                agent: Arc::clone(&self.agent),
                state,
                machine_id: machine.id.clone(),
            };
            Ok((Box::new(session), instances))
        } else {
            let (session, instances) = TcpDeploySession::connect(
                machine,
                self.remote_control_port,
                namespace,
                deploy_id,
                coordinator_id,
            )
            .await?;
            Ok((Box::new(session), instances))
        }
    }
}
