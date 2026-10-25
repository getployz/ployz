pub(crate) mod bridge_address;
mod checks;
mod create;
mod http_health;
mod images;
mod lifecycle;
mod limits;
mod managed_service;
mod observe;
pub(crate) use observe::published_spec;
mod peer_pull;
pub(crate) use peer_pull::ImageProxy;
mod spec_store;
mod stream;
mod telemetry;
mod unregistry;
mod volume;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
pub(crate) mod test_support;

use std::{
    collections::{BTreeMap, HashMap},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bollard::{
    Docker,
    models::{ContainerInspectResponse, HealthConfig, ImageManifestSummaryKindEnum},
    query_parameters::{ListContainersOptionsBuilder, ListImagesOptionsBuilder},
};
use futures_util::StreamExt;
use ployz_core::{
    BridgeEndpointCapacity, ConfiguredHealthcheck, ContainerAddress, ContainerId, ContainerKind,
    ContainerObservation, ContainerRuntimeObservation, DiskSpace, DockerVolumeId, DockerVolumeName,
    Gen, HEALTHCHECK_DISABLE_SENTINEL, HealthObservation, HealthcheckCommand, HealthcheckSpec,
    ImageSummary, LastHealthCheck, MachineId, MachineImages, MachineTelemetry, Namespace,
    QualifiedService, RpcError, RpcErrorCode, ServiceId, ServiceName, ValueError,
    healthcheck_deadline,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::{Mutex, watch};

use crate::sealed_compat::LogStore;
use checks::CheckRecords;
use http_health::probe as http_health_probe;
use observe::ObservationSink;

pub(crate) use create::container_log_config;
pub(crate) use lifecycle::{ContainerCreated, ContainerRequest, require_eligible, same_creation};
pub use limits::UnappliedLimits;
pub(crate) use managed_service::{DesiredContainer, Ensure, ManagedService};
pub(crate) use peer_pull::pull_from_ingest;
use ployz_core::LimitsRefusal;
pub use spec_store::{Error as SpecStoreError, FloorTurn, MachineSpecStore};
pub use unregistry::ImageIngest;

#[cfg(test)]
use create::{docker_healthcheck, docker_mounts, docker_ports, docker_resources};

pub const LABEL_MANAGED: &str = "ployz.managed";
pub const LABEL_SERVICE_ID: &str = "ployz.service.id";
pub const LABEL_SERVICE_NAME: &str = "ployz.service.name";
pub const LABEL_NAMESPACE: &str = "ployz.namespace";
pub const LABEL_HOOK: &str = "ployz.service.hook";

/// Only the Ingress Proxy takes an IPv6 address on the ployz bridge, so IPv6
/// visitors reach it by DNAT with their own address (#2837). Every other container
/// stays IPv4-only, so its published ports keep reaching an app that listens on
/// IPv4 alone through docker-proxy, as before.
#[must_use]
pub(crate) fn takes_bridge_ipv6(labels: &HashMap<String, String>) -> bool {
    let ingress = QualifiedService::system_ingress();
    labels.get(LABEL_NAMESPACE) == Some(&ingress.namespace.to_string())
        && labels.get(LABEL_SERVICE_NAME) == Some(&ingress.name.to_string())
        && !labels.contains_key(LABEL_HOOK)
}

/// The ployz bridge endpoint for a container that does or does not take IPv6.
#[must_use]
pub(crate) fn bridge_endpoint(ipv6: bool) -> Option<bollard::models::EndpointSettings> {
    (!ipv6).then(|| bollard::models::EndpointSettings {
        driver_opts: Some(HashMap::from([(
            "com.docker.network.endpoint.sysctls".into(),
            "net.ipv6.conf.IFNAME.disable_ipv6=1".into(),
        )])),
        ..Default::default()
    })
}
/// Deployment attempt that created a Service Container.
pub const LABEL_DEPLOYMENT: &str = "ployz.deployment.id";
pub const LABEL_GEN: &str = "ployz.gen";
pub const LABEL_HOOK_PRE_DEPLOY: &str = "pre-deploy";

#[derive(Clone)]
pub struct LocalDocker {
    client: Docker,
    /// Serializes bridge endpoint creates, and holds the last bridge address handed out.
    endpoint_creates: Arc<Mutex<Option<std::net::Ipv4Addr>>>,
    /// Counts container creates, so a stranded ingress retry can tell a newer
    /// container may have appeared since its scan, or be appearing still.
    creates: Arc<Creates>,
}

#[derive(Default)]
struct Creates {
    started: std::sync::atomic::AtomicU64,
    finished: std::sync::atomic::AtomicU64,
}

/// Counts a create as finished when dropped, even if the create was cancelled.
struct Creating<'a>(&'a Creates);

impl<'a> Creating<'a> {
    fn new(creates: &'a Creates) -> Self {
        creates
            .started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(creates)
    }
}

impl Drop for Creating<'_> {
    fn drop(&mut self) {
        self.0
            .finished
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl LocalDocker {
    pub fn connect() -> Result<Self, Error> {
        Ok(Self::from_client(Docker::connect_with_defaults()?))
    }

    fn from_client(client: Docker) -> Self {
        Self {
            client,
            endpoint_creates: Arc::new(Mutex::new(None)),
            creates: Arc::default(),
        }
    }

    #[cfg(test)]
    fn connect_socket(socket: &str) -> Result<Self, Error> {
        let socket = socket.trim_start_matches("unix://");
        Ok(Self::from_client(Docker::connect_with_socket(
            socket,
            120,
            bollard::API_DEFAULT_VERSION,
        )?))
    }

    async fn uses_containerd_store(&self) -> Result<bool, Error> {
        Ok(self
            .client
            .info()
            .await?
            .driver_status
            .as_deref()
            .is_some_and(containerd_store))
    }

    /// Creates finished so far.
    fn creates(&self) -> u64 {
        self.creates
            .finished
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every create started so far had finished when `creates` was read.
    fn created_nothing_since(&self, creates: u64) -> bool {
        self.creates
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
            == creates
    }

    pub(crate) async fn managed_container_ids(&self) -> Result<Vec<ContainerId>, Error> {
        Ok(self
            .managed_containers()
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }

    async fn managed_containers(
        &self,
    ) -> Result<Vec<(ContainerId, HashMap<String, String>)>, Error> {
        self.labelled(vec![
            LABEL_MANAGED.to_owned(),
            LABEL_NAMESPACE.to_owned(),
            LABEL_SERVICE_ID.to_owned(),
        ])
        .await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<(ContainerId, HashMap<String, String>)>, Error> {
        self.labelled(vec![
            LABEL_MANAGED.to_owned(),
            LABEL_SERVICE_ID.to_owned(),
            format!("{LABEL_NAMESPACE}={}", service.namespace),
            format!("{LABEL_SERVICE_NAME}={}", service.name),
        ])
        .await
    }

    async fn labelled(
        &self,
        labels: Vec<String>,
    ) -> Result<Vec<(ContainerId, HashMap<String, String>)>, Error> {
        let filters = HashMap::from([("label", labels)]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        self.client
            .list_containers(Some(options))
            .await?
            .into_iter()
            .map(|container| {
                let id = container.id.ok_or(Error::MissingField("container ID"))?;
                let id = ContainerId::parse(id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })?;
                Ok((id, container.labels.unwrap_or_default()))
            })
            .collect()
    }
}

/// One of this Machine's copies of a Service, from [`ContainerRuntime::service_copies`].
#[derive(Clone, Debug)]
pub struct ServiceCopy {
    pub container_id: ContainerId,
    pub r#gen: Gen,
    pub rotation: ployz_core::CopyRotation,
    /// The Deploy run that created it, from its `LABEL_DEPLOYMENT`.
    pub deployment: Option<ployz_core::DeploymentLogId>,
}

/// One existing copy's rotation, read under the lock `remove` takes and held until
/// the rotation is written or the Container stopped or removed.
pub struct RotationTurn<'a> {
    runtime: &'a ContainerRuntime,
    operation: spec_store::ConfigOperation<'a>,
    container_id: ContainerId,
    rotation: ployz_core::CopyRotation,
}

impl RotationTurn<'_> {
    #[must_use]
    pub fn rotation(&self) -> &ployz_core::CopyRotation {
        &self.rotation
    }

    /// Record `rotation` and republish observations at once, without a Docker event.
    ///
    /// # Errors
    ///
    /// Returns when the Machine database cannot be written.
    pub async fn write(mut self, rotation: &ployz_core::CopyRotation) -> Result<(), Error> {
        Ok(self
            .operation
            .set_rotation(&self.container_id, rotation)
            .await?)
    }

    /// Remove the Container and its records, as [`ContainerRuntime::remove`] does.
    ///
    /// # Errors
    ///
    /// As [`ContainerRuntime::remove`].
    pub async fn remove(mut self, remove_volumes: bool, force: bool) -> Result<(), Error> {
        self.runtime
            .remove_locked(
                &mut self.operation,
                &self.container_id,
                remove_volumes,
                force,
            )
            .await
    }
}

#[derive(Clone)]
pub struct ContainerRuntime {
    docker: LocalDocker,
    specs: MachineSpecStore,
    sink: Option<ObservationSink>,
    stopping: watch::Sender<BTreeMap<ContainerId, usize>>,
    first_synced: watch::Sender<bool>,
    checks: Arc<std::sync::Mutex<CheckRecords>>,
    stranded: Arc<tokio::sync::Mutex<observe::StrandedIngress>>,
    sealing_key_dir: Option<PathBuf>,
    log_store: LogStore,
    /// Set when a Leave begins removing every managed Container, and cleared only when
    /// that Leave fails, so a committed Leave keeps it set.
    departing: Arc<std::sync::atomic::AtomicBool>,
}

impl ContainerRuntime {
    #[must_use]
    pub fn new(docker: LocalDocker, specs: MachineSpecStore) -> Self {
        Self {
            docker,
            specs,
            sink: None,
            stopping: watch::Sender::new(BTreeMap::new()),
            first_synced: watch::Sender::new(false),
            checks: Arc::new(std::sync::Mutex::new(CheckRecords::new(chrono::Utc::now()))),
            stranded: Arc::default(),
            sealing_key_dir: None,
            log_store: LogStore::default(),
            departing: Arc::default(),
        }
    }

    /// Opens Sealed env values with the Sealing Key kept in `data_dir`.
    #[must_use]
    pub fn with_sealing_key(mut self, data_dir: PathBuf) -> Self {
        self.sealing_key_dir = Some(data_dir);
        self
    }

    #[must_use]
    pub(crate) fn with_log_store(mut self, log_store: LogStore) -> Self {
        self.log_store = log_store;
        self
    }

    /// Whether a Leave is removing every managed Container here, or has committed. A
    /// Container call that overlapped it met the Leave's stops.
    #[must_use]
    pub fn departing(&self) -> bool {
        self.departing.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A Leave that failed leaves this Machine in the Cluster, taking Container work again.
    pub fn departure_failed(&self) {
        self.departing
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn floor_turn(&self) -> FloorTurn<'_> {
        self.specs.floor_turn().await
    }

    pub async fn clear_floors(&self) -> Result<(), Error> {
        Ok(self.specs.clear_floors().await?)
    }

    /// Take the lock `remove` takes and read a copy's rotation under it, so no other
    /// rotation write or removal lands before the turn is spent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ContainerNotFound`] when Docker no longer has the Container,
    /// and when the Machine database cannot be read.
    pub async fn rotation_turn(
        &self,
        container_id: &ContainerId,
    ) -> Result<RotationTurn<'_>, Error> {
        let operation = self.specs.config_operation().await;
        self.ensure_managed(container_id).await?;
        let rotation = self.specs.rotation(container_id).await?.unwrap_or_default();
        Ok(RotationTurn {
            runtime: self,
            operation,
            container_id: *container_id,
            rotation,
        })
    }

    /// Record that this Machine stopped `run` of the Container for a `Stopped` want.
    /// Observations of that run, once exited, report it stopped; a later run does not.
    ///
    /// # Errors
    ///
    /// Returns when the Machine database cannot be written.
    pub async fn record_stop(
        &self,
        container_id: &ContainerId,
        run: &ployz_core::ContainerRun,
    ) -> Result<(), Error> {
        Ok(self
            .specs
            .config_operation()
            .await
            .set_stopped(container_id, run)
            .await?)
    }

    /// The recorded rotations of `container_ids`, in one read. A copy that never left
    /// Pending has no entry.
    ///
    /// # Errors
    ///
    /// Returns when the Machine database cannot be read.
    pub async fn rotations(
        &self,
        container_ids: Vec<ContainerId>,
    ) -> Result<BTreeMap<ContainerId, ployz_core::CopyRotation>, Error> {
        Ok(self.specs.rotations(container_ids).await?)
    }

    /// The full spec this Machine recorded when it created `container_id`.
    ///
    /// # Errors
    ///
    /// Returns when the Machine database cannot be read.
    pub async fn spec(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<ployz_core::ResolvedServiceSpec>, Error> {
        Ok(self.specs.get(container_id).await?)
    }

    /// Report this Container as `stopping` until the returned Withdrawal drops,
    /// so the Ingress Proxies stop routing to it before it stops. Each operation
    /// holds its own; the Container returns to ingress once none is held.
    pub fn withdraw(&self, container_id: ContainerId) -> Withdrawal {
        self.stopping.send_if_modified(|stopping| {
            let held = stopping.entry(container_id).or_default();
            *held += 1;
            *held == 1
        });
        Withdrawal {
            stopping: self.stopping.clone(),
            container_id,
        }
    }

    fn is_withdrawn(&self, container_id: &ContainerId) -> bool {
        self.stopping.borrow().contains_key(container_id)
    }

    #[cfg(test)]
    pub(crate) fn stopping_ids_for_test(&self) -> std::collections::BTreeSet<ContainerId> {
        self.stopping.borrow().keys().copied().collect()
    }

    #[cfg(test)]
    pub(crate) fn subscribe_stopping_for_test(
        &self,
    ) -> watch::Receiver<BTreeMap<ContainerId, usize>> {
        self.stopping.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn spec_store_path_for_test(&self) -> &std::path::Path {
        self.specs.path_for_test()
    }

    /// Drop what the daemon remembers about a container Docker no longer has.
    async fn forget(&self, container_id: &ContainerId) {
        self.checks
            .lock()
            .expect("check records are never poisoned")
            .forget(container_id);
        let mut stranded = self.stranded().await;
        stranded.held.forget(container_id);
        stranded.gates.remove(container_id);
    }

    pub async fn open(spec_store: impl Into<PathBuf>) -> Result<Self, Error> {
        Ok(Self::new(
            LocalDocker::connect()?,
            MachineSpecStore::open(spec_store).await?,
        ))
    }

    pub async fn list_images(
        &self,
        reference: Option<&str>,
        last_tagged: bool,
    ) -> Result<MachineImages, Error> {
        let filters = reference
            .map(|reference| HashMap::from([("reference", vec![reference])]))
            .unwrap_or_default();
        let options = ListImagesOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .manifests(true)
            .build();
        let listed = self.docker.client.list_images(Some(options)).await?;
        let tagged = if last_tagged {
            futures_util::future::try_join_all(
                listed.iter().map(|image| self.last_tagged(&image.id)),
            )
            .await?
        } else {
            vec![None; listed.len()]
        };
        let images = listed
            .into_iter()
            .zip(tagged)
            .map(|(image, last_tagged)| ImageSummary {
                last_tagged,
                ..project_image(image)
            })
            .collect();
        let info = self.docker.client.info().await?;
        Ok(MachineImages {
            containerd_store: info.driver_status.as_deref().is_some_and(containerd_store),
            images,
            docker_root: info
                .docker_root_dir
                .and_then(|root| crate::host_capacity::filesystem_space(&root).ok())
                .map(|(total_bytes, free_bytes)| DiskSpace {
                    total_bytes,
                    free_bytes,
                }),
        })
    }

    #[must_use]
    pub(crate) fn local_docker(&self) -> LocalDocker {
        self.docker.clone()
    }

    pub async fn list_managed(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut observations = Vec::new();
        let mut inspected = futures_util::stream::iter(self.managed_inventory().await?)
            .map(|container_id| async move {
                (
                    container_id,
                    self.inspect_managed(&container_id, machine_id).await,
                )
            })
            .buffered(8);
        while let Some((container_id, result)) = inspected.next().await {
            match result {
                Ok(observation) => observations.push(observation),
                Err(Error::SpecNotFound(_))
                    if self
                        .discard_half_made(&container_id)
                        .await
                        .unwrap_or_else(|error| {
                            eprintln!(
                                "couldn't remove container {container_id}, which has no spec: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            );
                            false
                        }) =>
                {
                    eprintln!(
                        "removed container {container_id}, which a crash left without its spec before it started"
                    );
                }
                Err(error) if malformed_container(&error) => {
                    eprintln!(
                        "ignoring malformed managed container {container_id}: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(observations)
    }

    /// Every managed Container's ID. A Container a container-level restore recreated
    /// under a new ID first takes over the records its slot's old ID left, so it is
    /// observed as it was before the restore.
    ///
    /// # Errors
    ///
    /// Returns when Docker or the Machine database cannot be read.
    pub(crate) async fn managed_inventory(&self) -> Result<Vec<ContainerId>, Error> {
        let containers = self.docker.managed_containers().await?;
        self.adopt_unrotated(&containers).await?;
        self.adopt_unprobed(&containers).await?;
        let listed = containers
            .into_iter()
            .map(|(id, labels)| (id, creation_slot(&labels)))
            .collect::<Vec<_>>();
        if !self.specs.record_creations(listed.clone()).await? {
            return Ok(listed.into_iter().map(|(id, _)| id).collect());
        }
        // Creates and removes hold this lock across their Docker call and spec write,
        // so only a listing taken under it agrees with the records it moves.
        let mut operation = self.specs.config_operation().await;
        let listed = self.managed_slots().await?;
        for (old, new) in operation.adopt_restored(listed.clone()).await? {
            eprintln!("adopted restored container {new}, which replaces {old}");
        }
        Ok(listed.into_iter().map(|(id, _)| id).collect())
    }

    /// A Service Container with no gen label was created by a daemon from before
    /// rotation, which routed every copy it ran. Until a rotation is written for it,
    /// it is In.
    async fn adopt_unrotated(
        &self,
        containers: &[(ContainerId, HashMap<String, String>)],
    ) -> Result<(), Error> {
        let unstamped = containers
            .iter()
            .filter(|(_, labels)| {
                !labels.contains_key(LABEL_GEN)
                    && ManagedLabels::parse(labels).is_ok_and(|managed| {
                        managed.kind == ContainerKind::ServiceContainer
                            && managed.identity != QualifiedService::system_ingress()
                    })
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if unstamped.is_empty() {
            return Ok(());
        }
        let unrotated = self.specs.unrotated(unstamped).await?;
        if unrotated.is_empty() {
            return Ok(());
        }
        // Rotation turns read under this lock, so none decides on Pending meanwhile.
        Ok(self
            .specs
            .config_operation()
            .await
            .adopt_unrotated(unrotated)
            .await?)
    }

    /// A user Service Container with no gen label, stored with no Healthcheck, which
    /// Docker runs with no probe, is recorded as Disabled. See
    /// [`ResolvedServiceSpec::adopt_unprobed`]. Every release that stored no Healthcheck
    /// predates the gen label. A Container this daemon created keeps the spec it was
    /// asked for, so a keyed create retried with that request still matches it. Only
    /// a Container stored with no Healthcheck is inspected.
    async fn adopt_unprobed(
        &self,
        containers: &[(ContainerId, HashMap<String, String>)],
    ) -> Result<(), Error> {
        let unstamped = containers
            .iter()
            .filter(|(_, labels)| {
                !labels.contains_key(LABEL_GEN)
                    && ManagedLabels::parse(labels).is_ok_and(|managed| {
                        managed.kind == ContainerKind::ServiceContainer
                            && !managed.identity.namespace.is_reserved()
                    })
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if unstamped.is_empty() {
            return Ok(());
        }
        let mut unprobed = Vec::new();
        // One whose image runs a probe keeps None and is inspected each sync until a
        // Deploy replaces it, a cost only 0.2.4 copies of a HEALTHCHECK image pay.
        for container_id in self.specs.without_healthcheck(unstamped).await? {
            // A Container Docker cannot inspect now is left for the next sync.
            let Ok(inspected) = decode_raw_inspect(
                self.docker
                    .client
                    .inspect_container(container_id.as_str(), None)
                    .await,
                &container_id,
            ) else {
                continue;
            };
            if !matches!(
                effective_healthcheck(inspected.config.as_ref()),
                Some(HealthcheckSpec::Configured(_))
            ) {
                unprobed.push(container_id);
            }
        }
        if unprobed.is_empty() {
            return Ok(());
        }
        let mut operation = self.specs.config_operation().await;
        if operation.adopt_unprobed(unprobed).await? {
            operation.republish();
        }
        Ok(())
    }

    async fn managed_slots(
        &self,
    ) -> Result<Vec<(ContainerId, Option<spec_store::CreationSlot>)>, Error> {
        Ok(self
            .docker
            .managed_containers()
            .await?
            .into_iter()
            .map(|(id, labels)| (id, creation_slot(&labels)))
            .collect())
    }

    /// This Machine's Service Containers of `service`, each with the gen its labels
    /// carry and the rotation this Machine recorded. Nothing is inspected, so no health
    /// check runs.
    ///
    /// # Errors
    ///
    /// Returns when Docker or the Machine database cannot be read.
    pub async fn service_copies(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ServiceCopy>, Error> {
        let listed = self
            .docker
            .service_containers(service)
            .await?
            .into_iter()
            .filter(|(_, labels)| !labels.contains_key(LABEL_HOOK))
            .collect::<Vec<_>>();
        let mut rotations = self
            .specs
            .rotations(listed.iter().map(|(id, _)| *id).collect())
            .await?;
        Ok(listed
            .into_iter()
            .map(|(container_id, labels)| ServiceCopy {
                container_id,
                r#gen: label_gen(&labels),
                rotation: rotations.remove(&container_id).unwrap_or_default(),
                deployment: labels
                    .get(LABEL_DEPLOYMENT)
                    .and_then(|label| ployz_core::DeploymentLogId::parse(label).ok()),
            })
            .collect())
    }

    /// Read fresh Docker bridge and host telemetry.
    ///
    /// # Errors
    ///
    /// Returns when Docker or a required host probe cannot be read.
    pub(crate) async fn telemetry(&self) -> Result<MachineTelemetry, Error> {
        self.docker.telemetry().await
    }

    /// Read fresh Ployz bridge endpoint capacity.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Ployz bridge.
    pub(crate) async fn bridge_capacity(&self) -> Result<BridgeEndpointCapacity, Error> {
        self.docker.bridge_capacity().await
    }

    /// Docker's runtime state for `container_id`. No health check runs.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Container.
    pub async fn inspect_runtime(
        &self,
        container_id: &ContainerId,
    ) -> Result<ContainerRuntimeObservation, Error> {
        let inspected = decode_raw_inspect(
            self.docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            container_id,
        )?;
        Ok(runtime_observation(inspected.state.as_ref()))
    }

    /// The run of `container_id` that exited at or after `since`: what a stop issued
    /// at `since` ended. Docker stamps the exit on this host's clock. `None` while it
    /// runs, and for a run that exited before `since`, which the stop found over.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Container.
    pub async fn run_ended_since(
        &self,
        container_id: &ContainerId,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<ployz_core::ContainerRun>, Error> {
        let inspected = decode_raw_inspect(
            self.docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            container_id,
        )?;
        let ended = matches!(
            runtime_observation(inspected.state.as_ref()),
            ContainerRuntimeObservation::Exited { .. }
        ) && finished_at(inspected.state.as_ref())
            .is_some_and(|finished| finished >= since);
        Ok(ended.then(|| container_run(&inspected)).flatten())
    }

    /// The host pid of the Container's process, or `None` when Docker reports none.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Container.
    pub async fn pid(&self, container_id: &ContainerId) -> Result<Option<u32>, Error> {
        let inspected = decode_raw_inspect(
            self.docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            container_id,
        )?;
        Ok(inspected
            .state
            .as_ref()
            .and_then(|state| state.get("Pid"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid != 0))
    }

    /// The record of the Sealed values `container_id` was given, from its label.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Container or its label does not parse.
    pub(crate) async fn sealed_record(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<crate::sealed_env::SealedRecord>, Error> {
        let inspected = decode_raw_inspect(
            self.docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            container_id,
        )?;
        let label = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(crate::sealed_env::LABEL_SEALED));
        Ok(crate::sealed_env::record_from_label(
            label.map(String::as_str),
        )?)
    }

    pub(crate) fn sealing_key_dir(&self) -> Option<&Path> {
        self.sealing_key_dir.as_deref()
    }

    pub async fn inspect_managed(
        &self,
        container_id: &ContainerId,
        machine_id: &MachineId,
    ) -> Result<ContainerObservation, Error> {
        Ok(self
            .inspect_managed_details(container_id, machine_id)
            .await?
            .container)
    }

    /// Inspect managed identity, current health, and live environment from Docker.
    ///
    /// # Errors
    /// Rejects Docker failures, malformed metadata, and Containers not managed by Ployz.
    pub async fn inspect_managed_details(
        &self,
        container_id: &ContainerId,
        machine_id: &MachineId,
    ) -> Result<ployz_core::ContainerDetails, Error> {
        Ok(self
            .inspect_managed_run(container_id, machine_id)
            .await?
            .details)
    }

    /// [`Self::inspect_managed_details`], and whether Docker OOM-killed a process of
    /// the run that same inspect saw.
    ///
    /// # Errors
    /// Rejects Docker failures, malformed metadata, and Containers not managed by Ployz.
    pub async fn inspect_managed_run(
        &self,
        container_id: &ContainerId,
        machine_id: &MachineId,
    ) -> Result<InspectedRun, Error> {
        let inspected = decode_raw_inspect(
            self.docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            container_id,
        )?;
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::MissingField("container labels"))?;
        let ManagedLabels { identity, kind } = ManagedLabels::parse(labels)?;
        let resolved_spec = self
            .specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        let rotation = self.specs.rotation(container_id).await?.unwrap_or_default();
        let r#gen = label_gen(labels);

        let environment = inspected.config.as_ref().and_then(inspected_environment);
        let address = container_address(&inspected);
        let run = container_run(&inspected);
        let mut runtime = runtime_observation(inspected.state.as_ref());
        let mut effective_check = effective_healthcheck(inspected.config.as_ref());
        let mut last_check = last_docker_check(inspected.state.as_ref(), || {
            crate::sealed_env::Lines::new(
                *container_id,
                crate::sealed_env::record_from_label(
                    labels
                        .get(crate::sealed_env::LABEL_SEALED)
                        .map(String::as_str),
                ),
                self.sealing_key_dir(),
            )
        });
        if kind == ContainerKind::ServiceContainer
            && let Some(HealthcheckSpec::Http(check)) = &resolved_spec.container.healthcheck
        {
            effective_check = Some(HealthcheckSpec::Http(check.clone()));
            if matches!(runtime, ContainerRuntimeObservation::Running { .. }) {
                let (health, probed) = http_health_probe(address, check).await;
                runtime = ContainerRuntimeObservation::Running { health };
                last_check = probed;
            }
        }
        if let ContainerRuntimeObservation::Running { health } = runtime {
            let deadline = match &resolved_spec.container.healthcheck {
                Some(spec) => spec.deadline(),
                None => effective_check
                    .as_ref()
                    .and_then(HealthcheckSpec::as_configured)
                    .map(|check| healthcheck_deadline(Some(check))),
            };
            runtime = ContainerRuntimeObservation::Running {
                health: self
                    .checks
                    .lock()
                    .expect("check records are never poisoned")
                    .settle(
                        container_id,
                        started_at(inspected.state.as_ref()),
                        deadline,
                        chrono::Utc::now(),
                        health,
                    ),
            };
        }
        let runtime = withdrawn(runtime, self.is_withdrawn(container_id));
        let stopped = if matches!(runtime, ContainerRuntimeObservation::Exited { .. })
            && let Some(run) = &run
        {
            self.specs.stopped_run(container_id).await?.as_ref() == Some(run)
        } else {
            false
        };
        let container =
            ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
                rotation: Some(rotation),
                r#gen: Some(r#gen),
                container_id: *container_id,
                display_name: display_name(inspected.name.as_deref()),
                created_at_unix_nanos: created_at_unix_nanos(inspected.created.as_deref()),
                machine_id: *machine_id,
                namespace: identity.namespace,
                kind,
                runtime,
                effective_healthcheck: effective_check,
                resolved_spec,
                address,
                labels: labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                stopped,
                run: run.clone(),
            })?;
        let oom_killed = inspected
            .state
            .as_ref()
            .and_then(|state| state.get("OOMKilled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Ok(InspectedRun {
            details: ployz_core::ContainerDetails {
                container,
                environment,
                image_id: inspected.image,
                last_check,
                run,
            },
            oom_killed,
        })
    }
}

/// One inspect of a managed Container.
pub struct InspectedRun {
    pub details: ployz_core::ContainerDetails,
    /// Docker OOM-killed a process of the run this inspect saw.
    pub oom_killed: bool,
}

/// One operation's hold on a Container's `stopping` mark.
#[must_use = "the Container returns to ingress when its Withdrawal drops"]
pub struct Withdrawal {
    stopping: watch::Sender<BTreeMap<ContainerId, usize>>,
    container_id: ContainerId,
}

impl Drop for Withdrawal {
    fn drop(&mut self) {
        self.stopping.send_if_modified(|stopping| {
            let Some(held) = stopping.get_mut(&self.container_id) else {
                return false;
            };
            *held -= 1;
            if *held > 0 {
                return false;
            }
            stopping.remove(&self.container_id);
            true
        });
    }
}

/// A Running Container marked stopping reports health `stopping`, which no proxy routes to.
fn withdrawn(runtime: ContainerRuntimeObservation, stopping: bool) -> ContainerRuntimeObservation {
    if stopping && matches!(runtime, ContainerRuntimeObservation::Running { .. }) {
        return ContainerRuntimeObservation::Running {
            health: HealthObservation::Stopping,
        };
    }
    runtime
}

#[cfg(test)]
#[test]
fn withdrawn_hides_only_running_containers_marked_stopping() {
    let healthy = ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };
    let failing = ContainerRuntimeObservation::Running {
        health: HealthObservation::Failing,
    };
    let stopping = ContainerRuntimeObservation::Running {
        health: HealthObservation::Stopping,
    };
    assert_eq!(withdrawn(healthy.clone(), true), stopping);
    assert_eq!(withdrawn(failing.clone(), true), stopping);
    assert!(!withdrawn(failing, true).may_serve());
    assert!(!withdrawn(healthy.clone(), true).may_serve());
    assert_eq!(withdrawn(healthy.clone(), false), healthy);
    assert_eq!(
        withdrawn(ContainerRuntimeObservation::Exited { code: 0 }, true),
        ContainerRuntimeObservation::Exited { code: 0 }
    );
}

fn decode_raw_inspect(
    result: Result<ContainerInspectResponse, bollard::errors::Error>,
    container_id: &ContainerId,
) -> Result<RawContainerInspect, Error> {
    match result {
        Ok(inspected) => Ok(RawContainerInspect::from_typed(&inspected)?),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str(&contents)?)
        }
        Err(error) => Err(docker_error(container_id, error)),
    }
}

#[allow(clippy::wildcard_enum_match_arm)]
fn docker_error(container_id: &ContainerId, error: bollard::errors::Error) -> Error {
    match error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => Error::ContainerNotFound(*container_id),
        error => Error::Docker(error),
    }
}

fn malformed_container(error: &Error) -> bool {
    matches!(
        error,
        Error::Observation(_)
            | Error::Json(_)
            | Error::SpecStore(SpecStoreError::Json(_))
            | Error::MissingField(_)
            | Error::MissingLabel(_)
            | Error::InvalidValue { .. }
            | Error::NotManaged
            | Error::SpecNotFound(_)
            | Error::ContainerNotFound(_)
    )
}

fn some_map<K, V>(values: impl IntoIterator<Item = (K, V)>) -> Option<HashMap<K, V>>
where
    K: Eq + std::hash::Hash,
{
    let values = values.into_iter().collect::<HashMap<_, _>>();
    (!values.is_empty()).then_some(values)
}

fn project_image(image: bollard::models::ImageSummary) -> ImageSummary {
    let mut platforms = image
        .manifests
        .unwrap_or_default()
        .into_iter()
        .filter(|manifest| {
            manifest.available && manifest.kind == Some(ImageManifestSummaryKindEnum::IMAGE)
        })
        .filter_map(|manifest| {
            let platform = manifest.image_data?.platform;
            let mut value = format!("{}/{}", platform.os?, platform.architecture?);
            if let Some(variant) = platform.variant {
                value.push('/');
                value.push_str(&variant);
            }
            Some(value)
        })
        .collect::<Vec<_>>();
    platforms.sort();
    platforms.dedup();
    ImageSummary {
        id: image.id,
        repo_tags: image.repo_tags,
        created: image.created,
        size: image.size,
        containers: image.containers,
        platforms,
        last_tagged: None,
    }
}

fn containerd_store(status: &[Vec<String>]) -> bool {
    status.iter().flatten().any(|value| {
        value.eq_ignore_ascii_case("io.containerd.snapshotter.v1")
            || value.to_ascii_lowercase().contains("containerd")
    })
}

fn display_name(name: Option<&str>) -> String {
    name.unwrap_or_default().trim_start_matches('/').to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerInspect {
    name: Option<String>,
    restart_count: Option<i64>,
    created: Option<String>,
    image: Option<String>,
    config: Option<RawContainerConfig>,
    state: Option<serde_json::Value>,
    network_settings: Option<RawNetworkSettings>,
}

impl RawContainerInspect {
    fn from_typed(inspected: &ContainerInspectResponse) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(inspected)?)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerConfig {
    env: Option<Vec<String>>,
    labels: Option<HashMap<String, String>>,
    healthcheck: Option<HealthConfig>,
}

fn inspected_environment(
    config: &RawContainerConfig,
) -> Option<std::collections::BTreeMap<String, String>> {
    parse_environment(config.env.as_deref().unwrap_or_default())
}

/// Docker's `Config.Env` entries as a map; `None` when an entry has no `KEY=`.
pub(crate) fn parse_environment(
    entries: &[String],
) -> Option<std::collections::BTreeMap<String, String>> {
    entries
        .iter()
        .map(|entry| {
            entry
                .split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.into(), value.into()))
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawNetworkSettings {
    networks: Option<HashMap<String, RawEndpointSettings>>,
}

#[derive(Deserialize)]
struct RawEndpointSettings {
    #[serde(rename = "IPAddress")]
    ip_address: Option<String>,
}

fn container_run(inspected: &RawContainerInspect) -> Option<ployz_core::ContainerRun> {
    let started = started_at(inspected.state.as_ref())?;
    Some(ployz_core::ContainerRun {
        restart_count: inspected.restart_count.unwrap_or_default(),
        started_at_unix_nanos: started.timestamp_nanos_opt()?,
    })
}

fn started_at(state: Option<&serde_json::Value>) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(state?.get("StartedAt")?.as_str()?)
        .ok()
        .map(Into::into)
}

fn finished_at(state: Option<&serde_json::Value>) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(state?.get("FinishedAt")?.as_str()?)
        .ok()
        .map(Into::into)
}

fn created_at_unix_nanos(created: Option<&str>) -> i64 {
    created
        .and_then(|created| chrono::DateTime::parse_from_rfc3339(created).ok())
        .and_then(|created| created.timestamp_nanos_opt())
        .unwrap_or_default()
}

struct ManagedLabels {
    identity: QualifiedService,
    kind: ContainerKind,
}

impl ManagedLabels {
    fn parse(labels: &HashMap<String, String>) -> Result<Self, Error> {
        if !labels.contains_key(LABEL_MANAGED) {
            return Err(Error::NotManaged);
        }
        let namespace = required_label(labels, LABEL_NAMESPACE)?;
        let service_id = required_label(labels, LABEL_SERVICE_ID)?;
        let service_name = required_label(labels, LABEL_SERVICE_NAME)?;
        ServiceId::parse(service_id).map_err(|source| Error::InvalidValue {
            field: LABEL_SERVICE_ID,
            source,
        })?;
        Ok(Self {
            identity: QualifiedService::new(
                Namespace::parse(namespace).map_err(|source| Error::InvalidValue {
                    field: LABEL_NAMESPACE,
                    source,
                })?,
                ServiceName::parse(service_name).map_err(|source| Error::InvalidValue {
                    field: LABEL_SERVICE_NAME,
                    source,
                })?,
            ),
            kind: match labels.get(LABEL_HOOK) {
                None => ContainerKind::ServiceContainer,
                Some(_) => ContainerKind::PreDeployHook,
            },
        })
    }
}

fn creation_slot(labels: &HashMap<String, String>) -> Option<spec_store::CreationSlot> {
    let ManagedLabels { identity, kind } = ManagedLabels::parse(labels).ok()?;
    let key = labels.get(ployz_core::labels::CREATION_KEY)?;
    Some(spec_store::CreationSlot::new(
        &identity.namespace,
        kind,
        key,
    ))
}

fn label_gen(labels: &HashMap<String, String>) -> Gen {
    labels
        .get(LABEL_GEN)
        .and_then(|label| label.parse().ok())
        .map_or(Gen::FIRST, Gen::new)
}

fn required_label<'a>(
    labels: &'a HashMap<String, String>,
    name: &'static str,
) -> Result<&'a str, Error> {
    labels
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(Error::MissingLabel(name))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectState {
    #[serde(default)]
    status: String,
    exit_code: Option<i64>,
    health: Option<InspectHealth>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectHealth {
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectHealthLog {
    #[serde(default)]
    start: String,
    exit_code: i64,
    #[serde(default)]
    output: String,
}

fn last_docker_check(
    state: Option<&serde_json::Value>,
    lines: impl FnOnce() -> crate::sealed_env::Lines,
) -> Option<LastHealthCheck> {
    let newest = state?.get("Health")?.get("Log")?.as_array()?.last()?;
    let newest = InspectHealthLog::deserialize(newest).ok()?;
    let checked = chrono::DateTime::parse_from_rfc3339(&newest.start).ok();
    if let (Some(checked), Some(started)) = (checked, started_at(state))
        && checked < started
    {
        return None;
    }
    let output = lines().text(&newest.output).unwrap_or_default();
    Some(LastHealthCheck::exited(newest.exit_code, &output))
}

fn runtime_observation(state: Option<&serde_json::Value>) -> ContainerRuntimeObservation {
    let Some(state) = state else {
        return ContainerRuntimeObservation::Unknown {
            raw: json!({ "state": null }),
        };
    };
    let parsed = InspectState::deserialize(state).unwrap_or_default();
    runtime_from_parts_with_raw(
        &parsed.status,
        parsed.exit_code,
        parsed
            .health
            .as_ref()
            .and_then(|health| health.status.as_deref()),
        &state_without_app_output(state),
    )
}

fn state_without_app_output(state: &serde_json::Value) -> serde_json::Value {
    let mut decoded = serde_json::Map::new();
    for field in ["Status", "ExitCode"] {
        if let Some(value) = state.get(field) {
            decoded.insert(field.into(), value.clone());
        }
    }
    if let Some(health) = state.get("Health").and_then(|health| health.get("Status")) {
        decoded.insert("Health".into(), json!({ "Status": health }));
    }
    serde_json::Value::Object(decoded)
}

fn effective_healthcheck(config: Option<&RawContainerConfig>) -> Option<HealthcheckSpec> {
    let healthcheck = config?.healthcheck.as_ref()?;
    let test = healthcheck.test.clone().unwrap_or_default();
    if test
        .first()
        .is_some_and(|command| command == HEALTHCHECK_DISABLE_SENTINEL)
    {
        return Some(HealthcheckSpec::Disabled);
    }
    Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(test).ok()?,
        interval_millis: nanos_to_millis(healthcheck.interval),
        timeout_millis: nanos_to_millis(healthcheck.timeout),
        start_period_millis: nanos_to_millis(healthcheck.start_period),
        start_interval_millis: nanos_to_millis(healthcheck.start_interval),
        retries: healthcheck
            .retries
            .filter(|retries| *retries > 0)
            .and_then(|retries| u32::try_from(retries).ok()),
        deadline_millis: None,
    }))
}

fn nanos_to_millis(nanos: Option<i64>) -> Option<u64> {
    nanos
        .filter(|nanos| *nanos > 0)
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(|nanos| nanos.div_ceil(1_000_000))
}

#[cfg(test)]
fn runtime_from_parts(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
) -> ContainerRuntimeObservation {
    runtime_from_parts_with_raw(
        status,
        exit_code,
        health,
        &json!({ "Status": status, "ExitCode": exit_code, "Health": health }),
    )
}

fn runtime_from_parts_with_raw(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
    raw: &serde_json::Value,
) -> ContainerRuntimeObservation {
    match status {
        "created" => ContainerRuntimeObservation::Created,
        "running" => ContainerRuntimeObservation::Running {
            health: match health {
                None | Some("none") => HealthObservation::NotConfigured,
                Some("starting") => HealthObservation::Starting,
                Some("healthy") => HealthObservation::Healthy,
                Some("unhealthy") => HealthObservation::Unhealthy,
                Some(value) => HealthObservation::Unrecognized(value.to_owned()),
            },
        },
        "paused" => ContainerRuntimeObservation::Paused,
        "restarting" => ContainerRuntimeObservation::Restarting,
        "exited" if exit_code.is_some() => ContainerRuntimeObservation::Exited {
            code: exit_code.expect("checked"),
        },
        "removing" => ContainerRuntimeObservation::Removing,
        "dead" => ContainerRuntimeObservation::Dead,
        _ => ContainerRuntimeObservation::Unknown { raw: raw.clone() },
    }
}

fn container_address(inspected: &RawContainerInspect) -> Option<ContainerAddress> {
    inspected
        .network_settings
        .as_ref()?
        .networks
        .as_ref()?
        .get(crate::network::DOCKER_NETWORK_NAME)?
        .ip_address
        .as_deref()?
        .parse::<Ipv4Addr>()
        .ok()
        .map(ContainerAddress)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Direct Image Transfer requires Docker's containerd image store")]
    UnsupportedImageStore,
    #[error(transparent)]
    Observation(#[from] ployz_core::ContainerObservationError),
    /// A required host telemetry read failed.
    #[error("host telemetry failed")]
    Io(#[from] std::io::Error),
    /// The Ployz bridge has no endpoint available for a new Container.
    #[error("Ployz Docker bridge has no free endpoint capacity")]
    EndpointCapacity,
    #[error("Docker operation failed")]
    Docker(#[from] bollard::errors::Error),
    #[error("Docker inspect JSON failed")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Network(#[from] crate::network::NetworkError),
    #[error(transparent)]
    Sealed(#[from] crate::sealed_env::OpenError),
    #[error(transparent)]
    LogStoreUpgrading(#[from] crate::sealed_compat::LogStoreUpgrading),
    #[error(transparent)]
    SpecStore(#[from] SpecStoreError),
    #[error(transparent)]
    ReplicatedStore(#[from] crate::corrosion::Error),
    #[error("managed container is missing {0}")]
    MissingField(&'static str),
    #[error("managed container is missing label {0}")]
    MissingLabel(&'static str),
    #[error("managed container has invalid {field}")]
    InvalidValue {
        field: &'static str,
        #[source]
        source: ValueError,
    },
    #[error("invalid container configuration: {0}")]
    InvalidContainerConfig(String),
    #[error("invalid Docker Provisioned Volume observation: {0}")]
    InvalidVolumeStatus(&'static str),
    #[error("Docker returned Volume '{actual}' when Volume '{requested}' was inspected")]
    UnexpectedVolumeName {
        requested: DockerVolumeName,
        actual: String,
    },
    /// A mounted external Volume is absent from the target Machine.
    #[error("external Docker Volume '{0}' does not exist on this Machine")]
    ExternalVolumeNotFound(DockerVolumeName),
    /// An existing managed Volume differs from the resolved mounted source.
    #[error("Docker Volume '{name}' does not match its resolved source: {reason}")]
    VolumeShapeMismatch {
        name: DockerVolumeName,
        reason: String,
    },
    /// Docker refused to remove a Volume that containers still mount.
    #[error("{message}")]
    VolumeInUse {
        message: String,
        services: Vec<QualifiedService>,
    },
    /// Docker created a Volume but its resulting state could not be observed.
    #[error("Docker Volume creation succeeded but verification failed for {id}: {error}")]
    VolumeCreatedButUnverified {
        id: DockerVolumeId,
        error: Box<RpcError>,
    },
    /// Docker reported that the ployz volume plugin failed to list its Volumes.
    #[error("the ployz volume plugin could not list Volumes: {0}")]
    VolumePluginListFailed(String),
    /// Fresh local storage evidence was unavailable for a Provisioned Volume container.
    #[error("local storage could not be observed for a mounted Provisioned Volume")]
    StorageUnobservable,
    /// The target Machine has no storage capable of hosting a Provisioned Volume.
    #[error("this Machine cannot host a mounted Provisioned Volume")]
    ProvisionedStorageUnsupported,
    /// The complete Resolved Service Spec does not permit this target Machine.
    #[error("this Machine does not satisfy the resolved Service placement")]
    ServicePlacementMismatch,
    /// The target Machine no longer accepts this kind of Service work.
    #[error("this Machine does not accept this Service role")]
    WorkNotAccepted,
    #[error("container is not managed by Ployz")]
    NotManaged,
    #[error("container {0} is not a Service Container")]
    NotAServiceContainer(ContainerId),
    /// The requested spec is not a limits-only change of this Container's spec.
    #[error("container {container_id} cannot take these limits in place: {refusal}")]
    LimitsRefused {
        container_id: ContainerId,
        refusal: LimitsRefusal,
    },
    /// Docker accepted the update but reports other limits.
    #[error("Docker reports other limits on container {container_id} after an update: {limits}")]
    LimitsUnapplied {
        container_id: ContainerId,
        limits: Box<UnappliedLimits>,
    },
    /// Docker took the update, but the Container was not running when read back.
    #[error("container {0} stopped after its limits update")]
    StoppedAfterLimitsUpdate(ContainerId),
    #[error("resolved spec not found in machine.db for {0}")]
    SpecNotFound(ContainerId),
    #[error("container {0} was not found")]
    ContainerNotFound(ContainerId),
    #[error("Docker name '{0}' is occupied by incompatible creation inputs")]
    SlotNameOccupied(String),
    #[error("image pull failed")]
    ImagePull(#[source] bollard::errors::Error),
    /// A registry or peer pull failed while the Docker root had almost no free space.
    #[error("{full}")]
    DiskFull {
        full: ployz_core::DiskFull,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("pre-deploy container requested without a pre-deploy hook")]
    MissingPreDeployHook,
    #[error("container duration exceeds Docker's range")]
    DurationOverflow,
    #[error("container size exceeds Docker's range")]
    SizeOverflow,
    #[error("Docker event stream closed")]
    EventStreamClosed,
    #[error("system clock cannot be represented for Docker event replay: {0}")]
    Clock(String),
    #[error("peer image pull failed: {0}")]
    PeerPull(String),
    /// The disposable image-ingest helper did not become reachable in time.
    #[error("Unregistry did not accept TCP at {address} within {} seconds", .timeout.as_secs_f64())]
    UnregistryNotReady {
        /// Management-plane endpoint that failed readiness.
        address: SocketAddr,
        /// Maximum time allowed for the helper to accept TCP.
        timeout: Duration,
    },
}

impl Error {
    #[must_use]
    pub const fn rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::ContainerNotFound(_)
            | Self::SpecNotFound(_)
            | Self::ExternalVolumeNotFound(_)
            | Self::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
            | Self::ImagePull(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }) => RpcErrorCode::NotFound,
            Self::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                ..
            })
            | Self::ImagePull(bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                ..
            })
            | Self::VolumeShapeMismatch { .. }
            | Self::VolumeInUse { .. }
            | Self::SlotNameOccupied(_)
            | Self::ServicePlacementMismatch
            | Self::LimitsRefused { .. }
            | Self::WorkNotAccepted => RpcErrorCode::Conflict,
            Self::ProvisionedStorageUnsupported | Self::UnsupportedImageStore => RpcErrorCode::Unsupported,
            Self::VolumeCreatedButUnverified { .. }
            | Self::StorageUnobservable
            | Self::VolumePluginListFailed(_)
            | Self::EventStreamClosed
            // A peer pull is idempotent, so a rerun is safe whatever docker printed.
            | Self::PeerPull(_)
            | Self::UnregistryNotReady { .. }
            | Self::Sealed(crate::sealed_env::OpenError::KeyMissing)
            | Self::LogStoreUpgrading(_) => RpcErrorCode::Unavailable,
            Self::Sealed(_) | Self::MissingPreDeployHook
            | Self::EndpointCapacity
            | Self::DurationOverflow
            | Self::SizeOverflow
            | Self::InvalidContainerConfig(_)
            | Self::SpecStore(SpecStoreError::InvalidConfig(_))
            | Self::MissingField(_)
            | Self::MissingLabel(_)
            | Self::InvalidValue { .. }
            | Self::NotAServiceContainer(_)
            | Self::NotManaged => RpcErrorCode::InvalidArgument,
            Self::Docker(_)
            | Self::ImagePull(_)
            | Self::DiskFull { .. }
            | Self::Io(_)
            | Self::Json(_)
            | Self::Network(_)
            | Self::SpecStore(_)
            | Self::ReplicatedStore(_)
            | Self::Clock(_)
            | Self::LimitsUnapplied { .. }
            | Self::StoppedAfterLimitsUpdate(_)
            | Self::InvalidVolumeStatus(_)
            | Self::UnexpectedVolumeName { .. }
            | Self::Observation(_) => RpcErrorCode::Internal,
        }
    }
}

impl From<&Error> for RpcError {
    fn from(error: &Error) -> Self {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "only a few errors carry structured RPC details"
        )]
        let details = match error {
            Error::VolumeCreatedButUnverified { id, error } => serde_json::json!({
                "created_volume": id,
                "verification_error": error,
            }),
            Error::VolumeInUse { services, .. } if !services.is_empty() => {
                serde_json::json!({ "in_use_by": services })
            }
            Error::DiskFull { full, .. } => full.details(),
            Error::LimitsRefused { refusal, .. } => serde_json::json!({ "reason": refusal }),
            _ => serde_json::Value::Null,
        };
        let code = error.rpc_code();
        if code == RpcErrorCode::Internal {
            tracing::warn!(error = %ployz_core::error_chain::inline(error), "Docker request failed");
        }
        Self {
            details,
            ..Self::caused(code, error)
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineGateway, ResolvedServiceSpec};

    use super::*;
    use bollard::models::{
        ImageManifestSummary, ImageManifestSummaryImageData, ImageManifestSummaryKindEnum,
        OciPlatform,
    };

    #[test]
    fn a_docker_500_reaches_the_caller_as_one_cause() {
        let error = Error::Docker(bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "error from registry: denied".into(),
        });
        let rpc = RpcError::from(&error);
        assert_eq!(rpc.code, RpcErrorCode::Internal);
        assert_eq!(rpc.message, "Docker operation failed");
        assert_eq!(
            rpc.cause,
            ["Docker responded with status code 500: error from registry: denied"]
        );
    }

    #[test]
    fn runtime_failures_are_coded_by_kind() {
        for (error, code) in [
            (Error::EventStreamClosed, RpcErrorCode::Unavailable),
            (
                Error::PeerPull("manifest unknown".into()),
                RpcErrorCode::Unavailable,
            ),
            (
                Error::UnregistryNotReady {
                    address: "10.0.0.1:5000".parse().unwrap(),
                    timeout: Duration::from_secs(5),
                },
                RpcErrorCode::Unavailable,
            ),
            (
                Error::ProvisionedStorageUnsupported,
                RpcErrorCode::Unsupported,
            ),
        ] {
            assert_eq!(error.rpc_code(), code);
        }
    }

    #[test]
    fn image_projection_keeps_only_available_runnable_platforms_sorted() {
        let manifest = |available, kind, os: &str, architecture: &str| ImageManifestSummary {
            available,
            kind: Some(kind),
            image_data: Some(ImageManifestSummaryImageData {
                platform: OciPlatform {
                    os: Some(os.into()),
                    architecture: Some(architecture.into()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let image = bollard::models::ImageSummary {
            id: "sha256:abc".into(),
            repo_tags: vec!["example.test/api:1".into()],
            created: 1,
            size: 2,
            containers: 3,
            manifests: Some(vec![
                manifest(true, ImageManifestSummaryKindEnum::IMAGE, "linux", "arm64"),
                manifest(
                    true,
                    ImageManifestSummaryKindEnum::ATTESTATION,
                    "linux",
                    "amd64",
                ),
                manifest(false, ImageManifestSummaryKindEnum::IMAGE, "linux", "386"),
                manifest(true, ImageManifestSummaryKindEnum::IMAGE, "linux", "amd64"),
            ]),
            ..Default::default()
        };

        assert_eq!(
            project_image(image).platforms,
            ["linux/amd64", "linux/arm64"]
        );
        assert!(containerd_store(&[vec![
            "driver-type".into(),
            "io.containerd.snapshotter.v1".into()
        ]]));
    }

    #[test]
    fn create_body_translates_runtime_fields_and_hook_overrides() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "command": ["serve"],
                "entrypoint": ["/entrypoint"],
                "environment": { "TOKEN": "service" },
                "labels": { "example.empty": "", "example.value": "unchanged" },
                "hostname": "shared-host",
                "extra_hosts": ["api:192.0.2.10", "gateway:host-gateway"],
                "pull_policy": "missing",
                "privileged": false,
                "resources": { "memory_bytes": 1048576 },
                "healthcheck": { "state": "configured", "test": ["CMD", "true"], "interval_millis": 1000 }
            },
            "ports": [{
                "mode": "host",
                "bind": { "kind": "address", "address": "127.0.0.1" },
                "published_port": 8080,
                "container_port": 80,
                "transport_protocol": "tcp"
            }],
            "pre_deploy": {
                "command": ["migrate"],
                "environment": {
                    "TOKEN": "hook",
                    "PLOYZ_MACHINE_ID": "not-the-machine"
                },
                "privileged": true,
                "user": "1000"
            }
        }))
        .unwrap();

        let namespace = ployz_core::Namespace::parse("shop").unwrap();
        let regular = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &namespace,
            &spec,
            None,
        )
        .unwrap();
        assert!(
            regular
                .env
                .as_ref()
                .unwrap()
                .contains(&format!("PLOYZ_MACHINE_ID={machine_id}"))
        );
        assert_eq!(regular.cmd, Some(vec!["serve".into()]));
        assert_eq!(regular.hostname.as_deref(), Some("shared-host"));
        let regular_host = regular.host_config.unwrap();
        assert_eq!(regular_host.log_config, Some(container_log_config()));
        assert_eq!(
            regular_host.network_mode.as_deref(),
            Some(crate::network::DOCKER_NETWORK_NAME)
        );
        assert_eq!(regular_host.dns, Some(vec![gateway.0.to_string()]));
        assert_eq!(regular_host.dns_search, Some(vec!["shop.internal".into()]));
        assert_eq!(regular_host.dns_options, Some(vec!["ndots:1".into()]));
        assert_eq!(regular_host.memory, Some(1_048_576));
        assert_eq!(
            regular_host.extra_hosts,
            Some(vec!["api:192.0.2.10".into(), "gateway:host-gateway".into()])
        );
        assert_eq!(
            regular_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED)
        );
        assert!(regular_host.port_bindings.is_some());
        let regular_labels = regular.labels.as_ref().unwrap();
        assert_eq!(
            regular_labels.get(LABEL_NAMESPACE).map(String::as_str),
            Some("shop")
        );
        assert_eq!(
            regular_labels.get(LABEL_SERVICE_NAME).map(String::as_str),
            Some("api")
        );
        assert_eq!(
            regular_labels.get("example.value").map(String::as_str),
            Some("unchanged")
        );
        assert_eq!(
            regular_labels.get("example.empty").map(String::as_str),
            Some("")
        );
        assert!(!regular_labels.contains_key(LABEL_HOOK));

        let hook = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::PreDeployHook,
            &namespace,
            &spec,
            None,
        )
        .unwrap();
        assert_eq!(hook.cmd, Some(vec!["migrate".into()]));
        assert_eq!(regular.entrypoint, Some(vec!["/entrypoint".into()]));
        assert_eq!(hook.entrypoint, Some(Vec::new()));
        assert_eq!(hook.hostname.as_deref(), Some("shared-host"));
        assert_eq!(hook.user.as_deref(), Some("1000"));
        let hook_env = hook.env.as_ref().unwrap();
        assert!(hook_env.contains(&"TOKEN=hook".into()));
        assert!(hook_env.contains(&format!("PLOYZ_MACHINE_ID={machine_id}")));
        assert!(!hook_env.contains(&"PLOYZ_MACHINE_ID=not-the-machine".into()));
        assert!(hook_env.contains(&"PLOYZ_HOOK_PRE_DEPLOY=true".into()));
        assert_eq!(hook.healthcheck.unwrap().test, Some(vec!["NONE".into()]));
        let hook_host = hook.host_config.unwrap();
        assert_eq!(hook_host.log_config, Some(container_log_config()));
        assert_eq!(hook_host.dns, Some(vec![gateway.0.to_string()]));
        assert_eq!(hook_host.dns_search, Some(vec!["shop.internal".into()]));
        assert_eq!(hook_host.dns_options, Some(vec!["ndots:1".into()]));
        assert_eq!(hook_host.privileged, Some(true));
        assert_eq!(
            hook_host.extra_hosts,
            Some(vec!["api:192.0.2.10".into(), "gateway:host-gateway".into()])
        );
        assert!(hook_host.port_bindings.is_none());
        assert_eq!(
            hook_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::NO)
        );
        let hook_labels = hook.labels.as_ref().unwrap();
        assert_eq!(
            hook_labels.get(LABEL_NAMESPACE).map(String::as_str),
            Some("shop")
        );
        assert_eq!(
            hook_labels.get(LABEL_SERVICE_NAME).map(String::as_str),
            Some("api")
        );
        assert_eq!(
            hook_labels.get(LABEL_HOOK).map(String::as_str),
            Some(LABEL_HOOK_PRE_DEPLOY)
        );
        assert_eq!(
            hook_labels.get("example.value").map(String::as_str),
            Some("unchanged")
        );
    }

    #[test]
    fn create_body_maps_docker_restart_pid_and_stop_timeout() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "pull_policy": "missing",
                "pid_mode": "container:abc",
                "stop_timeout_secs": 1,
                "restart": { "name": "on-failure", "maximum_retry_count": 3 }
            }
        }))
        .unwrap();
        let body = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &ployz_core::Namespace::parse("app").unwrap(),
            &spec,
            None,
        )
        .unwrap();
        assert_eq!(body.stop_timeout, Some(1));
        let host = body.host_config.unwrap();
        let restart = host.restart_policy.unwrap();
        assert_eq!(
            restart.name,
            Some(bollard::models::RestartPolicyNameEnum::ON_FAILURE)
        );
        assert_eq!(restart.maximum_retry_count, Some(3));
        assert_eq!(host.pid_mode.as_deref(), Some("container:abc"));
    }

    #[test]
    fn only_the_system_ingress_gets_its_sysctls_at_create() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let ingress = ployz_core::QualifiedService::system_ingress();
        let sysctls = |namespace: &ployz_core::Namespace, name: &str, kernel_supports| {
            let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
                "service_id": "a".repeat(32),
                "name": name,
                "mode": { "mode": "global" },
                "container": { "image": "caddy:2.11.4", "pull_policy": "missing" }
            }))
            .unwrap();
            let mut body = create::container_create_body(
                &machine_id,
                gateway,
                ContainerKind::ServiceContainer,
                namespace,
                &spec,
                None,
            )
            .unwrap();
            create::ingress_sysctls(&mut body, namespace, &spec, kernel_supports);
            let host = body.host_config.unwrap();
            assert_eq!(host.log_config, Some(container_log_config()));
            host.sysctls
        };
        let ingress_name = ingress.name.to_string();
        assert_eq!(
            sysctls(&ingress.namespace, &ingress_name, true),
            Some(HashMap::from([
                ("net.ipv4.tcp_migrate_req".into(), "1".into()),
                ("net.ipv4.tcp_retries2".into(), "5".into()),
            ]))
        );
        assert_eq!(
            sysctls(&ingress.namespace, &ingress_name, false),
            Some(HashMap::from([(
                "net.ipv4.tcp_retries2".into(),
                "5".into()
            )]))
        );
        assert_eq!(
            sysctls(
                &ployz_core::Namespace::parse("app").unwrap(),
                &ingress_name,
                true
            ),
            None
        );
    }

    #[test]
    fn only_the_system_ingress_takes_ipv6_on_the_bridge() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let ingress = ployz_core::QualifiedService::system_ingress();
        let caddy = ployz_core::caddy_service_spec("caddy:test".into(), Default::default())
            .to_resolved(
                ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
                ployz_core::ResolvedUpdateConfig::default(),
            )
            .unwrap();
        let endpoint = |namespace: &ployz_core::Namespace| {
            let body = create::container_create_body(
                &machine_id,
                gateway,
                ContainerKind::ServiceContainer,
                namespace,
                &caddy,
                None,
            )
            .unwrap();
            assert!(
                takes_bridge_ipv6(body.labels.as_ref().unwrap())
                    == (*namespace == ingress.namespace)
            );
            body.networking_config
                .and_then(|config| config.endpoints_config)
                .and_then(|endpoints| endpoints.get(crate::network::DOCKER_NETWORK_NAME).cloned())
        };

        assert_eq!(endpoint(&ingress.namespace), None);
        assert_eq!(
            endpoint(&ployz_core::Namespace::parse("app").unwrap())
                .and_then(|endpoint| endpoint.driver_opts),
            Some(HashMap::from([(
                "com.docker.network.endpoint.sysctls".into(),
                "net.ipv6.conf.IFNAME.disable_ipv6=1".into(),
            )]))
        );
    }

    #[test]
    fn run_service_container_does_not_restart_after_exit_and_keeps_bind_mounts() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "oneshot",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "command": ["cat", "/host-os"],
                "pull_policy": "missing",
                "restart": { "name": "no" }
            },
            "volumes": [{
                "reference": "host",
                "source": { "kind": "bind", "machine_path": "/etc/os-release" }
            }],
            "mounts": [{ "volume": "host", "target": "/x/..//host-os/./" }]
        }))
        .unwrap();

        let body = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &ployz_core::Namespace::parse("app").unwrap(),
            &spec,
            None,
        )
        .unwrap();
        let host = body.host_config.unwrap();
        assert_eq!(
            host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::NO)
        );
        let mounts = host.mounts.expect("bind mount must still be applied");
        let [bind_mount] = mounts.as_slice() else {
            panic!("expected one bind mount: {mounts:?}")
        };
        assert_eq!(bind_mount.typ, Some(bollard::models::MountType::BIND));
        assert_eq!(bind_mount.source.as_deref(), Some("/etc/os-release"));
        assert_eq!(bind_mount.target.as_deref(), Some("/host-os"));
    }

    #[test]
    fn resolved_mounts_map_bind_named_alias_and_tmpfs_without_inventing_defaults() {
        use bollard::models::MountType;
        use ployz_core::ResolvedServiceSpec;

        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "11111111111111111111111111111111",
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [
                {"reference":"host","source":{"kind":"bind","machine_path":"/srv/api"}},
                {"reference":"alias","source":{"kind":"ordinary","name":"app_database","scope":{"namespace":"app","logical_name":"database"},"driver":{"name":"local","options":{"type":"none"}},"labels":{"purpose":"db"}}},
                {"reference":"memory","source":{"kind":"tmpfs","size_bytes":4096,"mode":448}}
            ],
            "mounts": [
                {"volume":"host","target":"/host"},
                {"volume":"alias","target":"/data","read_only":true,"no_copy":true,"subpath":"current"},
                {"volume":"alias","target":"/archive","subpath":"archive"},
                {"volume":"memory","target":"/run/cache"}
            ]
        }))
        .unwrap();

        let mounts = docker_mounts(spec.volume_graph()).unwrap();
        let [bind_mount, named_mount, archive_mount, tmpfs_mount] = mounts.as_slice() else {
            panic!("expected four mounts: {mounts:?}")
        };
        assert_eq!(bind_mount.typ, Some(MountType::BIND));
        assert_eq!(bind_mount.source.as_deref(), Some("/srv/api"));
        let bind = bind_mount.bind_options.as_ref().unwrap();
        assert!(bind.propagation.is_none());
        assert!(bind.non_recursive.is_none());
        assert!(bind.read_only_non_recursive.is_none());
        assert!(bind.read_only_force_recursive.is_none());

        for (recursive, expected) in [
            (
                ployz_core::BindRecursive::Disabled,
                (Some(true), None, None),
            ),
            (
                ployz_core::BindRecursive::Writable,
                (None, Some(true), None),
            ),
            (
                ployz_core::BindRecursive::Readonly,
                (None, None, Some(true)),
            ),
        ] {
            let mut recursive_spec = spec.clone();
            let mut volumes = recursive_spec.volume_graph().volumes().to_vec();
            let mounts = recursive_spec.volume_graph().mounts().to_vec();
            let mut raw = volumes.first().unwrap().source.kind().clone();
            let ployz_core::RawVolumeSource::Bind {
                recursive: setting, ..
            } = &mut raw
            else {
                panic!("expected bind volume")
            };
            *setting = Some(recursive);
            volumes.first_mut().unwrap().source = raw.admit().unwrap();
            recursive_spec
                .set_volume_graph(
                    ployz_core::ServiceVolumeGraph::parse(volumes, mounts)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
            let translated = docker_mounts(recursive_spec.volume_graph())
                .unwrap()
                .remove(0)
                .bind_options
                .unwrap();
            assert_eq!(
                (
                    translated.non_recursive,
                    translated.read_only_non_recursive,
                    translated.read_only_force_recursive,
                ),
                expected
            );
        }

        for (propagation, expected) in [
            (
                ployz_core::BindPropagation::Private,
                Some(bollard::models::MountBindOptionsPropagationEnum::PRIVATE),
            ),
            (
                ployz_core::BindPropagation::Rprivate,
                Some(bollard::models::MountBindOptionsPropagationEnum::RPRIVATE),
            ),
            (
                ployz_core::BindPropagation::Shared,
                Some(bollard::models::MountBindOptionsPropagationEnum::SHARED),
            ),
            (
                ployz_core::BindPropagation::Rshared,
                Some(bollard::models::MountBindOptionsPropagationEnum::RSHARED),
            ),
            (
                ployz_core::BindPropagation::Slave,
                Some(bollard::models::MountBindOptionsPropagationEnum::SLAVE),
            ),
            (
                ployz_core::BindPropagation::Rslave,
                Some(bollard::models::MountBindOptionsPropagationEnum::RSLAVE),
            ),
        ] {
            let mut propagation_spec = spec.clone();
            let mut volumes = propagation_spec.volume_graph().volumes().to_vec();
            let mounts = propagation_spec.volume_graph().mounts().to_vec();
            let mut raw = volumes.first().unwrap().source.kind().clone();
            let ployz_core::RawVolumeSource::Bind {
                propagation: setting,
                ..
            } = &mut raw
            else {
                panic!("expected bind volume")
            };
            *setting = Some(propagation);
            volumes.first_mut().unwrap().source = raw.admit().unwrap();
            propagation_spec
                .set_volume_graph(
                    ployz_core::ServiceVolumeGraph::parse(volumes, mounts)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
            let translated = docker_mounts(propagation_spec.volume_graph())
                .unwrap()
                .remove(0)
                .bind_options
                .unwrap();
            assert_eq!(translated.propagation, expected);
        }
        assert_eq!(named_mount.typ, Some(MountType::VOLUME));
        assert_eq!(named_mount.source.as_deref(), Some("app_database"));
        assert_eq!(named_mount.read_only, Some(true));
        let named_options = named_mount.volume_options.as_ref().unwrap();
        assert_eq!(named_options.no_copy, Some(true));
        assert_eq!(named_options.subpath.as_deref(), Some("current"));
        assert_eq!(
            named_options
                .driver_config
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("local")
        );
        assert_eq!(archive_mount.source.as_deref(), Some("app_database"));
        let archive_options = archive_mount.volume_options.as_ref().unwrap();
        assert_eq!(archive_options.no_copy, Some(false));
        assert_eq!(archive_options.subpath.as_deref(), Some("archive"));
        assert_eq!(tmpfs_mount.typ, Some(MountType::TMPFS));
        assert!(tmpfs_mount.source.is_none());
        assert_eq!(
            tmpfs_mount.tmpfs_options.as_ref().unwrap().size_bytes,
            Some(4096)
        );

        let mut dangling = serde_json::to_value(&spec).unwrap();
        *dangling
            .get_mut("mounts")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|mounts| mounts.first_mut())
            .and_then(|mount| mount.get_mut("volume"))
            .expect("fixture has a mount volume") = serde_json::json!("missing");
        assert!(
            serde_json::from_value::<ResolvedServiceSpec>(dangling)
                .unwrap_err()
                .to_string()
                .contains("undeclared Service Volume")
        );

        let missing_docker_volume: ResolvedServiceSpec =
            serde_json::from_value(serde_json::json!({
                "service_id": "11111111111111111111111111111111",
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
                "volumes": [{"reference":"data","source":{"kind":"ordinary","name":"app_missing","scope":{"namespace":"app","logical_name":"missing"},"driver":{"name":"local","options":{}}}}],
                "mounts": [{"volume":"data","target":"/data"}]
            }))
            .unwrap();
        let mounts = docker_mounts(missing_docker_volume.volume_graph()).unwrap();
        let [named] = mounts.as_slice() else {
            panic!("valid Service Volume graph still maps a named Docker Volume")
        };
        assert_eq!(named.typ, Some(MountType::VOLUME));
        assert_eq!(named.source.as_deref(), Some("app_missing"));

        let external: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "11111111111111111111111111111111",
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [{"reference":"data","source":{
                "kind":"external",
                "name":"external-data"
            }}],
            "mounts": [{
                "volume":"data",
                "target":"/data",
                "no_copy":true,
                "subpath":"current"
            }]
        }))
        .unwrap();
        let external = docker_mounts(external.volume_graph()).unwrap().remove(0);
        assert_eq!(external.source.as_deref(), Some("external-data"));
        assert_eq!(
            external.volume_options,
            Some(bollard::models::MountVolumeOptions {
                no_copy: Some(true),
                subpath: Some("current".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn container_resources_map_to_docker_without_dropping_limits() {
        let resources = serde_json::from_value(serde_json::json!({
            "cpu_nanos": 500_000_000,
            "memory_bytes": 104_857_600,
            "memory_reservation_bytes": 52_428_800,
            "shared_memory_bytes": 26_214_400,
            "devices": [{
                "machine_path": "/dev/fuse",
                "container_path": "/dev/fuse",
                "cgroup_permissions": "rwm"
            }],
            "device_reservations": [{
                "driver": "nvidia",
                "count": -1,
                "capabilities": [["gpu"]],
                "options": {"virtualization": "false"}
            }],
            "ulimits": {"nofile": {"soft": 20_000, "hard": 40_000}}
        }))
        .unwrap();

        let mapped = docker_resources(&resources);
        assert_eq!(mapped.nano_cpus, Some(500_000_000));
        assert_eq!(mapped.memory, Some(104_857_600));
        assert_eq!(mapped.memory_reservation, Some(52_428_800));
        assert_eq!(mapped.shm_size, Some(26_214_400));
        let [device] = mapped.devices.as_deref().unwrap() else {
            panic!("expected one Docker device: {mapped:?}")
        };
        assert_eq!(device.path_on_host.as_deref(), Some("/dev/fuse"));
        assert_eq!(device.path_in_container.as_deref(), Some("/dev/fuse"));
        assert_eq!(device.cgroup_permissions.as_deref(), Some("rwm"));
        let [request] = mapped.device_requests.as_deref().unwrap() else {
            panic!("expected one Docker device request: {mapped:?}")
        };
        assert_eq!(request.driver.as_deref(), Some("nvidia"));
        assert_eq!(request.count, Some(-1));
        assert_eq!(request.capabilities, Some(vec![vec!["gpu".into()]]));
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| options.get("virtualization"))
                .map(String::as_str),
            Some("false")
        );
        let [ulimit] = mapped.ulimits.as_deref().unwrap() else {
            panic!("expected one Docker ulimit: {mapped:?}")
        };
        assert_eq!(ulimit.name.as_deref(), Some("nofile"));
        assert_eq!((ulimit.soft, ulimit.hard), (Some(20_000), Some(40_000)));
    }

    #[test]
    fn host_ports_and_healthchecks_map_without_losing_bind_or_timing_details() {
        let ports: Vec<ployz_core::PortPublication> = serde_json::from_value(serde_json::json!([
            {
                "mode":"host",
                "bind":{"kind":"all"},
                "published_port":8080,
                "container_port":80,
                "transport_protocol":"tcp"
            },
            {
                "mode":"host",
                "bind":{"kind":"prefix","prefix":"192.0.2.0/24"},
                "published_port":5353,
                "container_port":53,
                "transport_protocol":"udp"
            },
            {
                "mode":"ingress",
                "hostname":"api.example.com",
                "load_balancer_port":443,
                "container_port":8443,
                "http_protocol":"https"
            }
        ]))
        .unwrap();
        let (exposed, bindings) = docker_ports(
            &ports,
            &[
                "192.0.2.4".parse().unwrap(),
                "198.51.100.8".parse().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(exposed.unwrap(), ["53/udp", "80/tcp"]);
        let bindings = bindings.unwrap();
        let tcp = bindings
            .get("80/tcp")
            .and_then(Option::as_ref)
            .and_then(|bindings| bindings.first())
            .unwrap();
        let udp = bindings
            .get("53/udp")
            .and_then(Option::as_ref)
            .and_then(|bindings| bindings.first())
            .unwrap();
        assert_eq!(tcp.host_ip, None);
        assert_eq!(udp.host_ip.as_deref(), Some("192.0.2.4"));
        assert_eq!(udp.host_port.as_deref(), Some("5353"));

        let healthcheck = serde_json::from_value(serde_json::json!({
            "state":"configured",
            "test":["CMD","true"],
            "interval_millis":1000,
            "timeout_millis":2000,
            "start_period_millis":3000,
            "start_interval_millis":4000,
            "retries":5
        }))
        .unwrap();
        let mapped = docker_healthcheck(&healthcheck).unwrap();
        assert_eq!(mapped.test.unwrap(), ["CMD", "true"]);
        assert_eq!(mapped.interval, Some(1_000_000_000));
        assert_eq!(mapped.timeout, Some(2_000_000_000));
        assert_eq!(mapped.start_period, Some(3_000_000_000));
        assert_eq!(mapped.start_interval, Some(4_000_000_000));
        assert_eq!(mapped.retries, Some(5));

        let disabled = docker_healthcheck(&ployz_core::HealthcheckSpec::Disabled).unwrap();
        assert_eq!(disabled.test.unwrap(), ["NONE"]);
        assert_eq!(disabled.interval, None);
        assert_eq!(disabled.timeout, None);
        assert_eq!(disabled.retries, None);
    }

    #[test]
    fn managed_labels_distinguish_service_and_hook_containers() {
        assert!(matches!(
            ManagedLabels::parse(&HashMap::new()),
            Err(Error::NotManaged)
        ));
        let mut labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_NAMESPACE.to_owned(), "app".to_owned()),
            (LABEL_SERVICE_ID.to_owned(), "a".repeat(32)),
            (LABEL_SERVICE_NAME.to_owned(), "api".to_owned()),
        ]);
        let parsed = ManagedLabels::parse(&labels).unwrap();
        assert_eq!(parsed.kind, ContainerKind::ServiceContainer);
        assert_eq!(parsed.identity.namespace.as_str(), "app");
        assert_eq!(parsed.identity.name.as_str(), "api");
        labels.insert(LABEL_HOOK.to_owned(), LABEL_HOOK_PRE_DEPLOY.to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
        labels.insert(LABEL_HOOK.to_owned(), "future-hook".to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
    }

    #[test]
    fn managed_labels_reject_missing_namespace() {
        let labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_SERVICE_ID.to_owned(), "a".repeat(32)),
            (LABEL_SERVICE_NAME.to_owned(), "api".to_owned()),
        ]);
        assert!(matches!(
            ManagedLabels::parse(&labels),
            Err(Error::MissingLabel(LABEL_NAMESPACE))
        ));
    }

    #[test]
    fn managed_labels_reject_invalid_namespace() {
        let labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_NAMESPACE.to_owned(), "Not_DNS".to_owned()),
            (LABEL_SERVICE_ID.to_owned(), "a".repeat(32)),
            (LABEL_SERVICE_NAME.to_owned(), "api".to_owned()),
        ]);
        assert!(matches!(
            ManagedLabels::parse(&labels),
            Err(Error::InvalidValue {
                field: LABEL_NAMESPACE,
                ..
            })
        ));
    }

    #[test]
    fn runtime_and_health_keep_unknown_values_explicit() {
        assert_eq!(
            runtime_from_parts("created", None, None),
            ContainerRuntimeObservation::Created
        );
        assert_eq!(
            runtime_from_parts("running", None, None),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured
            }
        );
        assert_eq!(
            runtime_from_parts("running", None, Some("degraded")),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("degraded".into())
            }
        );
        for (health, expected) in [
            ("starting", HealthObservation::Starting),
            ("healthy", HealthObservation::Healthy),
            ("unhealthy", HealthObservation::Unhealthy),
        ] {
            assert_eq!(
                runtime_from_parts("running", None, Some(health)),
                ContainerRuntimeObservation::Running { health: expected }
            );
        }
        assert_eq!(
            runtime_from_parts("paused", None, Some("healthy")),
            ContainerRuntimeObservation::Paused
        );
        assert_eq!(
            runtime_from_parts("restarting", None, Some("healthy")),
            ContainerRuntimeObservation::Restarting
        );
        assert_eq!(
            runtime_from_parts("exited", Some(17), None),
            ContainerRuntimeObservation::Exited { code: 17 }
        );
        assert_eq!(
            runtime_from_parts("removing", None, None),
            ContainerRuntimeObservation::Removing
        );
        assert_eq!(
            runtime_from_parts("dead", None, None),
            ContainerRuntimeObservation::Dead
        );
        assert!(matches!(
            runtime_from_parts("stopping", None, None),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Status") == Some(&json!("stopping"))
        ));
        assert!(matches!(
            runtime_from_parts("future", None, Some("future-health")),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Health") == Some(&json!("future-health"))
        ));
    }

    #[test]
    fn the_last_docker_check_is_the_newest_log_entry() {
        let state = |health: serde_json::Value| json!({ "Status": "running", "ExitCode": 0, "Health": health });
        let entry = |exit_code: i64, output: &str| {
            json!({
                "Start": "2026-10-05T10:00:00Z",
                "End": "2026-10-05T10:00:01Z",
                "ExitCode": exit_code,
                "Output": output,
            })
        };
        assert_eq!(
            last_docker_check(
                Some(&state(json!({
                    "Status": "unhealthy",
                    "FailingStreak": 2,
                    "Log": [entry(0, "accepting connections\n"), entry(127, "sh: pg_isready: not found\nmore")],
                }))),
                || crate::sealed_env::Lines::Shown
            ),
            Some(LastHealthCheck::Exited {
                code: 127,
                output: "sh: pg_isready: not found".into(),
            })
        );
        for health in [
            json!({ "Status": "starting", "Log": [] }),
            json!({ "Status": "starting", "Log": null }),
            json!({ "Status": "starting" }),
        ] {
            assert_eq!(
                last_docker_check(Some(&state(health)), || crate::sealed_env::Lines::Shown),
                None
            );
        }
        assert_eq!(
            last_docker_check(Some(&json!({ "Status": "running" })), || {
                crate::sealed_env::Lines::Shown
            }),
            None
        );
        assert_eq!(
            last_docker_check(
                Some(&state(json!({ "Log": [{ "Output": "no exit code" }] }))),
                || crate::sealed_env::Lines::Shown
            ),
            None
        );
        assert_eq!(
            last_docker_check(None, || crate::sealed_env::Lines::Shown),
            None
        );
        assert_eq!(
            last_docker_check(
                Some(&json!({
                    "Status": "running",
                    "StartedAt": "2026-10-05T11:00:00Z",
                    "Health": { "Status": "starting", "Log": [entry(1, "from the previous start")] },
                })),
                || crate::sealed_env::Lines::Shown
            ),
            None
        );
    }

    #[test]
    fn raw_docker_inspect_keeps_future_state_and_effective_healthcheck() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
            "Config": {
                "Healthcheck": {
                    "Test": ["CMD", "true"],
                    "Interval": 1_500_000,
                    "Timeout": 2_000_000,
                    "Retries": 4,
                    "StartPeriod": 300_000_001,
                    "StartInterval": 4_000_000
                }
            },
            "State": {
                "Status": "future-state",
                "ExitCode": 7,
                "Health": { "Status": "future-health" }
            }
        }))
        .unwrap();
        assert!(matches!(
            runtime_observation(inspected.state.as_ref()),
            ContainerRuntimeObservation::Unknown { raw }
                if raw == json!({
                    "Status": "future-state",
                    "ExitCode": 7,
                    "Health": { "Status": "future-health" }
                })
        ));
        assert_eq!(
            effective_healthcheck(inspected.config.as_ref()),
            Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
                test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
                interval_millis: Some(2),
                timeout_millis: Some(2),
                start_period_millis: Some(301),
                start_interval_millis: Some(4),
                retries: Some(4),
                deadline_millis: None,
            }))
        );

        let disabled: RawContainerInspect = serde_json::from_value(json!({
            "Config": { "Healthcheck": { "Test": ["NONE"], "Interval": 1_000_000 } }
        }))
        .unwrap();
        assert_eq!(
            effective_healthcheck(disabled.config.as_ref()),
            Some(HealthcheckSpec::Disabled)
        );

        let inherited: RawContainerInspect = serde_json::from_value(json!({
            "Config": { "Healthcheck": { "Test": [] } }
        }))
        .unwrap();
        assert_eq!(effective_healthcheck(inherited.config.as_ref()), None);
    }

    #[test]
    fn each_docker_start_is_its_own_run() {
        let run = |restart_count: i64, started_at: &str| {
            let inspected: RawContainerInspect = serde_json::from_value(json!({
                "RestartCount": restart_count,
                "State": { "Status": "running", "StartedAt": started_at },
            }))
            .unwrap();
            container_run(&inspected)
        };
        let first = run(0, "2026-10-05T11:00:00.1Z");
        assert!(first.is_some());
        assert_ne!(first, run(1, "2026-10-05T11:00:00.1Z"));
        assert_ne!(first, run(0, "2026-10-05T11:00:00.2Z"));
        assert_eq!(first, run(0, "2026-10-05T11:00:00.1Z"));
        // Docker reports a container that never started with the zero time.
        assert_eq!(run(0, "0001-01-01T00:00:00Z"), None);
    }

    fn inspect_json() -> serde_json::Value {
        json!({
            "Name": "/api-1",
            "Created": "2024-03-15T12:34:56.123456789Z",
            "Config": {
                "Labels": { "ployz.managed": "1" },
                "Healthcheck": {
                    "Test": ["CMD", "true"],
                    "Interval": 1_500_000,
                    "Timeout": 2_000_000,
                    "Retries": 4,
                    "StartPeriod": 300_000_001,
                    "StartInterval": 4_000_000
                }
            },
            "State": {
                "Status": "running",
                "ExitCode": 0,
                "Health": { "Status": "healthy" }
            },
            "NetworkSettings": {
                "Networks": {
                    "ployz": { "IPAddress": "10.210.0.5" }
                }
            }
        })
    }

    fn projected(
        inspected: &RawContainerInspect,
    ) -> (
        String,
        i64,
        ContainerRuntimeObservation,
        Option<HealthcheckSpec>,
        Option<ContainerAddress>,
    ) {
        (
            display_name(inspected.name.as_deref()),
            created_at_unix_nanos(inspected.created.as_deref()),
            runtime_observation(inspected.state.as_ref()),
            effective_healthcheck(inspected.config.as_ref()),
            container_address(inspected),
        )
    }

    fn expected_typed_projection() -> (
        String,
        i64,
        ContainerRuntimeObservation,
        Option<HealthcheckSpec>,
        Option<ContainerAddress>,
    ) {
        (
            "api-1".into(),
            1_710_506_096_123_456_789,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
                test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
                interval_millis: Some(2),
                timeout_millis: Some(2),
                start_period_millis: Some(301),
                start_interval_millis: Some(4),
                retries: Some(4),
                deadline_millis: None,
            })),
            Some(ContainerAddress(Ipv4Addr::new(10, 210, 0, 5))),
        )
    }

    #[test]
    fn typed_and_raw_inspect_namespace_equivalent_observations() {
        let value = inspect_json();
        let typed: ContainerInspectResponse = serde_json::from_value(value.clone()).unwrap();
        let from_typed = projected(&RawContainerInspect::from_typed(&typed).unwrap());
        let from_raw = projected(&serde_json::from_value(value).unwrap());
        assert_eq!(from_typed, from_raw);
        assert_eq!(from_raw, expected_typed_projection());
    }

    #[test]
    fn raw_inspect_reads_docker_ip_address_spelling() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
            "NetworkSettings": {
                "Networks": {
                    "ployz": { "IPAddress": "10.210.0.5" }
                }
            }
        }))
        .unwrap();
        assert_eq!(
            container_address(&inspected),
            Some(ContainerAddress(Ipv4Addr::new(10, 210, 0, 5)))
        );
    }

    #[test]
    fn raw_inspect_keeps_unknown_health_on_a_running_container() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
            "State": {
                "Status": "running",
                "Health": { "Status": "future-health" }
            }
        }))
        .unwrap();
        assert_eq!(
            runtime_observation(inspected.state.as_ref()),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("future-health".into())
            }
        );
    }

    #[test]
    fn unknown_unrelated_docker_fields_use_raw_fallback() {
        let mut value = inspect_json();
        value
            .as_object_mut()
            .expect("inspect fixture is an object")
            .insert(
                "HostConfig".into(),
                json!({ "Isolation": "future-isolation" }),
            );
        assert!(serde_json::from_value::<ContainerInspectResponse>(value.clone()).is_err());
        let inspected = decode_raw_inspect(
            Err(bollard::errors::Error::JsonDataError {
                message: "unknown variant `future-isolation`".into(),
                contents: value.to_string(),
                column: 1,
            }),
            &ContainerId::parse("a".repeat(64)).unwrap(),
        )
        .unwrap();
        assert_eq!(projected(&inspected), expected_typed_projection());
    }
}

#[cfg(test)]
#[test]
fn inspection_environment_reads_docker_values_and_rejects_malformed_entries() {
    let config: RawContainerConfig = serde_json::from_value(serde_json::json!({
        "Env": ["TOKEN=private-live-value", "WITH_EQUALS=a=b"]
    }))
    .unwrap();
    let environment = inspected_environment(&config).unwrap();
    assert_eq!(environment.get("TOKEN").unwrap(), "private-live-value");
    assert_eq!(environment.get("WITH_EQUALS").unwrap(), "a=b");
    let malformed: RawContainerConfig =
        serde_json::from_value(serde_json::json!({"Env": ["INVALID"]})).unwrap();
    assert!(inspected_environment(&malformed).is_none());
}

#[cfg(test)]
#[tokio::test]
async fn inspection_reports_failing_once_the_check_passed_since_this_start() {
    use test_support::{FakeDocker, container_request, fake_runtime_with, machine};

    let containers = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let (runtime, _fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(containers.clone()),
        ..Default::default()
    })
    .await;
    let machine = machine();
    let namespace = Namespace::parse("app").unwrap();
    let spec = test_support::spec_with_sources(Vec::new());
    let created = runtime
        .create_with_admission::<_, _, Error>(
            &machine,
            container_request(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                std::future::ready(None),
            ),
        )
        .await
        .unwrap();
    let observe = async |started_at: &str, health: &str| {
        for container in containers.lock().unwrap().values_mut() {
            let container = container.as_object_mut().unwrap();
            container
                .get_mut("Config")
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .insert("Healthcheck".into(), json!({ "Test": ["CMD", "true"] }));
            container.insert(
                "State".into(),
                json!({
                    "Status": "running",
                    "StartedAt": started_at,
                    "Health": { "Status": health },
                }),
            );
        }
        let observed = runtime
            .inspect_managed(&created.container_id, &machine.id)
            .await
            .unwrap();
        let ContainerRuntimeObservation::Running { health } = &observed.runtime else {
            panic!("expected a running container, got {}", observed.runtime);
        };
        health.clone()
    };
    let first_start = chrono::Utc::now().to_rfc3339();
    let second_start = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();

    assert_eq!(
        observe(&first_start, "unhealthy").await,
        HealthObservation::Unhealthy
    );
    assert_eq!(
        observe(&first_start, "healthy").await,
        HealthObservation::Healthy
    );
    assert_eq!(
        observe(&first_start, "unhealthy").await,
        HealthObservation::Failing
    );
    assert_eq!(
        observe(&second_start, "unhealthy").await,
        HealthObservation::Unhealthy
    );
}

#[cfg(test)]
mod sealed_output_tests {
    use ployz_core::{
        ContainerDetails, EnvValue, LastHealthCheck, LogBody, LogEntry, LogsOptions, Machine,
        RuntimeWatchFrame, SealedScope, encode_runtime_watch_frame,
    };

    use super::test_support::{FakeDocker, container_request, fake_runtime_with, machine};
    use super::*;
    use crate::sealed_env::{OpenError, test_support::Cluster};
    use crate::sealing_key::SealingKey;

    const OPENED: &str = "opened-t0ken";

    type Containers = Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>;

    async fn created_with_token(
        cluster: &Cluster,
    ) -> (ContainerRuntime, Machine, ContainerId, Containers) {
        let containers = Containers::default();
        let (runtime, machine, created) = create_with_env(
            cluster,
            LogStore::Unsupervised,
            fake_docker(&containers),
            sealed_token(cluster),
        )
        .await;
        (runtime, machine, created.unwrap().container_id, containers)
    }

    fn fake_docker(containers: &Containers) -> FakeDocker {
        FakeDocker {
            named_containers: Some(containers.clone()),
            ..Default::default()
        }
    }

    fn sealed_token(cluster: &Cluster) -> EnvValue {
        EnvValue::Sealed(cluster.seal(
            "TOKEN",
            SealedScope::Service(cluster.identity.service),
            OPENED,
        ))
    }

    async fn create_with_env(
        cluster: &Cluster,
        log_store: LogStore,
        docker: FakeDocker,
        token: EnvValue,
    ) -> (ContainerRuntime, Machine, Result<ContainerCreated, Error>) {
        let (runtime, _fake) = fake_runtime_with(docker).await;
        let runtime = runtime
            .with_sealing_key(cluster.dir.0.clone())
            .with_log_store(log_store);
        let machine = machine();
        let namespace = Namespace::parse("app").unwrap();
        let mut spec = test_support::spec_with_sources(Vec::new());
        spec.container.sealed_identity = Some(cluster.identity.clone());
        spec.container.environment.insert("TOKEN".into(), token);
        let created = runtime
            .create_with_admission::<_, _, Error>(
                &machine,
                container_request(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    &spec,
                    std::future::ready(None),
                ),
            )
            .await;
        (runtime, machine, created)
    }

    #[tokio::test]
    async fn no_sealed_value_opens_while_the_log_store_runs_older_software() {
        let cluster = Cluster::new();
        let containers = Containers::default();

        let (_runtime, _machine, created) = create_with_env(
            &cluster,
            LogStore::Upgrading,
            fake_docker(&containers),
            sealed_token(&cluster),
        )
        .await;

        assert!(
            matches!(created, Err(Error::LogStoreUpgrading(_))),
            "{created:?}"
        );
        let refused = ployz_core::RpcError::from(&created.unwrap_err());
        assert_eq!(refused.code, RpcErrorCode::Unavailable);
        assert!(containers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sealed_value_creates_no_container_without_a_sealing_key() {
        let cluster = Cluster::new();
        let token = sealed_token(&cluster);
        SealingKey::set(&cluster.dir.0, None).unwrap();
        let containers = Containers::default();

        let (_runtime, _machine, created) = create_with_env(
            &cluster,
            LogStore::Unsupervised,
            fake_docker(&containers),
            token,
        )
        .await;

        assert!(
            matches!(created, Err(Error::Sealed(OpenError::KeyMissing))),
            "{created:?}"
        );
        assert!(containers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_multiline_value_with_a_short_line_creates_no_container() {
        let cluster = Cluster::new();
        let containers = Containers::default();
        let token = EnvValue::Sealed(cluster.seal(
            "TOKEN",
            SealedScope::Service(cluster.identity.service),
            "line-one-QX7MARKER\nZq9",
        ));

        let (_runtime, _machine, created) = create_with_env(
            &cluster,
            LogStore::Unsupervised,
            fake_docker(&containers),
            token,
        )
        .await;

        assert!(
            matches!(&created, Err(Error::Sealed(OpenError::ShortLine(key))) if key == "TOKEN"),
            "{created:?}"
        );
        let refused = ployz_core::RpcError::from(&created.unwrap_err());
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        assert!(!format!("{refused:?}").contains("QX7MARKER"), "{refused:?}");
        assert!(containers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_value_sealed_for_another_cluster_creates_no_container() {
        let cluster = Cluster::new();
        let containers = Containers::default();

        let (_runtime, _machine, created) = create_with_env(
            &cluster,
            LogStore::Unsupervised,
            fake_docker(&containers),
            sealed_token(&Cluster::new()),
        )
        .await;

        assert!(
            matches!(&created, Err(Error::Sealed(OpenError::Unopenable(key))) if key == "TOKEN"),
            "{created:?}"
        );
        assert!(containers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tailed_logs_never_show_an_opened_value() {
        let cluster = Cluster::new();
        let docker = fake_docker(&Containers::default());
        let logs = docker.logs.clone();
        let (runtime, machine, created) = create_with_env(
            &cluster,
            LogStore::Unsupervised,
            docker,
            sealed_token(&cluster),
        )
        .await;
        let frame = |stream: u8, line: &str| {
            let line = format!("2026-10-23T00:00:00.000000000Z {line}\n");
            let mut frame = vec![stream, 0, 0, 0];
            frame.extend(u32::try_from(line.len()).unwrap().to_be_bytes());
            frame.extend(line.into_bytes());
            frame
        };
        *logs.lock().unwrap() = [
            frame(1, &format!("token={OPENED}")),
            frame(2, &format!("rejected {OPENED} twice: {OPENED}")),
        ]
        .concat();

        let lines: Vec<_> = runtime
            .container_logs(
                &machine.id,
                &machine.name,
                created.unwrap().container_id,
                &LogsOptions {
                    follow: false,
                    tail: -1,
                    since_nanos: None,
                    until_unix_seconds: None,
                },
            )
            .await
            .unwrap()
            .map(|entry| LogEntry::decode(&entry.unwrap()).unwrap().body)
            .collect()
            .await;

        assert_eq!(
            lines,
            [
                LogBody::Stdout(b"token=[secret TOKEN]\n".to_vec()),
                LogBody::Stderr(b"rejected [secret TOKEN] twice: [secret TOKEN]\n".to_vec()),
            ]
        );
    }

    #[tokio::test]
    async fn text_values_create_while_the_log_store_runs_older_software() {
        let cluster = Cluster::new();
        let containers = Containers::default();

        let (_runtime, _machine, created) = create_with_env(
            &cluster,
            LogStore::Upgrading,
            fake_docker(&containers),
            EnvValue::Text(OPENED.into()),
        )
        .await;

        assert!(created.is_ok(), "{created:?}");
        assert_eq!(containers.lock().unwrap().len(), 1);
    }

    async fn printed_by_check(
        cluster: &Cluster,
        key_dir: PathBuf,
        mut state: serde_json::Value,
    ) -> ContainerDetails {
        let (runtime, machine, id, containers) = created_with_token(cluster).await;
        state.as_object_mut().unwrap().insert(
            "Health".into(),
            json!({
                "Status": "unhealthy",
                "Log": [{ "ExitCode": 1, "Output": format!("connection rejected: {OPENED}\n") }],
            }),
        );
        for container in containers.lock().unwrap().values_mut() {
            let container = container.as_object_mut().unwrap();
            container
                .get_mut("Config")
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .insert("Healthcheck".into(), json!({ "Test": ["CMD", "check"] }));
            container.insert("State".into(), state.clone());
        }
        let runtime = runtime.with_sealing_key(key_dir);
        runtime
            .inspect_managed_details(&id, &machine.id)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_spec_store_keeps_only_ciphertext() {
        let cluster = Cluster::new();
        let (runtime, _machine, _id, containers) = created_with_token(&cluster).await;

        let docker = serde_json::to_string(&*containers.lock().unwrap()).unwrap();
        assert!(docker.contains(&format!("TOKEN={OPENED}")), "{docker}");
        let store = runtime.spec_store_path_for_test();
        let name = store.file_name().unwrap().to_str().unwrap();
        let holds =
            |bytes: &[u8], text: &str| bytes.windows(text.len()).any(|w| w == text.as_bytes());
        let mut held_the_spec = false;
        for entry in std::fs::read_dir(store.parent().unwrap()).unwrap() {
            let path = entry.unwrap().path();
            if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(name)
            {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            held_the_spec |= holds(&bytes, "\"TOKEN\":{\"sealed\"");
            assert!(!holds(&bytes, OPENED), "{path:?} holds the opened value");
        }
        assert!(held_the_spec);
    }

    #[tokio::test]
    async fn inspect_scrubs_opened_values_from_the_last_check_output() {
        let cluster = Cluster::new();
        let details = printed_by_check(
            &cluster,
            cluster.dir.0.clone(),
            json!({ "Status": "running" }),
        )
        .await;

        assert_eq!(
            details.last_check,
            Some(LastHealthCheck::Exited {
                code: 1,
                output: "connection rejected: [secret TOKEN]".into(),
            })
        );
    }

    #[tokio::test]
    async fn inspect_withholds_check_output_when_the_sealing_key_is_gone() {
        let cluster = Cluster::new();
        let empty = crate::test_dir::TestDir::new("sealed-output-no-key");
        std::fs::create_dir_all(&empty.0).unwrap();
        let details =
            printed_by_check(&cluster, empty.0.clone(), json!({ "Status": "running" })).await;

        assert_eq!(
            details.last_check,
            Some(LastHealthCheck::Exited {
                code: 1,
                output: String::new(),
            })
        );
    }

    #[tokio::test]
    async fn an_unknown_docker_state_publishes_no_check_output() {
        let cluster = Cluster::new();
        let details = printed_by_check(
            &cluster,
            cluster.dir.0.clone(),
            json!({ "Status": "future-status" }),
        )
        .await;
        let published = observe::redacted_container(&details.container);
        let frame = encode_runtime_watch_frame(&RuntimeWatchFrame {
            machines: Vec::new(),
            containers: vec![published.clone()],
            volumes: Vec::new(),
            certificates: Vec::new(),
            incomplete_ids: Default::default(),
            observed_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();

        assert!(matches!(
            details.container.runtime,
            ContainerRuntimeObservation::Unknown { .. }
        ));
        for shown in [
            serde_json::to_string(&details.container).unwrap(),
            details.container.runtime.to_string(),
            serde_json::to_string(&published).unwrap(),
            frame
                .decode_json::<serde_json::Value>()
                .unwrap()
                .to_string(),
        ] {
            assert!(!shown.contains(OPENED), "{shown}");
            assert!(shown.contains("future-status"), "{shown}");
        }
    }
}
