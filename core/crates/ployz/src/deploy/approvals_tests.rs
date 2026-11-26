use ployz_core::authority::{PayloadDigest, ServiceVersion, Signed, Signer, SignerKey, Timestamp};
use ployz_core::{
    ContainerWant, NewContainer, ResolvedUpdateConfig, RunRotation, RunTarget, ServiceMode,
};

use super::{Approvals, RuleProofs};
use crate::global_fixtures::{requested, service_id};

fn version(digest: PayloadDigest) -> Signed<ServiceVersion> {
    edit(digest, None)
}

fn edit(digest: PayloadDigest, parent: Option<PayloadDigest>) -> Signed<ServiceVersion> {
    let signer = Signer::from_seed(&[1; 32]);
    Signed::sign(
        &ServiceVersion {
            forget_older: false,
            parent,
            cluster: ployz_core::SealingPublicKey::from_bytes([9; 32]),
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            service: ployz_core::ServiceName::parse("api").unwrap(),
            digest,
            signed_at: Timestamp::now(),
            signer: SignerKey::Device(signer.public().into()),
            proposal: None,
        },
        &signer,
    )
    .unwrap()
}

#[tokio::test]
async fn a_new_copy_carries_only_the_versions_of_the_spec_it_runs() {
    const PINNED: &str = "ghcr.io/getployz/api@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let namespace = ployz_core::Namespace::parse("app").unwrap();
    let mut pinned = requested(ServiceMode::Global);
    pinned.container.image = PINNED.to_owned();
    let tagged = requested(ServiceMode::Global);
    let signed = version(PayloadDigest::of(&pinned, &namespace).unwrap());
    let other = version(PayloadDigest::of(&tagged, &namespace).unwrap());
    let approvals = Approvals::signed(vec![signed.clone(), other.clone()], RuleProofs::default());
    for (spec, sent) in [(pinned, signed), (tagged, other)] {
        assert_eq!(
            approvals.carried(&want_of(spec, &namespace)).await.versions,
            vec![sent]
        );
    }
}

fn want_of(
    spec: ployz_core::RequestedServiceSpec,
    namespace: &ployz_core::Namespace,
) -> ContainerWant {
    let resolved = spec
        .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
        .unwrap();
    ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            r#gen: ployz_core::Gen::FIRST,
            slot: ployz_core::SlotKey {
                namespace: namespace.clone(),
                service: resolved.name.clone(),
                kind: ployz_core::ContainerKind::ServiceContainer,
                creation_key: "global:test".to_owned().try_into().unwrap(),
            },
            resolved_spec: resolved,
            deployment_id: None,
            registry_auth: None,
            registry_login: None,
            image_prepared: false,
            image_id: None,
            answer: ployz_core::AnswerWhen::Started,
            replaces: None,
            free_receipt: None,
        }),
        rotation: RunRotation::Pending,
    }
}

#[tokio::test]
async fn an_edit_carries_the_parentless_version_it_edits() {
    const PINNED: &str = "ghcr.io/getployz/api@sha256:2222222222222222222222222222222222222222222222222222222222222222";
    let namespace = ployz_core::Namespace::parse("app").unwrap();
    let mut pinned = requested(ServiceMode::Global);
    pinned.container.image = PINNED.to_owned();
    let tagged = requested(ServiceMode::Global);
    let digest = PayloadDigest::of(&pinned, &namespace).unwrap();
    let parent = PayloadDigest::of(&tagged, &namespace).unwrap();
    let child = edit(digest, Some(parent));
    let root = version(parent);
    let approvals = Approvals::signed(vec![child.clone(), root.clone()], RuleProofs::default());
    assert_eq!(
        approvals
            .carried(&want_of(pinned, &namespace))
            .await
            .versions,
        vec![child, root]
    );
}

// Catches a Deploy whose rule covers one Service handing that rule's proof to another
// Service's creation, which its Server then refuses under the rule.
#[tokio::test]
async fn a_service_the_rule_doesnt_cover_carries_no_rule_proof() {
    use ployz_core::authority::{
        AutoDeployRule, RegistrySource, RuleEvidence, RuleId, RuleProof, RuleSource,
    };
    let namespace = ployz_core::Namespace::parse("app").unwrap();
    let signer = Signer::from_seed(&[1; 32]);
    let rule = AutoDeployRule {
        proposal: None,
        cluster: ployz_core::SealingPublicKey::from_bytes([3; 32]),
        rule: RuleId::from_bytes([9; 16]),
        generation: 1,
        namespace: namespace.clone(),
        machines: Vec::new(),
        source: RuleSource::Registry(RegistrySource {
            host: "ghcr.io".to_owned(),
            repository: "acme/web".to_owned(),
        }),
        services: Vec::new(),
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    };
    let mut proofs = RuleProofs::default();
    proofs.insert(
        ployz_core::ServiceName::parse("web").unwrap(),
        ployz_core::authority::KeptRule::built(RuleProof {
            rule: Signed::sign(&rule, &signer).unwrap(),
            evidence: RuleEvidence::Registry,
            claim: None,
            reader: None,
        }),
    );
    let approvals = Approvals::signed(Vec::new(), proofs);
    let other = requested(ServiceMode::Global);
    assert_ne!(other.name.as_str(), "web");
    assert!(
        approvals
            .carried(&want_of(other, &namespace))
            .await
            .rule
            .is_none()
    );
}
