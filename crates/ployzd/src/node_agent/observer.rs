use std::collections::BTreeMap;

use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::ManagedContainerObservation;
use ployz_core::state::ContainerObservationKey;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InMemoryObservationStore {
    containers: BTreeMap<ContainerObservationKey, ManagedContainerObservation>,
}

impl InMemoryObservationStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put_container(&mut self, observation: ManagedContainerObservation) {
        let key =
            ContainerObservationKey::from_ids(&observation.node_id, &observation.container_id);
        self.containers.insert(key, observation);
    }

    pub fn replace_node_containers(
        &mut self,
        node_id: &NodeId,
        observations: impl IntoIterator<Item = ManagedContainerObservation>,
    ) -> Result<(), ObservationStoreError> {
        let observations: Vec<_> = observations.into_iter().collect();
        if let Some(observation) = observations
            .iter()
            .find(|observation| observation.node_id != *node_id)
        {
            return Err(ObservationStoreError::SnapshotNodeMismatch {
                expected: node_id.clone(),
                actual: observation.node_id.clone(),
                container_id: observation.container_id.clone(),
            });
        }

        self.containers
            .retain(|_key, observation| observation.node_id != *node_id);

        for observation in observations {
            self.put_container(observation);
        }

        Ok(())
    }

    #[must_use]
    pub fn container(
        &self,
        node_id: &NodeId,
        container_id: &ContainerId,
    ) -> Option<&ManagedContainerObservation> {
        let key = ContainerObservationKey::from_ids(node_id, container_id);
        self.containers.get(&key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationStoreError {
    SnapshotNodeMismatch {
        expected: NodeId,
        actual: NodeId,
        container_id: ContainerId,
    },
}
