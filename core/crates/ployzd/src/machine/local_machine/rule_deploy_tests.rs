#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use ployz_core::{
    GithubToken, Namespace,
    authority::{
        AttestKey, AuthorityRefusal, BuildClaim, BuildStage, CommitSha, ImageDigest, RuleProof,
        Signed, Timestamp,
    },
};
use serde_json::{Value, json};

use super::super::rule_grant::tests::{
    Fixture, claim_for, commit, fixture, github, now, rule_for, token_for,
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

fn want(kind: &str, key: &str, spec: &Value) -> ContainerWant {
    let resolved = requested(spec)
        .to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    serde_json::from_value(
        json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot":{"namespace":"app", "service":"web", "kind":kind, "creation_key":key},
        "resolved_spec":resolved, "gen":1}}}),
    )
    .unwrap()
}

impl Fixture {
    fn attest(&self) -> AttestKey {
        self.machine
            .held_attest(self.machine.record().phase())
            .unwrap()
    }

    /// The fixture's rule at `generation`, approving `spec()`'s settings.
    fn deploy_rule(&self, generation: u64) -> ployz_core::authority::AutoDeployRule {
        let mut rule = rule_for(
            self.cluster,
            self.machine.record().id(),
            &self.signer,
            &self.record,
        );
        rule.generation = generation;
        rule.services[0].template = ployz_core::authority::rule::template(
            &requested(&spec()),
            &Namespace::parse("app").unwrap(),
        )
        .unwrap();
        rule
    }

    /// The image claim of the head's build under `rule`, sealed from `attest`.
    fn built(&self, rule: &ployz_core::authority::AutoDeployRule, attest: AttestKey) -> BuildClaim {
        BuildClaim {
            stage: BuildStage::Built {
                image: ImageDigest::parse(&format!("sha256:{IMAGE}")).unwrap(),
                attest,
            },
            ..claim_for(self.cluster, rule)
        }
    }

    fn proof(
        &self,
        rule: &ployz_core::authority::AutoDeployRule,
        claim: &BuildClaim,
        token: String,
    ) -> RuleProof {
        RuleProof {
            rule: Signed::sign(rule, &self.signer).unwrap(),
            token: GithubToken::new(token),
            claim: claim.clone(),
            reader: None,
        }
    }

    async fn deploy(
        &self,
        want: ContainerWant,
        proof: &RuleProof,
        head: CommitSha,
        at: Timestamp,
    ) -> Result<ContainerWant, Error> {
        self.machine
            .admit_rule_deploy(want, proof, &github(head), at)
            .await
    }
}

fn refused(result: Result<ContainerWant, Error>) -> String {
    match result {
        Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
            field,
        }))) => field.to_owned(),
        Err(Error::RuleDeployRefused(RuleDeployRefusal::Proof(refusal))) => {
            format!("proof: {refusal}")
        }
        other => panic!("not a rule refusal: {other:?}"),
    }
}

fn after_expiry() -> Timestamp {
    Timestamp::from_millis(now().as_millis() + 3_600_000).unwrap()
}

#[tokio::test]
async fn a_rule_creates_the_heads_image_with_the_settings_it_approves() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "built-1"));
    fixture
        .deploy(
            want("service_container", "k1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_image_token_that_names_a_foreign_attest_key_is_refused() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, AttestKey::from_bytes([0x77; 32]));
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "foreign"));
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k1", &spec()),
                    &proof,
                    commit(),
                    now()
                )
                .await
        ),
        "the attest key"
    );
}

#[tokio::test]
async fn a_claim_the_token_does_not_name_is_refused_as_the_audience() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let other = BuildClaim {
        stage: BuildStage::Built {
            image: ImageDigest::from_bytes([0x66; 32]),
            attest: fixture.attest(),
        },
        ..claim.clone()
    };
    // Cloud relays the token of one image with the claim of another.
    let proof = fixture.proof(&rule, &claim, token_for(&other, "swap"));
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k1", &spec()),
                    &proof,
                    commit(),
                    now()
                )
                .await
        ),
        "an audience"
    );
}

#[tokio::test]
async fn an_accepted_build_creates_again_after_its_token_expires_until_the_head_moves() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "accepted"));
    let fresh = fixture
        .deploy(
            want("service_container", "k1", &spec()),
            &proof,
            commit(),
            after_expiry(),
        )
        .await;
    assert!(
        refused(fresh).starts_with("proof:"),
        "an expired token alone admits nothing"
    );
    fixture
        .deploy(
            want("service_container", "k1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
    fixture
        .deploy(
            want("service_container", "k2", &spec()),
            &proof,
            commit(),
            after_expiry(),
        )
        .await
        .unwrap();
    let moved = CommitSha::parse(&"9".repeat(40)).unwrap();
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k3", &spec()),
                    &proof,
                    moved,
                    after_expiry()
                )
                .await
        ),
        "a commit"
    );
}

#[tokio::test]
async fn a_push_runs_its_hook_once() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "hook"));
    fixture
        .deploy(
            want("pre_deploy_hook", "h1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
    // A resend of the same slot adopts the hook that ran.
    fixture
        .deploy(
            want("pre_deploy_hook", "h1", &spec()),
            &proof,
            commit(),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("pre_deploy_hook", "h2", &spec()),
                    &proof,
                    commit(),
                    now()
                )
                .await
        ),
        "a hook run"
    );
}

#[tokio::test]
async fn a_newer_generation_refuses_every_proof_below_it() {
    let fixture = fixture().await;
    let old = fixture.deploy_rule(3);
    let claim = fixture.built(&old, fixture.attest());
    let stale = fixture.proof(&old, &claim, token_for(&claim, "old"));
    let new = fixture.deploy_rule(4);
    let claim4 = fixture.built(&new, fixture.attest());
    let current = fixture.proof(&new, &claim4, token_for(&claim4, "new"));
    fixture
        .deploy(
            want("service_container", "k1", &spec()),
            &current,
            commit(),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k2", &spec()),
                    &stale,
                    commit(),
                    now()
                )
                .await
        ),
        "a generation"
    );
    // A person's client reads the floor it refused below, to refuse the same rule.
    let details = fixture
        .machine
        .inspect(ployz_core::InspectRequest::default())
        .await
        .unwrap();
    assert_eq!(
        details.rule_generations,
        std::collections::BTreeMap::from([(new.rule, 4)])
    );
}

/// GitHub that answers the head read only once `release` fires, telling `reached` it
/// was asked.
struct SlowHead {
    inner: super::super::rule_grant::FixedGithub,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Github for SlowHead {
    async fn claims(
        &self,
        rule: &ployz_core::authority::AutoDeployRule,
        token: &str,
        now: Timestamp,
    ) -> Result<ployz_core::authority::GithubClaims, crate::github::oidc::ProofRefusal> {
        self.inner.claims(rule, token, now).await
    }

    async fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> Result<CommitSha, crate::github::head::HeadError> {
        self.reached.notify_one();
        self.release.notified().await;
        self.inner.head(repository_id, branch, reader).await
    }

    async fn ancestry(
        &self,
        repository_id: u64,
        commit: CommitSha,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> Result<crate::github::head::Ancestry, crate::github::head::HeadError> {
        self.inner
            .ancestry(repository_id, commit, branch, reader)
            .await
    }
}

#[tokio::test]
async fn a_newer_generation_landing_during_the_head_read_refuses_the_older_one() {
    let fixture = fixture().await;
    let old = fixture.deploy_rule(3);
    let claim = fixture.built(&old, fixture.attest());
    let stale = fixture.proof(&old, &claim, token_for(&claim, "old"));
    let new = fixture.deploy_rule(4);
    let claim4 = fixture.built(&new, fixture.attest());
    let current = fixture.proof(&new, &claim4, token_for(&claim4, "new"));
    let slow = SlowHead {
        inner: github(commit()),
        reached: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    };
    let (in_flight, ()) = tokio::join!(
        fixture.machine.admit_rule_deploy(
            want("service_container", "k1", &spec()),
            &stale,
            &slow,
            now()
        ),
        async {
            slow.reached.notified().await;
            fixture
                .deploy(
                    want("service_container", "k2", &spec()),
                    &current,
                    commit(),
                    now(),
                )
                .await
                .unwrap();
            slow.release.notify_one();
        }
    );
    assert_eq!(refused(in_flight), "a generation");
}

#[tokio::test]
async fn an_ended_rule_deploys_nothing() {
    let fixture = fixture().await;
    let mut rule = fixture.deploy_rule(4);
    rule.ended = true;
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "ended"));
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k1", &spec()),
                    &proof,
                    commit(),
                    now()
                )
                .await
        ),
        "an ended rule"
    );
}

#[tokio::test]
async fn a_changed_setting_is_refused_as_the_payload() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "payload"));
    let mut privileged = spec();
    privileged["container"]["privileged"] = json!(true);
    assert_eq!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k1", &privileged),
                    &proof,
                    commit(),
                    now()
                )
                .await
        ),
        "a payload"
    );
}
