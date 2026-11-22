use crate::authority::MismatchField;
use serde_json::json;

use super::*;
use crate::authority::{
    keys::Signer,
    proposal::{ProposalApproval, ProposalStep, StepDigest},
    trusted_key::{Base64Url, CredentialId, PasskeyAssertion, tests::trusted_eddsa_passkey},
};
use crate::{
    DeviceKey, OperationId, RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig,
    ServiceId,
};

const IMAGE: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const CLUSTER: [u8; 32] = [9; 32];

fn signer(seed: u8) -> Signer {
    Signer::from_seed(&[seed; 32])
}

fn namespace() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn requested(env: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing", "environment": {"MODE": env}},
    }))
    .unwrap()
}

fn resolved(spec: &RequestedServiceSpec) -> ResolvedServiceSpec {
    spec.to_resolved(
        ServiceId::parse("c".repeat(32)).unwrap(),
        ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

fn body(by: &Signer, spec: &RequestedServiceSpec) -> ServiceVersion {
    ServiceVersion {
        forget_older: false,
        cluster: SealingPublicKey::from_bytes(CLUSTER),
        namespace: namespace(),
        service: web(),
        digest: PayloadDigest::of(spec, &namespace()).unwrap(),
        signed_at: Timestamp::from_millis(100_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from(by.public())),
        proposal: None,
    }
}

fn signed(by: &Signer, body: &ServiceVersion) -> Signed<ServiceVersion> {
    Signed::sign(body, by).unwrap()
}

fn trusting(signers: &[&Signer]) -> TrustedKeys {
    TrustedKeys::new(
        signers
            .iter()
            .map(|signer| crate::DeviceKey::from(signer.public())),
        [],
        [],
    )
}

fn check(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    spec: &ResolvedServiceSpec,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    verify(
        versions,
        &VersionFacts {
            cluster: Some(SealingPublicKey::from_bytes(CLUSTER)),
            trusted,
            namespace: &namespace(),
            creation: Creation {
                service: &web(),
                kind: ContainerKind::ServiceContainer,
                spec,
            },
        },
        &mut Budget::default(),
    )
}

#[test]
fn a_version_admits_its_spec_on_any_machine_that_trusts_its_signer() {
    let device = signer(1);
    let spec = requested("a");

    let admitted = check(
        &[signed(&device, &body(&device, &spec))],
        &trusting(&[&device]),
        &resolved(&spec),
    )
    .unwrap();

    assert_eq!(
        approvers(&admitted),
        vec![Approver::Signing(device.public())]
    );
}

#[test]
fn a_trusted_forget_its_machine_refuses_refuses_the_creation_another_version_admits() {
    let trusted = signer(1);
    let other = signer(2);
    let spec = requested("a");
    let forget = ServiceVersion {
        forget_older: true,
        cluster: SealingPublicKey::from_bytes([8; 32]),
        ..body(&other, &spec)
    };

    let refused = check(
        &[
            signed(&trusted, &body(&trusted, &spec)),
            signed(&other, &forget),
        ],
        &trusting(&[&trusted, &other]),
        &resolved(&spec),
    );

    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn a_removed_devices_old_forget_does_not_block_a_fresh_approval() {
    let trusted = signer(1);
    let removed = signer(2);
    let spec = requested("a");
    let forget = ServiceVersion {
        forget_older: true,
        ..body(&removed, &spec)
    };

    let admitted = check(
        &[
            signed(&removed, &forget),
            signed(&trusted, &body(&trusted, &spec)),
        ],
        &trusting(&[&trusted]),
        &resolved(&spec),
    );

    assert!(admitted.is_ok(), "{admitted:?}");
}

#[test]
fn a_version_moved_or_edited_by_whoever_carries_it_is_refused() {
    let device = signer(1);
    let spec = requested("a");
    let honest = body(&device, &spec);
    let trusted = trusting(&[&device]);
    let mut other_service = honest.clone();
    other_service.service = ServiceName::parse("api").unwrap();
    let mut other_namespace = honest.clone();
    other_namespace.namespace = Namespace::parse("other").unwrap();
    let mut other_cluster = honest.clone();
    other_cluster.cluster = SealingPublicKey::from_bytes([8; 32]);
    let mut edited = signed(&device, &honest);
    edited.body = edited.body.replace("\"app\"", "\"ap2\"");
    let cases = [
        (signed(&device, &other_service), MismatchField::Service),
        (
            signed(&device, &other_namespace),
            MismatchField::Environment,
        ),
        (signed(&device, &other_cluster), MismatchField::Cluster),
    ];

    for (version, field) in cases {
        assert_eq!(
            check(&[version], &trusted, &resolved(&spec)),
            Err(AuthorityRefusal::Mismatch { field }),
        );
    }
    assert!(matches!(
        check(&[edited], &trusted, &resolved(&spec)),
        Err(AuthorityRefusal::BadSignature { .. })
    ));
    assert_eq!(
        check(
            &[signed(&device, &honest)],
            &trusted,
            &resolved(&requested("b"))
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Payload
        }),
    );
    assert!(check(&[], &trusted, &resolved(&spec)).is_err());
}

#[test]
fn eight_versions_by_removed_signers_cost_nothing_beside_a_trusted_ninth() {
    let removed: Vec<Signer> = (10..18).map(signer).collect();
    let kept = signer(1);
    let spec = requested("a");
    let mut versions: Vec<_> = removed
        .iter()
        .map(|device| signed(device, &body(device, &spec)))
        .collect();
    versions.push(signed(&kept, &body(&kept, &spec)));

    let admitted = check(&versions, &trusting(&[&kept]), &resolved(&spec)).unwrap();

    assert_eq!(approvers(&admitted), vec![Approver::Signing(kept.public())]);
}

#[test]
fn one_envelope_with_nine_trusted_signatures_is_never_verified() {
    let first = signer(1);
    let others: Vec<Signer> = (2..10).map(signer).collect();
    let spec = requested("a");
    let mut version = signed(&first, &body(&first, &spec));
    for device in &others {
        let extra = signed(device, &body(&first, &spec));
        version.signatures.extend(extra.signatures);
    }
    let devices: Vec<&Signer> = std::iter::once(&first).chain(&others).collect();

    assert_eq!(
        check(&[version], &trusting(&devices), &resolved(&spec)),
        Err(AuthorityRefusal::Overspent),
    );
}

#[test]
fn steps_sharing_one_signed_bundle_each_pay_for_the_bundle() {
    let (first, second) = (signer(1), signer(2));
    let spec = requested("a");
    let operation = OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap();
    let mut step = body(&first, &requested("not this one"));
    step.proposal = Some(ProposalRef {
        operation,
        index: 0,
    });
    let step = Signed::unsigned(&step).unwrap();
    let plan = ProposalApproval {
        cluster: SealingPublicKey::from_bytes(CLUSTER),
        environment: namespace(),
        operation,
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Version,
            digest: StepDigest::of(&step.body),
        }],
        issued_at: Timestamp::from_millis(100_000).unwrap(),
        expires_at: Timestamp::from_millis(1_000_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from(first.public())),
    };
    let mut bundle = Signed::sign(&plan, &first).unwrap();
    bundle
        .signatures
        .extend(Signed::sign(&plan, &second).unwrap().signatures);
    let mut riding = step;
    riding.proposal = Some(Box::new(bundle));
    let mut versions = vec![riding; 4];
    versions.push(signed(&first, &body(&first, &spec)));

    assert_eq!(
        check(&versions, &trusting(&[&first, &second]), &resolved(&spec)),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Payload
        }),
        "four steps at two verifications each spend the budget before the fifth statement"
    );
}

#[test]
fn nine_trusted_passkeys_under_one_credential_cost_nine_verifications() {
    let device = signer(1);
    let spec = requested("a");
    let credential = CredentialId::try_from(Base64Url::new(vec![7, 7, 7])).unwrap();
    let passkeys = (1..10)
        .map(|seed| {
            let mut trusted = trusted_eddsa_passkey(&[seed; 32]);
            trusted.passkey.credential_id = credential.clone();
            trusted
        })
        .collect::<Vec<_>>();
    let mut version = Signed::unsigned(&body(&device, &spec)).unwrap();
    version.assertions.push(PasskeyAssertion {
        credential_id: credential,
        authenticator_data: Base64Url::new(vec![0; 37]),
        client_data_json: Base64Url::new(b"{}".to_vec()),
        signature: Base64Url::new(vec![0; 64]),
    });
    let trusted = TrustedKeys::new(Vec::new(), passkeys, []);

    assert_eq!(
        check(&[version], &trusted, &resolved(&spec)),
        Err(AuthorityRefusal::Overspent),
    );
}

#[test]
fn an_ensure_request_carrying_more_than_thirty_two_versions_does_not_parse() {
    let device = signer(1);
    let version = serde_json::to_value(signed(&device, &body(&device, &requested("a")))).unwrap();
    let parse = |count: usize| {
        serde_json::from_value::<crate::EnsureContainerRequest>(json!({
            "want": {"state": "gone", "container": {"id": "a".repeat(64)}, "drain": 0},
            "versions": vec![version.clone(); count],
        }))
        .map(|request| request.versions.len())
    };

    assert_eq!(parse(32).unwrap(), 32);
    assert!(parse(33).is_err());
}

/// A Volume born in another Environment is that Environment's data, so a version signed
/// here never opens it, whatever the signature covers.
#[test]
fn a_version_naming_a_volume_born_in_another_environment_is_refused() {
    let device = signer(1);
    let born_in = |namespace: &str| -> RequestedServiceSpec {
        serde_json::from_value(json!({
            "name": "web",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": IMAGE, "pull_policy": "missing"},
            "volumes": [{"reference": "data", "source": {"kind": "provisioned", "name": "data",
                "maximum_bytes": 1_073_741_824_u64,
                "birth": {"namespace": namespace, "name": "data", "made_at": 1_762_178_640_000_u64,
                    "nonce": "00112233445566778899aabbccddeeff"}}}],
            "mounts": [{"volume": "data", "target": "/data"}],
        }))
        .unwrap()
    };
    let scoped = |spec: &RequestedServiceSpec| {
        let mut spec = spec.clone();
        spec.mount_graph = spec.mount_graph.scope_to_namespace(&namespace()).unwrap();
        resolved(&spec)
    };
    let trusted = trusting(&[&device]);

    let here = born_in("app");
    assert!(
        check(
            &[signed(&device, &body(&device, &here))],
            &trusted,
            &scoped(&here)
        )
        .is_ok()
    );
    let elsewhere = born_in("other");
    assert_eq!(
        check(
            &[signed(&device, &body(&device, &elsewhere))],
            &trusted,
            &scoped(&elsewhere)
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::VolumesEnvironment
        }),
    );
}

fn approvers(admitted: &crate::authority::VersionAdmitted) -> Vec<Approver> {
    admitted
        .by
        .iter()
        .map(|approved| approved.by.clone())
        .collect()
}
