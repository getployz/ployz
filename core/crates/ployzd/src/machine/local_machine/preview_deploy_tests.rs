#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use ployz_core::{
    Namespace,
    authority::{AttestKey, ImageDigest, PreviewRule, Signed, preview::preview_namespace},
};
use serde_json::{Value, json};

use super::super::{
    preview_grant::tests::{ACTOR, claim, inherited, preview_rule, review, token},
    rule_grant::tests::{Fixture, commit, fixture, github, now},
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

#[tokio::test]
async fn a_reviewed_build_creates_only_with_the_settings_the_review_signed() {
    let deploy = Deploy::new().await;
    let namespace = preview_namespace(deploy.rule.rule, &PreviewKey::Fork(12));
    let mut body = review(&deploy.fixture, &deploy.rule, commit());
    let ployz_core::authority::Review::Signed(signed) = &body else {
        unreachable!("the fixture review is Device-signed");
    };
    let mut reviewed: ployz_core::authority::PreviewReview =
        ployz_core::authority::canonical::from_canonical(&signed.body).unwrap();
    let mut service = deploy.rule.services[0].clone();
    service.generated.clear();
    reviewed.services[0].digest =
        ployz_core::authority::preview::template(&requested(&spec()), &namespace, &service)
            .unwrap();
    body = ployz_core::authority::Review::Signed(
        Signed::sign(&reviewed, &deploy.fixture.signer).unwrap(),
    );
    let claim = deploy.built(CommitAuthority::Review(body.digest()));
    let proof = PreviewProof {
        rule: Signed::sign(&deploy.rule, &deploy.fixture.signer).unwrap(),
        review: Some(body),
        token: token(&claim, "reviewed", "workflow_dispatch", "main", ACTOR),
        claim,
        reader: None,
    };
    deploy
        .deploy(
            want(&namespace, "service_container", "k1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
    let mut changed = spec();
    changed["container"]["environment"]["PLAIN"] = json!("two");
    assert_eq!(
        refused(
            deploy
                .deploy(
                    want(&namespace, "service_container", "k2", &changed),
                    &proof,
                    commit(),
                    now(),
                )
                .await
        ),
        "a payload"
    );
}
