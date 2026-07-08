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
    DindMachine, DindMachineRole, MACHINE_GATEWAY_PORT, MACHINE_NATS_PORT, MachineSpec,
    PublishedPorts,
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

/// Image tag produced by `scripts/build-dind-machine-image.sh`.
pub const DEFAULT_MACHINE_IMAGE: &str = "ployz-dind-machine:local";
/// Artifact output of `scripts/build-dind-machine-image.sh` (release dir).
pub const DEFAULT_ARTIFACT_DIR: &str = "/tmp/ployz-dind-machine-target/release";
/// Read-only mount point of the host artifact dir inside every machine.
pub const ARTIFACTS_MOUNT_PATH: &str = "/opt/ployz/artifacts";

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
    /// Could not pre-reserve a loopback port for publishing.
    #[error("failed to reserve loopback port: {message}")]
    PortReservation { message: String },
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
