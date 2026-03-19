use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use bollard::models::{PortBinding, PortMap};

use crate::error::{Error, Result};
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
};
use crate::runtime::labels::{self, WorkloadMeta, build_workload_labels, extract_workload_labels};
use crate::runtime::{ContainerEngine, Probe, RuntimeContainerSpec};
use crate::spec::{
    ContainerSpec, Namespace, NetworkMode, PortProtocol, ServicePort, ServiceSpec, VolumeSource,
};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ManagedInstance {
    pub instance_id: InstanceId,
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub revision_hash: String,
    pub deploy_id: DeployId,
    pub docker_container_id: String,
    pub ip_address: Option<IpAddr>,
    pub backend_ports: BTreeMap<String, u16>,
}

impl ManagedInstance {
    pub fn to_status_record(
        &self,
        namespace: &Namespace,
        phase: InstancePhase,
        ready: bool,
        drain_state: DrainState,
        error: Option<String>,
    ) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: self.instance_id.clone(),
            namespace: namespace.clone(),
            service: self.service.clone(),
            slot_id: self.slot_id.clone(),
            machine_id: self.machine_id.clone(),
            revision_hash: self.revision_hash.clone(),
            deploy_id: self.deploy_id.clone(),
            docker_container_id: self.docker_container_id.clone(),
            overlay_ip: self.ip_address.and_then(|ip| match ip {
                IpAddr::V4(v4) => Some(v4),
                IpAddr::V6(_) => None,
            }),
            backend_ports: self.backend_ports.clone(),
            phase,
            ready,
            drain_state,
            error,
            started_at: now_unix_secs(),
            updated_at: now_unix_secs(),
        }
    }
}

pub struct StartCandidate<'a> {
    pub namespace: &'a Namespace,
    pub spec: &'a ServiceSpec,
    pub deploy_id: &'a DeployId,
    pub instance_id: &'a InstanceId,
    pub slot_id: &'a SlotId,
    pub machine_id: &'a MachineId,
    pub revision_hash: &'a str,
}

pub struct LocalDeployRuntime {
    engine: ContainerEngine,
    overlay_network: Option<String>,
    overlay_dns_server: Option<Ipv4Addr>,
}

impl LocalDeployRuntime {
    pub fn new(
        overlay_network: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
    ) -> Result<Self> {
        let docker = bollard::Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;
        let engine = ContainerEngine::new(docker);
        Ok(Self {
            engine,
            overlay_network,
            overlay_dns_server,
        })
    }

    pub async fn list_instances(&self, namespace: &Namespace) -> Result<Vec<ManagedInstance>> {
        let observed = self
            .engine
            .list_by_labels(&[
                (labels::LABEL_MANAGED, "true"),
                (labels::LABEL_NAMESPACE, &namespace.0),
            ])
            .await?;

        let mut instances = Vec::new();
        for obs in observed {
            let Some(wl) = extract_workload_labels(&obs.labels) else {
                continue;
            };
            instances.push(ManagedInstance {
                instance_id: InstanceId(wl.instance_id),
                service: wl.service,
                slot_id: SlotId(wl.slot_id),
                machine_id: MachineId(wl.machine_id),
                revision_hash: wl.revision_hash,
                deploy_id: DeployId(wl.deploy_id),
                docker_container_id: obs.container_id,
                ip_address: obs.ip_address,
                backend_ports: BTreeMap::new(),
            });
        }

        Ok(instances)
    }

    pub async fn start_candidate(&self, request: StartCandidate<'_>) -> Result<ManagedInstance> {
        let StartCandidate {
            namespace,
            spec,
            deploy_id,
            instance_id,
            slot_id,
            machine_id,
            revision_hash,
        } = request;
        let container_name = format!("ployz-{namespace}-{}-{}", spec.name, instance_id.0);
        let key = format!("{namespace}/{}/{}/{}", spec.name, slot_id.0, instance_id.0);

        let meta = WorkloadMeta {
            namespace: &namespace.0,
            service: &spec.name,
            revision: revision_hash,
            deploy_id: &deploy_id.0,
            instance_id: &instance_id.0,
            slot_id: &slot_id.0,
            machine_id: &machine_id.0,
        };
        let workload_labels = build_workload_labels(&key, &meta, &spec.labels);

        let container = &spec.template;

        let env: Vec<(String, String)> = container
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let network_mode = match &spec.network {
            NetworkMode::Host => Some("host".to_string()),
            NetworkMode::None => Some("none".to_string()),
            NetworkMode::Service(service) => Some(format!("container:ployz-{namespace}-{service}")),
            NetworkMode::Overlay => self.overlay_network.clone(),
        };

        let dns_servers = workload_dns_servers(&spec.network, self.overlay_dns_server);

        let runtime_spec = RuntimeContainerSpec {
            key,
            container_name: container_name.clone(),
            image: container.image.clone(),
            pull_policy: container.pull_policy,
            cmd: container.command.clone(),
            entrypoint: container.entrypoint.clone(),
            env,
            labels: workload_labels,
            binds: build_binds(container),
            tmpfs: build_tmpfs(container),
            dns_servers,
            network_mode,
            port_bindings: build_port_bindings(spec)?,
            exposed_ports: None,
            cap_add: container.cap_add.clone(),
            cap_drop: container.cap_drop.clone(),
            privileged: container.privileged,
            user: container.user.clone(),
            restart_policy: Some(build_restart_policy(&spec.restart)),
            memory_bytes: container.resources.memory_bytes.map(|v| v as i64),
            nano_cpus: container.resources.cpu_nano(),
            sysctls: container.sysctls.clone().into_iter().collect(),
            stop_timeout: spec
                .stop_grace_period
                .as_ref()
                .and_then(|v| parse_duration_secs(v)),
            pid_mode: None,
        };

        let result = self.engine.ensure(&runtime_spec).await?;

        Ok(ManagedInstance {
            instance_id: instance_id.clone(),
            service: spec.name.clone(),
            slot_id: slot_id.clone(),
            machine_id: machine_id.clone(),
            revision_hash: revision_hash.to_string(),
            deploy_id: deploy_id.clone(),
            docker_container_id: result.container_id,
            ip_address: result.ip_address,
            backend_ports: service_port_map(&spec.service_ports),
        })
    }

    pub async fn wait_ready(&self, spec: &ServiceSpec, instance: &ManagedInstance) -> Result<()> {
        let Some(readiness) = &spec.readiness else {
            return Ok(());
        };

        let Some(ip_address) = instance.ip_address else {
            return Err(Error::operation(
                "wait_ready",
                format!(
                    "instance '{}' for service '{}' has no reachable IP address",
                    instance.instance_id, spec.name
                ),
            ));
        };

        let probe = match &readiness.check {
            crate::spec::ReadinessCheck::Tcp { service_port } => Probe::Tcp {
                host: ip_address,
                port: resolve_service_port(spec, service_port)?,
            },
            crate::spec::ReadinessCheck::Http { service_port, path } => Probe::Http {
                host: ip_address,
                port: resolve_service_port(spec, service_port)?,
                path: path.clone(),
            },
            crate::spec::ReadinessCheck::Exec { command } => Probe::Exec {
                container_id: instance.docker_container_id.clone(),
                command: command.clone(),
            },
        };

        let interval = readiness
            .interval_duration()
            .map_err(|error| Error::operation("wait_ready", error))?;
        let timeout = readiness
            .timeout_duration()
            .map_err(|error| Error::operation("wait_ready", error))?;
        let start_period = readiness
            .start_period_duration()
            .map_err(|error| Error::operation("wait_ready", error))?;
        let retries = readiness.retries();

        self.engine
            .probe_runner()
            .wait_ready(&probe, start_period, interval, timeout, retries)
            .await
            .map_err(|_| {
                Error::operation(
                    "wait_ready",
                    format!(
                        "instance '{}' for service '{}' did not become ready before timeout",
                        instance.instance_id, spec.name
                    ),
                )
            })
    }

    pub async fn remove_instance(
        &self,
        instance_id: &InstanceId,
        namespace: &Namespace,
        service: &str,
    ) -> Result<()> {
        let container_name = format!("ployz-{namespace}-{service}-{}", instance_id.0);
        self.engine.remove(&container_name, STOP_GRACE_PERIOD).await
    }
}

fn service_port_map(service_ports: &[ServicePort]) -> BTreeMap<String, u16> {
    service_ports
        .iter()
        .map(|port| (port.name.clone(), port.container_port))
        .collect()
}

fn resolve_service_port(spec: &ServiceSpec, name: &str) -> Result<u16> {
    spec.service_ports
        .iter()
        .find(|port| port.name == name)
        .map(|port| port.container_port)
        .ok_or_else(|| {
            Error::operation(
                "resolve_service_port",
                format!(
                    "service '{}' has no service port named '{}'",
                    spec.name, name
                ),
            )
        })
}

fn build_binds(container: &ContainerSpec) -> Vec<String> {
    container
        .volumes
        .iter()
        .filter_map(|mount| match &mount.source {
            VolumeSource::Bind(source) => {
                let ro = if mount.readonly { ":ro" } else { "" };
                Some(format!("{source}:{}{ro}", mount.target))
            }
            VolumeSource::Managed(volume) => {
                let ro = if mount.readonly { ":ro" } else { "" };
                Some(format!("{}:{}{ro}", volume.name, mount.target))
            }
            VolumeSource::Tmpfs => None,
        })
        .collect()
}

fn build_tmpfs(container: &ContainerSpec) -> HashMap<String, String> {
    container
        .volumes
        .iter()
        .filter(|mount| matches!(mount.source, VolumeSource::Tmpfs))
        .map(|mount| (mount.target.clone(), String::new()))
        .collect()
}

fn build_port_bindings(spec: &ServiceSpec) -> Result<Option<PortMap>> {
    if spec.publish.is_empty() {
        return Ok(None);
    }

    let mut bindings = PortMap::new();
    for published in &spec.publish {
        let service_port = spec
            .service_ports
            .iter()
            .find(|port| port.name == published.service_port)
            .ok_or_else(|| {
                Error::operation(
                    "build_port_bindings",
                    format!(
                        "service '{}' publishes unknown port '{}'",
                        spec.name, published.service_port
                    ),
                )
            })?;
        let protocol = match service_port.protocol {
            PortProtocol::Tcp => "tcp",
            PortProtocol::Udp => "udp",
        };
        let key = format!("{}/{}", service_port.container_port, protocol);
        let binding = PortBinding {
            host_ip: published.host_ip.clone(),
            host_port: Some(published.host_port.to_string()),
        };
        let Some(items) = bindings
            .entry(key)
            .or_insert_with(|| Some(Vec::new()))
            .as_mut()
        else {
            return Err(Error::operation(
                "build_port_bindings",
                "port binding entry did not contain a binding list",
            ));
        };
        items.push(binding);
    }

    Ok(Some(bindings))
}

fn workload_dns_servers(
    network: &NetworkMode,
    overlay_dns_server: Option<Ipv4Addr>,
) -> Vec<String> {
    match network {
        NetworkMode::Overlay => overlay_dns_server
            .map(|ip| vec![ip.to_string()])
            .unwrap_or_default(),
        NetworkMode::Host | NetworkMode::None | NetworkMode::Service(_) => Vec::new(),
    }
}

fn build_restart_policy(policy: &crate::spec::RestartPolicy) -> bollard::models::RestartPolicy {
    let name = match policy {
        crate::spec::RestartPolicy::No => bollard::models::RestartPolicyNameEnum::NO,
        crate::spec::RestartPolicy::Always => bollard::models::RestartPolicyNameEnum::ALWAYS,
        crate::spec::RestartPolicy::OnFailure => bollard::models::RestartPolicyNameEnum::ON_FAILURE,
        crate::spec::RestartPolicy::UnlessStopped => {
            bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED
        }
    };
    bollard::models::RestartPolicy {
        name: Some(name),
        maximum_retry_count: Some(0),
    }
}

fn parse_duration_secs(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_suffix('s') {
        return rest.trim().parse().ok();
    }
    if let Some(rest) = trimmed.strip_suffix('m') {
        return rest.trim().parse::<i64>().ok().map(|minutes| minutes * 60);
    }
    trimmed.parse().ok()
}

pub use crate::time::now_unix_secs;

#[cfg(test)]
mod tests {
    use super::workload_dns_servers;
    use crate::spec::NetworkMode;
    use std::net::Ipv4Addr;

    #[test]
    fn overlay_network_uses_overlay_dns_server() {
        let dns_servers =
            workload_dns_servers(&NetworkMode::Overlay, Some(Ipv4Addr::new(10, 210, 0, 2)));
        assert_eq!(dns_servers, vec!["10.210.0.2"]);
    }

    #[test]
    fn non_overlay_networks_do_not_use_overlay_dns_server() {
        let dns_server = Some(Ipv4Addr::new(10, 210, 0, 2));
        assert!(workload_dns_servers(&NetworkMode::Host, dns_server).is_empty());
        assert!(workload_dns_servers(&NetworkMode::None, dns_server).is_empty());
        assert!(workload_dns_servers(&NetworkMode::Service("db".into()), dns_server).is_empty());
    }
}
