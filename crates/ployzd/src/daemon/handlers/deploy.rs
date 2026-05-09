use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, DeployCandidateStartedPayload, DeployFailurePayload,
    DeployFailureReason, DeployNamespaceSnapshotPayload, DeployOptions, VolumeZfsTransferPayload,
};
use ployz_cert_backends::InstantAcmeIssuerFactory;
use ployz_config::RuntimeTarget;
use ployz_nats::{NatsDeployLock, NatsLocks};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_orchestrator::certificates::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::participant::{
    DeployParticipantClient, MoveVolumeRequest, MoveVolumeResult, StartCandidateRequest,
};
use ployz_orchestrator::deploy::{
    apply_with_deploy_id_and_certificate_coordination, new_deploy_id, preview,
};
use ployz_runtime_backends::deploy::remote::DeployAgent;
use ployz_store_api::{DeployStore, StoreDriver, StoreRuntimeControl};
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::SlotId;
use ployz_types::model::{
    DeployId, DeployPhaseCommitPolicy, DeployPhaseFailure, DeployPhaseState, InstanceId,
    InstanceStatusRecord, MachineId, MachineMembership,
};
use ployz_types::spec::{DeployManifest, Namespace, ServiceSpec, VolumeDeclaration};

const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);
const DEPLOY_PARTICIPANT_RPC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEPLOY_VOLUME_MOVE_START_RPC_TIMEOUT: Duration = Duration::from_secs(60);
const DEPLOY_VOLUME_MOVE_POLL_RPC_TIMEOUT: Duration = Duration::from_secs(60);
const DEPLOY_VOLUME_MOVE_RPC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const DEPLOY_VOLUME_MOVE_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[async_trait::async_trait]
trait DeployMoveRpcClient: Clone + Send + Sync {
    fn with_rpc_policy(&self, policy: RpcPolicy) -> Self;

    async fn request(
        &self,
        subject: NodeCommandSubject,
        request: &ployz_api::DaemonRequest,
    ) -> std::result::Result<DaemonResponse, RpcFailure>;
}

#[async_trait::async_trait]
impl DeployMoveRpcClient for NatsNodeRpcClient {
    fn with_rpc_policy(&self, policy: RpcPolicy) -> Self {
        self.clone().with_policy(policy)
    }

    async fn request(
        &self,
        subject: NodeCommandSubject,
        request: &ployz_api::DaemonRequest,
    ) -> std::result::Result<DaemonResponse, RpcFailure> {
        NatsNodeRpcClient::request(self, subject, request).await
    }
}

impl DaemonState {
    fn overlay_network_name(&self) -> Option<String> {
        self.active
            .as_ref()
            .map(|active| format!("ployz-{}", active.config.name.0))
    }

    fn overlay_dns_server(&self) -> Option<std::net::Ipv4Addr> {
        if self.runtime_target != RuntimeTarget::Docker {
            return None;
        }
        self.active
            .as_ref()
            .and_then(|active| active.mesh.container_dns_server())
    }

    pub async fn handle_deploy_preview(
        &self,
        manifest_json: &str,
        _options: &DeployOptions,
    ) -> DaemonResponse {
        let manifest = match decode_manifest(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return *response,
        };
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return self.err("DEPLOY_PREVIEW_FAILED", error.to_string()),
            };
        if let Err(error) = nats_store.start().await {
            return self.err("DEPLOY_PREVIEW_FAILED", error.to_string());
        }
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&nats_store),
        );

        match preview(
            &active.mesh.store,
            &self.identity.machine_id,
            &manifest,
            &prober,
        )
        .await
        {
            Ok(plan) => self.ok_json_pretty(&plan, "ENCODE_PREVIEW", "encode preview"),
            Err(err) => self.deploy_error_response("DEPLOY_PREVIEW_FAILED", err),
        }
    }

    pub async fn handle_deploy_apply(
        &self,
        manifest_json: &str,
        _options: &DeployOptions,
    ) -> DaemonResponse {
        let manifest = match decode_manifest(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return *response,
        };
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return self.err("DEPLOY_APPLY_FAILED", error.to_string()),
            };
        if let Err(error) = nats_store.start().await {
            return self.err("DEPLOY_APPLY_FAILED", error.to_string());
        }
        let nats_locks = match NatsLocks::new(&nats_store).await {
            Ok(locks) => locks,
            Err(error) => return self.err("DEPLOY_APPLY_FAILED", error.to_string()),
        };
        let deploy_lock = match NatsDeployLock::acquire(
            nats_locks.clone(),
            &manifest.namespace,
            &ReservationId::random().0,
            &self.identity.machine_id,
            DEPLOY_LOCK_TTL,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => return self.err("DEPLOY_LOCK_FAILED", error.to_string()),
        };
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                nats_locks,
                self.identity.machine_id.clone(),
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::NatsChallengeReadiness::new(
                active.mesh.store.clone(),
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&nats_store),
        );
        let participant_client = NatsDeployParticipantClient::new(
            ployz_nats::NatsNodeRpcClient::for_store(&nats_store).with_policy(RpcPolicy {
                timeout: DEPLOY_PARTICIPANT_RPC_TIMEOUT,
            }),
        );

        let deploy_id = new_deploy_id();
        let apply = apply_with_deploy_id_and_certificate_coordination(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            &manifest,
            deploy_id.clone(),
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
        );
        tokio::pin!(apply);
        let mut deploy_lock_renewer = tokio::spawn(renew_deploy_lock(
            deploy_lock.clone(),
            DEPLOY_LOCK_TTL,
            DEPLOY_LOCK_RENEW_INTERVAL,
        ));
        let result = tokio::select! {
            result = &mut apply => result,
            renewal = &mut deploy_lock_renewer => {
                let message = match renewal {
                    Ok(Ok(())) => "deploy lock renewal task exited before apply completed".to_string(),
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => format!("deploy lock renewal task failed: {error}"),
                };
                match mark_deploy_failed_after_lock_loss(&active.mesh.store, &deploy_id, &message).await {
                    DeployLockLossOutcome::PastCommit => {
                        tracing::warn!(
                            %deploy_id,
                            %message,
                            "deploy lock was lost after commit point; waiting for apply to finish"
                        );
                        (&mut apply).await
                    }
                    DeployLockLossOutcome::MarkedFailed | DeployLockLossOutcome::NotApplying => {
                        if let Err(error) = deploy_lock.release().await {
                            tracing::warn!(%error, "failed to release NATS deploy lock after renewal failure");
                        }
                        return self.err("DEPLOY_LOCK_FAILED", message);
                    }
                }
            }
        };
        deploy_lock_renewer.abort();
        if let Err(error) = deploy_lock_renewer.await
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "deploy lock renewal task failed during shutdown");
        }
        if let Err(error) = deploy_lock.release().await {
            tracing::warn!(%error, "failed to release NATS deploy lock");
        }
        match result {
            Ok(result) => self.ok_json_pretty(&result, "ENCODE_DEPLOY", "encode deploy result"),
            Err(err) => self.deploy_error_response("DEPLOY_APPLY_FAILED", err),
        }
    }

    fn deploy_error_response(&self, code: &str, error: PloyzError) -> DaemonResponse {
        if let Some(payload) = deploy_failure_payload_for_error(&error) {
            self.err_with_payload(
                code,
                error.to_string(),
                Some(DaemonPayload::DeployFailure(payload)),
            )
        } else {
            self.err(code, error.to_string())
        }
    }

    pub async fn handle_deploy_export(&self, namespace: &str) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let namespace = Namespace(namespace.to_string());
        let manifest = match export_manifest(&active.mesh.store, &namespace).await {
            Ok(manifest) => manifest,
            Err(err) => return self.err("DEPLOY_EXPORT_FAILED", format!("{err}")),
        };
        self.ok_json_pretty(&manifest, "ENCODE_MANIFEST", "encode manifest")
    }

    pub async fn handle_deploy_node_inspect_namespace(
        &self,
        namespace: &str,
        _deploy_id: &str,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        match agent.inspect_namespace(&namespace).await {
            Ok(instances) => self.ok_with_payload(
                "namespace inspected",
                Some(DaemonPayload::DeployNamespaceSnapshot(
                    DeployNamespaceSnapshotPayload { instances },
                )),
            ),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn handle_deploy_node_start_candidate(
        &self,
        namespace: &str,
        deploy_id: &str,
        service: &str,
        slot_id: &str,
        instance_id: &str,
        spec_json: &str,
        volumes_json: &str,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let deploy_id = DeployId(deploy_id.to_string());
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let context = agent.command_context(namespace);
        match agent
            .start_candidate(
                &context,
                service,
                &SlotId(slot_id.to_string()),
                &InstanceId(instance_id.to_string()),
                &deploy_id,
                spec_json,
                volumes_json,
            )
            .await
        {
            Ok(status) => self.ok_with_payload(
                "candidate started",
                Some(DaemonPayload::DeployCandidateStarted(
                    DeployCandidateStartedPayload { status },
                )),
            ),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    pub async fn handle_deploy_node_drain_instance(
        &self,
        namespace: &str,
        deploy_id: &str,
        instance_id: &str,
    ) -> DaemonResponse {
        self.handle_deploy_node_instance_command(
            namespace,
            deploy_id,
            instance_id,
            DeployNodeOp::Drain,
        )
        .await
    }

    pub async fn handle_deploy_node_remove_instance(
        &self,
        namespace: &str,
        deploy_id: &str,
        instance_id: &str,
    ) -> DaemonResponse {
        self.handle_deploy_node_instance_command(
            namespace,
            deploy_id,
            instance_id,
            DeployNodeOp::Remove,
        )
        .await
    }

    async fn handle_deploy_node_instance_command(
        &self,
        namespace: &str,
        _deploy_id: &str,
        instance_id: &str,
        op: DeployNodeOp,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let context = agent.command_context(namespace);
        let instance_id = InstanceId(instance_id.to_string());
        let result = match op {
            DeployNodeOp::Drain => agent.drain_instance(&context, &instance_id).await,
            DeployNodeOp::Remove => agent.remove_instance(&context, &instance_id).await,
        };
        match result {
            Ok(()) => self.ok("deploy node command completed"),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    async fn deploy_node_agent(&self) -> Result<DeployAgent, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        let store = active.mesh.store.clone();
        let machine_id = self.identity.machine_id.clone();
        let overlay_network_name = self.overlay_network_name();
        let overlay_dns_server = self.overlay_dns_server();
        let storage_driver = self.zfs_storage_driver().await?;
        Ok(DeployAgent::new(
            store,
            machine_id,
            overlay_network_name,
            overlay_dns_server,
            storage_driver,
        ))
    }
}

async fn renew_deploy_lock(
    deploy_lock: NatsDeployLock,
    ttl: Duration,
    interval: Duration,
) -> ployz_types::Result<()> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        deploy_lock.renew(ttl).await?;
    }
}

enum DeployNodeOp {
    Drain,
    Remove,
}

#[derive(Clone)]
struct NatsDeployParticipantClient {
    client: NatsNodeRpcClient,
}

impl NatsDeployParticipantClient {
    #[must_use]
    fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl DeployParticipantClient for NatsDeployParticipantClient {
    fn supports_volume_moves(&self) -> bool {
        true
    }

    async fn inspect_namespace(
        &self,
        machine: &MachineMembership,
        namespace: &Namespace,
        deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> ployz_types::Result<Vec<InstanceStatusRecord>> {
        let response = self
            .client
            .request(
                NodeCommandSubject::deploy_inspect_namespace(&machine.id),
                &ployz_api::DaemonRequest::DeployNodeInspectNamespace {
                    namespace: namespace.0.clone(),
                    deploy_id: deploy_id.0.clone(),
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if !response.ok {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_inspect",
                code: response.code,
                message: response.message,
            }));
        }
        let Some(DaemonPayload::DeployNamespaceSnapshot(payload)) = response.payload else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "namespace snapshot",
            }));
        };
        Ok(payload.instances)
    }

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: StartCandidateRequest,
    ) -> ployz_types::Result<InstanceStatusRecord> {
        let response = self
            .client
            .request(
                NodeCommandSubject::deploy_start_candidate(machine_id),
                &ployz_api::DaemonRequest::DeployNodeStartCandidate {
                    namespace: namespace.0.clone(),
                    deploy_id: deploy_id.0.clone(),
                    service: request.service,
                    slot_id: request.slot_id.0,
                    instance_id: request.instance_id.0,
                    spec_json: request.spec_json,
                    volumes_json: request.volumes_json,
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if !response.ok {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_start_candidate",
                code: response.code,
                message: response.message,
            }));
        }
        let Some(DaemonPayload::DeployCandidateStarted(payload)) = response.payload else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "candidate",
            }));
        };
        Ok(payload.status)
    }

    async fn move_volume(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> ployz_types::Result<MoveVolumeResult> {
        run_volume_move_rpc(
            &self.client,
            machine_id,
            namespace,
            deploy_id,
            request,
            DEPLOY_VOLUME_MOVE_START_RPC_TIMEOUT,
            DEPLOY_VOLUME_MOVE_RPC_TIMEOUT,
            DEPLOY_VOLUME_MOVE_POLL_INTERVAL,
        )
        .await
    }

    async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_types::Result<()> {
        self.expect_ok(
            NodeCommandSubject::deploy_drain_instance(machine_id),
            ployz_api::DaemonRequest::DeployNodeDrainInstance {
                namespace: namespace.0.clone(),
                deploy_id: deploy_id.0.clone(),
                instance_id: instance_id.0.clone(),
            },
            "deploy_node_drain",
        )
        .await
    }

    async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_types::Result<()> {
        self.expect_ok(
            NodeCommandSubject::deploy_remove_instance(machine_id),
            ployz_api::DaemonRequest::DeployNodeRemoveInstance {
                namespace: namespace.0.clone(),
                deploy_id: deploy_id.0.clone(),
                instance_id: instance_id.0.clone(),
            },
            "deploy_node_remove",
        )
        .await
    }
}

impl NatsDeployParticipantClient {
    async fn expect_ok(
        &self,
        subject: NodeCommandSubject,
        request: ployz_api::DaemonRequest,
        operation: &'static str,
    ) -> ployz_types::Result<()> {
        let response = self
            .client
            .request(subject, &request)
            .await
            .map_err(PloyzError::from)?;
        if response.ok {
            return Ok(());
        }
        Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation,
            code: response.code,
            message: response.message,
        }))
    }
}

async fn run_volume_move_rpc<R: DeployMoveRpcClient>(
    client: &R,
    machine_id: &MachineId,
    namespace: &Namespace,
    _deploy_id: &DeployId,
    request: MoveVolumeRequest,
    start_timeout: Duration,
    wait_timeout: Duration,
    poll_interval: Duration,
) -> ployz_types::Result<MoveVolumeResult> {
    let MoveVolumeRequest {
        volume,
        from_machine,
        to_machine,
        snapshot,
    } = request;
    if *machine_id != from_machine {
        return Err(PloyzError::operation(
            "deploy_node_move_volume",
            format!(
                "move volume '{volume}' was sent to '{machine_id}' but request source was '{from_machine}'"
            ),
        ));
    }
    let move_client = client.with_rpc_policy(RpcPolicy {
        timeout: start_timeout,
    });
    let response = move_client
        .request(
            NodeCommandSubject::volume_zfs_send(machine_id),
            &ployz_api::DaemonRequest::VolumeZfsSend {
                namespace: namespace.0.clone(),
                volume,
                snapshot,
                target_machine: to_machine.0,
                from_snapshot: None,
            },
        )
        .await
        .map_err(|error| volume_move_rpc_error("volume_zfs_send", error))?;
    if !response.ok {
        return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation: "volume_zfs_send",
            code: response.code,
            message: response.message,
        }));
    }
    let Some(DaemonPayload::VolumeZfsTransfer(payload)) = response.payload else {
        return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
            payload: "volume zfs transfer",
        }));
    };
    wait_for_volume_transfer(
        client,
        machine_id,
        payload.transfer.id,
        wait_timeout,
        DEPLOY_VOLUME_MOVE_POLL_RPC_TIMEOUT,
        poll_interval,
    )
    .await
}

async fn wait_for_volume_transfer<R: DeployMoveRpcClient>(
    client: &R,
    machine_id: &MachineId,
    transfer_id: String,
    timeout: Duration,
    poll_rpc_timeout: Duration,
    poll_interval: Duration,
) -> ployz_types::Result<MoveVolumeResult> {
    let started = tokio::time::Instant::now();
    let mut retry_delay = poll_interval;
    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(PloyzError::operation(
                "volume_zfs_transfer",
                format!("timed out waiting for zfs transfer '{transfer_id}'"),
            ));
        };
        let poll_client = client.with_rpc_policy(RpcPolicy {
            timeout: std::cmp::min(poll_rpc_timeout, remaining),
        });
        let response = match tokio::time::timeout(
            remaining,
            poll_client.request(
                NodeCommandSubject::volume_zfs_transfer_get(machine_id),
                &ployz_api::DaemonRequest::VolumeZfsTransferGet {
                    id: transfer_id.clone(),
                },
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                if started.elapsed() >= timeout {
                    return Err(volume_move_rpc_error("volume_zfs_transfer_get", error));
                }
                tracing::warn!(
                    %error,
                    transfer_id,
                    machine_id = %machine_id,
                    "retrying zfs transfer status read after transient RPC failure"
                );
                tokio::time::sleep(retry_delay + retry_jitter(retry_delay)).await;
                retry_delay = std::cmp::min(retry_delay + retry_delay, Duration::from_secs(30));
                continue;
            }
            Err(error) => {
                return Err(PloyzError::operation(
                    "volume_zfs_transfer",
                    format!("timed out waiting for zfs transfer '{transfer_id}': {error}"),
                ));
            }
        };
        retry_delay = poll_interval;
        if !response.ok {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer_get",
                code: response.code,
                message: response.message,
            }));
        }
        let Some(DaemonPayload::VolumeZfsTransfer(payload)) = response.payload else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            }));
        };
        if let Some(result) = volume_move_result_from_transfer(payload)? {
            return Ok(result);
        }
        if started.elapsed() >= timeout {
            return Err(PloyzError::operation(
                "volume_zfs_transfer",
                format!("timed out waiting for zfs transfer '{transfer_id}'"),
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn retry_jitter(delay: Duration) -> Duration {
    let max_millis = (delay.as_millis() as u64 / 2).min(1_000);
    if max_millis == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(rand::random::<u64>() % (max_millis + 1))
    }
}

fn volume_move_rpc_error(operation: &'static str, error: RpcFailure) -> PloyzError {
    PloyzError::Deploy(DeployError::RemoteNodeError {
        operation,
        code: error.code().into(),
        message: error.message,
    })
}

fn volume_move_result_from_transfer(
    payload: VolumeZfsTransferPayload,
) -> ployz_types::Result<Option<MoveVolumeResult>> {
    match payload.transfer.status.as_str() {
        "succeeded" => {
            let Some(snapshot_guid) = payload.transfer.snapshot_guid else {
                return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                    payload: "volume zfs transfer snapshot guid",
                }));
            };
            let Some(bytes_transferred) = payload.transfer.bytes_transferred else {
                return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                    payload: "volume zfs transfer bytes",
                }));
            };
            Ok(Some(MoveVolumeResult {
                snapshot: payload.transfer.snapshot_name,
                snapshot_guid,
                bytes_transferred,
            }))
        }
        "failed" | "interrupted" => Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation: "volume_zfs_transfer",
            code: payload.transfer.status,
            message: payload.transfer.last_error.unwrap_or_else(|| {
                format!("zfs transfer '{}' did not succeed", payload.transfer.id)
            }),
        })),
        "running" => Ok(None),
        other => Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation: "volume_zfs_transfer",
            code: other.to_string(),
            message: format!(
                "zfs transfer '{}' reported unknown status '{other}'",
                payload.transfer.id
            ),
        })),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployLockLossOutcome {
    MarkedFailed,
    PastCommit,
    NotApplying,
}

async fn mark_deploy_failed_after_lock_loss(
    store: &StoreDriver,
    deploy_id: &DeployId,
    message: &str,
) -> DeployLockLossOutcome {
    let deploy = match store.get_deploy(deploy_id).await {
        Ok(Some(mut deploy)) => {
            match deploy.state {
                ployz_types::model::DeployState::Committed
                | ployz_types::model::DeployState::CleanupPending
                | ployz_types::model::DeployState::CheckpointCommitted => {
                    return DeployLockLossOutcome::PastCommit;
                }
                ployz_types::model::DeployState::Applying => {}
                ployz_types::model::DeployState::Planning
                | ployz_types::model::DeployState::FailedAfterCheckpoint
                | ployz_types::model::DeployState::Failed => {
                    return DeployLockLossOutcome::NotApplying;
                }
            }
            if deploy_has_checkpoint_commit_point(store, &deploy.namespace, deploy_id).await {
                return DeployLockLossOutcome::PastCommit;
            }
            deploy.state = ployz_types::model::DeployState::Failed;
            deploy.finished_at = Some(ployz_types::time::now_unix_secs());
            if let Ok(mut preview) =
                serde_json::from_str::<ployz_types::model::DeployPreview>(&deploy.summary_json)
            {
                preview
                    .warnings
                    .push(format!("deploy lock lost during apply: {message}"));
                if let Ok(summary_json) = serde_json::to_string(&preview) {
                    deploy.summary_json = summary_json;
                }
            }
            deploy
        }
        Ok(None) => return DeployLockLossOutcome::NotApplying,
        Err(error) => {
            tracing::warn!(%error, %deploy_id, "failed to read deploy record after lock loss");
            return DeployLockLossOutcome::NotApplying;
        }
    };
    if let Err(error) = store.write_deploy_status(&deploy).await {
        tracing::warn!(%error, %deploy_id, "failed to mark deploy failed after lock loss");
        return DeployLockLossOutcome::NotApplying;
    }
    if let Err(error) = mark_running_deploy_phases_failed_after_lock_loss(
        store,
        &deploy.namespace,
        deploy_id,
        message,
    )
    .await
    {
        tracing::warn!(
            %error,
            %deploy_id,
            "failed to mark running deploy phases failed after lock loss"
        );
    }
    DeployLockLossOutcome::MarkedFailed
}

async fn deploy_has_checkpoint_commit_point(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
) -> bool {
    let phases = match store.list_deploy_phases(namespace, deploy_id).await {
        Ok(phases) => phases,
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy phases while classifying lock loss"
            );
            return false;
        }
    };
    let checkpoint_phase_ids = phases
        .iter()
        .filter(|phase| phase.commit_policy == DeployPhaseCommitPolicy::Checkpoint)
        .map(|phase| DeployId(format!("{}:phase:{}", deploy_id.0, phase.phase_id.0)))
        .collect::<HashSet<_>>();
    if checkpoint_phase_ids.is_empty() {
        return false;
    }
    if phases.iter().any(|phase| {
        phase.commit_policy == DeployPhaseCommitPolicy::Checkpoint
            && matches!(phase.state, DeployPhaseState::Succeeded { .. })
    }) {
        return true;
    }
    match store.list_deploy_releases(namespace).await {
        Ok(releases) => {
            if releases
                .iter()
                .any(|release| checkpoint_phase_ids.contains(&release.release.updated_by_deploy_id))
            {
                return true;
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy releases while classifying lock loss"
            );
        }
    }
    match store.list_volumes(namespace).await {
        Ok(volumes) => volumes.iter().any(|volume| {
            checkpoint_phase_ids.contains(&volume.created_by_deploy_id)
                || checkpoint_phase_ids.contains(&volume.last_modified_by_deploy_id)
        }),
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy volumes while classifying lock loss"
            );
            false
        }
    }
}

async fn mark_running_deploy_phases_failed_after_lock_loss(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    message: &str,
) -> ployz_types::Result<()> {
    let phases = store.list_deploy_phases(namespace, deploy_id).await?;
    let completed_at = ployz_types::time::now_unix_secs();
    for mut phase in phases {
        if !matches!(
            phase.state,
            DeployPhaseState::Pending | DeployPhaseState::Running
        ) {
            continue;
        }
        phase.state = DeployPhaseState::Failed {
            completed_at,
            failure: DeployPhaseFailure {
                code: "DEPLOY_LOCK_LOST".into(),
                message: format!("deploy lock lost during apply: {message}"),
            },
        };
        store.upsert_deploy_phase(&phase).await?;
    }
    Ok(())
}

fn deploy_failure_payload_for_error(error: &PloyzError) -> Option<DeployFailurePayload> {
    match error {
        PloyzError::Deploy(DeployError::NoEligiblePlacementTargets) => Some(DeployFailurePayload {
            reason: DeployFailureReason::NoEligiblePlacementTargets,
        }),
        _ => None,
    }
}

fn decode_manifest(manifest_json: &str) -> Result<DeployManifest, Box<DaemonResponse>> {
    let manifest: DeployManifest = serde_json::from_str(manifest_json).map_err(|err| {
        Box::new(DaemonResponse {
            ok: false,
            code: "INVALID_MANIFEST".into(),
            message: format!("invalid deploy manifest: {err}"),
            payload: None,
        })
    })?;

    Ok(manifest)
}

async fn export_manifest(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_types::Result<DeployManifest> {
    let releases = store.list_deploy_releases(namespace).await?;
    let revisions = store.list_deploy_revisions(namespace).await?;
    let volume_records = store.list_volumes(namespace).await?;
    let revisions_by_key: BTreeMap<(String, String), String> = revisions
        .into_iter()
        .map(|revision| {
            (
                (revision.service.clone(), revision.revision_hash.clone()),
                revision.spec_json,
            )
        })
        .collect();

    let mut services = Vec::with_capacity(releases.len());
    for release in releases {
        let key = (
            release.service.clone(),
            release.release.primary_revision_hash.clone(),
        );
        let Some(spec_json) = revisions_by_key.get(&key) else {
            return Err(PloyzError::Deploy(
                DeployError::StoredReleaseMissingRevision {
                    service: release.service,
                    revision_hash: release.release.primary_revision_hash,
                },
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(spec_json).map_err(|err| {
            PloyzError::Deploy(DeployError::CommittedServiceSpecDecode {
                namespace: namespace.0.clone(),
                service: release.service.clone(),
                message: err.to_string(),
            })
        })?;
        if spec.name != release.service {
            return Err(PloyzError::Deploy(DeployError::StoredSpecServiceMismatch {
                stored_service: spec.name,
                release_service: release.service,
            }));
        }
        services.push(spec);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));

    let mut volumes: Vec<VolumeDeclaration> = volume_records
        .into_iter()
        .map(|record| VolumeDeclaration {
            name: record.volume_name,
            scope: record.scope,
            quota: record.quota,
            mode: record.mode,
            owner: record.owner,
        })
        .collect();
    volumes.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(DeployManifest {
        namespace: namespace.clone(),
        intent: None,
        volumes,
        services,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::{DaemonRequest, VolumeZfsTransferInfo};
    use ployz_nats::{RpcFailureKind, RpcPolicy};
    use ployz_store_api::{DeployCommit, DeployStore};
    use ployz_types::model::{
        DeployId, DeployPhaseCommitPolicy, DeployPhaseId, DeployPhaseRecord,
        DeployPhaseRollbackPolicy, DeployPreview, DeployRecord, DeployState, MachineId,
        ServiceRelease, ServiceReleaseRecord, ServiceRevisionRecord, ServiceRoutingPolicy,
        VolumeRecord,
    };
    use ployz_types::spec::{
        ContainerSpec, Mount, MountSource, NetworkMode, Placement, PullPolicy, Resources,
        RestartPolicy, RolloutStrategy, VolumeScope,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn test_service() -> ServiceSpec {
        ServiceSpec {
            name: "db".into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: "postgres:17".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                mounts: vec![Mount {
                    source: MountSource::Volume("data".into()),
                    target: "/var/lib/postgresql/data".into(),
                    readonly: false,
                }],
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                stop_grace_period: None,
                pid_mode: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[test]
    fn decode_manifest_accepts_empty_services() {
        let manifest_json = serde_json::to_string(&DeployManifest {
            namespace: Namespace("prod".into()),
            intent: None,
            volumes: Vec::new(),
            services: Vec::new(),
        })
        .expect("serialize manifest");

        let manifest = decode_manifest(&manifest_json).expect("decode manifest");

        assert_eq!(manifest.namespace, Namespace("prod".into()));
        assert!(manifest.services.is_empty());
    }

    #[test]
    fn decode_manifest_rejects_invalid_json_with_structured_error() {
        let error = decode_manifest("{not-json")
            .expect_err("invalid manifest json should return daemon error");

        assert!(!error.ok);
        assert_eq!(error.code, "INVALID_MANIFEST");
        assert!(error.message.starts_with("invalid deploy manifest:"));
        assert!(error.payload.is_none());
    }

    #[test]
    fn volume_move_result_maps_succeeded_transfer() {
        let result = volume_move_result_from_transfer(transfer_payload(
            "succeeded",
            Some(42),
            Some(4096),
            None,
        ))
        .expect("transfer result")
        .expect("succeeded transfer should complete");

        assert_eq!(result.snapshot, "ployz-move-manifest-data");
        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(result.bytes_transferred, 4096);
    }

    #[test]
    fn volume_move_result_keeps_running_transfer_pending() {
        let result =
            volume_move_result_from_transfer(transfer_payload("running", None, None, None))
                .expect("running transfer is not an error");

        assert!(result.is_none());
    }

    #[test]
    fn volume_move_result_rejects_terminal_failed_transfer() {
        let error = volume_move_result_from_transfer(transfer_payload(
            "failed",
            None,
            None,
            Some("receiver disconnected".into()),
        ))
        .expect_err("failed transfer should fail deploy move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "failed".into(),
                message: "receiver disconnected".into(),
            })
        );
    }

    #[test]
    fn volume_move_result_requires_success_evidence() {
        let missing_guid =
            volume_move_result_from_transfer(transfer_payload("succeeded", None, Some(4096), None))
                .expect_err("missing guid should fail");
        assert_eq!(
            missing_guid,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer snapshot guid",
            })
        );

        let missing_bytes =
            volume_move_result_from_transfer(transfer_payload("succeeded", Some(42), None, None))
                .expect_err("missing bytes should fail");
        assert_eq!(
            missing_bytes,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer bytes",
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_waits_for_terminal_transfer_success() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("succeeded", Some(42), Some(4096), None)),
        ]);

        let result = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect("move result");

        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(result.bytes_transferred, 4096);
        let requests = client.requests();
        let [send, first_poll, second_poll] = requests.as_slice() else {
            panic!("expected send plus two polls, got {requests:?}");
        };
        assert!(send.0.contains("volume.zfs.send"), "{send:?}");
        assert!(
            first_poll.0.contains("volume.zfs.transfer_get"),
            "{first_poll:?}"
        );
        assert!(
            second_poll.0.contains("volume.zfs.transfer_get"),
            "{second_poll:?}"
        );
        match &send.1 {
            DaemonRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot,
            } => {
                assert_eq!(namespace, "prod");
                assert_eq!(volume, "data");
                assert_eq!(snapshot, "ployz-move-manifest-data");
                assert_eq!(target_machine, "machine-b");
                assert_eq!(from_snapshot, &None);
            }
            other => panic!("expected VolumeZfsSend, got {other:?}"),
        }
        assert_eq!(
            client
                .policies()
                .first()
                .expect("move start timeout policy should be applied")
                .timeout,
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_classifies_transport_failure_as_volume_move_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![Err(RpcFailure::new(
            RpcFailureKind::NoResponders,
            "no responder",
        ))]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("transport error should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_send",
                code: "NATS_RPC_NO_RESPONDERS".into(),
                message: "no responder".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_rejects_source_machine_mismatch_before_rpc() {
        let client = FakeMoveRpcClient::with_responses(Vec::new());

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-other".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("source mismatch should fail before RPC");

        assert!(matches!(
            error,
            PloyzError::Operation {
                operation: "deploy_node_move_volume",
                ..
            }
        ));
        assert!(client.requests().is_empty());
    }

    #[tokio::test]
    async fn volume_move_rpc_surfaces_start_response_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![Ok(DaemonResponse {
            ok: false,
            code: "START_FAILED".into(),
            message: "cannot snapshot".into(),
            payload: None,
        })]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("failed start response should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_send",
                code: "START_FAILED".into(),
                message: "cannot snapshot".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_requires_start_payload() {
        let client = FakeMoveRpcClient::with_responses(vec![Ok(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "ok".into(),
            payload: None,
        })]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("missing start payload should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_retries_transient_transfer_poll_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Err(RpcFailure::new(
                RpcFailureKind::NoResponders,
                "no responder",
            )),
            Ok(transfer_response("succeeded", Some(42), Some(4096), None)),
        ]);

        let result = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect("move result");

        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(client.requests().len(), 3);
    }

    #[tokio::test]
    async fn volume_move_rpc_surfaces_terminal_poll_response_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(DaemonResponse {
                ok: false,
                code: "TRANSFER_FAILED".into(),
                message: "receiver disconnected".into(),
                payload: None,
            }),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("failed poll response should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer_get",
                code: "TRANSFER_FAILED".into(),
                message: "receiver disconnected".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_requires_poll_payload() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "ok".into(),
                payload: None,
            }),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("missing poll payload should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_times_out_waiting_for_running_transfer() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("running", None, None, None)),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId("machine-a".into()),
            &Namespace("prod".into()),
            &DeployId("deploy-1".into()),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId("machine-a".into()),
                to_machine: MachineId("machine-b".into()),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_millis(1),
        )
        .await
        .expect_err("running transfer should time out");

        assert!(matches!(
            error,
            PloyzError::Operation {
                operation: "volume_zfs_transfer",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn lock_loss_marks_applying_deploy_failed_with_warning() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId("deploy-lock-loss".into());
        let preview = DeployPreview {
            namespace: Namespace("prod".into()),
            manifest_hash: "manifest".into(),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            warnings: Vec::new(),
        };
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace("prod".into()),
                coordinator_machine_id: MachineId("local".into()),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&preview).expect("preview json"),
            })
            .await
            .expect("seed applying deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;
        assert_eq!(outcome, DeployLockLossOutcome::MarkedFailed);

        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Failed);
        assert!(record.finished_at.is_some());
        let preview: DeployPreview =
            serde_json::from_str(&record.summary_json).expect("preview json");
        assert!(
            preview
                .warnings
                .iter()
                .any(|warning| warning.contains("renew failed"))
        );
    }

    #[tokio::test]
    async fn lock_loss_marks_running_deploy_phases_failed() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-lock-loss-phase".into());
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId("local".into()),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&DeployPreview {
                    namespace: namespace.clone(),
                    manifest_hash: "manifest".into(),
                    participants: Vec::new(),
                    phases: Vec::new(),
                    services: Vec::new(),
                    service_branch_sources: Vec::new(),
                    volume_moves: Vec::new(),
                    warnings: Vec::new(),
                })
                .expect("preview json"),
            })
            .await
            .expect("seed applying deploy");
        store
            .upsert_deploy_phase(&DeployPhaseRecord {
                namespace: namespace.clone(),
                deploy_id: deploy_id.clone(),
                phase_id: DeployPhaseId("deploy".into()),
                commit_deploy_id: None,
                name: "Deploy".into(),
                order: 0,
                after: Vec::new(),
                participants: Vec::new(),
                work: Vec::new(),
                state: DeployPhaseState::Running,
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: ployz_types::model::DeployPhaseAdvancePolicy::Immediate,
                started_at: 1,
            })
            .await
            .expect("seed running phase");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::MarkedFailed);
        let phases = store
            .list_deploy_phases(&namespace, &deploy_id)
            .await
            .expect("list phases");
        let [phase] = phases.as_slice() else {
            panic!("expected one phase, got {phases:?}");
        };
        let DeployPhaseState::Failed { failure, .. } = &phase.state else {
            panic!("expected failed phase, got {:?}", phase.state);
        };
        assert_eq!(failure.code, "DEPLOY_LOCK_LOST");
        assert!(failure.message.contains("renew failed"));
    }

    #[tokio::test]
    async fn lock_loss_after_commit_does_not_mark_deploy_failed() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId("deploy-lock-loss-committed".into());
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace("prod".into()),
                coordinator_machine_id: MachineId("local".into()),
                manifest_hash: "manifest".into(),
                state: DeployState::Committed,
                started_at: 1,
                committed_at: Some(2),
                finished_at: Some(2),
                summary_json: "{}".into(),
            })
            .await
            .expect("seed committed deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Committed);
    }

    #[tokio::test]
    async fn lock_loss_after_checkpoint_status_waits_for_apply() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId("deploy-lock-loss-checkpoint-status".into());
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace("prod".into()),
                coordinator_machine_id: MachineId("local".into()),
                manifest_hash: "manifest".into(),
                state: DeployState::CheckpointCommitted,
                started_at: 1,
                committed_at: Some(2),
                finished_at: None,
                summary_json: "{}".into(),
            })
            .await
            .expect("seed checkpoint committed deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::CheckpointCommitted);
    }

    #[tokio::test]
    async fn lock_loss_after_durable_checkpoint_commit_evidence_waits_for_apply() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-lock-loss-checkpoint-evidence".into());
        let phase_id = DeployPhaseId("db".into());
        let phase_commit_id = DeployId(format!("{}:phase:{}", deploy_id.0, phase_id.0));
        let service = test_service();
        let revision_hash = "rev-db".to_string();
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId("local".into()),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&DeployPreview {
                    namespace: namespace.clone(),
                    manifest_hash: "manifest".into(),
                    participants: Vec::new(),
                    phases: Vec::new(),
                    services: Vec::new(),
                    service_branch_sources: Vec::new(),
                    volume_moves: Vec::new(),
                    warnings: Vec::new(),
                })
                .expect("preview json"),
            })
            .await
            .expect("seed applying deploy");
        store
            .upsert_deploy_phase(&DeployPhaseRecord {
                namespace: namespace.clone(),
                deploy_id: deploy_id.clone(),
                phase_id: phase_id.clone(),
                name: "db".into(),
                order: 0,
                after: Vec::new(),
                participants: Vec::new(),
                work: Vec::new(),
                state: DeployPhaseState::Running,
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: ployz_types::model::DeployPhaseAdvancePolicy::Immediate,
                started_at: 1,
            })
            .await
            .expect("seed running checkpoint phase");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId("local".into()),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: phase_commit_id.clone(),
                        updated_at: 2,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id: phase_commit_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId("local".into()),
                    manifest_hash: "manifest".into(),
                    state: DeployState::CheckpointCommitted,
                    started_at: 1,
                    committed_at: Some(2),
                    finished_at: None,
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed durable checkpoint commit evidence");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Applying);
        let phase = store
            .get_deploy_phase(&namespace, &deploy_id, &phase_id)
            .await
            .expect("get phase")
            .expect("phase");
        assert!(matches!(phase.state, DeployPhaseState::Running));
    }

    #[tokio::test]
    async fn export_manifest_includes_stored_volume_declarations() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let service = test_service();
        let revision_hash = "rev-db".to_string();
        let deploy_id = DeployId("deploy-1".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId("local".into()),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: vec![VolumeRecord {
                    namespace: namespace.clone(),
                    volume_name: "data".into(),
                    scope: VolumeScope::Single,
                    machine_id: MachineId("machine-a".into()),
                    quota: "10G".into(),
                    mode: "0750".into(),
                    owner: "999:999".into(),
                    attached_services: vec!["db".into()],
                    created_at: 1,
                    created_by_deploy_id: deploy_id.clone(),
                    last_modified_at: 1,
                    last_modified_by_deploy_id: deploy_id.clone(),
                }],
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId("local".into()),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed release and volume");

        let manifest = export_manifest(&store, &namespace)
            .await
            .expect("export manifest");

        let [volume] = manifest.volumes.as_slice() else {
            panic!("expected one volume declaration");
        };
        assert_eq!(volume.name, "data");
        assert_eq!(volume.scope, VolumeScope::Single);
        assert_eq!(volume.quota, "10G");
        assert_eq!(volume.mode, "0750");
        assert_eq!(volume.owner, "999:999");
        manifest.validate().expect("export should validate");
    }

    #[tokio::test]
    async fn export_manifest_surfaces_release_referencing_missing_revision() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-1".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    release: ServiceRelease {
                        primary_revision_hash: "missing-rev".into(),
                        referenced_revision_hashes: vec!["missing-rev".into()],
                        routing: ServiceRoutingPolicy::Direct {
                            revision_hash: "missing-rev".into(),
                        },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId("local".into()),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed corrupt release");

        let error = export_manifest(&store, &namespace)
            .await
            .expect_err("missing revision should fail export");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::StoredReleaseMissingRevision {
                service: "api".into(),
                revision_hash: "missing-rev".into()
            })
        );
    }

    #[tokio::test]
    async fn export_manifest_surfaces_stored_spec_service_mismatch() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let mut service = test_service();
        service.name = "wrong-service".into();
        let revision_hash = "rev-api".to_string();
        let deploy_id = DeployId("deploy-1".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId("local".into()),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId("local".into()),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed release");

        let error = export_manifest(&store, &namespace)
            .await
            .expect_err("mismatched stored spec should fail export");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::StoredSpecServiceMismatch {
                stored_service: "wrong-service".into(),
                release_service: "api".into()
            })
        );
    }

    #[derive(Clone, Default)]
    struct FakeMoveRpcClient {
        responses: Arc<Mutex<VecDeque<std::result::Result<DaemonResponse, RpcFailure>>>>,
        requests: Arc<Mutex<Vec<(String, DaemonRequest)>>>,
        policies: Arc<Mutex<Vec<RpcPolicy>>>,
    }

    impl FakeMoveRpcClient {
        fn with_responses(responses: Vec<std::result::Result<DaemonResponse, RpcFailure>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<(String, DaemonRequest)> {
            self.requests.lock().expect("requests").clone()
        }

        fn policies(&self) -> Vec<RpcPolicy> {
            self.policies.lock().expect("policies").clone()
        }
    }

    #[async_trait::async_trait]
    impl DeployMoveRpcClient for FakeMoveRpcClient {
        fn with_rpc_policy(&self, policy: RpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn request(
            &self,
            subject: NodeCommandSubject,
            request: &DaemonRequest,
        ) -> std::result::Result<DaemonResponse, RpcFailure> {
            self.requests
                .lock()
                .expect("requests")
                .push((format!("{subject:?}"), request.clone()));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("fake response queued")
        }
    }

    fn transfer_response(
        status: &str,
        snapshot_guid: Option<u64>,
        bytes_transferred: Option<u64>,
        last_error: Option<String>,
    ) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "ok".into(),
            payload: Some(DaemonPayload::VolumeZfsTransfer(transfer_payload(
                status,
                snapshot_guid,
                bytes_transferred,
                last_error,
            ))),
        }
    }

    fn transfer_payload(
        status: &str,
        snapshot_guid: Option<u64>,
        bytes_transferred: Option<u64>,
        last_error: Option<String>,
    ) -> VolumeZfsTransferPayload {
        VolumeZfsTransferPayload {
            transfer: VolumeZfsTransferInfo {
                id: "transfer-1".into(),
                namespace: "prod".into(),
                volume: "data".into(),
                source_machine: MachineId("machine-a".into()),
                target_machine: MachineId("machine-b".into()),
                status: status.into(),
                stage: "finished".into(),
                snapshot_name: "ployz-move-manifest-data".into(),
                snapshot_guid,
                from_snapshot_name: None,
                from_snapshot_guid: None,
                bytes_transferred,
                started_at: 1,
                updated_at: 2,
                last_error,
            },
        }
    }
}
