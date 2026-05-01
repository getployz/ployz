use crate::model::{MachineId, MachineMembership};
use async_trait::async_trait;
use futures_util::future::join_all;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParticipantReachability {
    pub(super) unreachable: Vec<MachineId>,
    details: HashMap<MachineId, ParticipantReachabilityDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParticipantReachabilityDetail {
    status: ProbeStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeStatus {
    Reachable,
    Timeout,
    Refused,
    Unexpected,
    InventoryMissing,
}

impl ProbeStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
            Self::Unexpected => "unexpected",
            Self::InventoryMissing => "inventory-missing",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeErrorKind {
    Timeout,
    Refused,
    Unexpected,
}

/// Issued at deploy decision time to confirm each participant's daemon is
/// processing requests on the overlay. Implementations live outside the
/// orchestrator so the core stays free of transport details.
#[async_trait]
pub trait ParticipantProbe: Send + Sync {
    async fn ping(&self, machine: &MachineMembership) -> Result<(), ProbeError>;
}

/// Probe that always succeeds. Use in tests and in single-machine setups
/// where no peer needs to be reached.
pub struct NoopParticipantProbe;

#[async_trait]
impl ParticipantProbe for NoopParticipantProbe {
    async fn ping(&self, _machine: &MachineMembership) -> Result<(), ProbeError> {
        Ok(())
    }
}

pub(super) async fn probe_participants(
    prober: &dyn ParticipantProbe,
    participants: &BTreeSet<MachineId>,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> ParticipantReachability {
    let probes: Vec<_> = participants
        .iter()
        .map(|machine_id| {
            let machine = machine_map.get(machine_id).cloned();
            async move {
                let Some(machine) = machine else {
                    return (
                        machine_id.clone(),
                        ParticipantReachabilityDetail {
                            status: ProbeStatus::InventoryMissing,
                            detail: "missing from machine inventory".into(),
                        },
                    );
                };
                let detail = match prober.ping(&machine).await {
                    Ok(()) => ParticipantReachabilityDetail {
                        status: ProbeStatus::Reachable,
                        detail: String::new(),
                    },
                    Err(error) => ParticipantReachabilityDetail {
                        status: match error.kind {
                            ProbeErrorKind::Timeout => ProbeStatus::Timeout,
                            ProbeErrorKind::Refused => ProbeStatus::Refused,
                            ProbeErrorKind::Unexpected => ProbeStatus::Unexpected,
                        },
                        detail: error.detail,
                    },
                };
                (machine.id.clone(), detail)
            }
        })
        .collect();

    let outcomes = join_all(probes).await;

    let mut unreachable = Vec::new();
    let mut details = HashMap::new();
    for (machine_id, detail) in outcomes {
        if detail.status != ProbeStatus::Reachable {
            unreachable.push(machine_id.clone());
        }
        details.insert(machine_id, detail);
    }
    unreachable.sort_by(|left, right| left.0.cmp(&right.0));
    ParticipantReachability {
        unreachable,
        details,
    }
}

pub(super) fn warnings_from_reachability(reachability: &ParticipantReachability) -> Vec<String> {
    reachability
        .unreachable
        .iter()
        .map(|machine_id| {
            let detail = reachability.details.get(machine_id);
            let (status, message) = detail
                .map(|detail| (detail.status.label(), detail.detail.as_str()))
                .unwrap_or(("unknown", ""));
            if message.is_empty() {
                format!("machine '{}' is unreachable ({})", machine_id, status)
            } else {
                format!(
                    "machine '{}' is unreachable ({}: {})",
                    machine_id, status, message
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineLifecycle, MachineRole, MachineTopology, OverlayIp, PublicKey};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;
    use std::sync::Mutex;

    struct ScriptedProbe {
        outcomes: Mutex<HashMap<MachineId, Result<(), ProbeError>>>,
    }

    impl ScriptedProbe {
        fn new(outcomes: impl IntoIterator<Item = (MachineId, Result<(), ProbeError>)>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl ParticipantProbe for ScriptedProbe {
        async fn ping(&self, machine: &MachineMembership) -> Result<(), ProbeError> {
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .remove(&machine.id)
                .unwrap_or(Ok(()))
        }
    }

    fn test_machine(id: &str, overlay_ip: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(overlay_ip.parse::<Ipv6Addr>().expect("valid overlay ip")),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            role: MachineRole::StorageCandidate,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn all_reachable_yields_empty_unreachable_list() {
        let machine = test_machine("alive-1", "fd00::1");
        let participants = BTreeSet::from([machine.id.clone()]);
        let machine_map = HashMap::from([(machine.id.clone(), machine)]);
        let prober = NoopParticipantProbe;

        let reachability = probe_participants(&prober, &participants, &machine_map).await;

        assert!(reachability.unreachable.is_empty());
        assert!(warnings_from_reachability(&reachability).is_empty());
    }

    #[tokio::test]
    async fn timeout_lands_in_unreachable_with_timeout_label() {
        let alive = test_machine("alive-1", "fd00::1");
        let dead = test_machine("dead-1", "fd00::2");
        let participants = BTreeSet::from([alive.id.clone(), dead.id.clone()]);
        let machine_map =
            HashMap::from([(alive.id.clone(), alive), (dead.id.clone(), dead.clone())]);
        let prober = ScriptedProbe::new([(
            dead.id.clone(),
            Err(ProbeError {
                kind: ProbeErrorKind::Timeout,
                detail: "rpc connect timed out after 3s".into(),
            }),
        )]);

        let reachability = probe_participants(&prober, &participants, &machine_map).await;

        assert_eq!(reachability.unreachable, vec![dead.id]);
        let warnings = warnings_from_reachability(&reachability);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("timeout"),
            "expected timeout label, got: {}",
            warnings[0]
        );
        assert!(warnings[0].contains("rpc connect timed out"));
    }

    #[tokio::test]
    async fn unexpected_response_marks_peer_unreachable() {
        let machine = test_machine("weird-1", "fd00::3");
        let participants = BTreeSet::from([machine.id.clone()]);
        let machine_map = HashMap::from([(machine.id.clone(), machine.clone())]);
        let prober = ScriptedProbe::new([(
            machine.id.clone(),
            Err(ProbeError {
                kind: ProbeErrorKind::Unexpected,
                detail: "remote daemon error [BAD_STATE]: not ready".into(),
            }),
        )]);

        let reachability = probe_participants(&prober, &participants, &machine_map).await;

        assert_eq!(reachability.unreachable, vec![machine.id]);
        let warnings = warnings_from_reachability(&reachability);
        assert!(warnings[0].contains("unexpected"));
    }

    #[tokio::test]
    async fn empty_participants_yields_no_warnings() {
        let participants = BTreeSet::<MachineId>::new();
        let machine_map = HashMap::new();
        let prober = NoopParticipantProbe;

        let reachability = probe_participants(&prober, &participants, &machine_map).await;

        assert!(reachability.unreachable.is_empty());
        assert!(warnings_from_reachability(&reachability).is_empty());
    }

    #[tokio::test]
    async fn missing_inventory_marks_unreachable() {
        let participants = BTreeSet::from([MachineId("ghost".into())]);
        let machine_map = HashMap::new();
        let prober = NoopParticipantProbe;

        let reachability = probe_participants(&prober, &participants, &machine_map).await;

        assert_eq!(reachability.unreachable, vec![MachineId("ghost".into())]);
        let warnings = warnings_from_reachability(&reachability);
        assert!(warnings[0].contains("inventory-missing"));
    }
}
