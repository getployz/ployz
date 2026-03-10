use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::stream::StreamExt;
use reqwest::StatusCode;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
};
use crate::spec::{
    ContainerSpec, Namespace, NetworkMode, PortProtocol, PullPolicy, ReadinessProbe,
    ResourcesExt, ServicePort, ServiceSpec, VolumeSource,
};
use crate::store::DeployStore;

const LABEL_MANAGED: &str = "dev.ployz.managed";
const LABEL_NAMESPACE: &str = "dev.ployz.namespace";
const LABEL_SERVICE: &str = "dev.ployz.service";
const LABEL_REVISION: &str = "dev.ployz.revision";
const LABEL_DEPLOY: &str = "dev.ployz.deploy";
const LABEL_INSTANCE: &str = "dev.ployz.instance";
const LABEL_SLOT: &str = "dev.ployz.slot";
const LABEL_MACHINE: &str = "dev.ployz.machine";

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

pub struct LocalDeployRuntime {
    docker: Docker,
    overlay_network: Option<String>,
}

impl LocalDeployRuntime {
    pub fn new(overlay_network: Option<String>) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;
        Ok(Self {
            docker,
            overlay_network,
        })
    }

    async fn list_instances(&self, namespace: &Namespace) -> Result<Vec<ManagedInstance>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_NAMESPACE}={}", namespace.0),
            ],
        );
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| Error::operation("list_instances", e.to_string()))?;

        let mut instances = Vec::new();
        for container in containers {
            let labels = container.labels.unwrap_or_default();
            let Some(instance_id) = labels.get(LABEL_INSTANCE) else {
                continue;
            };
            let Some(service) = labels.get(LABEL_SERVICE) else {
                continue;
            };
            let Some(slot_id) = labels.get(LABEL_SLOT) else {
                continue;
            };
            let Some(machine_id) = labels.get(LABEL_MACHINE) else {
                continue;
            };
            let Some(revision_hash) = labels.get(LABEL_REVISION) else {
                continue;
            };
            let Some(deploy_id) = labels.get(LABEL_DEPLOY) else {
                continue;
            };

            let ip_address = container
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                .and_then(|networks| {
                    networks
                        .values()
                        .find_map(|network| network.ip_address.as_ref())
                        .and_then(|ip| ip.parse::<IpAddr>().ok())
                });

            instances.push(ManagedInstance {
                instance_id: InstanceId(instance_id.clone()),
                service: service.clone(),
                slot_id: SlotId(slot_id.clone()),
                machine_id: MachineId(machine_id.clone()),
                revision_hash: revision_hash.clone(),
                deploy_id: DeployId(deploy_id.clone()),
                docker_container_id: container.id.unwrap_or_default(),
                ip_address,
                backend_ports: BTreeMap::new(),
            });
        }

        Ok(instances)
    }

    pub(super) async fn start_candidate(
        &self,
        spec: &ServiceSpec,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
        slot_id: &SlotId,
        machine_id: &MachineId,
        revision_hash: &str,
    ) -> Result<ManagedInstance> {
        let container_name = format!("ployz-{}-{}-{}", spec.namespace, spec.name, instance_id.0);

        match spec.template.pull_policy {
            PullPolicy::Always => self.pull_image(&spec.template.image).await?,
            PullPolicy::IfNotPresent => {
                if !self.image_exists(&spec.template.image).await {
                    self.pull_image(&spec.template.image).await?;
                }
            }
            PullPolicy::Never => {}
        }

        let config = self.build_container_config(
            spec,
            deploy_id,
            instance_id,
            slot_id,
            machine_id,
            revision_hash,
        )?;
        let options = CreateContainerOptionsBuilder::default()
            .name(&container_name)
            .build();

        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| Error::operation("start_candidate", format!("create container: {e}")))?;

        self.docker
            .start_container(&container_name, None)
            .await
            .map_err(|e| Error::operation("start_candidate", format!("start container: {e}")))?;

        let inspect = self
            .docker
            .inspect_container(&container_name, None)
            .await
            .map_err(|e| Error::operation("start_candidate", format!("inspect container: {e}")))?;

        let ip_address = inspect
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .and_then(|networks| {
                networks
                    .values()
                    .find_map(|network| network.ip_address.as_ref())
                    .and_then(|ip| ip.parse::<IpAddr>().ok())
            });

        Ok(ManagedInstance {
            instance_id: instance_id.clone(),
            service: spec.name.clone(),
            slot_id: slot_id.clone(),
            machine_id: machine_id.clone(),
            revision_hash: revision_hash.to_string(),
            deploy_id: deploy_id.clone(),
            docker_container_id: inspect.id.unwrap_or_default(),
            ip_address,
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

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ready = match readiness {
                ReadinessProbe::Tcp { service_port } => {
                    probe_tcp(ip_address, resolve_service_port(spec, service_port)?).await
                }
                ReadinessProbe::Http { service_port, path } => {
                    probe_http(ip_address, resolve_service_port(spec, service_port)?, path).await
                }
                ReadinessProbe::Exec { command } => {
                    self.probe_exec(&instance.docker_container_id, command)
                        .await?
                }
            };

            if ready {
                return Ok(());
            }

            if Instant::now() >= deadline {
                return Err(Error::operation(
                    "wait_ready",
                    format!(
                        "instance '{}' for service '{}' did not become ready before timeout",
                        instance.instance_id, spec.name
                    ),
                ));
            }

            sleep(Duration::from_millis(250)).await;
        }
    }

    async fn probe_exec(&self, container_id: &str, command: &[String]) -> Result<bool> {
        let options = CreateExecOptions {
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            cmd: Some(command.to_vec()),
            ..Default::default()
        };
        let exec = self
            .docker
            .create_exec(container_id, options)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("create exec: {e}")))?;
        let result = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("start exec: {e}")))?;

        match result {
            StartExecResults::Attached { mut output, .. } => while output.next().await.is_some() {},
            StartExecResults::Detached => {}
        }

        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("inspect exec: {e}")))?;
        Ok(inspect.exit_code == Some(0))
    }

    pub async fn remove_instance(
        &self,
        instance_id: &InstanceId,
        namespace: &Namespace,
        service: &str,
    ) -> Result<()> {
        let container_name = format!("ployz-{namespace}-{service}-{}", instance_id.0);
        // Graceful stop: sends SIGTERM and waits for the container's stop_timeout
        // (set from stop_grace_period at creation time) before SIGKILL.
        let stop_opts = StopContainerOptionsBuilder::default().build();
        match self.docker.stop_container(&container_name, Some(stop_opts)).await {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 304, .. }) => {
                // Container already stopped
            }
            Err(e) => return Err(Error::operation("remove_instance", e.to_string())),
        }
        let remove_opts = RemoveContainerOptionsBuilder::default().build();
        self.docker
            .remove_container(&container_name, Some(remove_opts))
            .await
            .map_err(|e| Error::operation("remove_instance", e.to_string()))?;
        Ok(())
    }

    fn build_container_config(
        &self,
        spec: &ServiceSpec,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
        slot_id: &SlotId,
        machine_id: &MachineId,
        revision_hash: &str,
    ) -> Result<ContainerCreateBody> {
        let container = &spec.template;

        let mut labels = HashMap::new();
        labels.insert(LABEL_MANAGED.to_string(), "true".to_string());
        labels.insert(LABEL_NAMESPACE.to_string(), spec.namespace.0.clone());
        labels.insert(LABEL_SERVICE.to_string(), spec.name.clone());
        labels.insert(LABEL_REVISION.to_string(), revision_hash.to_string());
        labels.insert(LABEL_DEPLOY.to_string(), deploy_id.0.clone());
        labels.insert(LABEL_INSTANCE.to_string(), instance_id.0.clone());
        labels.insert(LABEL_SLOT.to_string(), slot_id.0.clone());
        labels.insert(LABEL_MACHINE.to_string(), machine_id.0.clone());
        for (key, value) in &spec.labels {
            labels.insert(key.clone(), value.clone());
        }

        let host_config = HostConfig {
            network_mode: match &spec.network {
                NetworkMode::Host => Some("host".to_string()),
                NetworkMode::None => Some("none".to_string()),
                NetworkMode::Service(service) => {
                    Some(format!("container:ployz-{}-{service}", spec.namespace))
                }
                NetworkMode::Overlay => self.overlay_network.clone(),
            },
            binds: Some(build_binds(container)),
            port_bindings: build_port_bindings(spec)?,
            cap_add: if container.cap_add.is_empty() {
                None
            } else {
                Some(container.cap_add.clone())
            },
            cap_drop: if container.cap_drop.is_empty() {
                None
            } else {
                Some(container.cap_drop.clone())
            },
            privileged: Some(container.privileged),
            restart_policy: Some(build_restart_policy(&spec.restart)),
            memory: container.resources.memory_bytes.map(|value| value as i64),
            nano_cpus: container.resources.cpu_nano(),
            sysctls: if container.sysctls.is_empty() {
                None
            } else {
                Some(container.sysctls.clone().into_iter().collect())
            },
            tmpfs: {
                let mounts: HashMap<String, String> = container
                    .volumes
                    .iter()
                    .filter(|mount| matches!(mount.source, VolumeSource::Tmpfs))
                    .map(|mount| (mount.target.clone(), String::new()))
                    .collect();
                if mounts.is_empty() {
                    None
                } else {
                    Some(mounts)
                }
            },
            ..Default::default()
        };

        let env: Vec<String> = container
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();

        Ok(ContainerCreateBody {
            image: Some(container.image.clone()),
            cmd: container.command.clone(),
            entrypoint: container.entrypoint.clone(),
            env: if env.is_empty() { None } else { Some(env) },
            labels: Some(labels),
            user: container.user.clone(),
            host_config: Some(host_config),
            stop_timeout: spec
                .stop_grace_period
                .as_ref()
                .and_then(|value| parse_duration_secs(value)),
            ..Default::default()
        })
    }

    async fn image_exists(&self, image: &str) -> bool {
        self.docker.inspect_image(image).await.is_ok()
    }

    async fn pull_image(&self, image: &str) -> Result<()> {
        use bollard::query_parameters::CreateImageOptionsBuilder;

        let (from_image, tag) = match image.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            [img, tag] => (*img, *tag),
            _ => (image, "latest"),
        };

        let options = CreateImageOptionsBuilder::default()
            .from_image(from_image)
            .tag(tag)
            .build();
        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(|e| Error::operation("pull_image", e.to_string()))?;
        }
        Ok(())
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

#[allow(clippy::too_many_arguments)]
pub(super) fn build_instance_status_record(
    namespace: &Namespace,
    service: &str,
    slot_id: &SlotId,
    machine_id: &MachineId,
    revision_hash: &str,
    deploy_id: &DeployId,
    instance: &ManagedInstance,
    phase: InstancePhase,
    ready: bool,
    drain_state: DrainState,
    error: Option<String>,
) -> InstanceStatusRecord {
    InstanceStatusRecord {
        instance_id: instance.instance_id.clone(),
        namespace: namespace.clone(),
        service: service.to_string(),
        slot_id: slot_id.clone(),
        machine_id: machine_id.clone(),
        revision_hash: revision_hash.to_string(),
        deploy_id: deploy_id.clone(),
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

async fn probe_tcp(ip_address: IpAddr, port: u16) -> bool {
    let address = SocketAddr::new(ip_address, port);
    TcpStream::connect(address).await.is_ok()
}

async fn probe_http(ip_address: IpAddr, port: u16, path: &str) -> bool {
    let url = format!("http://{ip_address}:{port}{path}");
    let client = reqwest::Client::new();
    match client.get(url).send().await {
        Ok(response) => response.status() == StatusCode::OK,
        Err(_) => false,
    }
}

pub(super) fn stable_hash_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }

    format!("{hash:016x}")
}

pub(super) fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}
