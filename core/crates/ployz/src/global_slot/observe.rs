//! What every Server runs, with each Server's name, and when that answer is whole enough
//! to act on.

use ployz_core::{LiveServices, MachineId, MachineName, MachineObservation, RpcError};

/// Each Server's name, by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ServerNames(Vec<(MachineId, MachineName)>);

impl ServerNames {
    pub(crate) fn of(machines: &[MachineObservation]) -> Self {
        Self(machines.iter().map(named).collect())
    }

    #[cfg(test)]
    pub(crate) fn listing(names: impl IntoIterator<Item = (MachineId, MachineName)>) -> Self {
        Self(names.into_iter().collect())
    }

    /// The Unarrived Machines among `machines`, which hold no copy to wait on.
    pub(crate) fn unarrived(machines: &[MachineObservation]) -> Self {
        Self(
            machines
                .iter()
                .filter(|machine| machine.unarrived())
                .map(named)
                .collect(),
        )
    }

    /// `id`'s Server name, or `id` itself when the membership doesn't list it.
    pub(crate) fn name(&self, id: &MachineId) -> String {
        self.0
            .iter()
            .find(|(named, _)| named == id)
            .map_or_else(|| id.to_string(), |(_, name)| name.to_string())
    }

    pub(crate) fn without(&mut self, id: &MachineId) {
        self.0.retain(|(named, _)| named != id);
    }

    pub(crate) fn contains(&self, id: &MachineId) -> bool {
        self.0.iter().any(|(named, _)| named == id)
    }
}

impl IntoIterator for ServerNames {
    type Item = (MachineId, MachineName);
    type IntoIter = std::vec::IntoIter<(MachineId, MachineName)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

fn named(machine: &MachineObservation) -> (MachineId, MachineName) {
    (machine.machine.id, machine.machine.name.clone())
}

/// What every Server runs, with each Server's name.
pub(crate) struct Live {
    pub(crate) services: LiveServices<RpcError>,
    pub(crate) names: ServerNames,
}

impl Live {
    /// What each Server that did not answer said, or None when every target did. Whoever
    /// acts on this observation refuses when it is Some, each with its own sentence around
    /// it.
    pub(crate) fn partial(&self) -> Option<String> {
        (!self.services.containers.all_targets_succeeded()).then(|| {
            crate::failure::partial_failure_details(&self.services.containers, |id| {
                self.names.name(id)
            })
        })
    }
}
