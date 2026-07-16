use super::logs::{BuildLogPublisher, PublishedLogs, read_output};
use super::plan::{BuildExecutionPlan, PrepareCommand};
use super::runner::{
    BuildExecutionError, adapter_failure, check_cancelled, infrastructure, platform_failure,
};
use ployz_core::build::GitSource;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::BuildPlatformFailure;
use ployz_nats::service_runtime::NatsClient;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

pub(super) const BUILDER_LABEL: &str = "com.getployz.build=true";
pub(super) const CACHE_VOLUME: &str = "ployz-buildkit-cache-v1";
const COMMAND_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
pub(super) const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const BUILDKIT_READY_ATTEMPTS: usize = 30;
const BUILDKIT_READY_DELAY: Duration = Duration::from_millis(500);

pub(super) async fn run_prepare(
    prepare: &PrepareCommand,
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
pub(super) enum PinnedImageKind {
    Buildkit,
    Frontend,
}

pub(super) async fn create_builder(
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_buildctl(
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
