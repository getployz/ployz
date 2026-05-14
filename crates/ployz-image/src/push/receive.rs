use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;

use crate::archive::reconstruct_received_archive;
use crate::registry::{
    REGISTRY_OPERATION_HEADER, REGISTRY_SESSION_HEADER, REGISTRY_SOURCE_MACHINE_HEADER,
    validate_repository,
};
use crate::response::ImageServicePayload;
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImagePresence,
    ImageReceiveSessionPayload, ImageReceiveSessionRequest, ImageReceivedImportPayload,
    ImageReceivedImportRequest,
};
use ployz_runtime_api::{ImageArchiveReader, RuntimeImageBackend};
use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore};
use ployz_time::now_unix_secs;

use super::{ImageService, cleanup_image_work_dir, default_receive_repository, image_ref_from_tag};

impl ImageService {
    pub async fn handle_image_receive_session(
        &self,
        request: &ImageReceiveSessionRequest,
    ) -> super::ImageServiceResponse {
        let Some(bind_addr) = self.receiver_bind_addr else {
            return self.err(
                "IMAGE_RECEIVER_INACTIVE",
                "image receiver listener is not running",
            );
        };
        let repository = request
            .repository
            .clone()
            .unwrap_or_else(|| default_receive_repository(&request.operation_id));
        if let Err(error) = validate_repository(&repository) {
            return self.err("IMAGE_RECEIVER_INVALID_REPOSITORY", error.to_string());
        }
        let machines = match self.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "IMAGE_RECEIVER_SOURCE_LOOKUP_FAILED",
                    format!("list machines for image receive source validation: {error}"),
                );
            }
        };
        if !machines
            .iter()
            .any(|machine| machine.id == request.source_machine)
        {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_UNKNOWN",
                format!(
                    "source machine '{}' is not a cluster member",
                    request.source_machine
                ),
            );
        }
        if bind_addr.ip().is_loopback() && request.source_machine != self.local_machine {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_NOT_LOCAL",
                format!(
                    "image receiver is bound to loopback; source machine '{}' must match local machine '{}'",
                    request.source_machine, self.local_machine
                ),
            );
        }
        let session = self
            .registry
            .register_session(
                &request.operation_id,
                request.source_machine.clone(),
                repository.clone(),
            )
            .await;
        let mut headers = BTreeMap::new();
        headers.insert(
            REGISTRY_OPERATION_HEADER.to_string(),
            session.operation_id.clone(),
        );
        headers.insert(
            REGISTRY_SOURCE_MACHINE_HEADER.to_string(),
            session.source_machine.as_str().to_string(),
        );
        headers.insert(REGISTRY_SESSION_HEADER.to_string(), session.token.clone());
        let payload = ImageReceiveSessionPayload {
            target_machine: self.local_machine.clone(),
            endpoint: receiver_endpoint(bind_addr, &session.repository),
            token: session.token,
            expires_at_unix_secs: session.expires_at_unix_secs,
            headers,
        };

        self.ok_with_payload(
            "image receive session created",
            Some(ImageServicePayload::ImageReceiveSession(payload)),
        )
    }

    pub async fn handle_image_received_import_with_backend(
        &self,
        request: &ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> super::ImageServiceResponse {
        if let Err(error) = crate::operations::validate_operation_id(&request.operation_id) {
            return self.err("IMAGE_RECEIVED_IMPORT_INVALID_OPERATION", error);
        }
        if let Err(error) = validate_repository(&request.repository) {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_INVALID_REPOSITORY",
                error.to_string(),
            );
        }
        let archive_path = self
            .data_dir
            .join("image-import")
            .join(request.operation_id.clone())
            .join("received.tar");
        let import_dir = archive_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                self.data_dir
                    .join("image-import")
                    .join(&request.operation_id)
            });
        let archive_path = match reconstruct_received_archive(
            &self.registry,
            &request.repository,
            &request.reference,
            &request.repo_tags,
            archive_path,
        )
        .await
        {
            Ok(path) => path,
            Err(error) => {
                cleanup_image_work_dir(&import_dir).await;
                return self.err(
                    "IMAGE_RECEIVED_IMPORT_RECONSTRUCT_FAILED",
                    error.to_string(),
                );
            }
        };
        let archive = match tokio::fs::File::open(&archive_path).await {
            Ok(file) => Box::pin(file) as ImageArchiveReader,
            Err(error) => {
                cleanup_image_work_dir(&import_dir).await;
                return self.err(
                    "IMAGE_RECEIVED_IMPORT_ARCHIVE_OPEN_FAILED",
                    format!(
                        "open received image archive '{}': {error}",
                        archive_path.display()
                    ),
                );
            }
        };
        if let Err(error) = backend.import_image_archive(archive).await {
            cleanup_image_work_dir(&import_dir).await;
            return self.err(
                "IMAGE_RECEIVED_IMPORT_RUNTIME_FAILED",
                format!("import received image archive: {error}"),
            );
        }
        cleanup_image_work_dir(&import_dir).await;
        let verify_reference = request
            .repo_tags
            .first()
            .map(String::as_str)
            .unwrap_or_else(|| request.expected_digest.as_str());
        if let Err(error) = backend
            .verify_image_digest(verify_reference, &request.expected_digest)
            .await
        {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_VERIFY_FAILED",
                format!("verify imported image '{verify_reference}': {error}"),
            );
        }
        let now = now_unix_secs();
        let record = ImageAvailabilityRecord {
            machine_id: self.local_machine.clone(),
            digest: request.expected_digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: image_ref_from_tag(verify_reference, request.expected_digest.clone()),
                    platform: request.platform.clone(),
                    provenance: ImageArtifactProvenance::External {
                        source: Some(format!("image distribute from {}", request.source_machine)),
                    },
                    created_at: now,
                },
                recorded_at: now,
                source_operation_id: Some(request.operation_id.clone()),
            },
            updated_at: now,
        };
        if let Err(error) = self.store.upsert_image_availability(&record).await {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_AVAILABILITY_FAILED",
                format!("record imported image availability: {error}"),
            );
        }
        self.ok_with_payload(
            format!(
                "image {} imported on {}",
                request.expected_digest.as_str(),
                self.local_machine
            ),
            Some(ImageServicePayload::ImageReceivedImport(
                ImageReceivedImportPayload {
                    target_machine: self.local_machine.clone(),
                    record,
                },
            )),
        )
    }
}

fn receiver_endpoint(bind_addr: SocketAddr, repository: &str) -> String {
    format!("http://{bind_addr}/v2/{repository}")
}
