use ployz_types::model::{MachineMembership, MachineRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaPreference {
    Default,
    Five,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaPolicy {
    pub storage_candidates: usize,
    pub replicas: usize,
}

#[must_use]
pub fn replica_policy(
    machines: &[MachineMembership],
    preference: ReplicaPreference,
) -> ReplicaPolicy {
    let storage_candidates = machines
        .iter()
        .filter(|machine| machine.role == MachineRole::StorageCandidate)
        .count();
    ReplicaPolicy {
        storage_candidates,
        replicas: desired_replicas(storage_candidates, preference),
    }
}

#[must_use]
pub fn desired_replicas(storage_candidates: usize, preference: ReplicaPreference) -> usize {
    match (storage_candidates, preference) {
        (0..=2, _) => 1,
        (3..=4, _) => 3,
        (_, ReplicaPreference::Default) => 3,
        (_, ReplicaPreference::Five) => 5,
    }
}

#[must_use]
pub fn demotion_requires_degradation_acceptance(storage_candidates: usize) -> bool {
    storage_candidates == 3
}

#[must_use]
pub fn removal_preserves_quorum(storage_candidates: usize, replicas: usize) -> bool {
    if storage_candidates == 0 {
        return true;
    }
    let remaining = storage_candidates.saturating_sub(1);
    let quorum = (replicas / 2) + 1;
    remaining >= quorum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r_two_is_never_selected() {
        for candidates in 0..10 {
            assert_ne!(desired_replicas(candidates, ReplicaPreference::Default), 2);
            assert_ne!(desired_replicas(candidates, ReplicaPreference::Five), 2);
        }
    }

    #[test]
    fn two_node_cluster_uses_single_replica() {
        assert_eq!(desired_replicas(2, ReplicaPreference::Default), 1);
    }

    #[test]
    fn demoting_one_of_three_is_storage_degradation() {
        assert!(demotion_requires_degradation_acceptance(3));
        assert!(!demotion_requires_degradation_acceptance(4));
    }
}
