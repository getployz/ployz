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
    Fixture, claim_for, commit, fixture, fork, github, now, rule_for, token_for,
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
    want_in("app", kind, key, spec)
}

fn want_in(namespace: &str, kind: &str, key: &str, spec: &Value) -> ContainerWant {
    let resolved = requested(spec)
        .to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    serde_json::from_value(
        json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot":{"namespace":namespace, "service":spec["name"], "kind":kind, "creation_key":key},
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
                &github(commit()),
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
                &github(head),
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
        self.create_hook_in("app", key, proof).await
    }

    async fn create_hook_in(
        &self,
        namespace: &str,
        key: &str,
        proof: &RuleProof,
    ) -> Result<(), Error> {
        let admitted = self
            .machine
            .admit_rule_deploy(
                &want_in(namespace, "pre_deploy_hook", key, &spec()),
                proof,
                &github(commit()),
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
            &github(commit()),
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
        authority: &ployz_core::authority::CommitAuthority,
        now: Timestamp,
    ) -> Result<ployz_core::authority::GithubClaims, crate::github::oidc::ProofRefusal> {
        self.wait(SlowAt::Token).await;
        self.inner.claims(rule, token, authority, now).await
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
            .admit_rule_deploy(&first, &stale, &slow, now(), &mut budget, Admit::Record),
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
            &github(commit()),
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

impl Fixture {
    /// The preview `nonce` of `rule` on this Server.
    fn preview(&self, rule: &ployz_core::authority::AutoDeployRule, nonce: &str) -> Namespace {
        self.preview_of(rule, nonce)
    }

    /// The fixture's rule as a preview rule of the Environment `app`, approving `spec()` in
    /// every preview, built from any branch.
    fn preview_rule(&self) -> ployz_core::authority::AutoDeployRule {
        let mut rule = self.deploy_rule(3);
        rule.source.branch = String::new();
        rule.services[0].template = ployz_core::authority::preview_template(
            &requested(&spec()),
            &self.preview(&rule, "00000000"),
            false,
        )
        .unwrap();
        rule.preview = Some(ployz_core::authority::PreviewTerms {
            parent: uuid::Uuid::nil(),
            pushers: Vec::new(),
            pinned: Vec::new(),
        });
        rule
    }
}

/// A push to any branch deploys into each preview, whose hooks run once each, so a second
/// preview at the same push seeds its own database.
#[tokio::test]
async fn a_preview_rule_deploys_a_push_and_runs_its_hook_once_in_each_preview() {
    let fixture = fixture().await;
    let rule = fixture.preview_rule();
    let claim = fixture.built(&rule, fixture.attest());
    let proof = fixture.proof(&rule, &claim, token_for(&claim, "preview"));
    let [first, second] = ["0000aaaa", "0000bbbb"].map(|nonce| fixture.preview(&rule, nonce));
    fixture
        .create_hook_in(first.as_str(), "h1", &proof)
        .await
        .unwrap();
    fixture
        .create_hook_in(second.as_str(), "h2", &proof)
        .await
        .unwrap();
    assert_eq!(
        refused(fixture.create_hook_in(first.as_str(), "h3", &proof).await),
        "a hook run"
    );
    assert_eq!(
        refused(fixture.create_hook("h4", &proof).await),
        "an Environment",
        "a preview rule deploys nothing into its Parent"
    );
}

/// A Service the preview rule pins runs its image with no build and no GitHub token.
#[tokio::test]
async fn a_preview_rules_pinned_service_runs_with_no_build() {
    let fixture = fixture().await;
    let mut rule = fixture.preview_rule();
    let preview = fixture.preview(&rule, "0000aaaa");
    let db = json!({
        "name": "db",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": format!("postgres@sha256:{IMAGE}"), "pull_policy": "missing"}
    });
    rule.preview.as_mut().unwrap().pinned = vec![ployz_core::authority::PinnedService {
        name: ServiceName::parse("db").unwrap(),
        template: ployz_core::authority::preview_template(&requested(&db), &preview, true).unwrap(),
    }];
    let pinned = |claim: Option<BuildClaim>| RuleProof {
        rule: Signed::sign(&rule, &fixture.signer).unwrap(),
        evidence: RuleEvidence::Pinned,
        claim,
        reader: None,
    };
    let fixture = &fixture;
    let deploy = |spec: &Value, proof: RuleProof| {
        let want = want_in(preview.as_str(), "service_container", "d1", spec);
        async move { fixture.deploy(want, &proof, commit(), now()).await }
    };
    deploy(&db, pinned(None)).await.unwrap();
    assert_eq!(refused(deploy(&spec(), pinned(None)).await), "a Service");
    let claim = fixture.built(&rule, fixture.attest());
    assert_eq!(refused(deploy(&db, pinned(Some(claim))).await), "a stage");
}

/// A fork's commit a person approved for one preview creates that preview's first
/// Container online, though the run built on another commit and the branch head is a
/// third, then again from the verified build through the admission every create runs. In any
/// other preview it creates nothing, online or not, and without the approval it creates
/// nothing at all.
#[tokio::test]
async fn an_approved_fork_commit_creates_only_in_its_own_preview() {
    let fixture = fixture().await;
    let rule = fixture.preview_rule();
    let [preview, other] = ["0000aaaa", "0000bbbb"].map(|nonce| fixture.preview(&rule, nonce));
    let claim = BuildClaim {
        commit: fork(),
        authority: fixture.approved(&fixture.approval(&rule, &preview)),
        ..fixture.built(&rule, fixture.attest())
    };
    let online = fixture.proof(&rule, &claim, token_for(&claim, "fork"));
    let create = |namespace: &Namespace, key: &str| {
        want_in(namespace.as_str(), "service_container", key, &spec())
    };
    assert_eq!(
        refused(
            fixture
                .deploy(create(&other, "o1"), &online, commit(), now())
                .await
        ),
        "a commit approval"
    );
    let verified = fixture.issued(create(&preview, "k1"), &online).await;
    let offline = Fixture::offline(&online, verified);
    let admitted = fixture
        .machine
        .admit_spec(&create(&preview, "k2"), Some(&offline), &[], Admit::Record)
        .await
        .unwrap()
        .expect("a new Container's admission");
    assert!(
        matches!(admitted.by(), AdmittedBy::Rule { .. }),
        "{admitted:?}"
    );
    assert_eq!(
        refused(
            fixture
                .deploy(create(&other, "o2"), &offline, commit(), now())
                .await
        ),
        "a commit approval",
        "a verified build stands only in the approved preview"
    );

    let unapproved = BuildClaim {
        authority: CommitAuthority::Head,
        ..claim
    };
    let head = fixture.proof(&rule, &unapproved, token_for(&unapproved, "head"));
    assert_eq!(
        refused(
            fixture
                .deploy(create(&preview, "h1"), &head, commit(), now())
                .await
        ),
        "a commit"
    );
}
