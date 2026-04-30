use std::collections::BTreeMap;
use std::sync::Arc;

use crate::daemon::DaemonState;
use ployz_api::{DaemonResponse, DeployOptions};
use ployz_cert_backends::InstantAcmeIssuerFactory;
use ployz_config::RuntimeTarget;
use ployz_orchestrator::certificates::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_orchestrator::deploy::{apply_with_certificate_coordination, preview};
use ployz_runtime_backends::deploy::remote::DeployAgent;
use ployz_runtime_backends::deploy::session::DefaultDeploySessionFactory;
use ployz_store_api::DeployRepository;
use ployz_store_api::StoreDriver;
use ployz_types::Error as PloyzError;
use ployz_types::spec::{DeployManifest, Namespace, ServiceSpec, VolumeDeclaration};

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

        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return self.err("DEPLOY_PREVIEW_FAILED", error.to_string()),
        };
        let prober = crate::daemon::deploy_probe::OverlayRpcProbe::new(peer_rpc_port);

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
        let storage_driver = match self.zfs_storage_driver().await {
            Ok(driver) => driver,
            Err(error) => return self.err("DEPLOY_APPLY_FAILED", error),
        };

        let agent = Arc::new(DeployAgent::new(
            active.mesh.store.clone(),
            self.namespace_locks.clone(),
            self.identity.machine_id.clone(),
            self.overlay_network_name(),
            self.overlay_dns_server(),
            storage_driver,
        ));
        let factory = DefaultDeploySessionFactory::new(
            agent,
            self.identity.machine_id.clone(),
            self.remote_control_port,
        );

        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return self.err("DEPLOY_APPLY_FAILED", error.to_string()),
        };
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::OverlayIssuanceCoordinator::new(
                active.mesh.store.clone(),
                self.reservations.clone(),
                self.identity.machine_id.clone(),
                peer_rpc_port,
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::OverlayChallengeReadiness::new(
                active.mesh.store.clone(),
                self.identity.machine_id.clone(),
                peer_rpc_port,
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::OverlayRpcProbe::new(peer_rpc_port);

        match apply_with_certificate_coordination(
            &active.mesh.store,
            &factory,
            &self.identity.machine_id,
            &manifest,
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
        )
        .await
        {
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
    let snapshot = store.load_deploy_snapshot(namespace).await?;
    let releases = snapshot.releases;
    let revisions = snapshot.revisions;
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
            return Err(PloyzError::operation(
                "deploy_export",
                format!(
                    "current release for service '{}' referenced missing revision '{}'",
                    release.service, release.release.primary_revision_hash
                ),
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(spec_json).map_err(|err| {
            PloyzError::operation(
                "deploy_export",
                format!(
                    "invalid stored spec for service '{}': {err}",
                    release.service
                ),
            )
        })?;
        if spec.name != release.service {
            return Err(PloyzError::operation(
                "deploy_export",
                format!(
                    "stored spec service '{}' did not match release service '{}'",
                    spec.name, release.service
                ),
            ));
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
    use ployz_store_api::{DeployCommit, DeployRepository, DeployRevisionUpsert};
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

    #[tokio::test]
    async fn export_manifest_includes_stored_volume_declarations() {
        let store = StoreDriver::memory();
        let namespace = Namespace("prod".into());
        let service = test_service();
        let revision_hash = "rev-db".to_string();
        let deploy_id = DeployId("deploy-1".into());

        store
            .record_service_revision(&DeployRevisionUpsert {
                revision: ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId("local".into()),
                    created_at: 1,
                },
            })
            .await
            .expect("seed revision");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
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
}
