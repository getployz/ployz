use std::collections::BTreeMap;

use futures_util::FutureExt;
use ployz_core::{
    LocalMachinePhase, MachineId, RpcError, RpcErrorCode, SealedSealingKey, Sealing,
    SealingPublicKey, SealingRefusal, WireGuardPublicKey,
};
use serde_json::{Value, json};

use super::{
    BackfillClient, BackfillSealingKeyReport, BackfillSealingKeyRequest, Can, Distribution, Listed,
    MachineBackfill, MachineOutcome, Plan, Seen, Sight, SkipReason, distribution, execute, plan,
    settle,
};

fn id(n: u8) -> MachineId {
    MachineId::parse(format!("{n:0>32}")).unwrap()
}

fn key(n: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([n; 32])
}

fn wg(n: u8) -> WireGuardPublicKey {
    WireGuardPublicKey([n; 32])
}

const ALL: Can = Can {
    generate: true,
    share: true,
    install: true,
};

fn seen(n: u8, sealing: Sealing) -> Sight {
    Sight::Seen(Seen {
        phase: LocalMachinePhase::Participating,
        wireguard_key: wg(n),
        sealing,
        can: ALL,
    })
}

fn holds(n: u8, k: u8) -> Sight {
    seen(n, Sealing::Held { public_key: key(k) })
}

fn needs(n: u8) -> Sight {
    seen(n, Sealing::NeedsAccess)
}

fn awaits(n: u8, k: u8) -> Sight {
    seen(n, Sealing::AwaitsRecovery { public_key: key(k) })
}

fn with(sight: Sight, edit: impl FnOnce(&mut Seen)) -> Sight {
    let Sight::Seen(mut seen) = sight else {
        panic!("not seen");
    };
    edit(&mut seen);
    Sight::Seen(seen)
}

fn unknown() -> Sight {
    Sight::Unknown {
        reason: "its peers do not see it up".into(),
    }
}

fn sights(entries: Vec<(u8, Sight)>) -> BTreeMap<MachineId, Sight> {
    entries
        .into_iter()
        .map(|(n, sight)| (id(n), sight))
        .collect()
}

fn request(expected: Option<u8>, generator: Option<u8>) -> BackfillSealingKeyRequest {
    BackfillSealingKeyRequest {
        expected: expected.map(key),
        generator: generator.map(id),
    }
}

fn refusal(refusal: SealingRefusal) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: "refused".into(),
        details: refusal.details(),
        cause: Vec::new(),
    }
}

fn failure(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

#[test]
fn expected_key_holders_are_the_sources() {
    let sights = sights(vec![(1, holds(1, 7)), (2, holds(2, 9)), (3, needs(3))]);
    assert_eq!(
        plan(&request(Some(7), None), &sights),
        Plan::Distribute {
            public_key: key(7),
            sources: vec![id(1)],
        }
    );
}

#[test]
fn expected_key_nobody_holds_stops() {
    let sights = sights(vec![(1, holds(1, 9)), (2, needs(2))]);
    assert_eq!(
        plan(&request(Some(7), None), &sights),
        Plan::Stop(BackfillSealingKeyReport::NoHolder { expected: key(7) })
    );
}

#[test]
fn agreeing_holders_settle_the_key_and_only_sharers_are_sources() {
    let sights = sights(vec![
        (1, holds(1, 7)),
        (2, with(holds(2, 7), |seen| seen.can.share = false)),
        (3, needs(3)),
    ]);
    assert_eq!(
        plan(&request(None, None), &sights),
        Plan::Distribute {
            public_key: key(7),
            sources: vec![id(1)],
        }
    );
}

#[test]
fn disagreeing_holders_stop_with_their_keys_in_machine_order() {
    let sights = sights(vec![(1, holds(1, 9)), (2, holds(2, 7)), (3, holds(3, 9))]);
    assert_eq!(
        plan(&request(None, None), &sights),
        Plan::Stop(BackfillSealingKeyReport::Conflicting {
            keys: vec![key(9), key(7)],
        })
    );
}

#[test]
fn no_holder_names_the_lowest_capable_candidate() {
    let sights = sights(vec![
        (1, with(needs(1), |seen| seen.can.generate = false)),
        (
            2,
            with(needs(2), |seen| {
                seen.phase = LocalMachinePhase::Joining;
            }),
        ),
        (3, unknown()),
        (4, needs(4)),
        (5, needs(5)),
    ]);
    assert_eq!(
        plan(&request(None, None), &sights),
        Plan::Stop(BackfillSealingKeyReport::NeedsGenerator { candidate: id(4) })
    );
}

#[test]
fn no_holder_and_no_capable_machine_stops() {
    let sights = sights(vec![
        (1, with(needs(1), |seen| seen.can.generate = false)),
        (2, unknown()),
    ]);
    assert_eq!(
        plan(&request(None, None), &sights),
        Plan::Stop(BackfillSealingKeyReport::NoCandidate)
    );
}

#[test]
fn unlisted_generator_is_gone() {
    let sights = sights(vec![(1, needs(1))]);
    assert_eq!(
        plan(&request(None, Some(2)), &sights),
        Plan::Stop(BackfillSealingKeyReport::GeneratorGone { generator: id(2) })
    );
}

#[test]
fn generator_not_ready_waits() {
    for sight in [
        unknown(),
        with(needs(2), |seen| seen.can.generate = false),
        with(needs(2), |seen| seen.phase = LocalMachinePhase::Joining),
    ] {
        let sights = sights(vec![(1, needs(1)), (2, sight)]);
        assert_eq!(
            plan(&request(None, Some(2)), &sights),
            Plan::Stop(BackfillSealingKeyReport::Waiting { generator: id(2) })
        );
    }
}

#[test]
fn ready_generator_generates() {
    let sights = sights(vec![(1, needs(1)), (2, needs(2))]);
    assert_eq!(
        plan(&request(None, Some(2)), &sights),
        Plan::Generate { generator: id(2) }
    );
}

#[test]
fn a_holder_outranks_the_chosen_generator() {
    let sights = sights(vec![(1, holds(1, 7)), (2, needs(2))]);
    assert_eq!(
        plan(&request(None, Some(2)), &sights),
        Plan::Distribute {
            public_key: key(7),
            sources: vec![id(1)],
        }
    );
}

#[test]
fn distribution_sorts_every_machine_against_the_key() {
    let sights = sights(vec![
        (1, holds(1, 7)),
        (2, holds(2, 9)),
        (3, unknown()),
        (
            4,
            with(needs(4), |seen| seen.phase = LocalMachinePhase::Joining),
        ),
        (5, with(needs(5), |seen| seen.can.install = false)),
        (6, needs(6)),
        (7, needs(7)),
    ]);
    let row = |n, outcome| MachineBackfill {
        machine: id(n),
        outcome,
    };
    assert_eq!(
        distribution(key(7), &sights, Some(id(7))),
        Distribution {
            recipients: vec![(id(6), wg(6))],
            settled: vec![
                row(1, MachineOutcome::Held),
                row(2, MachineOutcome::Conflict { public_key: key(9) }),
                row(
                    3,
                    MachineOutcome::Unknown {
                        reason: "its peers do not see it up".into(),
                    }
                ),
                row(
                    4,
                    MachineOutcome::Skipped {
                        reason: SkipReason::NotParticipating,
                    }
                ),
                row(
                    5,
                    MachineOutcome::Skipped {
                        reason: SkipReason::CannotInstall,
                    }
                ),
                row(7, MachineOutcome::Generated),
            ],
        }
    );
}

#[derive(Debug, PartialEq)]
enum Call {
    Generate(MachineId),
    Share {
        holder: MachineId,
        recipient: MachineId,
        public_key: WireGuardPublicKey,
    },
    Install {
        recipient: MachineId,
        from: MachineId,
        sealed: SealedSealingKey,
        public_key: SealingPublicKey,
    },
}

#[test]
fn a_machine_awaiting_recovery_stops_generation_before_any_generator() {
    let sights = sights(vec![(1, needs(1)), (2, awaits(2, 7)), (3, needs(3))]);
    for generator in [None, Some(1)] {
        assert_eq!(
            plan(&request(None, generator), &sights),
            Plan::Stop(BackfillSealingKeyReport::AwaitsRecovery { machine: id(2) })
        );
    }
}

#[test]
fn a_holder_outranks_a_machine_awaiting_recovery() {
    let sights = sights(vec![(1, holds(1, 7)), (2, awaits(2, 7))]);
    let Plan::Distribute { public_key, .. } = plan(&request(None, None), &sights) else {
        panic!("a seen holder settles the key");
    };
    assert_eq!(public_key, key(7));
    assert_eq!(
        distribution(public_key, &sights, None).recipients,
        vec![(id(2), wg(2))]
    );
}

#[derive(Default)]
struct Fake {
    sights: BTreeMap<MachineId, Sight>,
    generate: Option<Result<SealingPublicKey, RpcError>>,
    share_fails: Vec<MachineId>,
    install_fails_from: Vec<MachineId>,
    calls: Vec<Call>,
}

fn sealed_by(holder: MachineId) -> SealedSealingKey {
    SealedSealingKey::from_bytes(holder.to_string().into_bytes())
}

impl BackfillClient for Fake {
    async fn list(&mut self) -> Result<Vec<Listed>, RpcError> {
        Ok(self
            .sights
            .keys()
            .map(|&id| Listed {
                id,
                invites_rpc: true,
            })
            .collect())
    }

    async fn observe(&mut self, listed: &[Listed]) -> BTreeMap<MachineId, Sight> {
        assert_eq!(listed.len(), self.sights.len());
        self.sights.clone()
    }

    async fn generate(&mut self, generator: MachineId) -> Result<SealingPublicKey, RpcError> {
        self.calls.push(Call::Generate(generator));
        self.generate.clone().expect("test set a generate outcome")
    }

    async fn share(
        &mut self,
        holder: MachineId,
        recipient: MachineId,
        public_key: WireGuardPublicKey,
    ) -> Result<SealedSealingKey, RpcError> {
        self.calls.push(Call::Share {
            holder,
            recipient,
            public_key,
        });
        if self.share_fails.contains(&holder) {
            return Err(failure("share refused"));
        }
        Ok(sealed_by(holder))
    }

    async fn install(
        &mut self,
        recipient: MachineId,
        from: MachineId,
        sealed: SealedSealingKey,
        public_key: SealingPublicKey,
    ) -> Result<(), RpcError> {
        self.calls.push(Call::Install {
            recipient,
            from,
            sealed,
            public_key,
        });
        if self.install_fails_from.contains(&from) {
            return Err(failure("install refused"));
        }
        Ok(())
    }
}

fn run(fake: &mut Fake, request: BackfillSealingKeyRequest) -> BackfillSealingKeyReport {
    execute(fake, &request)
        .now_or_never()
        .expect("the fake never waits")
        .expect("the fake always lists")
}

fn distributed(report: BackfillSealingKeyReport) -> (SealingPublicKey, Vec<MachineBackfill>) {
    let BackfillSealingKeyReport::Distributed {
        public_key,
        machines,
    } = report
    else {
        panic!("not distributed: {report:?}");
    };
    (public_key, machines)
}

#[test]
fn generates_then_carries_the_key_sealed_to_each_recipient() {
    let mut fake = Fake {
        sights: sights(vec![
            (1, needs(1)),
            (2, needs(2)),
            (3, with(needs(3), |seen| seen.can.install = false)),
        ]),
        generate: Some(Ok(key(7))),
        ..Fake::default()
    };
    let (public_key, machines) = distributed(run(&mut fake, request(None, Some(2))));
    assert_eq!(public_key, key(7));
    assert_eq!(
        machines,
        vec![
            MachineBackfill {
                machine: id(1),
                outcome: MachineOutcome::Installed { from: id(2) },
            },
            MachineBackfill {
                machine: id(2),
                outcome: MachineOutcome::Generated,
            },
            MachineBackfill {
                machine: id(3),
                outcome: MachineOutcome::Skipped {
                    reason: SkipReason::CannotInstall,
                },
            },
        ]
    );
    assert_eq!(
        fake.calls,
        vec![
            Call::Generate(id(2)),
            Call::Share {
                holder: id(2),
                recipient: id(1),
                public_key: wg(1),
            },
            Call::Install {
                recipient: id(1),
                from: id(2),
                sealed: sealed_by(id(2)),
                public_key: key(7),
            },
        ]
    );
}

#[test]
fn a_failing_source_hands_over_to_the_next() {
    let mut fake = Fake {
        sights: sights(vec![(1, holds(1, 7)), (2, holds(2, 7)), (3, needs(3))]),
        share_fails: vec![id(1)],
        ..Fake::default()
    };
    let (_, machines) = distributed(run(&mut fake, request(None, None)));
    assert_eq!(
        machines.last().map(|row| &row.outcome),
        Some(&MachineOutcome::Installed { from: id(2) })
    );
    assert!(matches!(
        fake.calls.as_slice(),
        [Call::Share { holder: a, .. }, Call::Share { holder: b, .. }, Call::Install { from: c, .. }]
            if *a == id(1) && *b == id(2) && *c == id(2)
    ));
}

#[test]
fn every_source_failing_reports_the_last_refusal() {
    let mut fake = Fake {
        sights: sights(vec![(1, holds(1, 7)), (2, holds(2, 7)), (3, needs(3))]),
        share_fails: vec![id(1)],
        install_fails_from: vec![id(2)],
        ..Fake::default()
    };
    let (_, machines) = distributed(run(&mut fake, request(None, None)));
    assert_eq!(
        machines.last().map(|row| &row.outcome),
        Some(&MachineOutcome::Failed {
            error: failure("install refused")
        })
    );
}

#[test]
fn no_sharing_holder_fails_each_recipient_without_an_rpc() {
    let mut fake = Fake {
        sights: sights(vec![
            (1, with(holds(1, 7), |seen| seen.can.share = false)),
            (2, needs(2)),
        ]),
        ..Fake::default()
    };
    let (_, machines) = distributed(run(&mut fake, request(None, None)));
    let Some(MachineOutcome::Failed { error }) = machines.last().map(|row| &row.outcome) else {
        panic!("not failed: {machines:?}");
    };
    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert!(fake.calls.is_empty());
}

#[test]
fn generator_awaiting_recovery_is_reported_as_such() {
    let mut fake = Fake {
        sights: sights(vec![(1, needs(1))]),
        generate: Some(Err(refusal(SealingRefusal::AwaitsRecovery))),
        ..Fake::default()
    };
    assert_eq!(
        run(&mut fake, request(None, Some(1))),
        BackfillSealingKeyReport::AwaitsRecovery { machine: id(1) }
    );
}

#[test]
fn a_named_generator_is_never_asked_while_another_machine_awaits_recovery() {
    let mut fake = Fake {
        sights: sights(vec![(1, needs(1)), (2, awaits(2, 7))]),
        generate: Some(Ok(key(9))),
        ..Fake::default()
    };
    assert_eq!(
        run(&mut fake, request(None, Some(1))),
        BackfillSealingKeyReport::AwaitsRecovery { machine: id(2) }
    );
    assert!(fake.calls.is_empty());
}

#[test]
fn any_other_generation_error_is_carried_in_the_report() {
    let error = refusal(SealingRefusal::Published);
    let mut fake = Fake {
        sights: sights(vec![(1, needs(1))]),
        generate: Some(Err(error.clone())),
        ..Fake::default()
    };
    assert_eq!(
        run(&mut fake, request(None, Some(1))),
        BackfillSealingKeyReport::GenerationFailed {
            generator: id(1),
            error,
        }
    );
}

#[test]
fn empty_request_means_nothing_expected_and_no_generator() {
    let request: BackfillSealingKeyRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(request, BackfillSealingKeyRequest::default());
}

#[test]
fn report_and_machine_rows_are_flat_on_result() {
    let report = BackfillSealingKeyReport::Distributed {
        public_key: key(7),
        machines: vec![
            MachineBackfill {
                machine: id(1),
                outcome: MachineOutcome::Installed { from: id(2) },
            },
            MachineBackfill {
                machine: id(3),
                outcome: MachineOutcome::Skipped {
                    reason: SkipReason::CannotInstall,
                },
            },
        ],
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({
            "result": "distributed",
            "public_key": key(7).to_string(),
            "machines": [
                { "machine": id(1).to_string(), "result": "installed", "from": id(2).to_string() },
                { "machine": id(3).to_string(), "result": "skipped", "reason": "cannot_install" },
            ],
        })
    );
    assert_eq!(
        serde_json::to_value(BackfillSealingKeyReport::NeedsGenerator { candidate: id(4) })
            .unwrap(),
        json!({ "result": "needs_generator", "candidate": id(4).to_string() })
    );
}

fn settled(fake: &mut Fake) -> Result<SealingPublicKey, Box<BackfillSealingKeyReport>> {
    settle(fake)
        .now_or_never()
        .expect("the fake never waits")
        .expect("the fake always lists")
}

#[test]
fn settling_a_cluster_without_a_key_generates_on_the_candidate_and_carries_it() {
    let mut fake = Fake {
        sights: sights(vec![(1, needs(1)), (2, needs(2))]),
        generate: Some(Ok(key(7))),
        ..Fake::default()
    };
    assert_eq!(settled(&mut fake), Ok(key(7)));
    assert!(matches!(fake.calls.first(), Some(Call::Generate(generator)) if *generator == id(1)));
    assert!(
        fake.calls
            .iter()
            .any(|call| matches!(call, Call::Install { recipient, .. } if *recipient == id(2)))
    );
}

#[test]
fn settling_a_cluster_that_holds_a_key_takes_it_and_generates_none() {
    let mut fake = Fake {
        sights: sights(vec![(1, holds(1, 4)), (2, needs(2))]),
        ..Fake::default()
    };
    assert_eq!(settled(&mut fake), Ok(key(4)));
    assert!(
        !fake
            .calls
            .iter()
            .any(|call| matches!(call, Call::Generate(_)))
    );
}

#[test]
fn settling_a_cluster_awaiting_recovery_makes_no_key() {
    let mut fake = Fake {
        sights: sights(vec![(1, awaits(1, 4)), (2, needs(2))]),
        ..Fake::default()
    };
    assert!(matches!(
        settled(&mut fake),
        Err(report) if matches!(*report, BackfillSealingKeyReport::AwaitsRecovery { .. })
    ));
    assert!(fake.calls.is_empty(), "{:?}", fake.calls);
}
