#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use serde_json::{Value, json};
use sha2::Digest as _;

use super::*;
use crate::authority::deploy::Creation;
use crate::authority::{
    keys::Signer,
    session::{Delegation, GrantBody, SessionGrant},
    trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
    validity,
};
use crate::{ContainerKind, DeviceKey};

const BUILDER: &str =
    "Ployz-test/build/.github/workflows/build.yml@0123456789abcdef0123456789abcdef01234567";
const BUILDER_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn signer() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn policy() -> BuildPolicy {
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

fn rule() -> AutoDeployRule {
    AutoDeployRule {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        namespace: Namespace::parse("app").unwrap(),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        source: RuleSource {
            repository_id: 1_450_985_696,
            owner_id: 278_111_222,
            repository: "Ployz-test/lab-service".to_owned(),
            branch: "main".to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
            builder: Builder {
                r#ref: BUILDER.to_owned(),
                sha: CommitSha::parse(BUILDER_SHA).unwrap(),
            },
            runner: Runner::GithubHosted,
        },
        services: vec![RuleService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            policy: policy(),
            secrets: None,
        }],
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(DeviceKey::from_bytes(*signer().public().as_bytes())),
    }
}

/// A token's claims, as GitHub writes them, that `rule()` approves.
fn claims_json() -> Value {
    json!({
        "iss": "https://token.actions.githubusercontent.com",
        "aud": "ployz-build:00",
        "jti": "4b6c2f9e-0000-0000-0000-000000000000",
        "sub": "repo:Ployz-test/lab-service:ref:refs/heads/main",
        "iat": 1_800_000_000_u64,
        "exp": 1_800_000_300_u64,
        "nbf": 1_799_999_700_u64,
        "repository_id": "1450985696",
        "repository_owner_id": "278111222",
        "repository": "Ployz-test/lab-service",
        "ref": "refs/heads/main",
        "ref_type": "branch",
        "sha": "fedcba9876543210fedcba9876543210fedcba98",
        "event_name": "push",
        "workflow_ref": "Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main",
        "job_workflow_ref": BUILDER,
        "job_workflow_sha": BUILDER_SHA,
        "runner_environment": "github-hosted",
        "run_id": "42597355451",
        "run_attempt": "1",
        "actor_id": "1234",
        "repository_visibility": "private"
    })
}

fn claims(edit: impl FnOnce(&mut Value)) -> GithubClaims {
    let mut value = claims_json();
    edit(&mut value);
    serde_json::from_value(value).unwrap()
}

#[test]
fn claims_parse_ids_and_seconds_from_githubs_strings() {
    let claims = claims(|_| {});
    assert_eq!(claims.repository_id, 1_450_985_696);
    assert_eq!(claims.run_attempt, 1);
    assert_eq!(claims.iat.as_millis(), 1_800_000_000_000);
    assert_eq!(claims.git_ref, "refs/heads/main");
    for bad in [
        json!(1_450_985_696_u64),
        json!("+1"),
        json!(""),
        json!("1e3"),
    ] {
        let mut value = claims_json();
        value["repository_id"] = bad.clone();
        assert!(
            serde_json::from_value::<GithubClaims>(value).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_rule_admits_the_claims_it_names() {
    assert_eq!(rule().verify_claims(&claims(|_| {})), Ok(()));
    let dispatched = claims(|value| value["event_name"] = json!("workflow_dispatch"));
    assert_eq!(rule().verify_claims(&dispatched), Ok(()));
}

type Edit = fn(&mut Value);

#[test]
fn each_claim_the_rule_names_is_refused_by_name() {
    let cases: [(&str, Edit); 9] = [
        ("repository_id", |value| {
            value["repository_id"] = json!("1450985829")
        }),
        ("repository_owner_id", |value| {
            value["repository_owner_id"] = json!("1")
        }),
        ("ref_type", |value| value["ref_type"] = json!("tag")),
        ("ref", |value| value["ref"] = json!("refs/heads/attacker")),
        ("event_name", |value| {
            value["event_name"] = json!("pull_request_target")
        }),
        ("workflow_ref", |value| {
            value["workflow_ref"] =
                json!("Ployz-test/lab-service/.github/workflows/other.yml@refs/heads/main");
        }),
        ("job_workflow_ref", |value| {
            value["job_workflow_ref"] =
                json!("Ployz-test/build/.github/workflows/build.yml@refs/heads/main");
        }),
        ("job_workflow_sha", |value| {
            value["job_workflow_sha"] = json!("1111111111111111111111111111111111111111");
        }),
        ("runner_environment", |value| {
            value["runner_environment"] = json!("self-hosted")
        }),
    ];
    for (field, edit) in cases {
        assert_eq!(
            rule().verify_claims(&claims(edit)),
            Err(AuthorityRefusal::Mismatch { field }),
            "{field}"
        );
    }
}

#[test]
fn a_tag_with_the_branchs_name_is_refused_as_a_tag() {
    let tag = claims(|value| {
        value["ref_type"] = json!("tag");
        value["ref"] = json!("refs/tags/main");
    });
    assert_eq!(
        rule().verify_claims(&tag),
        Err(AuthorityRefusal::Mismatch { field: "ref_type" })
    );
}

#[test]
fn the_first_failing_claim_in_table_order_is_named() {
    let many = claims(|value| {
        value["runner_environment"] = json!("self-hosted");
        value["job_workflow_sha"] = json!("1111111111111111111111111111111111111111");
        value["ref"] = json!("refs/heads/attacker");
        value["repository_owner_id"] = json!("1");
    });
    assert_eq!(
        rule().verify_claims(&many),
        Err(AuthorityRefusal::Mismatch {
            field: "repository_owner_id"
        })
    );
}

#[test]
fn an_event_the_rule_leaves_out_is_refused() {
    let mut push_only = rule();
    push_only.source.events = vec![RuleEvent::Push];
    let dispatched = claims(|value| value["event_name"] = json!("workflow_dispatch"));
    assert_eq!(
        push_only.verify_claims(&dispatched),
        Err(AuthorityRefusal::Mismatch {
            field: "event_name"
        })
    );
}

/// The builder copies these bytes in D3: a change here changes every audience.
#[test]
fn the_policy_digest_is_pinned() {
    assert_eq!(
        canonical::to_canonical(&policy()).unwrap(),
        r#"{"args":{"NODE_ENV":"production"},"cache":"actions","context":".","method":{"dockerfile":{"path":"Dockerfile"}},"platforms":["linux/amd64"],"secrets":["NPM_TOKEN"],"target":"runtime"}"#
    );
    assert_eq!(
        PolicyDigest::of(&policy()).unwrap().to_string(),
        "be4d54c7c851fe00d3290d61c5a1c3e426386985fdc7fbe36f88572cbb83bbea"
    );
    let railpack = BuildPolicy {
        method: PolicyMethod::Railpack { command: None },
        target: None,
        args: BTreeMap::new(),
        secrets: BTreeSet::new(),
        cache: PolicyCache::Off,
        ..policy()
    };
    assert_eq!(
        canonical::to_canonical(&railpack).unwrap(),
        r#"{"cache":"off","context":".","method":{"railpack":{}},"platforms":["linux/amd64"]}"#
    );
}

fn claim(stage: BuildStage) -> BuildClaim {
    BuildClaim {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        service: web(),
        policy: PolicyDigest::of(&policy()).unwrap(),
        commit: CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
        authority: CommitAuthority::Head,
        stage,
    }
}

fn grant() -> BuildStage {
    BuildStage::Grant {
        holder: BuildGrantId::parse("22".repeat(32)).unwrap(),
        recipient: RecipientKey::from_bytes([0x33; 32]),
    }
}

/// The builder copies these bytes in D3: a change here changes every audience.
#[test]
fn the_build_audience_is_pinned() {
    let grant = claim(grant());
    assert_eq!(
        canonical::to_canonical(&grant).unwrap(),
        concat!(
            r#"{"authority":"head","cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","#,
            r#""commit":"fedcba9876543210fedcba9876543210fedcba98","generation":3,"#,
            r#""policy":"be4d54c7c851fe00d3290d61c5a1c3e426386985fdc7fbe36f88572cbb83bbea","#,
            r#""rule":"11111111111111111111111111111111","service":"web","#,
            r#""stage":{"grant":{"holder":"2222222222222222222222222222222222222222222222222222222222222222","#,
            r#""recipient":"3333333333333333333333333333333333333333333333333333333333333333"}}}"#,
        )
    );
    assert_eq!(
        grant.audience().unwrap(),
        "ployz-build:d381dad98d9e05abc95e1f858a8daec9bac744f9df1e9ba29f4c6f2590053bce"
    );
    let built = claim(BuildStage::Built {
        image: ImageDigest::from_bytes([0x44; 32]),
        attest: AttestKey::from_bytes([0x55; 32]),
    });
    assert_eq!(
        built.audience().unwrap(),
        "ployz-build:ab9f3944cdb7dc7452e273796c83bcc5954e63da07cd1b72b153c6e5e5f70a25"
    );
}

#[test]
fn every_field_of_a_claim_changes_its_audience() {
    let base = claim(grant()).audience().unwrap();
    assert_eq!(base.len(), AUDIENCE_PREFIX.len() + 64);
    let changed = [
        BuildClaim {
            cluster: SealingPublicKey::from_bytes([8; 32]),
            ..claim(grant())
        },
        BuildClaim {
            rule: RuleId::from_bytes([0x12; 16]),
            ..claim(grant())
        },
        BuildClaim {
            generation: 4,
            ..claim(grant())
        },
        BuildClaim {
            service: ServiceName::parse("api").unwrap(),
            ..claim(grant())
        },
        BuildClaim {
            policy: PolicyDigest::from_bytes([0; 32]),
            ..claim(grant())
        },
        BuildClaim {
            commit: CommitSha::from_bytes([0; 20]),
            ..claim(grant())
        },
        claim(BuildStage::Grant {
            holder: BuildGrantId::parse("23".repeat(32)).unwrap(),
            recipient: RecipientKey::from_bytes([0x33; 32]),
        }),
        claim(BuildStage::Grant {
            holder: BuildGrantId::parse("22".repeat(32)).unwrap(),
            recipient: RecipientKey::from_bytes([0x34; 32]),
        }),
    ];
    for other in changed {
        assert_ne!(other.audience().unwrap(), base, "{other:?}");
    }
}

#[test]
fn a_rule_reads_back_only_in_canonical_order() {
    let body = canonical::to_canonical(&rule()).unwrap();
    assert_eq!(
        canonical::from_canonical::<AutoDeployRule>(&body).unwrap(),
        rule()
    );
    let mut unsorted = rule();
    unsorted.source.events = vec![RuleEvent::WorkflowDispatch, RuleEvent::Push];
    let body = canonical::to_canonical(&unsorted).unwrap();
    assert!(canonical::from_canonical::<AutoDeployRule>(&body).is_err());
    let registry = body.replace(r#""cache":"actions""#, r#""cache":"registry""#);
    assert!(canonical::from_canonical::<AutoDeployRule>(&registry).is_err());
}

#[test]
fn a_device_signs_a_rule() {
    let signed = Signed::sign(&rule(), &signer()).unwrap();
    let trusted = TrustedKeys {
        devices: vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        passkeys: Vec::new(),
    };
    let (body, key) =
        verify_signature(&signed, &trusted, Timestamp::from_millis(2_000).unwrap()).unwrap();
    assert_eq!(body, rule());
    assert_eq!(key, Approver::Device(signer().public()));
    assert!(
        signed
            .signing_input()
            .starts_with(b"ployz-authority/v1\nrule\n")
    );
}

#[test]
fn a_browser_key_under_a_valid_session_grant_cannot_sign_a_rule() {
    let browser = Signer::from_seed(&[4; 32]);
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: Namespace::parse("app").unwrap(),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        families: vec![super::super::session::ActionFamily::Deploy],
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let mut browser_rule = rule();
    browser_rule.signer = SignerKey::Browser {
        browser: browser.public(),
    };
    let signed =
        Signed::sign_under(&browser_rule, &browser, SessionGrant { body, assertion }).unwrap();
    let trusted = trusting(&passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    assert!(
        signed.approvers(&trusted, now).is_ok(),
        "the grant itself verifies"
    );
    assert_eq!(
        verify_signature(&signed, &trusted, now).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "the signer"
        }
    );
}

fn passkey_rule() -> AutoDeployRule {
    AutoDeployRule {
        signer: SignerKey::Passkey(crate::authority::restart::OwnPrompt::Passkey),
        ..rule()
    }
}

/// `rule` answered by `passkey`'s own prompt over its signing input.
fn answered(rule: &AutoDeployRule, passkey: &Authenticator) -> Signed<AutoDeployRule> {
    let unsigned = Signed::unsigned(rule).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    unsigned.answered(assertion)
}

#[test]
fn a_passkeys_own_prompt_signs_a_rule() {
    let passkey = Authenticator::es256(4);
    let signed = answered(&passkey_rule(), &passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    let (body, approver) = verify_signature(&signed, &trusting(&passkey), now).unwrap();
    assert_eq!(body, passkey_rule());
    assert_eq!(approver, Approver::Passkey(passkey.passkey().code()));
    let input = super::super::keys::signing_input(Purpose::Rule, &signed.body);
    assert_eq!(
        signed.challenge(),
        <[u8; 32]>::from(Sha256::digest(input)),
        "the challenge is the digest of the rule's own signing input"
    );
}

#[test]
fn a_passkey_answer_counts_only_for_the_rule_it_answered_and_a_trusted_passkey() {
    let passkey = Authenticator::es256(4);
    let now = Timestamp::from_millis(2_000).unwrap();
    let mut other = passkey_rule();
    other.generation += 1;
    let mut moved = answered(&passkey_rule(), &passkey);
    moved.body = Signed::unsigned(&other).unwrap().body;
    assert_eq!(
        verify_signature(&moved, &trusting(&passkey), now).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "a challenge"
        }
    );
    let stranger = Authenticator::es256(5);
    assert_eq!(
        verify_signature(
            &answered(&passkey_rule(), &stranger),
            &trusting(&passkey),
            now
        )
        .unwrap_err(),
        AuthorityRefusal::UnknownPasskey
    );
    assert_eq!(
        verify_signature(&answered(&rule(), &passkey), &trusting(&passkey), now).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "the signer"
        },
        "a body naming a Device is not a passkey's"
    );
}

#[test]
fn a_passkeys_own_answer_never_signs_a_workflow() {
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: signer().public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: Namespace::parse("app").unwrap(),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        families: vec![super::super::session::ActionFamily::Deploy],
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let unsigned = Signed::unsigned(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    assert_eq!(
        unsigned
            .answered(assertion)
            .approvers(&trusting(&passkey), Timestamp::from_millis(2_000).unwrap())
            .unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a passkey" }
    );
}

const BUILT: &str =
    "ployz-build/web@sha256:4444444444444444444444444444444444444444444444444444444444444444";
const SEALED_A: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

/// A built Service with a plain value, a Sealed value, a Bind Mount, a host port and a hook.
fn requested_json() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 2},
        "container": {
            "image": BUILT,
            "pull_policy": "missing",
            "environment": {
                "PLAIN": "one",
                "SECRET": {"sealed": SEALED_A, "scope": "shared", "key": "SECRET"}
            },
            "sealed_identity": {
                "organization": "org",
                "environment": "11111111-1111-1111-1111-111111111111",
                "service": "22222222-2222-2222-2222-222222222222"
            }
        },
        "ports": [{"mode": "host", "bind": {"kind": "all"}, "published_port": 8080,
                   "container_port": 80, "transport_protocol": "tcp"}],
        "volumes": [{"reference": "host", "source": {"kind": "bind", "machine_path": "/srv/web"}}],
        "mounts": [{"volume": "host", "target": "/srv"}],
        "pre_deploy": {"command": ["migrate"]}
    })
}

fn requested(value: Value) -> crate::RequestedServiceSpec {
    serde_json::from_value(value).unwrap()
}

fn resolved(spec: &crate::RequestedServiceSpec) -> crate::ResolvedServiceSpec {
    let mut spec = spec.clone();
    spec.mount_graph = spec
        .mount_graph
        .scope_to_namespace(&Namespace::parse("app").unwrap())
        .unwrap();
    spec.to_resolved(
        crate::ServiceId::parse("c".repeat(32)).unwrap(),
        crate::ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

fn approved_rule() -> AutoDeployRule {
    let mut rule = rule();
    rule.services[0].template = template(
        &requested(requested_json()),
        &Namespace::parse("app").unwrap(),
    )
    .unwrap();
    rule
}

fn built() -> BuildClaim {
    claim(BuildStage::Built {
        image: ImageDigest::from_bytes([0x44; 32]),
        attest: AttestKey::from_bytes([0x55; 32]),
    })
}

/// Check a creation of `spec` under `rule` with `claim`, on a Machine whose head is the
/// claim's commit.
fn create(
    rule: &AutoDeployRule,
    claim: &BuildClaim,
    spec: Value,
    kind: ContainerKind,
) -> Result<ImageDigest, AuthorityRefusal> {
    create_at(rule, claim, spec, kind, CommitNow::Head(head()))
}

fn head() -> CommitSha {
    CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap()
}

/// Check a creation of `spec` under `rule` with `claim`, with `commit` the reason it may
/// run now.
fn create_at(
    rule: &AutoDeployRule,
    claim: &BuildClaim,
    spec: Value,
    kind: ContainerKind,
    commit: CommitNow<'_>,
) -> Result<ImageDigest, AuthorityRefusal> {
    let spec = resolved(&requested(spec));
    let machine = MachineId::parse("1".repeat(32)).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    let facts = RuleFacts {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        attest: AttestKey::from_bytes([0x55; 32]),
        machine: &machine,
        namespace: &namespace,
        service: &spec.name,
        commit,
    };
    rule.verify_creation(
        claim,
        &facts,
        Creation {
            service: &spec.name,
            kind,
            spec: &spec,
        },
    )
}

fn changed(path: &[&str], value: Value) -> Value {
    let mut spec = requested_json();
    let mut at = &mut spec;
    for key in path {
        at = if let Ok(index) = key.parse::<usize>() {
            &mut at[index]
        } else {
            &mut at[*key]
        };
    }
    *at = value;
    spec
}

#[test]
fn the_template_leaves_out_what_the_build_binds() {
    let namespace = Namespace::parse("app").unwrap();
    let other = changed(&["container", "image"], json!("nginx:latest"));
    assert_eq!(
        template(&requested(requested_json()), &namespace).unwrap(),
        template(&requested(other), &namespace).unwrap()
    );
    // A Saved spec pulls when missing; the deploy binds the built image with `never`.
    let bound = changed(&["container", "pull_policy"], json!("never"));
    assert_eq!(
        template(&requested(requested_json()), &namespace).unwrap(),
        template(&requested(bound), &namespace).unwrap()
    );
    let mut plain = requested(requested_json());
    plain.container.image = String::new();
    plain.container.pull_policy = crate::PullPolicy::Never;
    assert_eq!(
        template(&requested(requested_json()), &namespace).unwrap(),
        PayloadDigest::of(&plain, &namespace).unwrap()
    );
}

#[test]
fn a_rule_admits_the_heads_image_with_the_settings_it_approves() {
    let rule = approved_rule();
    assert_eq!(
        create(
            &rule,
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Ok(ImageDigest::from_bytes([0x44; 32]))
    );
    assert_eq!(
        create(
            &rule,
            &built(),
            requested_json(),
            ContainerKind::PreDeployHook
        ),
        Ok(ImageDigest::from_bytes([0x44; 32]))
    );
}

#[test]
fn each_change_to_an_approved_creation_is_refused_by_name() {
    let rule = approved_rule();
    let other_image =
        "ployz-build/web@sha256:6666666666666666666666666666666666666666666666666666666666666666";
    let spec_cases: Vec<(&str, Value, &str)> = vec![
        (
            "image digest",
            changed(&["container", "image"], json!(other_image)),
            "an image",
        ),
        (
            "image tag",
            changed(&["container", "image"], json!("ployz-build/web:latest")),
            "an image",
        ),
        (
            "plain value",
            changed(&["container", "environment", "PLAIN"], json!("two")),
            "a payload",
        ),
        (
            "Sealed ciphertext",
            changed(
                &["container", "environment", "SECRET", "sealed"],
                json!("QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQg=="),
            ),
            "a payload",
        ),
        (
            "privileged",
            changed(&["container", "privileged"], json!(true)),
            "a payload",
        ),
        (
            "Bind Mount",
            changed(&["volumes", "0", "source", "machine_path"], json!("/")),
            "a payload",
        ),
        (
            "host port",
            changed(&["ports", "0", "published_port"], json!(22)),
            "a payload",
        ),
        (
            "hook command",
            changed(&["pre_deploy", "command"], json!(["sh", "-c", "curl evil"])),
            "a payload",
        ),
    ];
    for (case, spec, field) in spec_cases {
        assert_eq!(
            create(&rule, &built(), spec, ContainerKind::ServiceContainer),
            Err(AuthorityRefusal::Mismatch { field }),
            "{case}"
        );
    }
    let claim_cases: Vec<(&str, BuildClaim, &str)> = vec![
        (
            "lower generation",
            BuildClaim {
                generation: 2,
                ..built()
            },
            "a generation",
        ),
        (
            "commit not the head",
            BuildClaim {
                commit: CommitSha::parse(&"0".repeat(40)).unwrap(),
                ..built()
            },
            "a commit",
        ),
        (
            "aud naming another image",
            claim(BuildStage::Built {
                image: ImageDigest::from_bytes([0x66; 32]),
                attest: AttestKey::from_bytes([0x55; 32]),
            }),
            "an image",
        ),
        (
            "attest naming another key",
            claim(BuildStage::Built {
                image: ImageDigest::from_bytes([0x44; 32]),
                attest: AttestKey::from_bytes([0x77; 32]),
            }),
            "the attest key",
        ),
        ("the grant token", claim(grant()), "a stage"),
        (
            "another rule",
            BuildClaim {
                rule: RuleId::from_bytes([0x12; 16]),
                ..built()
            },
            "a rule",
        ),
        (
            "another policy",
            BuildClaim {
                policy: PolicyDigest::from_bytes([1; 32]),
                ..built()
            },
            "a policy",
        ),
        (
            "another Cluster",
            BuildClaim {
                cluster: SealingPublicKey::from_bytes([8; 32]),
                ..built()
            },
            "a Cluster",
        ),
    ];
    for (case, claim, field) in claim_cases {
        assert_eq!(
            create(
                &rule,
                &claim,
                requested_json(),
                ContainerKind::ServiceContainer
            ),
            Err(AuthorityRefusal::Mismatch { field }),
            "{case}"
        );
    }
}

#[test]
fn a_rule_refuses_outside_its_environment_machines_and_live_generations() {
    let ended = AutoDeployRule {
        ended: true,
        ..approved_rule()
    };
    assert_eq!(
        create(
            &ended,
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an ended rule"
        })
    );
    let elsewhere = AutoDeployRule {
        machines: vec![MachineId::parse("2".repeat(32)).unwrap()],
        ..approved_rule()
    };
    assert_eq!(
        create(
            &elsewhere,
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch { field: "a Machine" })
    );
    let other = AutoDeployRule {
        namespace: Namespace::parse("other").unwrap(),
        ..approved_rule()
    };
    assert_eq!(
        create(
            &other,
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an Environment"
        })
    );
    let api = changed(&["name"], json!("api"));
    assert_eq!(
        create(
            &approved_rule(),
            &built(),
            api,
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch { field: "a Service" })
    );
}

#[test]
fn a_hook_creation_needs_a_hook_in_the_spec() {
    let mut spec = requested_json();
    spec.as_object_mut().unwrap().remove("pre_deploy");
    let mut rule = approved_rule();
    rule.services[0].template =
        template(&requested(spec.clone()), &Namespace::parse("app").unwrap()).unwrap();
    assert_eq!(
        create(&rule, &built(), spec, ContainerKind::PreDeployHook),
        Err(AuthorityRefusal::Mismatch { field: "a hook" })
    );
}

/// An older commit of `web` a person rolls back to.
fn older() -> CommitSha {
    CommitSha::parse(&"0a".repeat(20)).unwrap()
}

fn machine() -> MachineId {
    MachineId::parse("1".repeat(32)).unwrap()
}

fn approval(edit: impl FnOnce(&mut CommitApproval)) -> CommitApproval {
    let mut approval = CommitApproval {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        service: web(),
        commit: older(),
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        expires_at: Timestamp::from_millis(1_000 + validity::MAX_LIFETIME_MILLIS).unwrap(),
        signer: SignerKey::Device(DeviceKey::from_bytes(*signer().public().as_bytes())),
    };
    edit(&mut approval);
    approval
}

fn device_trusted() -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        passkeys: Vec::new(),
    }
}

/// Check `approval`, signed by the trusted Device, against `rule` at `now`.
fn checked_at(
    approval: &CommitApproval,
    rule: &AutoDeployRule,
    now: u64,
) -> Result<CheckedCommitApproval, AuthorityRefusal> {
    let signed = Signed::sign(approval, &signer()).unwrap();
    verify_commit_approval(
        &signed,
        rule,
        &ApprovalFacts {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            machine: &machine(),
            trusted: &device_trusted(),
            now: Timestamp::from_millis(now).unwrap(),
        },
    )
}

fn checked(approval: &CommitApproval) -> CheckedCommitApproval {
    checked_at(approval, &approved_rule(), 2_000).unwrap()
}

/// The image the push of `older()` built, as its token's claim names it.
fn built_older(authority: CommitAuthority) -> BuildClaim {
    BuildClaim {
        commit: older(),
        authority,
        ..built()
    }
}

#[test]
fn a_commit_approval_is_a_workflow_statement_whose_digest_is_its_signing_input() {
    let signed = Signed::sign(&approval(|_| {}), &signer()).unwrap();
    assert!(
        signed
            .signing_input()
            .starts_with(b"ployz-authority/v1\nworkflow\n")
    );
    let digest = ApprovalDigest::of(&signed);
    assert_eq!(
        digest.as_bytes(),
        &<[u8; 32]>::from(sha2::Sha256::digest(signed.signing_input()))
    );
    assert_eq!(checked(&approval(|_| {})).digest(), digest);
    let claim = built_older(CommitAuthority::Approved(digest));
    let text = canonical::to_canonical(&claim).unwrap();
    assert!(
        text.starts_with(&format!(r#"{{"authority":{{"approved":"{digest}"}},"#)),
        "{text}"
    );
}

#[test]
fn a_live_approval_admits_the_approved_commits_image_with_the_rules_settings() {
    let approved = checked(&approval(|_| {}));
    let digest = approved.digest();
    for authority in [CommitAuthority::Head, CommitAuthority::Approved(digest)] {
        assert_eq!(
            create_at(
                &approved_rule(),
                &built_older(authority),
                requested_json(),
                ContainerKind::ServiceContainer,
                CommitNow::Approved(&approved),
            ),
            Ok(ImageDigest::from_bytes([0x44; 32])),
            "{authority:?}"
        );
    }
}

#[test]
fn an_approval_admits_only_the_service_and_commit_it_names() {
    let approved = checked(&approval(|_| {}));
    for (case, claim) in [
        ("the head, not the approved commit", built()),
        (
            "another commit",
            BuildClaim {
                commit: CommitSha::parse(&"0b".repeat(20)).unwrap(),
                ..built()
            },
        ),
    ] {
        assert_eq!(
            create_at(
                &approved_rule(),
                &claim,
                requested_json(),
                ContainerKind::ServiceContainer,
                CommitNow::Approved(&approved),
            ),
            Err(AuthorityRefusal::Mismatch {
                field: "an approval"
            }),
            "{case}"
        );
    }
    let api = checked(&approval(|approval| {
        approval.service = ServiceName::parse("api").unwrap();
    }));
    assert_eq!(
        create_at(
            &approved_rule(),
            &built_older(CommitAuthority::Head),
            requested_json(),
            ContainerKind::ServiceContainer,
            CommitNow::Approved(&api),
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an approval"
        })
    );
}

/// Hole 1 of D6's first design: Cloud leaves a build out and swaps the image.
#[test]
fn hole_1_an_image_that_is_not_the_tokens_is_refused_under_a_valid_approval() {
    let approved = checked(&approval(|_| {}));
    let other_image =
        "ployz-build/web@sha256:6666666666666666666666666666666666666666666666666666666666666666";
    assert_eq!(
        create_at(
            &approved_rule(),
            &built_older(CommitAuthority::Head),
            changed(&["container", "image"], json!(other_image)),
            ContainerKind::ServiceContainer,
            CommitNow::Approved(&approved),
        ),
        Err(AuthorityRefusal::Mismatch { field: "an image" })
    );
}

/// Hole 2 of D6's first design: Cloud keeps the genuine image and changes its settings.
#[test]
fn hole_2_settings_that_are_not_the_rules_template_are_refused_under_a_valid_approval() {
    let approved = checked(&approval(|_| {}));
    for (case, spec) in [
        (
            "entrypoint",
            changed(&["container", "entrypoint"], json!(["node", "-e"])),
        ),
        (
            "hook command",
            changed(&["pre_deploy", "command"], json!(["sh", "-c", "curl evil"])),
        ),
        (
            "plain value",
            changed(&["container", "environment", "PLAIN"], json!("two")),
        ),
    ] {
        assert_eq!(
            create_at(
                &approved_rule(),
                &built_older(CommitAuthority::Head),
                spec,
                ContainerKind::ServiceContainer,
                CommitNow::Approved(&approved),
            ),
            Err(AuthorityRefusal::Mismatch { field: "a payload" }),
            "{case}"
        );
    }
}

/// Finding 6 of the step-back review: Cloud reports its own attest key, so the build's
/// secrets came from an endpoint it ran.
#[test]
fn finding_6_a_claim_naming_another_attest_key_is_refused_under_a_valid_approval() {
    let approved = checked(&approval(|_| {}));
    let claim = BuildClaim {
        stage: BuildStage::Built {
            image: ImageDigest::from_bytes([0x44; 32]),
            attest: AttestKey::from_bytes([0x77; 32]),
        },
        ..built_older(CommitAuthority::Head)
    };
    assert_eq!(
        create_at(
            &approved_rule(),
            &claim,
            requested_json(),
            ContainerKind::ServiceContainer,
            CommitNow::Approved(&approved),
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "the attest key"
        })
    );
}

/// A build under an approval names the commit only in its audience: GitHub's `sha` is the
/// head the dispatch ran at, and may be that same commit.
#[test]
fn a_claim_built_under_an_approval_never_deploys_on_the_head_read_alone() {
    let approved = checked(&approval(|_| {}));
    let claim = BuildClaim {
        authority: CommitAuthority::Approved(approved.digest()),
        ..built()
    };
    assert_eq!(
        create(
            &approved_rule(),
            &claim,
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an authority"
        })
    );
    let reviewed = BuildClaim {
        authority: CommitAuthority::Review(ReviewDigest::from_bytes([1; 32])),
        ..built_older(CommitAuthority::Head)
    };
    assert_eq!(
        create_at(
            &approved_rule(),
            &reviewed,
            requested_json(),
            ContainerKind::ServiceContainer,
            CommitNow::Approved(&approved),
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an authority"
        })
    );
}

#[test]
fn an_approval_is_refused_for_another_rule_generation_cluster_or_an_ended_rule() {
    let cases: Vec<(&str, CommitApproval, AutoDeployRule, &str)> = vec![
        (
            "another rule",
            approval(|approval| approval.rule = RuleId::from_bytes([0x12; 16])),
            approved_rule(),
            "a rule",
        ),
        (
            "an older generation",
            approval(|approval| approval.generation = 2),
            approved_rule(),
            "a generation",
        ),
        (
            "a newer generation than the rule in hand",
            approval(|approval| approval.generation = 4),
            approved_rule(),
            "a generation",
        ),
        (
            "another Cluster",
            approval(|approval| approval.cluster = SealingPublicKey::from_bytes([8; 32])),
            approved_rule(),
            "a Cluster",
        ),
        (
            "an ended rule",
            approval(|_| {}),
            AutoDeployRule {
                ended: true,
                ..approved_rule()
            },
            "an ended rule",
        ),
    ];
    for (case, approval, rule, field) in cases {
        assert_eq!(
            checked_at(&approval, &rule, 2_000),
            Err(AuthorityRefusal::Mismatch { field }),
            "{case}"
        );
    }
}

#[test]
fn an_approval_lives_only_in_its_window() {
    let live = approval(|_| {});
    assert!(
        checked_at(
            &live,
            &approved_rule(),
            1_000 + validity::MAX_LIFETIME_MILLIS
        )
        .is_ok()
    );
    assert_eq!(
        checked_at(
            &live,
            &approved_rule(),
            1_000 + validity::MAX_LIFETIME_MILLIS + validity::CLOCK_SKEW_MILLIS + 1
        ),
        Err(AuthorityRefusal::Expired)
    );
    let long = approval(|approval| {
        approval.expires_at =
            Timestamp::from_millis(1_000 + validity::MAX_LIFETIME_MILLIS + 1).unwrap();
    });
    assert!(checked_at(&long, &approved_rule(), 2_000).is_err());
}

#[test]
fn an_approval_by_a_key_this_machine_does_not_trust_is_refused() {
    let stranger = Signer::from_seed(&[8; 32]);
    let body = approval(|approval| {
        approval.signer = SignerKey::Device(DeviceKey::from_bytes(*stranger.public().as_bytes()));
    });
    let signed = Signed::sign(&body, &stranger).unwrap();
    let refused = verify_commit_approval(
        &signed,
        &approved_rule(),
        &ApprovalFacts {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            machine: &machine(),
            trusted: &device_trusted(),
            now: Timestamp::from_millis(2_000).unwrap(),
        },
    );
    assert!(
        matches!(refused, Err(AuthorityRefusal::UnknownKey { .. })),
        "{refused:?}"
    );
    let named_other = approval(|approval| {
        approval.signer = SignerKey::Device(DeviceKey::from_bytes(*stranger.public().as_bytes()));
    });
    assert_eq!(
        checked_at(&named_other, &approved_rule(), 2_000),
        Err(AuthorityRefusal::Mismatch {
            field: "the signer"
        })
    );
}

/// A browser key under a session grant for `environment`, and the approval it signs.
fn session_signed(environment: &str) -> (Signed<CommitApproval>, TrustedKeys) {
    let browser = Signer::from_seed(&[4; 32]);
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: Namespace::parse(environment).unwrap(),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![machine()],
        families: vec![super::super::session::ActionFamily::Deploy],
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let approval = approval(|approval| {
        approval.signer = SignerKey::Browser {
            browser: browser.public(),
        };
    });
    let signed = Signed::sign_under(&approval, &browser, SessionGrant { body, assertion }).unwrap();
    (signed, trusting(&passkey))
}

#[test]
fn a_session_grant_approves_a_commit_only_in_the_rules_environment() {
    let machine = machine();
    let facts = |trusted| ApprovalFacts {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        machine: &machine,
        trusted,
        now: Timestamp::from_millis(2_000).unwrap(),
    };
    let (signed, trusted) = session_signed("app");
    let approved = verify_commit_approval(&signed, &approved_rule(), &facts(&trusted)).unwrap();
    assert!(matches!(approved.by(), Approver::Browser { .. }));
    let (elsewhere, trusted) = session_signed("other");
    assert!(
        verify_commit_approval(&elsewhere, &approved_rule(), &facts(&trusted)).is_err(),
        "a grant for another Environment"
    );
}
