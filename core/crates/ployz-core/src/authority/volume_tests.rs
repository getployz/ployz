use std::num::NonZeroU64;

use super::*;
use crate::{
    DeviceKey, ServiceName,
    authority::{
        canonical,
        deploy::PayloadDigest,
        keys::Signer,
        proposal::{ProposalApproval, ProposalStep, StepKind},
        session::{ActionFamily, Delegation, GrantBody, SessionGrant},
        trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
    },
};

const OPERATION: &str = "0f8fad5bd9cb469fa16570867728950e";

fn signer() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn id(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

fn a() -> MachineId {
    id('a')
}

fn b() -> MachineId {
    id('b')
}

fn c() -> MachineId {
    id('c')
}

fn volume() -> DockerVolumeName {
    DockerVolumeName::parse("app_data").unwrap()
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([9; 32])
}

fn bytes(gb: u64) -> ProvisionedVolumeMaximumBytes {
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(gb * 1_000_000_000).unwrap())
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn moving() -> VolumeWorkflow {
    VolumeWorkflow::Move {
        from: a(),
        to: b(),
        service: ApprovedService {
            name: ServiceName::parse("db").unwrap(),
            digest: PayloadDigest::try_from("d".repeat(64)).unwrap(),
            image: None,
        },
        mirrors: vec![c()],
    }
}

fn workflows() -> [VolumeWorkflow; 6] {
    [
        moving(),
        VolumeWorkflow::Mirror {
            writer: a(),
            mirror: b(),
            replace: false,
        },
        VolumeWorkflow::MirrorRemoval {
            mirror: b(),
            writer: Some(a()),
            role: SlotRole::Mirror,
        },
        VolumeWorkflow::Restore {
            machine: b(),
            at: SnapshotGuid::new(18_446_744_073_709_551_615),
            loses_after: at(500_000),
        },
        VolumeWorkflow::Resize {
            machines: vec![a(), b()],
            maximum_bytes: bytes(20),
            shrink: false,
        },
        VolumeWorkflow::Seed {
            machine: a(),
            source: DockerVolumeName::parse("app_old").unwrap(),
            at: SnapshotGuid::new(42),
        },
    ]
}

fn approval(workflow: VolumeWorkflow) -> VolumeApproval {
    VolumeApproval {
        cluster: cluster(),
        operation: OperationId::parse(OPERATION).unwrap(),
        namespace: Namespace::parse("app").unwrap(),
        volume: volume(),
        workflow,
        issued_at: at(100_000),
        expires_at: at(1_000_000),
        signer: SignerKey::Device(DeviceKey::from(signer().public())),
    }
}

fn trusted() -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from(signer().public())],
        passkeys: Vec::new(),
    }
}

fn check(
    signed: &Signed<VolumeApproval>,
    machine: &MachineId,
    step: &VolumeStep,
    now: u64,
) -> Result<VolumeApproved, AuthorityRefusal> {
    verify(
        signed,
        &VolumeFacts {
            cluster: Some(cluster()),
            machine,
            trusted: &trusted(),
            volume: &volume(),
            step,
            now: at(now),
            handed: None,
        },
    )
}

fn cut() -> VolumeStep {
    VolumeStep::Copy(CopyTransition::WriterToMirror)
}

fn mismatch(field: &'static str) -> Result<VolumeApproved, AuthorityRefusal> {
    Err(AuthorityRefusal::Mismatch { field })
}

#[test]
fn each_workflow_signs_its_golden_canonical_body() {
    let head = r#"{"cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":1000000,"issued_at":100000,"namespace":"app","operation":"0f8fad5bd9cb469fa16570867728950e","signer":"ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c","volume":"app_data","workflow":"#;
    let (a, b, c) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
    let d = "d".repeat(64);
    let golden = [
        format!(
            r#"{{"from":"{a}","kind":"move","mirrors":["{c}"],"service":{{"digest":"{d}","name":"db"}},"to":"{b}"}}"#
        ),
        format!(r#"{{"kind":"mirror","mirror":"{b}","replace":false,"writer":"{a}"}}"#),
        format!(r#"{{"kind":"mirror_removal","mirror":"{b}","role":"mirror","writer":"{a}"}}"#),
        format!(
            r#"{{"at":"18446744073709551615","kind":"restore","loses_after":500000,"machine":"{b}"}}"#
        ),
        format!(
            r#"{{"kind":"resize","machines":["{a}","{b}"],"maximum_bytes":20000000000,"shrink":false}}"#
        ),
        format!(r#"{{"at":"42","kind":"seed","machine":"{a}","source":"app_old"}}"#),
    ];
    for (workflow, golden) in workflows().into_iter().zip(golden) {
        let signed = Signed::sign(&approval(workflow.clone()), &signer()).unwrap();
        assert_eq!(signed.body, format!("{head}{golden}}}"), "{workflow:?}");
        assert_eq!(signed.open().unwrap(), approval(workflow));
        assert!(
            signed
                .signing_input()
                .starts_with(b"ployz-authority/v1\nvolume\n")
        );
    }
}

#[test]
fn a_device_approval_admits_each_step_its_graph_names_on_each_machine() {
    use CopyTransition::*;
    let moved = Signed::sign(&approval(moving()), &signer()).unwrap();
    for (machine, step) in [
        (a(), VolumeStep::Copy(WriterToSnapshot)),
        (a(), cut()),
        (a(), VolumeStep::Copy(WriterToWriter)),
        (a(), VolumeStep::Copy(MirrorToWriter)),
        (a(), VolumeStep::Start),
        (b(), VolumeStep::Copy(NoneToMirror)),
        (b(), VolumeStep::Copy(MirrorToMirror)),
        (b(), VolumeStep::Copy(MirrorToWriter)),
        (b(), VolumeStep::Copy(WriterToWriter)),
        (b(), VolumeStep::Start),
        (c(), VolumeStep::Copy(MirrorToMirror)),
    ] {
        let approved = check(&moved, &machine, &step, 200_000).unwrap();
        assert_eq!(approved.by, signer().public());
        assert_eq!(approved.digest, StepDigest::of(&moved.body));
    }
    for (machine, step) in [
        (a(), VolumeStep::Copy(NoneToMirror)),
        (a(), VolumeStep::Copy(WriterToForget)),
        (b(), cut()),
        (b(), VolumeStep::Copy(MirrorToForget)),
        (c(), VolumeStep::Copy(MirrorToWriter)),
        (c(), VolumeStep::Start),
        (
            a(),
            VolumeStep::Resize {
                maximum_bytes: bytes(20),
                allow_shrink: false,
            },
        ),
    ] {
        assert_eq!(
            check(&moved, &machine, &step, 200_000),
            mismatch("a Volume step"),
            "{machine} {step:?}"
        );
    }
}

#[test]
fn the_other_workflows_permit_only_their_own_steps() {
    use CopyTransition::*;
    let [_, mirror, removal, restore, resize, seed] = workflows();
    let shrink = |allow_shrink| VolumeStep::Resize {
        maximum_bytes: bytes(20),
        allow_shrink,
    };
    let seeding = |source: &str, guid| VolumeStep::Seed {
        source: DockerVolumeName::parse(source).unwrap(),
        at: SnapshotGuid::new(guid),
    };
    for (workflow, machine, step, permitted) in [
        (&mirror, a(), VolumeStep::Copy(WriterToSnapshot), true),
        (&mirror, b(), VolumeStep::Copy(NoneToMirror), true),
        (&mirror, b(), VolumeStep::Copy(MirrorToMirror), true),
        (&mirror, b(), VolumeStep::Copy(MirrorToForget), false),
        (&mirror, b(), VolumeStep::Copy(MirrorToWriter), false),
        (&mirror, a(), cut(), false),
        (&mirror, b(), VolumeStep::Start, false),
        (&removal, b(), VolumeStep::Copy(MirrorToForget), true),
        (&removal, a(), VolumeStep::Copy(WriterToForget), false),
        (&restore, b(), VolumeStep::Copy(MirrorToWriter), true),
        (&restore, b(), VolumeStep::Start, false),
        (&resize, a(), shrink(false), true),
        (&resize, b(), shrink(true), false),
        (&resize, c(), shrink(false), false),
        (&seed, a(), seeding("app_old", 42), true),
        (&seed, a(), seeding("other", 42), false),
        (&seed, a(), seeding("app_old", 43), false),
    ] {
        assert_eq!(
            workflow.permits(&machine, &step),
            permitted,
            "{workflow:?} {machine} {step:?}"
        );
    }
    let replacing = VolumeWorkflow::Mirror {
        writer: a(),
        mirror: b(),
        replace: true,
    };
    assert!(replacing.permits(&b(), &VolumeStep::Copy(MirrorToForget)));
    let resize_other = VolumeStep::Resize {
        maximum_bytes: bytes(30),
        allow_shrink: false,
    };
    assert!(!resize.permits(&a(), &resize_other));
    let shrinking = VolumeWorkflow::Resize {
        machines: vec![a(), b()],
        maximum_bytes: bytes(20),
        shrink: true,
    };
    assert!(shrinking.permits(&a(), &shrink(true)));
    assert!(
        !shrinking.permits(&a(), &shrink(false)),
        "a shrink's approval names its direction, so a request without allow_shrink is another change"
    );
}

#[test]
fn each_refusal_names_what_the_approval_lacks() {
    let signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    let other_volume = DockerVolumeName::parse("app_other").unwrap();
    assert_eq!(
        verify(
            &signed,
            &VolumeFacts {
                cluster: Some(cluster()),
                machine: &a(),
                trusted: &trusted(),
                volume: &other_volume,
                step: &cut(),
                now: at(200_000),
                handed: None,
            },
        ),
        mismatch("a Volume")
    );
    assert_eq!(
        verify(
            &signed,
            &VolumeFacts {
                cluster: Some(SealingPublicKey::from_bytes([8; 32])),
                machine: &a(),
                trusted: &trusted(),
                volume: &volume(),
                step: &cut(),
                now: at(200_000),
                handed: None,
            },
        ),
        mismatch("a Cluster")
    );
    assert_eq!(
        check(&signed, &id('e'), &cut(), 200_000),
        mismatch("this Server")
    );
    assert_eq!(
        check(&signed, &b(), &cut(), 200_000),
        mismatch("a Volume step")
    );

    let mut changed = signed.clone();
    changed.body = changed.body.replace(&"b".repeat(32), &"e".repeat(32));
    assert_eq!(
        check(&changed, &id('e'), &VolumeStep::Start, 200_000),
        Err(AuthorityRefusal::BadSignature {
            key: signer().public()
        })
    );

    let stranger = Signer::from_seed(&[3; 32]);
    assert_eq!(
        check(
            &Signed::sign(&approval(moving()), &stranger).unwrap(),
            &a(),
            &cut(),
            200_000
        ),
        Err(AuthorityRefusal::UnknownKey {
            key: stranger.public()
        })
    );

    let mut forged = signed.clone();
    let [signature] = forged.signatures.as_mut_slice() else {
        panic!("one signature");
    };
    signature.signature[0] ^= 1;
    assert_eq!(
        check(&forged, &a(), &cut(), 200_000),
        Err(AuthorityRefusal::BadSignature {
            key: signer().public()
        })
    );

    let mut unsigned = signed.clone();
    unsigned.signatures.clear();
    assert_eq!(
        check(&unsigned, &a(), &cut(), 200_000),
        Err(AuthorityRefusal::Unsigned {
            purpose: Purpose::Volume
        })
    );

    let mut to_itself = approval(moving());
    let VolumeWorkflow::Move { to, .. } = &mut to_itself.workflow else {
        unreachable!("a Move")
    };
    *to = a();
    assert_eq!(
        check(
            &Signed::sign(&to_itself, &signer()).unwrap(),
            &a(),
            &cut(),
            200_000
        ),
        mismatch("Machines")
    );
}

#[test]
fn the_window_bounds_the_first_step_with_clock_skew() {
    let signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    let skew = crate::authority::validity::CLOCK_SKEW_MILLIS;
    assert!(check(&signed, &a(), &cut(), 1_000_000 + skew).is_ok());
    assert_eq!(
        check(&signed, &a(), &cut(), 1_000_001 + skew),
        Err(AuthorityRefusal::Expired)
    );
    let early = VolumeApproval {
        issued_at: at(300_000),
        ..approval(moving())
    };
    assert_eq!(
        check(
            &Signed::sign(&early, &signer()).unwrap(),
            &a(),
            &cut(),
            299_999 - skew
        ),
        mismatch("an issue time")
    );
    let long = VolumeApproval {
        expires_at: at(100_000 + crate::authority::validity::MAX_LIFETIME_MILLIS + 1),
        ..approval(moving())
    };
    assert_eq!(
        check(
            &Signed::sign(&long, &signer()).unwrap(),
            &a(),
            &cut(),
            200_000
        ),
        mismatch("a lifetime")
    );
}

#[test]
fn a_browser_key_under_a_grant_of_every_family_cannot_sign_a_volume_approval() {
    let browser = Signer::from_seed(&[4; 32]);
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: cluster(),
        environment: Namespace::parse("app").unwrap(),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![a(), b(), c()],
        families: vec![
            ActionFamily::Deploy,
            ActionFamily::Restart,
            ActionFamily::Settings,
            ActionFamily::Start,
            ActionFamily::Stop,
        ],
        issued_at: at(0),
        expires_at: at(3_000_000),
        delegation: Delegation::None,
    };
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let browser_approval = VolumeApproval {
        signer: SignerKey::Browser {
            browser: browser.public(),
        },
        ..approval(moving())
    };
    let signed = Signed::sign_under(
        &browser_approval,
        &browser,
        SessionGrant { body, assertion },
    )
    .unwrap();
    let trusted = trusting(&passkey);
    assert!(
        signed.approvers(&trusted, at(200_000)).is_ok(),
        "the grant itself verifies"
    );
    assert_eq!(
        verify(
            &signed,
            &VolumeFacts {
                cluster: Some(cluster()),
                machine: &a(),
                trusted: &trusted,
                volume: &volume(),
                step: &cut(),
                now: at(200_000),
                handed: None,
            },
        ),
        mismatch("the signer")
    );
}

#[test]
fn a_signed_approval_riding_a_proposal_is_refused() {
    let mut signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    let bundle = ProposalApproval {
        cluster: cluster(),
        environment: Namespace::parse("app").unwrap(),
        operation: OperationId::parse(OPERATION).unwrap(),
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Deploy,
            digest: StepDigest::of(&signed.body),
        }],
        issued_at: at(100_000),
        expires_at: at(1_000_000),
        signer: SignerKey::Device(DeviceKey::from(signer().public())),
    };
    signed.proposal = Some(Box::new(Signed::sign(&bundle, &signer()).unwrap()));
    assert_eq!(
        check(&signed, &a(), &cut(), 200_000),
        mismatch("a proposal")
    );
}

#[test]
fn a_field_this_machine_cannot_read_is_malformed() {
    let mut signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    signed.body = signed
        .body
        .replace(r#""kind":"move","#, r#""kind":"move","keep":true,"#);
    assert!(matches!(
        signed.open(),
        Err(AuthorityRefusal::Malformed {
            purpose: Purpose::Volume,
            ..
        })
    ));
    let mut unsorted = Signed::sign(&approval(workflows()[4].clone()), &signer()).unwrap();
    let (a, b) = ("a".repeat(32), "b".repeat(32));
    unsorted.body = unsorted
        .body
        .replace(&format!(r#"["{a}","{b}"]"#), &format!(r#"["{b}","{a}"]"#));
    assert!(matches!(
        unsorted.open(),
        Err(AuthorityRefusal::Malformed { .. })
    ));
}

#[test]
fn a_mirror_approval_shows_the_volume_both_servers_and_that_syncs_continue() {
    let name = |machine: &MachineId| if *machine == a() { "web-1" } else { "web-2" }.to_owned();
    let mirror = |replace| {
        approval(VolumeWorkflow::Mirror {
            writer: a(),
            mirror: b(),
            replace,
        })
    };
    let change = ApprovedVolumeChange::of(&mirror(false), "data", name).unwrap();
    assert_eq!(
        change.line(),
        "Approved: mirror data from web-1 to web-2. Later syncs run without asking until you remove the Mirror."
    );
    assert_eq!(
        serde_json::to_value(&change).unwrap(),
        serde_json::json!({
            "kind": "mirror",
            "volume": "data",
            "writer": "web-1",
            "mirror": "web-2",
            "replace": false,
        })
    );
    assert_eq!(
        ApprovedVolumeChange::of(&mirror(true), "data", name)
            .unwrap()
            .line(),
        "Approved: copy data from web-1 to web-2 again, replacing the copy there. Later syncs run without asking until you remove the Mirror."
    );
    assert_eq!(
        ApprovedVolumeChange::of(&approval(moving()), "data", name),
        None
    );
}

#[test]
fn a_move_approval_shows_both_servers_the_service_it_starts_and_the_mirrors_that_follow() {
    let name = |machine: &MachineId| {
        if *machine == a() {
            "web-1"
        } else if *machine == b() {
            "web-2"
        } else {
            "web-3"
        }
        .to_owned()
    };
    let service = |digest: &str| ApprovedChange {
        service: ServiceName::parse("db").unwrap(),
        image: format!("postgres@sha256:{}", "0123456789ab".repeat(5)),
        tag: None,
        digest: PayloadDigest::try_from(digest.repeat(64)).unwrap(),
        host: Vec::new(),
        settings: vec![r#"command ["postgres"]"#.to_owned()],
    };
    let change =
        ApprovedVolumeChange::moving(&approval(moving()), "data", name, service("d")).unwrap();
    assert_eq!(
        change.lines(),
        [
            "Approved: move data from web-1 to web-2. db stops on web-1 for the last copy, then starts on web-2. The Mirror on web-3 keeps syncing.".to_owned(),
            format!("Approved db: postgres@sha256:{}", "0123456789ab".repeat(5)),
            r#"  command ["postgres"]"#.to_owned(),
        ]
    );
    assert_eq!(
        ApprovedVolumeChange::moving(&approval(moving()), "data", name, service("e")),
        None,
        "a payload the approval does not name"
    );
    let mirror = approval(VolumeWorkflow::Mirror {
        writer: a(),
        mirror: b(),
        replace: false,
    });
    assert_eq!(
        ApprovedVolumeChange::moving(&mirror, "data", name, service("d")),
        None
    );
}

#[test]
fn a_resize_approval_shows_the_volume_its_size_every_server_and_whether_a_copy_shrinks() {
    let name = |machine: &MachineId| {
        if *machine == a() {
            "web-1".to_owned()
        } else {
            "web 2\u{1b}[2K".to_owned()
        }
    };
    let resize = |maximum_bytes, shrink| {
        approval(VolumeWorkflow::Resize {
            machines: vec![a(), b()],
            maximum_bytes,
            shrink,
        })
    };
    let grow = ApprovedVolumeChange::of(&resize(bytes(20), false), "data", name).unwrap();
    assert_eq!(
        grow.line(),
        r#"Approved: set data to 20GB on web-1, "web 2\u{1b}[2K". No copy shrinks."#
    );
    assert_eq!(
        serde_json::to_value(&grow).unwrap(),
        serde_json::json!({
            "kind": "resize",
            "volume": "data",
            "servers": ["web-1", "web 2\u{1b}[2K"],
            "size": "20GB",
            "shrink": false,
        })
    );
    let half = ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(1_500_000_001).unwrap());
    assert_eq!(
        ApprovedVolumeChange::of(&resize(half, true), "data", name)
            .unwrap()
            .line(),
        r#"Approved: set data to 1.500000001GB on web-1, "web 2\u{1b}[2K", lowering any copy above it."#
    );
}

fn handed(signed: &Signed<VolumeApproval>) -> HandoffReceipt {
    HandoffReceipt {
        cluster: cluster(),
        operation: OperationId::parse(OPERATION).unwrap(),
        approval: StepDigest::of(&signed.body),
        volume: volume(),
        from: a(),
        to: b(),
        at: SnapshotGuid::new(9),
        snapshot: "f-2".parse().unwrap(),
        from_epoch: WriterEpoch::new(1),
        epoch: WriterEpoch::new(2),
    }
}

#[test]
fn a_receipt_for_this_move_and_machine_stands_in_for_the_window() {
    let signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    let pull = VolumeStep::Copy(CopyTransition::MirrorToMirror);
    let late = |machine: &MachineId, receipt: Option<&HandoffReceipt>| {
        verify(
            &signed,
            &VolumeFacts {
                cluster: Some(cluster()),
                machine,
                trusted: &trusted(),
                volume: &volume(),
                step: &pull,
                now: at(5_000_000),
                handed: receipt,
            },
        )
        .map(|approved| approved.body.operation)
    };
    assert_eq!(late(&b(), None), Err(AuthorityRefusal::Expired));
    let receipt = handed(&signed);
    assert!(late(&b(), Some(&receipt)).is_ok());
    let other = Signed::sign(&approval(moving()).clone(), &Signer::from_seed(&[8; 32])).unwrap();
    for receipt in [
        HandoffReceipt {
            operation: OperationId::parse("1f8fad5bd9cb469fa16570867728950e").unwrap(),
            ..handed(&signed)
        },
        HandoffReceipt {
            approval: StepDigest::of(&format!("{} ", other.body)),
            ..handed(&signed)
        },
        HandoffReceipt {
            to: c(),
            ..handed(&signed)
        },
    ] {
        assert_eq!(late(&b(), Some(&receipt)), Err(AuthorityRefusal::Expired));
    }
}

#[test]
fn a_machine_statement_verifies_only_under_its_own_key_and_never_as_a_devices() {
    let receipt = handed(&Signed::sign(&approval(moving()), &signer()).unwrap());
    let cluster_key = Signer::from_seed(&[3; 32]);
    let signed = MachineSigned::sign(&receipt, &cluster_key).unwrap();
    assert_eq!(signed.verify(cluster_key.public()), Ok(receipt.clone()));
    assert_eq!(
        signed.verify(Signer::from_seed(&[4; 32]).public()),
        Err(AuthorityRefusal::ForeignMachineKey {
            kind: "volume-handoff"
        })
    );
    let mut flipped = signed.clone();
    flipped.signature[63] ^= 1;
    assert_eq!(
        flipped.verify(cluster_key.public()),
        Err(AuthorityRefusal::BadMachineSignature {
            kind: "volume-handoff"
        })
    );
    let mut rewritten = signed.clone();
    rewritten.body = rewritten.body.replace("\"epoch\":2", "\"epoch\":3");
    assert_ne!(rewritten.body, signed.body);
    assert_eq!(
        rewritten.verify(cluster_key.public()),
        Err(AuthorityRefusal::BadMachineSignature {
            kind: "volume-handoff"
        })
    );
    let device_input = crate::authority::keys::signing_input(Purpose::Volume, &signed.body);
    let device_signature = {
        use ed25519_dalek::Signer as _;
        ed25519_dalek::SigningKey::from_bytes(&[3; 32])
            .sign(&device_input)
            .to_bytes()
    };
    let mut as_device = signed.clone();
    as_device.signature = device_signature;
    assert_eq!(
        as_device.verify(cluster_key.public()),
        Err(AuthorityRefusal::BadMachineSignature {
            kind: "volume-handoff"
        }),
        "a signature over a Device statement's input is no Machine's"
    );
    assert_eq!(
        serde_json::to_value(&signed)
            .unwrap()
            .get("key")
            .cloned()
            .unwrap(),
        serde_json::json!({ "kind": "cluster", "key": cluster_key.public().to_string() })
    );
}

#[test]
fn a_receipt_names_the_snapshot_and_epoch_the_step_lands_on() {
    let signed = Signed::sign(&approval(moving()), &signer()).unwrap();
    let cluster_key = Signer::from_seed(&[3; 32]);
    let receipt = MachineSigned::sign(&handed(&signed), &cluster_key).unwrap();
    let check = |at: Handed<'_>| {
        verify_receipt(
            &receipt,
            &HandoffFacts {
                key: cluster_key.public(),
                cluster: cluster(),
                machine: &b(),
                operation: OperationId::parse(OPERATION).unwrap(),
                approval: StepDigest::of(&signed.body),
                volume: &volume(),
                at,
            },
        )
        .map(|_| ())
    };
    let guid = SnapshotGuid::new(9);
    let named: SnapshotName = "f-2".parse().unwrap();
    let other: SnapshotName = "f-3".parse().unwrap();
    assert_eq!(
        check(Handed::Writer {
            at: &guid,
            epoch: WriterEpoch::new(2)
        }),
        Ok(())
    );
    assert_eq!(check(Handed::Pull { snapshot: &named }), Ok(()));
    assert_eq!(
        check(Handed::Writer {
            at: &SnapshotGuid::new(8),
            epoch: WriterEpoch::new(2)
        }),
        Err(AuthorityRefusal::Mismatch {
            field: "a snapshot"
        })
    );
    assert_eq!(
        check(Handed::Writer {
            at: &guid,
            epoch: WriterEpoch::new(3)
        }),
        Err(AuthorityRefusal::Mismatch { field: "an epoch" })
    );
    assert_eq!(
        check(Handed::Pull { snapshot: &other }),
        Err(AuthorityRefusal::Mismatch {
            field: "a snapshot"
        })
    );
}
