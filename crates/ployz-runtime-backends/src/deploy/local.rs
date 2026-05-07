use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
};
use crate::runtime::labels::{self, WorkloadMeta, build_workload_labels, extract_workload_labels};
use crate::runtime::{
    ContainerEngine, PortBinding, PortMap, Probe, PullPolicy, RestartPolicy, RestartPolicyName,
    RuntimeContainerSpec,
};
use crate::spec::{
    ContainerSpec, MountSource, Namespace, NetworkMode, PortProtocol, ServicePort, ServiceSpec,
    VolumeDeclaration,
};
use crate::storage::{ShellRunner, TokioShellRunner, ZfsDriver, resolve_volumes};
use ployz_store_api::InstanceStatusStore;

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

impl ManagedInstance {
    pub(super) fn to_status_record(
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

pub(super) struct StartCandidate<'a> {
    pub(super) namespace: &'a Namespace,
    pub(super) spec: &'a ServiceSpec,
    pub(super) deploy_id: &'a DeployId,
    pub(super) instance_id: &'a InstanceId,
    pub(super) slot_id: &'a SlotId,
    pub(super) machine_id: &'a MachineId,
    pub(super) revision_hash: &'a str,
    pub(super) volumes: &'a HashMap<String, VolumeDeclaration>,
}

pub struct LocalDeployRuntime {
    engine: ContainerEngine,
    overlay_network: Option<String>,
    overlay_dns_server: Option<Ipv4Addr>,
    storage_driver: Option<Arc<ZfsDriver<TokioShellRunner>>>,
}

impl LocalDeployRuntime {
    pub fn new(
        overlay_network: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
        storage_driver: Option<Arc<ZfsDriver<TokioShellRunner>>>,
    ) -> Result<Self> {
        let docker = bollard::Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;
        let engine = ContainerEngine::new(docker);
        Ok(Self {
            engine,
            overlay_network,
            overlay_dns_server,
            storage_driver,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_engine(
        engine: ContainerEngine,
        overlay_network: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
        storage_driver: Option<Arc<ZfsDriver<TokioShellRunner>>>,
    ) -> Self {
        Self {
            engine,
            overlay_network,
            overlay_dns_server,
            storage_driver,
        }
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
            let Some(labels) = obs.labels.as_observed() else {
                continue;
            };
            let Some(wl) = extract_workload_labels(labels) else {
                continue;
            };
            let Some(container_id) = obs.container_id.as_observed() else {
                continue;
            };
            let Some(ip_address) = obs.ip_address.as_observed() else {
                continue;
            };
            instances.push(ManagedInstance {
                instance_id: InstanceId(wl.instance_id),
                service: wl.service,
                slot_id: SlotId(wl.slot_id),
                machine_id: MachineId(wl.machine_id),
                revision_hash: wl.revision_hash,
                deploy_id: DeployId(wl.deploy_id),
                docker_container_id: container_id.clone(),
                ip_address: *ip_address,
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
            volumes,
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

        let resolved_volumes = self.resolve_mounts(namespace, container, volumes).await?;

        let runtime_spec = RuntimeContainerSpec {
            key,
            container_name: container_name.clone(),
            image: container.image.clone(),
            pull_policy,
            cmd: container.command.clone(),
            entrypoint: container.entrypoint.clone(),
            env,
            labels: workload_labels,
            binds: build_binds(container, &resolved_volumes)?,
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
            stop_timeout: container
                .stop_grace_period
                .as_ref()
                .and_then(|v| parse_duration_secs(v)),
            pid_mode: container.pid_mode.clone(),
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

    async fn resolve_mounts(
        &self,
        namespace: &Namespace,
        container: &ContainerSpec,
        volumes: &HashMap<String, VolumeDeclaration>,
    ) -> Result<HashMap<String, PathBuf>> {
        let has_volumes = container
            .mounts
            .iter()
            .any(|mount| matches!(mount.source, MountSource::Volume(_)));
        if !has_volumes {
            return Ok(HashMap::new());
        }
        let Some(driver) = &self.storage_driver else {
            return resolve_mounts_with_driver::<TokioShellRunner>(
                None, namespace, container, volumes,
            )
            .await;
        };
        resolve_mounts_with_driver(Some(driver.as_ref()), namespace, container, volumes).await
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

async fn resolve_mounts_with_driver<R: ShellRunner>(
    driver: Option<&ZfsDriver<R>>,
    namespace: &Namespace,
    container: &ContainerSpec,
    volumes: &HashMap<String, VolumeDeclaration>,
) -> Result<HashMap<String, PathBuf>> {
    let has_volumes = container
        .mounts
        .iter()
        .any(|mount| matches!(mount.source, MountSource::Volume(_)));
    if !has_volumes {
        return Ok(HashMap::new());
    }
    let Some(driver) = driver else {
        return Err(Error::operation(
            "start_candidate",
            "service uses managed volumes but daemon has no [storage] zfs_root configured",
        ));
    };
    resolve_volumes(driver, namespace, container, volumes).await
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
        let record = instance.to_status_record(
            namespace,
            InstancePhase::Ready,
            true,
            DrainState::None,
            None,
        );
        store.record_instance_status(&record).await?;
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
    instance.to_status_record(namespace, phase, ready, drain_state, error)
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

fn build_binds(
    container: &ContainerSpec,
    resolved: &HashMap<String, PathBuf>,
) -> Result<Vec<String>> {
    let mut binds = Vec::new();
    for mount in &container.mounts {
        match &mount.source {
            MountSource::Bind(source) => {
                let ro = if mount.readonly { ":ro" } else { "" };
                binds.push(format!("{source}:{}{ro}", mount.target));
            }
            MountSource::Volume(volume) => {
                let Some(host) = resolved.get(volume) else {
                    return Err(Error::operation(
                        "build_binds",
                        format!("volume '{volume}' was not resolved before container start"),
                    ));
                };
                let ro = if mount.readonly { ":ro" } else { "" };
                binds.push(format!("{}:{}{ro}", host.display(), mount.target));
            }
            MountSource::Tmpfs => {}
        }
    }
    Ok(binds)
}

fn build_tmpfs(container: &ContainerSpec) -> HashMap<String, String> {
    container
        .mounts
        .iter()
        .filter(|mount| matches!(mount.source, MountSource::Tmpfs))
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

fn build_restart_policy(policy: &crate::spec::RestartPolicy) -> RestartPolicy {
    let name = match policy {
        crate::spec::RestartPolicy::No => RestartPolicyName::No,
        crate::spec::RestartPolicy::Always => RestartPolicyName::Always,
        crate::spec::RestartPolicy::OnFailure => RestartPolicyName::OnFailure,
        crate::spec::RestartPolicy::UnlessStopped => RestartPolicyName::UnlessStopped,
    };
    RestartPolicy {
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::{
        LocalDeployRuntime, StartCandidate, build_binds, resolve_mounts_with_driver,
        workload_dns_servers,
    };
    use crate::error::{Error, Result};
    use crate::model::{DeployId, InstanceId, MachineId, SlotId};
    use crate::runtime::ContainerEngine;
    use crate::spec::{
        ContainerSpec, Mount, MountSource, Namespace, NetworkMode, Placement, PullPolicy,
        Resources, RestartPolicy, RolloutStrategy, ServiceSpec, VolumeDeclaration, VolumeScope,
    };
    use crate::storage::{ShellOutput, ShellRunner, ZfsDriver};
    use bollard::Docker;
    use std::net::Ipv4Addr;

    #[derive(Clone, Default)]
    struct FakeShellRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        outputs: Arc<Mutex<VecDeque<ShellOutput>>>,
    }

    impl FakeShellRunner {
        fn push(&self, status: i32, stdout: &str, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(ShellOutput {
                    status,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.as_bytes().to_vec(),
                });
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls").clone()
        }
    }

    #[async_trait]
    impl ShellRunner for FakeShellRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            self.calls.lock().expect("calls").push(call);
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .ok_or_else(|| Error::operation("fake_shell", "missing output"))
        }
    }

    fn volume_container() -> ContainerSpec {
        ContainerSpec {
            image: "postgres:17".into(),
            command: None,
            entrypoint: None,
            env: BTreeMap::new(),
            mounts: vec![Mount {
                source: MountSource::Volume("data".into()),
                target: "/var/lib/postgresql/data".into(),
                readonly: false,
            }],
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            stop_grace_period: None,
            pid_mode: None,
            pull_policy: PullPolicy::IfNotPresent,
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        }
    }

    fn volume_declarations() -> HashMap<String, VolumeDeclaration> {
        HashMap::from([(
            "data".into(),
            VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "1G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            },
        )])
    }

    #[tokio::test]
    async fn managed_volume_resolves_zfs_dataset_and_builds_bind_mount() {
        let fake = FakeShellRunner::default();
        fake.push(0, "/tank/ployz-test\n", "");
        let driver = ZfsDriver::new(fake.clone(), "tank/ployz-test", 1.0)
            .await
            .expect("driver");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, &format!("{}\n", 1024_u64.pow(4)), "");
        fake.push(0, "tank/ployz-test\t0\n", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        let namespace = Namespace("test".into());
        let container = volume_container();
        let volumes = volume_declarations();
        let resolved = resolve_mounts_with_driver(Some(&driver), &namespace, &container, &volumes)
            .await
            .expect("resolve mounts");

        assert_eq!(
            resolved.get("data").map(|path| path.as_path()),
            Some(std::path::Path::new("/tank/ployz-test/test/data"))
        );
        assert_eq!(
            build_binds(&container, &resolved).expect("build binds"),
            vec!["/tank/ployz-test/test/data:/var/lib/postgresql/data"]
        );

        let calls = fake.calls();
        assert!(calls.iter().any(|call| {
            call == &[
                "zfs",
                "create",
                "-p",
                "-o",
                "quota=1G",
                "-o",
                "mountpoint=/tank/ployz-test/test/data",
                "-o",
                "compression=lz4",
                "tank/ployz-test/test/data",
            ]
        }));
        assert!(
            calls
                .iter()
                .any(|call| { call == &["chmod", "0750", "/tank/ployz-test/test/data"] })
        );
        assert!(
            calls
                .iter()
                .any(|call| { call == &["chown", "999:999", "/tank/ployz-test/test/data"] })
        );

        fake.push(
            0,
            "tank/ployz-test/test/data\t1G\t/tank/ployz-test/test/data\n",
            "",
        );
        fake.push(0, "750:999:999\n", "");
        let _ = resolve_mounts_with_driver(Some(&driver), &namespace, &container, &volumes)
            .await
            .expect("adopt existing dataset");
        let create_count = fake
            .calls()
            .iter()
            .filter(|call| call.get(1).is_some_and(|arg| arg == "create"))
            .count();
        assert_eq!(create_count, 1);
    }

    #[tokio::test]
    async fn managed_volume_without_storage_driver_fails_before_runtime_start() {
        let error = resolve_mounts_with_driver::<FakeShellRunner>(
            None,
            &Namespace("test".into()),
            &volume_container(),
            &volume_declarations(),
        )
        .await
        .expect_err("missing driver should fail");

        assert!(
            error
                .to_string()
                .contains("no [storage] zfs_root configured")
        );
    }

    fn volume_service_spec() -> ServiceSpec {
        ServiceSpec {
            name: "db".into(),
            placement: Placement::Replicated { count: 1 },
            template: volume_container(),
            network: NetworkMode::None,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            restart: RestartPolicy::No,
        }
    }

    #[tokio::test]
    async fn start_candidate_without_storage_driver_fails_for_volume_service() {
        // Construct a runtime via the test seam so the no-zfs_root path is
        // exercised through start_candidate, not just the inner resolver.
        // resolve_mounts short-circuits before any Docker call, so the engine
        // here is never reached at runtime. We build the Docker handle via
        // `connect_with_http` instead of `connect_with_socket_defaults` so the
        // test still runs in sandboxes/CI without `/var/run/docker.sock`;
        // `connect_with_http` only builds an HTTP client and never touches
        // the network until a request is issued. Don't switch back unless the
        // test starts actually exercising Docker.
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("placeholder docker handle");
        let engine = ContainerEngine::new(docker);
        let runtime = LocalDeployRuntime::from_engine(engine, None, None, None);

        let namespace = Namespace("test".into());
        let spec = volume_service_spec();
        let deploy_id = DeployId("dep-1".into());
        let instance_id = InstanceId("inst-1".into());
        let slot_id = SlotId("slot-1".into());
        let machine_id = MachineId("mach-1".into());
        let volumes = volume_declarations();

        let error = runtime
            .start_candidate(StartCandidate {
                namespace: &namespace,
                spec: &spec,
                deploy_id: &deploy_id,
                instance_id: &instance_id,
                slot_id: &slot_id,
                machine_id: &machine_id,
                revision_hash: "rev",
                volumes: &volumes,
            })
            .await
            .expect_err("missing storage driver should surface through start_candidate");

        let message = error.to_string();
        assert!(
            message.contains("start_candidate"),
            "expected start_candidate operation tag, got: {message}"
        );
        assert!(
            message.contains("no [storage] zfs_root configured"),
            "expected zfs_root guidance in error, got: {message}"
        );
    }

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
