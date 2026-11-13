#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use ployz_core::{
    Namespace,
    authority::{
        AttestKey, BuildClaim, ImageDigest, PreviewReview, PreviewRule, Review, ReviewedService,
        Signed, preview::preview_namespace,
    },
};
use serde_json::{Value, json};

use super::super::{
    preview_grant::tests::{ACTOR, claim, inherited, preview_rule, token},
    rule_grant::tests::{Fixture, commit, fixture, github, now, web},
};
use super::*;

const IMAGE: &str = "4444444444444444444444444444444444444444444444444444444444444444";

fn spec() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": format!("ployz-build/web@sha256:{IMAGE}"),
            "pull_policy": "missing",
            "environment": {"PLAIN": "one"}
        },
        "pre_deploy": {"command": ["migrate"]}
    })
}

fn requested(spec: &Value) -> ployz_core::RequestedServiceSpec {
    serde_json::from_value(spec.clone()).unwrap()
}

fn want(namespace: &Namespace, kind: &str, key: &str, spec: &Value) -> ContainerWant {
    let resolved = requested(spec)
        .to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    serde_json::from_value(
        json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot":{"namespace":namespace, "service":"web", "kind":kind, "creation_key":key},
        "resolved_spec":resolved, "gen":1}}}),
    )
    .unwrap()
}

/// The `feature` branch's preview Namespace under `rule`.
fn feature(rule: &PreviewRule) -> Namespace {
    preview_namespace(rule.rule, &PreviewKey::Branch("feature".to_owned()))
}

struct Deploy {
    fixture: Fixture,
    rule: PreviewRule,
}

impl Deploy {
    /// A preview rule approving [`spec`] in each preview, with no build secrets.
    async fn new() -> Self {
        let fixture = fixture().await;
        let record = inherited(&fixture);
        let mut rule = preview_rule(&fixture, &record);
        let namespace = feature(&rule);
        let service = &mut rule.services[0];
        service.secrets = None;
        service.inherit.clear();
        service.template =
            ployz_core::authority::preview::template(&requested(&spec()), &namespace, service)
                .unwrap();
        Self { fixture, rule }
    }

    fn attest(&self) -> AttestKey {
        self.fixture
            .machine
            .held_attest(self.fixture.machine.record().phase())
            .unwrap()
    }

    fn built(&self, authority: CommitAuthority) -> BuildClaim {
        BuildClaim {
            stage: ployz_core::authority::BuildStage::Built {
                image: ImageDigest::parse(&format!("sha256:{IMAGE}")).unwrap(),
                attest: self.attest(),
            },
            ..claim(&self.rule, authority)
        }
    }

    /// A trusted push of `feature`, its image token spent as `jti`.
    fn pushed(&self, jti: &str) -> PreviewProof {
        let claim = self.built(CommitAuthority::Head);
        PreviewProof {
            rule: Signed::sign(&self.rule, &self.fixture.signer).unwrap(),
            review: None,
            token: token(&claim, jti, "push", "feature", ACTOR),
            claim,
            reader: None,
        }
    }

    async fn deploy(
        &self,
        want: ContainerWant,
        proof: &PreviewProof,
        head: CommitSha,
        at: Timestamp,
    ) -> Result<ContainerWant, Error> {
        self.fixture
            .machine
            .admit_preview_deploy(want, proof, &github(head), at)
            .await
    }
}

fn refused(result: Result<ContainerWant, Error>) -> String {
    match result {
        Err(Error::PreviewDeployRefused(PreviewRefusal::Rule(AuthorityRefusal::Mismatch {
            field,
        }))) => field.to_owned(),
        Err(Error::PreviewDeployRefused(PreviewRefusal::Proof(refusal))) => {
            format!("proof: {refusal}")
        }
        other => panic!("not a preview refusal: {other:?}"),
    }
}

fn after_expiry() -> Timestamp {
    Timestamp::from_millis(now().as_millis() + 3_600_000).unwrap()
}

#[tokio::test]
async fn a_trusted_push_creates_its_preview_in_the_namespace_the_machine_derives() {
    let deploy = Deploy::new().await;
    let proof = deploy.pushed("push");
    let namespace = feature(&deploy.rule);
    deploy
        .deploy(
            want(&namespace, "service_container", "k1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_trusted_push_into_any_other_namespace_is_refused() {
    let deploy = Deploy::new().await;
    let proof = deploy.pushed("elsewhere");
    let other = preview_namespace(deploy.rule.rule, &PreviewKey::Branch("other".to_owned()));
    assert_eq!(
        refused(
            deploy
                .deploy(
                    want(&other, "service_container", "k1", &spec()),
                    &proof,
                    commit(),
                    now(),
                )
                .await
        ),
        "a Namespace"
    );
}

#[tokio::test]
async fn settings_the_rule_does_not_approve_are_refused() {
    let deploy = Deploy::new().await;
    let proof = deploy.pushed("settings");
    let mut changed = spec();
    changed["container"]["environment"]["PLAIN"] = json!("two");
    assert_eq!(
        refused(
            deploy
                .deploy(
                    want(&feature(&deploy.rule), "service_container", "k1", &changed),
                    &proof,
                    commit(),
                    now(),
                )
                .await
        ),
        "a payload"
    );
}

#[tokio::test]
async fn an_accepted_preview_build_creates_again_after_its_token_expires_until_the_head_moves() {
    let deploy = Deploy::new().await;
    let proof = deploy.pushed("accepted");
    let namespace = feature(&deploy.rule);
    let fresh = deploy
        .deploy(
            want(&namespace, "service_container", "k1", &spec()),
            &proof,
            commit(),
            after_expiry(),
        )
        .await;
    assert!(
        refused(fresh).starts_with("proof:"),
        "an expired token alone admits nothing"
    );
    for key in ["k1", "k2"] {
        let at = if key == "k1" { now() } else { after_expiry() };
        deploy
            .deploy(
                want(&namespace, "service_container", key, &spec()),
                &proof,
                commit(),
                at,
            )
            .await
            .unwrap();
    }
    let moved = CommitSha::parse(&"9".repeat(40)).unwrap();
    assert_eq!(
        refused(
            deploy
                .deploy(
                    want(&namespace, "service_container", "k3", &spec()),
                    &proof,
                    moved,
                    after_expiry(),
                )
                .await
        ),
        "a commit"
    );
}

#[tokio::test]
async fn a_push_runs_its_hook_once_in_each_preview() {
    let deploy = Deploy::new().await;
    let proof = deploy.pushed("hook");
    let namespace = feature(&deploy.rule);
    deploy
        .deploy(
            want(&namespace, "pre_deploy_hook", "h1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        refused(
            deploy
                .deploy(
                    want(&namespace, "pre_deploy_hook", "h2", &spec()),
                    &proof,
                    commit(),
                    now(),
                )
                .await
        ),
        "a hook run"
    );
}

/// The fork preview a review names.
fn fork(rule: &PreviewRule) -> Namespace {
    preview_namespace(rule.rule, &PreviewKey::Fork(12))
}

impl Deploy {
    /// A review of the fork preview at the fixture's commit, approving [`spec`] there.
    fn review_body(&self) -> PreviewReview {
        let digest = ployz_core::authority::preview::template(
            &requested(&spec()),
            &fork(&self.rule),
            &self.rule.services[0],
        )
        .unwrap();
        PreviewReview {
            cluster: self.rule.cluster,
            rule: self.rule.rule,
            generation: self.rule.generation,
            key: PreviewKey::Fork(12),
            commit: commit(),
            environment: uuid::Uuid::from_u128(1),
            services: vec![ReviewedService {
                name: web(),
                digest,
            }],
            issued_at: Timestamp::from_millis(now().as_millis() - 1_000).unwrap(),
            expires_at: Timestamp::from_millis(now().as_millis() + 600_000).unwrap(),
        }
    }

    /// A reviewed build's proof under `review`, its image token spent as `jti`.
    fn reviewed(&self, review: Review, jti: &str) -> PreviewProof {
        let claim = self.built(CommitAuthority::Review(review.digest()));
        PreviewProof {
            rule: Signed::sign(&self.rule, &self.fixture.signer).unwrap(),
            review: Some(review),
            token: token(&claim, jti, "workflow_dispatch", "main", ACTOR),
            claim,
            reader: None,
        }
    }

    async fn create(
        &self,
        namespace: &Namespace,
        key: &str,
        spec: &Value,
        proof: &PreviewProof,
        at: Timestamp,
    ) -> Result<ContainerWant, Error> {
        self.deploy(
            want(namespace, "service_container", key, spec),
            proof,
            commit(),
            at,
        )
        .await
    }
}

fn device(deploy: &Deploy, body: &PreviewReview) -> Review {
    Review::Signed(Signed::sign(body, &deploy.fixture.signer).unwrap())
}

#[tokio::test]
async fn a_reviewed_build_creates_only_the_reviewed_settings_in_its_preview() {
    let deploy = Deploy::new().await;
    let namespace = fork(&deploy.rule);
    let proof = deploy.reviewed(device(&deploy, &deploy.review_body()), "reviewed");
    deploy
        .create(&namespace, "k1", &spec(), &proof, now())
        .await
        .unwrap();
    let mut edited = spec();
    edited["container"]["environment"]["PLAIN"] = json!("two");
    assert_eq!(
        refused(
            deploy
                .create(&namespace, "k2", &edited, &proof, now())
                .await
        ),
        "a payload"
    );
    assert_eq!(
        refused(
            deploy
                .create(&feature(&deploy.rule), "k3", &spec(), &proof, now())
                .await
        ),
        "a Namespace"
    );
}

#[tokio::test]
async fn a_reviewed_build_without_its_review_deploys_nothing() {
    let deploy = Deploy::new().await;
    let namespace = fork(&deploy.rule);
    let mut proof = deploy.reviewed(device(&deploy, &deploy.review_body()), "bare");
    proof.review = None;
    assert_eq!(
        refused(
            deploy
                .create(&namespace, "k1", &spec(), &proof, now())
                .await
        ),
        "a review"
    );
    let mut other = deploy.review_body();
    other.commit = CommitSha::parse(&"7".repeat(40)).unwrap();
    let mut swapped = deploy.reviewed(device(&deploy, &deploy.review_body()), "swapped");
    swapped.review = Some(device(&deploy, &other));
    assert_eq!(
        refused(
            deploy
                .create(&namespace, "k2", &spec(), &swapped, now())
                .await
        ),
        "a review"
    );
}

/// A copy made after the review's window, on a Machine that admitted the build while it
/// held, still runs what was reviewed. One that never admitted it refuses.
#[tokio::test]
async fn a_reviewed_preview_starts_again_after_its_review_expires() {
    let deploy = Deploy::new().await;
    let namespace = fork(&deploy.rule);
    let proof = deploy.reviewed(device(&deploy, &deploy.review_body()), "later");
    assert!(matches!(
        deploy
            .create(&namespace, "k1", &spec(), &proof, after_expiry())
            .await,
        Err(Error::PreviewDeployRefused(PreviewRefusal::Rule(
            AuthorityRefusal::Expired
        )))
    ));
    deploy
        .create(&namespace, "k1", &spec(), &proof, now())
        .await
        .unwrap();
    deploy
        .create(&namespace, "k2", &spec(), &proof, after_expiry())
        .await
        .unwrap();
}

/// Operator decision 6 at creation: a session reviews only a preview that opens nothing.
#[tokio::test]
async fn a_session_review_deploys_only_a_preview_that_opens_no_secret() {
    let mut deploy = Deploy::new().await;
    let namespace = fork(&deploy.rule);
    let session = |deploy: &Deploy| {
        super::super::preview_grant::tests::session_review(&deploy.rule, &deploy.review_body())
    };
    deploy
        .create(
            &namespace,
            "k1",
            &spec(),
            &deploy.reviewed(session(&deploy), "open"),
            now(),
        )
        .await
        .unwrap();
    deploy.rule.services[0].inherit = ["NPM_TOKEN".to_owned()].into();
    deploy.rule.generation += 1;
    assert_eq!(
        refused(
            deploy
                .create(
                    &namespace,
                    "k2",
                    &spec(),
                    &deploy.reviewed(session(&deploy), "secret"),
                    now(),
                )
                .await
        ),
        "the signer"
    );
}
