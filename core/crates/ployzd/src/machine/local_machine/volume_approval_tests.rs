use std::path::Path;

use ployz_core::{
    CopyTransition, DeviceKey, MachineName, Namespace, ServiceName, SnapshotGuid,
    authority::{ApprovedService, PayloadDigest, Signer, SignerKey, validity::MAX_LIFETIME_MILLIS},
};

use ployz_decide::volume_records::{Decision, Withdrawal};

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
            records
                .admit(
                    operation(op),
                    digest(body),
                    &self.machine,
                    volume,
                    step,
                    || verified,
                )
                .map_err(Error::VolumeStepRefused)
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
            records
                .grant(operation(op), id(recipient), scope)
                .map_err(Error::VolumeStepRefused)
        })
    }

    fn live(&self, volume: &DockerVolumeName, recipient: char, snapshot: &str) -> bool {
        self.log()
            .read_volumes(|records| records.live(volume, &id(recipient), Some(&name(snapshot))))
            .unwrap()
    }

    fn withdraw(&self, volume: &DockerVolumeName, recipient: char) {
        self.log()
            .change_volumes(at(5_000), |records, _| {
                records.withdraw(&self.machine, volume, &id(recipient));
                Ok(())
            })
            .unwrap();
    }

    fn withdrawals(&self) -> Vec<Withdrawal> {
        self.log()
            .read_volumes(|records| records.withdrawals().to_vec())
            .unwrap()
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
            .read_volumes(|records| records.decision(operation(op)).cloned())
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
            records
                .admit(
                    operation('1'),
                    digest('1'),
                    &id('a'),
                    &data(),
                    &snapshot(),
                    || unreachable!("an open operation is not checked against the clock"),
                )
                .map_err(Error::VolumeStepRefused)
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
    enforcing_with(Vec::new()).await
}

/// An enforcing Machine that pins `starting` as the Cluster's other Members.
async fn enforcing_with(starting: Vec<ployz_core::authority::Member>) -> Enforcing {
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
            store.set_device_enforcement(true, starting).unwrap();
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

    /// `workflow` on `volume`, signed now by this Machine's Device.
    fn signed(&self, volume: DockerVolumeName, workflow: VolumeWorkflow) -> Signed<VolumeApproval> {
        let issued_at = Timestamp::now();
        let body = VolumeApproval {
            cluster: self.cluster,
            operation: operation('1'),
            namespace: Namespace::parse("app").unwrap(),
            volume,
            workflow,
            issued_at,
            expires_at: issued_at.plus(MAX_LIFETIME_MILLIS),
            signer: SignerKey::Device(DeviceKey::from(self.signer.public())),
        };
        Signed::sign(&body, &self.signer).unwrap()
    }

    /// The handoff receipt `from` signs with this Cluster's receipt key for `approval`'s
    /// Move to `to` at the cut snapshot `f-1`, held as guid `at`, taking epoch 2.
    fn receipt(
        &self,
        approval: &Signed<VolumeApproval>,
        from: MachineId,
        to: MachineId,
        at: u64,
    ) -> HandoffReceiptEnvelope {
        let key = crate::sealing_key::SealingKey::load(&self.data_dir.0)
            .unwrap()
            .unwrap();
        let body = approval.open().unwrap();
        let receipt = HandoffReceipt {
            cluster: key.public_key(),
            operation: body.operation,
            approval: StepDigest::of(&approval.body),
            volume: body.volume,
            from,
            to,
            at: SnapshotGuid::new(at),
            snapshot: name("f-1"),
            from_epoch: ployz_core::WriterEpoch::new(1),
            epoch: ployz_core::WriterEpoch::new(2),
        };
        MachineSigned::sign(&receipt, &key.receipt_signer()).unwrap()
    }

    fn records_empty(&self) -> bool {
        ConsumptionLog::new(&self.data_dir.0)
            .read_volumes(VolumeRecords::is_empty)
            .unwrap()
    }
}

#[tokio::test]
async fn only_a_replacing_mirror_of_this_volume_and_machine_withdraws_with_its_forget() {
    let fixture = enforcing().await;
    let here = fixture.machine.record().id();
    let mirror_here = |replace| VolumeWorkflow::Mirror {
        writer: id('a'),
        mirror: here,
        replace,
    };
    let refused = |volume: DockerVolumeName, workflow: VolumeWorkflow| match fixture
        .machine
        .admit_volume_step(&fixture.signed(volume, workflow), &data(), &forget())
    {
        Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch { field })) => Some(field),
        _ => None,
    };

    assert_eq!(refused(data(), mirror_here(false)), Some("a Volume step"));
    assert_eq!(
        refused(
            data(),
            VolumeWorkflow::Move {
                from: id('a'),
                to: id('b'),
                service: ApprovedService {
                    name: ServiceName::parse("db").unwrap(),
                    digest: PayloadDigest::parse(&"d".repeat(64)).unwrap(),
                    image: None,
                },
                mirrors: vec![here],
            }
        ),
        Some("a Volume step")
    );
    assert_eq!(refused(data(), replacing('a', 'c')), Some("this Server"));
    assert_eq!(refused(other_volume(), mirror_here(true)), Some("a Volume"));
    assert!(fixture.records_empty(), "a refused Forget writes nothing");

    let replacement = fixture.signed(data(), mirror_here(true));
    fixture
        .machine
        .admit_volume_step(&replacement, &data(), &forget())
        .unwrap();
    fixture
        .machine
        .admit_volume_step(&replacement, &data(), &forget())
        .unwrap();
    let withdrawals = ConsumptionLog::new(&fixture.data_dir.0)
        .read_volumes(|records| records.withdrawals().to_vec())
        .unwrap();
    assert_eq!(
        withdrawals,
        vec![Withdrawal {
            volume: data(),
            recipient: here,
            at: 1,
        }]
    );
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
            .volume_grant_live(&data(), &id('b'), Some(&name("f-1")))
            .unwrap()
    );

    fixture
        .machine
        .end_volume_operation(operation('1'))
        .unwrap();
    let reopened = ConsumptionLog::new(&fixture.data_dir.0);
    assert!(
        !reopened
            .read_volumes(|records| records.live(&data(), &id('b'), Some(&name("f-1"))))
            .unwrap()
    );
    assert!(matches!(
        reopened.change_volumes(Timestamp::now(), |records, _| {
            records
                .admit(
                    operation('1'),
                    StepDigest::of(&approval.body),
                    &fixture.machine.record().id(),
                    &data(),
                    &snapshot(),
                    || unreachable!("an ended operation is refused before any check"),
                )
                .map_err(Error::VolumeStepRefused)
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
    fixture.machine.withdraw_volume(&data(), &id('b')).unwrap();
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
        .read_volumes(|records| records.withdrawals().to_vec())
        .unwrap();
    assert_eq!(
        withdrawn,
        vec![Withdrawal {
            volume: data(),
            recipient: id('b'),
            at: 2,
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

fn replacing(writer: char, mirror: char) -> VolumeWorkflow {
    VolumeWorkflow::Mirror {
        writer: id(writer),
        mirror: id(mirror),
        replace: true,
    }
}

fn forget() -> VolumeStep {
    VolumeStep::Copy(CopyTransition::MirrorToForget)
}

#[test]
fn a_replacing_mirrors_opening_forget_withdraws_every_other_mirror_here_and_keeps_itself() {
    let dir = tempfile::tempdir().unwrap();
    let replaced = Records {
        dir: dir.path(),
        machine: id('b'),
    };
    replaced.admit_new('1', mirror('a', 'b'), &pull());
    replaced
        .admit('3', '3', &other_volume(), &pull(), Ok(mirror('a', 'b')))
        .unwrap();

    replaced.admit_new('2', replacing('a', 'b'), &forget());

    assert_eq!(
        replaced.withdrawals(),
        vec![Withdrawal {
            volume: data(),
            recipient: id('b'),
            at: 3,
        }]
    );
    assert!(replayed(replaced.admit_late('1', &pull()), '1'));
    replaced
        .admit_late('2', &VolumeStep::Copy(CopyTransition::NoneToMirror))
        .unwrap();
    replaced
        .admit(
            '3',
            '3',
            &other_volume(),
            &pull(),
            Err(AuthorityRefusal::Expired),
        )
        .unwrap();
}

#[test]
fn a_replacement_withdraws_once_and_a_later_withdrawal_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let replaced = Records {
        dir: dir.path(),
        machine: id('b'),
    };
    replaced.admit_new('2', replacing('a', 'b'), &forget());
    replaced.admit_late('2', &forget()).unwrap();
    assert_eq!(
        replaced.withdrawals().len(),
        1,
        "a resent Forget withdraws nothing"
    );
    replaced.admit_late('2', &pull()).unwrap();

    replaced.withdraw(&data(), 'b');
    assert!(replayed(replaced.admit_late('2', &pull()), '2'));
}

#[test]
fn a_replacement_opened_by_a_pull_withdraws_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mirror_machine = Records {
        dir: dir.path(),
        machine: id('b'),
    };
    mirror_machine.admit_new('2', replacing('a', 'b'), &pull());
    assert!(mirror_machine.withdrawals().is_empty());
    mirror_machine.admit_late('2', &pull()).unwrap();
}

/// A peer's row, as its own daemon publishes it.
fn peer(name: &str, seed: u8) -> ployz_core::Machine {
    ployz_core::Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::random(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        public_key: crate::network::WireGuardPrivateKey::from_bytes([seed; 32]).public_key(),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        join_approval: None,
        build_concurrency: None,
    }
}

fn address(machine: &ployz_core::Machine) -> std::net::IpAddr {
    std::net::IpAddr::V6(machine.management_address().0)
}

/// An enforcing Machine that pins web-2 and web-3, with their rows and web-4's in its
/// Cluster store. It never accepted web-4.
struct Peered {
    machine: LocalMachine,
    fixture: Enforcing,
    web2: ployz_core::Machine,
    web3: ployz_core::Machine,
    web4: ployz_core::Machine,
    _server: tokio::task::JoinHandle<()>,
}

async fn peered() -> Peered {
    let (web2, web3, web4) = (peer("web-2", 2), peer("web-3", 3), peer("web-4", 4));
    let fixture = enforcing_with(vec![(&web2).into(), (&web3).into()]).await;
    let (replicated, server) = crate::corrosion::fake_cluster::store().await;
    for row in [&web2, &web3, &web4] {
        replicated.publish_local_machine(row).await.unwrap();
    }
    let machine = LocalMachine::new(fixture.machine.owner().clone()).with_cluster(Some((
        replicated,
        crate::corrosion::AdminClient::new("/no/admin"),
    )));
    Peered {
        machine,
        fixture,
        web2,
        web3,
        web4,
        _server: server,
    }
}

fn refused<T>(result: Result<T, Error>) -> Option<AuthorityRefusal> {
    match result {
        Err(Error::VolumeStepRefused(refusal)) => Some(refusal),
        _ => None,
    }
}

#[tokio::test]
async fn the_send_stream_serves_only_an_accepted_machine_a_live_grant_covers() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let sends = |peer: std::net::IpAddr, snapshot: Option<&'static str>| {
        let machine = &peered.machine;
        async move {
            machine
                .sends_volume(peer, &data(), snapshot.map(name).as_ref())
                .await
                .unwrap()
        }
    };
    assert_eq!(
        sends(address(&peered.web2), Some("w-1-1")).await,
        Sends::Refused {
            recipient: Some(peered.web2.name.clone())
        },
        "no grant before a Mirror approval"
    );
    let approval = peered.fixture.signed(
        data(),
        VolumeWorkflow::Mirror {
            writer: this,
            mirror: peered.web2.id,
            replace: false,
        },
    );
    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Snapshot)
        .await
        .unwrap();
    assert_eq!(
        sends(address(&peered.web2), Some("w-1-1")).await,
        Sends::Granted(peered.web2.id)
    );
    assert_eq!(
        sends(address(&peered.web2), None).await,
        Sends::Granted(peered.web2.id),
        "the bound of a granted Volume"
    );
    assert!(matches!(
        peered
            .machine
            .sends_volume(address(&peered.web2), &other_volume(), None)
            .await
            .unwrap(),
        Sends::Refused { .. }
    ));
    assert_eq!(
        sends(address(&peered.web3), Some("w-1-1")).await,
        Sends::Refused {
            recipient: Some(peered.web3.name.clone())
        },
        "another accepted Machine"
    );
    let stranger: std::net::IpAddr = "fdcc::99".parse().unwrap();
    for peer in [
        address(&peered.web4),
        stranger,
        "10.210.2.1".parse().unwrap(),
    ] {
        assert_eq!(
            sends(peer, Some("w-1-1")).await,
            Sends::Refused { recipient: None },
            "{peer} is no Machine this one accepts"
        );
    }
    let withdrew = peered
        .machine
        .withdraw_volume(&data(), &peered.web2.id)
        .unwrap();
    assert!(withdrew);
    assert!(matches!(
        sends(address(&peered.web2), Some("w-1-1")).await,
        Sends::Refused { .. }
    ));
    assert_eq!(
        refused(
            peered
                .machine
                .admit_operator_copy(Some(&approval), &data(), CopyStep::Snapshot)
                .await
        ),
        Some(AuthorityRefusal::Replayed {
            operation: operation('1')
        }),
        "the old approval's snapshot step"
    );
}

#[tokio::test]
async fn a_machine_that_does_not_enforce_sends_to_any_peer() {
    let peered = peered().await;
    peered
        .fixture
        .machine
        .owner()
        .mutate(|store| store.set_device_enforcement(false, Vec::new()).unwrap())
        .await
        .unwrap();
    let stranger: std::net::IpAddr = "fdcc::99".parse().unwrap();
    assert_eq!(
        peered
            .machine
            .sends_volume(stranger, &data(), None)
            .await
            .unwrap(),
        Sends::Open
    );
    assert!(peered.fixture.records_empty());
}

/// An Operator's cut, which stops the Writer's Service, needs a Move approval under
/// enforcement; a promotion and a snapshot without one go on as before E4.
#[tokio::test]
async fn a_cut_needs_an_approval() {
    let fixture = enforcing().await;
    assert_eq!(
        refused(
            fixture
                .machine
                .admit_operator_copy(None, &data(), CopyStep::Cut)
                .await
        ),
        Some(AuthorityRefusal::Unsigned {
            purpose: ployz_core::authority::Purpose::Volume
        })
    );
    for step in [
        CopyStep::Snapshot,
        CopyStep::Promote {
            transition: CopyTransition::MirrorToWriter,
            at: &SnapshotGuid::new(7),
            epoch: ployz_core::WriterEpoch::new(2),
            receipt: None,
        },
    ] {
        assert!(matches!(
            fixture
                .machine
                .admit_operator_copy(None, &data(), step)
                .await,
            Ok(None)
        ));
    }
    assert!(fixture.records_empty());
}

#[tokio::test]
async fn a_pull_needs_an_approval_that_names_this_machine_and_its_source() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let target = name("w-1-1");
    let pull = |from| CopyStep::Pull {
        transition: CopyTransition::NoneToMirror,
        from,
        target: &target,
        receipt: None,
    };
    let web2 = peered.web2.management_address();
    let web3 = peered.web3.management_address();
    let admit = |approval: Option<Signed<VolumeApproval>>, from| {
        let machine = &peered.machine;
        async move {
            machine
                .admit_operator_copy(approval.as_ref(), &data(), pull(from))
                .await
        }
    };
    assert_eq!(
        refused(admit(None, &web2).await),
        Some(AuthorityRefusal::Unsigned {
            purpose: ployz_core::authority::Purpose::Volume
        })
    );
    let elsewhere = peered.fixture.signed(
        data(),
        VolumeWorkflow::Mirror {
            writer: peered.web2.id,
            mirror: peered.web3.id,
            replace: false,
        },
    );
    assert_eq!(
        refused(admit(Some(elsewhere), &web2).await),
        Some(AuthorityRefusal::Mismatch {
            field: "this Server"
        })
    );
    let approval = peered.fixture.signed(
        data(),
        VolumeWorkflow::Mirror {
            writer: peered.web2.id,
            mirror: this,
            replace: false,
        },
    );
    assert_eq!(
        refused(admit(Some(approval.clone()), &web3).await),
        Some(AuthorityRefusal::Mismatch {
            field: "the source Server"
        })
    );
    assert!(
        peered.fixture.records_empty(),
        "no refused pull opened anything"
    );
    admit(Some(approval.clone()), &web2).await.unwrap();
    admit(Some(approval.clone()), &web2)
        .await
        .expect("a resent pull under the same approval");
    assert!(peered.machine.withdraw_volume(&data(), &this).unwrap());
    let reloaded = LocalMachine::new(peered.fixture.machine.owner().clone()).with_cluster(Some((
        peered.machine.replicated().unwrap().clone(),
        crate::corrosion::AdminClient::new("/no/admin"),
    )));
    assert_eq!(
        refused(
            reloaded
                .admit_operator_copy(Some(&approval), &data(), pull(&web2))
                .await
        ),
        Some(AuthorityRefusal::Replayed {
            operation: operation('1')
        }),
        "a Forget withdrew this Mirror, so its old approval never pulls again"
    );
}

#[tokio::test]
async fn a_caught_up_mirror_admits_its_new_approval_and_passes_a_resend_without_one() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let web2 = peered.web2.management_address();
    let web3 = peered.web3.management_address();
    let held = |from| CopyStep::Held { from };
    peered
        .machine
        .admit_operator_copy(None, &data(), held(&web2))
        .await
        .expect("a resent pull at the held snapshot, as before");
    assert!(peered.fixture.records_empty());
    let approval = peered.fixture.signed(
        data(),
        VolumeWorkflow::Mirror {
            writer: peered.web2.id,
            mirror: this,
            replace: false,
        },
    );
    assert_eq!(
        refused(
            peered
                .machine
                .admit_operator_copy(Some(&approval), &data(), held(&web3))
                .await
        ),
        Some(AuthorityRefusal::Mismatch {
            field: "the source Server"
        })
    );
    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), held(&web2))
        .await
        .unwrap();
    assert!(
        !peered.fixture.records_empty(),
        "the new approval is admitted, so later refreshes pass after its window"
    );
}

fn container(fill: char) -> ContainerId {
    fill.to_string().repeat(64).parse().unwrap()
}

/// A's side of a Move to web-2: each warm snapshot and the cut grant web-2 only what A left
/// newest, the resume ends the Move here with its grants, and every later snapshot or cut
/// under it is refused, after a reload too.
#[tokio::test]
async fn a_move_grants_its_target_only_its_own_snapshots_and_ends_at_the_resume() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let target = address(&peered.web2);
    let sends = |snapshot: &'static str| {
        let machine = &peered.machine;
        async move {
            machine
                .sends_volume(target, &data(), Some(&name(snapshot)))
                .await
                .unwrap()
        }
    };
    let approval = peered.fixture.signed(
        data(),
        VolumeWorkflow::Move {
            from: this,
            to: peered.web2.id,
            service: ApprovedService {
                name: ServiceName::parse("db").unwrap(),
                digest: PayloadDigest::parse(&"d".repeat(64)).unwrap(),
                image: None,
            },
            mirrors: vec![peered.web3.id],
        },
    );
    let admitted = peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Snapshot)
        .await
        .unwrap()
        .expect("an enforcing Machine admits the warm snapshot");
    assert!(
        matches!(sends("w-1-1").await, Sends::Refused { .. }),
        "the snapshot is granted only once it exists"
    );
    peered
        .machine
        .grant_move_snapshot(&admitted, &name("w-1-1"))
        .unwrap();
    assert_eq!(sends("w-1-1").await, Sends::Granted(peered.web2.id));
    assert!(matches!(sends("w-2-1").await, Sends::Refused { .. }));
    assert!(
        matches!(
            peered
                .machine
                .sends_volume(address(&peered.web3), &data(), Some(&name("w-1-1")))
                .await
                .unwrap(),
            Sends::Refused { .. }
        ),
        "a Mirror that follows the Move gets nothing from its source"
    );

    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Cut)
        .await
        .unwrap();
    peered
        .machine
        .record_cut(
            operation('1'),
            (&name("f-1"), ployz_core::WriterEpoch::new(1)),
            &[container('c')],
        )
        .unwrap();
    peered
        .machine
        .record_cut(
            operation('1'),
            (&name("f-1"), ployz_core::WriterEpoch::new(1)),
            &[container('c')],
        )
        .expect("a resent cut");
    assert_eq!(
        refused(peered.machine.record_cut(
            operation('1'),
            (&name("f-2"), ployz_core::WriterEpoch::new(1)),
            &[container('c')]
        )),
        Some(AuthorityRefusal::Mismatch {
            field: "a second cut"
        })
    );
    assert_eq!(sends("f-1").await, Sends::Granted(peered.web2.id));
    assert_eq!(
        refused(
            peered
                .machine
                .admit_operator_copy(Some(&approval), &data(), CopyStep::Snapshot)
                .await
        ),
        Some(AuthorityRefusal::Mismatch {
            field: "a snapshot after the cut"
        })
    );

    let cut_at = SnapshotGuid::new(7);
    let resume = || CopyStep::Promote {
        transition: CopyTransition::MirrorToWriter,
        at: &cut_at,
        epoch: ployz_core::WriterEpoch::new(2),
        receipt: None,
    };
    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), resume())
        .await
        .unwrap();
    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), resume())
        .await
        .expect("a resent resume");
    let replayed = Some(AuthorityRefusal::Replayed {
        operation: operation('1'),
    });
    let reloaded = LocalMachine::new(peered.fixture.machine.owner().clone()).with_cluster(Some((
        peered.machine.replicated().unwrap().clone(),
        crate::corrosion::AdminClient::new("/no/admin"),
    )));
    for machine in [&peered.machine, &reloaded] {
        assert!(matches!(
            machine
                .sends_volume(target, &data(), Some(&name("f-1")))
                .await
                .unwrap(),
            Sends::Refused { .. }
        ));
        for step in [CopyStep::Snapshot, CopyStep::Cut] {
            assert_eq!(
                refused(
                    machine
                        .admit_operator_copy(Some(&approval), &data(), step)
                        .await
                ),
                replayed
            );
        }
        assert_eq!(
            refused(machine.record_cut(
                operation('1'),
                (&name("f-1"), ployz_core::WriterEpoch::new(1)),
                &[]
            )),
            replayed
        );
    }
}

/// B's side of a Move from web-2: its first step must fall inside the window, and its
/// promotion grants each Mirror the Move names every snapshot B takes from then on.
#[tokio::test]
async fn a_move_target_needs_the_window_and_its_promotion_grants_the_following_mirrors() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let web2 = peered.web2.management_address();
    let moving_here = |issued_at: Timestamp, lifetime| {
        let body = VolumeApproval {
            cluster: peered.fixture.cluster,
            operation: operation('1'),
            namespace: Namespace::parse("app").unwrap(),
            volume: data(),
            workflow: VolumeWorkflow::Move {
                from: peered.web2.id,
                to: this,
                service: ApprovedService {
                    name: ServiceName::parse("db").unwrap(),
                    digest: PayloadDigest::parse(&"d".repeat(64)).unwrap(),
                    image: None,
                },
                mirrors: vec![peered.web3.id],
            },
            issued_at,
            expires_at: issued_at.plus(lifetime),
            signer: SignerKey::Device(DeviceKey::from(peered.fixture.signer.public())),
        };
        Signed::sign(&body, &peered.fixture.signer).unwrap()
    };
    let target = name("f-1");
    let pull = |transition| CopyStep::Pull {
        transition,
        from: &web2,
        target: &target,
        receipt: None,
    };
    let lapsed = moving_here(
        at(Timestamp::now().as_millis() - MAX_LIFETIME_MILLIS - 600_000),
        MAX_LIFETIME_MILLIS,
    );
    assert_eq!(
        refused(
            peered
                .machine
                .admit_operator_copy(Some(&lapsed), &data(), pull(CopyTransition::NoneToMirror))
                .await
        ),
        Some(AuthorityRefusal::Expired)
    );
    assert!(peered.fixture.records_empty());

    let approval = moving_here(Timestamp::now(), MAX_LIFETIME_MILLIS);
    peered
        .machine
        .admit_operator_copy(Some(&approval), &data(), pull(CopyTransition::NoneToMirror))
        .await
        .unwrap();
    peered
        .machine
        .admit_operator_copy(
            Some(&approval),
            &data(),
            pull(CopyTransition::MirrorToMirror),
        )
        .await
        .unwrap();
    assert!(matches!(
        peered
            .machine
            .sends_volume(address(&peered.web3), &data(), Some(&name("w-1-1")))
            .await
            .unwrap(),
        Sends::Refused { .. }
    ));
    peered
        .machine
        .admit_operator_copy(
            Some(&approval),
            &data(),
            CopyStep::Promote {
                transition: CopyTransition::MirrorToWriter,
                at: &SnapshotGuid::new(9),
                epoch: ployz_core::WriterEpoch::new(2),
                receipt: Some(&peered.fixture.receipt(&approval, peered.web2.id, this, 9)),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        peered
            .machine
            .sends_volume(address(&peered.web3), &data(), Some(&name("w-1-1")))
            .await
            .unwrap(),
        Sends::Granted(peered.web3.id),
        "the Mirror that followed the Move now follows B"
    );
}

fn db() -> ApprovedService {
    ApprovedService {
        name: ServiceName::parse("db").unwrap(),
        digest: PayloadDigest::parse(&"d".repeat(64)).unwrap(),
        image: None,
    }
}

/// A Move of `data` from `from` to `to` under operation `op`, signed by `fixture`'s Device at
/// `issued_at` for the longest window.
fn move_signed(
    fixture: &Enforcing,
    op: char,
    (from, to): (MachineId, MachineId),
    issued_at: Timestamp,
) -> Signed<VolumeApproval> {
    let body = VolumeApproval {
        cluster: fixture.cluster,
        operation: operation(op),
        namespace: Namespace::parse("app").unwrap(),
        volume: data(),
        workflow: VolumeWorkflow::Move {
            from,
            to,
            service: db(),
            mirrors: Vec::new(),
        },
        issued_at,
        expires_at: issued_at.plus(MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(DeviceKey::from(fixture.signer.public())),
    };
    Signed::sign(&body, &fixture.signer).unwrap()
}

/// A change to a receipt's body before it is signed.
type Change<'a> = dyn Fn(&mut HandoffReceipt) + 'a;

fn promoting<'a>(
    at: &'a SnapshotGuid,
    receipt: Option<&'a HandoffReceiptEnvelope>,
) -> CopyStep<'a> {
    CopyStep::Promote {
        transition: CopyTransition::MirrorToWriter,
        at,
        epoch: epoch(2),
        receipt,
    }
}

fn lapsed() -> Timestamp {
    at(Timestamp::now().as_millis() - MAX_LIFETIME_MILLIS - 600_000)
}

fn epoch(value: u32) -> ployz_core::WriterEpoch {
    ployz_core::WriterEpoch::new(value)
}

fn cut_slot(guid: u64, snapshot: &str) -> ployz_core::VolumeCopy {
    ployz_core::VolumeCopy::Slot {
        slot: ployz_core::SlotId::new(1),
        readonly: true,
        newest: Some(ployz_core::Snapshot {
            name: name(snapshot),
            guid: SnapshotGuid::new(guid),
            created_unix_seconds: 1,
        }),
        resume_token: None,
        pull: None,
        role: Some(ployz_core::SlotRole::OldCopy),
        pull_state: None,
    }
}

/// A Move's source decides once. A handoff needs the cut, its snapshot and a higher epoch,
/// and is synced before its receipt leaves; a resend returns the same bytes, after a reload
/// too. From then on the source refuses its resume, any snapshot or cut, and a second
/// handoff, and serves its target the cut's snapshot and nothing else.
#[tokio::test]
async fn a_source_hands_off_once_and_never_resumes_after() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let web2 = peered.web2.id;
    let approval = move_signed(&peered.fixture, '1', (this, web2), Timestamp::now());
    let machine = &peered.machine;
    let hand = |to, snapshot: &str, wanted: u32| {
        machine.hand_off(
            &approval,
            &data(),
            to,
            (SnapshotGuid::new(9), &name(snapshot)),
            epoch(wanted),
        )
    };
    let admitted = machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Snapshot)
        .await
        .unwrap()
        .unwrap();
    machine
        .grant_move_snapshot(&admitted, &name("w-1-1"))
        .unwrap();
    assert_eq!(
        refused(hand(web2, "f-1", 2)),
        Some(AuthorityRefusal::Mismatch { field: "a cut" }),
        "no handoff before the cut"
    );
    machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Cut)
        .await
        .unwrap();
    machine
        .record_cut(operation('1'), (&name("f-1"), epoch(1)), &[container('c')])
        .unwrap();
    for (to, snapshot, wanted, field) in [
        (peered.web3.id, "f-1", 2, "a handoff"),
        (web2, "w-1-1", 2, "a snapshot"),
        (web2, "f-1", 1, "an epoch"),
    ] {
        assert_eq!(
            refused(hand(to, snapshot, wanted)),
            Some(AuthorityRefusal::Mismatch { field }),
            "{field}"
        );
    }

    let receipt = hand(web2, "f-1", 2).unwrap();
    let key = crate::sealing_key::SealingKey::load(&peered.fixture.data_dir.0)
        .unwrap()
        .unwrap();
    let body = receipt.verify(key.receipt_signer().public()).unwrap();
    assert_eq!(
        (
            body.from,
            body.to,
            body.at,
            body.snapshot.clone(),
            body.from_epoch,
            body.epoch
        ),
        (
            this,
            web2,
            SnapshotGuid::new(9),
            name("f-1"),
            epoch(1),
            epoch(2)
        )
    );
    assert_eq!(body.approval, StepDigest::of(&approval.body));
    assert_eq!(
        hand(web2, "f-1", 2).unwrap(),
        receipt,
        "a resend returns the same receipt"
    );
    assert_eq!(
        refused(hand(web2, "f-1", 3)),
        Some(AuthorityRefusal::Mismatch {
            field: "a second handoff"
        })
    );

    let reloaded = Records {
        dir: &peered.fixture.data_dir.0,
        machine: this,
    };
    assert!(matches!(
        reloaded.decision('1'),
        Some(Decision::Handed(handoff)) if handoff.to == web2 && handoff.epoch == epoch(2)
    ));
    let cut_at = SnapshotGuid::new(9);
    let resume = CopyStep::Promote {
        transition: CopyTransition::MirrorToWriter,
        at: &cut_at,
        epoch: epoch(3),
        receipt: None,
    };
    for step in [resume, CopyStep::Snapshot, CopyStep::Cut] {
        assert_eq!(
            refused(
                machine
                    .admit_operator_copy(Some(&approval), &data(), step)
                    .await
            ),
            Some(AuthorityRefusal::Replayed {
                operation: operation('1')
            }),
            "{step:?} after the handoff"
        );
    }
    assert_eq!(
        machine
            .handed_off(&data(), Some(&cut_slot(9, "f-1")))
            .unwrap(),
        Some(receipt),
        "a Writer want on the handed copy is refused with its receipt"
    );
    assert_eq!(
        machine
            .handed_off(&data(), Some(&cut_slot(10, "f-1")))
            .unwrap(),
        None
    );
    let sends = |peer, snapshot: &'static str| {
        let machine = &peered.machine;
        async move {
            machine
                .sends_volume(peer, &data(), Some(&name(snapshot)))
                .await
                .unwrap()
        }
    };
    assert_eq!(
        sends(address(&peered.web2), "f-1").await,
        Sends::Granted(web2)
    );
    assert!(matches!(
        sends(address(&peered.web2), "w-1-1").await,
        Sends::Refused { .. }
    ));
    assert!(matches!(
        sends(address(&peered.web3), "f-1").await,
        Sends::Refused { .. }
    ));
}

/// A resume first decides the other way: no handoff follows it.
#[tokio::test]
async fn a_source_that_resumed_never_hands_off() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let approval = move_signed(
        &peered.fixture,
        '1',
        (this, peered.web2.id),
        Timestamp::now(),
    );
    let machine = &peered.machine;
    machine
        .admit_operator_copy(Some(&approval), &data(), CopyStep::Cut)
        .await
        .unwrap();
    machine
        .record_cut(operation('1'), (&name("f-1"), epoch(1)), &[container('c')])
        .unwrap();
    let cut_at = SnapshotGuid::new(9);
    machine
        .admit_operator_copy(
            Some(&approval),
            &data(),
            CopyStep::Promote {
                transition: CopyTransition::MirrorToWriter,
                at: &cut_at,
                epoch: epoch(3),
                receipt: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        refused(machine.hand_off(
            &approval,
            &data(),
            peered.web2.id,
            (cut_at, &name("f-1")),
            epoch(2)
        )),
        Some(AuthorityRefusal::Replayed {
            operation: operation('1')
        })
    );
    assert_eq!(
        machine
            .handed_off(&data(), Some(&cut_slot(9, "f-1")))
            .unwrap(),
        None
    );
}

/// A Move's target becomes the Writer only on its source's receipt for this operation, this
/// Server, this snapshot and epoch, signed with the key this Cluster derives. The receipt
/// admits its first steps after the window, and once it promoted only a resend of the
/// promotion runs, after a reload too.
#[tokio::test]
async fn a_target_promotes_only_on_its_sources_receipt() {
    let peered = peered().await;
    let this = peered.machine.record().id();
    let web2 = peered.web2.id;
    let approval = move_signed(&peered.fixture, '1', (web2, this), lapsed());
    let machine = &peered.machine;
    let key = crate::sealing_key::SealingKey::load(&peered.fixture.data_dir.0)
        .unwrap()
        .unwrap();
    let signed = |change: &Change<'_>, signer: &ployz_core::authority::Signer| {
        let mut receipt = HandoffReceipt {
            cluster: key.public_key(),
            operation: operation('1'),
            approval: StepDigest::of(&approval.body),
            volume: data(),
            from: web2,
            to: this,
            at: SnapshotGuid::new(9),
            snapshot: name("f-1"),
            from_epoch: epoch(1),
            epoch: epoch(2),
        };
        change(&mut receipt);
        MachineSigned::sign(&receipt, signer).unwrap()
    };
    let cluster_key = key.receipt_signer();
    let receipt = signed(&|_| {}, &cluster_key);
    let cut_at = SnapshotGuid::new(9);
    let promote = |receipt| promoting(&cut_at, receipt);
    let target = name("f-1");
    let web2_address = peered.web2.management_address();
    let pull_from = |receipt| CopyStep::Pull {
        transition: CopyTransition::NoneToMirror,
        from: &web2_address,
        target: &target,
        receipt,
    };

    assert_eq!(
        refused(
            machine
                .admit_operator_copy(Some(&approval), &data(), promote(None))
                .await
        ),
        Some(AuthorityRefusal::Mismatch {
            field: "a handoff receipt"
        })
    );
    assert_eq!(
        refused(
            machine
                .admit_operator_copy(Some(&approval), &data(), pull_from(None))
                .await
        ),
        Some(AuthorityRefusal::Expired),
        "with no receipt the window has passed"
    );
    let other_operation = move_signed(&peered.fixture, '2', (web2, this), lapsed());
    let foreign = crate::sealing_key::SealingKey::generate().receipt_signer();
    let wrong: [(&Change<'_>, AuthorityRefusal); 6] = [
        (
            &|receipt| receipt.operation = operation('2'),
            AuthorityRefusal::Mismatch {
                field: "an operation",
            },
        ),
        (
            &|receipt| receipt.approval = StepDigest::of(&other_operation.body),
            AuthorityRefusal::Mismatch {
                field: "an approval",
            },
        ),
        (
            &|receipt| receipt.to = peered.web3.id,
            AuthorityRefusal::Mismatch {
                field: "this Server",
            },
        ),
        (
            &|receipt| receipt.at = SnapshotGuid::new(8),
            AuthorityRefusal::Mismatch {
                field: "a snapshot",
            },
        ),
        (
            &|receipt| receipt.epoch = epoch(3),
            AuthorityRefusal::Mismatch { field: "an epoch" },
        ),
        (
            &|receipt| receipt.volume = other_volume(),
            AuthorityRefusal::Mismatch { field: "a Volume" },
        ),
    ];
    for (change, refusal) in wrong {
        let receipt = signed(change, &cluster_key);
        assert_eq!(
            refused(
                machine
                    .admit_operator_copy(
                        Some(&approval),
                        &data(),
                        promoting(&cut_at, Some(&receipt))
                    )
                    .await
            ),
            Some(refusal)
        );
    }
    let forged = signed(&|_| {}, &foreign);
    assert_eq!(
        refused(
            machine
                .admit_operator_copy(Some(&approval), &data(), promote(Some(&forged)))
                .await
        ),
        Some(AuthorityRefusal::ForeignMachineKey {
            kind: "volume-handoff"
        })
    );
    let mut tampered = receipt.clone();
    tampered.signature[0] ^= 1;
    assert_eq!(
        refused(
            machine
                .admit_operator_copy(Some(&approval), &data(), promote(Some(&tampered)))
                .await
        ),
        Some(AuthorityRefusal::BadMachineSignature {
            kind: "volume-handoff"
        })
    );
    assert!(
        peered.fixture.records_empty(),
        "no refused receipt left a record"
    );

    machine
        .admit_operator_copy(Some(&approval), &data(), pull_from(Some(&receipt)))
        .await
        .expect("the receipt admits the pull after the window");
    machine
        .admit_operator_copy(Some(&approval), &data(), promote(Some(&receipt)))
        .await
        .expect("and the promotion");
    machine
        .admit_operator_copy(Some(&approval), &data(), promote(Some(&receipt)))
        .await
        .expect("a resend of the promotion");
    let reloaded = Records {
        dir: &peered.fixture.data_dir.0,
        machine: this,
    };
    assert!(
        reloaded
            .log()
            .read_volumes(|records| records.promoted(operation('1')))
            .unwrap()
    );
    assert_eq!(
        refused(
            machine
                .admit_operator_copy(Some(&approval), &data(), pull_from(Some(&receipt)))
                .await
        ),
        Some(AuthorityRefusal::Replayed {
            operation: operation('1')
        }),
        "the Move ended here at the promotion"
    );
    let later = SnapshotGuid::new(10);
    assert_eq!(
        refused(
            machine
                .admit_operator_copy(
                    Some(&approval),
                    &data(),
                    CopyStep::Promote {
                        transition: CopyTransition::WriterToWriter,
                        at: &later,
                        epoch: epoch(2),
                        receipt: Some(&receipt),
                    },
                )
                .await
        ),
        Some(AuthorityRefusal::Mismatch {
            field: "a snapshot"
        }),
        "the receipt names one snapshot"
    );
}

/// Every step of a Move, as Cloud could replay it at either end.
fn every_move_step() -> Vec<VolumeStep> {
    let mut steps: Vec<VolumeStep> = [
        CopyTransition::WriterToSnapshot,
        CopyTransition::WriterToMirror,
        CopyTransition::WriterToWriter,
        CopyTransition::MirrorToWriter,
        CopyTransition::MirrorToMirror,
        CopyTransition::NoneToMirror,
    ]
    .into_iter()
    .map(VolumeStep::Copy)
    .collect();
    steps.extend([VolumeStep::Start, VolumeStep::Handoff]);
    steps
}

/// After a Move ends, by a handoff and its promotion or by a resume, each Machine reads its
/// decision back from disk on every step. A replayed step is refused, or is the resend of
/// the step that ended the Move there and changes nothing; A serves B the cut's snapshot
/// after a handoff and nothing after a resume.
#[test]
fn a_move_replayed_after_it_ended_changes_nothing_on_either_end() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = Records {
        dir: a_dir.path(),
        machine: id('a'),
    };
    let b = Records {
        dir: b_dir.path(),
        machine: id('b'),
    };
    let cut = CopyTransition::WriterToMirror;
    let (at, epoch2) = (SnapshotGuid::new(9), ployz_core::WriterEpoch::new(2));
    a.admit_new('1', moving('a', 'b', &[]), &snapshot());
    a.grant('1', 'b', GrantScope::Snapshots(vec![name("w-1-1")]))
        .unwrap();
    a.admit_late('1', &VolumeStep::Copy(cut)).unwrap();
    let change =
        |records: &Records<'_>,
         step: &dyn Fn(&mut VolumeRecords) -> Result<(), AuthorityRefusal>| {
            records.log().change_volumes(at_now(), |volumes, _| {
                step(volumes).map_err(Error::VolumeStepRefused)
            })
        };
    change(&a, &|volumes| {
        volumes.cut(
            operation('1'),
            &id('a'),
            &name("f-1"),
            ployz_core::WriterEpoch::new(1),
            &[],
        )
    })
    .unwrap();
    let hand = || {
        a.log().change_volumes(at_now(), |volumes, _| {
            volumes
                .handoff(
                    operation('1'),
                    &id('a'),
                    id('b'),
                    (at, &name("f-1")),
                    epoch2,
                )
                .map_err(Error::VolumeStepRefused)
        })
    };
    let handed = hand().unwrap();
    let receipt = MachineSigned::sign(
        &HandoffReceipt {
            cluster: ployz_core::SealingPublicKey::from_bytes([9; 32]),
            operation: operation('1'),
            approval: digest('1'),
            volume: data(),
            from: id('a'),
            to: id('b'),
            at,
            snapshot: name("f-1"),
            from_epoch: handed.handoff.from_epoch,
            epoch: epoch2,
        },
        &Signer::from_seed(&[7; 32]),
    )
    .unwrap();
    b.admit_new('1', moving('a', 'b', &[]), &pull());
    let promote = |receipt: &HandoffReceiptEnvelope| {
        change(&b, &|volumes| {
            volumes.promote(operation('1'), &id('b'), (at, epoch2), receipt.clone())
        })
    };
    promote(&receipt).unwrap();

    for step in every_move_step() {
        let resend = step == VolumeStep::Handoff;
        let on_a = a.admit_late('1', &step);
        assert_eq!(on_a.is_ok(), resend, "{step:?} at A after the handoff");
        if !resend {
            assert!(replayed(on_a, '1'), "{step:?} at A");
        }
        let writer = matches!(
            step,
            VolumeStep::Copy(CopyTransition::WriterToWriter | CopyTransition::MirrorToWriter)
                | VolumeStep::Start
        );
        let on_b = b.admit_late('1', &step);
        assert_eq!(on_b.is_ok(), writer, "{step:?} at B after its promotion");
        if !writer {
            assert!(replayed(on_b, '1'), "{step:?} at B");
        }
    }
    assert_eq!(
        hand().unwrap(),
        handed,
        "a resent handoff returns the same one"
    );
    promote(&receipt).expect("a resent promotion is its recorded outcome");
    assert_eq!(
        b.log()
            .change_volumes(at_now(), |volumes, _| {
                volumes
                    .promote(
                        operation('1'),
                        &id('b'),
                        (SnapshotGuid::new(10), epoch2),
                        receipt.clone(),
                    )
                    .map_err(Error::VolumeStepRefused)
            })
            .map_err(|error| refused::<()>(Err(error))),
        Err(Some(AuthorityRefusal::Mismatch {
            field: "a second promotion"
        }))
    );
    assert!(a.live(&data(), 'b', "f-1"), "A serves B the cut's snapshot");
    assert!(!a.live(&data(), 'b', "w-1-1"), "and nothing else");
    assert!(!a.live(&data(), 'c', "f-1"));
    assert!(matches!(a.decision('1'), Some(Decision::Handed(_))));
    let resume = change(&a, &|volumes| volumes.resume(operation('1')));
    assert!(replayed(resume, '1'), "A never resumes after its handoff");

    a.admit_new('2', moving('a', 'b', &[]), &VolumeStep::Copy(cut));
    change(&a, &|volumes| {
        volumes.cut(
            operation('2'),
            &id('a'),
            &name("f-2"),
            ployz_core::WriterEpoch::new(2),
            &[],
        )
    })
    .unwrap();
    change(&a, &|volumes| volumes.resume(operation('2'))).unwrap();
    for step in every_move_step() {
        let resend = matches!(
            step,
            VolumeStep::Copy(CopyTransition::WriterToWriter | CopyTransition::MirrorToWriter)
                | VolumeStep::Start
        );
        assert_eq!(
            a.admit_late('2', &step).is_ok(),
            resend,
            "{step:?} at A after its resume"
        );
    }
    let handoff = a.log().change_volumes(at_now(), |volumes, _| {
        volumes
            .handoff(
                operation('2'),
                &id('a'),
                id('b'),
                (SnapshotGuid::new(11), &name("f-2")),
                ployz_core::WriterEpoch::new(3),
            )
            .map(|_| ())
            .map_err(Error::VolumeStepRefused)
    });
    assert!(replayed(handoff, '2'), "no handoff after a resume");
    assert!(
        !a.live(&data(), 'b', "f-2"),
        "A serves B nothing after a resume"
    );
}

fn at_now() -> Timestamp {
    at(5_000)
}
