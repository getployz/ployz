//! One privileged systemd machine container: creation, port publishing, and
//! readiness (systemd reached `running`/`degraded` AND the inner Docker
//! daemon answers `docker info`).

use super::cluster::DindRunId;
use super::docker_api_error;
use super::exec::exec_in_container;
use super::{ARTIFACTS_MOUNT_PATH, DindError, MANAGED_LABEL, MANAGED_LABEL_VALUE, RUN_LABEL};
use bollard::Docker;
use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigCgroupnsModeEnum, PortBinding, PortMap,
};
use bollard::query_parameters::{CreateContainerOptionsBuilder, InspectContainerOptions};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::time::{Duration, Instant};

/// NATS client port inside every machine container.
pub const MACHINE_NATS_PORT: u16 = 4222;
/// Gateway route port inside every machine container: the Host Runner
/// renders the gateway role env with its default listen address, port 80.
pub const MACHINE_GATEWAY_PORT: u16 = 80;
pub const MACHINE_GATEWAY_TLS_PORT: u16 = 443;

/// Total budget for systemd + inner dockerd readiness.
const READINESS_BUDGET: Duration = Duration::from_secs(90);
const READINESS_INITIAL_DELAY: Duration = Duration::from_millis(250);
const READINESS_MAX_DELAY: Duration = Duration::from_secs(2);

/// Budget for the started container to report its bridge IP.
const BRIDGE_IP_BUDGET: Duration = Duration::from_secs(10);
const BRIDGE_IP_DELAY: Duration = Duration::from_millis(250);

/// Role a machine plays in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DindMachineRole {
    Core,
    Edge,
}

/// Request for one machine container.
#[derive(Debug, Clone)]
pub struct MachineSpec {
    pub role: DindMachineRole,
    pub image: String,
}

/// Host-side `127.0.0.1` ports published into the machine.
///
/// Ports are pre-reserved explicitly (bind-then-close) and pinned in the
/// container's port bindings, because Docker re-randomizes auto-published
/// ports across container restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedPorts {
    /// Maps to [`MACHINE_NATS_PORT`] inside the machine.
    pub nats: SocketAddr,
    /// Maps to [`MACHINE_GATEWAY_PORT`] inside the machine.
    pub gateway: SocketAddr,
    /// Maps to [`MACHINE_GATEWAY_TLS_PORT`] inside the machine.
    pub gateway_tls: SocketAddr,
}

impl PublishedPorts {
    pub(super) fn reserve() -> Result<Self, DindError> {
        let nats_listener = bind_loopback()?;
        let gateway_listener = bind_loopback()?;
        let gateway_tls_listener = bind_loopback()?;
        let nats = local_addr(&nats_listener)?;
        let gateway = local_addr(&gateway_listener)?;
        let gateway_tls = local_addr(&gateway_tls_listener)?;
        drop(nats_listener);
        drop(gateway_listener);
        drop(gateway_tls_listener);
        Ok(Self {
            nats,
            gateway,
            gateway_tls,
        })
    }
}

fn bind_loopback() -> Result<TcpListener, DindError> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|source| DindError::PortReservation {
        message: source.to_string(),
    })
}

fn local_addr(listener: &TcpListener) -> Result<SocketAddr, DindError> {
    listener
        .local_addr()
        .map_err(|source| DindError::PortReservation {
            message: source.to_string(),
        })
}

/// One running machine container.
#[derive(Debug, Clone)]
pub struct DindMachine {
    pub name: String,
    pub container_id: String,
    pub bridge_ip: IpAddr,
    pub published: PublishedPorts,
}

pub(super) async fn provision_machine(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &Path,
    spec: &MachineSpec,
    name: String,
) -> Result<DindMachine, DindError> {
    // Role only drives naming, which the cluster already resolved into `name`.
    let MachineSpec { role: _, image } = spec;
    let published = PublishedPorts::reserve()?;
    let options = CreateContainerOptionsBuilder::new().name(&name).build();
    let body = machine_create_body(run_id, network_name, artifact_dir, image, &name, published);
    let created = docker
        .create_container(Some(options), body)
        .await
        .map_err(docker_api_error("create machine container"))?;
    docker
        .start_container(&created.id, None)
        .await
        .map_err(docker_api_error("start machine container"))?;
    wait_for_machine_ready(docker, &name, &created.id).await?;
    let bridge_ip = wait_for_bridge_ip(docker, &created.id, network_name, &name).await?;
    Ok(DindMachine {
        name,
        container_id: created.id,
        bridge_ip,
        published,
    })
}

fn machine_create_body(
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &Path,
    image: &str,
    name: &str,
    published: PublishedPorts,
) -> ContainerCreateBody {
    let nats_port_key = format!("{MACHINE_NATS_PORT}/tcp");
    let gateway_port_key = format!("{MACHINE_GATEWAY_PORT}/tcp");
    let gateway_tls_port_key = format!("{MACHINE_GATEWAY_TLS_PORT}/tcp");
    let port_bindings: PortMap = HashMap::from([
        (
            nats_port_key.clone(),
            Some(vec![loopback_binding(published.nats)]),
        ),
        (
            gateway_port_key.clone(),
            Some(vec![loopback_binding(published.gateway)]),
        ),
        (
            gateway_tls_port_key.clone(),
            Some(vec![loopback_binding(published.gateway_tls)]),
        ),
    ]);
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
        exposed_ports: Some(vec![nats_port_key, gateway_port_key, gateway_tls_port_key]),
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
            port_bindings: Some(port_bindings),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn loopback_binding(address: SocketAddr) -> PortBinding {
    PortBinding {
        host_ip: Some(address.ip().to_string()),
        host_port: Some(address.port().to_string()),
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

    #[test]
    fn machine_console_is_attached_to_docker_logs() {
        let body = machine_create_body(
            &DindRunId::generate(),
            "ployz-test-network",
            Path::new("/tmp/ployz-artifacts"),
            "ployz-dind-machine:test",
            "ployz-test-machine",
            PublishedPorts {
                nats: SocketAddr::from((Ipv4Addr::LOCALHOST, 14222)),
                gateway: SocketAddr::from((Ipv4Addr::LOCALHOST, 18080)),
                gateway_tls: SocketAddr::from((Ipv4Addr::LOCALHOST, 18443)),
            },
        );

        assert_eq!(body.tty, Some(true));
    }
}
