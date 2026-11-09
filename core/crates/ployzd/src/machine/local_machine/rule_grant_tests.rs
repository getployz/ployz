#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::collections::BTreeMap;

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
    github::test_support::{CLAIMS, TEST_KID, keys_with_test_signer, signed_by_test_signer},
    machine::{LocalMachineStore, RecordOwner},
    management::build_grant::Mint,
    sealed_env::test_support::Cluster,
};

const BUILDER_SHA: &str = "1234567890abcdef1234567890abcdef12345678";
const COMMIT: &str = "54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";
const MARKER: &str = "marker-sealed-rule-0123";

struct Fixture {
    machine: LocalMachine,
    signer: Signer,
    cluster: SealingPublicKey,
    /// The Sealing Key the Machine holds, in a directory of its own.
    sealing: Cluster,
    /// The build's one Sealed record: sealing again would change its digest.
    record: SealedRecord,
    _data_dir: crate::test_dir::TestDir,
}

async fn fixture() -> Fixture {
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

fn claims() -> Value {
    serde_json::from_str(CLAIMS).unwrap()
}

fn now() -> Timestamp {
    Timestamp::from_millis(claims()["iat"].as_u64().unwrap() * 1000).unwrap()
}

pub(crate) fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

pub(crate) fn commit() -> CommitSha {
    CommitSha::parse(COMMIT).unwrap()
}

fn policy() -> BuildPolicy {
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
        recipient: recipient(),
        reader: None,
    }
}

/// GitHub as the fixture tokens expect it, with the branch at `head`.
pub(crate) fn github(head: CommitSha) -> FixedGithub {
    FixedGithub {
        keys: keys_with_test_signer(),
        head,
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
