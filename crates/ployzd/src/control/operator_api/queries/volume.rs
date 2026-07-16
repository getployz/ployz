use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    MAX_CONCURRENT_MACHINE_READS, MachineVolumeTestimonyReadError, NatsMachineVolumeTestimonyReader,
};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{MachineVolumeTestimony, MachineVolumeTestimonyResult};
use futures_util::{StreamExt, stream};
use ployz_core::ids::MachineId;
use ployz_core::intent::{IntentSnapshot, VolumePinState};
use ployz_sdk_types::{
    VolumeListError, VolumeListResult, VolumeSnapshot, VolumeStatus, VolumeTestimony,
};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

const VOLUME_QUERY_TIMEOUT: Duration = Duration::from_secs(8);

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
        let deadline = Instant::now() + VOLUME_QUERY_TIMEOUT;
        let intent = tokio::time::timeout_at(deadline, self.intent_reader.intent())
            .await
            .map_err(|_| VolumeListError::Unavailable {
                message: "volume list timed out reading intent".to_owned(),
            })?
            .map_err(|error| VolumeListError::Unavailable {
                message: error.to_string(),
            })?;
        let answers = gather_volume_testimony(&self.testimony_reader, &intent, deadline).await;
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
    deadline: Instant,
) -> Vec<MachineAnswer> {
    let mut pins_by_machine = BTreeMap::<MachineId, Vec<VolumePinState>>::new();
    for pin in &intent.volume_pins {
        pins_by_machine
            .entry(pin.machine_id().clone())
            .or_default()
            .push(pin.clone());
    }

    collect_bounded_machine_answers(pins_by_machine, deadline, |machine_id, pins| async move {
        let answer = reader.volume_testimony(&machine_id, pins).await;
        (machine_id, answer)
    })
    .await
}

async fn collect_bounded_machine_answers<F, Fut>(
    requests: impl IntoIterator<Item = (MachineId, Vec<VolumePinState>)>,
    deadline: Instant,
    mut read: F,
) -> Vec<MachineAnswer>
where
    F: FnMut(MachineId, Vec<VolumePinState>) -> Fut,
    Fut: Future<Output = MachineAnswer>,
{
    let requests = requests.into_iter().collect::<Vec<_>>();
    let mut pending = requests
        .iter()
        .map(|(machine_id, _)| machine_id.clone())
        .collect::<BTreeSet<_>>();
    let mut reads = stream::iter(requests)
        .map(move |(machine_id, pins)| read(machine_id, pins))
        .buffer_unordered(MAX_CONCURRENT_MACHINE_READS);

    let mut answers = Vec::new();
    while let Ok(Some(answer)) = tokio::time::timeout_at(deadline, reads.next()).await {
        pending.remove(&answer.0);
        answers.push(answer);
    }
    answers.extend(pending.into_iter().map(|machine_id| {
        (
            machine_id.clone(),
            Err(MachineVolumeTestimonyReadError::Unavailable {
                machine_id,
                reason: MachineRuntimeUnavailableReason::RequestTimedOut,
            }),
        )
    }));
    answers.sort_by(|(left, _), (right, _)| left.cmp(right));
    answers
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
                for requested_pin in intent
                    .volume_pins
                    .iter()
                    .filter(|pin| pin.machine_id() == &machine_id)
                {
                    if !results.iter().any(|result| result.pin == *requested_pin) {
                        return Err(VolumeListError::Unavailable {
                            message: format!(
                                "machine {} returned incomplete volume testimony: missing {}/{}",
                                machine_id.as_str(),
                                requested_pin.namespace_id().as_str(),
                                requested_pin.volume_name().as_str()
                            ),
                        });
                    }
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
    use super::{
        MAX_CONCURRENT_MACHINE_READS, MachineAnswer, VOLUME_QUERY_TIMEOUT,
        collect_bounded_machine_answers, volume_snapshots,
    };
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        for _ in 0..1_000 {
            if counter.load(Ordering::SeqCst) == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), expected);
    }

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

    #[tokio::test]
    async fn machine_gather_is_bounded_incremental_and_retains_every_answer() {
        assert_eq!(MAX_CONCURRENT_MACHINE_READS, 16);
        let request_count = MAX_CONCURRENT_MACHINE_READS + 1;
        let requests = (0..request_count)
            .map(|index| (machine_id(&format!("machine-{index:02}")), Vec::new()))
            .collect::<Vec<_>>();
        let started = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Semaphore::new(0));

        let gather = tokio::spawn({
            let started = Arc::clone(&started);
            let in_flight = Arc::clone(&in_flight);
            let max_in_flight = Arc::clone(&max_in_flight);
            let release = Arc::clone(&release);
            async move {
                collect_bounded_machine_answers(
                    requests,
                    tokio::time::Instant::now() + Duration::from_secs(60),
                    move |machine_id, pins| {
                        let started = Arc::clone(&started);
                        let in_flight = Arc::clone(&in_flight);
                        let max_in_flight = Arc::clone(&max_in_flight);
                        let release = Arc::clone(&release);
                        async move {
                            assert!(pins.is_empty());
                            started.fetch_add(1, Ordering::SeqCst);
                            let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                            max_in_flight.fetch_max(current, Ordering::SeqCst);
                            release
                                .acquire()
                                .await
                                .expect("release remains open")
                                .forget();
                            in_flight.fetch_sub(1, Ordering::SeqCst);
                            (
                                machine_id.clone(),
                                Err(MachineVolumeTestimonyReadError::Unavailable {
                                    machine_id,
                                    reason: MachineRuntimeUnavailableReason::RequestTimedOut,
                                }),
                            )
                        }
                    },
                )
                .await
            }
        });

        wait_for_count(&started, MAX_CONCURRENT_MACHINE_READS).await;
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            MAX_CONCURRENT_MACHINE_READS
        );
        release.add_permits(1);
        wait_for_count(&started, request_count).await;
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            MAX_CONCURRENT_MACHINE_READS
        );
        release.add_permits(MAX_CONCURRENT_MACHINE_READS);

        let answers = gather.await.expect("gather task completes");
        assert_eq!(answers.len(), request_count);
        let [first, .., last] = answers.as_slice() else {
            panic!("gather must retain the complete request set");
        };
        assert_eq!(first.0.as_str(), "machine-00");
        assert_eq!(last.0.as_str(), "machine-16");
    }

    #[tokio::test(start_paused = true)]
    async fn machine_gather_returns_every_machine_at_the_query_deadline() {
        let request_count = 33;
        let requests = (0..request_count)
            .map(|index| (machine_id(&format!("machine-{index:02}")), Vec::new()))
            .collect::<Vec<_>>();
        let started_at = tokio::time::Instant::now();

        let answers = collect_bounded_machine_answers(
            requests,
            started_at + VOLUME_QUERY_TIMEOUT,
            |machine_id, pins| async move {
                assert!(pins.is_empty());
                if machine_id.as_str() == "machine-00" {
                    return (machine_id, Ok(Vec::new()));
                }
                std::future::pending::<()>().await;
                unreachable!("silent machine cannot complete")
            },
        )
        .await;

        assert_eq!(
            tokio::time::Instant::now() - started_at,
            VOLUME_QUERY_TIMEOUT
        );
        assert_eq!(answers.len(), request_count);
        let [fast, timed_out @ ..] = answers.as_slice() else {
            panic!("the complete request set must be retained");
        };
        assert_eq!(fast.0.as_str(), "machine-00");
        assert!(fast.1.is_ok());
        assert_eq!(timed_out.len(), request_count - 1);
        assert!(timed_out.iter().all(|(machine_id, answer)| matches!(
            answer,
            Err(MachineVolumeTestimonyReadError::Unavailable {
                machine_id: unavailable_machine_id,
                reason: MachineRuntimeUnavailableReason::RequestTimedOut,
            }) if unavailable_machine_id == machine_id
        )));
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
    fn ignores_foreign_results_instead_of_creating_or_borrowing_truth() {
        let requested = plain("team", "requested", "machine-a");
        let foreign = plain("other", "foreign", "machine-a");
        let intent = intent_with(vec![requested.clone()]);
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
        assert_eq!(projected.len(), 1);
        let [projected] = projected.as_slice() else {
            panic!("only the requested volume may be projected");
        };
        assert_eq!(projected.testimony, VolumeTestimony::Unavailable);
    }

    #[test]
    fn answered_machine_omitting_requested_pin_is_an_invariant_error() {
        let returned = plain("team", "returned", "machine-a");
        let omitted = plain("team", "omitted", "machine-a");
        let intent = intent_with(vec![returned.clone(), omitted]);
        let answers = vec![(
            machine_id("machine-a"),
            Ok(vec![result(returned, MachineVolumeTestimony::Unavailable)]),
        )];

        let error = volume_snapshots(&intent, answers).expect_err("omission must fail");
        assert!(error.to_string().contains("incomplete volume testimony"));
        assert!(error.to_string().contains("team/omitted"));
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
        let [alpha, projected_zeta] = projected.as_slice() else {
            panic!("intent volumes must project exactly once");
        };
        assert_eq!(alpha.volume_name.as_str(), "alpha");
        assert_eq!(projected_zeta.volume_name, zeta);
        assert!(matches!(&alpha.kind, VolumeKind::Provisioned { .. }));
        assert_eq!(alpha.status, VolumeStatus::InUse);
        assert_eq!(
            alpha
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
