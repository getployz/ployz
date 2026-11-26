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

/// A drain recreates a free Container on another Server with the Free receipt its Build
/// gave for the image the copy runs. Without it the Server would have to pull that image,
/// which no registry holds.
#[tokio::test]
async fn a_drained_free_copy_carries_the_receipt_of_the_image_it_runs() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, Deploy, DeploymentId, EnvironmentId,
        EnvironmentRef, OrganizationId, ProjectId, ProjectName, RunEvidence, RunnerId, SealingKey,
    };
    const DEPLOYMENT: &str = "00000000-0000-4000-8000-000000000103";
    let store = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap(),
    );
    let organization = OrganizationId::parse("owner").unwrap();
    let owner = Actor::system(organization.clone());
    store
        .write(
            &owner,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000101").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000102")
                    .unwrap(),
                region: None,
            },
        )
        .unwrap();
    store
        .write(
            &owner,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse(DEPLOYMENT).unwrap(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let deployment = DeploymentId::parse(DEPLOYMENT).unwrap();
    store.claim(&deployment, &runner).unwrap();
    let image = format!("sha256:{}", "1".repeat(64));
    let receipt = serde_json::json!({"image": image, "mac": "b".repeat(64)});
    store
        .record(
            &deployment,
            &runner,
            RunEvidence::Built(
                [(
                    ployz_core::ServiceName::parse("api").unwrap(),
                    serde_json::json!({"fingerprint": "f".repeat(64), "free": receipt}),
                )]
                .into(),
            ),
        )
        .unwrap();

    let namespace = ployz_core::Namespace::parse("shop-production").unwrap();
    let approvals = Approvals::kept(super::KeptVersions::new(store, organization));
    let mut free = requested(ServiceMode::Global);
    free.container.image = format!("ployz-build/api@{image}");
    let resolved = |spec: ployz_core::RequestedServiceSpec| {
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .unwrap()
    };
    // A drain asks the same before it picks a Server.
    assert!(
        approvals
            .carries_free(&namespace, &resolved(free.clone()))
            .await
    );
    assert_eq!(
        approvals.carried(&want_of(free, &namespace)).await.free,
        Some(serde_json::from_value(receipt).unwrap())
    );
    let mut other = requested(ServiceMode::Global);
    other.container.image = format!("ployz-build/api@sha256:{}", "2".repeat(64));
    assert!(
        !approvals
            .carries_free(&namespace, &resolved(other.clone()))
            .await
    );
    assert_eq!(
        approvals.carried(&want_of(other, &namespace)).await.free,
        None
    );
}
