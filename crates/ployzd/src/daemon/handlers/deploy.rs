use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, DeployCandidateStartedPayload, DeployNamespaceSnapshotPayload,
    DeployOptions,
};
use ployz_cert_backends::InstantAcmeIssuerFactory;
use ployz_config::RuntimeTarget;
use ployz_nats::{NatsDeployLock, NatsLocks};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcPolicy};
use ployz_orchestrator::certificates::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::participant::{DeployParticipantClient, StartCandidateRequest};
use ployz_orchestrator::deploy::{apply_with_certificate_coordination, preview};
use ployz_runtime_backends::deploy::remote::DeployAgent;
use ployz_store_api::{DeployStore, StoreDriver, StoreRuntimeControl};
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::SlotId;
use ployz_types::model::{
    DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineMembership,
};
use ployz_types::spec::{DeployManifest, Namespace, ServiceSpec, VolumeDeclaration};

const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);
const DEPLOY_PARTICIPANT_RPC_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
                Ok(store) => store,
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
            Err(err) => self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
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
                Ok(store) => store,
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

        let apply = apply_with_certificate_coordination(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            &manifest,
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
                if let Err(error) = deploy_lock.release().await {
                    tracing::warn!(%error, "failed to release NATS deploy lock after renewal failure");
                }
                return self.err("DEPLOY_LOCK_FAILED", message);
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
            Err(err) => self.err("DEPLOY_APPLY_FAILED", format!("{err}")),
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
        volumes,
        services,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_store_api::{DeployCommit, DeployStore};
    use ployz_types::model::{
        DeployId, DeployRecord, DeployState, MachineId, ServiceRelease, ServiceReleaseRecord,
        ServiceRevisionRecord, ServiceRoutingPolicy, VolumeRecord,
    };
    use ployz_types::spec::{
        ContainerSpec, Mount, MountSource, NetworkMode, Placement, PullPolicy, Resources,
        RestartPolicy, RolloutStrategy, VolumeScope,
    };

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
}
