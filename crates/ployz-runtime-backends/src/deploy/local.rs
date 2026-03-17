use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use bollard::models::{PortBinding, PortMap};

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
};
use crate::runtime::labels::{self, WorkloadMeta, build_workload_labels};
use crate::runtime::{ContainerEngine, Probe, PullPolicy, RuntimeContainerSpec};
use crate::spec::{
    ContainerSpec, Namespace, NetworkMode, PortProtocol, ResourcesExt, ServicePort, ServiceSpec,
    VolumeSource,
};
use crate::store::DeployStore;

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(super) struct ManagedInstance {
    pub(super) instance_id: InstanceId,
    pub(super) service: String,
    pub(super) slot_id: SlotId,
    pub(super) machine_id: MachineId,
    pub(super) revision_hash: String,
    pub(super) deploy_id: DeployId,
    pub(super) docker_container_id: String,
    pub(super) ip_address: Option<IpAddr>,
    pub(super) backend_ports: BTreeMap<String, u16>,
}

pub(super) struct StartCandidate<'a> {
    pub(super) namespace: &'a Namespace,
    pub(super) spec: &'a ServiceSpec,
    pub(super) deploy_id: &'a DeployId,
    pub(super) instance_id: &'a InstanceId,
    pub(super) slot_id: &'a SlotId,
    pub(super) machine_id: &'a MachineId,
    pub(super) revision_hash: &'a str,
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

    async fn list_instances(&self, namespace: &Namespace) -> Result<Vec<ManagedInstance>> {
        let observed = self
            .engine
            .list_by_labels(&[
                (labels::LABEL_MANAGED, "true"),
                (labels::LABEL_NAMESPACE, &namespace.0),
            ])
            .await?;

        let mut instances = Vec::new();
        for obs in observed {
            let Some(instance_id) = obs.labels.get(labels::LABEL_INSTANCE) else {
                continue;
            };
            let Some(service) = obs.labels.get(labels::LABEL_SERVICE) else {
                continue;
            };
            let Some(slot_id) = obs.labels.get(labels::LABEL_SLOT) else {
                continue;
            };
            let Some(machine_id) = obs.labels.get(labels::LABEL_MACHINE) else {
                continue;
            };
            let Some(revision_hash) = obs.labels.get(labels::LABEL_REVISION) else {
                continue;
            };
            let Some(deploy_id) = obs.labels.get(labels::LABEL_DEPLOY) else {
                continue;
            };

            instances.push(ManagedInstance {
                instance_id: InstanceId(instance_id.clone()),
                service: service.clone(),
                slot_id: SlotId(slot_id.clone()),
                machine_id: MachineId(machine_id.clone()),
                revision_hash: revision_hash.clone(),
                deploy_id: DeployId(deploy_id.clone()),
                docker_container_id: obs.container_id,
                ip_address: obs.ip_address,
                backend_ports: BTreeMap::new(),
            });
        }

        Ok(instances)
    }

    pub(super) async fn start_candidate(
        &self,
        request: StartCandidate<'_>,
    ) -> Result<ManagedInstance> {
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

        let pull_policy = match spec.template.pull_policy {
            crate::spec::PullPolicy::Always => PullPolicy::Always,
            crate::spec::PullPolicy::IfNotPresent => PullPolicy::IfNotPresent,
            crate::spec::PullPolicy::Never => PullPolicy::Never,
        };

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
            pull_policy,
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

    pub(super) async fn wait_ready(
        &self,
        spec: &ServiceSpec,
        instance: &ManagedInstance,
    ) -> Result<()> {
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

pub(super) async fn adopt_instances(
    store: &StoreDriver,
    runtime: &LocalDeployRuntime,
    namespace: &Namespace,
) -> Result<()> {
    let existing = store.list_instance_status(namespace).await?;
    let known: BTreeSet<String> = existing
        .iter()
        .map(|record| record.instance_id.0.clone())
        .collect();
    for instance in runtime.list_instances(namespace).await? {
        if known.contains(&instance.instance_id.0) {
            continue;
        }
        store
            .upsert_instance_status(&InstanceStatusRecord {
                instance_id: instance.instance_id.clone(),
                namespace: namespace.clone(),
                service: instance.service.clone(),
                slot_id: instance.slot_id.clone(),
                machine_id: instance.machine_id.clone(),
                revision_hash: instance.revision_hash.clone(),
                deploy_id: instance.deploy_id.clone(),
                docker_container_id: instance.docker_container_id.clone(),
                overlay_ip: instance.ip_address.and_then(|ip| match ip {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                }),
                backend_ports: instance.backend_ports.clone(),
                phase: InstancePhase::Ready,
                ready: true,
                drain_state: crate::model::DrainState::None,
                error: None,
                started_at: now_unix_secs(),
                updated_at: now_unix_secs(),
            })
            .await?;
    }
    Ok(())
}

pub(super) fn build_instance_status_record(
    namespace: &Namespace,
    instance: &ManagedInstance,
    phase: InstancePhase,
    ready: bool,
    drain_state: DrainState,
    error: Option<String>,
) -> InstanceStatusRecord {
    InstanceStatusRecord {
        instance_id: instance.instance_id.clone(),
        namespace: namespace.clone(),
        service: instance.service.clone(),
        slot_id: instance.slot_id.clone(),
        machine_id: instance.machine_id.clone(),
        revision_hash: instance.revision_hash.clone(),
        deploy_id: instance.deploy_id.clone(),
        docker_container_id: instance.docker_container_id.clone(),
        overlay_ip: instance.ip_address.and_then(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        }),
        backend_ports: instance.backend_ports.clone(),
        phase,
        ready,
        drain_state,
        error,
        started_at: now_unix_secs(),
        updated_at: now_unix_secs(),
    }
}

pub(super) async fn list_local_instance_status(
    store: &StoreDriver,
    namespace: &Namespace,
    local_machine_id: &MachineId,
) -> Result<Vec<InstanceStatusRecord>> {
    Ok(store
        .list_instance_status(namespace)
        .await?
        .into_iter()
        .filter(|record| &record.machine_id == local_machine_id)
        .collect())
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

pub(super) use crate::time::now_unix_secs;
pub(super) use ployz_types::spec::stable_hash_hex;

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
