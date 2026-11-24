use super::*;

#[test]
fn an_unreadable_record_refuses_rather_than_forgetting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(FILE_NAME), b"{").unwrap();
    let log = ConsumptionLog::new(dir.path());
    let machine = ployz_core::MachineId::parse("1".repeat(32)).unwrap();
    assert!(matches!(
        log.require_unused_join(machine),
        Err(Error::JoinRecord(_))
    ));
}

fn copy_step(index: u32, window: CopyWindow, body: &str) -> CopyVerified {
    use ployz_core::{
        BirthNonce, DockerVolumeName, MachineId, Namespace, OperationId, SnapshotGuid, VolumeBirth,
        VolumeOrigin,
        authority::{CopyStep, Signer, SignerKey},
    };
    let app = Namespace::parse("app").unwrap();
    let source = DockerVolumeName::parse("app_vol-data").unwrap();
    let made_at = Timestamp::from_millis(1_000_000).unwrap();
    CopyVerified {
        step: CopyStep {
            cluster: SealingPublicKey::from_bytes([5; 32]),
            namespace: app.clone(),
            proposal: ProposalRef {
                operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
                index,
            },
            signer: SignerKey::Device(Signer::from_seed(&[1; 32]).public().into()),
            machine: MachineId::parse("a".repeat(32)).unwrap(),
            origin: VolumeOrigin::legacy(&source),
            source,
            point: SnapshotGuid::new(42),
            taken_at: made_at,
            into: DockerVolumeName::parse("app_vol-data2").unwrap(),
            birth: VolumeBirth {
                namespace: app,
                name: "data2".to_owned(),
                made_at,
                nonce: BirthNonce::from_bytes([3; 16]),
            },
        },
        digest: StepDigest::of(body),
        window,
        closes: made_at,
    }
}

/// A step taken inside its window is taken again after a restart whatever its window, so a
/// resend can finish its copy; another step at its place, or a new step past its window,
/// is refused.
#[test]
fn a_copy_step_is_taken_once_and_kept_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let log = ConsumptionLog::new(dir.path());
    log.take_copy(&copy_step(0, CopyWindow::Open, "a")).unwrap();

    let restarted = ConsumptionLog::new(dir.path());
    restarted
        .take_copy(&copy_step(0, CopyWindow::Closed, "a"))
        .unwrap();
    assert!(matches!(
        restarted.take_copy(&copy_step(0, CopyWindow::Open, "b")),
        Err(Error::CopyRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::Step
        }))
    ));
    assert!(matches!(
        restarted.take_copy(&copy_step(1, CopyWindow::Closed, "c")),
        Err(Error::CopyRefused(AuthorityRefusal::Expired))
    ));
}
