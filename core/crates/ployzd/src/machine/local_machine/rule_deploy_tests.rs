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
    Fixture, claim_for, commit, fixture, github, github_mut, now, rule_for, token_for,
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
            claim: Some(claim.clone()),
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
                (&github(commit()), &NoRegistry),
                now(),
                &mut Budget::default(),
                Admit::Record,
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
            .admit_rule_deploy(
                &want,
                proof,
                (&github(head), &NoRegistry),
                at,
                &mut Budget::default(),
                Admit::Record,
            )
            .await
            .map(drop)
    }
}

impl Fixture {
    /// Admit the hook in slot `key` under `proof` and take its place, as its create does.
    async fn create_hook(&self, key: &str, proof: &RuleProof) -> Result<(), Error> {
        let admitted = self
            .machine
            .admit_rule_deploy(
                &want("pre_deploy_hook", key, &spec()),
                proof,
                (&github(commit()), &NoRegistry),
                now(),
                &mut Budget::default(),
                Admit::Record,
            )
            .await?;
        admitted
            .hook
            .expect("a hook's admission names its place")
            .take(&self.machine.approvals)
    }
}

/// A registry no GitHub rule reaches: its pull is a bug.
struct NoRegistry;

impl Registry for NoRegistry {
    async fn pull(
        &self,
        pin: &ployz_core::authority::RegistryPin,
        _login: Option<&ployz_core::SealedLogin>,
    ) -> Result<ployz_core::ImageDigest, Error> {
        panic!("a GitHub rule pulled {}", pin.reference())
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
        claim: Some(claim.clone()),
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
            store
                .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device)
                .unwrap();
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
    github_mut(&mut rule).builder = Builder {
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
                claim: Some(claim),
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
    fixture.create_hook("h1", &proof).await.unwrap();
    // A resend of the same slot adopts the hook that ran.
    fixture.create_hook("h1", &proof).await.unwrap();
    assert_eq!(
        refused(fixture.create_hook("h2", &proof).await),
        "a hook run"
    );
}

/// A hook a rule admitted whose create never ran, as when its caller's key went before the
/// create's turn, leaves its push's place: a valid retry in another slot is admitted.
#[tokio::test]
async fn a_hook_admitted_without_a_create_leaves_its_place() {
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
    fixture.create_hook("h2", &proof).await.unwrap();
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
            (&github(commit()), &NoRegistry),
            now(),
            &mut Budget::default(),
            Admit::Record,
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
        rule: &ployz_core::authority::AutoDeployRule,
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
        fixture.machine.admit_rule_deploy(
            &first,
            &stale,
            (&slow, &NoRegistry),
            now(),
            &mut budget,
            Admit::Record,
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
            (&github(commit()), &NoRegistry),
            now(),
            &mut one_left,
            Admit::Record,
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
            (&github(commit()), &NoRegistry),
            now(),
            &mut two_left,
            Admit::Record,
        )
        .await
        .unwrap();
}

/// `web` from the rule's image, mounting the Provisioned Volume `data` born in `app`.
fn mounting_data() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": format!("ployz-build/web@sha256:{IMAGE}"), "pull_policy": "missing"},
        "volumes": [{"reference": "data", "source": {"kind": "provisioned", "name": "data",
            "maximum_bytes": 1_073_741_824_u64, "birth": {"namespace": "app", "name": "data",
            "made_at": 1_762_178_640_000_u64, "nonce": "00112233445566778899aabbccddeeff"}}}],
        "mounts": [{"volume": "data", "target": "/data"}],
    })
}

/// A Move's Create of [`mounting_data`] under `proof`, as a check when `check`.
fn moved_start(proof: &RuleProof, check: bool) -> ployz_core::Start {
    let app = Namespace::parse("app").unwrap();
    let mut scoped = requested(&mounting_data());
    scoped.mount_graph = scoped.mount_graph.scope_to_namespace(&app).unwrap();
    let resolved = scoped
        .to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    serde_json::from_value(json!({"kind": "create", "namespace": "app",
        "resolved_spec": resolved, "creation_key": "move-1", "gen": 2, "rotation": "pending",
        "rule": proof, "check": check}))
    .unwrap()
}

impl Fixture {
    /// An Operator's start of `start` on `app_data`, and the rule it was admitted under.
    async fn start_moved(&self, start: &ployz_core::Start) -> Result<(), Error> {
        let admitted = self
            .machine
            .clone()
            .with_zpool("true")
            .at_operator_scope()
            .admit_volume_start(&"app_data".parse().unwrap(), start)
            .await?
            .expect("a Create's admission");
        assert!(matches!(admitted.by(), AdmittedBy::Rule { .. }));
        Ok(())
    }
}

/// A Move carries the verified build Cloud kept from the holder's deploy; this Machine
/// takes it only for the build it verified and only when the rule names this Machine.
#[tokio::test]
async fn a_moved_volume_starts_under_a_verified_rule_only_where_the_rule_places_it() {
    let fixture = fixture().await;
    let app = Namespace::parse("app").unwrap();
    let mut rule = fixture.deploy_rule(3);
    rule.services[0].template =
        ployz_core::authority::rule::template(&requested(&mounting_data()), &app).unwrap();
    let claim = fixture.built(&rule, fixture.attest());
    let online = fixture.proof(&rule, &claim, token_for(&claim, "moved"));
    let holder = moved_start(&online, false);
    let ployz_core::Start::Create { resolved_spec, .. } = &holder else {
        unreachable!("a Create");
    };
    let mut issuing = want("service_container", "k1", &spec());
    if let ContainerWant::Running {
        container: RunTarget::New(new),
        ..
    } = &mut issuing
    {
        new.resolved_spec = resolved_spec.clone();
    }
    let verified = fixture.issued(issuing, &online).await;
    let proof = Fixture::offline(&online, verified);
    fixture
        .start_moved(&moved_start(&proof, false))
        .await
        .unwrap();

    let forged = Fixture::offline(&online, VerifiedBuild::from_bytes([0x5a; 32]));
    assert_eq!(
        refused(fixture.start_moved(&moved_start(&forged, false)).await),
        "a verified build"
    );

    let mut elsewhere = rule.clone();
    elsewhere.machines = vec![ployz_core::MachineId::parse("1".repeat(32)).unwrap()];
    let off_list = RuleProof {
        rule: Signed::sign(&elsewhere, &fixture.signer).unwrap(),
        ..proof.clone()
    };
    assert_eq!(
        refused(fixture.start_moved(&moved_start(&off_list, false)).await),
        "a Machine"
    );

    // A check carrying a newer generation marks nothing, so the holder's rule still stands.
    let mut next = rule.clone();
    next.generation = 4;
    let swapped = RuleProof {
        rule: Signed::sign(&next, &fixture.signer).unwrap(),
        ..proof.clone()
    };
    assert_eq!(
        refused(fixture.start_moved(&moved_start(&swapped, true)).await),
        "a generation"
    );
    assert_eq!(
        fixture
            .machine
            .approvals
            .rule_generation(rule.rule, Error::ApprovalRecord)
            .unwrap(),
        Some(3)
    );
    fixture
        .start_moved(&moved_start(&proof, true))
        .await
        .unwrap();
}

/// A rule deploy through this Server's own socket is admitted under the rule, as from
/// anywhere else, so its copy keeps its name and the next deploy under the rule finds it
/// signed instead of replacing it again.
#[tokio::test]
async fn a_rule_deploy_through_the_servers_socket_is_admitted_under_the_rule() {
    let fixture = fixture().await;
    let rule = fixture.deploy_rule(3);
    let claim = fixture.built(&rule, fixture.attest());
    let online = fixture.proof(&rule, &claim, token_for(&claim, "socket"));
    let verified = fixture
        .issued(want("service_container", "k1", &spec()), &online)
        .await;
    let admitted = fixture
        .machine
        .clone()
        .on_local_socket()
        .admit_spec(
            &want("service_container", "k2", &spec()),
            Some(&Fixture::offline(&online, verified)),
            &[],
            Admit::Record,
        )
        .await
        .unwrap()
        .expect("a new Container's admission");
    assert!(
        matches!(admitted.by(), AdmittedBy::Rule { .. }),
        "{admitted:?}"
    );
    assert_eq!(admitted.admission(), ployz_core::Admission::Signed);
}

/// The image ID a registry pull places, which differs from the manifest digest it named.
const PLACED: &str = "5555555555555555555555555555555555555555555555555555555555555555";

fn registry_spec(service: &str, digest: &str) -> Value {
    let mut spec = spec();
    spec["name"] = json!(service);
    spec["container"]["image"] = json!(format!("ghcr.io/acme/web@sha256:{digest}"));
    spec
}

fn placed() -> ployz_core::ImageDigest {
    ployz_core::ImageDigest::parse(format!("sha256:{PLACED}")).unwrap()
}

/// A registry that serves `answered` under every digest, or the digest asked for, and
/// places [`PLACED`]. A newer rule in `landing` lands on the Machine while it pulls.
#[derive(Default)]
struct FixedRegistry<'a> {
    answered: Option<String>,
    landing: Option<(&'a Fixture, ployz_core::authority::AutoDeployRule)>,
    pulled: std::sync::Mutex<Vec<String>>,
}

impl Registry for FixedRegistry<'_> {
    async fn pull(
        &self,
        pin: &ployz_core::authority::RegistryPin,
        _login: Option<&ployz_core::SealedLogin>,
    ) -> Result<ployz_core::ImageDigest, Error> {
        self.pulled.lock().unwrap().push(pin.reference());
        if let Some((fixture, newer)) = &self.landing {
            fixture
                .machine
                .approvals
                .mark_rule(newer, Admit::Record, |refusal| {
                    Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal))
                })
                .unwrap();
        }
        match &self.answered {
            Some(answered) => Err(pull_refused(
                crate::docker_image::RegistryPull::OtherDigest {
                    answered: answered.clone(),
                },
            )),
            None => Ok(placed()),
        }
    }
}

impl Fixture {
    /// The fixture's rule at `generation` as a registry rule for `ghcr.io/acme/web`,
    /// approving `services`' settings.
    fn registry_rule(
        &self,
        generation: u64,
        services: &[&str],
    ) -> ployz_core::authority::AutoDeployRule {
        let mut rule = self.deploy_rule(generation);
        rule.source =
            ployz_core::authority::RuleSource::Registry(ployz_core::authority::RegistrySource {
                host: "ghcr.io".to_owned(),
                repository: "acme/web".to_owned(),
            });
        rule.services = services
            .iter()
            .map(|name| ployz_core::authority::RuleService {
                name: ServiceName::parse(*name).unwrap(),
                template: ployz_core::authority::rule::template(
                    &requested(&registry_spec(name, IMAGE)),
                    &Namespace::parse("app").unwrap(),
                )
                .unwrap(),
                policy: None,
                secrets: None,
            })
            .collect();
        rule
    }

    fn registry_proof(&self, rule: &ployz_core::authority::AutoDeployRule) -> RuleProof {
        RuleProof {
            rule: Signed::sign(rule, &self.signer).unwrap(),
            evidence: RuleEvidence::Registry,
            claim: None,
            reader: None,
        }
    }

    async fn pull_deploy(
        &self,
        want: &ContainerWant,
        proof: &RuleProof,
        registry: &FixedRegistry<'_>,
    ) -> Result<RuleAdmitted, Error> {
        self.machine
            .admit_rule_deploy(
                want,
                proof,
                (&github(commit()), registry),
                now(),
                &mut Budget::default(),
                Admit::Record,
            )
            .await
    }

    /// Admit `service`'s hook for image `digest` in slot `key` under `proof`, and take
    /// its place, as its create does.
    async fn registry_hook(
        &self,
        (service, digest): (&str, &str),
        key: &str,
        proof: &RuleProof,
    ) -> Result<(), Error> {
        let mut want = want("pre_deploy_hook", key, &registry_spec(service, digest));
        if let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = &mut want
        {
            new.slot.service = ServiceName::parse(service).unwrap();
        }
        self.pull_deploy(&want, proof, &FixedRegistry::default())
            .await?
            .hook
            .expect("a hook's admission names its place")
            .take(&self.machine.approvals)
    }
}

fn refused_admission(result: Result<RuleAdmitted, Error>) -> String {
    refused(result.map(drop))
}

// Catches a registry rule's Container created, adopted or started from whatever image
// this Server holds under the name, rather than the one its own pull placed.
#[tokio::test]
async fn a_registry_rule_runs_the_image_its_own_pull_placed() {
    let fixture = fixture().await;
    let rule = fixture.registry_rule(3, &["web"]);
    let proof = fixture.registry_proof(&rule);
    let registry = FixedRegistry::default();
    let admitted = fixture
        .pull_deploy(
            &want("service_container", "k1", &registry_spec("web", IMAGE)),
            &proof,
            &registry,
        )
        .await
        .unwrap();
    assert_eq!(admitted.pin, Some(placed()));
    assert_eq!(
        *registry.pulled.lock().unwrap(),
        [format!("ghcr.io/acme/web@sha256:{IMAGE}")],
        "the Server pulls the exact digest, never a tag"
    );
    assert!(admitted.hook.is_none() && admitted.verified.is_none());
}

// Catches a caller's pin naming a local image under the same repository spelling winning
// over the Server's own pull.
#[tokio::test]
async fn a_caller_pin_that_names_other_content_is_refused() {
    let fixture = fixture().await;
    let proof = fixture.registry_proof(&fixture.registry_rule(3, &["web"]));
    let mut local = want("service_container", "k1", &registry_spec("web", IMAGE));
    let ContainerWant::Running {
        container: RunTarget::New(new),
        ..
    } = &mut local
    else {
        unreachable!()
    };
    new.image_id =
        Some(ployz_core::ImageDigest::parse(format!("sha256:{}", "6".repeat(64))).unwrap());
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(&local, &proof, &FixedRegistry::default())
                .await
        ),
        MismatchField::Image.to_string()
    );
    new_pin(&mut local, placed());
    fixture
        .pull_deploy(&local, &proof, &FixedRegistry::default())
        .await
        .unwrap();
}

fn new_pin(want: &mut ContainerWant, pin: ployz_core::ImageDigest) {
    let ContainerWant::Running {
        container: RunTarget::New(new),
        ..
    } = want
    else {
        unreachable!()
    };
    new.image_id = Some(pin);
}

#[tokio::test]
async fn a_registry_that_serves_other_content_under_the_digest_is_refused() {
    let fixture = fixture().await;
    let proof = fixture.registry_proof(&fixture.registry_rule(3, &["web"]));
    let registry = FixedRegistry {
        answered: Some(format!("sha256:{}", "7".repeat(64))),
        ..FixedRegistry::default()
    };
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(
                    &want("service_container", "k1", &registry_spec("web", IMAGE)),
                    &proof,
                    &registry,
                )
                .await
        ),
        MismatchField::RegistryDigest.to_string()
    );
}

// Catches an approval turned off or changed while the registry answered still deploying
// the image it was pulling.
#[tokio::test]
async fn a_newer_generation_landing_during_the_pull_refuses_the_older_one() {
    let fixture = fixture().await;
    let proof = fixture.registry_proof(&fixture.registry_rule(3, &["web"]));
    let registry = FixedRegistry {
        landing: Some((&fixture, fixture.registry_rule(4, &["web"]))),
        ..FixedRegistry::default()
    };
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(
                    &want("service_container", "k1", &registry_spec("web", IMAGE)),
                    &proof,
                    &registry,
                )
                .await
        ),
        "a generation"
    );
}

// Catches a registry rule's hook running twice for one image under one approval, or
// one Service's run standing in for another's on the same image.
#[tokio::test]
async fn a_registry_hook_runs_once_per_service_image_and_generation() {
    const OTHER: &str = "8888888888888888888888888888888888888888888888888888888888888888";
    let fixture = fixture().await;
    let proof = fixture.registry_proof(&fixture.registry_rule(3, &["api", "web"]));
    let hook_run = MismatchField::HookRun.to_string();
    fixture
        .registry_hook(("web", IMAGE), "k1", &proof)
        .await
        .unwrap();
    // A retry of the same slot adopts the hook that ran.
    fixture
        .registry_hook(("web", IMAGE), "k1", &proof)
        .await
        .unwrap();
    assert_eq!(
        refused(fixture.registry_hook(("web", IMAGE), "k2", &proof).await),
        hook_run
    );
    // Another Service on the same digest runs its own hook.
    fixture
        .registry_hook(("api", IMAGE), "k3", &proof)
        .await
        .unwrap();
    // Another image runs once, and moving back does not run the first image again.
    fixture
        .registry_hook(("web", OTHER), "k4", &proof)
        .await
        .unwrap();
    assert_eq!(
        refused(fixture.registry_hook(("web", IMAGE), "k5", &proof).await),
        hook_run
    );
    // A slot whose hook was made is never made again, once its Container is gone.
    assert!(!fixture.machine.approvals.hook_made("k1").unwrap());
    fixture.machine.approvals.mark_hook_made("k1").unwrap();
    assert!(fixture.machine.approvals.hook_made("k1").unwrap());
    // A newer approval runs the image's hook once more.
    let newer = fixture.registry_proof(&fixture.registry_rule(4, &["api", "web"]));
    fixture
        .registry_hook(("web", IMAGE), "k6", &newer)
        .await
        .unwrap();
    assert_eq!(
        refused(fixture.registry_hook(("web", IMAGE), "k6", &proof).await),
        "a generation"
    );
}

// Catches a GitHub rule's proof admitting a registry pull, and the reverse.
#[tokio::test]
async fn a_proof_of_one_source_admits_nothing_under_the_other() {
    let fixture = fixture().await;
    let github_rule = fixture.deploy_rule(3);
    let registry = fixture.registry_proof(&github_rule);
    let source = MismatchField::RuleSource.to_string();
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(
                    &want("service_container", "k1", &spec()),
                    &registry,
                    &FixedRegistry::default(),
                )
                .await
        ),
        source
    );
    // A rule's source never changes under its ID, so the other half runs on a fresh
    // Machine.
    let fixture = super::super::rule_grant::tests::fixture().await;
    let rule = fixture.registry_rule(3, &["web"]);
    let claim = fixture.built(&rule, fixture.attest());
    let online = fixture.proof(&rule, &claim, token_for(&claim, "swap"));
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(
                    &want("service_container", "k1", &registry_spec("web", IMAGE)),
                    &online,
                    &FixedRegistry::default(),
                )
                .await
        ),
        source
    );
}

// Catches a rule switched to another repository on Automatic deploys leaving the
// repository before it approved on a Server that took the switch.
#[tokio::test]
async fn switching_a_registry_rule_to_another_repository_ends_the_one_before() {
    let fixture = fixture().await;
    let web = fixture.registry_rule(3, &["web"]);
    let proof = fixture.registry_proof(&web);
    let deploy = |key| want("service_container", key, &registry_spec("web", IMAGE));
    fixture
        .pull_deploy(&deploy("k1"), &proof, &FixedRegistry::default())
        .await
        .unwrap();
    let mut api = fixture.registry_rule(4, &["web"]);
    api.source =
        ployz_core::authority::RuleSource::Registry(ployz_core::authority::RegistrySource {
            host: "ghcr.io".to_owned(),
            repository: "acme/api".to_owned(),
        });
    fixture
        .machine
        .approvals
        .mark_rule(&api, Admit::Record, |refusal| {
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal))
        })
        .expect("a newer generation may move a registry rule to another repository");
    assert_eq!(
        refused_admission(
            fixture
                .pull_deploy(&deploy("k2"), &proof, &FixedRegistry::default())
                .await
        ),
        "a generation"
    );
}
