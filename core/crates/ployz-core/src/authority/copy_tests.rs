use super::*;
use crate::authority::{
    PasskeyApprover, ProposalApproval, ProposalStep, Signer,
    scope::{CapSet, Grant, GrantKind, KeyPlaces, KeyScope, TrustedGrant},
};
use crate::{BirthNonce, DeviceKey, OperationId};

const ISSUED: u64 = 1_000_000;
const WINDOW: u64 = 10 * 60 * 1000;

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([5; 32])
}

fn device() -> Signer {
    Signer::from_seed(&[1; 32])
}

fn ci() -> Signer {
    Signer::from_seed(&[2; 32])
}

fn here() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn app() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn volume(name: &str) -> DockerVolumeName {
    DockerVolumeName::parse(name).unwrap()
}

fn seed() -> SeedVolumeRequest {
    SeedVolumeRequest {
        name: volume("app_vol-data"),
        into: volume("app_vol-data2"),
        guid: SnapshotGuid::new(42),
        r#gen: None,
    }
}

fn step(signer: &Signer) -> CopyStep {
    CopyStep {
        cluster: cluster(),
        namespace: app(),
        proposal: ProposalRef {
            operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
            index: 0,
        },
        signer: SignerKey::Device(signer.public().into()),
        machine: here(),
        source: volume("app_vol-data"),
        origin: VolumeOrigin::legacy(&volume("app_vol-data")),
        point: SnapshotGuid::new(42),
        taken_at: at(900_000),
        into: volume("app_vol-data2"),
        birth: VolumeBirth {
            namespace: app(),
            name: "data2".to_owned(),
            made_at: at(ISSUED),
            nonce: BirthNonce::from_bytes([3; 16]),
        },
    }
}

/// `step` as step 0 of a bundle `signer` signed.
fn bundled(step: &CopyStep, signer: &Signer) -> Signed<CopyStep> {
    let unsigned = Signed::unsigned(step).unwrap();
    let plan = ProposalApproval {
        cluster: cluster(),
        environment: app(),
        operation: step.proposal.operation,
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Copy,
            digest: StepDigest::of(&unsigned.body),
        }],
        issued_at: at(ISSUED),
        expires_at: at(ISSUED + WINDOW),
        signer: step.signer,
    };
    Signed::proposed(step, Signed::sign(&plan, signer).unwrap()).unwrap()
}

/// A Machine trusting `device()` as a Device and `ci()` as a CI key in app.
fn trusted() -> TrustedKeys {
    TrustedKeys::new(
        [DeviceKey::from(device().public())],
        [],
        [TrustedGrant {
            grant: Grant {
                key: ci().public(),
                kind: GrantKind::Ci,
                scope: KeyScope {
                    caps: CapSet::ALL,
                    places: KeyPlaces::Only(vec![crate::authority::KeyPlace {
                        environment: uuid::Uuid::nil(),
                        namespace: crate::Namespace::parse("app").unwrap(),
                        service: None,
                    }]),
                },
                volumes: Vec::new(),
                name: None,
                issued_at: at(ISSUED),
            },
            approved_by: PasskeyApprover::Server,
        }],
    )
}

fn facts(trusted: &TrustedKeys, now: u64) -> CopyFacts<'_> {
    CopyFacts {
        trusted,
        cluster: Some(cluster()),
        machine: here(),
        now: at(now),
    }
}

fn refused(field: MismatchField) -> AuthorityRefusal {
    AuthorityRefusal::Mismatch { field }
}

#[test]
fn a_copy_opens_inside_its_window_and_reports_a_closed_one() {
    let trusted = trusted();
    let signed = bundled(&step(&device()), &device());
    let open = verify(&signed, &seed(), &facts(&trusted, ISSUED + 60_000)).unwrap();
    assert_eq!(open.window, CopyWindow::Open);
    assert_eq!(open.digest, StepDigest::of(&signed.body));
    let late = verify(&signed, &seed(), &facts(&trusted, ISSUED + 30 * 60 * 1000)).unwrap();
    assert_eq!(late.window, CopyWindow::Closed);
}

#[test]
fn a_copy_needs_a_person_or_full_cli_key_and_a_bundle() {
    let trusted = trusted();
    let by_ci = bundled(&step(&ci()), &ci());
    assert!(matches!(
        verify(&by_ci, &seed(), &facts(&trusted, ISSUED)),
        Err(AuthorityRefusal::OutOfScope { .. })
    ));
    let alone = Signed::sign(&step(&device()), &device()).unwrap();
    assert_eq!(
        verify(&alone, &seed(), &facts(&trusted, ISSUED)),
        Err(refused(MismatchField::Proposal))
    );
}

#[test]
fn a_copy_is_refused_for_another_machine_namespace_or_seed() {
    let trusted = trusted();
    let check = |edit: fn(&mut CopyStep), asked: SeedVolumeRequest| {
        let mut body = step(&device());
        edit(&mut body);
        verify(&bundled(&body, &device()), &asked, &facts(&trusted, ISSUED))
    };
    assert_eq!(
        check(
            |body| body.machine = MachineId::parse("b".repeat(32)).unwrap(),
            seed()
        ),
        Err(refused(MismatchField::ThisMachine))
    );
    assert_eq!(
        check(|body| body.into = volume("web_vol-data2"), seed()),
        Err(refused(MismatchField::Namespace))
    );
    assert_eq!(
        check(
            |body| body.birth.namespace = Namespace::parse("web").unwrap(),
            seed()
        ),
        Err(refused(MismatchField::Namespace))
    );
    let other_point = SeedVolumeRequest {
        guid: SnapshotGuid::new(43),
        ..seed()
    };
    assert_eq!(
        check(|_| {}, other_point),
        Err(refused(MismatchField::RestorePoint))
    );
    let other_into = SeedVolumeRequest {
        into: volume("app_vol-data3"),
        ..seed()
    };
    assert_eq!(
        check(|_| {}, other_into),
        Err(refused(MismatchField::CopyDestination))
    );
}
