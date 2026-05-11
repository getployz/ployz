use std::sync::Arc;

use ployz_api::{DaemonPayload, ImageInspectPayload, ImageInspectRequest};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::ImageAvailabilityStore;
use ployz_types::model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest,
    ImageOperationKind, ImageOperationTargetOutcome, ImagePresence, ImageRef, MachineId,
    OperationStatus,
};
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_image_inspect(
        &self,
        request: &ImageInspectRequest,
    ) -> ployz_api::DaemonResponse {
        let Some(_) = self.active.as_ref() else {
            return self.err("NO_ACTIVE_MESH", "image inspect requires a running mesh");
        };
        if let Err(error) = inspect_target_machine(&self.identity.machine_id, request) {
            return self.err(error.code, error.message);
        }

        self.handle_image_inspect_with_backend(request, self.runtime_image_backend().await)
            .await
    }

    async fn handle_image_inspect_with_backend(
        &self,
        request: &ImageInspectRequest,
        backend_result: Result<Arc<dyn RuntimeImageBackend>, String>,
    ) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_ACTIVE_MESH", "image inspect requires a running mesh");
        };
        let target_machine = match inspect_target_machine(&self.identity.machine_id, request) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err(error.code, error.message),
        };
        let reference = image_inspect_reference(request);
        let operation_store = self.image_operation_store();
        let mut operation = match operation_store.begin(
            ImageOperationKind::Inspect,
            "inspecting",
            Some(request.digest.clone()),
            None,
            vec![target_machine.clone()],
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("IMAGE_INSPECT_OPERATION_FAILED", error),
        };

        let record = match backend_result {
            Ok(backend) => {
                inspect_runtime_image_record(
                    backend.as_ref(),
                    target_machine.clone(),
                    request.digest.clone(),
                    &reference,
                    &operation.id,
                )
                .await
            }
            Err(error) => failed_image_record(
                target_machine.clone(),
                request.digest.clone(),
                error,
                Some(operation.id.clone()),
            ),
        };

        if let Err(error) = active.mesh.store.upsert_image_availability(&record).await {
            let message = error.to_string();
            let _ = operation_store.update_target(
                &mut operation,
                image_operation_target(
                    target_machine,
                    OperationStatus::Failed,
                    Some(message.clone()),
                ),
            );
            let _ = operation_store.update_status(
                &mut operation,
                OperationStatus::Failed,
                Some(message.clone()),
            );
            return self.err_with_payload(
                "IMAGE_INSPECT_STORE_FAILED",
                message,
                Some(image_inspect_payload(operation.id, record)),
            );
        }

        let (status, last_error) = image_operation_result(&record);
        if let Err(error) = operation_store.update_target(
            &mut operation,
            image_operation_target(target_machine.clone(), status, last_error.clone()),
        ) {
            return self.err_with_payload(
                "IMAGE_INSPECT_OPERATION_FAILED",
                error,
                Some(image_inspect_payload(operation.id, record)),
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, status, last_error.clone())
        {
            return self.err_with_payload(
                "IMAGE_INSPECT_OPERATION_FAILED",
                error,
                Some(image_inspect_payload(operation.id, record)),
            );
        }

        match &record.presence {
            ImagePresence::Present { .. } => self.ok_with_payload(
                render_image_inspect_record(&operation.id, &record),
                Some(image_inspect_payload(operation.id, record)),
            ),
            ImagePresence::Absent { .. } => self.ok_with_payload(
                render_image_inspect_record(&operation.id, &record),
                Some(image_inspect_payload(operation.id, record)),
            ),
            ImagePresence::Failed { reason, .. } => self.err_with_payload(
                "IMAGE_INSPECT_FAILED",
                format!("{}  {}", operation.id, reason),
                Some(image_inspect_payload(operation.id, record)),
            ),
            ImagePresence::Transferring { .. } => self.err_with_payload(
                "IMAGE_INSPECT_FAILED",
                "image inspect produced an invalid transferring record",
                Some(image_inspect_payload(operation.id, record)),
            ),
        }
    }
}

async fn inspect_runtime_image_record(
    backend: &dyn RuntimeImageBackend,
    machine_id: MachineId,
    digest: ImageDigest,
    reference: &str,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    match backend.verify_image_digest(reference, &digest).await {
        Ok(image) => present_image_record(machine_id, digest, reference, image, operation_id),
        Err(RuntimeImageError::NotFound { .. }) => absent_image_record(machine_id, digest),
        Err(
            error @ (RuntimeImageError::UnsupportedCapability { .. }
            | RuntimeImageError::MissingDigest { .. }
            | RuntimeImageError::DigestMismatch { .. }
            | RuntimeImageError::Backend { .. }),
        ) => failed_image_record(
            machine_id,
            digest,
            error.to_string(),
            Some(operation_id.into()),
        ),
    }
}

fn present_image_record(
    machine_id: MachineId,
    digest: ImageDigest,
    reference: &str,
    image: RuntimeImage,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest: digest.clone(),
        presence: ImagePresence::Present {
            artifact: ImageArtifact {
                image: ImageRef::repository_digest(image.reference, None, digest),
                platform: image.platform,
                provenance: ImageArtifactProvenance::External {
                    source: Some(reference.into()),
                },
                created_at: now,
            },
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

fn absent_image_record(machine_id: MachineId, digest: ImageDigest) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest,
        presence: ImagePresence::Absent { observed_at: now },
        updated_at: now,
    }
}

fn failed_image_record(
    machine_id: MachineId,
    digest: ImageDigest,
    reason: String,
    operation_id: Option<String>,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest,
        presence: ImagePresence::Failed {
            reason,
            failed_at: now,
            operation_id,
        },
        updated_at: now,
    }
}

fn inspect_target_machine(
    local_machine: &MachineId,
    request: &ImageInspectRequest,
) -> Result<MachineId, InspectTargetError> {
    match request.machines.as_slice() {
        [] => Ok(local_machine.clone()),
        [machine_id] if machine_id == local_machine => Ok(machine_id.clone()),
        [machine_id] => Err(InspectTargetError {
            code: "IMAGE_INSPECT_REMOTE_UNSUPPORTED",
            message: format!(
                "image inspect only supports local machine '{}' in this release, got '{}'",
                local_machine.as_str(),
                machine_id.as_str()
            ),
        }),
        [first, second, rest @ ..] => Err(InspectTargetError {
            code: "IMAGE_INSPECT_REMOTE_UNSUPPORTED",
            message: format!(
                "image inspect supports one local target in this release, got '{}', '{}' and {} more",
                first.as_str(),
                second.as_str(),
                rest.len()
            ),
        }),
    }
}

fn image_inspect_reference(request: &ImageInspectRequest) -> String {
    request
        .reference
        .as_ref()
        .cloned()
        .unwrap_or_else(|| request.digest.as_str().into())
}

fn image_operation_result(record: &ImageAvailabilityRecord) -> (OperationStatus, Option<String>) {
    match &record.presence {
        ImagePresence::Present { .. } | ImagePresence::Absent { .. } => {
            (OperationStatus::Succeeded, None)
        }
        ImagePresence::Failed { reason, .. } => (OperationStatus::Failed, Some(reason.clone())),
        ImagePresence::Transferring { .. } => (
            OperationStatus::Failed,
            Some("image inspect produced an invalid transferring record".into()),
        ),
    }
}

fn image_operation_target(
    machine_id: MachineId,
    status: OperationStatus,
    last_error: Option<String>,
) -> ImageOperationTargetOutcome {
    ImageOperationTargetOutcome {
        machine_id,
        status,
        bytes_transferred: None,
        last_error,
    }
}

fn image_inspect_payload(operation_id: String, record: ImageAvailabilityRecord) -> DaemonPayload {
    DaemonPayload::ImageInspect(ImageInspectPayload {
        operation_id,
        records: vec![record],
    })
}

fn render_image_inspect_record(operation_id: &str, record: &ImageAvailabilityRecord) -> String {
    format!(
        "{}  {}  {}  {}",
        operation_id,
        record.machine_id.as_str(),
        record.digest.as_str(),
        image_presence_label(record)
    )
}

fn image_presence_label(record: &ImageAvailabilityRecord) -> &'static str {
    match record.presence {
        ImagePresence::Present { .. } => "present",
        ImagePresence::Absent { .. } => "absent",
        ImagePresence::Transferring { .. } => "transferring",
        ImagePresence::Failed { .. } => "failed",
    }
}

#[derive(Debug)]
struct InspectTargetError {
    code: &'static str,
    message: String,
}

#[cfg(test)]
mod tests {
    use crate::daemon::{ActiveMesh, RetainedSubnet};
    use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use ipnet::Ipv4Net;
    use ployz_api::DaemonPayload;
    use ployz_orchestrator::Mesh;
    use ployz_orchestrator::mesh::driver::WireguardDriver;
    use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
    use ployz_runtime_api::Identity;
    use ployz_store_api::memory::{MemoryService, MemoryStore};
    use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
    use ployz_types::model::{
        ImagePlatform, ImagePresence, MachineLifecycle, MachineMembership, MachineTopology,
        NetworkLifecycle, NetworkName, OverlayIp, RegionRole, StorageParticipation,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct FakeImageBackend {
        outcome: FakeImageOutcome,
        inspected_references: Arc<Mutex<Vec<String>>>,
    }

    enum FakeImageOutcome {
        Found(RuntimeImage),
        Missing,
        BackendError,
    }

    #[async_trait::async_trait]
    impl RuntimeImageBackend for FakeImageBackend {
        async fn inspect_image(
            &self,
            reference: &str,
        ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
            self.inspected_references
                .lock()
                .expect("reference lock")
                .push(reference.into());
            match &self.outcome {
                FakeImageOutcome::Found(image) => Ok(Some(image.clone())),
                FakeImageOutcome::Missing => Ok(None),
                FakeImageOutcome::BackendError => Err(RuntimeImageError::backend(
                    "inspect image",
                    format!("cannot inspect {reference}"),
                )),
            }
        }
    }

    fn digest(hex: char) -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn fake_backend(outcome: FakeImageOutcome) -> Arc<FakeImageBackend> {
        Arc::new(FakeImageBackend {
            outcome,
            inspected_references: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn request(machines: Vec<MachineId>) -> ImageInspectRequest {
        ImageInspectRequest {
            digest: digest('a'),
            reference: Some("example/app:latest".into()),
            machines,
        }
    }

    #[tokio::test]
    async fn inspect_present_records_artifact_with_platform() {
        let digest = digest('a');
        let backend = fake_backend(FakeImageOutcome::Found(RuntimeImage {
            reference: "example/app:latest".into(),
            id: Some("sha256:config".into()),
            repo_digests: vec![digest.clone()],
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            size_bytes: Some(42),
        }));

        let record = inspect_runtime_image_record(
            backend.as_ref(),
            MachineId::new("founder"),
            digest.clone(),
            "example/app:latest",
            "op-1",
        )
        .await;

        assert_eq!(record.machine_id, MachineId::new("founder"));
        assert_eq!(record.digest, digest);
        let ImagePresence::Present {
            artifact,
            source_operation_id,
            ..
        } = record.presence
        else {
            panic!("expected present record");
        };
        assert_eq!(artifact.image.repository(), Some("example/app:latest"));
        assert_eq!(artifact.platform.expect("platform").architecture, "amd64");
        assert_eq!(source_operation_id.as_deref(), Some("op-1"));
    }

    #[tokio::test]
    async fn inspect_missing_records_absent() {
        let digest = digest('a');
        let backend = fake_backend(FakeImageOutcome::Missing);

        let record = inspect_runtime_image_record(
            backend.as_ref(),
            MachineId::new("founder"),
            digest,
            "example/app:latest",
            "op-1",
        )
        .await;

        assert!(matches!(record.presence, ImagePresence::Absent { .. }));
    }

    #[tokio::test]
    async fn inspect_backend_error_records_failed() {
        let digest = digest('a');
        let backend = fake_backend(FakeImageOutcome::BackendError);

        let record = inspect_runtime_image_record(
            backend.as_ref(),
            MachineId::new("founder"),
            digest,
            "example/app:latest",
            "op-1",
        )
        .await;

        let ImagePresence::Failed {
            reason,
            operation_id,
            ..
        } = record.presence
        else {
            panic!("expected failed record");
        };
        assert!(reason.contains("cannot inspect example/app:latest"));
        assert_eq!(operation_id.as_deref(), Some("op-1"));
    }

    #[tokio::test]
    async fn inspect_digest_mismatch_records_failed() {
        let backend = fake_backend(FakeImageOutcome::Found(RuntimeImage {
            reference: "example/app:latest".into(),
            id: None,
            repo_digests: vec![digest('b')],
            platform: None,
            size_bytes: None,
        }));

        let record = inspect_runtime_image_record(
            backend.as_ref(),
            MachineId::new("founder"),
            digest('a'),
            "example/app:latest",
            "op-1",
        )
        .await;

        let ImagePresence::Failed { reason, .. } = record.presence else {
            panic!("expected failed record");
        };
        assert!(reason.contains("digest mismatch"));
    }

    #[test]
    fn inspect_defaults_to_local_machine() {
        let local = MachineId::new("founder");
        let target = inspect_target_machine(&local, &request(Vec::new())).expect("local target");
        assert_eq!(target, local);
    }

    #[test]
    fn inspect_rejects_remote_machine() {
        let local = MachineId::new("founder");
        let error = inspect_target_machine(&local, &request(vec![MachineId::new("worker")]))
            .expect_err("remote unsupported");
        assert_eq!(error.code, "IMAGE_INSPECT_REMOTE_UNSUPPORTED");
    }

    #[test]
    fn inspect_defaults_reference_to_digest() {
        let request = ImageInspectRequest {
            digest: digest('a'),
            reference: None,
            machines: Vec::new(),
        };

        assert_eq!(image_inspect_reference(&request), request.digest.as_str());
    }

    #[tokio::test]
    async fn handler_writes_present_record_and_preserves_unrelated_records() {
        let (state, store) = make_active_state().await;
        let unrelated_digest = digest('b');
        let unrelated = absent_image_record(MachineId::new("worker"), unrelated_digest.clone());
        store
            .upsert_image_availability(&unrelated)
            .await
            .expect("upsert unrelated");
        let inspected_digest = digest('a');
        let backend = fake_backend(FakeImageOutcome::Found(RuntimeImage {
            reference: "example/app:latest".into(),
            id: Some("sha256:config".into()),
            repo_digests: vec![inspected_digest.clone()],
            platform: None,
            size_bytes: None,
        }));

        let response = state
            .handle_image_inspect_with_backend(
                &ImageInspectRequest {
                    digest: inspected_digest.clone(),
                    reference: Some("example/app:latest".into()),
                    machines: Vec::new(),
                },
                Ok(backend),
            )
            .await;

        assert!(response.ok, "response: {response:?}");
        let Some(DaemonPayload::ImageInspect(payload)) = response.payload else {
            panic!("expected image inspect payload");
        };
        assert!(!payload.operation_id.is_empty());
        assert!(response.message.contains(&payload.operation_id));
        assert_eq!(payload.records.len(), 1);
        assert_eq!(payload.records[0].machine_id, MachineId::new("founder"));
        assert_eq!(payload.records[0].digest, inspected_digest);
        assert!(matches!(
            payload.records[0].presence,
            ImagePresence::Present { .. }
        ));

        let stored = store
            .get_image_availability(&MachineId::new("founder"), &inspected_digest)
            .await
            .expect("get inspected")
            .expect("inspected record");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let preserved = store
            .get_image_availability(&MachineId::new("worker"), &unrelated_digest)
            .await
            .expect("get unrelated")
            .expect("unrelated record");
        assert_eq!(preserved, unrelated);
        let operations = state
            .image_operation_store()
            .list()
            .expect("list image operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Succeeded);
    }

    #[tokio::test]
    async fn handler_rejects_remote_target_before_mutating_state() {
        let (state, store) = make_active_state().await;
        let backend = fake_backend(FakeImageOutcome::Missing);

        let response = state
            .handle_image_inspect_with_backend(
                &ImageInspectRequest {
                    digest: digest('a'),
                    reference: Some("example/app:latest".into()),
                    machines: vec![MachineId::new("worker")],
                },
                Ok(backend),
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_INSPECT_REMOTE_UNSUPPORTED");
        assert!(
            store
                .list_image_availability()
                .await
                .expect("list availability")
                .is_empty()
        );
        assert!(
            state
                .image_operation_store()
                .list()
                .expect("list image operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn handler_uses_digest_as_default_reference() {
        let (state, _) = make_active_state().await;
        let inspected_digest = digest('a');
        let backend = fake_backend(FakeImageOutcome::Missing);
        let inspected_references = backend.inspected_references.clone();

        let response = state
            .handle_image_inspect_with_backend(
                &ImageInspectRequest {
                    digest: inspected_digest.clone(),
                    reference: None,
                    machines: Vec::new(),
                },
                Ok(backend),
            )
            .await;

        assert!(response.ok, "missing image should be an absent observation");
        assert_eq!(
            inspected_references
                .lock()
                .expect("reference lock")
                .as_slice(),
            &[inspected_digest.as_str().to_string()]
        );
    }

    #[tokio::test]
    async fn handler_records_backend_error_as_failed_availability() {
        let (state, store) = make_active_state().await;
        let inspected_digest = digest('a');
        let response = state
            .handle_image_inspect_with_backend(
                &ImageInspectRequest {
                    digest: inspected_digest.clone(),
                    reference: Some("example/app:latest".into()),
                    machines: Vec::new(),
                },
                Ok(fake_backend(FakeImageOutcome::BackendError)),
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_INSPECT_FAILED");
        let Some(DaemonPayload::ImageInspect(payload)) = response.payload else {
            panic!("expected image inspect payload");
        };
        assert!(response.message.contains(&payload.operation_id));
        let stored = store
            .get_image_availability(&MachineId::new("founder"), &inspected_digest)
            .await
            .expect("get failed availability")
            .expect("failed availability");
        assert!(matches!(stored.presence, ImagePresence::Failed { .. }));
        assert_eq!(payload.records, vec![stored]);
    }

    #[test]
    fn render_inspect_line_includes_machine_digest_and_presence() {
        let digest = digest('a');
        let record = absent_image_record(MachineId::new("founder"), digest.clone());

        let line = render_image_inspect_record("op-1", &record);

        assert!(line.contains("op-1"));
        assert!(line.contains("founder"));
        assert!(line.contains(digest.as_str()));
        assert!(line.contains("absent"));
    }

    async fn make_active_state() -> (DaemonState, Arc<MemoryStore>) {
        let identity = Identity::generate(MachineId::new("founder"), [1; 32]);
        let founder_subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let data_dir = unique_temp_dir("ployz-image-inspect-state");
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            founder_subnet,
        );
        config.lifecycle = NetworkLifecycle::Running;
        config
            .save(&NetworkConfig::path(&data_dir, "alpha"))
            .expect("save config");

        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        store
            .upsert_self_machine(&MachineMembership {
                id: identity.machine_id.clone(),
                public_key: identity.public_key.clone(),
                subnet: Some("10.210.0.0/24".parse().expect("valid subnet")),
                overlay_ip: OverlayIp("fd00::1".parse().expect("valid ip")),
                topology: MachineTopology::local(),
                lifecycle: MachineLifecycle::Active,
                region_role: RegionRole::HomeData,
                bridge_ip: None,
                endpoints: Vec::new(),
                storage_role: StorageParticipation::default_authority().into(),
                created_at: now_unix_secs(),
                updated_at: now_unix_secs(),
                labels: BTreeMap::new(),
            })
            .await
            .expect("upsert founder");

        let mesh = Mesh::new(
            WireguardDriver::memory_with(Arc::new(MemoryWireGuard::new())),
            StoreDriver::memory_with(store.clone(), service),
            None,
            identity.machine_id.clone(),
            51820,
        );
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );
        state.active = Some(ActiveMesh {
            retained_subnet: RetainedSubnet::from_running_config(config.subnet),
            config,
            mesh,
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver_bind_addr: None,
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
        (state, store)
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}
