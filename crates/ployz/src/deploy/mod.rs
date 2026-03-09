pub mod remote;

use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
};
use futures_util::stream::StreamExt;
use reqwest::StatusCode;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::machine_liveness::machine_is_fresh;
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployPreview, DeployRecord,
    DeployState, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
    ServiceHeadRecord, ServicePlan, ServiceRevisionRecord, ServiceSlotRecord, SlotId, SlotPlan,
};
use crate::spec::{
    ContainerSpec, DeployManifest, Namespace, NetworkMode, Placement, PortProtocol, PullPolicy,
    ReadinessProbe, ResourcesExt, ServicePort, ServiceSpec, VolumeSource,
};
use crate::store::{DeployStore, MachineStore};

const LABEL_MANAGED: &str = "dev.ployz.managed";
const LABEL_NAMESPACE: &str = "dev.ployz.namespace";
const LABEL_SERVICE: &str = "dev.ployz.service";
const LABEL_REVISION: &str = "dev.ployz.revision";
const LABEL_DEPLOY: &str = "dev.ployz.deploy";
const LABEL_INSTANCE: &str = "dev.ployz.instance";
const LABEL_SLOT: &str = "dev.ployz.slot";
const LABEL_MACHINE: &str = "dev.ployz.machine";

#[derive(Clone, Default)]
pub struct NamespaceLockManager {
    held: Arc<Mutex<HashMap<String, DeployId>>>,
}

impl NamespaceLockManager {
    pub fn try_acquire(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<NamespaceLock> {
        let mut guard = self.lock_inner();
        if let Some(current) = guard.get(&namespace.0) {
            return Err(Error::operation(
                "namespace_lock",
                format!(
                    "namespace '{}' is already locked by deploy '{}'",
                    namespace, current
                ),
            ));
        }
        guard.insert(namespace.0.clone(), deploy_id.clone());
        Ok(NamespaceLock {
            held: Arc::clone(&self.held),
            namespace: namespace.clone(),
        })
    }

    fn lock_inner(&self) -> MutexGuard<'_, HashMap<String, DeployId>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct NamespaceLock {
    held: Arc<Mutex<HashMap<String, DeployId>>>,
    namespace: Namespace,
}

impl Drop for NamespaceLock {
    fn drop(&mut self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.remove(&self.namespace.0);
    }
}

#[derive(Debug, Clone)]
struct DesiredSlot {
    slot_id: SlotId,
    machine_id: MachineId,
}

#[derive(Debug, Clone)]
struct ManagedInstance {
    instance_id: InstanceId,
    service: String,
    slot_id: SlotId,
    machine_id: MachineId,
    revision_hash: String,
    deploy_id: DeployId,
    docker_container_id: String,
    ip_address: Option<IpAddr>,
    backend_ports: BTreeMap<String, u16>,
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

    async fn start_candidate(
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

    async fn wait_ready(&self, spec: &ServiceSpec, instance: &ManagedInstance) -> Result<()> {
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
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        self.docker
            .remove_container(&container_name, Some(options))
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

pub async fn preview(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    manifest: &DeployManifest,
) -> Result<DeployPreview> {
    manifest
        .validate(namespace)
        .map_err(|e| Error::operation("deploy_preview", e))?;

    let current_heads = store.list_service_heads(namespace).await?;
    let current_slots = store.list_service_slots(namespace).await?;
    let machines = store.list_machines().await?;
    let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
    let current_head_map: HashMap<String, ServiceHeadRecord> = current_heads
        .into_iter()
        .map(|record| (record.service.clone(), record))
        .collect();
    let mut current_slots_by_service: HashMap<String, Vec<ServiceSlotRecord>> = HashMap::new();
    for slot in current_slots {
        current_slots_by_service
            .entry(slot.service.clone())
            .or_default()
            .push(slot);
    }

    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(manifest)
            .map_err(|e| Error::operation("deploy_preview", format!("serialize manifest: {e}")))?
            .as_slice(),
    );

    let mut participants = BTreeSet::new();
    for machine_id in &desired_machines {
        participants.insert(machine_id.clone());
    }
    let mut services = Vec::new();
    for spec in &manifest.services {
        let revision_hash = spec
            .revision_hash()
            .map_err(|e| Error::operation("deploy_preview", e))?;
        let desired_slots = desired_slots(
            spec,
            &desired_machines,
            current_slots_by_service.get(&spec.name),
        )?;
        let current_service_slots = current_slots_by_service
            .get(&spec.name)
            .cloned()
            .unwrap_or_default();
        let current_head = current_head_map.get(&spec.name);
        let mut slot_plans = Vec::new();
        for desired_slot in desired_slots {
            participants.insert(desired_slot.machine_id.clone());
            let current_slot = current_service_slots
                .iter()
                .find(|slot| slot.slot_id == desired_slot.slot_id);
            let action = match current_slot {
                Some(slot)
                    if slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash =>
                {
                    DeployChangeKind::Unchanged
                }
                Some(_) => DeployChangeKind::Replace,
                None => DeployChangeKind::Create,
            };
            slot_plans.push(SlotPlan {
                slot_id: desired_slot.slot_id,
                machine_id: desired_slot.machine_id,
                current_instance_id: current_slot.map(|slot| slot.active_instance_id.clone()),
                next_instance_id: None,
                current_revision_hash: current_slot.map(|slot| slot.revision_hash.clone()),
                next_revision_hash: Some(revision_hash.clone()),
                action,
            });
        }
        for slot in &current_service_slots {
            participants.insert(slot.machine_id.clone());
        }
        let action = if slot_plans
            .iter()
            .all(|plan| plan.action == DeployChangeKind::Unchanged)
            && current_head.map(|head| head.current_revision_hash.as_str())
                == Some(revision_hash.as_str())
        {
            DeployChangeKind::Unchanged
        } else if current_head.is_none() {
            DeployChangeKind::Create
        } else {
            DeployChangeKind::Replace
        };
        services.push(ServicePlan {
            service: spec.name.clone(),
            current_revision_hash: current_head.map(|head| head.current_revision_hash.clone()),
            next_revision_hash: Some(revision_hash),
            slots: slot_plans,
            action,
        });
    }

    for (service, slots) in current_slots_by_service {
        if manifest.services.iter().any(|spec| spec.name == service) {
            continue;
        }
        for slot in &slots {
            participants.insert(slot.machine_id.clone());
        }
        services.push(ServicePlan {
            service,
            current_revision_hash: current_head_map
                .get(&slots[0].service)
                .map(|head| head.current_revision_hash.clone()),
            next_revision_hash: None,
            slots: slots
                .into_iter()
                .map(|slot| SlotPlan {
                    slot_id: slot.slot_id,
                    machine_id: slot.machine_id,
                    current_instance_id: Some(slot.active_instance_id),
                    next_instance_id: None,
                    current_revision_hash: Some(slot.revision_hash),
                    next_revision_hash: None,
                    action: DeployChangeKind::Remove,
                })
                .collect(),
            action: DeployChangeKind::Remove,
        });
    }

    Ok(DeployPreview {
        namespace: namespace.clone(),
        manifest_hash,
        participants: participants.into_iter().collect(),
        services,
        warnings: Vec::new(),
    })
}

pub async fn apply(
    store: &StoreDriver,
    runtime: &LocalDeployRuntime,
    locks: &NamespaceLockManager,
    local_machine_id: &MachineId,
    remote_control_port: u16,
    namespace: &Namespace,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    let deploy_id = DeployId(Uuid::new_v4().to_string());
    let _lock = locks.try_acquire(namespace, &deploy_id)?;
    let started_at = now_unix_secs();
    let initial_preview = preview(store, local_machine_id, namespace, manifest).await?;
    let machines = store.list_machines().await?;
    let machine_map: HashMap<MachineId, crate::model::MachineRecord> = machines
        .iter()
        .map(|machine| (machine.id.clone(), machine.clone()))
        .collect();
    let mut remote_sessions = Vec::new();
    let participant_fingerprint = initial_preview.participants.clone();
    let mut events = vec![DeployEvent {
        step: "lock".into(),
        message: format!("acquired namespace lock for '{}'", namespace),
    }];

    for participant in &initial_preview.participants {
        if participant == local_machine_id {
            continue;
        }
        let Some(machine) = machine_map.get(participant) else {
            return Err(Error::operation(
                "deploy_apply",
                format!(
                    "participant '{}' is missing from machine inventory",
                    participant
                ),
            ));
        };
        let session = remote::RemoteDeploySession::connect(
            machine,
            remote_control_port,
            namespace,
            &deploy_id,
            local_machine_id,
            &participant_fingerprint,
        )
        .await?;
        events.push(DeployEvent {
            step: "lock_remote".into(),
            message: format!("acquired remote lock on '{}'", participant),
        });
        remote_sessions.push(session);
    }

    let result = async {
        let remote_session_index: HashMap<String, usize> = remote_sessions
            .iter()
            .enumerate()
            .map(|(index, session)| (session.machine_id().0.clone(), index))
            .collect();

        adopt_instances(store, runtime, namespace).await?;
        for session in &remote_sessions {
            let instances = session.inspect_namespace().await?;
            events.push(DeployEvent {
                step: "inspect".into(),
                message: format!(
                    "inspected '{}' and found {} local instance records",
                    session.machine_id(),
                    instances.len()
                ),
            });
        }

        let final_preview = preview(store, local_machine_id, namespace, manifest).await?;
        if final_preview.participants != initial_preview.participants {
            return Err(Error::operation(
                "deploy_apply",
                "participant set changed after lock acquisition; retry deploy",
            ));
        }

        let mut deploy_record = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: namespace.clone(),
            coordinator_machine_id: local_machine_id.clone(),
            manifest_hash: final_preview.manifest_hash.clone(),
            state: DeployState::Applying,
            started_at,
            committed_at: None,
            finished_at: None,
            summary_json: serde_json::to_string(&final_preview)
                .map_err(|e| Error::operation("deploy_apply", format!("serialize preview: {e}")))?,
        };
        store.upsert_deploy(&deploy_record).await?;

        let current_slots_by_service =
            current_slots_by_service(store.list_service_slots(namespace).await?);
        let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
        let mut removed_services = Vec::new();
        let mut committed_heads = Vec::new();
        let mut committed_slots = Vec::new();

        for spec in &manifest.services {
            let revision_hash = spec
                .revision_hash()
                .map_err(|e| Error::operation("deploy_apply", e))?;
            let spec_json = spec
                .canonical_revision_json()
                .map_err(|e| Error::operation("deploy_apply", e))?;
            store
                .upsert_service_revision(&ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: spec.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: spec_json.clone(),
                    created_by: local_machine_id.clone(),
                    created_at: started_at,
                })
                .await?;

            let desired = desired_slots(
                spec,
                &desired_machines,
                current_slots_by_service.get(&spec.name),
            )?;
            let mut next_slots = Vec::new();
            for desired_slot in desired {
                let current_slot = current_slots_by_service.get(&spec.name).and_then(|slots| {
                    slots
                        .iter()
                        .find(|slot| slot.slot_id == desired_slot.slot_id)
                });
                let keep_current = current_slot.is_some_and(|slot| {
                    slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash
                });

                let active_instance_id = if keep_current {
                    let Some(slot) = current_slot else {
                        return Err(Error::operation("deploy_apply", "missing current slot"));
                    };
                    slot.active_instance_id.clone()
                } else {
                    let instance_id = InstanceId(Uuid::new_v4().to_string());
                    events.push(DeployEvent {
                        step: "start_candidate".into(),
                        message: format!(
                            "starting {} slot {} as instance {} on {}",
                            spec.name, desired_slot.slot_id, instance_id, desired_slot.machine_id
                        ),
                    });
                    let status = if desired_slot.machine_id == *local_machine_id {
                        let instance = runtime
                            .start_candidate(
                                spec,
                                &deploy_id,
                                &instance_id,
                                &desired_slot.slot_id,
                                &desired_slot.machine_id,
                                &revision_hash,
                            )
                            .await?;
                        runtime.wait_ready(spec, &instance).await?;
                        build_instance_status_record(
                            namespace,
                            &spec.name,
                            &desired_slot.slot_id,
                            &desired_slot.machine_id,
                            &revision_hash,
                            &deploy_id,
                            &instance,
                            InstancePhase::Ready,
                            true,
                            DrainState::None,
                            None,
                        )
                    } else {
                        let Some(index) = remote_session_index.get(&desired_slot.machine_id.0)
                        else {
                            return Err(Error::operation(
                                "deploy_apply",
                                format!(
                                    "no remote session was available for machine '{}'",
                                    desired_slot.machine_id
                                ),
                            ));
                        };
                        remote_sessions[*index]
                            .start_candidate(
                                namespace,
                                &spec.name,
                                &desired_slot.slot_id,
                                &instance_id,
                                &deploy_id,
                                &spec_json,
                            )
                            .await?
                    };
                    store.upsert_instance_status(&status).await?;
                    status.instance_id
                };

                next_slots.push(ServiceSlotRecord {
                    namespace: namespace.clone(),
                    service: spec.name.clone(),
                    slot_id: desired_slot.slot_id,
                    machine_id: desired_slot.machine_id,
                    active_instance_id,
                    revision_hash: revision_hash.clone(),
                    updated_by_deploy_id: deploy_id.clone(),
                    updated_at: now_unix_secs(),
                });
            }

            committed_heads.push(ServiceHeadRecord {
                namespace: namespace.clone(),
                service: spec.name.clone(),
                current_revision_hash: revision_hash,
                updated_by_deploy_id: deploy_id.clone(),
                updated_at: now_unix_secs(),
            });
            committed_slots.extend(next_slots);
        }

        for service in final_preview
            .services
            .iter()
            .filter(|plan| plan.action == DeployChangeKind::Remove)
            .map(|plan| plan.service.clone())
        {
            removed_services.push(service);
        }

        deploy_record.state = DeployState::Committed;
        deploy_record.committed_at = Some(now_unix_secs());
        deploy_record.finished_at = deploy_record.committed_at;
        deploy_record.summary_json = serde_json::to_string(&final_preview)
            .map_err(|e| Error::operation("deploy_apply", format!("serialize preview: {e}")))?;

        store
            .commit_deploy(
                namespace,
                &removed_services,
                &committed_heads,
                &committed_slots,
                &deploy_record,
            )
            .await?;
        events.push(DeployEvent {
            step: "commit".into(),
            message: format!("committed deploy {} for '{}'", deploy_id, namespace),
        });

        for session in &remote_sessions {
            session.apply_route_epoch().await?;
            events.push(DeployEvent {
                step: "route_epoch".into(),
                message: format!("applied route epoch on '{}'", session.machine_id()),
            });
        }

        let active_instance_ids: BTreeSet<String> = committed_slots
            .iter()
            .map(|slot| slot.active_instance_id.0.clone())
            .collect();
        let participant_ids: BTreeSet<String> = final_preview
            .participants
            .iter()
            .map(|machine_id| machine_id.0.clone())
            .collect();
        let mut cleanup_errors = Vec::new();
        for status in store.list_instance_status(namespace).await? {
            if active_instance_ids.contains(&status.instance_id.0) {
                continue;
            }
            if !participant_ids.contains(&status.machine_id.0) {
                continue;
            }
            if status.machine_id == *local_machine_id {
                let mut draining = status.clone();
                draining.phase = InstancePhase::Draining;
                draining.ready = false;
                draining.drain_state = DrainState::Requested;
                draining.updated_at = now_unix_secs();
                if let Err(err) = store.upsert_instance_status(&draining).await {
                    cleanup_errors.push(err.to_string());
                    continue;
                }
                match runtime
                    .remove_instance(&status.instance_id, namespace, &status.service)
                    .await
                {
                    Ok(()) => {
                        store.delete_instance_status(&status.instance_id).await?;
                        events.push(DeployEvent {
                            step: "cleanup".into(),
                            message: format!("removed old instance {}", status.instance_id),
                        });
                    }
                    Err(err) => cleanup_errors.push(err.to_string()),
                }
            } else if let Some(index) = remote_session_index.get(&status.machine_id.0) {
                if let Err(err) = remote_sessions[*index]
                    .drain_instance(&status.instance_id)
                    .await
                {
                    cleanup_errors.push(err.to_string());
                    continue;
                }
                match remote_sessions[*index]
                    .remove_instance(&status.instance_id)
                    .await
                {
                    Ok(()) => {
                        events.push(DeployEvent {
                            step: "cleanup".into(),
                            message: format!(
                                "removed remote instance {} from {}",
                                status.instance_id, status.machine_id
                            ),
                        });
                    }
                    Err(err) => cleanup_errors.push(err.to_string()),
                }
            }
        }

        let final_state = if cleanup_errors.is_empty() {
            DeployState::Committed
        } else {
            deploy_record.state = DeployState::CleanupPending;
            deploy_record.finished_at = Some(now_unix_secs());
            store.upsert_deploy(&deploy_record).await?;
            for error in cleanup_errors {
                events.push(DeployEvent {
                    step: "cleanup_pending".into(),
                    message: error,
                });
            }
            DeployState::CleanupPending
        };

        Ok(DeployApplyResult {
            deploy_id: deploy_id.clone(),
            preview: final_preview,
            state: final_state,
            events,
        })
    }
    .await;

    for session in remote_sessions {
        session.shutdown().await;
    }

    result
}

async fn adopt_instances(
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

fn deployable_machines(
    machines: &[crate::model::MachineRecord],
    local_machine_id: &MachineId,
    now: u64,
) -> Vec<MachineId> {
    let mut enabled: Vec<MachineId> = machines
        .iter()
        .filter(|machine| machine.participation == crate::model::Participation::Enabled)
        .filter(|machine| machine_is_fresh(machine, now))
        .map(|machine| machine.id.clone())
        .collect();
    enabled.sort_by(|left, right| left.0.cmp(&right.0));
    if enabled.is_empty() {
        return vec![local_machine_id.clone()];
    }
    enabled
}

fn desired_slots(
    spec: &ServiceSpec,
    machines: &[MachineId],
    current_slots: Option<&Vec<ServiceSlotRecord>>,
) -> Result<Vec<DesiredSlot>> {
    let candidates = if machines.is_empty() {
        vec![MachineId("local".into())]
    } else {
        machines.to_vec()
    };

    let mut desired = Vec::new();
    match spec.placement {
        Placement::Singleton => {
            let machine_id = current_slots
                .and_then(|slots| slots.first().map(|slot| slot.machine_id.clone()))
                .unwrap_or_else(|| candidates[0].clone());
            desired.push(DesiredSlot {
                slot_id: SlotId("slot-0001".into()),
                machine_id,
            });
        }
        Placement::Replicated { count } => {
            if count == 0 {
                return Err(Error::operation(
                    "desired_slots",
                    format!("service '{}' requested zero replicas", spec.name),
                ));
            }
            for index in 0..count {
                let machine_id = candidates[usize::from(index) % candidates.len()].clone();
                desired.push(DesiredSlot {
                    slot_id: SlotId(format!("slot-{number:04}", number = usize::from(index) + 1)),
                    machine_id,
                });
            }
        }
        Placement::Global => {
            for machine_id in &candidates {
                desired.push(DesiredSlot {
                    slot_id: SlotId(format!("slot-{}", machine_id.0)),
                    machine_id: machine_id.clone(),
                });
            }
        }
    }
    Ok(desired)
}

fn current_slots_by_service(
    current_slots: Vec<ServiceSlotRecord>,
) -> HashMap<String, Vec<ServiceSlotRecord>> {
    let mut grouped = HashMap::new();
    for slot in current_slots {
        grouped
            .entry(slot.service.clone())
            .or_insert_with(Vec::new)
            .push(slot);
    }
    grouped
}

fn build_instance_status_record(
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

async fn list_local_instance_status(
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

fn stable_hash_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey};
    use std::net::Ipv6Addr;

    #[test]
    fn deployable_machines_excludes_stale_and_down_peers() {
        let now = 100;
        let machines = vec![
            test_machine(
                "fresh-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                90,
            ),
            test_machine(
                "stale-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                69,
            ),
            test_machine(
                "down-enabled",
                Participation::Enabled,
                MachineStatus::Down,
                100,
            ),
            test_machine(
                "draining-fresh",
                Participation::Draining,
                MachineStatus::Up,
                100,
            ),
        ];

        let deployable = deployable_machines(&machines, &MachineId("local".into()), now);

        assert_eq!(deployable, vec![MachineId("fresh-enabled".into())]);
    }

    #[test]
    fn deployable_machines_falls_back_to_local_when_none_are_fresh_enabled() {
        let machines = vec![
            test_machine(
                "stale-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                10,
            ),
            test_machine(
                "down-enabled",
                Participation::Enabled,
                MachineStatus::Down,
                100,
            ),
        ];

        let deployable = deployable_machines(&machines, &MachineId("local".into()), 100);

        assert_eq!(deployable, vec![MachineId("local".into())]);
    }

    fn test_machine(
        id: &str,
        participation: Participation,
        status: MachineStatus,
        last_heartbeat: u64,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
        }
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}
