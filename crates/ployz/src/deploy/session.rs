use std::sync::Arc;

use crate::error::Result;
use crate::model::{DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineRecord, SlotId};
use crate::spec::Namespace;

use super::remote::{DeployAgent, SessionState, TcpDeploySession};

/// Factory for opening deploy sessions to machines.
///
/// Each `open()` call acquires a namespace lock on the target machine and returns
/// a snapshot of its current instances. The lock is held until the session is
/// closed or dropped.
// TODO: remove async_trait when RPITIT is sufficient for dyn dispatch
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

/// A deploy session to a single machine for a single namespace.
///
/// The namespace lock is held for the lifetime of the session.
/// Methods take `&mut self` — sessions are not shared.
// TODO: remove async_trait when RPITIT is sufficient for dyn dispatch
#[async_trait::async_trait]
pub trait DeploySession: Send {
    fn machine_id(&self) -> &MachineId;

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>>;

    async fn start_candidate(
        &mut self,
        req: StartCandidateRequest,
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
    pub deploy_id: DeployId,
    pub spec_json: String,
}

// ---------------------------------------------------------------------------
// InProcessDeploySession — local participant
// ---------------------------------------------------------------------------

/// Deploy session that runs in-process against the local DeployAgent.
/// Lock is held via SessionState until the session is dropped.
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
                &req.deploy_id,
                &req.spec_json,
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
        // Lock released on drop via SessionState._lock.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DefaultDeploySessionFactory
// ---------------------------------------------------------------------------

/// Factory that creates in-process sessions for the local machine and
/// TCP sessions for remote machines.
pub struct DefaultDeploySessionFactory {
    agent: Arc<DeployAgent>,
    local_machine_id: MachineId,
    remote_control_port: u16,
}

impl DefaultDeploySessionFactory {
    #[must_use]
    pub fn new(
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
}

// TODO: remove async_trait when RPITIT is sufficient for dyn dispatch
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
