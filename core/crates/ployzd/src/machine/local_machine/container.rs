//! Machine-local container admission and creation.

use std::sync::Arc;

use ployz_core::{
    ContainerChanged, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CopyRefusal, DockerVolumeName, Drain, ImageIngestOpened,
    ImageIngestReason, ImagePulled, ImagesRemoved, LocalMachinePhase, MachineStorageObservation,
    Namespace, PruneImagesRequest, PullImageFromMachineRequest, ResolvedServiceSpec, StopOptions,
    VolumeRemoved,
};

use super::super::ingress::admit_ingress_service;
use super::{Error, LocalMachine};
use crate::docker::{ContainerRequest, ImageIngest};
use crate::machine::{STORAGE_OBSERVATION_TIMEOUT, local_storage};

impl LocalMachine {
    /// Return fresh local storage evidence for container admission and Global reconciliation.
    pub(crate) async fn observe_storage(&self) -> Option<MachineStorageObservation> {
        local_storage(&self.zpool, STORAGE_OBSERVATION_TIMEOUT).await
    }

    /// Recheck the complete local placement and secure all provisioned Volumes before applications start.
    pub(crate) async fn prepare_volumes(
        &self,
        specs: Vec<ployz_core::ServiceStorageSpec>,
    ) -> Result<ployz_core::PreparedVolumes, Error> {
        use ployz_core::RawVolumeSource;
        let local = self.clone();
        self.finish_mutation(async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let record = local.record();
            if !matches!(
                record.phase(),
                LocalMachinePhase::Joining | LocalMachinePhase::Participating
            ) {
                return Err(Error::NotParticipating);
            }
            let machine = record.machine().ok_or(Error::NotParticipating)?;
            let storage = local.observe_storage().await;
            let mut requested = std::collections::BTreeMap::new();
            for spec in &specs {
                crate::docker::require_eligible(
                    spec.placement_eligibility(machine, storage.as_ref()),
                )?;
                for volume in spec.volume_graph().mounted_provisioned_volumes() {
                    let RawVolumeSource::Provisioned {
                        name,
                        maximum_bytes,
                        ..
                    } = volume.source.kind()
                    else {
                        unreachable!("provisioned iterator")
                    };
                    if let Some(existing) = requested.insert(name.clone(), *maximum_bytes)
                        && existing != *maximum_bytes
                    {
                        return Err(ployz_core::StorageCapacityError::VolumeSizeConflict {
                            name: name.clone(),
                        }
                        .into_rpc_error()
                        .into());
                    }
                }
            }
            containers
                .validate_provisioned_volumes(&machine.id, &specs)
                .await?;
            for name in requested.keys() {
                local.admit_plain_mount(&machine.id, name).await?;
            }
            let names: Vec<ployz_core::DockerVolumeName> =
                local.plugin().call("Storage.Prepare", &requested).await?;
            containers
                .ensure_provisioned_volumes(&machine.id, &specs)
                .await?;
            Ok(ployz_core::PreparedVolumes { names })
        })
        .await
    }

    /// A plain Deploy mounts `name` only on its writer: this Machine's writable root, or
    /// nothing at all while no other Machine holds a copy.
    async fn admit_plain_mount(
        &self,
        me: &ployz_core::MachineId,
        name: &DockerVolumeName,
    ) -> Result<(), Error> {
        use ployz_core::{CopyRole, DockerVolumeStorageObservation, KnownCopy, SwitchError};
        let held: ployz_core::VolumeCopyView = self
            .plugin()
            .call(
                "Volume.Inspect",
                &ployz_core::InspectVolumeCopyRequest { name: name.clone() },
            )
            .await?;
        let replicated = self.replicated()?;
        let machines = replicated.machines().await?.observations;
        let machine_name =
            |id: &ployz_core::MachineId| match machines.iter().find(|machine| machine.id == *id) {
                Some(machine) => Some(machine.name.clone()),
                None => ployz_core::MachineName::parse(id.as_str()).ok(),
            };
        let elsewhere: Vec<KnownCopy> = replicated
            .volumes()
            .await?
            .observations
            .into_iter()
            .filter(|volume| volume.id.machine_id != *me && volume.id.name == *name)
            .filter_map(|volume| {
                let DockerVolumeStorageObservation::Provisioned { role, .. } = volume.storage
                else {
                    return None;
                };
                Some(KnownCopy {
                    machine: machine_name(&volume.id.machine_id)?,
                    role: role.unwrap_or(CopyRole::Writer),
                })
            })
            .collect();
        match ployz_core::admit_plain_mount(held.copy.as_ref(), &elsewhere) {
            Ok(()) => Ok(()),
            Err(SwitchError::NoWriter) => {
                // This Server's own slot is a copy Restore can name, beside any elsewhere.
                let mut copies = elsewhere;
                if let Some(ployz_core::VolumeCopy::Slot { .. }) = held.copy
                    && let Some(machine) = machine_name(me)
                {
                    copies.push(KnownCopy {
                        machine,
                        role: CopyRole::Slot,
                    });
                }
                Err(SwitchError::NoWriter
                    .rpc_error(ployz_core::no_writer_message(
                        ployz_core::Namespace::declared_volume_name(name).unwrap_or(name.as_str()),
                        &copies,
                    ))
                    .into())
            }
            Err(reason) => Err(reason
                .rpc_error(format!(
                    "Volume {name} is mid-run on this Machine; wait for the run to finish"
                ))
                .into()),
        }
    }

    /// Create a container after storage admission and deferred Machine-local validation.
    ///
    /// # Errors
    ///
    /// Returns when local state, networking, storage, or Docker cannot safely create the
    /// requested container.
    pub(crate) async fn create_container(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        registry_auth: Option<ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        // Once admitted, caller cancellation must not let reset overtake a Docker request.
        let (local, namespace, spec) = (self.clone(), namespace.clone(), spec.clone());
        self.finish_mutation(async move {
            local
                .create_container_admitted(
                    kind,
                    &namespace,
                    &spec,
                    creation_key,
                    deployment_id,
                    registry_auth,
                )
                .await
        })
        .await
    }

    /// Start one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot start the
    /// container.
    pub(crate) async fn start_container(
        &self,
        container_id: ContainerId,
    ) -> Result<ContainerChanged, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .start(&container_id)
                .await?;
            Ok(ContainerChanged { container_id })
        })
        .await
    }

    /// Stop one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot stop the
    /// container.
    pub(crate) async fn stop_container(
        &self,
        container_id: ContainerId,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> Result<ContainerChanged, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .stop(&container_id, signal.as_deref(), grace_period_seconds)
                .await?;
            Ok(ContainerChanged { container_id })
        })
        .await
    }

    /// Remove one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot remove the
    /// container.
    pub(crate) async fn remove_container(
        &self,
        container_id: ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<ContainerChanged, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .remove(&container_id, remove_volumes, force)
                .await?;
            Ok(ContainerChanged { container_id })
        })
        .await
    }

    /// Take one running managed Container out of ingress, wait `drain`, stop it.
    ///
    /// Contract: the caller holds `installation` and no admission turn. The
    /// helper takes the exclusive turn for the Docker stop itself, so a caller
    /// already holding a turn deadlocks.
    ///
    /// The stopping mark is set without a turn so replication tells every proxy
    /// while the drain sleeps. The stop clears the mark whether it succeeds or
    /// fails, so a Container that failed to stop is routed again. A daemon
    /// restart forgets the mark; a still-running Container returns to ingress
    /// and the resend redoes the drain.
    ///
    /// Polls no proxy and reads no other Machine.
    pub(crate) async fn stop_behind_ingress(
        &self,
        installation: &crate::mutation::MutationGuard,
        container: &ContainerId,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<ContainerObservation, Error> {
        let _ = installation;
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let observed = containers.inspect_managed(container, &machine_id).await?;
        if !is_running(&observed) {
            return Ok(observed);
        }
        containers.mark_stopping(*container);
        tokio::time::sleep(drain.as_duration()).await;
        let turn = self.owner.admission_lock().write_owned().await;
        let stopped = containers
            .stop(
                container,
                stop.signal.as_deref(),
                stop.grace_period_seconds.map(i32::from),
            )
            .await;
        drop(turn);
        stopped?;
        Ok(containers.inspect_managed(container, &machine_id).await?)
    }

    /// The cut's Container half: drain and stop every holder of the Volume, then run
    /// `zfs` under the exclusive turn once no holder runs. The holders are listed again
    /// under the turn, so one started during the drain is `Busy` too, as is one the
    /// stop left running. A `Busy` cut leaves the root a writer.
    ///
    /// Detached from the RPC with the installation held, so a caller that hangs up
    /// does not abandon a half-stopped Container.
    pub(crate) async fn cut_volume_copy<Z, Fut>(
        &self,
        name: DockerVolumeName,
        drain: Drain,
        zfs: Z,
    ) -> Result<Result<(), CopyRefusal>, Error>
    where
        Z: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
    {
        let local = self.clone();
        self.finish_installed(move |installation| async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let stop = StopOptions::default();
            for holder in containers.volume_holder_ids(&name).await? {
                let observed = local
                    .stop_behind_ingress(&installation, &holder, drain, &stop)
                    .await?;
                if is_running(&observed) {
                    return Ok(Err(CopyRefusal::Busy));
                }
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            if local.volume_holder_running(&name).await? {
                return Ok(Err(CopyRefusal::Busy));
            }
            zfs().await.map(Ok)
        })
        .await
    }

    /// Whether Docker runs any Container that mounts the Volume `name`. A holder gone
    /// between the list and its inspect does not run.
    pub(crate) async fn volume_holder_running(
        &self,
        name: &DockerVolumeName,
    ) -> Result<bool, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        for holder in containers.volume_holder_ids(name).await? {
            match containers.inspect_managed(&holder, &machine_id).await {
                Ok(observed) if is_running(&observed) => return Ok(true),
                Ok(_) | Err(crate::docker::Error::ContainerNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    /// Remove one local Docker Volume while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot remove the
    /// Volume.
    pub(crate) async fn remove_volume(
        &self,
        name: DockerVolumeName,
        force: bool,
    ) -> Result<VolumeRemoved, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .remove_volume(&name, force)
                .await?;
            Ok(VolumeRemoved {})
        })
        .await
    }

    /// Remove each repository's superseded build images while holding Machine mutation
    /// admission, so no Container is created from a reference as it is removed.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot list
    /// images or Containers.
    pub(crate) async fn prune_images(
        &self,
        request: PruneImagesRequest,
    ) -> Result<ImagesRemoved, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            Ok(local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .prune_images(&request.repositories)
                .await?)
        })
        .await
    }

    /// Start or reconcile image ingest without taking a turn in Machine admission:
    /// a Build Grant mint answers while a Deploy or image transfer runs. The ingest
    /// serializes its own reconciliation, and daemon shutdown removes its helper, so a
    /// Reset racing it leaves at most a disposable helper behind.
    ///
    /// # Errors
    ///
    /// Returns when this Machine is not participating, an installation or upgrade is active,
    /// or the ingest helper cannot start.
    pub(crate) async fn ensure_image_ingest(
        &self,
        ingest: Arc<ImageIngest>,
    ) -> Result<ImageIngestOpened, Error> {
        let _installation = self.admit_installation()?;
        let record = self.record();
        let address = record
            .machine()
            .filter(|_| record.phase() == LocalMachinePhase::Participating)
            .map(|machine| machine.management_address())
            .ok_or_else(|| {
                Error::StoragePreparation(
                    ImageIngestReason::NotParticipating.rpc_error("Machine is not participating"),
                )
            })?;
        ingest
            .open(address)
            .await
            .map_err(Error::StoragePreparation)
    }

    /// Pull one image from peer ingest under shared admission: transfers run side by
    /// side, and exclusive work, such as Reset or image removal, waits for them.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the image transfer fails.
    pub(crate) async fn pull_image_from_machine(
        &self,
        request: PullImageFromMachineRequest,
    ) -> Result<ImagePulled, Error> {
        let local = self.clone();
        let shared = self.owner.admission_lock().read_owned().await;
        self.finish_admitted(shared, async move {
            local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            crate::docker::pull_from_ingest(&request.pull, request.source, &request.platform)
                .await?;
            Ok(ImagePulled {})
        })
        .await
    }

    async fn create_container_admitted(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        registry_auth: Option<ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record();
        if !matches!(
            record.phase(),
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        containers
            .create_with_admission(
                machine,
                ContainerRequest {
                    creation_key: creation_key.as_deref(),
                    deployment_id: deployment_id.as_ref(),
                    registry_auth: registry_auth.as_ref(),
                    kind,
                    namespace,
                    spec,
                    admission: async { admit_ingress_service(namespace, spec) },
                    storage: self.observe_storage(),
                },
            )
            .await
    }
}

/// Whether Docker still runs the Container's process. Paused and restarting count as
/// running: both can write the Volume again without a start.
fn is_running(observed: &ContainerObservation) -> bool {
    matches!(
        observed.runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ployz_core::{MachineId, Namespace, ResolvedServiceSpec, ServiceId, ServiceMode};
    use serde_json::json;

    use crate::machine::{LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner};
    use crate::storage::test_support::FakePlugin;

    #[tokio::test]
    async fn fresh_service_revocation_preserves_management_and_trusted_ingress() {
        use crate::docker::test_support::{FakeDocker, fake_runtime_with};
        use ployz_core::{AdvertisedEndpoint, ContainerKind, MachineName};
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-role-admission-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
            })
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: Some(Arc::new(tokio::sync::Barrier::new(1))),
            ..Default::default()
        })
        .await;
        let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime.clone()));
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container": {"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
        let namespace = Namespace::parse("app").unwrap();
        let existing = local
            .create_container(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let update = serde_json::from_value(json!({
            "accepts_services": false, "accepts_ingress": true, "accepts_builds": true
        }))
        .unwrap();
        owner
            .mutate(move |store| store.update(update, &[]))
            .await
            .unwrap()
            .unwrap();
        for kind in [
            ContainerKind::ServiceContainer,
            ContainerKind::PreDeployHook,
        ] {
            let error = local
                .create_container(kind, &namespace, &spec, None, None, None)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("accept"), "{error}");
        }
        let error = local
            .prepare_volumes(vec![ployz_core::ServiceStorageSpec::from(&spec)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("accept"), "{error}");
        let labels = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.ends_with("/containers/create"))
            .unwrap()
            .1
            .get("Labels")
            .unwrap()
            .clone();
        fake.existing_container.lock().unwrap().as_mut().unwrap()["Config"] =
            json!({"Labels": labels});
        runtime.start(&existing.container_id).await.unwrap();
        runtime
            .stop(&existing.container_id, None, None)
            .await
            .unwrap();
        runtime
            .remove(&existing.container_id, false, false)
            .await
            .unwrap();
        assert!(fake.existing_container.lock().unwrap().is_none());
        let ingress = ployz_core::caddy_service_spec("caddy:test".into(), Default::default())
            .to_resolved(ServiceId::random(), Default::default())
            .unwrap();
        let error = local
            .create_container(
                ContainerKind::ServiceContainer,
                &namespace,
                &ingress,
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("accept"), "{error}");
        local
            .create_container(
                ContainerKind::ServiceContainer,
                &Namespace::system(),
                &ingress,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn cancelled_create_finishes_before_reset_cleanup() {
        for before_create in [false, true] {
            use crate::docker::test_support::{FakeDocker, fake_runtime_with};
            use ployz_core::{AdvertisedEndpoint, ContainerKind, LocalMachinePhase, MachineName};
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-admission-{}", MachineId::random()));
            let mut store = LocalMachineStore::open(&data_dir).unwrap();
            store
                .enroll(ployz_core::EnrollRequest {
                    role: ployz_core::EnrollAs::Founder {
                        initial_policy: Default::default(),
                        name: MachineName::parse("local").unwrap(),
                        cluster_network: "10.210.0.0/16".parse().unwrap(),
                        public_ip: None,
                        advertised_endpoints: vec![AdvertisedEndpoint(
                            "192.0.2.1:51820".parse().unwrap(),
                        )],
                    },
                    wireguard_mtu: None,
                })
                .unwrap();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                create_barrier: (!before_create).then(|| barrier.clone()),
                image_barrier: before_create.then(|| barrier.clone()),
                ..Default::default()
            })
            .await;
            let owner = RecordOwner::spawn(store).unwrap();
            let plugin = FakePlugin::default();
            plugin.reply("Storage.Demote", json!({"Ok": []}));
            let (plugin, _plugin_server) = plugin.serve(&data_dir);
            let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime.clone()));
            let resetting = LocalMachine::new(owner)
                .with_containers(Some(runtime))
                .with_plugin(plugin);
            let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Replicated { replicas: 1.try_into().unwrap() }).unwrap(),
            "container":{"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
            let namespace = Namespace::parse("app").unwrap();
            let creating = tokio::spawn({
                let local = local.clone();
                let spec = spec.clone();
                let namespace = namespace.clone();
                async move {
                    local
                        .create_container(
                            ContainerKind::ServiceContainer,
                            &namespace,
                            &spec,
                            None,
                            None,
                            None,
                        )
                        .await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), barrier.wait())
                .await
                .unwrap();
            creating.abort();
            let _ = creating.await;
            let mut reset = Box::pin(resetting.reset());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut reset)
                    .await
                    .is_err(),
                "reset must wait for the admitted create even after its caller is cancelled"
            );
            barrier.wait().await;
            tokio::time::timeout(std::time::Duration::from_secs(5), reset)
                .await
                .unwrap()
                .unwrap();
            assert!(fake.existing_container.lock().unwrap().is_none());
            assert_eq!(local.record().phase(), LocalMachinePhase::Resetting);
            assert!(matches!(
                local
                    .create_container(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        None,
                        None,
                        None,
                    )
                    .await,
                Err(LocalMachineError::NotParticipating)
            ));
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    #[tokio::test]
    async fn joining_admits_direct_create() {
        use crate::docker::test_support::{FakeDocker, fake_runtime_with};
        use ployz_core::{AdvertisedEndpoint, ContainerKind, Machine, MachineName};
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-joining-admission-{}", MachineId::random()));
        let owner = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (runtime, _) = fake_runtime_with(FakeDocker {
            create_barrier: Some(barrier.clone()),
            ..Default::default()
        })
        .await;
        let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime));
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container": {"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
        let namespace = Namespace::parse("app").unwrap();
        assert!(matches!(
            local
                .create_container(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    &spec,
                    None,
                    None,
                    None,
                )
                .await,
            Err(LocalMachineError::NotParticipating)
        ));
        let record = owner.record();
        let machine = Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: record.id(),
            name: MachineName::parse("joining").unwrap(),
            subnet: "10.210.0.0/24".parse().unwrap(),
            public_key: record.private_key().public_key(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            runtime: Default::default(),
            build_concurrency: None,
        };
        let mut peer = machine.clone();
        peer.id = MachineId::random();
        owner
            .mutate(move |store| {
                store.enroll(ployz_core::EnrollRequest {
                    role: ployz_core::EnrollAs::Member {
                        registration: ployz_core::Registered {
                            assigned_machine: machine,
                            visible_peers: vec![peer],
                            target_versions: Default::default(),
                        },
                    },
                    wireguard_mtu: None,
                })
            })
            .await
            .unwrap()
            .unwrap();
        let release = tokio::spawn(async move {
            barrier.wait().await;
            barrier.wait().await;
        });
        local
            .create_container(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        release.await.unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn removal_warning_keeps_recovery_available_without_container_admission() {
        use crate::{
            corrosion::{AdminClient, fake_cluster},
            docker::test_support::{FakeDocker, fake_runtime_with},
        };
        use ployz_core::{
            AdvertisedEndpoint, ContainerKind, LocalMachinePhase, MachineName,
            RemoveLocalMachineRequest,
        };
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-removal-admission-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
            })
            .unwrap();
        let (replicated, server) = fake_cluster::store().await;
        server.abort();
        let _ = server.await;
        let (runtime, _) = fake_runtime_with(FakeDocker::default()).await;
        let owner = RecordOwner::spawn(store).unwrap();
        let restarting = owner.restart_requested();
        let plugin = FakePlugin::default();
        plugin.reply("Storage.Demote", json!({"Ok": []}));
        let (plugin, _plugin_server) = plugin.serve(&data_dir);
        let local = LocalMachine::new(owner)
            .with_containers(Some(runtime))
            .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))))
            .with_plugin(plugin);
        let removed = local
            .remove_local(RemoveLocalMachineRequest {
                restart_on_cleanup_failure: false,
            })
            .await
            .unwrap();
        assert!(removed.reset_warning.is_some());
        assert!(!*restarting.borrow());
        assert_eq!(local.record().phase(), LocalMachinePhase::Resetting);
        assert!(
            local.record().machine().is_some(),
            "historical Machine remains available for recovery"
        );
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name":"api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container":{"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
        let namespace = Namespace::parse("app").unwrap();
        assert!(matches!(
            local
                .create_container(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    &spec,
                    None,
                    None,
                    None,
                )
                .await,
            Err(LocalMachineError::NotParticipating)
        ));
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}
