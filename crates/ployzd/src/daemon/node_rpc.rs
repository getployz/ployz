mod deploy;
mod image;
mod machine;
mod mesh;
mod probe;
mod volume_zfs;

use ployz_node_api::NodeResponse;
use ployz_node_runtime::{NodeRpcError, decode_node_response_payload};

pub(crate) use deploy::NatsDeployRpcTransport;
pub(crate) use machine::{
    NatsMachineLifecycleRpcTransport, NatsMachineOperationRpcTransport,
    NatsMachineStorageRpcTransport, NatsMachineUpdateRpcTransport,
};
pub(crate) use mesh::{NatsMeshReadinessRpcTransport, NatsMeshRpcTransport};
pub(crate) use probe::NatsNodeProbeRpcTransport;
pub(crate) use volume_zfs::NatsVolumeZfsRpcTransport;

pub(crate) const STATUS_PAYLOAD_KIND: &str = "status";
pub(crate) const MESH_READY_PAYLOAD_KIND: &str = "mesh-ready";
pub(crate) const MESH_SELF_RECORD_PAYLOAD_KIND: &str = "mesh-self-record";
pub(crate) const MACHINE_OPERATION_PAYLOAD_KIND: &str = "machine-operation";

pub(crate) fn decode_daemon_node_payload<P>(
    operation_name: &'static str,
    response: NodeResponse,
    expected_kind: &'static str,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    decode_node_response_payload(operation_name, response, expected_kind)
}
