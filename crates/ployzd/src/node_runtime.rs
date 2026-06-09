//! Runtime composition for node-local services.

use crate::node_agent::runtime::{NodeContainerRunner, NodeLogReader};
use crate::node_service_runtime::{
    NodeServiceRuntimeError, NodeWireGuardEbpfPreparer, start_node_runtime_service,
};
use ployz_core::ids::NodeId;
use ployz_nats::service_runtime::{NatsClient, RunningNatsService};
use std::fmt;

pub type RunningNodeRuntime = RunningNatsService;

pub async fn start_node_runtime_with_ports<R, P, L>(
    client: NatsClient,
    node_id: NodeId,
    runner: R,
    wireguard_ebpf: P,
    log_reader: L,
) -> Result<RunningNodeRuntime, NodeRuntimeStartError>
where
    R: Clone + NodeContainerRunner + Send + Sync + 'static,
    P: Clone + NodeWireGuardEbpfPreparer + Send + Sync + 'static,
    L: Clone + NodeLogReader + Send + Sync + 'static,
{
    start_node_runtime_service(client, node_id, runner, wireguard_ebpf, log_reader)
        .await
        .map_err(NodeRuntimeStartError::StartService)
}

#[derive(Debug)]
pub enum NodeRuntimeStartError {
    StartService(NodeServiceRuntimeError),
}

impl fmt::Display for NodeRuntimeStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartService(error) => {
                write!(formatter, "failed to start node service: {error:?}")
            }
        }
    }
}

impl std::error::Error for NodeRuntimeStartError {}
