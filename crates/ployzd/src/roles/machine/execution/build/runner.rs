use super::ValidatedOciLayout;
use super::oci::validate_oci_layout;
use super::plan::{
    BuildAdapterToolchain, BuildExecutionPlan, BuildToolchain, lower_build_adapter,
    toolchain_for_platform,
};
use super::source::checkout_git_source;
use crate::roles::machine::protocol::MachineBuildLogFrame;
use ployz_core::build::{BuildAdapter, GitSource};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::{
    BuildLogChunk, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
    MAX_BUILD_LOG_CHUNK_BYTES,
};
use ployz_nats::service_runtime::NatsClient;
use ployz_nats::subjects::machine_build_log;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

const CACHE_VOLUME: &str = "ployz-buildkit-cache-v1";
const BUILDER_LABEL: &str = "com.getployz.build=true";
const BUILD_LOG_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const COMMAND_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const BUILDKIT_READY_ATTEMPTS: usize = 30;
const BUILDKIT_READY_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct DockerBuildExecutor {
    machine_id: MachineId,
    client: NatsClient,
    workspace_root: PathBuf,
}

impl DockerBuildExecutor {
    #[must_use]
    pub fn new(machine_id: MachineId, client: NatsClient, workspace_root: PathBuf) -> Self {
        Self {
            machine_id,
            client,
            workspace_root,
        }
    }

    pub async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        let filter = format!("label={BUILDER_LABEL}");
        let output = run_bounded(
            "docker",
            ["ps", "-aq", "--filter", filter.as_str()],
            DOCKER_COMMAND_TIMEOUT,
        )
        .await?;
        require_success("list orphaned builders", &output)?;
        for id in String::from_utf8_lossy(&output.stdout).lines() {
            remove_builder(id).await?;
        }
        remove_workspace_tree(&self.workspace_root).await?;
        Ok(())
    }

    pub async fn force_cleanup(
        &self,
        operation_id: &OperationId,
        platform: &OciPlatform,
    ) -> Result<(), BuildExecutionError> {
        let builder = builder_name(operation_id, platform);
        let builder_result = remove_builder(&builder).await;
        let workspace_result = remove_workspace_tree(&self.workspace(operation_id, platform)).await;
        builder_result.and(workspace_result)
    }

    pub async fn execute(
        &self,
        operation_id: &OperationId,
        source: &GitSource,
        adapter: &BuildAdapter,
        platform: &OciPlatform,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        let workspace = self.workspace(operation_id, platform);
        prepare_private_directory(&self.workspace_root).await?;
        let Some(operation_workspace) = workspace.parent() else {
            return Err(infrastructure(
                "prepare build workspace",
                "operation workspace has no parent",
            ));
        };
        prepare_private_directory(operation_workspace).await?;
        prepare_workspace(&workspace).await?;
        let result = self
            .execute_in_workspace(
                operation_id,
                source,
                adapter,
                platform,
                &workspace,
                &mut cancelled,
            )
            .await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        result
    }

    async fn execute_in_workspace(
        &self,
        operation_id: &OperationId,
        source: &GitSource,
        adapter: &BuildAdapter,
        platform: &OciPlatform,
        workspace: &Path,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        check_cancelled(cancelled)?;
        let checkout = checkout_git_source(source, workspace)
            .await
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::SourceFetchFailed {
                    message: failure(error.to_string()),
                })
            })?;
        let toolchain = toolchain_for_platform(platform, adapter).map_err(|error| {
            platform_failure(BuildPlatformFailure::AdapterFailed {
                message: failure(error.to_string()),
            })
        })?;
        verify_helper(&toolchain).await?;
        let plan = lower_build_adapter(&checkout, adapter, platform, workspace, &toolchain)
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::AdapterFailed {
                    message: failure(error.to_string()),
                })
            })?;
        if let Some(prepare) = &plan.prepare {
            run_prepare(prepare, source, cancelled).await?;
        }
        tokio::fs::create_dir_all(&plan.oci_layout)
            .await
            .map_err(|error| infrastructure("create OCI output", error.to_string()))?;

        pull_exact_image(
            &toolchain.buildkit_reference,
            &toolchain.buildkit_manifest_digest,
            platform,
            PinnedImageKind::Buildkit,
        )
        .await?;
        if let BuildAdapterToolchain::Railpack {
            frontend_reference,
            frontend_manifest_digest,
            ..
        } = &toolchain.adapter
        {
            pull_exact_image(
                frontend_reference,
                frontend_manifest_digest,
                platform,
                PinnedImageKind::Frontend,
            )
            .await?;
        }
        let volume = run_bounded(
            "docker",
            ["volume", "create", CACHE_VOLUME],
            DOCKER_COMMAND_TIMEOUT,
        )
        .await?;
        require_success("ensure BuildKit cache volume", &volume)?;

        let builder = builder_name(operation_id, platform);
        remove_builder(&builder).await?;
        create_builder(
            &builder,
            operation_id,
            platform,
            workspace,
            &toolchain.buildkit_reference,
        )
        .await?;
        let build = async {
            let started = run_bounded(
                "docker",
                ["start", builder.as_str()],
                DOCKER_COMMAND_TIMEOUT,
            )
            .await?;
            require_success("start BuildKit", &started)?;
            await_buildkit(&builder, cancelled).await?;
            run_buildctl(
                &builder,
                &plan,
                source,
                cancelled,
                &self.client,
                &self.machine_id,
                operation_id,
                platform,
            )
            .await
        }
        .await;
        let cleanup = remove_builder(&builder).await;
        let logs = match (build, cleanup) {
            (Ok(logs), Ok(())) => logs,
            (Err(error), _) | (Ok(_), Err(error)) => return Err(error),
        };
        let layout_path = plan.oci_layout;
        let platform_for_validation = platform.clone();
        let layout = tokio::task::spawn_blocking(move || {
            validate_oci_layout(&layout_path, &platform_for_validation)
        })
        .await
        .map_err(|error| infrastructure("join OCI validation", error.to_string()))?
        .map_err(|error| {
            platform_failure(BuildPlatformFailure::ImagePushFailed {
                message: failure(error.to_string()),
            })
        })?;
        Ok(BuildExecutionResult {
            layout,
            verified_commit: checkout.commit().clone(),
            toolchain: toolchain.evidence(),
            final_log_sequence: logs.final_sequence,
            omitted_log_bytes: logs.omitted_bytes,
        })
    }

    fn workspace(&self, operation_id: &OperationId, platform: &OciPlatform) -> PathBuf {
        self.workspace_root
            .join(operation_id.as_str())
            .join(format!("{}-{}", platform.os(), platform.architecture()))
    }
}

pub struct BuildExecutionResult {
    pub layout: ValidatedOciLayout,
    pub verified_commit: ployz_core::build::VerifiedGitCommit,
    pub toolchain: BuildToolchainEvidence,
    pub final_log_sequence: u64,
    pub omitted_log_bytes: u64,
}

async fn prepare_workspace(path: &Path) -> Result<(), BuildExecutionError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| infrastructure("inspect build workspace", error.to_string()))?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| infrastructure("remove old build workspace", error.to_string()))?;
    }
    prepare_private_directory(path).await
}

async fn prepare_private_directory(path: &Path) -> Result<(), BuildExecutionError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| infrastructure("create build workspace", error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| {
                infrastructure("set build workspace permissions", error.to_string())
            })?;
    }
    Ok(())
}

async fn remove_workspace_tree(path: &Path) -> Result<(), BuildExecutionError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| infrastructure("inspect build workspace", error.to_string()))?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| infrastructure("remove build workspace", error.to_string()))?;
    }
    Ok(())
}

async fn verify_helper(toolchain: &BuildToolchain) -> Result<(), BuildExecutionError> {
    let BuildAdapterToolchain::Railpack {
        helper_path,
        helper_sha256,
        ..
    } = &toolchain.adapter
    else {
        return Ok(());
    };
    let path = helper_path.clone();
    let actual = tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|error| infrastructure("join Railpack verification", error.to_string()))??;
    if actual == helper_sha256.as_str() {
        return Ok(());
    }
    let actual = ployz_core::install::InstallSha256Digest::try_new(actual)
        .map_err(|error| infrastructure("parse Railpack helper digest", error.to_string()))?;
    Err(platform_failure(
        BuildPlatformFailure::HelperDigestMismatch {
            expected: helper_sha256.clone(),
            actual,
        },
    ))
}

fn sha256_file(path: &Path) -> Result<String, BuildExecutionError> {
    use std::io::Read;
    let file = std::fs::File::open(path)
        .map_err(|error| infrastructure("open Railpack helper", error.to_string()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| infrastructure("read Railpack helper", error.to_string()))?;
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            return Err(infrastructure(
                "read Railpack helper",
                "reader returned an impossible byte count",
            ));
        };
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn run_prepare(
    prepare: &super::plan::PrepareCommand,
    source: &GitSource,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<(), BuildExecutionError> {
    let mut command = Command::new(&prepare.program);
    command
        .args(&prepare.arguments)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::select! {
        output = command.output() => output.map_err(|error| infrastructure("run Railpack prepare", error.to_string()))?,
        changed = cancelled.changed() => {
            let _ = changed;
            return Err(BuildExecutionError::cancelled());
        }
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.len() > COMMAND_OUTPUT_LIMIT_BYTES {
        return Err(adapter_failure(
            "Railpack prepare output exceeded its bound",
        ));
    }
    if combined.contains(source.credential().secret().secret()) {
        return Err(adapter_failure(
            "Railpack prepare attempted to disclose the Git credential",
        ));
    }
    if !output.status.success() {
        return Err(adapter_failure(
            source.credential().redact_secret_in(combined),
        ));
    }
    Ok(())
}

async fn pull_exact_image(
    reference: &str,
    expected: &OciDigest,
    expected_platform: &OciPlatform,
    kind: PinnedImageKind,
) -> Result<(), BuildExecutionError> {
    let pulled = run_bounded("docker", ["pull", reference], DOCKER_COMMAND_TIMEOUT).await?;
    require_success("pull pinned build image", &pulled)?;
    let inspected = run_bounded(
        "docker",
        [
            "image",
            "inspect",
            "--format",
            r#"{"os":{{json .Os}},"architecture":{{json .Architecture}},"repo_digests":{{json .RepoDigests}}}"#,
            reference,
        ],
        DOCKER_COMMAND_TIMEOUT,
    )
    .await?;
    require_success("inspect pinned build image", &inspected)?;
    let inspected: InspectedImage = serde_json::from_slice(&inspected.stdout)
        .map_err(|error| infrastructure("decode pinned image digests", error.to_string()))?;
    if inspected.os != expected_platform.os()
        || inspected.architecture != expected_platform.architecture()
    {
        return Err(platform_failure(BuildPlatformFailure::PlatformMismatch {
            expected: expected_platform.clone(),
            actual: OciPlatform::try_new(inspected.os, inspected.architecture).map_err(
                |error| infrastructure("decode pinned image platform", error.to_string()),
            )?,
        }));
    }
    if inspected
        .repo_digests
        .iter()
        .any(|digest| digest.ends_with(expected.as_str()))
    {
        return Ok(());
    }
    let actual = inspected
        .repo_digests
        .into_iter()
        .find_map(|reference| {
            reference
                .rsplit_once('@')
                .map(|(_, digest)| digest.to_owned())
        })
        .and_then(|digest| OciDigest::try_new(digest).ok())
        .ok_or_else(|| {
            infrastructure(
                "inspect pinned build image",
                "Docker reported no repository digest",
            )
        })?;
    Err(platform_failure(match kind {
        PinnedImageKind::Buildkit => BuildPlatformFailure::BuildkitDigestMismatch {
            expected: expected.clone(),
            actual,
        },
        PinnedImageKind::Frontend => BuildPlatformFailure::FrontendDigestMismatch {
            expected: expected.clone(),
            actual,
        },
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectedImage {
    os: String,
    architecture: String,
    repo_digests: Vec<String>,
}

#[derive(Clone, Copy)]
enum PinnedImageKind {
    Buildkit,
    Frontend,
}

async fn create_builder(
    name: &str,
    operation_id: &OperationId,
    platform: &OciPlatform,
    workspace: &Path,
    image: &str,
) -> Result<(), BuildExecutionError> {
    let workspace_mount = format!(
        "type=bind,src={},dst=/workspace",
        workspace.to_string_lossy()
    );
    let cache_mount = format!("type=volume,src={CACHE_VOLUME},dst=/var/lib/buildkit");
    let operation_label = format!("com.getployz.build.operation={}", operation_id.as_str());
    let platform_label = format!(
        "com.getployz.build.platform={}/{}",
        platform.os(),
        platform.architecture()
    );
    let created = run_bounded(
        "docker",
        [
            "create",
            "--name",
            name,
            "--label",
            BUILDER_LABEL,
            "--label",
            operation_label.as_str(),
            "--label",
            platform_label.as_str(),
            "--privileged",
            "--mount",
            workspace_mount.as_str(),
            "--mount",
            cache_mount.as_str(),
            image,
            "--addr",
            "unix:///run/buildkit/buildkitd.sock",
        ],
        DOCKER_COMMAND_TIMEOUT,
    )
    .await?;
    require_success("create BuildKit container", &created)
}

async fn await_buildkit(
    builder: &str,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<(), BuildExecutionError> {
    for _ in 0..BUILDKIT_READY_ATTEMPTS {
        check_cancelled(cancelled)?;
        if run_bounded(
            "docker",
            ["exec", builder, "buildctl", "debug", "workers"],
            Duration::from_secs(5),
        )
        .await
        .is_ok_and(|output| output.status.success())
        {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(BUILDKIT_READY_DELAY) => {}
            changed = cancelled.changed() => {
                let _ = changed;
                return Err(BuildExecutionError::cancelled());
            }
        }
    }
    Err(infrastructure(
        "wait for BuildKit readiness",
        "BuildKit did not report a worker before the readiness deadline",
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_buildctl(
    builder: &str,
    plan: &BuildExecutionPlan,
    source: &GitSource,
    cancelled: &mut watch::Receiver<bool>,
    client: &NatsClient,
    machine_id: &MachineId,
    operation_id: &OperationId,
    platform: &OciPlatform,
) -> Result<PublishedLogs, BuildExecutionError> {
    let mut command = Command::new("docker");
    command
        .args(["exec", builder, "buildctl"])
        .args(&plan.buildctl_arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| infrastructure("start buildctl", error.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| infrastructure("start buildctl", "stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| infrastructure("start buildctl", "stderr was not piped"))?;
    let (output_tx, mut output_rx) = mpsc::channel(16);
    tokio::spawn(read_output(stdout, output_tx.clone()));
    tokio::spawn(read_output(stderr, output_tx));
    let mut publisher = BuildLogPublisher::new(
        client.clone(),
        machine_id.clone(),
        operation_id.clone(),
        platform.clone(),
        source.credential().secret().secret(),
    );
    while let Some(bytes) = tokio::select! {
        bytes = output_rx.recv() => bytes,
        changed = cancelled.changed() => {
            let _ = changed;
            let _ = child.kill().await;
            let logs = publisher.finish().await?;
            return Err(BuildExecutionError::cancelled().with_logs(logs));
        }
    } {
        if let Err(error) = publisher.push(&bytes).await {
            let logs = publisher.finish().await?;
            return Err(error.with_logs(logs));
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| infrastructure("wait for buildctl", error.to_string()))?;
    let logs = publisher.finish().await?;
    if !status.success() {
        return Err(adapter_failure(format!(
            "BuildKit build failed with exit code {:?}",
            status.code()
        ))
        .with_logs(logs));
    }
    Ok(logs)
}

async fn read_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<Vec<u8>>,
) {
    let mut buffer = vec![0_u8; MAX_BUILD_LOG_CHUNK_BYTES];
    loop {
        let Ok(read) = reader.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let Some(bytes) = buffer.get(..read) else {
            return;
        };
        if sender.send(bytes.to_vec()).await.is_err() {
            return;
        }
    }
}

struct BuildLogPublisher {
    client: NatsClient,
    machine_id: MachineId,
    operation_id: OperationId,
    platform: OciPlatform,
    secret: String,
    pending: String,
    sequence: u64,
    published_bytes: u64,
    omitted_bytes: u64,
}

impl BuildLogPublisher {
    fn new(
        client: NatsClient,
        machine_id: MachineId,
        operation_id: OperationId,
        platform: OciPlatform,
        secret: &str,
    ) -> Self {
        Self {
            client,
            machine_id,
            operation_id,
            platform,
            secret: secret.to_owned(),
            pending: String::new(),
            sequence: 0,
            published_bytes: 0,
            omitted_bytes: 0,
        }
    }

    async fn push(&mut self, bytes: &[u8]) -> Result<(), BuildExecutionError> {
        self.pending.push_str(&String::from_utf8_lossy(bytes));
        let safe = take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::More);
        self.publish_text(safe).await
    }

    async fn finish(mut self) -> Result<PublishedLogs, BuildExecutionError> {
        let remaining =
            take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::End);
        self.publish_text(remaining).await?;
        self.client
            .flush()
            .await
            .map_err(|error| infrastructure("flush build logs", error.to_string()))?;
        Ok(PublishedLogs {
            final_sequence: self.sequence,
            omitted_bytes: self.omitted_bytes,
        })
    }

    async fn publish_text(&mut self, mut text: String) -> Result<(), BuildExecutionError> {
        while !text.is_empty() {
            let split =
                char_boundary_at_or_before(&text, text.len().min(MAX_BUILD_LOG_CHUNK_BYTES));
            if split == 0 {
                break;
            }
            let chunk = text[..split].to_owned();
            text.drain(..split);
            let bytes = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            if self.published_bytes.saturating_add(bytes) > BUILD_LOG_LIMIT_BYTES {
                self.omitted_bytes = self
                    .omitted_bytes
                    .saturating_add(bytes)
                    .saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
                break;
            }
            self.sequence = self.sequence.saturating_add(1);
            let frame = MachineBuildLogFrame {
                operation_id: self.operation_id.clone(),
                machine_id: self.machine_id.clone(),
                platform: self.platform.clone(),
                sequence: self.sequence,
                chunk: BuildLogChunk::try_new(chunk)
                    .map_err(|error| infrastructure("frame build log", error.to_string()))?,
            };
            let payload = serde_json::to_vec(&frame)
                .map_err(|error| infrastructure("encode build log", error.to_string()))?;
            self.client
                .publish(
                    machine_build_log(&self.machine_id, &self.operation_id),
                    payload.into(),
                )
                .await
                .map_err(|error| infrastructure("publish build log", error.to_string()))?;
            self.published_bytes = self.published_bytes.saturating_add(bytes);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum RedactionBoundary {
    More,
    End,
}

fn take_redacted_output(pending: &mut String, secret: &str, boundary: RedactionBoundary) -> String {
    if secret.is_empty() {
        return std::mem::take(pending);
    }
    let mut safe = String::new();
    while let Some(position) = pending.find(secret) {
        safe.push_str(&pending[..position]);
        safe.push_str("[redacted]");
        pending.drain(..position + secret.len());
    }
    let keep = match boundary {
        RedactionBoundary::More => secret.len().saturating_sub(1),
        RedactionBoundary::End => 0,
    };
    let split = char_boundary_at_or_before(pending, pending.len().saturating_sub(keep));
    safe.push_str(&pending[..split]);
    pending.drain(..split);
    safe
}

struct PublishedLogs {
    final_sequence: u64,
    omitted_bytes: u64,
}

async fn run_bounded<const N: usize>(
    program: &str,
    arguments: [&str; N],
    timeout: Duration,
) -> Result<std::process::Output, BuildExecutionError> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| infrastructure("run Docker command", "command timed out"))?
        .map_err(|error| infrastructure("run Docker command", error.to_string()))?;
    if output.stdout.len().saturating_add(output.stderr.len()) > COMMAND_OUTPUT_LIMIT_BYTES {
        return Err(infrastructure(
            "run Docker command",
            "command output exceeded its bound",
        ));
    }
    Ok(output)
}

fn require_success(
    action: &'static str,
    output: &std::process::Output,
) -> Result<(), BuildExecutionError> {
    if output.status.success() {
        return Ok(());
    }
    Err(infrastructure(
        action,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

async fn remove_builder(container: &str) -> Result<(), BuildExecutionError> {
    let removed = run_bounded("docker", ["rm", "-f", container], DOCKER_COMMAND_TIMEOUT).await?;
    if removed.status.success()
        || String::from_utf8_lossy(&removed.stderr).contains("No such container")
    {
        Ok(())
    } else {
        require_success("remove BuildKit container", &removed)
    }
}

fn builder_name(operation_id: &OperationId, platform: &OciPlatform) -> String {
    format!(
        "ployz-build-{}-{}-{}",
        operation_id.as_str(),
        platform.os(),
        platform.architecture()
    )
}

fn char_boundary_at_or_before(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn check_cancelled(cancelled: &watch::Receiver<bool>) -> Result<(), BuildExecutionError> {
    if *cancelled.borrow() {
        Err(BuildExecutionError::cancelled())
    } else {
        Ok(())
    }
}

fn adapter_failure(message: impl Into<String>) -> BuildExecutionError {
    platform_failure(BuildPlatformFailure::AdapterFailed {
        message: failure(message),
    })
}

fn platform_failure(failure: BuildPlatformFailure) -> BuildExecutionError {
    BuildExecutionError::Platform {
        failure,
        final_log_sequence: 0,
        omitted_log_bytes: 0,
    }
}

fn failure(message: impl Into<String>) -> FailureMessage {
    let message = message.into();
    FailureMessage::try_new(if message.is_empty() {
        "build execution failed".to_owned()
    } else {
        message
    })
    .expect("build execution failures are non-empty")
}

fn infrastructure(action: &'static str, message: impl Into<String>) -> BuildExecutionError {
    BuildExecutionError::Infrastructure {
        action,
        message: message.into(),
        final_log_sequence: 0,
        omitted_log_bytes: 0,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildExecutionError {
    #[error("build was cancelled")]
    Cancelled {
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    #[error("build platform failed: {failure:?}")]
    Platform {
        failure: BuildPlatformFailure,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    #[error("failed to {action}: {message}")]
    Infrastructure {
        action: &'static str,
        message: String,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
}

impl BuildExecutionError {
    fn cancelled() -> Self {
        Self::Cancelled {
            final_log_sequence: 0,
            omitted_log_bytes: 0,
        }
    }

    fn with_logs(self, logs: PublishedLogs) -> Self {
        let PublishedLogs {
            final_sequence,
            omitted_bytes,
        } = logs;
        match self {
            Self::Cancelled { .. } => Self::Cancelled {
                final_log_sequence: final_sequence,
                omitted_log_bytes: omitted_bytes,
            },
            Self::Platform { failure, .. } => Self::Platform {
                failure,
                final_log_sequence: final_sequence,
                omitted_log_bytes: omitted_bytes,
            },
            Self::Infrastructure {
                action, message, ..
            } => Self::Infrastructure {
                action,
                message,
                final_log_sequence: final_sequence,
                omitted_log_bytes: omitted_bytes,
            },
        }
    }

    #[must_use]
    pub const fn log_summary(&self) -> (u64, u64) {
        match self {
            Self::Cancelled {
                final_log_sequence,
                omitted_log_bytes,
            }
            | Self::Platform {
                final_log_sequence,
                omitted_log_bytes,
                ..
            }
            | Self::Infrastructure {
                final_log_sequence,
                omitted_log_bytes,
                ..
            } => (*final_log_sequence, *omitted_log_bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_redaction_keeps_a_possible_secret_suffix() {
        let secret = "token-123";
        let mut pending = "prefix token-".to_owned();
        let first = take_redacted_output(&mut pending, secret, RedactionBoundary::More);
        pending.push_str("123 suffix");
        let second = take_redacted_output(&mut pending, secret, RedactionBoundary::End);

        assert_eq!(first + &second, "prefix [redacted] suffix");
        assert!(pending.is_empty());
    }

    #[test]
    fn builder_identity_is_deterministic_and_platform_scoped() {
        let operation = OperationId::try_new("build-01").expect("operation");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");
        assert_eq!(
            builder_name(&operation, &platform),
            "ployz-build-build-01-linux-arm64"
        );
    }
}
