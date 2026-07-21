//! Docker-in-Docker e2e harness.
//!
//! Boots privileged systemd "machine" containers from the image built by
//! `scripts/build-dind-machine-image.sh` (plan C1), wired onto a per-run
//! bridge network with label-based cleanup. Modelled on uncloud's `ucind`
//! merged with the proven recipe from `scripts/local-dataplane-proof.sh`.
//!
//! Every Docker resource the harness creates carries
//! [`MANAGED_LABEL`]`=true` plus [`RUN_LABEL`]`=<run id>` so stale resources
//! from crashed runs are swept before provisioning and by
//! `scripts/dind-clean.sh`.
//!
//! Tests built on this module must early-return unless `PLOYZ_DIND_E2E=1`
//! (see [`e2e_enabled`]); `PLOYZ_DIND_KEEP=1` ([`keep_requested`]) keeps the
//! cluster alive after the test for debugging.

mod cluster;
mod evidence;
mod exec;
mod machine;

pub use cluster::{DindCluster, DindClusterSpec, DindRunId, sweep_managed_resources};
pub use evidence::{capture_machine_evidence, evidence_dir};
pub use exec::{
    ExecOutcome, exec_in_container, read_file_from_container, shell_quote, write_file_in_container,
};
pub use machine::{
    DindMachine, DindMachineRole, MACHINE_GATEWAY_PORT, MACHINE_GATEWAY_TLS_PORT,
    MACHINE_NATS_PORT, MachineSpec, PublishedPorts,
};

use bollard::Docker;
use std::env;
use std::path::PathBuf;

/// Marker label present on every Docker resource the harness creates.
pub const MANAGED_LABEL: &str = "dev.ployz.dind.managed";
/// Value of [`MANAGED_LABEL`].
pub const MANAGED_LABEL_VALUE: &str = "true";
/// Per-run label tying a resource to one [`DindRunId`].
pub const RUN_LABEL: &str = "dev.ployz.dind.run";

/// Gate env: tests early-return unless this is `1`.
pub const E2E_GATE_ENV: &str = "PLOYZ_DIND_E2E";
/// Debug env: when `1`, tests skip teardown and leave the cluster running.
pub const KEEP_ENV: &str = "PLOYZ_DIND_KEEP";
/// Override env for the machine image tag.
pub const MACHINE_IMAGE_ENV: &str = "PLOYZ_DIND_MACHINE_IMAGE";
/// Override env for the host directory holding linux ployz artifacts.
pub const ARTIFACT_DIR_ENV: &str = "PLOYZ_DIND_ARTIFACT_DIR";
/// Target platform selected by the DinD wrapper.
pub const PLATFORM_ENV: &str = "PLOYZ_DIND_PLATFORM";

/// Image tag produced by `scripts/build-dind-machine-image.sh`.
pub const DEFAULT_MACHINE_IMAGE: &str = "ployz-dind-machine:local";
/// Artifact output of `scripts/build-dind-machine-image.sh` (release dir).
pub const DEFAULT_ARTIFACT_DIR: &str = "/tmp/ployz-dind-machine-target/release";
/// Read-only mount point of the host artifact dir inside every machine.
pub const ARTIFACTS_MOUNT_PATH: &str = "/opt/ployz/artifacts";

/// Linux platform shared by the DinD image, artifacts, and release manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DindPlatform {
    LinuxAmd64,
    LinuxArm64,
}

impl DindPlatform {
    #[must_use]
    pub const fn release_slug(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "linux-amd64",
            Self::LinuxArm64 => "linux-arm64",
        }
    }

    fn from_docker_slug(value: &str) -> Self {
        match value {
            "linux/amd64" => Self::LinuxAmd64,
            "linux/arm64" => Self::LinuxArm64,
            unsupported => panic!("unsupported normalized DinD platform {unsupported}"),
        }
    }

    fn native() -> Self {
        match env::consts::ARCH {
            "x86_64" => Self::LinuxAmd64,
            "aarch64" => Self::LinuxArm64,
            unsupported => panic!("unsupported native DinD architecture {unsupported}"),
        }
    }
}

/// True when the gated DinD e2e suite is enabled (`PLOYZ_DIND_E2E=1`).
#[must_use]
pub fn e2e_enabled() -> bool {
    env::var(E2E_GATE_ENV).is_ok_and(|value| value == "1")
}

/// True when teardown should be skipped for debugging (`PLOYZ_DIND_KEEP=1`).
#[must_use]
pub fn keep_requested() -> bool {
    env::var(KEEP_ENV).is_ok_and(|value| value == "1")
}

/// Machine image tag, honoring [`MACHINE_IMAGE_ENV`].
#[must_use]
pub fn machine_image() -> String {
    env::var(MACHINE_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_MACHINE_IMAGE.to_owned())
}

/// Host artifact directory, honoring [`ARTIFACT_DIR_ENV`].
#[must_use]
pub fn artifact_dir() -> PathBuf {
    env::var(ARTIFACT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ARTIFACT_DIR))
}

/// Platform selected by the wrapper, or the native platform for direct
/// `PLOYZ_DIND_E2E=1 cargo test` invocations.
#[must_use]
pub fn platform() -> DindPlatform {
    match env::var(PLATFORM_ENV) {
        Ok(value) => DindPlatform::from_docker_slug(&value),
        Err(env::VarError::NotPresent) => DindPlatform::native(),
        Err(env::VarError::NotUnicode(_)) => panic!("{PLATFORM_ENV} must be valid UTF-8"),
    }
}

/// Connects the Docker client from the environment (`DOCKER_HOST` when set,
/// the local socket otherwise), so Docker Desktop / OrbStack contexts work.
pub fn connect_docker() -> Result<Docker, DindError> {
    Docker::connect_with_defaults().map_err(|source| DindError::DockerConnect {
        message: source.to_string(),
    })
}

/// Typed failures of the DinD harness.
#[derive(Debug, thiserror::Error)]
pub enum DindError {
    /// Could not build a Docker client from the environment.
    #[error("failed to connect to Docker: {message}")]
    DockerConnect { message: String },
    /// A Docker API call failed.
    #[error("docker api call failed ({context}): {message}")]
    DockerApi { context: String, message: String },
    /// The requested cluster shape is invalid (e.g. zero or two cores).
    #[error("invalid DinD cluster shape: {detail}")]
    ClusterShape { detail: String },
    /// Docker did not report a usable published loopback port.
    #[error("machine container has no published {container_port}/tcp port: {detail}")]
    PublishedPortUnavailable { container_port: u16, detail: String },
    /// A machine container exited before it became ready.
    #[error("machine container {machine} exited before readiness")]
    MachineExited { machine: String },
    /// A machine did not reach systemd + inner-docker readiness in budget.
    #[error(
        "machine {machine} not ready in budget \
         (last systemd state: {last_system_state}; \
         last inner docker info: {last_docker_info})"
    )]
    MachineReadinessTimeout {
        machine: String,
        last_system_state: String,
        last_docker_info: String,
    },
    /// A started machine never reported a bridge IP on the cluster network.
    #[error("machine {machine} has no bridge IP: {detail}")]
    BridgeIpUnavailable { machine: String, detail: String },
    /// An exec did not finish within the per-exec budget.
    #[error("exec in {container} timed out: {command}")]
    ExecTimeout { container: String, command: String },
    /// Docker started the exec detached even though we asked for output.
    #[error("exec in {container} unexpectedly started detached")]
    ExecDetached { container: String },
    /// The finished exec carried no exit code.
    #[error("exec in {container} finished without exit code: {command}")]
    ExecExitCodeMissing { container: String, command: String },
    /// Writing evidence files failed.
    #[error("failed to write evidence {}: {message}", path.display())]
    EvidenceIo { path: PathBuf, message: String },
}

pub(crate) fn docker_api_error(
    context: &str,
) -> impl FnOnce(bollard::errors::Error) -> DindError + use<> {
    let context = context.to_owned();
    move |source| DindError::DockerApi {
        context,
        message: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::DindPlatform;

    #[test]
    fn dind_platforms_have_release_manifest_slugs() {
        assert_eq!(
            DindPlatform::from_docker_slug("linux/amd64").release_slug(),
            "linux-amd64"
        );
        assert_eq!(
            DindPlatform::from_docker_slug("linux/arm64").release_slug(),
            "linux-arm64"
        );
        assert!(matches!(
            DindPlatform::native(),
            DindPlatform::LinuxAmd64 | DindPlatform::LinuxArm64
        ));
    }

    #[test]
    #[should_panic(expected = "unsupported normalized DinD platform linux/riscv64")]
    fn unsupported_dind_platform_fails_loudly() {
        let _ = DindPlatform::from_docker_slug("linux/riscv64");
    }
}
