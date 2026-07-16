use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    MachineVolumeTestimonyReadError, NatsMachineVolumeTestimonyReader,
};
use crate::roles::machine::protocol::{MachineVolumeTestimony, MachineVolumeTestimonyResult};
use futures_util::future::join_all;
use ployz_core::ids::MachineId;
use ployz_core::intent::{IntentSnapshot, VolumePinState};
use ployz_sdk_types::{
    VolumeListError, VolumeListResult, VolumeSnapshot, VolumeStatus, VolumeTestimony,
};
use std::collections::BTreeMap;
use std::time::Duration;

const VOLUME_GATHER_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub struct VolumeQueryService {
    intent_reader: NatsIntentReader,
    testimony_reader: NatsMachineVolumeTestimonyReader,
}

impl VolumeQueryService {
    #[must_use]
    pub(crate) const fn new(
        intent_reader: NatsIntentReader,
        testimony_reader: NatsMachineVolumeTestimonyReader,
    ) -> Self {
        Self {
            intent_reader,
            testimony_reader,
        }
    }

    pub(crate) async fn list(&self) -> Result<VolumeListResult, VolumeListError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| VolumeListError::Unavailable {
                    message: error.to_string(),
                })?;
        let answers = gather_volume_testimony(&self.testimony_reader, &intent).await;
        Ok(VolumeListResult {
            volumes: volume_snapshots(&intent, answers)?,
        })
    }
}

type MachineAnswer = (
    MachineId,
    Result<Vec<MachineVolumeTestimonyResult>, MachineVolumeTestimonyReadError>,
);

async fn gather_volume_testimony(
    reader: &NatsMachineVolumeTestimonyReader,
    intent: &IntentSnapshot,
) -> Vec<MachineAnswer> {
    let mut pins_by_machine = BTreeMap::<MachineId, Vec<VolumePinState>>::new();
    for pin in &intent.volume_pins {
        pins_by_machine
            .entry(pin.machine_id().clone())
            .or_default()
            .push(pin.clone());
    }

    let requests = pins_by_machine
        .into_iter()
        .map(|(machine_id, pins)| async move {
            let answer = reader.volume_testimony(&machine_id, pins).await;
            (machine_id, answer)
        });

    tokio::time::timeout(VOLUME_GATHER_TIMEOUT, join_all(requests))
        .await
        .unwrap_or_default()
}

fn volume_snapshots(
    intent: &IntentSnapshot,
    answers: Vec<MachineAnswer>,
) -> Result<Vec<VolumeSnapshot>, VolumeListError> {
    let mut answers_by_machine = BTreeMap::new();
    for (machine_id, answer) in answers {
        match answer {
            Ok(results) => {
                if results
                    .iter()
                    .any(|result| result.pin.machine_id() != &machine_id)
                {
                    return Err(VolumeListError::Unavailable {
                        message: format!(
                            "machine {} returned volume testimony for another machine",
                            machine_id.as_str()
                        ),
                    });
                }
                answers_by_machine.insert(machine_id, results);
            }
            Err(MachineVolumeTestimonyReadError::Unavailable { .. }) => {}
            Err(error @ MachineVolumeTestimonyReadError::MachineMismatch { .. }) => {
                return Err(VolumeListError::Unavailable {
                    message: error.to_string(),
                });
            }
        }
    }

    let mut volumes = intent
        .volume_pins
        .iter()
        .map(|pin| {
            let referencing_services =
                intent.services_referencing_volume(pin.namespace_id(), pin.volume_name());
            let testimony = answers_by_machine
                .get(pin.machine_id())
                .and_then(|results| results.iter().find(|result| result.pin == *pin))
                .map_or(VolumeTestimony::NoAnswer, |result| {
                    match &result.testimony {
                        MachineVolumeTestimony::Available { facts } => VolumeTestimony::Available {
                            used_bytes: facts.used_bytes,
                            last_write_unix_seconds: facts.last_write_unix_seconds,
                        },
                        MachineVolumeTestimony::Unavailable => VolumeTestimony::Unavailable,
                    }
                });
            VolumeSnapshot {
                namespace_id: pin.namespace_id().clone(),
                volume_name: pin.volume_name().clone(),
                machine_id: pin.machine_id().clone(),
                kind: pin.kind().clone(),
                status: if referencing_services.is_empty() {
                    VolumeStatus::Orphaned
                } else {
                    VolumeStatus::InUse
                },
                referencing_services,
                testimony,
            }
        })
        .collect::<Vec<_>>();
    volumes.sort_by(|left, right| {
        (&left.namespace_id, &left.volume_name).cmp(&(&right.namespace_id, &right.volume_name))
    });
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::{MachineAnswer, volume_snapshots};
    use crate::control::role_client::machine::MachineVolumeTestimonyReadError;
    use crate::roles::machine::MachineRuntimeUnavailableReason;
    use crate::roles::machine::protocol::{MachineVolumeTestimony, MachineVolumeTestimonyResult};
    use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::intent::{IntentSnapshot, VolumeKind, VolumePinState};
    use ployz_core::machine::storage::VolumeUsageFacts;
    use ployz_sdk_types::{VolumeStatus, VolumeTestimony};
    use ployz_test_support::fixtures::serving_target_entry_in;
    use ployz_test_support::ids::{machine_id, namespace_id};

    fn intent_with(pins: Vec<VolumePinState>) -> IntentSnapshot {
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine_id("core"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::network::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: pins,
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }

    fn plain(namespace: &str, volume: &str, machine: &str) -> VolumePinState {
        VolumePinState::plain(
            namespace_id(namespace),
            VolumeName::try_new(volume).expect("valid volume"),
            machine_id(machine),
        )
    }

    fn result(
        pin: VolumePinState,
        testimony: MachineVolumeTestimony,
    ) -> MachineVolumeTestimonyResult {
        MachineVolumeTestimonyResult { pin, testimony }
    }

    #[test]
    fn projects_available_unavailable_and_silent_machine_without_discarding_answers() {
        let available = plain("team", "available", "machine-a");
        let unavailable = plain("team", "unavailable", "machine-a");
        let silent = plain("team", "silent", "machine-b");
        let intent = intent_with(vec![silent, unavailable.clone(), available.clone()]);
        let answers = vec![
            (
                machine_id("machine-a"),
                Ok(vec![
                    result(
                        available,
                        MachineVolumeTestimony::Available {
                            facts: VolumeUsageFacts {
                                used_bytes: 42,
                                last_write_unix_seconds: 99,
                            },
                        },
                    ),
                    result(unavailable, MachineVolumeTestimony::Unavailable),
                ]),
            ),
            (
                machine_id("machine-b"),
                Err(MachineVolumeTestimonyReadError::Unavailable {
                    machine_id: machine_id("machine-b"),
                    reason: MachineRuntimeUnavailableReason::RequestTimedOut,
                }),
            ),
        ];

        let projected = volume_snapshots(&intent, answers).expect("valid projection");
        assert_eq!(
            projected
                .iter()
                .map(|snapshot| &snapshot.testimony)
                .collect::<Vec<_>>(),
            vec![
                &VolumeTestimony::Available {
                    used_bytes: 42,
                    last_write_unix_seconds: 99,
                },
                &VolumeTestimony::NoAnswer,
                &VolumeTestimony::Unavailable,
            ]
        );
    }

    #[test]
    fn ignores_foreign_and_missing_results_instead_of_creating_or_borrowing_truth() {
        let requested = plain("team", "requested", "machine-a");
        let missing = plain("team", "missing", "machine-a");
        let foreign = plain("other", "foreign", "machine-a");
        let intent = intent_with(vec![requested.clone(), missing]);
        let answers = vec![(
            machine_id("machine-a"),
            Ok(vec![
                result(requested, MachineVolumeTestimony::Unavailable),
                result(
                    foreign,
                    MachineVolumeTestimony::Available {
                        facts: VolumeUsageFacts {
                            used_bytes: 9000,
                            last_write_unix_seconds: 9001,
                        },
                    },
                ),
            ]),
        )];

        let projected = volume_snapshots(&intent, answers).expect("valid projection");
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].testimony, VolumeTestimony::NoAnswer);
        assert_eq!(projected[1].testimony, VolumeTestimony::Unavailable);
    }

    #[test]
    fn durable_fields_references_and_order_come_only_from_intent() {
        let namespace = namespace_id("team");
        let alpha = VolumeName::try_new("alpha").expect("valid volume");
        let zeta = VolumeName::try_new("zeta").expect("valid volume");
        let pool = ZfsPoolName::try_new("tank").expect("valid pool");
        let provisioned = VolumePinState::try_new(
            namespace.clone(),
            alpha.clone(),
            machine_id("machine-a"),
            VolumeKind::Provisioned {
                dataset: DatasetName::for_volume(&pool, &namespace, &alpha)
                    .expect("canonical dataset"),
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("valid size"),
            },
        )
        .expect("valid pin");
        let mut target_b = serving_target_entry_in("team", "web-b", "entry-b");
        target_b.volume_names = vec![alpha.clone(), alpha.clone()];
        let mut target_a = serving_target_entry_in("team", "web-a", "entry-a");
        target_a.volume_names = vec![alpha];
        let mut intent = intent_with(vec![plain("team", "zeta", "machine-z"), provisioned]);
        intent.serving_target_entries = vec![target_b, target_a];

        let projected =
            volume_snapshots(&intent, Vec::<MachineAnswer>::new()).expect("valid projection");
        assert_eq!(projected[0].volume_name.as_str(), "alpha");
        assert_eq!(projected[1].volume_name, zeta);
        assert!(matches!(projected[0].kind, VolumeKind::Provisioned { .. }));
        assert_eq!(projected[0].status, VolumeStatus::InUse);
        assert_eq!(
            projected[0]
                .referencing_services
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            vec!["web-a", "web-b"]
        );
    }

    #[test]
    fn responder_machine_mismatch_is_an_invariant_error() {
        let pin = plain("team", "data", "machine-a");
        let intent = intent_with(vec![pin]);
        let answers = vec![(
            machine_id("machine-a"),
            Err(MachineVolumeTestimonyReadError::MachineMismatch {
                expected_machine_id: machine_id("machine-a"),
                responder_machine_id: machine_id("machine-b"),
            }),
        )];

        let error = volume_snapshots(&intent, answers).expect_err("mismatch must fail");
        assert!(error.to_string().contains("rejected volume testimony"));
    }

    #[test]
    fn result_for_another_machine_is_an_invariant_error() {
        let pin = plain("team", "data", "machine-a");
        let intent = intent_with(vec![pin]);
        let answers = vec![(
            machine_id("machine-a"),
            Ok(vec![result(
                plain("team", "data", "machine-b"),
                MachineVolumeTestimony::Unavailable,
            )]),
        )];

        let error = volume_snapshots(&intent, answers).expect_err("mismatch must fail");
        assert!(error.to_string().contains("another machine"));
    }
}
