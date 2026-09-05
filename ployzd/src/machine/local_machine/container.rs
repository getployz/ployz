//! Machine-local container admission and creation.

use std::path::Path;

use ployz_core::{
    ContainerCreated, ContainerKind, LocalMachinePhase, MachineStorageObservation, ProjectName,
    ResolvedServiceSpec,
};

use super::{Error, LocalMachine};
use crate::docker::{ContainerRequest, GlobalSlotConvergence, GlobalSlotRequest};
use crate::machine::{STORAGE_OBSERVATION_TIMEOUT, local_storage};

impl LocalMachine {
    /// Return fresh local storage evidence for container admission and Global reconciliation.
    pub(crate) async fn observe_storage(&self) -> Option<MachineStorageObservation> {
        local_storage(Path::new("zpool"), STORAGE_OBSERVATION_TIMEOUT).await
    }

    /// Create a container after storage admission and deferred Machine-local runtime preparation.
    ///
    /// # Errors
    ///
    /// Returns when local state, networking, storage, or Docker cannot safely create the
    /// requested container.
    pub(crate) async fn create_container(
        &self,
        kind: ContainerKind,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        // Once admitted, caller cancellation must not let reset overtake a Docker request.
        let admission = self.lock_store()?.admission_lock.clone();
        let guard = admission.lock_owned().await;
        let (local, project, spec) = (self.clone(), project.clone(), spec.clone());
        tokio::spawn(async move {
            let _guard = guard;
            local.create_container_admitted(kind, &project, &spec).await
        })
        .await?
    }

    async fn create_container_admitted(
        &self,
        kind: ContainerKind,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record()?;
        if !matches!(
            record.phase(),
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        containers
            .create_with_network(
                machine,
                ContainerRequest {
                    kind,
                    project_name: project,
                    spec,
                    network: self.prepare_service_runtime(kind, project, spec),
                    storage: self.observe_storage(),
                },
            )
            .await
    }

    /// Converge this Machine's Global slot from one fresh target-side eligibility decision.
    ///
    /// # Errors
    ///
    /// Returns when local state is unavailable, Docker is unavailable, this Machine
    /// is not participating, or Docker cannot converge the slot.
    pub(crate) async fn converge_global_slot(
        &self,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<GlobalSlotConvergence, Error> {
        let admission = self.lock_store()?.admission_lock.clone();
        let guard = admission.lock_owned().await;
        let (local, project, spec) = (self.clone(), project.clone(), spec.clone());
        tokio::spawn(async move {
            let _guard = guard;
            local.converge_global_slot_admitted(&project, &spec).await
        })
        .await?
    }

    async fn converge_global_slot_admitted(
        &self,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<GlobalSlotConvergence, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record()?;
        if record.phase() != LocalMachinePhase::Participating {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        containers
            .converge_global_slot(
                machine,
                GlobalSlotRequest {
                    project_name: project,
                    spec,
                    network: self.prepare_service_runtime(
                        ContainerKind::ServiceContainer,
                        project,
                        spec,
                    ),
                    storage: self.observe_storage(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ployz_core::{
        MachineId, ProjectName, ResolvedServiceSpec, ServiceId, ServiceMode, ServiceName,
    };
    use serde_json::json;

    use crate::machine::{LocalMachine, LocalMachineError, LocalMachineStore};

    #[tokio::test]
    async fn cancelled_create_finishes_before_reset_cleanup() {
        for before_create in [false, true] {
            use crate::docker::test_support::{FakeDocker, fake_runtime_with};
            use ployz_core::{AdvertisedEndpoint, ContainerKind, LocalMachinePhase, MachineName};
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-admission-{}", MachineId::random()));
            let mut store = LocalMachineStore::open(&data_dir).unwrap();
            store
                .initialize(
                    MachineName::parse("local").unwrap(),
                    crate::machine::FoundingCluster {
                        network: "10.210.0.0/16".parse().unwrap(),
                        ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
                    },
                    None,
                    vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                    None,
                    None,
                )
                .unwrap();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                create_barrier: (!before_create).then(|| barrier.clone()),
                image_barrier: before_create.then(|| barrier.clone()),
                ..Default::default()
            })
            .await;
            let (restart, _) = tokio::sync::watch::channel(false);
            let (participating, participation) = tokio::sync::watch::channel(true);
            let store = Arc::new(Mutex::new(store));
            let local = LocalMachine::new(store.clone(), restart.clone())
                .with_containers(Some(runtime.clone()))
                .with_participation(participating.clone());
            let resetting = LocalMachine::new(store, restart)
                .with_containers(Some(runtime))
                .with_participation(participating);
            let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Replicated { replicas: 1.try_into().unwrap() }).unwrap(),
            "container":{"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
            let project = ProjectName::parse("app").unwrap();
            let creating = tokio::spawn({
                let local = local.clone();
                let spec = spec.clone();
                let project = project.clone();
                async move {
                    local
                        .create_container(ContainerKind::ServiceContainer, &project, &spec)
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
            assert!(!*participation.borrow());
            assert!(fake.existing_container.lock().unwrap().is_none());
            assert_eq!(
                local.record().unwrap().phase(),
                LocalMachinePhase::Resetting
            );
            assert!(matches!(
                local
                    .create_container(ContainerKind::ServiceContainer, &project, &spec)
                    .await,
                Err(LocalMachineError::NotParticipating)
            ));
            assert!(matches!(
                local.converge_global_slot(&project, &spec).await,
                Err(LocalMachineError::NotParticipating)
            ));
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    #[tokio::test]
    async fn joining_admits_direct_create_but_not_global_convergence() {
        use crate::docker::test_support::{FakeDocker, fake_runtime_with};
        use ployz_core::{AdvertisedEndpoint, ContainerKind, Machine, MachineName};
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-joining-admission-{}", MachineId::random()));
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (runtime, _) = fake_runtime_with(FakeDocker {
            create_barrier: Some(barrier.clone()),
            ..Default::default()
        })
        .await;
        let local = LocalMachine::new(store.clone(), tokio::sync::watch::channel(false).0)
            .with_containers(Some(runtime));
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container": {"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
        let project = ProjectName::parse("app").unwrap();
        assert!(matches!(
            local
                .create_container(ContainerKind::ServiceContainer, &project, &spec)
                .await,
            Err(LocalMachineError::NotParticipating)
        ));
        {
            let mut store = store.lock().unwrap();
            let machine = Machine {
                id: store.record().id(),
                name: MachineName::parse("joining").unwrap(),
                subnet: "10.210.0.0/24".parse().unwrap(),
                public_key: store.record().private_key().public_key(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                runtime: Default::default(),
            };
            let mut peer = machine.clone();
            peer.id = MachineId::random();
            store
                .join(machine, vec![peer], Default::default(), None, None)
                .unwrap();
        }
        assert!(matches!(
            local.converge_global_slot(&project, &spec).await,
            Err(LocalMachineError::NotParticipating)
        ));
        let release = tokio::spawn(async move {
            barrier.wait().await;
            barrier.wait().await;
        });
        local
            .create_container(ContainerKind::ServiceContainer, &project, &spec)
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
            AdvertisedEndpoint, ContainerKind, MachineName, RemoveLocalMachineRequest,
        };
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-removal-admission-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .initialize(
                MachineName::parse("local").unwrap(),
                crate::machine::FoundingCluster {
                    network: "10.210.0.0/16".parse().unwrap(),
                    ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
                },
                None,
                vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                None,
                None,
            )
            .unwrap();
        let (replicated, server) = fake_cluster::store().await;
        server.abort();
        let _ = server.await;
        let (runtime, _) = fake_runtime_with(FakeDocker::default()).await;
        let (restart, restarting) = tokio::sync::watch::channel(false);
        let (participating, participation) = tokio::sync::watch::channel(true);
        let local = LocalMachine::new(Arc::new(Mutex::new(store)), restart)
            .with_containers(Some(runtime))
            .with_participation(participating)
            .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))));
        let removed = local
            .remove_local(RemoveLocalMachineRequest {
                restart_on_cleanup_failure: false,
            })
            .await
            .unwrap();
        assert!(removed.reset_warning.is_some());
        assert!(!*restarting.borrow());
        assert!(!*participation.borrow());
        assert!(
            local.record().unwrap().machine().is_some(),
            "historical Machine remains available for recovery"
        );
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(), "name":"api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container":{"image":"example.test/api", "pull_policy":"missing"}
        })).unwrap();
        let project = ProjectName::parse("app").unwrap();
        assert!(matches!(
            local
                .create_container(ContainerKind::ServiceContainer, &project, &spec)
                .await,
            Err(LocalMachineError::NotParticipating)
        ));
        assert!(matches!(
            local.converge_global_slot(&project, &spec).await,
            Err(LocalMachineError::NotParticipating)
        ));
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn converge_global_slot_reports_missing_docker_at_local_machine_seam() {
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-local-global-slot-{}", MachineId::random()));
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
        let (restart, _) = tokio::sync::watch::channel(false);
        let local = LocalMachine::new(store, restart);
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": ServiceId::random(),
            "name": ServiceName::parse("api").unwrap(),
            "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
            "container": { "image": "example.test/api", "pull_policy": "missing" }
        }))
        .unwrap();

        let error = local
            .converge_global_slot(&ProjectName::parse("app").unwrap(), &spec)
            .await
            .unwrap_err();

        assert!(matches!(error, LocalMachineError::DockerUnavailable));
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}
