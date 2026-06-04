use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::{
    ManagedContainerObservation, NodeContainerObservationSnapshot,
    NodeContainerObservationSnapshotError,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InMemoryObservationStore {
    snapshots: BTreeMap<NodeId, NodeContainerObservationSnapshot>,
}

impl InMemoryObservationStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put_container(
        &mut self,
        observation: ManagedContainerObservation,
    ) -> Result<(), NodeContainerObservationSnapshotError> {
        let node_id = observation.node_id.clone();
        let snapshot = if let Some(snapshot) = self.snapshots.get(&node_id) {
            snapshot.with_container_replaced(observation)?
        } else {
            NodeContainerObservationSnapshot::try_new(node_id, [observation])?
        };
        self.replace_node_containers(snapshot);
        Ok(())
    }

    pub fn replace_node_containers(&mut self, snapshot: NodeContainerObservationSnapshot) {
        self.snapshots.insert(snapshot.node_id().clone(), snapshot);
    }

    #[must_use]
    pub fn container(
        &self,
        node_id: &NodeId,
        container_id: &ContainerId,
    ) -> Option<&ManagedContainerObservation> {
        self.snapshots.get(node_id)?.container(container_id)
    }
}
