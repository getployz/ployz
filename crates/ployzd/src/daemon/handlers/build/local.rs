use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{BuildLocalRequest, BuildOperationPayload, BuildResultPayload, DaemonPayload};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::ImageAvailabilityStore;
use ployz_types::model::{
    BuildLocation, BuildMethod, BuildOperationKind, ImageArtifact, ImageArtifactProvenance,
    ImageAvailabilityRecord, ImageDigest, ImagePlatform, ImagePresence, ImageRef, OperationStatus,
};
use ployz_types::time::now_unix_secs;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::daemon::DaemonState;

const BUILD_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUILD_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct BuildCommand {
    program: &'static str,
    args: Vec<String>,
}

#[derive(Debug)]
struct BuildCommandOutput {
    status_success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[async_trait]
trait BuildCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String>;
}

struct TokioBuildCommandRunner;

#[async_trait]
impl BuildCommandRunner for TokioBuildCommandRunner {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String> {
        let mut child = Command::new(command.program)
            .args(&command.args)
            .current_dir(current_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "spawn {} {}: {error}",
                    command.program,
                    command.args.join(" ")
                )
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("capture {} stdout", command.program))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("capture {} stderr", command.program))?;
        let stdout_task = tokio::spawn(read_tail(stdout, BUILD_OUTPUT_TAIL_BYTES));
        let stderr_task = tokio::spawn(read_tail(stderr, BUILD_OUTPUT_TAIL_BYTES));
        let (status_success, timed_out) = match timeout(BUILD_COMMAND_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => (status.success(), false),
            Ok(Err(error)) => {
                return Err(format!(
                    "wait for {} {}: {error}",
                    command.program,
                    command.args.join(" ")
                ));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (false, true)
            }
        };
        Ok(BuildCommandOutput {
            status_success,
            timed_out,
            stdout: stdout_task
                .await
                .map_err(|error| format!("read {} stdout: {error}", command.program))?,
            stderr: stderr_task
                .await
                .map_err(|error| format!("read {} stderr: {error}", command.program))?,
        })
    }
}

async fn read_tail<R>(mut reader: R, limit: usize) -> String
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tail.extend_from_slice(format!("\n[read error: {error}]").as_bytes());
                break;
            }
        };
        tail.extend_from_slice(&buffer[..read]);
        if tail.len() > limit {
            let excess = tail.len() - limit;
            tail.drain(0..excess);
            truncated = true;
        };
    }
    let body = String::from_utf8_lossy(&tail).to_string();
    if truncated {
        format!("[output truncated to last {limit} bytes]\n{body}")
    } else {
        body
    }
}

impl DaemonState {
    pub(crate) async fn handle_build_local(
        &self,
        request: &BuildLocalRequest,
    ) -> ployz_api::DaemonResponse {
        let runner = TokioBuildCommandRunner;
        self.handle_build_local_with_runner_and_backend(request, &runner, None)
            .await
    }

    async fn handle_build_local_with_runner_and_backend(
        &self,
        request: &BuildLocalRequest,
        runner: &dyn BuildCommandRunner,
        backend_result: Option<Result<Arc<dyn RuntimeImageBackend>, String>>,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "BUILD_LOCAL_INACTIVE",
            "build local requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        if request.push_target.is_some() || !request.distribute_targets.is_empty() {
            return self.err(
                "BUILD_LOCAL_IMAGE_MOVEMENT_UNSUPPORTED",
                "build local does not support push_target or distribute_targets in this release; run image push or image distribute explicitly after the build",
            );
        }
        let context_dir = PathBuf::from(&request.context_dir);
        if !context_dir.is_dir() {
            return self.err(
                "BUILD_LOCAL_CONTEXT_NOT_FOUND",
                format!(
                    "build context '{}' is not a directory",
                    context_dir.display()
                ),
            );
        }
        let image_name = normalize_build_image_name(&request.image_name);
        let request = if image_name == request.image_name {
            request.clone()
        } else {
            BuildLocalRequest {
                image_name,
                ..request.clone()
            }
        };

        let operation_store = self.build_operation_store();
        let mut operation = match operation_store.begin(
            BuildOperationKind::Local,
            request.method,
            BuildLocation::Local,
            "waiting for image build lock",
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("BUILD_LOCAL_OPERATION_FAILED", error),
        };

        let build_lock = self.local_build_lock(&request.image_name).await;
        let Ok(_build_guard) = build_lock.try_lock() else {
            let message = format!(
                "another local build for image '{}' is already running",
                request.image_name
            );
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_IMAGE_BUSY",
                message,
            );
        };
        let command = build_command(&request);
        if let Err(error) = operation_store.update_stage(&mut operation, "running build command") {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let backend = match match backend_result {
            Some(result) => result,
            None => self.runtime_image_backend().await,
        } {
            Ok(backend) => backend,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_RUNTIME_UNAVAILABLE",
                    error,
                );
            }
        };
        let output = match runner.run(&command, &context_dir).await {
            Ok(output) if output.status_success => output,
            Ok(output) => {
                let message = build_command_failure_message(&command, &output);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    message,
                );
            }
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    error,
                );
            }
        };

        if let Err(error) = operation_store.update_stage(&mut operation, "inspecting built image") {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let image = match backend.as_ref().inspect_image(&request.image_name).await {
            Ok(Some(image)) => image,
            Ok(None) => {
                let message = format!("built image '{}' was not found", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_IMAGE_NOT_FOUND",
                    message,
                );
            }
            Err(error) => {
                let message = format!("inspect built image '{}': {error}", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INSPECT_FAILED",
                    message,
                );
            }
        };
        let artifact = match build_image_artifact(&request, &image) {
            Ok(artifact) => artifact,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_MISSING_DIGEST",
                    error,
                );
            }
        };
        let record =
            present_build_availability(&self.identity.machine_id, artifact.clone(), &operation.id);

        if let Err(error) = operation_store.update_artifact(&mut operation, artifact.clone()) {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                None,
            );
        }
        if let Err(error) = active.mesh.store.upsert_image_availability(&record).await {
            let message = format!("record built image availability: {error}");
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_AVAILABILITY_FAILED",
                message,
                artifact,
                None,
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
        {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                Some(record),
            );
        }

        let message = render_build_result(&operation.id, &record, output);
        self.ok_with_payload(
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id,
                artifact,
                availability: Some(record),
            })),
        )
    }

    fn fail_build_local_operation(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_types::model::BuildOperationRecord,
        code: &str,
        message: String,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildOperation(BuildOperationPayload {
                operation: operation.clone(),
            })),
        )
    }

    fn fail_build_local_operation_with_result(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_types::model::BuildOperationRecord,
        code: &str,
        message: String,
        artifact: ImageArtifact,
        availability: Option<ImageAvailabilityRecord>,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id.clone(),
                artifact,
                availability,
            })),
        )
    }
}

fn build_command(request: &BuildLocalRequest) -> BuildCommand {
    let mut args = match request.method {
        BuildMethod::Dockerfile => vec!["build".into(), "-t".into(), request.image_name.clone()],
        BuildMethod::Railpack => vec!["build".into(), "--name".into(), request.image_name.clone()],
    };
    if let Some(platform) = &request.platform {
        args.push("--platform".into());
        args.push(format_platform(platform));
    }
    args.push(".".into());
    let program = match request.method {
        BuildMethod::Dockerfile => "docker",
        BuildMethod::Railpack => "railpack",
    };
    BuildCommand { program, args }
}

fn normalize_build_image_name(reference: &str) -> String {
    if reference.starts_with("sha256:") || image_reference_has_tag(reference) {
        reference.into()
    } else {
        format!("{reference}:latest")
    }
}

fn image_reference_has_tag(reference: &str) -> bool {
    let last_slash = reference.rfind('/');
    reference.rfind(':').is_some_and(|index| {
        index + 1 < reference.len() && last_slash.is_none_or(|slash| index > slash)
    })
}

fn format_platform(platform: &ImagePlatform) -> String {
    match platform.variant.as_deref() {
        Some(variant) => format!("{}/{}/{}", platform.os, platform.architecture, variant),
        None => format!("{}/{}", platform.os, platform.architecture),
    }
}

fn build_image_artifact(
    request: &BuildLocalRequest,
    image: &RuntimeImage,
) -> Result<ImageArtifact, String> {
    let digest = runtime_image_identity(&request.image_name, image)?;
    let now = now_unix_secs();
    Ok(ImageArtifact {
        image: image_ref_from_tag(&request.image_name, digest),
        platform: request.platform.clone().or_else(|| image.platform.clone()),
        provenance: ImageArtifactProvenance::Build {
            method: request.method,
            location: BuildLocation::Local,
            source_digest: None,
        },
        created_at: now,
    })
}

fn runtime_image_identity(reference: &str, image: &RuntimeImage) -> Result<ImageDigest, String> {
    let Some(id) = image.id.as_deref() else {
        return Err(RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string());
    };
    ImageDigest::try_new(id).map_err(|_| {
        RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string()
    })
}

fn present_build_availability(
    machine_id: &ployz_types::model::MachineId,
    artifact: ImageArtifact,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id: machine_id.clone(),
        digest: artifact.image.digest.clone(),
        presence: ImagePresence::Present {
            artifact,
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

fn image_ref_from_tag(reference: &str, digest: ImageDigest) -> ImageRef {
    let (repository, tag) = if reference.starts_with("sha256:") {
        (None, None)
    } else {
        let last_slash = reference.rfind('/');
        match reference
            .rfind(':')
            .filter(|index| last_slash.is_none_or(|slash| *index > slash))
        {
            Some(index) if index + 1 < reference.len() => {
                let (repository, tag) = reference.split_at(index);
                (Some(repository.to_string()), Some(tag[1..].to_string()))
            }
            _ => (Some(reference.to_string()), None),
        }
    };
    ImageRef {
        repository,
        tag,
        digest,
    }
}

fn build_command_failure_message(command: &BuildCommand, output: &BuildCommandOutput) -> String {
    let mut lines = vec![format!(
        "{} {} exited unsuccessfully",
        command.program,
        command.args.join(" ")
    )];
    if output.timed_out {
        lines.push(format!(
            "timed out after {} seconds",
            BUILD_COMMAND_TIMEOUT.as_secs()
        ));
    }
    if !output.stdout.trim().is_empty() {
        lines.push(format!("stdout: {}", output.stdout.trim()));
    }
    if !output.stderr.trim().is_empty() {
        lines.push(format!("stderr: {}", output.stderr.trim()));
    }
    lines.join("; ")
}

fn render_build_result(
    operation_id: &str,
    record: &ImageAvailabilityRecord,
    output: BuildCommandOutput,
) -> String {
    let mut message = format!(
        "{}  {}  {}  present",
        operation_id,
        record.machine_id,
        record.digest.as_str()
    );
    if !output.stdout.trim().is_empty() {
        message.push('\n');
        message.push_str(output.stdout.trim());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{ActiveMesh, RetainedSubnet};
    use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use ployz_orchestrator::Mesh;
    use ployz_orchestrator::mesh::driver::WireguardDriver;
    use ployz_runtime_api::{Identity, NoopRuntimeHandle};
    use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
    use ployz_types::model::{
        ImagePlatform, MachineId, MachineMembership, NetworkLifecycle, NetworkName, OverlayIp,
        PublicKey,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn digest(ch: char) -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", ch.to_string().repeat(64))).expect("digest")
    }

    struct FakeRunner {
        output: BuildCommandOutput,
    }

    #[async_trait]
    impl BuildCommandRunner for FakeRunner {
        async fn run(
            &self,
            _command: &BuildCommand,
            _current_dir: &Path,
        ) -> Result<BuildCommandOutput, String> {
            Ok(BuildCommandOutput {
                status_success: self.output.status_success,
                timed_out: self.output.timed_out,
                stdout: self.output.stdout.clone(),
                stderr: self.output.stderr.clone(),
            })
        }
    }

    struct FakeBackend {
        image: Option<RuntimeImage>,
    }

    #[async_trait]
    impl RuntimeImageBackend for FakeBackend {
        async fn inspect_image(
            &self,
            _reference: &str,
        ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
            Ok(self.image.clone())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    async fn active_state() -> DaemonState {
        let data_dir = temp_dir("ployz-build-local-test");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let identity = Identity::generate(MachineId("founder".into()), [11; 32]);
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let store = StoreDriver::memory();
        store
            .upsert_self_machine(&MachineMembership::seed(
                MachineId("founder".into()),
                PublicKey([11; 32]),
                OverlayIp("fd00::11".parse().expect("valid overlay")),
                None,
                Vec::new(),
            ))
            .await
            .expect("insert founder");
        let mesh = Mesh::new(
            WireguardDriver::memory(),
            store,
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
            nats_control: Box::new(NoopRuntimeHandle),
            zfs_transfer: Box::new(NoopRuntimeHandle),
            image_receiver: Box::new(NoopRuntimeHandle),
            image_receiver_bind_addr: None,
            gateway: Box::new(NoopRuntimeHandle),
            dns: Box::new(NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
        state
    }

    fn build_request(context_dir: &Path) -> BuildLocalRequest {
        BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: context_dir.display().to_string(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
        }
    }

    #[test]
    fn dockerfile_command_tags_platform_and_context() {
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            push_target: None,
            distribute_targets: Vec::new(),
        };

        let command = build_command(&request);

        assert_eq!(command.program, "docker");
        assert_eq!(
            command.args,
            vec![
                "build",
                "-t",
                "example/app:latest",
                "--platform",
                "linux/amd64",
                "."
            ]
        );
    }

    #[test]
    fn railpack_command_uses_name_and_platform_variant() {
        let request = BuildLocalRequest {
            method: BuildMethod::Railpack,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: Some("v8".into()),
            }),
            push_target: None,
            distribute_targets: Vec::new(),
        };

        let command = build_command(&request);

        assert_eq!(command.program, "railpack");
        assert_eq!(
            command.args,
            vec![
                "build",
                "--name",
                "example/app:latest",
                "--platform",
                "linux/arm64/v8",
                "."
            ]
        );
    }

    #[test]
    fn build_artifact_uses_image_id_and_build_provenance() {
        let image_digest = digest('a');
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
        };
        let image = RuntimeImage {
            reference: "example/app:latest".into(),
            id: Some(image_digest.as_str().into()),
            repo_digests: Vec::new(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            size_bytes: None,
        };

        let artifact = build_image_artifact(&request, &image).expect("artifact");

        assert_eq!(artifact.image.repository.as_deref(), Some("example/app"));
        assert_eq!(artifact.image.tag.as_deref(), Some("latest"));
        assert_eq!(artifact.image.digest, image_digest);
        assert_eq!(
            artifact.provenance,
            ImageArtifactProvenance::Build {
                method: BuildMethod::Dockerfile,
                location: BuildLocation::Local,
                source_digest: None
            }
        );
        assert_eq!(artifact.platform.expect("platform").architecture, "amd64");
    }

    #[test]
    fn present_availability_points_at_build_operation() {
        let artifact = ImageArtifact {
            image: ImageRef {
                repository: Some("example/app".into()),
                tag: Some("latest".into()),
                digest: digest('b'),
            },
            platform: None,
            provenance: ImageArtifactProvenance::Build {
                method: BuildMethod::Railpack,
                location: BuildLocation::Local,
                source_digest: None,
            },
            created_at: 1,
        };

        let record = present_build_availability(&MachineId("founder".into()), artifact, "build-1");

        assert_eq!(record.machine_id.0, "founder");
        let ImagePresence::Present {
            source_operation_id,
            ..
        } = record.presence
        else {
            panic!("expected present record");
        };
        assert_eq!(source_operation_id.as_deref(), Some("build-1"));
    }

    #[test]
    fn image_ref_parses_registry_port_without_confusing_tag() {
        let image = image_ref_from_tag("localhost:5000/example/app:latest", digest('c'));

        assert_eq!(
            image.repository.as_deref(),
            Some("localhost:5000/example/app")
        );
        assert_eq!(image.tag.as_deref(), Some("latest"));

        let untagged = image_ref_from_tag("localhost:5000/example/app", digest('d'));
        assert_eq!(
            untagged.repository.as_deref(),
            Some("localhost:5000/example/app")
        );
        assert_eq!(untagged.tag, None);
    }

    #[test]
    fn normalize_build_image_name_defaults_missing_tag_to_latest() {
        assert_eq!(
            normalize_build_image_name("example/app"),
            "example/app:latest"
        );
        assert_eq!(
            normalize_build_image_name("localhost:5000/example/app"),
            "localhost:5000/example/app:latest"
        );
        assert_eq!(
            normalize_build_image_name("localhost:5000/example/app:v1"),
            "localhost:5000/example/app:v1"
        );
    }

    #[tokio::test]
    async fn build_local_success_persists_operation_and_local_availability() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let image_digest = digest('e');
        let response = state
            .handle_build_local_with_runner_and_backend(
                &build_request(&context_dir),
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(image_digest.as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(response.ok, "{}", response.message);
        let Some(DaemonPayload::BuildResult(payload)) = response.payload else {
            panic!("expected build result payload");
        };
        assert_eq!(payload.artifact.digest(), &image_digest);
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(operation.status, OperationStatus::Succeeded);
        assert_eq!(
            operation.artifact.expect("artifact").digest(),
            &image_digest
        );
        let record = state
            .active
            .as_ref()
            .expect("active")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &image_digest)
            .await
            .expect("read availability")
            .expect("availability exists");
        assert!(matches!(record.presence, ImagePresence::Present { .. }));
    }

    #[tokio::test]
    async fn build_local_command_failure_marks_operation_failed_without_availability() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let response = state
            .handle_build_local_with_runner_and_backend(
                &build_request(&context_dir),
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: false,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: "bad dockerfile".into(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.ok);
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status, OperationStatus::Failed);
        assert!(
            state
                .active
                .as_ref()
                .expect("active")
                .mesh
                .store
                .list_image_availability()
                .await
                .expect("list availability")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn build_local_rejects_overlapping_build_for_same_image() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.image_name = "example/app".into();
        let build_lock = state.local_build_lock("example/app:latest").await;
        let _build_guard = build_lock.lock().await;

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_LOCAL_IMAGE_BUSY");
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status, OperationStatus::Failed);
    }
}
