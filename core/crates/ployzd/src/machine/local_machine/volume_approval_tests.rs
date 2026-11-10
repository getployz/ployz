use std::path::Path;

use ployz_core::{
    CopyTransition, DeviceKey, MachineName, Namespace, ServiceName, SnapshotGuid,
    authority::{ApprovedService, PayloadDigest, Signer, SignerKey, validity::MAX_LIFETIME_MILLIS},
};

use super::*;
use crate::machine::{
    LocalMachineStore, RecordOwner, local_machine::restart_approval::ConsumptionLog,
};

fn id(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

fn data() -> DockerVolumeName {
    DockerVolumeName::parse("app_data").unwrap()
}

fn other_volume() -> DockerVolumeName {
    DockerVolumeName::parse("app_other").unwrap()
}

fn operation(fill: char) -> OperationId {
    OperationId::parse(fill.to_string().repeat(32)).unwrap()
}

fn digest(fill: char) -> StepDigest {
    StepDigest::of(&fill.to_string())
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn mirror(writer: char, mirror: char) -> VolumeWorkflow {
    VolumeWorkflow::Mirror {
        writer: id(writer),
        mirror: id(mirror),
        replace: false,
    }
}

fn moving(from: char, to: char, mirrors: &[char]) -> VolumeWorkflow {
    moving_from(id(from), to, mirrors)
}

fn moving_from(from: MachineId, to: char, mirrors: &[char]) -> VolumeWorkflow {
    VolumeWorkflow::Move {
        from,
        to: id(to),
        service: ApprovedService {
            name: ServiceName::parse("db").unwrap(),
            digest: PayloadDigest::parse(&"d".repeat(64)).unwrap(),
            image: None,
        },
        mirrors: mirrors.iter().map(|fill| id(*fill)).collect(),
    }
}

fn restore(machine: char) -> VolumeWorkflow {
    VolumeWorkflow::Restore {
        machine: id(machine),
        at: SnapshotGuid::new(7),
        loses_after: at(1),
    }
}

fn snapshot() -> VolumeStep {
    VolumeStep::Copy(CopyTransition::WriterToSnapshot)
}

fn pull() -> VolumeStep {
    VolumeStep::Copy(CopyTransition::MirrorToMirror)
}

fn name(value: &str) -> SnapshotName {
    value.parse().unwrap()
}

/// One Machine's view of the approvals log at `dir`, opened afresh for each call, so every
/// answer comes from what reached the disk.
struct Records<'a> {
    dir: &'a Path,
    machine: MachineId,
}

impl Records<'_> {
    fn log(&self) -> std::sync::Arc<ConsumptionLog> {
        ConsumptionLog::new(self.dir)
    }

    /// Admit `step` of operation `op` with `body` as its digest. `verified` stands in for the
    /// signed approval's check, which only a first step reaches.
    fn admit(
        &self,
        op: char,
        body: char,
        volume: &DockerVolumeName,
        step: &VolumeStep,
        verified: Result<VolumeWorkflow, AuthorityRefusal>,
    ) -> Result<(), Error> {
        self.log().change_volumes(at(5_000), |records, _| {
            records.admit(
                operation(op),
                digest(body),
                &self.machine,
                volume,
                step,
                || verified,
            )
        })
    }

    fn admit_new(&self, op: char, workflow: VolumeWorkflow, step: &VolumeStep) {
        self.admit(op, op, &data(), step, Ok(workflow)).unwrap();
    }

    /// A later step of an open operation, whose window has passed.
    fn admit_late(&self, op: char, step: &VolumeStep) -> Result<(), Error> {
        self.admit(op, op, &data(), step, Err(AuthorityRefusal::Expired))
    }

    fn grant(&self, op: char, recipient: char, scope: GrantScope) -> Result<(), Error> {
        self.log().change_volumes(at(5_000), |records, _| {
            records.grant(operation(op), id(recipient), scope)
        })
    }

    fn live(&self, volume: &DockerVolumeName, recipient: char, snapshot: &str) -> bool {
        self.log()
            .read_volumes(|records| records.live(volume, &id(recipient), &name(snapshot)))
            .unwrap()
    }

    fn withdraw(&self, volume: &DockerVolumeName, recipient: char) {
        self.withdraw_replacing(volume, recipient, None);
    }

    fn withdraw_replacing(
        &self,
        volume: &DockerVolumeName,
        recipient: char,
        replacing: Option<char>,
    ) {
        self.log()
            .change_volumes(at(5_000), |records, _| {
                records.withdraw(
                    &self.machine,
                    volume,
                    &id(recipient),
                    replacing.map(operation),
                );
                Ok(())
            })
            .unwrap();
    }

    fn end(&self, op: char) {
        self.log()
            .change_volumes(at(5_000), |records, _| {
                records.end(operation(op));
                Ok(())
            })
            .unwrap();
    }

    fn decision(&self, op: char) -> Option<Decision> {
        self.log()
            .read_volumes(|records| {
                records
                    .operations
                    .get(&operation(op))
                    .and_then(|record| record.decision)
            })
            .unwrap()
    }
}

fn replayed(result: Result<(), Error>, op: char) -> bool {
    matches!(
        result,
        Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed { operation: refused }))
            if refused == operation(op)
    )
}

fn mismatch(result: Result<(), Error>) -> Option<&'static str> {
    match result {
        Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch { field })) => Some(field),
        _ => None,
    }
}

#[test]
fn an_open_operation_finishes_after_its_window_and_an_ended_one_never_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Records {
        dir: dir.path(),
        machine: id('a'),
    };

    assert!(matches!(
        writer.admit(
            '1',
            '1',
            &data(),
            &snapshot(),
            Err(AuthorityRefusal::Expired)
        ),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Expired))
    ));
    assert!(
        !dir.path().join("restart-approvals.json").exists(),
        "a refused first step writes nothing"
    );

    writer.admit_new('1', mirror('a', 'b'), &snapshot());
    writer.admit_late('1', &snapshot()).unwrap();
    writer
        .log()
        .change_volumes(at(1), |records, _| {
            records.admit(
                operation('1'),
                digest('1'),
                &id('a'),
                &data(),
                &snapshot(),
                || unreachable!("an open operation is not checked against the clock"),
            )
        })
        .unwrap();
    writer.grant('1', 'b', GrantScope::Standing).unwrap();
    assert!(writer.live(&data(), 'b', "w-1-1"));

    writer.end('1');
    assert!(replayed(writer.admit_late('1', &snapshot()), '1'));
    assert!(replayed(
        writer.admit('1', '1', &data(), &snapshot(), Ok(mirror('a', 'b'))),
        '1'
    ));
    assert!(!writer.live(&data(), 'b', "w-1-1"));
    assert!(replayed(writer.grant('1', 'b', GrantScope::Standing), '1'));
}

#[test]
fn an_open_operation_still_admits_only_its_own_body_volume_and_graph() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    writer.admit_new('1', mirror('a', 'b'), &snapshot());
    assert_eq!(
        mismatch(writer.admit('1', '2', &data(), &snapshot(), Ok(mirror('a', 'b')))),
        Some("an operation")
    );
    assert_eq!(
        mismatch(writer.admit('1', '1', &other_volume(), &snapshot(), Ok(mirror('a', 'b')))),
        Some("a Volume")
    );
    assert_eq!(
        mismatch(writer.admit_late('1', &VolumeStep::Copy(CopyTransition::WriterToMirror))),
        Some("a Volume step")
    );
    assert_eq!(
        mismatch(writer.grant('1', 'c', GrantScope::Standing)),
        Some("a recipient")
    );
    assert_eq!(
        mismatch(writer.grant('9', 'b', GrantScope::Standing)),
        Some("an operation")
    );
}

#[test]
fn a_bounded_grant_serves_only_its_snapshots_and_ends_with_its_move() {
    let dir = tempfile::tempdir().unwrap();
    let source = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    source.admit_new('1', moving('a', 'b', &[]), &snapshot());
    assert_eq!(
        mismatch(source.grant('1', 'b', GrantScope::Standing)),
        Some("a recipient"),
        "a Move's target never holds every snapshot"
    );
    source
        .grant('1', 'b', GrantScope::Snapshots(vec![name("w-1-1")]))
        .unwrap();
    source
        .grant('1', 'b', GrantScope::Snapshots(vec![name("f-1")]))
        .unwrap();
    assert!(source.live(&data(), 'b', "w-1-1"));
    assert!(source.live(&data(), 'b', "f-1"));
    assert!(!source.live(&data(), 'b', "w-2-1"));
    assert!(!source.live(&data(), 'c', "f-1"));
    assert!(!source.live(&other_volume(), 'b', "f-1"));

    source.end('1');
    assert!(!source.live(&data(), 'b', "f-1"));
}

#[test]
fn a_withdrawal_ends_only_what_mirrored_that_volume_to_that_machine() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    writer.admit_new('1', mirror('a', 'b'), &snapshot());
    writer.admit_new('2', mirror('a', 'c'), &snapshot());
    writer
        .admit('3', '3', &other_volume(), &snapshot(), Ok(mirror('a', 'b')))
        .unwrap();
    for (op, recipient) in [('1', 'b'), ('2', 'c'), ('3', 'b')] {
        writer.grant(op, recipient, GrantScope::Standing).unwrap();
    }

    writer.withdraw(&data(), 'b');

    assert!(replayed(writer.admit_late('1', &snapshot()), '1'));
    assert!(replayed(
        writer.admit('1', '1', &data(), &snapshot(), Ok(mirror('a', 'b'))),
        '1'
    ));
    assert!(!writer.live(&data(), 'b', "w-1-1"));
    writer.admit_late('2', &snapshot()).unwrap();
    assert!(writer.live(&data(), 'c', "w-1-1"));
    writer
        .admit(
            '3',
            '3',
            &other_volume(),
            &snapshot(),
            Err(AuthorityRefusal::Expired),
        )
        .unwrap();
    assert!(writer.live(&other_volume(), 'b', "w-1-1"));

    writer.admit_new('4', mirror('a', 'b'), &snapshot());
    writer.grant('4', 'b', GrantScope::Standing).unwrap();
    assert!(writer.live(&data(), 'b', "w-1-1"));
}

#[test]
fn a_mirror_that_withdraws_itself_ends_its_own_pulls_and_a_followed_move() {
    let dir = tempfile::tempdir().unwrap();
    let removed = Records {
        dir: dir.path(),
        machine: id('c'),
    };
    removed.admit_new('1', mirror('a', 'c'), &pull());
    removed.withdraw(&data(), 'c');
    assert!(replayed(removed.admit_late('1', &pull()), '1'));

    removed.admit_new('2', moving('a', 'b', &['c']), &pull());
    removed.withdraw(&data(), 'c');
    assert!(replayed(removed.admit_late('2', &pull()), '2'));
}

#[test]
fn a_withdrawal_outranks_a_grant_under_a_move_admitted_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let source = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    source.admit_new('1', moving('a', 'b', &['c']), &snapshot());
    source.withdraw(&data(), 'c');

    source.admit_late('1', &snapshot()).unwrap();
    assert!(replayed(source.grant('1', 'c', GrantScope::Standing), '1'));
    source
        .grant('1', 'b', GrantScope::Snapshots(vec![name("f-1")]))
        .unwrap();

    source.admit_new('2', moving('a', 'b', &['c']), &snapshot());
    source.grant('2', 'c', GrantScope::Standing).unwrap();
}

#[test]
fn a_new_owner_of_the_copy_ends_the_old_one_and_a_source_without_a_decision_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let source = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    source.admit_new('1', moving('a', 'b', &[]), &snapshot());
    source
        .grant('1', 'b', GrantScope::Snapshots(vec![name("f-1")]))
        .unwrap();
    source.admit_new('2', mirror('e', 'a'), &pull());
    source.admit_new(
        '3',
        VolumeWorkflow::Resize {
            machines: vec![id('a')],
            maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(1_000_000_000).unwrap(),
            ),
            shrink: false,
        },
        &VolumeStep::Resize {
            maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(1_000_000_000).unwrap(),
            ),
            allow_shrink: false,
        },
    );
    assert!(replayed(source.admit_late('1', &snapshot()), '1'));
    assert_eq!(source.decision('1'), Some(Decision::Resumed));
    assert!(!source.live(&data(), 'b', "f-1"));

    source.admit_new(
        '4',
        restore('a'),
        &VolumeStep::Copy(CopyTransition::MirrorToWriter),
    );
    assert!(replayed(source.admit_late('2', &pull()), '2'));
    assert_eq!(source.decision('2'), None);
    source
        .admit_late(
            '3',
            &VolumeStep::Resize {
                maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(1_000_000_000).unwrap(),
                ),
                allow_shrink: false,
            },
        )
        .unwrap();

    let target = Records {
        dir: dir.path(),
        machine: id('b'),
    };
    target
        .admit(
            '5',
            '5',
            &other_volume(),
            &pull(),
            Ok(moving('a', 'b', &[])),
        )
        .unwrap();
    target
        .admit(
            '6',
            '6',
            &other_volume(),
            &VolumeStep::Copy(CopyTransition::MirrorToWriter),
            Ok(restore('b')),
        )
        .unwrap();
    assert!(replayed(
        target.admit(
            '5',
            '5',
            &other_volume(),
            &pull(),
            Err(AuthorityRefusal::Expired)
        ),
        '5'
    ));
    assert_eq!(target.decision('5'), None, "b is not the Move's source");
}

#[test]
fn records_written_before_this_format_existed_read_back_with_no_volume_records() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("restart-approvals.json"),
        br#"{"expiry_time_floor":1000}"#,
    )
    .unwrap();
    let writer = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    assert!(!writer.live(&data(), 'b', "w-1-1"));
    writer.admit_new('1', mirror('a', 'b'), &snapshot());
    let stored = std::fs::read_to_string(dir.path().join("restart-approvals.json")).unwrap();
    assert!(
        stored.contains(r#""volumes":{"sequence":1,"operations""#),
        "{stored}"
    );
}

struct Enforcing {
    machine: LocalMachine,
    signer: Signer,
    cluster: ployz_core::SealingPublicKey,
    data_dir: crate::test_dir::TestDir,
}

async fn enforcing() -> Enforcing {
    let sealing = crate::sealed_env::test_support::Cluster::new();
    let data_dir = crate::test_dir::TestDir::new("volume-approval");
    let mut store = LocalMachineStore::open(&data_dir.0).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    std::fs::copy(
        sealing.dir.0.join("sealing.key"),
        data_dir.0.join("sealing.key"),
    )
    .unwrap();
    let owner = RecordOwner::spawn(store).unwrap();
    let signer = Signer::from_seed(&[7; 32]);
    let device = DeviceKey::from(signer.public());
    owner
        .mutate(move |store| {
            store.approve_device(device).unwrap();
            store.set_device_enforcement(true, Vec::new()).unwrap();
        })
        .await
        .unwrap();
    let cluster = crate::sealing_key::SealingKey::load(&data_dir.0)
        .unwrap()
        .unwrap()
        .public_key();
    Enforcing {
        machine: LocalMachine::new(owner),
        signer,
        cluster,
        data_dir,
    }
}

impl Enforcing {
    /// A Move of `app_data` from this Machine to another, signed now by `signer`, open
    /// from `issued` milliseconds ago for `lifetime` milliseconds.
    fn approval(&self, signer: &Signer, issued: u64, lifetime: u64) -> Signed<VolumeApproval> {
        let issued_at = at(Timestamp::now().as_millis() - issued);
        let body = VolumeApproval {
            cluster: self.cluster,
            operation: operation('1'),
            namespace: Namespace::parse("app").unwrap(),
            volume: data(),
            workflow: moving_from(self.machine.record().id(), 'b', &[]),
            issued_at,
            expires_at: issued_at.plus(lifetime),
            signer: SignerKey::Device(DeviceKey::from(signer.public())),
        };
        Signed::sign(&body, signer).unwrap()
    }
}

#[tokio::test]
async fn a_device_approval_opens_the_operation_on_disk_and_a_late_or_foreign_one_is_refused() {
    let fixture = enforcing().await;
    let late = fixture.approval(
        &fixture.signer,
        MAX_LIFETIME_MILLIS + 600_000,
        MAX_LIFETIME_MILLIS,
    );
    assert!(matches!(
        fixture
            .machine
            .admit_volume_step(&late, &data(), &snapshot()),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Expired))
    ));
    let stranger = Signer::from_seed(&[3; 32]);
    assert!(matches!(
        fixture.machine.admit_volume_step(
            &fixture.approval(&stranger, 0, MAX_LIFETIME_MILLIS),
            &data(),
            &snapshot()
        ),
        Err(Error::VolumeStepRefused(
            AuthorityRefusal::UnknownKey { .. }
        ))
    ));

    let approval = fixture.approval(&fixture.signer, 0, MAX_LIFETIME_MILLIS);
    assert_eq!(
        fixture
            .machine
            .admit_volume_step(&approval, &data(), &snapshot())
            .unwrap(),
        operation('1')
    );
    assert!(matches!(
        fixture
            .machine
            .admit_volume_step(&approval, &data(), &pull()),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch {
            field: "a Volume step"
        }))
    ));
    fixture
        .machine
        .grant_volume(
            operation('1'),
            id('b'),
            GrantScope::Snapshots(vec![name("f-1")]),
        )
        .unwrap();
    assert!(
        fixture
            .machine
            .volume_grant_live(&data(), &id('b'), &name("f-1"))
            .unwrap()
    );

    fixture
        .machine
        .end_volume_operation(operation('1'))
        .unwrap();
    let reopened = ConsumptionLog::new(&fixture.data_dir.0);
    assert!(
        !reopened
            .read_volumes(|records| records.live(&data(), &id('b'), &name("f-1")))
            .unwrap()
    );
    assert!(matches!(
        reopened.change_volumes(Timestamp::now(), |records, _| {
            records.admit(
                operation('1'),
                StepDigest::of(&approval.body),
                &fixture.machine.record().id(),
                &data(),
                &snapshot(),
                || unreachable!("an ended operation is refused before any check"),
            )
        }),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed { .. }))
    ));
    assert!(matches!(
        fixture
            .machine
            .admit_volume_step(&approval, &data(), &snapshot()),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed { .. }))
    ));
}

#[tokio::test]
async fn a_withdrawal_by_this_machine_survives_a_reload() {
    let fixture = enforcing().await;
    let approval = fixture.approval(&fixture.signer, 0, MAX_LIFETIME_MILLIS);
    fixture
        .machine
        .admit_volume_step(&approval, &data(), &snapshot())
        .unwrap();
    fixture
        .machine
        .withdraw_volume(&data(), &id('b'), None)
        .unwrap();
    assert!(matches!(
        fixture.machine.grant_volume(
            operation('1'),
            id('b'),
            GrantScope::Snapshots(vec![name("f-1")])
        ),
        Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed { .. }))
    ));
    let reopened = ConsumptionLog::new(&fixture.data_dir.0);
    let withdrawn = reopened
        .read_volumes(|records| records.withdrawals.clone())
        .unwrap();
    assert_eq!(
        withdrawn,
        vec![Withdrawal {
            volume: data(),
            recipient: id('b'),
            at: 2,
            replacing: None,
        }]
    );
}

#[test]
fn a_later_owner_of_a_following_mirrors_copy_ends_the_move_there() {
    let dir = tempfile::tempdir().unwrap();
    let follower = Records {
        dir: dir.path(),
        machine: id('c'),
    };
    follower.admit_new('1', moving('a', 'b', &['c']), &pull());
    follower.admit_new('2', mirror('b', 'c'), &pull());

    assert!(replayed(follower.admit_late('1', &pull()), '1'));
    follower.admit_late('2', &pull()).unwrap();
}

#[test]
fn a_replacing_mirror_survives_the_withdrawal_its_own_forget_writes() {
    let dir = tempfile::tempdir().unwrap();
    let replaced = Records {
        dir: dir.path(),
        machine: id('b'),
    };
    let replacing = VolumeWorkflow::Mirror {
        writer: id('a'),
        mirror: id('b'),
        replace: true,
    };
    replaced.admit_new('1', mirror('a', 'b'), &pull());
    replaced.admit_new(
        '2',
        replacing.clone(),
        &VolumeStep::Copy(CopyTransition::MirrorToForget),
    );
    replaced.withdraw_replacing(&data(), 'b', Some('2'));

    assert!(replayed(replaced.admit_late('1', &pull()), '1'));
    replaced
        .admit_late('2', &VolumeStep::Copy(CopyTransition::NoneToMirror))
        .unwrap();

    let writer_dir = tempfile::tempdir().unwrap();
    let writer = Records {
        dir: writer_dir.path(),
        machine: id('a'),
    };
    writer.admit_new('1', mirror('a', 'b'), &snapshot());
    writer.grant('1', 'b', GrantScope::Standing).unwrap();
    writer.admit_new('2', replacing, &snapshot());
    writer.grant('2', 'b', GrantScope::Standing).unwrap();
    writer.withdraw_replacing(&data(), 'b', Some('2'));
    assert!(replayed(writer.admit_late('1', &snapshot()), '1'));
    assert!(replayed(writer.grant('1', 'b', GrantScope::Standing), '1'));
    writer.admit_late('2', &snapshot()).unwrap();
    assert!(writer.live(&data(), 'b', "w-1-1"));
    writer.grant('2', 'b', GrantScope::Standing).unwrap();
}

#[test]
fn a_replacement_spares_its_operation_only_from_its_own_withdrawal() {
    let dir = tempfile::tempdir().unwrap();
    let source = Records {
        dir: dir.path(),
        machine: id('a'),
    };
    source.admit_new('1', moving('a', 'b', &['c', 'e']), &snapshot());
    source.withdraw(&data(), 'c');
    source.withdraw_replacing(&data(), 'e', Some('1'));

    assert!(replayed(source.grant('1', 'c', GrantScope::Standing), '1'));
    source.grant('1', 'e', GrantScope::Standing).unwrap();
}
