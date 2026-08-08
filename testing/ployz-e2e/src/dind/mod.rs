//! Role-neutral Docker-in-Docker harness with label-scoped cleanup.

mod cluster;
mod evidence;
mod exec;
mod founding;
mod machine;
mod operation;

pub use cluster::{DindCluster, DindClusterSpec, DindRunId, sweep_managed_resources};
pub use evidence::{capture_machine_evidence, evidence_dir};
pub use exec::{
    ExecOutcome, exec_in_container, read_file_from_container, shell_quote, write_file_in_container,
};
pub use founding::{
    ARTIFACT_ROOT, RELEASE_MANIFEST, corrosion_access, corrosion_query, env_value, exec_ok,
    install_local_release_channel, render_release_manifest, require, write_release_manifest,
};
pub use machine::{DindMachine, MachineSpec, assert_keeper_isolation_root};
pub use operation::{
    FOUNDER_NAME, JoinedMachine, OperatorFixture, REGISTRY_PORT,
    assert_cluster_wide_operation_replay, assert_dns_and_http,
    assert_driver_local_evidence_is_secret_free, assert_first_revision_container_is_gone,
    assert_gateway_http, create_namespace, create_namespace_and_deploy, fetch_gateway_http,
    found_and_join, found_and_join_with_service_urls, gateway_status, parse_deploy_operation,
    public_lens, push_second_revision, require_success, run_cli, spawn_deploy,
    start_mutable_registry, wait_for_gateway_status,
};

use bollard::Docker;
use std::env;
use std::path::PathBuf;

pub const MANAGED_LABEL: &str = "dev.ployz.dind.managed";
pub const MANAGED_LABEL_VALUE: &str = "true";
pub const RUN_LABEL: &str = "dev.ployz.dind.run";
pub const E2E_GATE_ENV: &str = "PLOYZ_DIND_E2E";
pub const KEEP_ENV: &str = "PLOYZ_DIND_KEEP";
pub const MACHINE_IMAGE_ENV: &str = "PLOYZ_DIND_MACHINE_IMAGE";
pub const ARTIFACT_DIR_ENV: &str = "PLOYZ_DIND_ARTIFACT_DIR";
pub const DEFAULT_MACHINE_IMAGE: &str = "ployz-dind-machine:local";
pub const DEFAULT_ARTIFACT_DIR: &str = "/tmp/ployz-dind-machine-target/release";
pub const ARTIFACTS_MOUNT_PATH: &str = "/opt/ployz/artifacts";

#[must_use]
pub fn e2e_enabled() -> bool {
    env::var(E2E_GATE_ENV).is_ok_and(|value| value == "1")
}

#[must_use]
pub fn keep_requested() -> bool {
    env::var(KEEP_ENV).is_ok_and(|value| value == "1")
}

#[must_use]
pub fn machine_image() -> String {
    env::var(MACHINE_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_MACHINE_IMAGE.to_owned())
}

#[must_use]
pub fn artifact_dir() -> PathBuf {
    env::var(ARTIFACT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ARTIFACT_DIR))
}

pub fn connect_docker() -> Result<Docker, DindError> {
    Docker::connect_with_defaults().map_err(|source| DindError::DockerConnect {
        message: source.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum DindError {
    #[error("failed to connect to Docker: {message}")]
    DockerConnect { message: String },
    #[error("docker api call failed ({context}): {message}")]
    DockerApi { context: String, message: String },
    #[error("machine container {machine} exited before readiness")]
    MachineExited { machine: String },
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
    #[error("machine {machine} has no bridge IP: {detail}")]
    BridgeIpUnavailable { machine: String, detail: String },
    #[error("machine {machine} has no isolated cgroup-v2 root: {detail}")]
    MachineCgroupUnavailable { machine: String, detail: String },
    #[error("machine {machine} has no usable bpffs mount: {detail}")]
    BpffsUnavailable { machine: String, detail: String },
    #[error("exec in {container} timed out: {command}")]
    ExecTimeout { container: String, command: String },
    #[error("exec in {container} unexpectedly started detached")]
    ExecDetached { container: String },
    #[error("exec in {container} finished without exit code: {command}")]
    ExecExitCodeMissing { container: String, command: String },
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
