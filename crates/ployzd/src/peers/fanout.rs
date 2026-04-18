use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ployz_api::NodeStatusPayload;
use ployz_sdk::{DaemonClient, TcpTransport};
use ployz_types::model::MachineId;
use tokio::task::JoinSet;

use crate::coordination::fanout::FanOutTarget;

#[derive(Debug, Clone)]
pub(crate) enum NodeStatusResult {
    Ok(NodeStatusPayload),
    Offline,
    InvalidIdentity { reported: MachineId },
}

#[derive(Debug, Clone)]
pub(crate) struct NodeStatusFanoutItem {
    pub(crate) expected: MachineId,
    pub(crate) result: NodeStatusResult,
}

fn client(overlay_ip: ployz_types::model::OverlayIp, rpc_port: u16) -> DaemonClient<TcpTransport> {
    let addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), rpc_port);
    DaemonClient::new(TcpTransport::new(addr))
}

pub(crate) async fn fanout_node_status(
    targets: &[FanOutTarget],
    rpc_port: u16,
    deadline: Duration,
) -> Vec<NodeStatusFanoutItem> {
    if targets.is_empty() {
        return Vec::new();
    }

    let mut set: JoinSet<(MachineId, Result<NodeStatusPayload, ()>)> = JoinSet::new();
    for target in targets {
        let expected = target.machine_id.clone();
        let c = client(target.overlay_ip, rpc_port);
        set.spawn(async move {
            match tokio::time::timeout(deadline, c.node_status()).await {
                Ok(Ok(payload)) => (expected, Ok(payload)),
                Ok(Err(_io_err)) => (expected, Err(())),
                Err(_timeout) => (expected, Err(())),
            }
        });
    }

    let mut items = Vec::with_capacity(targets.len());
    while let Some(join_result) = set.join_next().await {
        let Ok((expected, outcome)) = join_result else {
            continue;
        };
        let result = match outcome {
            Ok(payload) => {
                if payload.machine_id == expected.0 {
                    NodeStatusResult::Ok(payload)
                } else {
                    NodeStatusResult::InvalidIdentity {
                        reported: MachineId(payload.machine_id),
                    }
                }
            }
            Err(()) => NodeStatusResult::Offline,
        };
        items.push(NodeStatusFanoutItem { expected, result });
    }
    items
}
