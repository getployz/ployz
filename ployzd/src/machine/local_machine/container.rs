//! Machine-local container admission and creation.

use std::path::Path;

use ployz_core::{
    ContainerCreated, ContainerKind, MachineStorageObservation, ProjectName, ResolvedServiceSpec,
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
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record()?;
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
        let _guard = self.global_slot_lock.lock().await;
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record()?;
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
