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

/// A creation admitted under one approval, whose image's hook place that approval took
/// already, is refused as superseded once a newer approval of the rule reached this
/// Machine, not as a hook that already ran.
#[test]
fn a_registry_hook_under_an_older_approval_says_a_newer_one_replaced_it() {
    use ployz_core::authority::{
        AutoDeployRule, RegistryPin, RegistrySource, RuleId, RuleSource, SignerKey,
    };
    let dir = tempfile::tempdir().unwrap();
    let log = ConsumptionLog::new(dir.path());
    let rule = RuleId::from_bytes([0x11; 16]);
    let service = ServiceName::parse("web").unwrap();
    let source = RegistrySource {
        host: "ghcr.io".to_owned(),
        repository: "acme/web".to_owned(),
    };
    let pin = RegistryPin {
        source: source.clone(),
        digest: ployz_core::ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap(),
    };
    let slot = |key: &str| ployz_core::CreationKey::try_from(key.to_owned()).unwrap();
    log.consume_registry_hook((rule, 2), &service, &pin, &slot("k1"), Error::DeployRefused)
        .unwrap();
    // Generation 3 reaches this Machine through another Service's creation.
    let newer = AutoDeployRule {
        cluster: ployz_core::SealingPublicKey::from_bytes([3; 32]),
        rule,
        generation: 3,
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        machines: Vec::new(),
        source: RuleSource::Registry(source),
        services: Vec::new(),
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes([1; 32])),
        proposal: None,
    };
    log.mark_rule(&newer, Admit::Record, Error::DeployRefused)
        .unwrap();

    let older =
        log.consume_registry_hook((rule, 2), &service, &pin, &slot("k2"), Error::DeployRefused);
    assert!(
        matches!(
            older,
            Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                field: MismatchField::Generation
            }))
        ),
        "{older:?}"
    );
}
