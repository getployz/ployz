#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::collections::BTreeMap;

use ployz_core::{
    BuildGrantRepository, EnvValue, GithubToken, MintBuildGrantRequest, Namespace, SealedScope,
    authority::{
        CommitSha, PayloadDigest, PreviewRule, PreviewService, Review, ReviewedService, RuleId,
        Signed, SignerKey, preview::PreviewReview,
    },
};
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{
    github::test_support::{TEST_KID, signed_by_test_signer},
    machine::local_machine::rule_grant::tests::{
        BUILDER_SHA, COMMIT, Fixture, claims, commit, fixture, github, holder, now, policy,
        recipient, rule_for, web,
    },
    management::build_grant::Mint,
};

/// The parent Environment's Cloud ID, which its inherited values are sealed for.
pub(crate) const PARENT: Uuid = Uuid::from_u128(9);
/// The GitHub account the fixture's tokens name as their actor.
pub(crate) const ACTOR: u64 = 1_109_914;

/// A build record whose one value is the parent's `NPM_TOKEN`, sealed for [`PARENT`].
pub(crate) fn inherited(fixture: &Fixture) -> SealedRecord {
    let env = BTreeMap::from([(
        "NPM_TOKEN".to_owned(),
        EnvValue::Sealed(
            fixture
                .sealing
                .seal_inherited("NPM_TOKEN", PARENT, "parent-token"),
        ),
    )]);
    fixture.sealing.open(env).unwrap().record.unwrap()
}

/// A preview rule over the fixture's Cluster and Machine, its one Service inheriting
/// `NPM_TOKEN` for the build `record`.
pub(crate) fn preview_rule(fixture: &Fixture, record: &SealedRecord) -> PreviewRule {
    let base = rule_for(
        fixture.cluster,
        fixture.machine.record().id(),
        &fixture.signer,
        &fixture.record,
    );
    PreviewRule {
        cluster: fixture.cluster,
        rule: RuleId::from_bytes([0x22; 16]),
        generation: 2,
        machines: base.machines,
        parent: Namespace::parse("app").unwrap(),
        parent_id: PARENT,
        domain: "preview.example.com".to_owned(),
        source: base.source,
        trusted: vec![ACTOR],
        services: vec![PreviewService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            setup: None,
            policy: policy(),
            secrets: Some(ployz_core::authority::preview::secrets_digest(record).unwrap()),
            inherit: ["NPM_TOKEN".to_owned()].into(),
            generated: BTreeMap::new(),
        }],
        issued_at: Timestamp::from_millis(now().as_millis() - 60_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(fixture.signer.public().into()),
    }
}

pub(crate) fn claim(rule: &PreviewRule, authority: CommitAuthority) -> BuildClaim {
    BuildClaim {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        service: web(),
        policy: PolicyDigest::of(&policy()).unwrap(),
        commit: commit(),
        authority,
        stage: BuildStage::Grant {
            holder: holder(),
            recipient: recipient(),
        },
    }
}

/// A token for `claim`'s audience, run by `event` on `branch` by `actor`, spent as `jti`.
pub(crate) fn token(
    claim: &BuildClaim,
    jti: &str,
    event: &str,
    branch: &str,
    actor: u64,
) -> GithubToken {
    let mut claims = claims();
    claims["aud"] = json!(claim.audience().unwrap());
    claims["jti"] = json!(jti);
    claims["event_name"] = json!(event);
    claims["ref"] = json!(format!("refs/heads/{branch}"));
    claims["workflow_ref"] = json!(format!(
        "Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/{branch}"
    ));
    claims["job_workflow_ref"] = json!(format!(
        "Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"
    ));
    claims["job_workflow_sha"] = json!(BUILDER_SHA);
    claims["actor_id"] = json!(actor.to_string());
    claims["sha"] = json!(COMMIT);
    GithubToken::new(signed_by_test_signer(TEST_KID, &claims))
}

fn grant(fixture: &Fixture, rule: &PreviewRule, proof: GithubToken) -> PreviewGrant {
    PreviewGrant {
        rule: Signed::sign(rule, &fixture.signer).unwrap(),
        review: None,
        proof,
        service: web(),
        commit: commit(),
        authority: CommitAuthority::Head,
        recipient: recipient(),
        reader: None,
    }
}

/// A Device-signed review of the `feature` preview of `commit` under `rule`.
pub(crate) fn review(fixture: &Fixture, rule: &PreviewRule, commit: CommitSha) -> Review {
    let body = PreviewReview {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        key: PreviewKey::Fork(12),
        commit,
        environment: Uuid::from_u128(1),
        services: vec![ReviewedService {
            name: web(),
            digest: PayloadDigest::parse(&"cd".repeat(32)).unwrap(),
        }],
        issued_at: Timestamp::from_millis(now().as_millis() - 1_000).unwrap(),
        expires_at: Timestamp::from_millis(now().as_millis() + 600_000).unwrap(),
    };
    Review::Signed(Signed::sign(&body, &fixture.signer).unwrap())
}

/// A reviewed build's grant: a dispatch on the parent's branch under `review`.
fn reviewed(fixture: &Fixture, rule: &PreviewRule, review: Review, jti: &str) -> PreviewGrant {
    let authority = CommitAuthority::Review(review.digest());
    let mut grant = grant(
        fixture,
        rule,
        token(
            &claim(rule, authority),
            jti,
            "workflow_dispatch",
            "main",
            ACTOR,
        ),
    );
    grant.authority = authority;
    grant.review = Some(review);
    grant
}

async fn admit(
    fixture: &Fixture,
    grant: &PreviewGrant,
    sealed: &SealedRecord,
) -> Result<AdmittedRule, Error> {
    fixture
        .machine
        .admit_preview_grant(
            grant,
            Some(&holder()),
            Some(sealed),
            &github(commit()),
            now(),
        )
        .await
}

#[track_caller]
fn refused(result: Result<AdmittedRule, Error>, field: &str) {
    match result {
        Err(Error::PreviewGrantRefused(PreviewRefusal::Rule(AuthorityRefusal::Mismatch {
            field: named,
        }))) => assert_eq!(named, field),
        other => panic!("expected a refusal naming {field}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_trusted_push_to_another_branch_mints_and_its_inherited_values_open() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let claim = claim(&rule, CommitAuthority::Head);
    let grant = grant(
        &fixture,
        &rule,
        token(&claim, "trusted", "push", "feature", ACTOR),
    );
    let admitted = admit(&fixture, &grant, &record).await.unwrap();
    assert_eq!(admitted.audience, claim.audience().unwrap());
    let mint = Mint::check_admitted(
        MintBuildGrantRequest {
            repository: BuildGrantRepository::parse("ployz-build/web").unwrap(),
            holder: Some(holder()),
            sealed: Some(record),
            rule: None,
            preview: Some(grant),
        },
        Some(&fixture.sealing.dir.0),
        Some(admitted),
    );
    assert!(mint.is_ok(), "{:?}", mint.err());
}

#[tokio::test]
async fn a_push_by_an_account_the_rule_does_not_trust_is_refused() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let claim = claim(&rule, CommitAuthority::Head);
    let grant = grant(
        &fixture,
        &rule,
        token(&claim, "stranger", "push", "feature", 42),
    );
    refused(admit(&fixture, &grant, &record).await, "an account");
}

#[tokio::test]
async fn a_push_to_the_parents_branch_is_not_a_preview() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let claim = claim(&rule, CommitAuthority::Head);
    let grant = grant(
        &fixture,
        &rule,
        token(&claim, "parent", "push", "main", ACTOR),
    );
    refused(admit(&fixture, &grant, &record).await, "a branch");
}

#[tokio::test]
async fn a_build_record_with_a_value_of_its_own_is_refused() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let claim = claim(&rule, CommitAuthority::Head);
    let grant = grant(
        &fixture,
        &rule,
        token(&claim, "own", "push", "feature", ACTOR),
    );
    let own = BTreeMap::from([(
        "NPM_TOKEN".to_owned(),
        EnvValue::Sealed(
            fixture
                .sealing
                .seal("NPM_TOKEN", SealedScope::Shared, "own"),
        ),
    )]);
    let own = fixture.sealing.open(own).unwrap().record.unwrap();
    refused(
        admit(&fixture, &grant, &own).await,
        ployz_core::authority::preview::SEALED_VALUE,
    );
}

#[tokio::test]
async fn a_reviewed_dispatch_of_the_reviewed_commit_mints() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let grant = reviewed(&fixture, &rule, review(&fixture, &rule, commit()), "review");
    admit(&fixture, &grant, &record).await.unwrap();
}

#[tokio::test]
async fn a_review_of_another_commit_is_refused() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let other = CommitSha::parse(&"7".repeat(40)).unwrap();
    let grant = reviewed(&fixture, &rule, review(&fixture, &rule, other), "other");
    refused(admit(&fixture, &grant, &record).await, "a commit");
}

#[tokio::test]
async fn a_review_of_another_rule_is_refused() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let mut other = rule.clone();
    other.rule = RuleId::from_bytes([0x33; 16]);
    let grant = reviewed(&fixture, &rule, review(&fixture, &other, commit()), "rule");
    refused(admit(&fixture, &grant, &record).await, "a rule");
}

#[tokio::test]
async fn a_reviewed_build_without_its_review_is_refused() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let mut grant = reviewed(&fixture, &rule, review(&fixture, &rule, commit()), "bare");
    grant.review = None;
    refused(admit(&fixture, &grant, &record).await, "a review");
}

#[tokio::test]
async fn a_preview_grant_token_is_spent_once() {
    let fixture = fixture().await;
    let record = inherited(&fixture);
    let rule = preview_rule(&fixture, &record);
    let claim = claim(&rule, CommitAuthority::Head);
    let grant = grant(
        &fixture,
        &rule,
        token(&claim, "once", "push", "feature", ACTOR),
    );
    admit(&fixture, &grant, &record).await.unwrap();
    assert!(matches!(
        admit(&fixture, &grant, &record).await,
        Err(Error::PreviewGrantRefused(PreviewRefusal::Rule(
            AuthorityRefusal::ProofReplayed
        )))
    ));
}
