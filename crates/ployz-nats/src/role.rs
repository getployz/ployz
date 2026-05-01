use ployz_types::model::{
    AvailabilityZoneName, MachineId, MachineLifecycle, MachineMembership, MachineRole,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaPreference {
    /// Preserve the current single-copy authority unless an operator asks for
    /// an HA target.
    Default,
    /// Operator-explicit R=3 target.
    Three,
    /// Operator-explicit R=5 target.
    Five,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaPolicy {
    pub storage_candidates: usize,
    pub replicas: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePromotionPlan {
    pub replicas: usize,
    pub candidates: Vec<MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoragePromotionError {
    InvalidReplicaTarget {
        replicas: usize,
    },
    CandidateCountMismatch {
        replicas: usize,
        selected: usize,
    },
    DuplicateCandidate {
        machine_id: MachineId,
    },
    UnknownCandidate {
        machine_id: MachineId,
    },
    IneligibleLifecycle {
        machine_id: MachineId,
        lifecycle: MachineLifecycle,
    },
    IneligibleRole {
        machine_id: MachineId,
        role: MachineRole,
    },
    InsufficientFailureDomains {
        replicas: usize,
        availability_zones: usize,
    },
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
pub fn desired_replicas(_storage_candidates: usize, preference: ReplicaPreference) -> usize {
    match preference {
        ReplicaPreference::Default => 1,
        ReplicaPreference::Three => 3,
        ReplicaPreference::Five => 5,
    }
}

pub fn plan_storage_promotion(
    machines: &[MachineMembership],
    selected_candidates: &[MachineId],
    preference: ReplicaPreference,
) -> Result<StoragePromotionPlan, StoragePromotionError> {
    let replicas = desired_replicas(machines.len(), preference);
    validate_replica_target(replicas)?;
    if selected_candidates.len() != replicas {
        return Err(StoragePromotionError::CandidateCountMismatch {
            replicas,
            selected: selected_candidates.len(),
        });
    }

    let mut seen = BTreeSet::new();
    for machine_id in selected_candidates {
        if !seen.insert(machine_id.clone()) {
            return Err(StoragePromotionError::DuplicateCandidate {
                machine_id: machine_id.clone(),
            });
        }
    }

    let machines_by_id = machines
        .iter()
        .map(|machine| (machine.id.clone(), machine))
        .collect::<BTreeMap<_, _>>();
    let selected = selected_candidates
        .iter()
        .map(|machine_id| {
            machines_by_id.get(machine_id).copied().ok_or_else(|| {
                StoragePromotionError::UnknownCandidate {
                    machine_id: machine_id.clone(),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    for machine in &selected {
        match machine.lifecycle {
            MachineLifecycle::Active => {}
            MachineLifecycle::Standby | MachineLifecycle::Draining => {
                return Err(StoragePromotionError::IneligibleLifecycle {
                    machine_id: machine.id.clone(),
                    lifecycle: machine.lifecycle,
                });
            }
        }
        match machine.role {
            MachineRole::StorageCandidate => {}
            MachineRole::Mirror | MachineRole::Leaf => {
                return Err(StoragePromotionError::IneligibleRole {
                    machine_id: machine.id.clone(),
                    role: machine.role,
                });
            }
        }
    }

    validate_declared_failure_domains(&selected, replicas)?;

    Ok(StoragePromotionPlan {
        replicas,
        candidates: selected_candidates.to_vec(),
    })
}

fn validate_replica_target(replicas: usize) -> Result<(), StoragePromotionError> {
    match replicas {
        1 | 3 | 5 => Ok(()),
        _ => Err(StoragePromotionError::InvalidReplicaTarget { replicas }),
    }
}

fn validate_declared_failure_domains(
    machines: &[&MachineMembership],
    replicas: usize,
) -> Result<(), StoragePromotionError> {
    if replicas == 1
        || machines
            .iter()
            .any(|machine| machine.topology.availability_zone.is_none())
    {
        return Ok(());
    }

    let availability_zones = machines
        .iter()
        .filter_map(|machine| machine.topology.availability_zone.as_ref())
        .collect::<BTreeSet<&AvailabilityZoneName>>()
        .len();
    if availability_zones < replicas {
        return Err(StoragePromotionError::InsufficientFailureDomains {
            replicas,
            availability_zones,
        });
    }
    Ok(())
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
    use ployz_types::model::{MachineTopology, OverlayIp, PublicKey};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

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
    fn three_storage_candidates_do_not_promote_without_operator_intent() {
        assert_eq!(desired_replicas(3, ReplicaPreference::Default), 1);
    }

    #[test]
    fn r_three_requires_explicit_preference() {
        assert_eq!(desired_replicas(3, ReplicaPreference::Three), 3);
    }

    #[test]
    fn storage_promotion_plan_accepts_three_active_storage_candidates() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-b"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b", "c"]);

        let plan = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect("storage promotion should pass");

        assert_eq!(
            plan,
            StoragePromotionPlan {
                replicas: 3,
                candidates: selected,
            }
        );
    }

    #[test]
    fn storage_promotion_requires_exact_candidate_count() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-b"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b"]);

        let error = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect_err("partial selection should fail");

        assert_eq!(
            error,
            StoragePromotionError::CandidateCountMismatch {
                replicas: 3,
                selected: 2,
            }
        );
    }

    #[test]
    fn storage_promotion_rejects_duplicate_candidate() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-b"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "a", "c"]);

        let error = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect_err("duplicate selection should fail");

        assert_eq!(
            error,
            StoragePromotionError::DuplicateCandidate {
                machine_id: MachineId("a".into()),
            }
        );
    }

    #[test]
    fn storage_promotion_rejects_inactive_candidate() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Standby,
                MachineRole::StorageCandidate,
                Some("az-b"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b", "c"]);

        let error = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect_err("standby candidate should fail");

        assert_eq!(
            error,
            StoragePromotionError::IneligibleLifecycle {
                machine_id: MachineId("b".into()),
                lifecycle: MachineLifecycle::Standby,
            }
        );
    }

    #[test]
    fn storage_promotion_rejects_non_storage_role() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::Mirror,
                Some("az-b"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b", "c"]);

        let error = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect_err("mirror candidate should fail");

        assert_eq!(
            error,
            StoragePromotionError::IneligibleRole {
                machine_id: MachineId("b".into()),
                role: MachineRole::Mirror,
            }
        );
    }

    #[test]
    fn storage_promotion_rejects_duplicate_declared_availability_zone() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b", "c"]);

        let error = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect_err("duplicate failure domain should fail");

        assert_eq!(
            error,
            StoragePromotionError::InsufficientFailureDomains {
                replicas: 3,
                availability_zones: 2,
            }
        );
    }

    #[test]
    fn storage_promotion_allows_missing_availability_zone_metadata() {
        let machines = vec![
            machine(
                "a",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-a"),
            ),
            machine(
                "b",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                None,
            ),
            machine(
                "c",
                MachineLifecycle::Active,
                MachineRole::StorageCandidate,
                Some("az-c"),
            ),
        ];
        let selected = machine_ids(["a", "b", "c"]);

        let plan = plan_storage_promotion(&machines, &selected, ReplicaPreference::Three)
            .expect("missing AZ metadata should not block local clusters");

        assert_eq!(plan.replicas, 3);
    }

    #[test]
    fn demoting_one_of_three_is_storage_degradation() {
        assert!(demotion_requires_degradation_acceptance(3));
        assert!(!demotion_requires_degradation_acceptance(4));
    }

    fn machine_ids<const N: usize>(ids: [&str; N]) -> Vec<MachineId> {
        ids.into_iter().map(|id| MachineId(id.into())).collect()
    }

    fn machine(
        id: &str,
        lifecycle: MachineLifecycle,
        role: MachineRole,
        availability_zone: Option<&str>,
    ) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::new("local", availability_zone)
                .expect("test topology should be valid"),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle,
            role,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }
}
