use super::*;
use ipnet::Ipv4Net;

#[derive(Debug, Clone)]
pub struct MeshNodeStatus {
    pub machine_id: MachineId,
    pub boot_id: String,
    pub phase: Phase,
    pub ready: bool,
    pub draining: bool,
    pub subnet_claim: Option<Ipv4Net>,
    pub slot_count: usize,
}

impl Mesh {
    pub async fn node_status(&self) -> MeshNodeStatus {
        let readiness = self.ready_status().await;
        let (subnet_claim, draining) = match self.authoritative_self.as_ref() {
            Some(handle) => {
                let record = handle.read().await;
                (record.subnet, record.drain)
            }
            None => (None, false),
        };

        MeshNodeStatus {
            machine_id: self.machine_id.clone(),
            boot_id: self.boot_id.clone(),
            phase: readiness.phase,
            ready: readiness.ready,
            draining,
            subnet_claim,
            slot_count: 0,
        }
    }
}
