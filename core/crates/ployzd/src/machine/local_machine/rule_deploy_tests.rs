#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use ployz_core::{
    DeviceKey, GithubToken, Namespace, ServiceName,
    authority::{
        AttestKey, AuthorityRefusal, Budget, BuildClaim, BuildStage, BuiltImageDigest, CommitSha,
        RuleEvidence, RuleProof, Signed, Timestamp, VerifiedBuild, rule::Builder,
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
                image: BuiltImageDigest::parse(&format!("sha256:{IMAGE}")).unwrap(),
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
            evidence: RuleEvidence::Online(GithubToken::new(token)),
            claim: claim.clone(),
            reader: None,
        }
    }

    /// `proof` with `verified` in place of its token.
    fn offline(proof: &RuleProof, verified: VerifiedBuild) -> RuleProof {
        RuleProof {
            evidence: RuleEvidence::Verified(verified),
            ..proof.clone()
        }
    }

    /// Admit `want` under `proof`, checked online now, and answer the verified build this
    /// Machine issued.
    async fn issued(&self, want: ContainerWant, proof: &RuleProof) -> VerifiedBuild {
        self.machine
            .admit_rule_deploy(
                &want,
                proof,
                &github(commit()),
                now(),
                &mut Budget::default(),
            )
            .await
            .unwrap()
            .verified
            .expect("an online check issues a verified build")
    }

    async fn deploy(
        &self,
        want: ContainerWant,
        proof: &RuleProof,
        head: CommitSha,
        at: Timestamp,
    ) -> Result<(), Error> {
        self.machine
            .admit_rule_deploy(&want, proof, &github(head), at, &mut Budget::default())
            .await
            .map(drop)
    }
}

fn refused(result: Result<(), Error>) -> String {
    match result {
        Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
            field,
        }))) => field.to_string(),
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
            image: BuiltImageDigest::from_bytes([0x66; 32]),
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

fn moved() -> CommitSha {
    CommitSha::parse(&"9".repeat(40)).unwrap()
}

#[tokio::test]
async fn a_verified_build_creates_after_its_token_expired_and_the_branch_moved() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "verified"));
    let verified = fixture
        .issued(want("service_container", "k1", &spec()), &proof)
        .await;
    assert!(
        refused(
            fixture
                .deploy(
                    want("service_container", "k2", &spec()),
                    &proof,
                    moved(),
                    after_expiry()
                )
                .await
        )
        .starts_with("proof:"),
        "an expired token alone admits nothing"
    );
    fixture
        .deploy(
            want("service_container", "k2", &spec()),
            &Fixture::offline(&proof, verified),
            moved(),
            after_expiry(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_verified_build_stands_only_for_its_own_build_service_and_generation() {
    let fixture = fixture().await;
    let mut rule = fixture.deploy_rule(3);
    // The rule covers a second Service with the same settings, so only the verified
    // build tells their builds apart.
    let api = ServiceName::parse("api").unwrap();
    let mut api_spec = spec();
    api_spec["name"] = json!("api");
    let mut covered = rule.services[0].clone();
    covered.name = api.clone();
    covered.template = ployz_core::authority::rule::template(
        &requested(&api_spec),
        &Namespace::parse("app").unwrap(),
    )
    .unwrap();
    rule.services.insert(0, covered);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "own"));
    let verified = fixture
        .issued(want("service_container", "k1", &spec()), &proof)
        .await;
    let offline = |claim: &BuildClaim, verified| RuleProof {
        claim: claim.clone(),
        ..Fixture::offline(&proof, verified)
    };
    let fixture = &fixture;
    let creates = |spec: &Value, proof: RuleProof| {
        let want = want("service_container", "k2", spec);
        async move { fixture.deploy(want, &proof, moved(), after_expiry()).await }
    };

    let forged = VerifiedBuild::from_bytes([0x5a; 32]);
    assert_eq!(
        refused(creates(&spec(), offline(&claim, forged)).await),
        "a verified build"
    );
    let other_image = "6".repeat(64);
    let mut other_spec = spec();
    other_spec["container"]["image"] = json!(format!("ployz-build/web@sha256:{other_image}"));
    let other = BuildClaim {
        stage: BuildStage::Built {
            image: BuiltImageDigest::parse(&format!("sha256:{other_image}")).unwrap(),
            attest: fixture.attest(),
        },
        ..claim.clone()
    };
    assert_eq!(
        refused(creates(&other_spec, offline(&other, verified)).await),
        "a verified build"
    );
    let mut api_want = want("service_container", "k3", &api_spec);
    if let ContainerWant::Running {
        container: RunTarget::New(new),
        ..
    } = &mut api_want
    {
        new.slot.service = api.clone();
    }
    let moved_to_api = BuildClaim {
        service: api,
        ..claim.clone()
    };
    assert_eq!(
        refused(
            fixture
                .deploy(
                    api_want,
                    &offline(&moved_to_api, verified),
                    moved(),
                    after_expiry()
                )
                .await
        ),
        "a verified build"
    );

    // Approving the rule again: generation 4 reaches this Machine with its first push.
    let mut next = rule.clone();
    next.generation = 4;
    let claim4 = fixture.built(&next, fixture.attest());
    fixture
        .issued(
            want("service_container", "k4", &spec()),
            &fixture.proof(&next, &claim4, token_for(&claim4, "next")),
        )
        .await;
    assert_eq!(
        refused(creates(&spec(), offline(&claim, verified)).await),
        "a generation",
        "the older approval's proof, as Cloud kept it"
    );
    let under_next = RuleProof {
        rule: Signed::sign(&next, &fixture.signer).unwrap(),
        ..offline(&claim4, verified)
    };
    assert_eq!(
        refused(creates(&spec(), under_next).await),
        "a verified build",
        "the older build's MAC under the newer approval"
    );
}

#[tokio::test]
async fn a_verified_build_stops_once_the_rules_signer_is_removed() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "removed"));
    let verified = fixture
        .issued(want("service_container", "k1", &spec()), &proof)
        .await;
    let device = DeviceKey::from(fixture.signer.public()).code();
    fixture
        .machine
        .owner
        .mutate(move |store| {
            store.remove_by_code(device).unwrap();
        })
        .await
        .unwrap();
    let result = fixture
        .deploy(
            want("service_container", "k2", &spec()),
            &Fixture::offline(&proof, verified),
            moved(),
            after_expiry(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(
                AuthorityRefusal::UnknownKey { .. }
            )))
        ),
        "{result:?}"
    );
}

/// No retired builder can build, so the verified build here is made directly: the check
/// is that a Server refuses the build before it trusts any MAC.
#[tokio::test]
async fn a_verified_build_under_a_retired_builder_is_refused() {
    let fixture = fixture().await;
    let lab = "a19b4fca3adea371c25ec29f181e8c7d21037d1e";
    let mut rule = fixture.deploy_rule(3);
    rule.source.builder = Builder {
        r#ref: format!("getployz/build/.github/workflows/build.yml@{lab}"),
        sha: CommitSha::parse(lab).unwrap(),
    };
    let claim = fixture.built(&rule, fixture.attest());
    let signed = Signed::sign(&rule, &fixture.signer).unwrap();
    let key = fixture
        .machine
        .held_sealing_key(fixture.machine.record().phase())
        .unwrap();
    let verified = issue(
        &CheckedOnline {
            rule: &signed,
            claim: &claim,
        },
        &key,
    )
    .unwrap();
    let result = fixture
        .deploy(
            want("service_container", "k1", &spec()),
            &RuleProof {
                rule: signed,
                evidence: RuleEvidence::Verified(verified),
                claim,
                reader: None,
            },
            moved(),
            after_expiry(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(
                AuthorityRefusal::RetiredBuilder { .. }
            )))
        ),
        "{result:?}"
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
}

#[tokio::test]
async fn what_a_rule_admitted_stops_standing_once_a_newer_generation_arrives() {
    let fixture = fixture().await;
    let old = fixture.deploy_rule(3);
    let claim = fixture.built(&old, fixture.attest());
    let by = fixture
        .machine
        .admit_rule_deploy(
            &want("service_container", "k1", &spec()),
            &fixture.proof(&old, &claim, token_for(&claim, "old")),
            &github(commit()),
            now(),
            &mut Budget::default(),
        )
        .await
        .unwrap()
        .by;
    let standing = fixture.machine.standing();
    assert_eq!(
        standing.admits(&by, std::convert::identity).unwrap(),
        Ok(())
    );

    let new = fixture.deploy_rule(4);
    let claim4 = fixture.built(&new, fixture.attest());
    fixture
        .deploy(
            want("service_container", "k2", &spec()),
            &fixture.proof(&new, &claim4, token_for(&claim4, "new")),
            commit(),
            now(),
        )
        .await
        .unwrap();

    assert_eq!(
        standing.admits(&by, std::convert::identity).unwrap(),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Generation
        }),
        "a Container made under generation 3 neither starts nor is created once 4 arrived"
    );
}

/// GitHub that answers the read at `at` only once `release` fires, telling `reached` it
/// was asked.
struct Slow {
    inner: super::super::rule_grant::FixedGithub,
    at: SlowAt,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[derive(PartialEq)]
enum SlowAt {
    Head,
    Token,
}

impl Slow {
    fn new(inner: super::super::rule_grant::FixedGithub, at: SlowAt) -> Self {
        Self {
            inner,
            at,
            reached: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }

    async fn wait(&self, at: SlowAt) {
        if self.at == at {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

impl Github for Slow {
    async fn claims(
        &self,
        rule: &impl ployz_core::authority::ClaimRule,
        token: &str,
        now: Timestamp,
    ) -> Result<ployz_core::authority::GithubClaims, crate::github::oidc::ProofRefusal> {
        self.wait(SlowAt::Token).await;
        self.inner.claims(rule, token, now).await
    }

    async fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> Result<CommitSha, crate::github::head::HeadError> {
        self.wait(SlowAt::Head).await;
        self.inner.head(repository_id, branch, reader).await
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
    let slow = Slow::new(github(commit()), SlowAt::Head);
    let first = want("service_container", "k1", &spec());
    let mut budget = Budget::default();
    let (in_flight, ()) = tokio::join!(
        fixture
            .machine
            .admit_rule_deploy(&first, &stale, &slow, now(), &mut budget),
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
    assert_eq!(refused(in_flight.map(drop)), "a generation");
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

#[tokio::test]
async fn a_new_builds_token_is_charged_to_the_same_budget_as_the_rule() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "built-1"));
    let attempt = |left: usize| {
        let mut budget = Budget::default();
        for _ in left..8 {
            budget.reserve_one().unwrap();
        }
        budget
    };
    let mut one_left = attempt(1);
    let result = fixture
        .machine
        .admit_rule_deploy(
            &want("service_container", "k1", &spec()),
            &proof,
            &github(commit()),
            now(),
            &mut one_left,
        )
        .await;
    assert!(
        matches!(
            result,
            Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(
                AuthorityRefusal::Overspent
            )))
        ),
        "{result:?}"
    );
    let mut two_left = attempt(2);
    fixture
        .machine
        .admit_rule_deploy(
            &want("service_container", "k1", &spec()),
            &proof,
            &github(commit()),
            now(),
            &mut two_left,
        )
        .await
        .unwrap();
}
