//! Privileged systemd machine-container creation and readiness.

use super::cluster::DindRunId;
use super::docker_api_error;
use super::exec::{exec_in_container, write_file_in_container};
use super::{ARTIFACTS_MOUNT_PATH, DindError, MANAGED_LABEL, MANAGED_LABEL_VALUE, RUN_LABEL};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, HostConfigCgroupnsModeEnum};
use bollard::query_parameters::{CreateContainerOptionsBuilder, InspectContainerOptions};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

const READINESS_BUDGET: Duration = Duration::from_secs(90);
const READINESS_INITIAL_DELAY: Duration = Duration::from_millis(250);
const READINESS_MAX_DELAY: Duration = Duration::from_secs(2);
const BRIDGE_IP_BUDGET: Duration = Duration::from_secs(10);
const BRIDGE_IP_DELAY: Duration = Duration::from_millis(250);
const BPFFS_PATH: &str = "/sys/fs/bpf";
const BPFFS_TYPE: &str = "bpf_fs";

/// Request for one role-neutral machine container.
#[derive(Debug, Clone)]
pub struct MachineSpec {
    pub image: String,
}

/// One running machine container.
#[derive(Debug, Clone)]
pub struct DindMachine {
    pub name: String,
    pub container_id: String,
    pub bridge_ip: IpAddr,
    /// The outer machine container's cgroup-v2 scope. This is the semantic
    /// machine root for root-cgroup eBPF attachment in the DinD harness.
    pub cgroup_root: String,
}

pub(super) async fn provision_machine(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &Path,
    spec: &MachineSpec,
    name: String,
) -> Result<DindMachine, DindError> {
    let MachineSpec { image } = spec;
    let options = CreateContainerOptionsBuilder::new().name(&name).build();
    let body = machine_create_body(run_id, network_name, artifact_dir, image, &name);
    let created = docker
        .create_container(Some(options), body)
        .await
        .map_err(docker_api_error("create machine container"))?;
    docker
        .start_container(&created.id, None)
        .await
        .map_err(docker_api_error("start machine container"))?;
    wait_for_machine_ready(docker, &name, &created.id).await?;
    ensure_bpffs_mounted(docker, &created.id, &name).await?;
    let bridge_ip = wait_for_bridge_ip(docker, &created.id, network_name, &name).await?;
    let cgroup_root = machine_cgroup_root(docker, &created.id, &name).await?;
    install_isolation_root_drop_in(docker, &created.id, &cgroup_root).await?;
    Ok(DindMachine {
        name,
        container_id: created.id,
        bridge_ip,
        cgroup_root,
    })
}

async fn ensure_bpffs_mounted(
    docker: &Docker,
    container_id: &str,
    name: &str,
) -> Result<(), DindError> {
    let observed = bpffs_type(docker, container_id, name).await?;
    if observed == BPFFS_TYPE {
        return Ok(());
    }
    let outcome = exec_in_container(
        docker,
        container_id,
        &["mount", "-t", "bpf", "bpf", BPFFS_PATH],
    )
    .await?;
    if !outcome.success() {
        return Err(DindError::BpffsUnavailable {
            machine: name.to_owned(),
            detail: format!(
                "mounting {BPFFS_PATH} over {observed:?} exited {}: {}",
                outcome.exit_code,
                outcome.stderr.trim()
            ),
        });
    }
    let mounted = bpffs_type(docker, container_id, name).await?;
    if mounted != BPFFS_TYPE {
        return Err(DindError::BpffsUnavailable {
            machine: name.to_owned(),
            detail: format!(
                "mount command succeeded but {BPFFS_PATH} has filesystem type {mounted:?}"
            ),
        });
    }
    Ok(())
}

async fn bpffs_type(docker: &Docker, container_id: &str, name: &str) -> Result<String, DindError> {
    let outcome = exec_in_container(
        docker,
        container_id,
        &["stat", "-f", "-c", "%T", BPFFS_PATH],
    )
    .await?;
    if !outcome.success() {
        return Err(DindError::BpffsUnavailable {
            machine: name.to_owned(),
            detail: format!(
                "inspecting {BPFFS_PATH} exited {}: {}",
                outcome.exit_code,
                outcome.stderr.trim()
            ),
        });
    }
    let filesystem_type = outcome.stdout.trim();
    if filesystem_type.is_empty() || filesystem_type.lines().count() != 1 {
        return Err(DindError::BpffsUnavailable {
            machine: name.to_owned(),
            detail: format!(
                "inspecting {BPFFS_PATH} returned malformed filesystem type {:?}",
                outcome.stdout
            ),
        });
    }
    Ok(filesystem_type.to_owned())
}

async fn install_isolation_root_drop_in(
    docker: &Docker,
    container_id: &str,
    cgroup_root: &str,
) -> Result<(), DindError> {
    let contents = format!("[Service]\nEnvironment=PLOYZ_ISOLATION_CGROUP_ROOT={cgroup_root}\n");
    write_file_in_container(
        docker,
        container_id,
        "/etc/systemd/system/ployzd-keeper.service.d/10-dind-isolation-root.conf",
        &contents,
        "0644",
    )
    .await
}

async fn machine_cgroup_root(
    docker: &Docker,
    container_id: &str,
    name: &str,
) -> Result<String, DindError> {
    let outcome = exec_in_container(docker, container_id, &["cat", "/proc/1/cgroup"]).await?;
    if !outcome.success() {
        return Err(DindError::MachineCgroupUnavailable {
            machine: name.to_owned(),
            detail: format!(
                "reading /proc/1/cgroup exited {}: {}",
                outcome.exit_code,
                outcome.stderr.trim()
            ),
        });
    }
    let root = parse_machine_cgroup_root(&outcome.stdout).map_err(|detail| {
        DindError::MachineCgroupUnavailable {
            machine: name.to_owned(),
            detail,
        }
    })?;
    let controllers = format!("{root}/cgroup.controllers");
    let outcome = exec_in_container(docker, container_id, &["test", "-f", &controllers]).await?;
    if !outcome.success() {
        return Err(DindError::MachineCgroupUnavailable {
            machine: name.to_owned(),
            detail: format!("{controllers} is not a cgroup-v2 controller file"),
        });
    }
    Ok(root)
}

fn parse_machine_cgroup_root(contents: &str) -> Result<String, String> {
    let matches = contents
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect::<Vec<_>>();
    let [pid_one_path] = matches.as_slice() else {
        return Err(format!(
            "expected exactly one cgroup-v2 PID 1 entry, got {matches:?}"
        ));
    };
    let Some(scope) = pid_one_path.strip_suffix("/init.scope") else {
        return Err(format!(
            "PID 1 cgroup path {pid_one_path:?} does not end in /init.scope"
        ));
    };
    if scope.is_empty() || scope == "/" {
        return Err("PID 1 cgroup resolved to the host root".to_owned());
    }
    Ok(format!("/sys/fs/cgroup{scope}"))
}

/// Verifies that a running Keeper received this DinD machine's isolated root.
pub async fn assert_keeper_isolation_root(
    docker: &Docker,
    machine: &DindMachine,
    unit: &str,
) -> Result<(), String> {
    if machine.cgroup_root == "/sys/fs/cgroup" {
        return Err(format!(
            "{} Keeper assertion refused the unified host cgroup root",
            machine.name
        ));
    }
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "systemctl",
            "show",
            unit,
            "--property",
            "Environment",
            "--value",
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    if !outcome.success() {
        return Err(format!(
            "{} could not inspect {unit} environment: exit {} stderr={:?}",
            machine.name,
            outcome.exit_code,
            outcome.stderr.trim()
        ));
    }
    let expected = format!("PLOYZ_ISOLATION_CGROUP_ROOT={}", machine.cgroup_root);
    if outcome
        .stdout
        .split_ascii_whitespace()
        .any(|value| value == expected)
    {
        Ok(())
    } else {
        Err(format!(
            "{} {unit} did not receive {expected:?}: {:?}",
            machine.name,
            outcome.stdout.trim()
        ))
    }
}

fn machine_create_body(
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &Path,
    image: &str,
    name: &str,
) -> ContainerCreateBody {
    ContainerCreateBody {
        hostname: Some(name.to_owned()),
        image: Some(image.to_owned()),
        cmd: Some(vec!["/sbin/init".to_owned()]),
        tty: Some(true),
        labels: Some(HashMap::from([
            (MANAGED_LABEL.to_owned(), MANAGED_LABEL_VALUE.to_owned()),
            (RUN_LABEL.to_owned(), run_id.as_str().to_owned()),
        ])),
        stop_signal: Some("SIGRTMIN+3".to_owned()),
        host_config: Some(HostConfig {
            privileged: Some(true),
            cgroupns_mode: Some(HostConfigCgroupnsModeEnum::HOST),
            network_mode: Some(network_name.to_owned()),
            binds: Some(vec![
                "/sys/fs/cgroup:/sys/fs/cgroup:rw".to_owned(),
                format!("{}:{ARTIFACTS_MOUNT_PATH}:ro", artifact_dir.display()),
            ]),
            tmpfs: Some(HashMap::from([
                ("/run".to_owned(), String::new()),
                ("/run/lock".to_owned(), String::new()),
            ])),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn wait_for_machine_ready(
    docker: &Docker,
    name: &str,
    container_id: &str,
) -> Result<(), DindError> {
    let deadline = Instant::now() + READINESS_BUDGET;
    let mut delay = READINESS_INITIAL_DELAY;
    let mut last_system_state;
    let mut last_docker_info = String::from("<unobserved>");
    loop {
        ensure_still_running(docker, name, container_id).await?;
        match exec_in_container(docker, container_id, &["systemctl", "is-system-running"]).await {
            Ok(outcome) => {
                let state = outcome.stdout.trim().to_owned();
                let system_ready = state == "running" || state == "degraded";
                last_system_state = state;
                if system_ready {
                    match exec_in_container(docker, container_id, &["docker", "info"]).await {
                        Ok(info) if info.success() => return Ok(()),
                        Ok(info) => {
                            last_docker_info = format!(
                                "exit {}: {}",
                                info.exit_code,
                                info.stderr.trim().to_owned()
                            );
                        }
                        Err(error) => last_docker_info = error.to_string(),
                    }
                }
            }
            Err(error) => last_system_state = error.to_string(),
        }
        if Instant::now() + delay >= deadline {
            return Err(DindError::MachineReadinessTimeout {
                machine: name.to_owned(),
                last_system_state,
                last_docker_info,
            });
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(READINESS_MAX_DELAY);
    }
}

async fn ensure_still_running(
    docker: &Docker,
    name: &str,
    container_id: &str,
) -> Result<(), DindError> {
    let inspected = docker
        .inspect_container(container_id, None::<InspectContainerOptions>)
        .await
        .map_err(docker_api_error("inspect machine container"))?;
    let running = inspected
        .state
        .as_ref()
        .and_then(|state| state.running)
        .unwrap_or(false);
    if running {
        Ok(())
    } else {
        Err(DindError::MachineExited {
            machine: name.to_owned(),
        })
    }
}

async fn wait_for_bridge_ip(
    docker: &Docker,
    container_id: &str,
    network_name: &str,
    name: &str,
) -> Result<IpAddr, DindError> {
    let deadline = Instant::now() + BRIDGE_IP_BUDGET;
    let mut last_detail;
    loop {
        let inspected = docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(docker_api_error("inspect machine container"))?;
        let raw_ip = inspected
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .and_then(|networks| networks.get(network_name))
            .and_then(|endpoint| endpoint.ip_address.clone());
        match raw_ip {
            Some(raw) if !raw.is_empty() => match raw.parse::<IpAddr>() {
                Ok(ip) => return Ok(ip),
                Err(error) => last_detail = format!("unparseable IP {raw:?}: {error}"),
            },
            Some(_empty) => last_detail = String::from("empty IP on network endpoint"),
            None => last_detail = String::from("network endpoint not reported yet"),
        }
        if Instant::now() + BRIDGE_IP_DELAY >= deadline {
            return Err(DindError::BridgeIpUnavailable {
                machine: name.to_owned(),
                detail: last_detail,
            });
        }
        tokio::time::sleep(BRIDGE_IP_DELAY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dind::{
        DindCluster, DindClusterSpec, artifact_dir, connect_docker, e2e_enabled, machine_image,
    };

    #[test]
    fn machine_body_is_role_neutral_and_publishes_no_ports() {
        let body = machine_create_body(
            &DindRunId::generate(),
            "ployz-test-network",
            Path::new("/tmp/ployz-artifacts"),
            "ployz-dind-machine:test",
            "ployz-test-machine",
        );

        assert_eq!(body.tty, Some(true));
        assert!(body.exposed_ports.is_none());
        assert!(
            body.host_config
                .expect("host config")
                .port_bindings
                .is_none()
        );
    }

    #[test]
    fn machine_root_is_the_parent_of_pid_one_init_scope() {
        assert_eq!(
            parse_machine_cgroup_root("0::/system.slice/docker-abc.scope/init.scope\n",),
            Ok("/sys/fs/cgroup/system.slice/docker-abc.scope".to_owned())
        );
    }

    #[test]
    fn machine_root_rejects_non_unified_or_unsafe_pid_one_paths() {
        for contents in [
            "1:name=systemd:/system.slice/docker-abc.scope/init.scope\n",
            "0::/system.slice/docker-abc.scope\n",
            "0::/init.scope\n",
            "0::/a/init.scope\n0::/b/init.scope\n",
        ] {
            assert!(parse_machine_cgroup_root(contents).is_err(), "{contents:?}");
        }
    }

    #[tokio::test]
    async fn provisioned_machine_mounts_bpffs() {
        if !e2e_enabled() {
            return;
        }
        let docker = connect_docker().expect("connect to Docker");
        let cluster = DindCluster::provision(
            &docker,
            DindClusterSpec {
                artifact_dir: artifact_dir(),
                machines: vec![MachineSpec {
                    image: machine_image(),
                }],
            },
        )
        .await
        .expect("provision one DinD machine");
        let [machine] = cluster.machines() else {
            panic!("one-machine fixture returned the wrong machine count");
        };
        let outcome = exec_in_container(
            &docker,
            &machine.container_id,
            &["stat", "-f", "-c", "%T", "/sys/fs/bpf"],
        )
        .await
        .expect("inspect /sys/fs/bpf filesystem type");
        let result = if outcome.success() && outcome.stdout.trim() == "bpf_fs" {
            Ok(())
        } else {
            Err(format!(
                "/sys/fs/bpf is not bpffs: exit={} stdout={:?} stderr={:?}",
                outcome.exit_code, outcome.stdout, outcome.stderr
            ))
        };
        cluster.teardown().await.expect("tear down DinD machine");
        result.expect("provisioned machine mounts private bpffs");
    }
}
