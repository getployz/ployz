use crate::daemon::DaemonState;
use ployz_api::{CoordOp, DaemonResponse, ResourceKey as ApiResourceKey};
use ployz_orchestrator::coordination::{Reservation, ReservationId, ResourceKey, Vote};
use ployz_store_api::MachineRegistry;
use ployz_types::time::now_unix_secs;

impl DaemonState {
    pub(crate) async fn handle_coord(&self, op: CoordOp) -> DaemonResponse {
        match op {
            CoordOp::Prepare {
                id,
                key,
                owner,
                nonce,
                ttl_secs,
            } => {
                let key = into_resource_key(key);
                let now = now_unix_secs();
                let expires_at = now.saturating_add(ttl_secs);
                let committed_taken = match self.is_committed_taken(&key, now).await {
                    Ok(value) => value,
                    Err(err) => return self.err("COORDINATION_FAILED", err),
                };
                let vote = self
                    .reservations
                    .prepare(
                        Reservation {
                            id: ReservationId(id),
                            key,
                            owner,
                            nonce,
                            expires_at,
                        },
                        committed_taken,
                        now,
                    )
                    .await;
                coord_vote_response(self, vote)
            }
            CoordOp::Renew {
                key,
                nonce,
                ttl_secs,
                ..
            } => coord_vote_response(
                self,
                self.reservations
                    .renew(
                        &into_resource_key(key),
                        &nonce,
                        now_unix_secs().saturating_add(ttl_secs),
                        now_unix_secs(),
                    )
                    .await,
            ),
            CoordOp::Release { key, nonce, .. } => coord_vote_response(
                self,
                self.reservations
                    .release(&into_resource_key(key), &nonce, now_unix_secs())
                    .await,
            ),
            // Subnet coordination only uses prepare/release as a quorum reservation guard.
            // The durable commit is the target machine's eventual MachineMembership write.
            CoordOp::Commit { .. } => self.ok("commit acknowledged"),
        }
    }

    async fn is_committed_taken(&self, key: &ResourceKey, _now: u64) -> Result<bool, String> {
        let Some(active) = self.active.as_ref() else {
            return Ok(false);
        };
        match key {
            ResourceKey::Subnet(subnet) => {
                let machines = active
                    .mesh
                    .store
                    .list_machines()
                    .await
                    .map_err(|err| format!("list machines: {err}"))?;
                Ok(machines
                    .iter()
                    .any(|machine| machine.subnet == Some(*subnet)))
            }
            ResourceKey::DeployNamespace(_)
            | ResourceKey::CertIssuance(_)
            | ResourceKey::AcmeAccount(_) => Ok(false),
        }
    }
}

fn coord_vote_response(state: &DaemonState, vote: Vote) -> DaemonResponse {
    match vote {
        Vote::Allow => state.ok("allow"),
        Vote::Deny(conflict) => state.err("COORDINATION_DENIED", format!("{conflict:?}")),
    }
}

fn into_resource_key(key: ApiResourceKey) -> ResourceKey {
    match key {
        ApiResourceKey::Subnet(subnet) => ResourceKey::Subnet(subnet),
        ApiResourceKey::DeployNamespace(namespace) => ResourceKey::DeployNamespace(namespace),
        ApiResourceKey::CertIssuance(hostname) => ResourceKey::CertIssuance(hostname),
        ApiResourceKey::AcmeAccount(issuer_url) => ResourceKey::AcmeAccount(issuer_url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{ActiveMesh, DaemonState};
    use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use ployz_orchestrator::Mesh;
    use ployz_orchestrator::mesh::driver::WireguardDriver;
    use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
    use ployz_runtime_api::Identity;
    use ployz_store_api::StoreDriver;
    use ployz_store_api::memory::{MemoryService, MemoryStore};
    use ployz_types::model::{MachineId, PublicKey};
    use std::sync::Arc;

    #[tokio::test]
    async fn coord_prepare_same_nonce_replays_idempotently() {
        let state = make_state("founder").await;
        let request = CoordOp::Prepare {
            id: "reservation-1".into(),
            key: ApiResourceKey::Subnet("10.210.2.0/24".parse().expect("valid subnet")),
            owner: MachineId("joiner".into()),
            nonce: "nonce-1".into(),
            ttl_secs: 30,
        };

        let first = state.handle_coord(request.clone()).await;
        assert!(first.ok, "{}", first.message);

        let replay = state.handle_coord(request).await;
        assert!(replay.ok, "{}", replay.message);
    }

    #[tokio::test]
    async fn coord_prepare_denies_when_committed_subnet_exists() {
        let state = make_state("founder").await;
        let active = state.active.as_ref().expect("active mesh");
        active
            .mesh
            .store
            .upsert_self_machine(&ployz_types::model::MachineMembership::seed(
                MachineId("peer-1".into()),
                PublicKey([2; 32]),
                "fd00::2"
                    .parse()
                    .map(ployz_types::model::OverlayIp)
                    .expect("valid overlay"),
                Some("10.210.2.0/24".parse().expect("valid subnet")),
                vec!["127.0.0.1:51820".into()],
            ))
            .await
            .expect("upsert peer");

        let response = state
            .handle_coord(CoordOp::Prepare {
                id: "reservation-1".into(),
                key: ApiResourceKey::Subnet("10.210.2.0/24".parse().expect("valid subnet")),
                owner: MachineId("joiner".into()),
                nonce: "nonce-1".into(),
                ttl_secs: 30,
            })
            .await;
        assert!(!response.ok);
        assert_eq!(response.code, "COORDINATION_DENIED");
    }

    async fn make_state(machine_id: &str) -> DaemonState {
        let identity = Identity::generate(MachineId(machine_id.into()), [1; 32]);
        let subnet = "10.210.0.0/24".parse().expect("valid subnet");
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            subnet,
        );
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let mesh = Mesh::new(
            WireguardDriver::memory_with(Arc::new(MemoryWireGuard::new())),
            StoreDriver::memory_with(store, service),
            None,
            identity.machine_id.clone(),
            51820,
        );
        let mut state = DaemonState::new_for_tests(
            &std::env::temp_dir().join(format!("ployz-coord-{machine_id}")),
            identity,
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            None,
            1,
        );
        let cached_subnet = config.subnet;
        state.active = Some(ActiveMesh {
            config,
            cached_subnet,
            mesh,
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_seed_cache: None,
        });
        state
    }
}
