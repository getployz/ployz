#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::collections::BTreeMap;

use ployz_core::authority::SigningPublicKey;
use ployz_core::authority::github_oidc::test_support::{
    CLAIMS, JWKS, TEST_KID, keys_with_test_signer, signed_by_test_signer,
};
use ployz_core::authority::rule::{
    Builder, PolicyCache, PolicyMethod, RuleEvent, RuleSource, Runner,
};
use ployz_core::authority::{
    AttestKey, AutoDeployRule, BuildClaim, BuildPolicy, BuildStage, CommitAuthority, CommitSha,
    ImageDigest, PayloadDigest, PolicyDigest, RuleId, RuleService, SignerKey, StatementDigest,
    Timestamp,
};
use ployz_core::{Namespace, SealingPublicKey, ServiceName};
use ployz_store::ImageToken;
use serde_json::{Value, json};

use super::{Checked, Facts, KeySet, Refused, TokenRefusal, check};

const BUILDER_SHA: &str = "1234567890abcdef1234567890abcdef12345678";
const COMMIT: &str = "54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";
const GENERATION: u64 = 3;

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([1; 32])
}

fn attest() -> AttestKey {
    AttestKey::from_bytes([2; 32])
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn commit() -> CommitSha {
    CommitSha::parse(COMMIT).unwrap()
}

fn image() -> ImageDigest {
    ImageDigest::parse(&format!("sha256:{}", "c".repeat(64))).unwrap()
}

fn policy(dockerfile: &str) -> BuildPolicy {
    BuildPolicy {
        method: PolicyMethod::Dockerfile {
            path: dockerfile.to_owned(),
        },
        context: ".".to_owned(),
        target: None,
        args: BTreeMap::new(),
        secrets: Default::default(),
        platforms: ["linux/amd64".to_owned()].into(),
        cache: PolicyCache::Off,
    }
}

/// The rule the fixture token's claims match, building `web` with `dockerfile`.
fn rule(dockerfile: &str) -> AutoDeployRule {
    AutoDeployRule {
        cluster: cluster(),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: GENERATION,
        namespace: Namespace::parse("app").unwrap(),
        machines: Vec::new(),
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
            policy: policy(dockerfile),
            secrets: None,
        }],
        issued_at: Timestamp::from_millis(1).unwrap(),
        ended: false,
        signer: SignerKey::Device(SigningPublicKey::from_bytes([7; 32]).into()),
    }
}

fn facts() -> Facts {
    Facts {
        attest: attest(),
        rule: rule("Dockerfile"),
        keys: keys_with_test_signer(),
    }
}

/// What a build of `commit` under `generation`, with `dockerfile`, by a builder sealing
/// from `attest`, attests for `image`.
struct Built {
    dockerfile: &'static str,
    attest: AttestKey,
    generation: u64,
    sha: &'static str,
    kid: &'static str,
}

impl Default for Built {
    fn default() -> Self {
        Self {
            dockerfile: "Dockerfile",
            attest: attest(),
            generation: GENERATION,
            sha: COMMIT,
            kid: TEST_KID,
        }
    }
}

impl Built {
    /// The token GitHub would sign for this build: the fixture's claims, long expired,
    /// with the rule's source and this build's audience.
    fn token(&self, authority: CommitAuthority) -> ImageToken {
        let claim = BuildClaim {
            cluster: cluster(),
            rule: RuleId::from_bytes([0x11; 16]),
            generation: self.generation,
            service: web(),
            policy: PolicyDigest::of(&policy(self.dockerfile)).unwrap(),
            commit: commit(),
            authority,
            stage: BuildStage::Built {
                image: image(),
                attest: self.attest,
            },
        };
        let mut claims: Value = serde_json::from_str(CLAIMS).unwrap();
        claims["aud"] = json!(claim.audience().unwrap());
        claims["event_name"] = json!("workflow_dispatch");
        claims["workflow_ref"] =
            json!("Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main");
        claims["job_workflow_ref"] = json!(format!(
            "Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"
        ));
        claims["job_workflow_sha"] = json!(BUILDER_SHA);
        claims["sha"] = json!(self.sha);
        ImageToken {
            token: signed_by_test_signer(self.kid, &claims),
            image: image().to_string(),
            generation: self.generation,
            authority,
        }
    }
}

fn approved() -> CommitAuthority {
    CommitAuthority::Approved(StatementDigest::from_bytes([9; 32]))
}

fn checked(token: &ImageToken) -> Result<Checked, Refused> {
    check(token, &web(), commit(), &facts(), cluster())
}

#[test]
fn an_expired_token_whose_claims_match_verifies_its_image() {
    let fixture: Value = serde_json::from_str(CLAIMS).unwrap();
    assert!(
        fixture["exp"].as_u64().unwrap() * 1000 < Timestamp::now().as_millis(),
        "the fixture token has expired"
    );
    for authority in [CommitAuthority::Head, approved()] {
        assert_eq!(
            checked(&Built::default().token(authority)),
            Ok(Checked::Verified(image()))
        );
    }
}

#[test]
fn a_token_for_another_commit_is_refused() {
    let token = Built {
        sha: BUILDER_SHA,
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    assert_eq!(checked(&token), Err(Refused::Commit));
}

#[test]
fn a_token_built_with_other_settings_under_this_generation_is_refused() {
    let token = Built {
        dockerfile: "Dockerfile.other",
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    assert_eq!(checked(&token), Err(Refused::Audience));
}

#[test]
fn a_token_an_older_generation_built_with_other_settings_builds_again() {
    let token = Built {
        dockerfile: "Dockerfile.other",
        generation: GENERATION - 1,
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    assert_eq!(checked(&token), Ok(Checked::Rebuild));
}

#[test]
fn a_token_sealed_from_another_attest_key_is_refused() {
    let token = Built {
        attest: AttestKey::from_bytes([3; 32]),
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    assert_eq!(checked(&token), Err(Refused::Audience));
}

#[test]
fn a_token_cloud_relabels_with_another_image_or_authority_is_refused() {
    let mut token = Built::default().token(CommitAuthority::Head);
    token.image = format!("sha256:{}", "d".repeat(64));
    assert_eq!(checked(&token), Err(Refused::Audience));
    let mut token = Built::default().token(CommitAuthority::Head);
    token.authority = approved();
    assert_eq!(checked(&token), Err(Refused::Audience));
}

#[test]
fn a_token_whose_key_github_rotated_out_builds_again() {
    let token = Built {
        kid: "rotated-out",
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    assert_eq!(checked(&token), Ok(Checked::Rebuild));
}

#[test]
fn a_token_github_did_not_sign_is_refused() {
    let token = Built::default().token(CommitAuthority::Head);
    // GitHub's own keys, without the test signer: the kid is unknown, so it builds again.
    let github = Facts {
        keys: KeySet::parse(JWKS).unwrap(),
        ..facts()
    };
    assert_eq!(
        check(&token, &web(), commit(), &github, cluster()),
        Ok(Checked::Rebuild)
    );
    // A known kid over a payload GitHub never signed is refused.
    let mut forged = token.clone();
    let parts: Vec<&str> = token.token.split('.').collect();
    let other = Built {
        sha: BUILDER_SHA,
        ..Built::default()
    }
    .token(CommitAuthority::Head);
    let payload = other.token.split('.').nth(1).unwrap();
    forged.token = format!("{}.{payload}.{}", parts[0], parts[2]);
    assert_eq!(
        checked(&forged),
        Err(Refused::Signature(TokenRefusal::Signature))
    );
}
