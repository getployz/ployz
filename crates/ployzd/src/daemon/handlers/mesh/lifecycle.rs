use crate::mesh_state::network::NetworkConfig;
use ployz_api::{DaemonRequest, MachineSelfTransition};
use ployz_nats::{NodeCommandSubject, RpcPolicy};
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_store_api::MachineMembershipStore;
use ployz_types::model::MachineMembership;
use ployz_types::model::{
    MachineId, MachineLifecycle, MachineLifecycleGoal, MachineLifecycleTransition,
    MachineTransitionEvidence, NetworkId, NetworkLifecycle, NetworkLifecycleGoal,
    NetworkLifecycleTransition, NetworkName, NetworkTransitionEvidence, StandbyTransitionClearance,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{error, warn};

use super::DaemonState;
use crate::daemon::ActiveMesh;

struct MeshControlHandles {
    nats_control: Box<dyn ployz_runtime_api::RuntimeHandle>,
}

const MESH_DESTRUCTIVE_RPC_TIMEOUT: Duration = Duration::from_secs(120);

impl DaemonState {
    pub(crate) fn handle_mesh_create(&self, network: &str) -> ployz_api::DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        self.ok(format!(
            "created network '{}'\n  overlay: {}\n  lifecycle: {}",
            net_config.name, net_config.overlay_ip, net_config.lifecycle
        ))
    }

    pub(crate) async fn handle_mesh_init(&mut self, network: &str) -> ployz_api::DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh stop` first",
                    active.config.name
                ),
            );
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        match self.start_network_transition(net_config, true).await {
            Ok(message) => self.ok(message),
            Err(error) => self.err("NETWORK_START_FAILED", error),
        }
    }

    pub(crate) fn create_network_config(&self, network: &str) -> Result<NetworkConfig, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return Err(format!(
                "network '{network}' already exists -- use `mesh start {network}` or `mesh destroy {network}`"
            ));
        }

        let cluster: ipnet::Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|e| format!("invalid cluster CIDR '{}': {e}", self.cluster_cidr))?;
        let subnet = pick_candidate_subnet(
            cluster,
            self.subnet_prefix_len,
            &std::collections::HashSet::new(),
            0,
        )
        .ok_or_else(|| "no available subnets in cluster CIDR".to_string())?;

        let net_config = NetworkConfig::new(
            NetworkName(network.into()),
            &self.identity.public_key,
            &self.cluster_cidr,
            subnet,
        );

        net_config
            .save(&config_path)
            .map_err(|e| format!("failed to save network config: {e}"))?;

        Ok(net_config)
    }

    pub(crate) async fn handle_mesh_start(&mut self, network: &str) -> ployz_api::DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh stop` first",
                    active.config.name
                ),
            );
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!(
                    "network '{network}' does not exist -- run `mesh create {network}` or `mesh init {network}`"
                ),
            );
        }

        let net_config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                return self.err("IO_ERROR", format!("failed to load network config: {e}"));
            }
        };
        if net_config.lifecycle != NetworkLifecycle::Stopped {
            return self.err(
                "INVALID_TRANSITION",
                format!(
                    "network '{}' is {} -- expected stopped before start",
                    net_config.name, net_config.lifecycle
                ),
            );
        }

        match self.start_network_transition(net_config, false).await {
            Ok(message) => self.ok(message),
            Err(error) => self.err("NETWORK_START_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_stop(&mut self, force: bool) -> ployz_api::DaemonResponse {
        let (network_name, overlay_ip, subnet_to_persist, current_lifecycle, previous_self_record) = {
            let Some(active) = self.active.as_ref() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            let Some(self_record) = active.mesh.authoritative_self_record().await else {
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            (
                active.config.name.0.clone(),
                active.config.overlay_ip,
                active.subnet_to_persist_after_stop(),
                self_record.lifecycle,
                self_record,
            )
        };

        if !force && current_lifecycle != MachineLifecycle::Draining {
            return self.err(
                "INVALID_TRANSITION",
                "mesh stop requires the local machine to be draining; rerun with --force to bypass",
            );
        }

        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        active.stop_certificate_renewal().await;
        active.stop_bootstrap_peer_seed().await;
        if let Err(error) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh stop failed: {error}"));
        }
        if let Err(error) = persist_stopped_self_record(&mut active, &previous_self_record).await {
            warn!(%error, "failed to persist standby self record after mesh stop");
        }
        let mut persisted = active.config.clone();
        let persisted_network_name = persisted.name.clone();
        let _ = persisted
            .lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Stop,
                evidence: NetworkTransitionEvidence::MeshTeardown {
                    network: persisted_network_name,
                },
                at_unix_secs: ployz_types::time::now_unix_secs(),
            });
        persisted.subnet = subnet_to_persist;
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        if let Err(error) = persisted.save(&config_path) {
            return self.err(
                "IO_ERROR",
                format!("failed to persist stopped network config: {error}"),
            );
        }

        let _ = active.nats_control.shutdown().await;
        if let Err(error) = active.dns.shutdown().await {
            warn!(?error, "dns stop failed during mesh stop");
        }
        let gateway_error = active.gateway.shutdown().await.err();
        if let Err(error) = active.image_receiver.shutdown().await {
            warn!(?error, "image receiver stop failed during mesh stop");
        }
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, "image receive session cleanup failed during mesh stop");
        }
        if let Err(error) = active.zfs_transfer.shutdown().await {
            warn!(?error, "zfs transfer listener stop failed during mesh stop");
        }
        if let Some(error) = gateway_error {
            return self.err(
                "NETWORK_STOP_FAILED",
                format!("gateway stop failed: {error}"),
            );
        }

        self.ok(format!(
            "mesh '{}' stopped\n  overlay: {}\n  lifecycle: stopped",
            network_name, overlay_ip
        ))
    }

    pub(crate) async fn handle_mesh_destroy(&mut self, network: &str) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            let config_path = NetworkConfig::path(&self.data_dir, network);
            if !config_path.exists() {
                return self.err(
                    "NETWORK_NOT_FOUND",
                    format!("network '{network}' does not exist"),
                );
            }
            if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
                return self.err("IO_ERROR", format!("failed to delete network config: {e}"));
            }
            return self.ok(format!("mesh '{network}' destroyed"));
        };

        if active.config.name.0 != network {
            return self.err(
                "NETWORK_NOT_RUNNING",
                format!(
                    "network '{network}' is not running; active network is '{}'",
                    active.config.name
                ),
            );
        }

        let rpc_client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let network_id = active.config.id.clone();
        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "LIST_FAILED",
                    format!("failed to list machines before destroy: {error}"),
                );
            }
        };
        let operation_id = format!("mesh-destroy-{}", NetworkId::random());
        let expected_machine_ids = sorted_machine_ids(&machines);
        let peers = machines
            .iter()
            .filter(|machine| machine.id != self.identity.machine_id)
            .cloned()
            .collect::<Vec<_>>();

        let mut prepared = Vec::new();
        let mut failures = Vec::new();
        for peer in &peers {
            let response = rpc_client
                .request(
                    NodeCommandSubject::mesh_prepare_destroy(&peer.id),
                    &DaemonRequest::MeshPeerPrepareDestroy {
                        operation_id: operation_id.clone(),
                        network_id: network_id.clone(),
                        coordinator_id: self.identity.machine_id.clone(),
                        expected_machine_ids: expected_machine_ids.clone(),
                    },
                )
                .await;
            match response {
                Ok(response) if response.ok => prepared.push(peer.clone()),
                Ok(response) => failures.push(format!(
                    "{} rejected prepare [{}]: {}",
                    peer.id, response.code, response.message
                )),
                Err(error) => failures.push(format!("{} unreachable: {error}", peer.id)),
            }
        }

        if !failures.is_empty() {
            for peer in &prepared {
                if let Err(error) = rpc_client
                    .request_expect_ok(
                        NodeCommandSubject::mesh_cancel_destroy(&peer.id),
                        &DaemonRequest::MeshPeerCancelDestroy {
                            operation_id: operation_id.clone(),
                        },
                    )
                    .await
                {
                    warn!(peer = %peer.id, %operation_id, error = %error, "mesh destroy cancel failed");
                }
            }
            return self.err(
                "MESH_DESTROY_PEERS_UNREACHABLE",
                format!(
                    "mesh destroy refused; all registered peers must confirm teardown first: {}",
                    failures.join("; ")
                ),
            );
        }

        let execute_client = rpc_client.with_policy(RpcPolicy {
            timeout: MESH_DESTRUCTIVE_RPC_TIMEOUT,
        });
        let mut execute_failures = Vec::new();
        for peer in &prepared {
            if let Err(error) = execute_client
                .request_expect_ok(
                    NodeCommandSubject::mesh_execute_destroy(&peer.id),
                    &DaemonRequest::MeshPeerExecuteDestroy {
                        operation_id: operation_id.clone(),
                        network_id: network_id.clone(),
                    },
                )
                .await
            {
                execute_failures.push(format!("{} execute failed: {error}", peer.id));
                warn!(peer = %peer.id, %operation_id, error = %error, "mesh destroy execute failed");
            }
        }
        if !execute_failures.is_empty() {
            return self.err(
                "MESH_DESTROY_PEER_EXECUTE_FAILED",
                format!(
                    "peer execute failed after prepare succeeded: {}",
                    execute_failures.join("; ")
                ),
            );
        }

        match self.destroy_local_mesh_runtime(&network_id).await {
            Ok(()) => self.ok(format!("mesh '{network}' destroyed")),
            Err(error) => self.err("NETWORK_DESTROY_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_prepare_destroy(
        &self,
        operation_id: &str,
        network_id: &NetworkId,
        coordinator_id: &MachineId,
        expected_machine_ids: &[MachineId],
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        if active.config.id != *network_id {
            return self.err(
                "NETWORK_MISMATCH",
                format!(
                    "destroy operation '{operation_id}' targets network '{}', local network is '{}'",
                    network_id, active.config.id
                ),
            );
        }

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "LIST_FAILED",
                    format!("failed to list machines before prepare: {error}"),
                );
            }
        };
        let local_ids = sorted_machine_ids(&machines);
        let mut expected = expected_machine_ids.to_vec();
        expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if local_ids != expected {
            return self.err(
                "MACHINE_SET_MISMATCH",
                format!(
                    "destroy operation '{operation_id}' from '{coordinator_id}' has a different machine set"
                ),
            );
        }

        self.ok(format!(
            "mesh destroy operation '{operation_id}' prepared on '{}'",
            self.identity.machine_id
        ))
    }

    pub(crate) async fn handle_mesh_peer_cancel_destroy(
        &self,
        operation_id: &str,
    ) -> ployz_api::DaemonResponse {
        self.ok(format!(
            "mesh destroy operation '{operation_id}' cancelled on '{}'",
            self.identity.machine_id
        ))
    }

    pub(crate) async fn handle_mesh_peer_execute_destroy(
        &mut self,
        operation_id: &str,
        network_id: &NetworkId,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> ployz_api::DaemonResponse {
        match self.take_active_for_destroy(network_id) {
            Ok(active) => {
                self.revoke_image_sessions_for_teardown("mesh destroy")
                    .await;
                let data_dir = self.data_dir.clone();
                let machine_id = self.identity.machine_id.clone();
                let operation_id_for_log = operation_id.to_string();
                let network_id_for_log = network_id.clone();
                Self::spawn_teardown_after_response(
                    response_flushed,
                    async move { Self::perform_mesh_teardown(data_dir, active).await },
                    move |error| {
                        error!(
                            operation_id = %operation_id_for_log,
                            network_id = %network_id_for_log,
                            %machine_id,
                            %error,
                            "mesh destroy teardown failed after peer ack"
                        );
                    },
                );
                self.ok(format!(
                    "mesh destroy operation '{operation_id}' executed on '{}'",
                    self.identity.machine_id
                ))
            }
            Err(error) => self.err("NETWORK_DESTROY_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_remove_machine(
        &mut self,
        operation_id: &str,
        network_id: &NetworkId,
        machine_id: &MachineId,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> ployz_api::DaemonResponse {
        if *machine_id != self.identity.machine_id {
            return self.err(
                "MACHINE_MISMATCH",
                format!(
                    "remove operation '{operation_id}' targets machine '{machine_id}', local machine is '{}'",
                    self.identity.machine_id
                ),
            );
        }

        match self.take_active_for_destroy(network_id) {
            Ok(active) => {
                self.revoke_image_sessions_for_teardown("machine remove")
                    .await;
                let data_dir = self.data_dir.clone();
                let local_machine_id = self.identity.machine_id.clone();
                let operation_id_for_log = operation_id.to_string();
                let network_id_for_log = network_id.clone();
                Self::spawn_teardown_after_response(
                    response_flushed,
                    async move { Self::perform_mesh_teardown(data_dir, active).await },
                    move |error| {
                        error!(
                            operation_id = %operation_id_for_log,
                            network_id = %network_id_for_log,
                            machine_id = %local_machine_id,
                            %error,
                            "machine remove teardown failed after peer ack"
                        );
                    },
                );
                self.ok(format!(
                    "machine remove operation '{operation_id}' executed on '{}'",
                    self.identity.machine_id
                ))
            }
            Err(error) => self.err("MACHINE_REMOVE_FAILED", error),
        }
    }

    async fn destroy_local_mesh_runtime(&mut self, network_id: &NetworkId) -> Result<(), String> {
        let active = self.take_active_for_destroy(network_id)?;
        self.revoke_image_sessions_for_teardown("mesh destroy")
            .await;
        Self::perform_mesh_teardown(self.data_dir.clone(), active).await
    }

    async fn revoke_image_sessions_for_teardown(&self, operation: &'static str) {
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, operation, "image receive session cleanup failed during mesh teardown");
        }
    }

    fn take_active_for_destroy(&mut self, network_id: &NetworkId) -> Result<ActiveMesh, String> {
        let active_id = {
            let Some(active) = self.active.as_ref() else {
                return Err("no mesh running".to_string());
            };
            if active.config.id != *network_id {
                return Err(format!(
                    "operation targets network '{}', local network is '{}'",
                    network_id, active.config.id
                ));
            }
            active.config.id.clone()
        };

        let Some(active) = self.active.take() else {
            return Err("no mesh running".to_string());
        };
        if active.config.id != active_id {
            self.active = Some(active);
            return Err("active network changed during destroy".to_string());
        }
        Ok(active)
    }

    async fn perform_mesh_teardown(data_dir: PathBuf, active: ActiveMesh) -> Result<(), String> {
        let (result, control_handles) =
            Self::perform_mesh_teardown_before_control_shutdown(&data_dir, active).await;
        let _ = control_handles.nats_control.shutdown().await;
        result
    }

    async fn perform_mesh_teardown_before_control_shutdown(
        data_dir: &Path,
        active: ActiveMesh,
    ) -> (Result<(), String>, MeshControlHandles) {
        let ActiveMesh {
            config,
            mut mesh,
            nats_control,
            zfs_transfer,
            image_receiver,
            gateway,
            dns,
            mut certificate_renewal,
            mut bootstrap_peer_seed,
            ..
        } = active;
        let network_name = config.name.0;
        let control_handles = MeshControlHandles { nats_control };

        let result = async move {
            if let Some(task) = certificate_renewal.take() {
                task.shutdown().await;
            }
            if let Some(task) = bootstrap_peer_seed.take() {
                task.shutdown().await;
            }
            if let Err(error) = mesh.destroy_and_wipe_store_data().await {
                return Err(format!("mesh runtime destroy and wipe failed: {error}"));
            }
            if let Err(error) = dns.shutdown().await {
                warn!(?error, "dns stop failed during mesh destroy");
            }
            let gateway_error = gateway.shutdown().await.err();
            if let Err(error) = image_receiver.shutdown().await {
                warn!(?error, "image receiver stop failed during mesh destroy");
            }
            if let Err(error) = zfs_transfer.shutdown().await {
                warn!(
                    ?error,
                    "zfs transfer listener stop failed during mesh destroy"
                );
            }
            if let Some(error) = gateway_error {
                return Err(format!("gateway stop failed: {error}"));
            }

            NetworkConfig::delete(data_dir, &network_name)
                .map_err(|error| format!("failed to delete network config: {error}"))
        }
        .await;

        (result, control_handles)
    }

    fn spawn_teardown_after_response<T, F>(
        response_flushed: Option<oneshot::Receiver<()>>,
        teardown: T,
        log_error: F,
    ) where
        T: std::future::Future<Output = Result<(), String>> + Send + 'static,
        F: FnOnce(String) + Send + 'static,
    {
        tokio::spawn(async move {
            if let Some(response_flushed) = response_flushed {
                let _ = response_flushed.await;
            }
            if let Err(error) = teardown.await {
                log_error(error);
            }
        });
    }

    async fn start_network_transition(
        &mut self,
        net_config: NetworkConfig,
        initialized: bool,
    ) -> Result<String, String> {
        let Some(assigned_subnet) = net_config.subnet else {
            return Err(format!(
                "network '{}' has no local subnet assignment to activate",
                net_config.name
            ));
        };

        let mut running_config = net_config.clone();
        running_config
            .lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Start,
                evidence: NetworkTransitionEvidence::OperatorCommand {
                    command: if initialized {
                        "mesh init".into()
                    } else {
                        "mesh start".into()
                    },
                },
                at_unix_secs: ployz_types::time::now_unix_secs(),
            })
            .map_err(|error| error.message().to_string())?;
        let network_name = running_config.name.clone();
        let overlay_ip = running_config.overlay_ip;
        self.start_mesh(running_config.clone())
            .await
            .map_err(|error| error.to_string())?;

        if let Err(error) = self
            .transition_local_machine(MachineSelfTransition::Activate { assigned_subnet })
            .await
        {
            self.stop_started_mesh_after_transition_failure().await;
            return Err(error.message);
        }

        let config_path = NetworkConfig::path(&self.data_dir, &network_name.0);
        if let Some(active) = self.active.as_mut() {
            let active_command = if initialized {
                "mesh init".into()
            } else {
                "mesh start".into()
            };
            active
                .config
                .lifecycle
                .apply_transition(NetworkLifecycleTransition {
                    goal: NetworkLifecycleGoal::Start,
                    evidence: NetworkTransitionEvidence::OperatorCommand {
                        command: active_command,
                    },
                    at_unix_secs: ployz_types::time::now_unix_secs(),
                })
                .map_err(|error| error.message().to_string())?;
            if let Err(error) = active.config.save(&config_path) {
                self.stop_started_mesh_after_transition_failure().await;
                return Err(format!("save network config: {error}"));
            }
            active.retained_subnet =
                crate::daemon::RetainedSubnet::from_running_config(active.config.subnet);
        }

        let verb = if initialized {
            "initialized and started"
        } else {
            "started"
        };
        Ok(format!(
            "mesh '{}' {}\n  overlay: {}\n  lifecycle: running",
            network_name, verb, overlay_ip
        ))
    }

    pub(super) async fn stop_started_mesh_after_transition_failure(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.stop_certificate_renewal().await;
        active.stop_bootstrap_peer_seed().await;
        if let Err(error) = active.mesh.destroy().await {
            warn!(?error, "failed to stop mesh after transition error");
        }
        let _ = active.nats_control.shutdown().await;
        if let Err(error) = active.dns.shutdown().await {
            warn!(?error, "failed to stop dns after transition error");
        }
        if let Err(error) = active.gateway.shutdown().await {
            warn!(?error, "failed to stop gateway after transition error");
        }
        if let Err(error) = active.image_receiver.shutdown().await {
            warn!(
                ?error,
                "failed to stop image receiver after transition error"
            );
        }
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, "image receive session cleanup failed after transition error");
        }
        if let Err(error) = active.zfs_transfer.shutdown().await {
            warn!(
                ?error,
                "failed to stop zfs transfer listener after transition error"
            );
        }
    }
}

fn sorted_machine_ids(machines: &[MachineMembership]) -> Vec<MachineId> {
    let mut ids = machines
        .iter()
        .map(|machine| machine.id.clone())
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    ids
}

async fn persist_stopped_self_record(
    active: &mut ActiveMesh,
    previous_self_record: &MachineMembership,
) -> Result<(), String> {
    let mut standby = previous_self_record.clone();
    standby
        .apply_lifecycle_transition(MachineLifecycleTransition {
            goal: MachineLifecycleGoal::Standby {
                clearance: StandbyTransitionClearance::OperatorForced,
            },
            evidence: MachineTransitionEvidence::MeshStop {
                network: active.config.name.clone(),
            },
            at_unix_secs: ployz_types::time::now_unix_secs(),
        })
        .map_err(|err| format!("build standby self record: {err}"))?;

    if let Err(error) = active.mesh.store.upsert_self_machine(&standby).await {
        return Err(format!("persist standby self record in store: {error}"));
    }

    if active
        .mesh
        .update_authoritative_self_record(|record| {
            *record = standby.clone();
        })
        .await
        .is_none()
    {
        return Err("update in-memory self record after standby persistence".to_string());
    }

    Ok(())
}
