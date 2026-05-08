mod bootstrap;
mod lifecycle;
mod participation;
mod status;

use crate::mesh_state::network::NetworkConfig;
use ployz_api::MeshReadyPayload;
use ployz_orchestrator::mesh::orchestrator::MeshReadyStatus;
use ployz_types::model::MachineMembership;
use std::path::Path;

use super::super::DaemonState;

fn restore_network_config_subnet(
    config_path: &Path,
    config: &mut NetworkConfig,
    subnet: Option<ipnet::Ipv4Net>,
) -> Result<(), String> {
    config.subnet = subnet;
    config
        .save(config_path)
        .map_err(|error| format!("restore network config: {error}"))
}

fn mesh_ready_payload(value: MeshReadyStatus, self_record: &MachineMembership) -> MeshReadyPayload {
    MeshReadyPayload {
        ready: value.ready,
        phase: value.phase.to_string(),
        store_healthy: value.store_healthy,
        sync_connected: value.sync_connected,
        workload_subnet_present: self_record.subnet.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::mesh_state::invite::issue_invite_token;
    use crate::mesh_state::network::NetworkConfig;
    use ployz_api::{MachineTransitionGoal, MeshBootstrapRequest};
    use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
    use ployz_orchestrator::{Mesh, WireguardDriver};
    use ployz_runtime_api::Identity;
    use ployz_store_api::MachineMembershipStore;
    use ployz_store_api::StoreDriver;
    use ployz_store_api::memory::{MemoryService, MemoryStore};
    use ployz_types::model::{MachineId, MachineLifecycle, MachineTopology};
    use ployz_types::time::now_unix_secs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn mesh_join_is_unsupported_without_machine_add() {
        let founder_identity =
            Identity::generate(ployz_types::model::MachineId("founder".into()), [7; 32]);
        let joiner_identity =
            Identity::generate(ployz_types::model::MachineId("joiner".into()), [8; 32]);
        let founder_subnet: ipnet::Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let network = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &founder_identity.public_key,
            "10.210.0.0/16",
            founder_subnet,
        );

        let (token, _) = issue_invite_token(
            &founder_identity,
            &network,
            "invite-1".into(),
            600,
            now_unix_secs(),
            Vec::new(),
            Some(network.overlay_ip.0.to_string()),
            Some("wg-public".into()),
            Vec::new(),
        )
        .expect("issue invite");

        let data_dir = unique_temp_dir("ployz-mesh-join");
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            joiner_identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );

        let response = state.handle_mesh_join(&token).await;
        assert!(!response.ok);
        assert_eq!(response.code, "UNSUPPORTED");
    }

    #[tokio::test]
    async fn started_mesh_cleanup_removes_active_after_transition_failure() {
        let (mut state, _, network) = make_active_state().await;
        assert!(network.is_up(), "mesh should start up for cleanup test");

        state.stop_started_mesh_after_transition_failure().await;

        assert!(state.active.is_none(), "cleanup should clear active mesh");
        assert!(!network.is_up(), "cleanup should tear down the runtime");
    }

    #[tokio::test]
    async fn local_transition_fails_without_authoritative_self_record() {
        let identity = Identity::generate(MachineId("founder".into()), [11; 32]);
        let machine_id = identity.machine_id.clone();
        let data_dir = unique_temp_dir("ployz-startup-participation-fail");
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );

        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );
        let retained_subnet = crate::daemon::RetainedSubnet::from_running_config(config.subnet);
        state.active = Some(ActiveMesh {
            config,
            retained_subnet,
            mesh: Mesh::new(
                WireguardDriver::memory(),
                StoreDriver::memory(),
                None,
                machine_id,
                51820,
            ),
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });

        let error = state
            .transition_local_machine(
                MachineTransitionGoal::Activate,
                Some("10.210.0.0/24".parse().expect("valid subnet")),
                false,
            )
            .await
            .expect_err("missing self record should fail");
        assert_eq!(error.code, "SELF_RECORD_MISSING");
    }

    #[tokio::test]
    async fn mesh_bootstrap_refuses_to_overwrite_existing_network_config() {
        let identity = Identity::generate(MachineId("joiner".into()), [9; 32]);
        let data_dir = unique_temp_dir("ployz-bootstrap-guard");
        let existing = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.1.0/24".parse().expect("valid subnet"),
        );
        let config_path = NetworkConfig::path(&data_dir, "alpha");
        existing.save(&config_path).expect("save existing config");

        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );

        let response = state
            .handle_mesh_bootstrap(&MeshBootstrapRequest {
                network_id: ployz_types::model::NetworkId("net-new".into()),
                network_name: "alpha".into(),
                cluster_cidr: "10.210.0.0/16".into(),
                assigned_subnet: "10.210.2.0/24".parse().expect("valid subnet"),
                bootstrap_peers: Vec::new(),
            })
            .await;
        assert!(!response.ok);
        assert_eq!(response.code, "NETWORK_ALREADY_EXISTS");

        let persisted = NetworkConfig::load(&config_path).expect("load existing config");
        assert_eq!(persisted.id, existing.id);
        assert_eq!(persisted.subnet, existing.subnet);
    }

    #[test]
    fn restore_network_config_subnet_restores_previous_value() {
        let identity = Identity::generate(MachineId("joiner".into()), [10; 32]);
        let data_dir = unique_temp_dir("ployz-promote-rollback");
        let config_path = NetworkConfig::path(&data_dir, "alpha");
        let previous_subnet: ipnet::Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        let mut config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            previous_subnet,
        );
        config.save(&config_path).expect("save initial config");

        config.subnet = Some("10.210.2.0/24".parse().expect("valid subnet"));
        config.save(&config_path).expect("save promoted config");

        restore_network_config_subnet(&config_path, &mut config, Some(previous_subnet))
            .expect("restore subnet");

        let persisted = NetworkConfig::load(&config_path).expect("load restored config");
        assert_eq!(persisted.subnet, Some(previous_subnet));
    }

    async fn make_active_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&ployz_types::model::MachineMembership {
                id: identity.machine_id.clone(),
                public_key: identity.public_key.clone(),
                overlay_ip: config.overlay_ip,
                topology: MachineTopology::local(),
                region_role: ployz_types::model::RegionRole::HomeData,
                subnet: config.subnet,
                bridge_ip: None,
                endpoints: vec!["127.0.0.1:51820".into()],
                lifecycle: MachineLifecycle::Standby,
                storage: true,
                storage_participation: ployz_types::model::StorageParticipation::default_authority(
                ),
                created_at: 0,
                updated_at: 0,
                labels: std::collections::BTreeMap::new(),
            })
            .await
            .expect("seed founder record");

        let network = Arc::new(MemoryWireGuard::new());
        let mut mesh = Mesh::new(
            WireguardDriver::memory_with(network.clone()),
            StoreDriver::memory_with(store.clone(), Arc::new(MemoryService::new())),
            None,
            identity.machine_id.clone(),
            51820,
        );
        mesh.up().await.expect("mesh up");
        let data_dir = unique_temp_dir("ployz-mesh-accept");
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );
        let retained_subnet = crate::daemon::RetainedSubnet::from_running_config(config.subnet);
        state.active = Some(ActiveMesh {
            config,
            retained_subnet,
            mesh,
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
        (state, store, network)
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        path.push(format!("{prefix}-{nanos}"));
        path
    }
}
