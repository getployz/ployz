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
            writer: a(),
            role: SlotRole::Mirror,
            last_copy: true,
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
        format!(
            r#"{{"kind":"mirror_removal","last_copy":true,"mirror":"{b}","role":"mirror","writer":"{a}"}}"#
        ),
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
        (a(), VolumeStep::Start),
        (b(), VolumeStep::Copy(NoneToMirror)),
        (b(), VolumeStep::Copy(MirrorToMirror)),
        (b(), VolumeStep::Copy(MirrorToWriter)),
        (b(), VolumeStep::Start),
        (c(), VolumeStep::Copy(MirrorToMirror)),
    ] {
        let approved = check(&moved, &machine, &step, 200_000).unwrap();
        assert_eq!(approved.by, signer().public());
        assert_eq!(approved.digest, StepDigest::of(&moved.body));
    }
    for (machine, step) in [
        (a(), VolumeStep::Copy(MirrorToWriter)),
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
