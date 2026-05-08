use std::time::Duration;

use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineId;

use crate::NatsStore;
use crate::subjects::{self, NatsScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeCommandPlane {
    Authority,
    Substrate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCommandSubject {
    machine_id: MachineId,
    command: &'static str,
    plane: NodeCommandPlane,
}

impl NodeCommandSubject {
    #[must_use]
    pub fn deploy_start_candidate(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.start_candidate")
    }

    #[must_use]
    pub fn deploy_stop(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.stop")
    }

    #[must_use]
    pub fn deploy_inspect_namespace(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.inspect_namespace")
    }

    #[must_use]
    pub fn deploy_drain_instance(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.drain_instance")
    }

    #[must_use]
    pub fn deploy_remove_instance(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.remove_instance")
    }

    #[must_use]
    pub fn volume_ensure(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.ensure")
    }

    #[must_use]
    pub fn volume_remove(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.remove")
    }

    #[must_use]
    pub fn ping(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "ping")
    }

    #[must_use]
    pub fn status(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "status")
    }

    #[must_use]
    pub fn mesh_self_record(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.self_record")
    }

    #[must_use]
    pub fn mesh_ready(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.ready")
    }

    #[must_use]
    pub fn mesh_prepare_destroy(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.prepare_destroy")
    }

    #[must_use]
    pub fn mesh_cancel_destroy(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.cancel_destroy")
    }

    #[must_use]
    pub fn mesh_execute_destroy(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.execute_destroy")
    }

    #[must_use]
    pub fn mesh_remove_machine(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "mesh.remove_machine")
    }

    #[must_use]
    pub fn machine_transition_self(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "machine.transition_self")
    }

    #[must_use]
    pub fn machine_storage_promote_self(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "machine.storage.promote_self")
    }

    #[must_use]
    pub fn machine_update_prepare(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "machine.update.prepare")
    }

    #[must_use]
    pub fn machine_update_execute(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "machine.update.execute")
    }

    #[must_use]
    pub fn machine_operation_get(machine_id: &MachineId) -> Self {
        Self::new_substrate(machine_id, "machine.operation.get")
    }

    #[must_use]
    pub fn volume_zfs_inspect(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.zfs.inspect")
    }

    #[must_use]
    pub fn volume_zfs_snapshot(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.zfs.snapshot")
    }

    #[must_use]
    pub fn volume_zfs_snapshot_guid(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.zfs.snapshot_guid")
    }

    #[must_use]
    pub fn volume_zfs_start_send(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.zfs.start_send")
    }

    #[must_use]
    pub(crate) fn subject_in(&self, scope: &NatsScope) -> String {
        match self.plane {
            NodeCommandPlane::Authority => {
                subjects::authority_node_command_in(scope, &self.machine_id, self.command)
            }
            NodeCommandPlane::Substrate => {
                subjects::substrate_node_command_in(scope, &self.machine_id, self.command)
            }
        }
    }

    fn new(machine_id: &MachineId, command: &'static str) -> Self {
        Self {
            machine_id: machine_id.clone(),
            command,
            plane: NodeCommandPlane::Authority,
        }
    }

    fn new_substrate(machine_id: &MachineId, command: &'static str) -> Self {
        Self {
            machine_id: machine_id.clone(),
            command,
            plane: NodeCommandPlane::Substrate,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcPolicy {
    pub timeout: Duration,
}

impl Default for RpcPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcFailureKind {
    Timeout,
    NoResponders,
    Transport,
    Encode,
    Decode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcFailure {
    pub kind: RpcFailureKind,
    pub message: String,
}

impl RpcFailure {
    #[must_use]
    pub fn new(kind: RpcFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for RpcFailure {}

impl RpcFailure {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self.kind {
            RpcFailureKind::Timeout => "NATS_RPC_TIMEOUT",
            RpcFailureKind::NoResponders => "NATS_RPC_NO_RESPONDERS",
            RpcFailureKind::Transport => "NATS_RPC_TRANSPORT",
            RpcFailureKind::Encode => "NATS_RPC_ENCODE",
            RpcFailureKind::Decode => "NATS_RPC_DECODE",
        }
    }
}

#[derive(Clone)]
pub struct NatsNodeRpcClient {
    client: async_nats::Client,
    scope: NatsScope,
    policy: RpcPolicy,
}

impl NatsNodeRpcClient {
    #[must_use]
    pub fn for_store(store: &NatsStore) -> Self {
        Self::new(store.client().clone(), store.scope().clone())
    }

    #[must_use]
    pub(crate) fn new(client: async_nats::Client, scope: NatsScope) -> Self {
        Self {
            client,
            scope,
            policy: RpcPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: RpcPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub async fn request(
        &self,
        subject: NodeCommandSubject,
        request: &DaemonRequest,
    ) -> std::result::Result<DaemonResponse, RpcFailure> {
        let subject_name = subject.subject_in(&self.scope);
        let payload = serde_json::to_vec(request).map_err(|error| {
            RpcFailure::new(
                RpcFailureKind::Encode,
                format!("encode node rpc request: {error}"),
            )
        })?;
        let response = tokio::time::timeout(
            self.policy.timeout,
            self.client.request(subject_name.clone(), payload.into()),
        )
        .await
        .map_err(|_| {
            RpcFailure::new(
                RpcFailureKind::Timeout,
                format!("request to '{subject_name}' timed out"),
            )
        })?
        .map_err(classify_request_error)?;
        serde_json::from_slice(response.payload.as_ref()).map_err(|error| {
            RpcFailure::new(
                RpcFailureKind::Decode,
                format!("decode node rpc response from '{subject_name}': {error}"),
            )
        })
    }

    pub async fn request_expect_ok(
        &self,
        subject: NodeCommandSubject,
        request: &DaemonRequest,
    ) -> std::result::Result<(), RpcFailure> {
        let response = self.request(subject, request).await?;
        if response.ok {
            return Ok(());
        }
        Err(RpcFailure::new(
            RpcFailureKind::Transport,
            format!(
                "remote daemon error [{}]: {}",
                response.code, response.message
            ),
        ))
    }
}

#[must_use]
pub(crate) fn classify_request_error(error: async_nats::RequestError) -> RpcFailure {
    match error.kind() {
        async_nats::RequestErrorKind::TimedOut => {
            RpcFailure::new(RpcFailureKind::Timeout, error.to_string())
        }
        async_nats::RequestErrorKind::NoResponders => {
            RpcFailure::new(RpcFailureKind::NoResponders, error.to_string())
        }
        async_nats::RequestErrorKind::Other => {
            RpcFailure::new(RpcFailureKind::Transport, error.to_string())
        }
        async_nats::RequestErrorKind::InvalidSubject => {
            RpcFailure::new(RpcFailureKind::Transport, error.to_string())
        }
    }
}

impl From<RpcFailure> for Error {
    fn from(error: RpcFailure) -> Self {
        Error::operation(error.code(), error.message)
    }
}

pub fn decode_daemon_request(payload: &[u8]) -> Result<DaemonRequest> {
    serde_json::from_slice(payload)
        .map_err(|error| Error::operation("nats_rpc_decode_request", error.to_string()))
}

pub fn encode_daemon_response(response: &DaemonResponse) -> Result<Vec<u8>> {
    serde_json::to_vec(response)
        .map_err(|error| Error::operation("nats_rpc_encode_response", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_command_subject_uses_substrate_for_whole_node_commands() {
        let machine_id = MachineId("machine.a".into());
        let scope = NatsScope::local_default();
        let subject = NodeCommandSubject::status(&machine_id);

        assert_eq!(
            subject.subject_in(&scope),
            "ployz.v1.local.substrate.rpc.node.machine%2Ea.status"
        );
        assert_eq!(
            subjects::node_command_queue_group(&machine_id),
            "ployzd-node-machine%2Ea"
        );
    }

    #[test]
    fn mesh_command_subjects_name_operation() {
        let machine_id = MachineId("machine-a".into());
        let scope = NatsScope::local_default();

        assert_eq!(
            NodeCommandSubject::mesh_prepare_destroy(&machine_id).subject_in(&scope),
            "ployz.v1.local.substrate.rpc.node.machine-a.mesh.prepare_destroy"
        );
        assert_eq!(
            NodeCommandSubject::mesh_execute_destroy(&machine_id).subject_in(&scope),
            "ployz.v1.local.substrate.rpc.node.machine-a.mesh.execute_destroy"
        );
        assert_eq!(
            NodeCommandSubject::mesh_ready(&machine_id).subject_in(&scope),
            "ployz.v1.local.substrate.rpc.node.machine-a.mesh.ready"
        );
    }

    #[test]
    fn deploy_command_subjects_remain_authority_scoped() {
        let machine_id = MachineId("machine-a".into());
        let scope = NatsScope::local_default();

        assert_eq!(
            NodeCommandSubject::deploy_start_candidate(&machine_id).subject_in(&scope),
            "ployz.v1.local.auth-default.rpc.node.machine-a.deploy.start_candidate"
        );
    }

    #[test]
    fn rpc_failure_codes_are_stable() {
        let failure = RpcFailure::new(RpcFailureKind::NoResponders, "none");

        assert_eq!(failure.code(), "NATS_RPC_NO_RESPONDERS");
    }
}
