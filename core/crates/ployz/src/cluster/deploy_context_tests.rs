use ployz_core::authority::{PayloadDigest, ServiceVersion, Signed, Signer, SignerKey, Timestamp};
use ployz_core::{
    ContainerWant, NewContainer, ResolvedUpdateConfig, RunRotation, RunTarget, ServiceMode,
};

use super::DeployContext;
use crate::global_fixtures::{requested, service_id};

fn version(digest: PayloadDigest) -> Signed<ServiceVersion> {
    let signer = Signer::from_seed(&[1; 32]);
    Signed::sign(
        &ServiceVersion {
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
    let mut context = DeployContext::default();
    context.set_versions(vec![signed.clone(), other.clone()]);
    for (spec, sent) in [(pinned, signed), (tagged, other)] {
        let resolved = spec
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .unwrap();
        let want = ContainerWant::Running {
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
                image_prepared: false,
                answer: ployz_core::AnswerWhen::Started,
            }),
            rotation: RunRotation::Pending,
        };
        assert_eq!(context.carried(&want).await.versions, vec![sent]);
    }
}
