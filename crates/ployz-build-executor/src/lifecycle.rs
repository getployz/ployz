use super::logs::{
    BuildLogDestination, BuildLogProgress, BuildLogPublisher, PublishedLogs, read_output,
};
use super::plan::{BuildExecutionPlan, PrepareCommand};
use super::runner::{
    BuildExecutionError, adapter_failure, check_cancelled, infrastructure, platform_failure,
};
use ployz_core::build::BUILD_CACHE_PRUNE_COMMAND_TIMEOUT;
use ployz_core::build::BuildSource;
use ployz_core::ids::OperationId;
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::BuildPlatformFailure;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

pub(super) const BUILDER_LABEL: &str = "com.getployz.build=true";
pub(super) const BUILDER_OWNER_LABEL_KEY: &str = "com.getployz.build.owner";
pub(super) const CACHE_VOLUME: &str = "ployz-buildkit-cache-v1";
const COMMAND_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
const RAILPACK_PREPARE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DOCKER_RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const BUILDKIT_READY_ATTEMPTS: usize = 30;
const BUILDKIT_READY_DELAY: Duration = Duration::from_millis(500);

pub(super) async fn probe_docker_runtime(
    native: &OciPlatform,
) -> Result<OciPlatform, BuildExecutionError> {
    let output = run_bounded(
        "docker",
        [
            "info",
            "--format",
            r#"{"os":{{json .OSType}},"architecture":{{json .Architecture}}}"#,
        ],
        DOCKER_RUNTIME_PROBE_TIMEOUT,
    )
    .await?;
    require_success("probe Docker build runtime", &output)?;
    validate_docker_server_platform(&output.stdout, native)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerServerPlatform {
    os: String,
    architecture: String,
}

fn validate_docker_server_platform(
    output: &[u8],
    native: &OciPlatform,
) -> Result<OciPlatform, BuildExecutionError> {
    let reported: DockerServerPlatform = serde_json::from_slice(output).map_err(|error| {
        infrastructure("decode Docker build runtime platform", error.to_string())
    })?;
    let actual = crate::oci_platform(&reported.os, &reported.architecture)
        .map_err(|message| infrastructure("normalize Docker build runtime platform", message))?;
    if &actual != native {
        return Err(platform_failure(BuildPlatformFailure::PlatformMismatch {
            expected: native.clone(),
            actual,
        }));
    }
    Ok(actual)
}

pub(super) async fn run_prepare(
    prepare: &PrepareCommand,
    source: &BuildSource,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<(), BuildExecutionError> {
    run_prepare_with_timeout(prepare, source, cancelled, RAILPACK_PREPARE_TIMEOUT).await
}

async fn run_prepare_with_timeout(
    prepare: &PrepareCommand,
    source: &BuildSource,
    cancelled: &mut watch::Receiver<bool>,
    timeout: Duration,
) -> Result<(), BuildExecutionError> {
    check_cancelled(cancelled)?;
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
    let mut child = command
        .spawn()
        .map_err(|error| infrastructure("run Railpack prepare", error.to_string()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| infrastructure("run Railpack prepare", "stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| infrastructure("run Railpack prepare", "stderr was not piped"))?;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut stdout_buffer = [0_u8; 8 * 1024];
    let mut stderr_buffer = [0_u8; 8 * 1024];
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;

    while stdout_open || stderr_open {
        tokio::select! {
            biased;
            changed = cancelled.changed() => {
                let _ = changed;
                terminate(&mut child).await;
                return Err(BuildExecutionError::cancelled());
            }
            () = &mut deadline => {
                terminate(&mut child).await;
                return Err(adapter_failure("Railpack prepare timed out"));
            }
            read = stdout.read(&mut stdout_buffer), if stdout_open => {
                let read = match read {
                    Ok(read) => read,
                    Err(error) => {
                        terminate(&mut child).await;
                        return Err(infrastructure("read Railpack prepare stdout", error.to_string()));
                    }
                };
                if read == 0 {
                    stdout_open = false;
                } else if append_bounded(&mut stdout_bytes, read_bytes(&stdout_buffer, read), stderr_bytes.len()) {
                    terminate(&mut child).await;
                    return Err(adapter_failure("Railpack prepare output exceeded its bound"));
                }
            }
            read = stderr.read(&mut stderr_buffer), if stderr_open => {
                let read = match read {
                    Ok(read) => read,
                    Err(error) => {
                        terminate(&mut child).await;
                        return Err(infrastructure("read Railpack prepare stderr", error.to_string()));
                    }
                };
                if read == 0 {
                    stderr_open = false;
                } else if append_bounded(&mut stderr_bytes, read_bytes(&stderr_buffer, read), stdout_bytes.len()) {
                    terminate(&mut child).await;
                    return Err(adapter_failure("Railpack prepare output exceeded its bound"));
                }
            }
        }
    }
    enum WaitOutcome {
        Exited(std::io::Result<std::process::ExitStatus>),
        Cancelled,
        TimedOut,
    }
    let outcome = tokio::select! {
        status = child.wait() => WaitOutcome::Exited(status),
        changed = cancelled.changed() => {
            let _ = changed;
            WaitOutcome::Cancelled
        }
        () = &mut deadline => WaitOutcome::TimedOut,
    };
    let status = match outcome {
        WaitOutcome::Exited(status) => {
            status.map_err(|error| infrastructure("run Railpack prepare", error.to_string()))?
        }
        WaitOutcome::Cancelled => {
            terminate(&mut child).await;
            return Err(BuildExecutionError::cancelled());
        }
        WaitOutcome::TimedOut => {
            terminate(&mut child).await;
            return Err(adapter_failure("Railpack prepare timed out"));
        }
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout_bytes),
        String::from_utf8_lossy(&stderr_bytes)
    );
    if combined.len() > COMMAND_OUTPUT_LIMIT_BYTES {
        return Err(adapter_failure(
            "Railpack prepare output exceeded its bound",
        ));
    }
    if source.contains_sensitive_value(&combined) {
        return Err(adapter_failure(
            "Railpack prepare attempted to disclose the Git credential",
        ));
    }
    if !status.success() {
        return Err(adapter_failure(source.redact_sensitive_in(combined)));
    }
    Ok(())
}

fn append_bounded(output: &mut Vec<u8>, bytes: &[u8], other_len: usize) -> bool {
    let remaining = COMMAND_OUTPUT_LIMIT_BYTES.saturating_sub(output.len() + other_len);
    let keep = bytes.len().min(remaining);
    let Some(bounded) = bytes.get(..keep) else {
        unreachable!("bounded length comes from the source slice");
    };
    output.extend_from_slice(bounded);
    bytes.len() > remaining
}

fn read_bytes(buffer: &[u8], read: usize) -> &[u8] {
    let Some(bytes) = buffer.get(..read) else {
        unreachable!("AsyncRead cannot report more bytes than the supplied buffer");
    };
    bytes
}

async fn terminate(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

pub(super) async fn pull_exact_image(
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
    verify_inspected_image(inspected, expected, expected_platform, kind)
}

fn verify_inspected_image(
    inspected: InspectedImage,
    expected: &OciDigest,
    expected_platform: &OciPlatform,
    kind: PinnedImageKind,
) -> Result<(), BuildExecutionError> {
    if inspected.os.is_empty() && inspected.architecture.is_empty() {
        if matches!(kind, PinnedImageKind::Buildkit) {
            return Err(infrastructure(
                "decode pinned image platform",
                "Docker reported no platform for the runnable BuildKit image",
            ));
        }
    } else {
        let actual = OciPlatform::try_new(inspected.os, inspected.architecture)
            .map_err(|error| infrastructure("decode pinned image platform", error.to_string()))?;
        if &actual != expected_platform {
            return Err(platform_failure(BuildPlatformFailure::PlatformMismatch {
                expected: expected_platform.clone(),
                actual,
            }));
        }
    }
    let mut reported_digests = inspected.repo_digests.into_iter().filter_map(|reference| {
        reference
            .rsplit_once('@')
            .and_then(|(_, digest)| OciDigest::try_new(digest).ok())
    });
    let Some(actual) = reported_digests.next() else {
        return Err(infrastructure(
            "inspect pinned build image",
            "Docker reported no repository digest",
        ));
    };
    if &actual == expected || reported_digests.any(|digest| &digest == expected) {
        return Ok(());
    }
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
pub(super) enum PinnedImageKind {
    Buildkit,
    Frontend,
}

pub(super) async fn create_builder(
    name: &str,
    operation_id: &OperationId,
    platform: &OciPlatform,
    owner: &str,
    workspace: &Path,
    config: &Path,
    image: &str,
) -> Result<(), BuildExecutionError> {
    let workspace_mount = format!(
        "type=bind,src={},dst=/workspace",
        workspace.to_string_lossy()
    );
    let cache_mount = format!("type=volume,src={CACHE_VOLUME},dst=/var/lib/buildkit");
    let config_mount = format!(
        "type=bind,src={},dst=/etc/buildkit/buildkitd.toml,readonly",
        config.to_string_lossy()
    );
    let [build_label, owner_label, operation_label, platform_label] =
        builder_create_labels(operation_id, platform, owner);
    let created = run_bounded(
        "docker",
        [
            "create",
            "--name",
            name,
            "--label",
            build_label.as_str(),
            "--label",
            owner_label.as_str(),
            "--label",
            operation_label.as_str(),
            "--label",
            platform_label.as_str(),
            "--privileged",
            "--mount",
            workspace_mount.as_str(),
            "--mount",
            cache_mount.as_str(),
            "--mount",
            config_mount.as_str(),
            image,
            "--config",
            "/etc/buildkit/buildkitd.toml",
            "--addr",
            "unix:///run/buildkit/buildkitd.sock",
        ],
        DOCKER_COMMAND_TIMEOUT,
    )
    .await?;
    require_success("create BuildKit container", &created)
}

fn builder_create_labels(
    operation_id: &OperationId,
    platform: &OciPlatform,
    owner: &str,
) -> [String; 4] {
    [
        BUILDER_LABEL.to_owned(),
        format!("{BUILDER_OWNER_LABEL_KEY}={owner}"),
        format!("com.getployz.build.operation={}", operation_id.as_str()),
        format!(
            "com.getployz.build.platform={}/{}",
            platform.os(),
            platform.architecture()
        ),
    ]
}

pub(super) async fn prune_buildkit_cache() -> Result<(), BuildExecutionError> {
    let volume = run_bounded(
        "docker",
        [
            "volume",
            "ls",
            "-q",
            "--filter",
            &format!("name=^{CACHE_VOLUME}$"),
        ],
        DOCKER_COMMAND_TIMEOUT,
    )
    .await?;
    require_success("inspect BuildKit cache volume", &volume)?;
    if String::from_utf8_lossy(&volume.stdout).trim() != CACHE_VOLUME {
        return Ok(());
    }
    let removed = run_bounded(
        "docker",
        ["volume", "rm", CACHE_VOLUME],
        DOCKER_COMMAND_TIMEOUT,
    )
    .await?;
    require_success("remove BuildKit cache volume", &removed)
}

pub(super) async fn inspect_buildkit_cache(
    cancelled: &mut watch::Receiver<bool>,
) -> Result<bool, BuildExecutionError> {
    let volume = run_bounded_cancelled(
        "docker",
        [
            "volume",
            "ls",
            "-q",
            "--filter",
            &format!("name=^{CACHE_VOLUME}$"),
        ],
        BUILD_CACHE_PRUNE_COMMAND_TIMEOUT,
        cancelled,
    )
    .await?;
    require_success("inspect BuildKit cache volume", &volume)?;
    Ok(String::from_utf8_lossy(&volume.stdout).trim() == CACHE_VOLUME)
}

pub(super) async fn remove_buildkit_cache(
    cancelled: &mut watch::Receiver<bool>,
) -> Result<(), BuildExecutionError> {
    let removed = run_bounded_cancelled(
        "docker",
        ["volume", "rm", CACHE_VOLUME],
        BUILD_CACHE_PRUNE_COMMAND_TIMEOUT,
        cancelled,
    )
    .await?;
    require_success("remove BuildKit cache volume", &removed)
}

pub(super) async fn await_buildkit(
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

pub(super) struct BuildLogContext<'a> {
    pub(super) destination: &'a BuildLogDestination,
    pub(super) operation_id: &'a OperationId,
    pub(super) platform: &'a OciPlatform,
    pub(super) progress: BuildLogProgress,
}

pub(super) async fn run_buildctl(
    builder: &str,
    plan: &BuildExecutionPlan,
    source: &BuildSource,
    cancelled: &mut watch::Receiver<bool>,
    logs: BuildLogContext<'_>,
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
        logs.destination.clone(),
        logs.operation_id.clone(),
        logs.platform.clone(),
        source.sensitive_value().unwrap_or(""),
        logs.progress,
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

pub(super) async fn run_bounded<const N: usize>(
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

async fn run_bounded_cancelled<const N: usize>(
    program: &str,
    arguments: [&str; N],
    timeout: Duration,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<std::process::Output, BuildExecutionError> {
    check_cancelled(cancelled)?;
    tokio::select! {
        biased;
        changed = cancelled.changed() => {
            match changed {
                Ok(()) if *cancelled.borrow() => Err(BuildExecutionError::cancelled()),
                Ok(()) => Err(infrastructure(
                    "run Docker command",
                    "cancellation signal changed without requesting cancellation",
                )),
                Err(_) => Err(infrastructure("run Docker command", "cancellation channel closed")),
            }
        }
        output = run_bounded(program, arguments, timeout) => output,
    }
}

pub(super) fn require_success(
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

pub(super) async fn remove_builder(container: &str) -> Result<(), BuildExecutionError> {
    let removed = run_bounded("docker", ["rm", "-f", container], DOCKER_COMMAND_TIMEOUT).await?;
    if removed.status.success()
        || String::from_utf8_lossy(&removed.stderr).contains("No such container")
    {
        Ok(())
    } else {
        require_success("remove BuildKit container", &removed)
    }
}

pub(super) async fn restore_oci_layout_ownership(
    builder: &str,
    workspace: &Path,
) -> Result<(), BuildExecutionError> {
    let metadata = tokio::fs::metadata(workspace)
        .await
        .map_err(|error| infrastructure("read build workspace ownership", error.to_string()))?;
    let arguments = ownership_restore_arguments(builder, metadata.uid(), metadata.gid());
    let arguments = arguments.each_ref().map(String::as_str);
    let restored = run_bounded("docker", arguments, DOCKER_COMMAND_TIMEOUT).await?;
    require_success("restore OCI output ownership", &restored)
}

fn ownership_restore_arguments(builder: &str, uid: u32, gid: u32) -> [String; 6] {
    [
        "exec".to_owned(),
        builder.to_owned(),
        "chown".to_owned(),
        "-R".to_owned(),
        format!("{uid}:{gid}"),
        "/workspace/oci".to_owned(),
    ]
}

pub(super) fn builder_name(operation_id: &OperationId, platform: &OciPlatform) -> String {
    format!(
        "ployz-build-{}-{}-{}",
        operation_id.as_str(),
        platform.os(),
        platform.architecture()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildSource, GitSource};
    use std::path::PathBuf;

    fn source() -> BuildSource {
        GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source")
        .into()
    }

    fn shell_prepare(script: &str) -> PrepareCommand {
        PrepareCommand {
            program: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), script.to_owned()],
        }
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

    #[test]
    fn builder_creation_carries_exact_owner_label() {
        let operation = OperationId::try_new("build-01").expect("operation");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");

        assert_eq!(
            builder_create_labels(&operation, &platform, "owner-digest"),
            [
                "com.getployz.build=true",
                "com.getployz.build.owner=owner-digest",
                "com.getployz.build.operation=build-01",
                "com.getployz.build.platform=linux/arm64",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn ownership_restore_uses_exact_oci_path_without_a_shell() {
        assert_eq!(
            ownership_restore_arguments("builder-1", 1001, 1002),
            [
                "exec",
                "builder-1",
                "chown",
                "-R",
                "1001:1002",
                "/workspace/oci",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn docker_server_platform_accepts_docker_and_oci_architecture_names() {
        let native = OciPlatform::try_new("linux", "amd64").expect("native");
        for architecture in ["x86_64", "amd64"] {
            let output = format!(r#"{{"os":"linux","architecture":"{architecture}"}}"#);
            assert_eq!(
                validate_docker_server_platform(output.as_bytes(), &native)
                    .expect("compatible server"),
                native
            );
        }
    }

    #[test]
    fn docker_server_platform_rejects_remote_architecture_mismatch() {
        let native = OciPlatform::try_new("linux", "amd64").expect("native");
        let error =
            validate_docker_server_platform(br#"{"os":"linux","architecture":"aarch64"}"#, &native)
                .expect_err("remote architecture must not be testified as native");
        assert!(matches!(
            error,
            BuildExecutionError::Platform {
                failure: BuildPlatformFailure::PlatformMismatch { expected, actual },
                ..
            } if expected == native && actual.architecture() == "arm64"
        ));
    }

    #[test]
    fn exact_frontend_digest_accepts_non_runnable_frontend_without_platform_metadata() {
        let digest = OciDigest::try_new(
            "sha256:17b4f33fca2b79aba474a400650bb338a32130b5ca40d8bc755cde93f594a95c",
        )
        .expect("digest");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");
        let inspected = InspectedImage {
            os: String::new(),
            architecture: String::new(),
            repo_digests: vec![format!("frontend@{digest}")],
        };

        verify_inspected_image(inspected, &digest, &platform, PinnedImageKind::Frontend)
            .expect("digest-selected frontend");
    }

    #[test]
    fn runnable_buildkit_image_still_requires_platform_metadata() {
        let digest = OciDigest::try_new(
            "sha256:4eee950fb9d134cbf4e228ea3906eb4c7403323334af013c443302f7b74f2737",
        )
        .expect("digest");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");
        let inspected = InspectedImage {
            os: String::new(),
            architecture: String::new(),
            repo_digests: vec![format!("buildkit@{digest}")],
        };

        let error =
            verify_inspected_image(inspected, &digest, &platform, PinnedImageKind::Buildkit)
                .expect_err("runnable image needs a platform");
        assert!(error.to_string().contains("reported no platform"));
    }

    #[test]
    fn malformed_digest_suffix_does_not_satisfy_the_exact_frontend_pin() {
        let expected = OciDigest::try_new(
            "sha256:17b4f33fca2b79aba474a400650bb338a32130b5ca40d8bc755cde93f594a95c",
        )
        .expect("digest");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");
        let inspected = InspectedImage {
            os: String::new(),
            architecture: String::new(),
            repo_digests: vec![format!("frontend@not-{expected}")],
        };

        let error =
            verify_inspected_image(inspected, &expected, &platform, PinnedImageKind::Frontend)
                .expect_err("malformed digest is not exact");
        assert!(error.to_string().contains("no repository digest"));
    }

    #[tokio::test]
    async fn prepare_output_is_rejected_while_still_bounded() {
        let prepare = shell_prepare("dd if=/dev/zero bs=1024 count=257 2>/dev/null");
        let (_cancel_tx, mut cancelled) = watch::channel(false);
        let error =
            run_prepare_with_timeout(&prepare, &source(), &mut cancelled, Duration::from_secs(5))
                .await
                .expect_err("oversized output fails");
        assert!(error.to_string().contains("output exceeded its bound"));
    }

    #[tokio::test]
    async fn prepare_timeout_terminates_the_child() {
        let prepare = shell_prepare("sleep 30");
        let (_cancel_tx, mut cancelled) = watch::channel(false);
        let error = run_prepare_with_timeout(
            &prepare,
            &source(),
            &mut cancelled,
            Duration::from_millis(20),
        )
        .await
        .expect_err("timed out prepare fails");
        assert!(error.to_string().contains("prepare timed out"));
    }

    #[tokio::test]
    async fn bounded_command_observes_cancellation_while_running() {
        let (cancel, mut cancelled) = watch::channel(false);
        let command = run_bounded_cancelled(
            "/bin/sh",
            ["-c", "sleep 30"],
            Duration::from_secs(30),
            &mut cancelled,
        );
        tokio::pin!(command);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(20)) => {
                cancel.send(true).expect("cancellation receiver remains live");
            }
            result = &mut command => panic!("command ended before cancellation: {result:?}"),
        }
        assert!(matches!(
            command.await,
            Err(BuildExecutionError::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn bounded_command_enforces_its_supplied_timeout() {
        let (_cancel, mut cancelled) = watch::channel(false);
        let error = run_bounded_cancelled(
            "/bin/sh",
            ["-c", "sleep 30"],
            Duration::from_millis(20),
            &mut cancelled,
        )
        .await
        .expect_err("command must time out");
        assert!(error.to_string().contains("command timed out"));
    }

    #[tokio::test]
    async fn prepare_timeout_still_applies_after_output_pipes_close() {
        let prepare = shell_prepare("exec 1>&- 2>&-; sleep 30");
        let (_cancel_tx, mut cancelled) = watch::channel(false);
        let error = run_prepare_with_timeout(
            &prepare,
            &source(),
            &mut cancelled,
            Duration::from_millis(20),
        )
        .await
        .expect_err("timed out prepare fails after closing pipes");
        assert!(error.to_string().contains("prepare timed out"));
    }

    #[tokio::test]
    async fn prepare_cancellation_terminates_the_child() {
        let prepare = shell_prepare("sleep 30");
        let (cancel_tx, mut cancelled) = watch::channel(false);
        let cancellation = async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_tx.send(true).expect("receiver remains open");
        };
        let source = source();
        let execution =
            run_prepare_with_timeout(&prepare, &source, &mut cancelled, Duration::from_secs(5));
        let ((), result) = tokio::join!(cancellation, execution);
        assert!(matches!(result, Err(BuildExecutionError::Cancelled { .. })));
    }

    #[tokio::test]
    async fn prepare_honors_cancellation_that_is_already_true() {
        let prepare = shell_prepare("exit 0");
        let (_cancel_tx, mut cancelled) = watch::channel(true);
        let result =
            run_prepare_with_timeout(&prepare, &source(), &mut cancelled, Duration::from_secs(5))
                .await;
        assert!(matches!(result, Err(BuildExecutionError::Cancelled { .. })));
    }
}
