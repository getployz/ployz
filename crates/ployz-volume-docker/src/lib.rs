use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use ployz_volume_api::{
    EnsureVolumeRequest, Result, VolumeBackend, VolumeBackendInfo, VolumeBackendKind,
    VolumeCapabilities, VolumeError, VolumeIdentity, VolumeInspection, VolumeMount,
};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerVolumeCommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait DockerVolumeRunner: Send + Sync {
    async fn run(&self, args: &[&str]) -> std::result::Result<DockerVolumeCommandOutput, String>;
}

#[derive(Debug, Clone, Default)]
pub struct TokioDockerVolumeRunner;

#[async_trait]
impl DockerVolumeRunner for TokioDockerVolumeRunner {
    async fn run(&self, args: &[&str]) -> std::result::Result<DockerVolumeCommandOutput, String> {
        let output = Command::new("docker")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| format!("spawn docker {}: {error}", args.join(" ")))?;
        Ok(DockerVolumeCommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Clone)]
pub struct DockerVolumeBackend<R = TokioDockerVolumeRunner> {
    runner: Arc<R>,
    name_prefix: String,
}

impl DockerVolumeBackend<TokioDockerVolumeRunner> {
    #[must_use]
    pub fn local_default() -> Self {
        Self::new(TokioDockerVolumeRunner, "ployz")
    }
}

impl<R> DockerVolumeBackend<R>
where
    R: DockerVolumeRunner,
{
    #[must_use]
    pub fn new(runner: R, name_prefix: impl Into<String>) -> Self {
        Self {
            runner: Arc::new(runner),
            name_prefix: name_prefix.into(),
        }
    }

    #[must_use]
    pub fn volume_name(&self, identity: &VolumeIdentity) -> String {
        docker_volume_name(&self.name_prefix, identity)
    }

    async fn run_success(&self, operation: &'static str, args: &[&str]) -> Result<()> {
        let output = self
            .runner
            .run(args)
            .await
            .map_err(|message| docker_volume_error(operation, message))?;
        if output.status == 0 {
            return Ok(());
        }
        Err(docker_volume_error(
            operation,
            command_failure_message(&output.stderr, output.status),
        ))
    }
}

#[async_trait]
impl<R> VolumeBackend for DockerVolumeBackend<R>
where
    R: DockerVolumeRunner,
{
    fn info(&self) -> VolumeBackendInfo {
        VolumeBackendInfo {
            id: "docker-volume".to_string(),
            kind: VolumeBackendKind::Docker,
            capabilities: VolumeCapabilities::docker_local(),
        }
    }

    async fn ensure(&self, request: EnsureVolumeRequest) -> Result<VolumeMount> {
        reject_unsupported_constraints(&request)?;
        let name = self.volume_name(&request.identity);
        self.run_success("docker volume create", &["volume", "create", &name])
            .await?;
        Ok(VolumeMount::docker_volume(request.identity, name))
    }

    async fn remove(&self, identity: VolumeIdentity) -> Result<()> {
        let name = self.volume_name(&identity);
        self.run_success("docker volume rm", &["volume", "rm", "-f", &name])
            .await
    }

    async fn mount(&self, identity: VolumeIdentity) -> Result<VolumeMount> {
        let name = self.volume_name(&identity);
        Ok(VolumeMount::docker_volume(identity, name))
    }

    async fn inspect(&self, identity: VolumeIdentity) -> Result<Option<VolumeInspection>> {
        let name = self.volume_name(&identity);
        let output = self
            .runner
            .run(&["volume", "inspect", &name])
            .await
            .map_err(|message| docker_volume_error("docker volume inspect", message))?;
        if output.status == 0 {
            return Ok(Some(VolumeInspection {
                identity,
                mountpoint: None,
                used_bytes: None,
                snapshots: Vec::new(),
            }));
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("no such volume") || stderr.contains("not found") {
            return Ok(None);
        }
        Err(docker_volume_error(
            "docker volume inspect",
            command_failure_message(&output.stderr, output.status),
        ))
    }
}

#[must_use]
pub fn docker_volume_name(prefix: &str, identity: &VolumeIdentity) -> String {
    let raw = format!("{prefix}-{}-{}", identity.namespace, identity.name);
    let mut sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if !sanitized
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
    {
        sanitized.insert(0, 'v');
    }
    sanitized
}

fn docker_volume_error(operation: &'static str, message: String) -> VolumeError {
    VolumeError::Backend {
        backend: "docker-volume".to_string(),
        operation,
        message,
    }
}

fn reject_unsupported_constraints(request: &EnsureVolumeRequest) -> Result<()> {
    if request.mountpoint.is_some() {
        return Err(unsupported_constraint(
            "mountpoint",
            "docker-volume backend uses Docker-managed named volume mountpoints",
        ));
    }
    if request.quota.is_some() {
        return Err(unsupported_constraint(
            "quota",
            "docker-volume backend cannot enforce volume quotas",
        ));
    }
    if request.mode.is_some() {
        return Err(unsupported_constraint(
            "mode",
            "docker-volume backend cannot enforce volume modes",
        ));
    }
    if request.owner.is_some() {
        return Err(unsupported_constraint(
            "owner",
            "docker-volume backend cannot enforce volume owners",
        ));
    }
    Ok(())
}

fn unsupported_constraint(field: &str, message: &str) -> VolumeError {
    VolumeError::InvalidRequest {
        field: field.to_string(),
        message: message.to_string(),
    }
}

fn command_failure_message(stderr: &[u8], status: i32) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        format!("exited with status {status}")
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeDockerRunner {
        calls: Mutex<Vec<Vec<String>>>,
        outputs: Mutex<VecDeque<std::result::Result<DockerVolumeCommandOutput, String>>>,
    }

    impl FakeDockerRunner {
        fn push_status(&self, status: i32, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(Ok(DockerVolumeCommandOutput {
                    status,
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                }));
        }
    }

    #[async_trait]
    impl DockerVolumeRunner for FakeDockerRunner {
        async fn run(
            &self,
            args: &[&str],
        ) -> std::result::Result<DockerVolumeCommandOutput, String> {
            self.calls
                .lock()
                .expect("calls")
                .push(args.iter().map(|arg| (*arg).to_string()).collect());
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(DockerVolumeCommandOutput {
                        status: 0,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    })
                })
        }
    }

    #[test]
    fn docker_volume_name_sanitizes_identity() {
        let name = docker_volume_name("ployz", &VolumeIdentity::new("prod/blue", "data set"));

        assert_eq!(name, "ployz-prod-blue-data-set");
    }

    #[tokio::test]
    async fn ensure_creates_named_docker_volume() {
        let backend = DockerVolumeBackend::new(FakeDockerRunner::default(), "ployz");
        let mount = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: None,
                mode: None,
                owner: None,
            })
            .await
            .expect("ensure docker volume");

        assert_eq!(
            mount,
            VolumeMount::docker_volume(VolumeIdentity::new("prod", "data"), "ployz-prod-data")
        );
        assert!(
            !backend
                .info()
                .capabilities
                .contains(ployz_volume_api::VolumeCapability::Snapshot)
        );
        assert!(
            !backend
                .info()
                .capabilities
                .contains(ployz_volume_api::VolumeCapability::EnforceQuota)
        );
    }

    #[tokio::test]
    async fn ensure_surfaces_docker_command_failure() {
        let runner = FakeDockerRunner::default();
        runner.push_status(1, "permission denied");
        let backend = DockerVolumeBackend::new(runner, "ployz");

        let error = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: None,
                mode: None,
                owner: None,
            })
            .await
            .expect_err("docker failure should be visible");

        assert!(
            error
                .to_string()
                .contains("docker volume create: permission denied"),
            "got: {error}"
        );
    }

    #[tokio::test]
    async fn ensure_rejects_custom_mountpoint() {
        let backend = DockerVolumeBackend::new(FakeDockerRunner::default(), "ployz");

        let error = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: Some("/custom/path".into()),
                quota: None,
                mode: None,
                owner: None,
            })
            .await
            .expect_err("docker named volumes cannot honor custom mountpoints");

        assert_eq!(
            error.to_string(),
            "invalid volume request field 'mountpoint': docker-volume backend uses Docker-managed named volume mountpoints"
        );
    }

    #[tokio::test]
    async fn ensure_rejects_unenforced_filesystem_constraints() {
        let backend = DockerVolumeBackend::new(FakeDockerRunner::default(), "ployz");

        for (field, mut request) in [
            (
                "quota",
                EnsureVolumeRequest {
                    identity: VolumeIdentity::new("prod", "data"),
                    mountpoint: None,
                    quota: Some("1G".to_string()),
                    mode: None,
                    owner: None,
                },
            ),
            (
                "mode",
                EnsureVolumeRequest {
                    identity: VolumeIdentity::new("prod", "data"),
                    mountpoint: None,
                    quota: None,
                    mode: Some("0750".to_string()),
                    owner: None,
                },
            ),
            (
                "owner",
                EnsureVolumeRequest {
                    identity: VolumeIdentity::new("prod", "data"),
                    mountpoint: None,
                    quota: None,
                    mode: None,
                    owner: Some("999:999".to_string()),
                },
            ),
        ] {
            request.identity = VolumeIdentity::new("prod", field);
            let error = backend
                .ensure(request)
                .await
                .expect_err("docker named volumes cannot enforce filesystem constraints");

            assert!(
                error.to_string().contains(&format!("'{field}'")),
                "got: {error}"
            );
        }
        assert!(backend.runner.calls.lock().expect("calls").is_empty());
    }

    #[tokio::test]
    async fn inspect_missing_docker_volume_returns_none() {
        let runner = FakeDockerRunner::default();
        runner.push_status(1, "No such volume: ployz-prod-data");
        let backend = DockerVolumeBackend::new(runner, "ployz");

        let inspection = backend
            .inspect(VolumeIdentity::new("prod", "data"))
            .await
            .expect("inspect should treat missing volume as absent");

        assert!(inspection.is_none());
    }
}
