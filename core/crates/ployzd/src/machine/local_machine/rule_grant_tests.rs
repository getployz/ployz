#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::collections::BTreeMap;

use ployz_core::authority::github_oidc::test_support::{
    CLAIMS, TEST_KID, keys_with_test_signer, signed_by_test_signer,
};
use ployz_core::{
    BuildGrantRepository, DeviceKey, EnvValue, MachineName, MintBuildGrantRequest, Namespace,
    SealedScope, SealingPublicKey, ServiceName,
    authority::{
        BuildPolicy, PayloadDigest, RuleId, RuleService, Signed, Signer, SignerKey,
        rule::{Builder, PolicyCache, PolicyMethod, RuleEvent, RuleSource, Runner},
    },
};
use serde_json::{Value, json};

use super::*;
use crate::{
    machine::{LocalMachineStore, RecordOwner},
    management::build_grant::Mint,
    sealed_env::test_support::Cluster,
};

const BUILDER_SHA: &str = "1234567890abcdef1234567890abcdef12345678";
const COMMIT: &str = "54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";
const MARKER: &str = "marker-sealed-rule-0123";

pub(crate) struct Fixture {
    pub(crate) machine: LocalMachine,
    pub(crate) signer: Signer,
    pub(crate) cluster: SealingPublicKey,
    /// The Sealing Key the Machine holds, in a directory of its own.
    sealing: Cluster,
    /// The build's one Sealed record: sealing again would change its digest.
    pub(crate) record: SealedRecord,
    _data_dir: crate::test_dir::TestDir,
}

pub(crate) async fn fixture() -> Fixture {
    let sealing = Cluster::new();
    let data_dir = crate::test_dir::TestDir::new("rule-grant");
    let mut store = LocalMachineStore::open(&data_dir.0).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    // One key for both: the Machine's own and the one its runtime opens values with.
    std::fs::copy(
        sealing.dir.0.join("sealing.key"),
        data_dir.0.join("sealing.key"),
    )
    .unwrap();
    let owner = RecordOwner::spawn(store).unwrap();
    let signer = Signer::from_seed(&[7; 32]);
    let device = DeviceKey::from(signer.public());
    owner
        .mutate(move |store| {
            store.approve_device(device).unwrap();
            store.approve_passkey(passkey()).unwrap();
            store.set_device_enforcement(true, Vec::new()).unwrap();
        })
        .await
        .unwrap();
    let cluster = crate::sealing_key::SealingKey::load(&data_dir.0)
        .unwrap()
        .unwrap()
        .public_key();
    let env = BTreeMap::from([(
        "NPM_TOKEN".to_owned(),
        EnvValue::Sealed(sealing.seal("NPM_TOKEN", SealedScope::Shared, MARKER)),
    )]);
    let record = sealing.open(env).unwrap().record.unwrap();
    Fixture {
        machine: LocalMachine::new(owner),
        signer,
        cluster,
        sealing,
        record,
        _data_dir: data_dir,
    }
}

pub(crate) fn claims() -> Value {
    serde_json::from_str(CLAIMS).unwrap()
}

pub(crate) fn now() -> Timestamp {
    Timestamp::from_millis(claims()["iat"].as_u64().unwrap() * 1000).unwrap()
}

pub(crate) fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

pub(crate) fn commit() -> CommitSha {
    CommitSha::parse(COMMIT).unwrap()
}

pub(crate) fn policy() -> BuildPolicy {
    BuildPolicy {
        method: PolicyMethod::Dockerfile {
            path: "Dockerfile".to_owned(),
        },
        context: ".".to_owned(),
        target: None,
        args: BTreeMap::new(),
        secrets: ["NPM_TOKEN".to_owned()].into(),
        platforms: ["linux/amd64".to_owned()].into(),
        cache: PolicyCache::Off,
    }
}

pub(crate) fn holder() -> BuildGrantId {
    BuildGrantId::parse(hex::encode([3u8; 32])).unwrap()
}

fn recipient() -> RecipientKey {
    RecipientKey::from_bytes([4; 32])
}

/// A rule over `record` for `cluster`, naming `machine` and signed by `signer`.
pub(crate) fn rule_for(
    cluster: SealingPublicKey,
    machine: ployz_core::MachineId,
    signer: &Signer,
    record: &SealedRecord,
) -> AutoDeployRule {
    AutoDeployRule {
        cluster,
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        namespace: Namespace::parse("app").unwrap(),
        machines: vec![machine],
        source: RuleSource {
            repository_id: 1_450_985_696,
            owner_id: 354_323_584,
            repository: "Ployz-test/lab-service".to_owned(),
            branch: "main".to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
            builder: Builder {
                r#ref: format!("Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"),
                sha: CommitSha::parse(BUILDER_SHA).unwrap(),
            },
            runner: Runner::GithubHosted,
        },
        services: vec![RuleService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            policy: policy(),
            secrets: Some(SealedDigest::of(record).unwrap()),
        }],
        issued_at: Timestamp::from_millis(now().as_millis() - 60_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    }
}

/// The grant claim a build of [`commit`] under `rule` asks its token for.
pub(crate) fn claim_for(cluster: SealingPublicKey, rule: &AutoDeployRule) -> BuildClaim {
    BuildClaim {
        cluster,
        rule: rule.rule,
        generation: rule.generation,
        service: web(),
        policy: PolicyDigest::of(&policy()).unwrap(),
        commit: commit(),
        authority: CommitAuthority::Head,
        stage: BuildStage::Grant {
            holder: holder(),
            recipient: recipient(),
        },
    }
}

/// A token for `claim`'s audience whose claims the fixture rule approves, spent once as
/// `jti`.
pub(crate) fn token_for(claim: &BuildClaim, jti: &str) -> String {
    let mut claims = claims();
    claims["aud"] = json!(claim.audience().unwrap());
    claims["jti"] = json!(jti);
    claims["event_name"] = json!("push");
    claims["workflow_ref"] =
        json!("Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main");
    claims["job_workflow_ref"] = json!(format!(
        "Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"
    ));
    claims["job_workflow_sha"] = json!(BUILDER_SHA);
    claims["sha"] = json!(COMMIT);
    signed_by_test_signer(TEST_KID, &claims)
}

/// What the builder sends for `rule`, signed by `signer`, with `token` as its proof.
pub(crate) fn grant_for(rule: &AutoDeployRule, signer: &Signer, token: String) -> RuleGrant {
    RuleGrant {
        rule: Signed::sign(rule, signer).unwrap(),
        proof: GithubToken::new(token),
        service: web(),
        commit: commit(),
        authority: CommitAuthority::Head,
        approval: None,
        recipient: recipient(),
        reader: None,
    }
}

/// GitHub as the fixture tokens expect it, with the branch at `head`.
pub(crate) fn github(head: CommitSha) -> FixedGithub {
    FixedGithub {
        keys: keys_with_test_signer(),
        head,
        ancestry: None,
        now: now(),
    }
}

impl Fixture {
    fn record(&self) -> SealedRecord {
        self.record.clone()
    }

    fn rule(&self) -> AutoDeployRule {
        rule_for(
            self.cluster,
            self.machine.record().id(),
            &self.signer,
            &self.record,
        )
    }

    fn claim(&self, rule: &AutoDeployRule) -> BuildClaim {
        claim_for(self.cluster, rule)
    }

    fn token(&self, claim: &BuildClaim, jti: &str) -> String {
        token_for(claim, jti)
    }

    fn grant(&self, rule: &AutoDeployRule, token: String) -> RuleGrant {
        grant_for(rule, &self.signer, token)
    }

    async fn admit(
        &self,
        grant: &RuleGrant,
        holder: &BuildGrantId,
        sealed: Option<&SealedRecord>,
        head: CommitSha,
    ) -> Result<AdmittedRule, Error> {
        self.machine
            .admit_rule_grant(grant, Some(holder), sealed, &github(head), now())
            .await
    }

    /// Admit the fixture's valid grant with `change` applied to the rule, the claim the
    /// token names, and the grant the builder sends.
    async fn admit_changed(
        &self,
        jti: &str,
        change: impl FnOnce(&mut AutoDeployRule, &mut BuildClaim, &mut RuleGrant),
    ) -> Result<AdmittedRule, Error> {
        let mut rule = self.rule();
        let mut claim = self.claim(&rule);
        let mut grant = self.grant(&rule, String::new());
        change(&mut rule, &mut claim, &mut grant);
        grant.proof = GithubToken::new(self.token(&claim, jti));
        grant.rule = Signed::sign(&rule, &self.signer).unwrap();
        self.admit(&grant, &holder(), Some(&self.record()), commit())
            .await
    }
}

#[track_caller]
fn refused(result: Result<AdmittedRule, Error>, field: &str) {
    match result {
        Err(Error::RuleGrantRefused(AuthorityRefusal::Mismatch { field: named })) => {
            assert_eq!(named, field);
        }
        other => panic!("expected a refusal naming {field}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_valid_token_mints_and_its_secrets_open_only_for_the_build() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let claim = fixture.claim(&rule);
    let grant = fixture.grant(&rule, fixture.token(&claim, "valid"));
    let admitted = fixture
        .admit(&grant, &holder(), Some(&fixture.record()), commit())
        .await
        .unwrap();
    assert_eq!(admitted.audience, claim.audience().unwrap());
    assert_eq!(admitted.commit, commit());
    let mint = Mint::check_admitted(
        MintBuildGrantRequest {
            repository: BuildGrantRepository::parse("ployz-build/web").unwrap(),
            holder: Some(holder()),
            sealed: Some(fixture.record()),
            rule: Some(grant),
        },
        Some(&fixture.sealing.dir.0),
        Some(admitted),
    );
    assert!(mint.is_ok(), "{:?}", mint.err());
}

#[tokio::test]
async fn a_grant_token_is_spent_once() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let grant = fixture.grant(&rule, fixture.token(&fixture.claim(&rule), "once"));
    let record = fixture.record();
    fixture
        .admit(&grant, &holder(), Some(&record), commit())
        .await
        .unwrap();
    let again = fixture
        .admit(&grant, &holder(), Some(&record), commit())
        .await;
    assert!(
        matches!(
            again,
            Err(Error::RuleGrantRefused(AuthorityRefusal::ProofReplayed))
        ),
        "{again:?}"
    );
}

#[tokio::test]
async fn another_holder_in_the_audience_is_refused() {
    let fixture = fixture().await;
    let result = fixture
        .admit_changed("holder", |_, claim, _| {
            claim.stage = BuildStage::Grant {
                holder: BuildGrantId::parse(hex::encode([5u8; 32])).unwrap(),
                recipient: recipient(),
            };
        })
        .await;
    refused(result, "an audience");
}

#[tokio::test]
async fn another_recipient_is_refused() {
    let fixture = fixture().await;
    let result = fixture
        .admit_changed("recipient", |_, _, grant| {
            grant.recipient = RecipientKey::from_bytes([6; 32]);
        })
        .await;
    refused(result, "an audience");
}

#[tokio::test]
async fn another_policy_in_the_audience_is_refused() {
    let fixture = fixture().await;
    let result = fixture
        .admit_changed("policy", |_, claim, _| {
            let mut policy = policy();
            policy.args.insert("EXTRA".to_owned(), "1".to_owned());
            claim.policy = PolicyDigest::of(&policy).unwrap();
        })
        .await;
    refused(result, "an audience");
}

#[tokio::test]
async fn a_sealed_record_the_rule_does_not_name_is_refused() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let grant = fixture.grant(&rule, fixture.token(&fixture.claim(&rule), "sealed"));
    let other = BTreeMap::from([(
        "NPM_TOKEN".to_owned(),
        EnvValue::Sealed(
            fixture
                .sealing
                .seal("NPM_TOKEN", SealedScope::Shared, "another"),
        ),
    )]);
    let other = fixture.sealing.open(other).unwrap().record.unwrap();
    refused(
        fixture
            .admit(&grant, &holder(), Some(&other), commit())
            .await,
        "the Sealed values",
    );
    refused(
        fixture.admit(&grant, &holder(), None, commit()).await,
        "the Sealed values",
    );
}

#[tokio::test]
async fn a_generation_below_the_highest_seen_is_refused() {
    let fixture = fixture().await;
    fixture
        .admit_changed("gen-4", |rule, claim, _| {
            rule.generation = 4;
            claim.generation = 4;
        })
        .await
        .unwrap();
    let result = fixture.admit_changed("gen-3", |_, _, _| {}).await;
    refused(result, "a generation");
}

#[tokio::test]
async fn an_ended_rule_is_refused() {
    let fixture = fixture().await;
    let result = fixture
        .admit_changed("ended", |rule, _, _| rule.ended = true)
        .await;
    refused(result, "an ended rule");
}

#[tokio::test]
async fn an_ended_generation_refuses_every_generation_below_it() {
    let fixture = fixture().await;
    let ended = fixture
        .admit_changed("gen-4-ended", |rule, claim, _| {
            rule.generation = 4;
            claim.generation = 4;
            rule.ended = true;
        })
        .await;
    refused(ended, "an ended rule");
    let older = fixture.admit_changed("gen-3", |_, _, _| {}).await;
    refused(older, "a generation");
}

#[tokio::test]
async fn a_machine_the_rule_does_not_name_is_refused() {
    let fixture = fixture().await;
    let result = fixture
        .admit_changed("machine", |rule, _, _| {
            rule.machines = vec![ployz_core::MachineId::parse("1".repeat(32)).unwrap()];
        })
        .await;
    refused(result, "a Machine");
}

#[tokio::test]
async fn a_head_that_moved_is_refused() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let grant = fixture.grant(&rule, fixture.token(&fixture.claim(&rule), "head"));
    let newer = CommitSha::parse(&"9".repeat(40)).unwrap();
    refused(
        fixture
            .admit(&grant, &holder(), Some(&fixture.record()), newer)
            .await,
        "a commit",
    );
}

#[tokio::test]
async fn a_commit_other_than_the_one_github_ran_is_refused_under_head() {
    let fixture = fixture().await;
    let fork = CommitSha::parse(&"7".repeat(40)).unwrap();
    let result = fixture
        .admit_changed("commit-input", |_, claim, grant| {
            claim.commit = fork;
            grant.commit = fork;
        })
        .await;
    refused(result, "a commit");
}

#[tokio::test]
async fn a_rule_signed_by_a_key_this_machine_does_not_trust_is_refused() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let mut grant = fixture.grant(&rule, fixture.token(&fixture.claim(&rule), "untrusted"));
    let stranger = Signer::from_seed(&[9; 32]);
    let mut stranger_rule = rule;
    stranger_rule.signer = SignerKey::Device(stranger.public().into());
    grant.rule = Signed::sign(&stranger_rule, &stranger).unwrap();
    let result = fixture
        .admit(&grant, &holder(), Some(&fixture.record()), commit())
        .await;
    assert!(
        matches!(result, Err(Error::RuleGrantRefused(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_token_for_another_branch_is_refused_naming_ref() {
    let fixture = fixture().await;
    let rule = fixture.rule();
    let mut claims = claims();
    claims["aud"] = json!(fixture.claim(&rule).audience().unwrap());
    claims["jti"] = json!("attacker");
    claims["event_name"] = json!("workflow_dispatch");
    claims["ref"] = json!("refs/heads/attacker");
    let grant = fixture.grant(&rule, signed_by_test_signer(TEST_KID, &claims));
    let result = fixture
        .admit(&grant, &holder(), Some(&fixture.record()), commit())
        .await;
    assert!(
        matches!(
            result,
            Err(Error::RuleProofRefused(ProofRefusal::Claim(
                AuthorityRefusal::Mismatch { field: "ref" }
            )))
        ),
        "{result:?}"
    );
}

/// The passkey the fixture Machine trusts: an EdDSA authenticator on `ployz.cloud`.
fn passkey() -> ployz_core::authority::TrustedPasskey {
    use ployz_core::authority::{
        Passkey, PasskeyAccount, PasskeyApprover, PasskeyPublicKey, TrustedPasskey,
        trusted_key::{Base64Url, PasskeyAlgorithm, RelyingParty},
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]).verifying_key();
    TrustedPasskey {
        passkey: Passkey {
            credential_id: Base64Url::new(vec![5, 1, 2, 3]).try_into().unwrap(),
            public_key: PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes()).unwrap(),
            account: PasskeyAccount::parse("ada@example.com").unwrap(),
            relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
        },
        approved_by: PasskeyApprover::Server,
    }
}

/// The trusted passkey's user-verified answer to `challenge`, as a browser returns it.
fn answer(challenge: &[u8]) -> ployz_core::authority::PasskeyAssertion {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::Signer as _;
    use ployz_core::authority::{PasskeyAssertion, trusted_key::Base64Url};
    use sha2::{Digest as _, Sha256};

    let client_data = json!({
        "type": "webauthn.get",
        "challenge": URL_SAFE_NO_PAD.encode(challenge),
        "origin": "https://ployz.cloud",
    })
    .to_string()
    .into_bytes();
    let mut authenticator_data = Sha256::digest(b"ployz.cloud").to_vec();
    authenticator_data.extend_from_slice(&[0x05, 0, 0, 0, 1]);
    let mut answered = authenticator_data.clone();
    answered.extend_from_slice(&Sha256::digest(&client_data));
    let authenticator = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    PasskeyAssertion {
        credential_id: Base64Url::new(vec![5, 1, 2, 3]).try_into().unwrap(),
        authenticator_data: Base64Url::new(authenticator_data),
        client_data_json: Base64Url::new(client_data),
        signature: Base64Url::new(authenticator.sign(&answered).to_bytes().to_vec()),
    }
}

#[tokio::test]
async fn a_rule_a_passkey_answered_with_its_own_prompt_mints() {
    let fixture = fixture().await;
    let rule = AutoDeployRule {
        signer: SignerKey::Passkey(ployz_core::authority::OwnPrompt::Passkey),
        ..fixture.rule()
    };
    let claim = fixture.claim(&rule);
    let mut grant = fixture.grant(&rule, fixture.token(&claim, "passkey"));
    let unsigned = Signed::unsigned(&rule).unwrap();
    let assertion = answer(&unsigned.challenge());
    grant.rule = unsigned.answered(assertion);
    let admitted = fixture
        .admit(&grant, &holder(), Some(&fixture.record()), commit())
        .await;
    assert!(admitted.is_ok(), "{:?}", admitted.err());
}

#[tokio::test]
async fn a_rule_signed_under_a_browser_session_is_refused_before_anything_opens() {
    use ployz_core::authority::{ActionFamily, GrantBody, SessionGrant, session::Delegation};

    let fixture = fixture().await;
    let browser = Signer::from_seed(&[8; 32]);
    let rule = AutoDeployRule {
        signer: SignerKey::Browser {
            browser: browser.public(),
        },
        ..fixture.rule()
    };
    let grant_body = GrantBody {
        browser_key: browser.public(),
        cluster: fixture.cluster,
        environment: rule.namespace.clone(),
        environment_id: uuid::Uuid::nil(),
        machines: rule.machines.clone(),
        families: vec![ActionFamily::Deploy],
        issued_at: rule.issued_at,
        expires_at: rule.issued_at.plus(60 * 60_000),
        delegation: Delegation::None,
    };
    let body = ployz_core::authority::canonical::to_canonical(&grant_body).unwrap();
    let session = SessionGrant {
        assertion: answer(&GrantBody::challenge(&body)),
        body,
    };
    let claim = fixture.claim(&rule);
    let mut grant = fixture.grant(&rule, fixture.token(&claim, "session"));
    grant.rule = Signed::sign_under(&rule, &browser, session).unwrap();
    let result = fixture
        .admit(&grant, &holder(), Some(&fixture.record()), commit())
        .await;
    refused(result, "the signer");
}

mod approved {
    //! A build of a commit that is not the head, under a person's build approval.

    use ployz_core::authority::{BuildApproval, StatementDigest};

    use super::*;
    use crate::github::head::Ancestry;

    /// A commit earlier on the branch than the head GitHub ran the dispatch at.
    fn older() -> CommitSha {
        CommitSha::parse(&"4".repeat(40)).unwrap()
    }

    fn approval_for(fixture: &Fixture, rule: &AutoDeployRule) -> BuildApproval {
        BuildApproval {
            cluster: fixture.cluster,
            rule: rule.rule,
            generation: rule.generation,
            service: web(),
            commit: older(),
            issued_at: Timestamp::from_millis(now().as_millis() - 60_000).unwrap(),
            expires_at: Timestamp::from_millis(now().as_millis() + 14 * 60_000).unwrap(),
            signer: SignerKey::Device(fixture.signer.public().into()),
        }
    }

    /// Admit an approved grant for [`older`] with `change` applied to the approval
    /// before it is signed and to the token's claims, while the branch stands to the
    /// commit as `ancestry` says. The audience names `digest`, or the signed approval's.
    async fn admit_approved(
        fixture: &Fixture,
        jti: &str,
        ancestry: Option<Ancestry>,
        digest: Option<StatementDigest>,
        change: impl FnOnce(&mut BuildApproval, &mut Value),
    ) -> Result<AdmittedRule, Error> {
        let rule = fixture.rule();
        let mut approval = approval_for(fixture, &rule);
        let mut claim = fixture.claim(&rule);
        let mut claims = claims();
        claims["event_name"] = json!("workflow_dispatch");
        claims["workflow_ref"] =
            json!("Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main");
        claims["job_workflow_ref"] = json!(format!(
            "Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"
        ));
        claims["job_workflow_sha"] = json!(BUILDER_SHA);
        claims["sha"] = json!(COMMIT);
        claims["jti"] = json!(jti);
        claim.commit = older();
        change(&mut approval, &mut claims);
        let approval = Signed::sign(&approval, &fixture.signer).unwrap();
        claim.authority =
            CommitAuthority::Approved(digest.unwrap_or_else(|| StatementDigest::of(&approval)));
        claims["aud"] = json!(claim.audience().unwrap());
        let mut grant = fixture.grant(&rule, signed_by_test_signer(TEST_KID, &claims));
        grant.commit = claim.commit;
        grant.authority = claim.authority;
        grant.approval = Some(approval);
        let github = FixedGithub {
            ancestry,
            ..github(commit())
        };
        fixture
            .machine
            .admit_rule_grant(
                &grant,
                Some(&holder()),
                Some(&fixture.record()),
                &github,
                now(),
            )
            .await
    }

    #[tokio::test]
    async fn a_commit_the_branch_descends_from_is_admitted() {
        let fixture = fixture().await;
        for (jti, ancestry) in [
            ("ahead", Ancestry::Ahead),
            ("identical", Ancestry::Identical),
        ] {
            let admitted = admit_approved(&fixture, jti, Some(ancestry), None, |_, _| {})
                .await
                .unwrap();
            assert_eq!(admitted.commit, older(), "{jti}");
        }
    }

    #[tokio::test]
    async fn a_commit_not_on_the_branch_is_refused() {
        let fixture = fixture().await;
        for (jti, ancestry) in [
            // A fork's commit, or one on another branch.
            ("diverged", Some(Ancestry::Diverged)),
            // A descendant of the head that was never merged.
            ("behind", Some(Ancestry::Behind)),
            // A commit GitHub cannot find while it reads the branch.
            ("unknown", None),
        ] {
            refused(
                admit_approved(&fixture, jti, ancestry, None, |_, _| {}).await,
                "a commit",
            );
        }
    }

    #[tokio::test]
    async fn an_approval_for_another_service_generation_or_commit_is_refused() {
        let fixture = fixture().await;
        let ahead = Some(Ancestry::Ahead);
        let service = admit_approved(&fixture, "service", ahead, None, |approval, _| {
            approval.service = ServiceName::parse("api").unwrap();
        })
        .await;
        refused(service, "a Service");
        let generation = admit_approved(&fixture, "generation", ahead, None, |approval, _| {
            approval.generation = 2;
        })
        .await;
        refused(generation, "a generation");
        let commit = admit_approved(&fixture, "commit", ahead, None, |approval, _| {
            approval.commit = CommitSha::parse(&"5".repeat(40)).unwrap();
        })
        .await;
        refused(commit, "a commit");
    }

    #[tokio::test]
    async fn an_expired_approval_is_refused() {
        let fixture = fixture().await;
        let result = admit_approved(
            &fixture,
            "expired",
            Some(Ancestry::Ahead),
            None,
            |approval, _| {
                approval.issued_at =
                    Timestamp::from_millis(now().as_millis() - 40 * 60_000).unwrap();
                approval.expires_at =
                    Timestamp::from_millis(now().as_millis() - 25 * 60_000).unwrap();
            },
        )
        .await;
        assert!(
            matches!(
                result,
                Err(Error::RuleGrantRefused(AuthorityRefusal::Expired))
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_push_run_cannot_build_an_approved_commit() {
        let fixture = fixture().await;
        let result = admit_approved(
            &fixture,
            "push",
            Some(Ancestry::Ahead),
            None,
            |_, claims| {
                claims["event_name"] = json!("push");
            },
        )
        .await;
        refused(result, "an event");
    }

    #[tokio::test]
    async fn an_audience_naming_another_approval_is_refused() {
        let fixture = fixture().await;
        let result = admit_approved(
            &fixture,
            "digest",
            Some(Ancestry::Ahead),
            Some(StatementDigest::from_bytes([9; 32])),
            |_, _| {},
        )
        .await;
        refused(result, "an approval");
    }

    #[tokio::test]
    async fn an_approval_signed_by_a_key_this_machine_does_not_trust_is_refused() {
        let fixture = fixture().await;
        let rule = fixture.rule();
        let stranger = Signer::from_seed(&[9; 32]);
        let mut approval = approval_for(&fixture, &rule);
        approval.signer = SignerKey::Device(stranger.public().into());
        let signed = Signed::sign(&approval, &stranger).unwrap();
        let mut claim = fixture.claim(&rule);
        claim.commit = older();
        claim.authority = CommitAuthority::Approved(StatementDigest::of(&signed));
        let mut claims = claims();
        claims["aud"] = json!(claim.audience().unwrap());
        claims["event_name"] = json!("workflow_dispatch");
        claims["workflow_ref"] =
            json!("Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main");
        claims["job_workflow_ref"] = json!(format!(
            "Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"
        ));
        claims["job_workflow_sha"] = json!(BUILDER_SHA);
        let mut grant = fixture.grant(&rule, signed_by_test_signer(TEST_KID, &claims));
        grant.commit = older();
        grant.authority = claim.authority;
        grant.approval = Some(signed);
        let github = FixedGithub {
            ancestry: Some(Ancestry::Ahead),
            ..github(commit())
        };
        let result = fixture
            .machine
            .admit_rule_grant(
                &grant,
                Some(&holder()),
                Some(&fixture.record()),
                &github,
                now(),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(Error::RuleGrantRefused(AuthorityRefusal::UnknownKey { .. }))
            ),
            "{result:?}"
        );
    }
}
