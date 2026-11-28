#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use crate::authority::MismatchField;
use serde_json::{Value, json};
use sha2::Digest as _;

use super::*;
use crate::authority::deploy::Creation;
use crate::authority::{
    keys::Signer,
    trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
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
        proposal: None,
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 3,
        namespace: Namespace::parse("app").unwrap(),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        source: RuleSource::Github(GithubSource {
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
        }),
        services: vec![RuleService {
            name: web(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            policy: Some(policy()),
            secrets: None,
            seeded: None,
        }],
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(DeviceKey::from_bytes(*signer().public().as_bytes())),
        preview: None,
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

/// [`super::verify_signature`] on a fresh admission's budget.
fn verify_signature(
    rule: &Signed<AutoDeployRule>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(AutoDeployRule, Approver), AuthorityRefusal> {
    super::verify_signature(Budget::default().charge(rule, trusted)?, cluster)
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
    assert_eq!(
        rule().verify_claims(&claims(|_| {}), &CommitAuthority::Head),
        Ok(())
    );
    let dispatched = claims(|value| value["event_name"] = json!("workflow_dispatch"));
    assert_eq!(
        rule().verify_claims(&dispatched, &CommitAuthority::Head),
        Ok(())
    );
}

type Edit = fn(&mut Value);

#[test]
fn each_claim_the_rule_names_is_refused_by_name() {
    let cases: [(MismatchField, Edit); 9] = [
        (MismatchField::ClaimRepositoryId, |value| {
            value["repository_id"] = json!("1450985829")
        }),
        (MismatchField::ClaimRepositoryOwnerId, |value| {
            value["repository_owner_id"] = json!("1")
        }),
        (MismatchField::ClaimRefType, |value| {
            value["ref_type"] = json!("tag")
        }),
        (MismatchField::ClaimRef, |value| {
            value["ref"] = json!("refs/heads/attacker")
        }),
        (MismatchField::ClaimEventName, |value| {
            value["event_name"] = json!("pull_request_target")
        }),
        (MismatchField::ClaimWorkflowRef, |value| {
            value["workflow_ref"] =
                json!("Ployz-test/lab-service/.github/workflows/other.yml@refs/heads/main");
        }),
        (MismatchField::ClaimJobWorkflowRef, |value| {
            value["job_workflow_ref"] =
                json!("Ployz-test/build/.github/workflows/build.yml@refs/heads/main");
        }),
        (MismatchField::ClaimJobWorkflowSha, |value| {
            value["job_workflow_sha"] = json!("1111111111111111111111111111111111111111");
        }),
        (MismatchField::ClaimRunnerEnvironment, |value| {
            value["runner_environment"] = json!("self-hosted")
        }),
    ];
    for (field, edit) in cases {
        assert_eq!(
            rule().verify_claims(&claims(edit), &CommitAuthority::Head),
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
        rule().verify_claims(&tag, &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimRefType
        })
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
        rule().verify_claims(&many, &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimRepositoryOwnerId
        })
    );
}

#[test]
fn an_event_the_rule_leaves_out_is_refused() {
    let mut push_only = rule();
    github_mut(&mut push_only).events = vec![RuleEvent::Push];
    let dispatched = claims(|value| value["event_name"] = json!("workflow_dispatch"));
    assert_eq!(
        push_only.verify_claims(&dispatched, &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimEventName
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
        image: BuiltImageDigest::from_bytes([0x44; 32]),
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
    github_mut(&mut unsorted).events = vec![RuleEvent::WorkflowDispatch, RuleEvent::Push];
    let body = canonical::to_canonical(&unsorted).unwrap();
    assert!(canonical::from_canonical::<AutoDeployRule>(&body).is_err());
    let registry = body.replace(r#""cache":"actions""#, r#""cache":"registry""#);
    assert!(canonical::from_canonical::<AutoDeployRule>(&registry).is_err());
}

#[test]
fn a_device_signs_a_rule() {
    let signed = Signed::sign(&rule(), &signer()).unwrap();
    let trusted = TrustedKeys::new(
        vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        Vec::new(),
        [],
    );
    let (body, key) = verify_signature(
        &signed,
        &trusted,
        Some(SealingPublicKey::from_bytes([9; 32])),
    )
    .unwrap();
    assert_eq!(body, rule());
    assert_eq!(key, Approver::Signing(signer().public()));
    assert!(
        signed
            .signing_input()
            .starts_with(b"ployz-authority/v1\nrule\n")
    );
}

#[test]
fn a_ci_key_signs_no_rule() {
    use crate::authority::{
        Cap, CapSet, Grant, GrantKind, KeyKind, KeyPlaces, KeyScope, TrustedGrant,
    };
    let ci = Signer::from_seed(&[2; 32]);
    let body = AutoDeployRule {
        signer: SignerKey::Device(ci.public().into()),
        ..rule()
    };
    let trusted = TrustedKeys::new(
        [],
        [],
        [TrustedGrant {
            grant: Grant {
                key: ci.public(),
                kind: GrantKind::Ci,
                scope: KeyScope {
                    caps: CapSet::of(&[Cap::Spec]),
                    places: KeyPlaces::Only(vec![crate::authority::KeyPlace {
                        environment: uuid::Uuid::nil(),
                        namespace: crate::Namespace::parse("app").unwrap(),
                        service: None,
                    }]),
                },
                volumes: Vec::new(),
                issued_at: Timestamp::from_millis(1).unwrap(),
                name: None,
            },
            approved_by: crate::authority::PasskeyApprover::Server,
        }],
    );

    assert_eq!(
        verify_signature(
            &Signed::sign(&body, &ci).unwrap(),
            &trusted,
            Some(SealingPublicKey::from_bytes([9; 32])),
        ),
        Err(AuthorityRefusal::OutOfScope {
            code: ci.public().code(),
            kind: KeyKind::Ci,
            what: "an automatic deploy rule",
        }),
    );
}

fn passkey_rule() -> AutoDeployRule {
    AutoDeployRule {
        proposal: None,
        signer: SignerKey::Passkey(crate::authority::keys::OwnPrompt::Passkey),
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
    let (body, approver) = verify_signature(
        &signed,
        &trusting(&passkey),
        Some(SealingPublicKey::from_bytes([9; 32])),
    )
    .unwrap();
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
    let mut other = passkey_rule();
    other.generation += 1;
    let mut moved = answered(&passkey_rule(), &passkey);
    moved.body = Signed::unsigned(&other).unwrap().body;
    assert_eq!(
        verify_signature(
            &moved,
            &trusting(&passkey),
            Some(SealingPublicKey::from_bytes([9; 32]))
        )
        .unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Challenge
        }
    );
    let stranger = Authenticator::es256(5);
    assert_eq!(
        verify_signature(
            &answered(&passkey_rule(), &stranger),
            &trusting(&passkey),
            Some(SealingPublicKey::from_bytes([9; 32]))
        )
        .unwrap_err(),
        AuthorityRefusal::UnknownPasskey
    );
    assert_eq!(
        verify_signature(
            &answered(&rule(), &passkey),
            &trusting(&passkey),
            Some(SealingPublicKey::from_bytes([9; 32]))
        )
        .unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Signer
        },
        "a body naming a Device is not a passkey's"
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
        image: BuiltImageDigest::from_bytes([0x44; 32]),
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
) -> Result<BuiltImageDigest, AuthorityRefusal> {
    create_on(rule, claim, spec, kind, (Some(head()), "1"))
}

fn head() -> CommitSha {
    CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap()
}

/// Check a creation of `spec` under `rule` with `claim` on the Machine whose ID repeats
/// `machine`, with the branch head read online, or none for a verified build.
fn create_on(
    rule: &AutoDeployRule,
    claim: &BuildClaim,
    spec: Value,
    kind: ContainerKind,
    (head, machine): (Option<CommitSha>, &str),
) -> Result<BuiltImageDigest, AuthorityRefusal> {
    let spec = resolved(&requested(spec));
    let machine = MachineId::parse(machine.repeat(32)).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    let facts = RuleFacts {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        attest: AttestKey::from_bytes([0x55; 32]),
        machine: &machine,
        namespace: &namespace,
        service: &spec.name,
        head,
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
        Ok(BuiltImageDigest::from_bytes([0x44; 32]))
    );
    assert_eq!(
        create(
            &rule,
            &built(),
            requested_json(),
            ContainerKind::PreDeployHook
        ),
        Ok(BuiltImageDigest::from_bytes([0x44; 32]))
    );
}

#[test]
fn each_change_to_an_approved_creation_is_refused_by_name() {
    let rule = approved_rule();
    let other_image =
        "ployz-build/web@sha256:6666666666666666666666666666666666666666666666666666666666666666";
    let spec_cases: Vec<(&str, Value, MismatchField)> = vec![
        (
            "image digest",
            changed(&["container", "image"], json!(other_image)),
            MismatchField::Image,
        ),
        (
            "image tag",
            changed(&["container", "image"], json!("ployz-build/web:latest")),
            MismatchField::Image,
        ),
        (
            "plain value",
            changed(&["container", "environment", "PLAIN"], json!("two")),
            MismatchField::Payload,
        ),
        (
            "Sealed ciphertext",
            changed(
                &["container", "environment", "SECRET", "sealed"],
                json!("QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQg=="),
            ),
            MismatchField::Payload,
        ),
        (
            "privileged",
            changed(&["container", "privileged"], json!(true)),
            MismatchField::Payload,
        ),
        (
            "Bind Mount",
            changed(&["volumes", "0", "source", "machine_path"], json!("/")),
            MismatchField::Payload,
        ),
        (
            "host port",
            changed(&["ports", "0", "published_port"], json!(22)),
            MismatchField::Payload,
        ),
        (
            "hook command",
            changed(&["pre_deploy", "command"], json!(["sh", "-c", "curl evil"])),
            MismatchField::Payload,
        ),
    ];
    for (case, spec, field) in spec_cases {
        assert_eq!(
            create(&rule, &built(), spec, ContainerKind::ServiceContainer),
            Err(AuthorityRefusal::Mismatch { field }),
            "{case}"
        );
    }
    let claim_cases: Vec<(&str, BuildClaim, MismatchField)> = vec![
        (
            "lower generation",
            BuildClaim {
                generation: 2,
                ..built()
            },
            MismatchField::Generation,
        ),
        (
            "commit not the head",
            BuildClaim {
                commit: CommitSha::parse(&"0".repeat(40)).unwrap(),
                ..built()
            },
            MismatchField::Commit,
        ),
        (
            "aud naming another image",
            claim(BuildStage::Built {
                image: BuiltImageDigest::from_bytes([0x66; 32]),
                attest: AttestKey::from_bytes([0x55; 32]),
            }),
            MismatchField::Image,
        ),
        (
            "attest naming another key",
            claim(BuildStage::Built {
                image: BuiltImageDigest::from_bytes([0x44; 32]),
                attest: AttestKey::from_bytes([0x77; 32]),
            }),
            MismatchField::AttestKey,
        ),
        ("the grant token", claim(grant()), MismatchField::Stage),
        (
            "another rule",
            BuildClaim {
                rule: RuleId::from_bytes([0x12; 16]),
                ..built()
            },
            MismatchField::Rule,
        ),
        (
            "another policy",
            BuildClaim {
                policy: PolicyDigest::from_bytes([1; 32]),
                ..built()
            },
            MismatchField::Policy,
        ),
        (
            "another Cluster",
            BuildClaim {
                cluster: SealingPublicKey::from_bytes([8; 32]),
                ..built()
            },
            MismatchField::Cluster,
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
        proposal: None,
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
            field: MismatchField::EndedRule
        })
    );
    let elsewhere = AutoDeployRule {
        proposal: None,
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
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Machine
        })
    );
    let other = AutoDeployRule {
        proposal: None,
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
            field: MismatchField::Environment
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
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Service
        })
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
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Hook
        })
    );
}

#[test]
fn a_commit_shows_its_first_twelve_digits() {
    let commit = CommitSha::parse(BUILDER_SHA).unwrap();
    assert_eq!(commit.short(), "0123456789ab");
}

#[test]
fn a_build_by_a_builder_that_installs_a_lab_ployz_is_refused_under_a_rule_pinning_it() {
    let lab = "a19b4fca3adea371c25ec29f181e8c7d21037d1e";
    let mut pinned = rule();
    github_mut(&mut pinned).builder = Builder {
        r#ref: format!("getployz/build/.github/workflows/build.yml@{lab}"),
        sha: CommitSha::parse(lab).unwrap(),
    };
    let ran = claims(|value| {
        value["job_workflow_ref"] = json!(pinned.source.github().unwrap().builder.r#ref);
        value["job_workflow_sha"] = json!(lab);
    });
    assert_eq!(
        pinned.verify_claims(&ran, &CommitAuthority::Head),
        Err(AuthorityRefusal::RetiredBuilder {
            sha: lab.to_owned()
        })
    );
}

/// A rule approves a template for its own Environment, and a Volume born elsewhere is
/// refused before the template is compared.
#[test]
fn a_creation_naming_a_volume_born_in_another_environment_is_refused() {
    let born_in = |namespace: &str| {
        let mut spec = requested_json();
        spec["volumes"] = json!([{"reference": "data", "source": {"kind": "provisioned",
            "name": "data", "maximum_bytes": 1_073_741_824_u64,
            "birth": {"namespace": namespace, "name": "data", "made_at": 1_762_178_640_000_u64,
                "nonce": "00112233445566778899aabbccddeeff"}}}]);
        spec["mounts"] = json!([{"volume": "data", "target": "/data"}]);
        spec
    };
    let mut rule = approved_rule();
    rule.services[0].template = template(
        &requested(born_in("app")),
        &Namespace::parse("app").unwrap(),
    )
    .unwrap();

    assert!(
        create(
            &rule,
            &built(),
            born_in("app"),
            ContainerKind::ServiceContainer
        )
        .is_ok()
    );
    assert_eq!(
        create(
            &rule,
            &built(),
            born_in("other"),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::VolumesEnvironment
        })
    );
}

/// A spec that keeps no data on its Server and reaches nothing of the host runs on any
/// Server of the Cluster, one that joined after the approval included. Any other keeps
/// to the Servers the rule names.
#[test]
fn only_a_spec_whose_placement_is_signed_keeps_to_the_rules_machines() {
    let mut stateless = requested_json();
    for key in ["volumes", "mounts", "ports"] {
        stateless.as_object_mut().unwrap().remove(key);
    }
    let mut rule = approved_rule();
    rule.services[0].template = template(
        &requested(stateless.clone()),
        &Namespace::parse("app").unwrap(),
    )
    .unwrap();
    let later = (Some(head()), "2");
    assert!(
        create_on(
            &rule,
            &built(),
            stateless,
            ContainerKind::ServiceContainer,
            later
        )
        .is_ok()
    );
    assert_eq!(
        create_on(
            &approved_rule(),
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer,
            later
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Machine
        })
    );
}

/// A verified build needs no branch head: it runs a commit the branch moved past, under
/// every other check an online build passes.
#[test]
fn a_verified_build_runs_an_older_commit_and_keeps_every_other_check() {
    let offline = (None, "1");
    let older = BuildClaim {
        commit: CommitSha::parse(&"7".repeat(40)).unwrap(),
        ..built()
    };
    assert!(
        create_on(
            &approved_rule(),
            &older,
            requested_json(),
            ContainerKind::ServiceContainer,
            offline
        )
        .is_ok()
    );
    assert_eq!(
        create_on(
            &approved_rule(),
            &older,
            requested_json(),
            ContainerKind::ServiceContainer,
            (Some(head()), "1")
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Commit
        })
    );
    let next = BuildClaim {
        generation: older.generation + 1,
        ..older.clone()
    };
    assert_eq!(
        create_on(
            &approved_rule(),
            &next,
            requested_json(),
            ContainerKind::ServiceContainer,
            offline
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Generation
        })
    );
    assert_eq!(
        create_on(
            &approved_rule(),
            &older,
            requested_json(),
            ContainerKind::ServiceContainer,
            (None, "2")
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Machine
        })
    );
}

/// A preview of `rule()`, on the Server whose home is `01234567`.
fn preview(nonce: &str) -> Namespace {
    Namespace::parse(format!("pv-1111111111111111-01234567-{nonce}")).unwrap()
}

/// `requested_json()` as a preview runs it: the Parent's values, a Volume born for the
/// preview `born`, and a domain naming `named`.
fn preview_json(born: &Namespace, named: &str) -> Value {
    let birth = PreviewNamespace::of(born).unwrap().birth("data");
    let mut spec = requested_json();
    spec["volumes"] = json!([{"reference": "data", "source": {"kind": "provisioned",
        "name": "data", "maximum_bytes": 1_073_741_824_u64, "birth": birth}}]);
    spec["mounts"] = json!([{"volume": "data", "target": "/data"}]);
    spec["ports"] = json!([{"mode": "ingress", "hostname": format!("web-{named}.example.com"),
        "load_balancer_port": 443, "container_port": 80, "http_protocol": "http"}]);
    spec
}

fn preview_rule(pushers: Vec<u64>) -> AutoDeployRule {
    let namespace = preview("0000aaaa");
    let spec = requested(preview_json(&namespace, namespace.as_str()));
    let mut rule = rule();
    github_mut(&mut rule).branch = String::new();
    rule.services[0].template = preview_template(&spec, &namespace, false).unwrap();
    rule.preview = Some(PreviewTerms {
        parent: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
        pushers,
        pinned: Vec::new(),
    });
    rule
}

/// Check a creation of `spec` in `namespace` under `rule`, with the head read online.
fn create_in(
    rule: &AutoDeployRule,
    spec: Value,
    namespace: &Namespace,
    pinned: bool,
) -> Result<(), AuthorityRefusal> {
    let mut spec = requested(spec);
    spec.mount_graph = spec.mount_graph.scope_to_namespace(namespace).unwrap();
    let spec = spec
        .to_resolved(
            crate::ServiceId::parse("c".repeat(32)).unwrap(),
            crate::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    let machine = MachineId::parse("1".repeat(32)).unwrap();
    let facts = RuleFacts {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        attest: AttestKey::from_bytes([0x55; 32]),
        machine: &machine,
        namespace,
        service: &spec.name,
        head: Some(head()),
    };
    let creation = Creation {
        service: &spec.name,
        kind: ContainerKind::ServiceContainer,
        spec: &spec,
    };
    if pinned {
        rule.verify_pinned(&facts, creation)
    } else {
        // A preview rule's build runs with its cache off, so its claim digests that policy.
        let claim = BuildClaim {
            policy: PolicyDigest::of(&rule.build_policy(&spec.name).unwrap()).unwrap(),
            ..built()
        };
        rule.verify_creation(&claim, &facts, creation).map(|_| ())
    }
}

/// One approval covers every preview of the rule, each with its own Volumes and domains,
/// and nothing outside them.
#[test]
fn a_preview_rule_admits_each_of_its_previews_and_nothing_else() {
    let rule = preview_rule(Vec::new());
    let (first, second) = (preview("0000aaaa"), preview("0000bbbb"));
    let own = |namespace: &Namespace| preview_json(namespace, namespace.as_str());
    assert_eq!(create_in(&rule, own(&first), &first, false), Ok(()));
    assert_eq!(create_in(&rule, own(&second), &second, false), Ok(()));

    let other_rule = Namespace::parse("pv-2222222222222222-01234567-0000aaaa").unwrap();
    let parent = Namespace::parse("app").unwrap();
    let mut other_parent = own(&first);
    other_parent["container"]["sealed_identity"]["environment"] =
        json!("33333333-3333-3333-3333-333333333333");
    let mut signed_birth = own(&first);
    signed_birth["volumes"][0]["source"]["birth"]["nonce"] =
        json!("00112233445566778899aabbccddeeff");
    let mut edited = own(&first);
    edited["container"]["environment"]["PLAIN"] = json!("two");
    let cases: Vec<(&str, Value, &Namespace, MismatchField)> = vec![
        (
            "another rule's preview",
            own(&other_rule),
            &other_rule,
            MismatchField::Environment,
        ),
        (
            "the Parent itself",
            own(&first),
            &parent,
            MismatchField::Environment,
        ),
        (
            "another Environment's values",
            other_parent,
            &first,
            MismatchField::SealedValues,
        ),
        (
            "a signer's birth",
            signed_birth,
            &first,
            MismatchField::VolumesEnvironment,
        ),
        (
            "a sibling's Volume",
            preview_json(&second, first.as_str()),
            &first,
            MismatchField::VolumesEnvironment,
        ),
        (
            "a domain naming no preview",
            preview_json(&first, "shared"),
            &first,
            MismatchField::Hostname,
        ),
        ("an edited setting", edited, &first, MismatchField::Payload),
    ];
    for (case, spec, namespace, field) in cases {
        assert_eq!(
            create_in(&rule, spec, namespace, false),
            Err(AuthorityRefusal::Mismatch { field }),
            "{case}"
        );
    }
}

/// A rule whose template names its preview in `PLAIN` and in an environment key.
fn naming_rule(first: &Namespace) -> AutoDeployRule {
    let mut rule = preview_rule(Vec::new());
    let named = naming(
        first,
        json!({"PLAIN": first.as_str(), format!("FOR_{first}"): "one"}),
    );
    rule.services[0].template = preview_template(&requested(named), first, false).unwrap();
    rule
}

fn naming(first: &Namespace, environment: Value) -> Value {
    let mut spec = preview_json(first, first.as_str());
    spec["container"]["environment"] = environment;
    spec
}

/// A value holding the placeholder itself is not the preview's Namespace, though both
/// rewrite to the same text.
#[test]
fn a_preview_spec_holding_the_placeholder_is_refused() {
    let first = preview("0000aaaa");
    let rule = naming_rule(&first);
    let own = json!({"PLAIN": first.as_str(), format!("FOR_{first}"): "one"});
    assert_eq!(create_in(&rule, naming(&first, own), &first, false), Ok(()));

    let literal = json!({"PLAIN": PREVIEW_NAMESPACE, format!("FOR_{first}"): "one"});
    assert_eq!(
        create_in(&rule, naming(&first, literal), &first, false),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Payload
        })
    );
}

/// A key that the rewrite would rename onto one the spec already holds can't hide that
/// key's value from the approval.
#[test]
fn a_preview_spec_whose_renamed_key_meets_another_is_refused() {
    let first = preview("0000aaaa");
    let rule = naming_rule(&first);
    let smuggled = json!({
        "PLAIN": first.as_str(),
        format!("FOR_{first}"): "one",
        format!("FOR_{PREVIEW_NAMESPACE}"): "unapproved",
    });
    assert_eq!(
        create_in(&rule, naming(&first, smuggled), &first, false),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Payload
        })
    );
}

#[test]
fn a_preview_rule_builds_any_branch_pushed_by_its_pushers() {
    let feature = |actor: &str| {
        claims(|value| {
            value["ref"] = json!("refs/heads/feature");
            value["workflow_ref"] = json!(
                "Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/feature"
            );
            value["actor_id"] = json!(actor);
        })
    };
    let anyone = preview_rule(Vec::new());
    assert_eq!(
        anyone.verify_claims(&feature("1234"), &CommitAuthority::Head),
        Ok(())
    );
    assert_eq!(anyone.head_branch(&feature("1234")), "feature");
    let named = preview_rule(vec![1234, 5678]);
    assert_eq!(
        named.verify_claims(&feature("5678"), &CommitAuthority::Head),
        Ok(())
    );
    assert_eq!(
        named.verify_claims(&feature("9999"), &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimActor
        })
    );
    // Cloud dispatches an approved commit, so no pusher names the run's actor.
    let approved = CommitAuthority::Approved(Signed::sign(&approval(&named), &signer()).unwrap());
    assert_eq!(named.verify_claims(&feature("9999"), &approved), Ok(()));
    let tag = claims(|value| {
        value["ref"] = json!("refs/tags/v1");
        value["ref_type"] = json!("tag");
    });
    assert_eq!(
        anyone.verify_claims(&tag, &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimRef
        })
    );
    // A rule that isn't a preview's still builds only its own branch.
    assert_eq!(
        rule().verify_claims(&feature("1234"), &CommitAuthority::Head),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::ClaimRef
        })
    );
}

#[test]
fn a_pinned_service_runs_only_the_digest_its_template_names() {
    const PINNED: &str =
        "postgres@sha256:7777777777777777777777777777777777777777777777777777777777777777";
    let namespace = preview("0000aaaa");
    let db = |image: &str| {
        let mut spec = preview_json(&namespace, namespace.as_str());
        spec["container"]["image"] = json!(image);
        spec
    };
    let mut rule = preview_rule(Vec::new());
    rule.preview.as_mut().unwrap().pinned = vec![PinnedService {
        name: web(),
        template: preview_template(&requested(db(PINNED)), &namespace, true).unwrap(),
    }];
    assert_eq!(create_in(&rule, db(PINNED), &namespace, true), Ok(()));
    let other = PINNED.replace('7', "8");
    for (image, field) in [
        (other.as_str(), MismatchField::Payload),
        ("postgres:18", MismatchField::Image),
    ] {
        assert_eq!(
            create_in(&rule, db(image), &namespace, true),
            Err(AuthorityRefusal::Mismatch { field }),
            "{image}"
        );
    }
    rule.preview.as_mut().unwrap().pinned.clear();
    assert_eq!(
        create_in(&rule, db(PINNED), &namespace, true),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Service
        })
    );
}

/// The fixture Device's approval of a commit for the preview `0000aaaa` of `rule`.
fn approval(rule: &AutoDeployRule) -> CommitApproval {
    CommitApproval {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        repository_id: rule.source.github().unwrap().repository_id,
        commit: head(),
        namespace: preview("0000aaaa"),
        issued_at: Timestamp::from_millis(2_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from_bytes(*signer().public().as_bytes())),
    }
}

#[test]
fn only_a_person_or_a_full_device_approves_a_commit() {
    use crate::authority::{
        Cap, CapSet, Grant, GrantKind, KeyKind, KeyPlaces, KeyScope, TrustedGrant,
    };
    let rule = preview_rule(Vec::new());
    let device = TrustedKeys::new(
        vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        Vec::new(),
        [],
    );
    let check = |signed: Signed<CommitApproval>, trusted: &TrustedKeys| {
        let authority = CommitAuthority::Approved(signed);
        rule.verify_authority(
            authority.charge(&mut Budget::default(), trusted)?,
            rule.cluster,
            head(),
            Some(&preview("0000aaaa")),
        )
    };
    let signed = Signed::sign(&approval(&rule), &signer()).unwrap();
    assert!(
        signed
            .signing_input()
            .starts_with(b"ployz-authority/v1\ncommit\n")
    );
    assert_eq!(check(signed, &device), Ok(()));

    let ci = Signer::from_seed(&[2; 32]);
    let by_ci = CommitApproval {
        signer: SignerKey::Device(ci.public().into()),
        ..approval(&rule)
    };
    let trusted = TrustedKeys::new(
        [],
        [],
        [TrustedGrant {
            grant: Grant {
                key: ci.public(),
                kind: GrantKind::Ci,
                scope: KeyScope {
                    caps: CapSet::of(&[Cap::Spec]),
                    places: KeyPlaces::Only(vec![crate::authority::KeyPlace {
                        environment: uuid::Uuid::nil(),
                        namespace: crate::Namespace::parse("app").unwrap(),
                        service: None,
                    }]),
                },
                volumes: Vec::new(),
                issued_at: Timestamp::from_millis(1).unwrap(),
                name: None,
            },
            approved_by: crate::authority::PasskeyApprover::Server,
        }],
    );
    assert_eq!(
        check(Signed::sign(&by_ci, &ci).unwrap(), &trusted),
        Err(AuthorityRefusal::OutOfScope {
            code: ci.public().code(),
            kind: KeyKind::Ci,
            what: "a preview's commit",
        }),
    );
}

fn github_mut(rule: &mut AutoDeployRule) -> &mut GithubSource {
    match &mut rule.source {
        RuleSource::Github(source) => source,
        RuleSource::Registry(_) => panic!("a GitHub rule"),
    }
}

fn registry_rule() -> AutoDeployRule {
    let mut rule = approved_rule();
    rule.source = RuleSource::Registry(RegistrySource {
        host: "ghcr.io".to_owned(),
        repository: "acme/web".to_owned(),
    });
    rule.services[0].policy = None;
    rule
}

/// Check a creation of `spec` running `image` under the registry `rule` on Machine `1…`.
fn create_from_registry(
    rule: &AutoDeployRule,
    image: &str,
) -> Result<RegistryPin, AuthorityRefusal> {
    let spec = resolved(&requested(changed(&["container", "image"], json!(image))));
    let machine = MachineId::parse("1".repeat(32)).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    rule.verify_registry_creation(
        &RuleFacts {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            attest: AttestKey::from_bytes([0x55; 32]),
            machine: &machine,
            namespace: &namespace,
            service: &spec.name,
            head: None,
        },
        Creation {
            service: &spec.name,
            kind: ContainerKind::ServiceContainer,
            spec: &spec,
        },
    )
}

// Catches a registry rule running an image from a repository that only starts like the
// approved one, or from the same path on another registry.
#[test]
fn a_registry_rule_runs_only_digests_of_its_own_repository() {
    let digest = format!("sha256:{}", "4".repeat(64));
    let rule = registry_rule();
    let pin = create_from_registry(&rule, &format!("ghcr.io/acme/web@{digest}")).unwrap();
    assert_eq!(pin.reference(), format!("ghcr.io/acme/web@{digest}"));
    for lookalike in [
        "ghcr.io/acme/web-evil",
        "ghcr.io/acme/web/inner",
        "ghcr.io/acme/we",
        "docker.io/acme/web",
        "acme/web",
        "ghcr.io:443/acme/web",
    ] {
        assert_eq!(
            create_from_registry(&rule, &format!("{lookalike}@{digest}")),
            Err(AuthorityRefusal::Mismatch {
                field: MismatchField::Repository
            }),
            "{lookalike}"
        );
    }
    // A tag names whatever was pushed last, so it names nothing the Server can pin.
    assert_eq!(
        create_from_registry(&rule, "ghcr.io/acme/web:latest"),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Image
        })
    );
    // Every other setting is still the approved template.
    let mut other = rule;
    other.services[0].template = PayloadDigest::parse(&"cd".repeat(32)).unwrap();
    assert!(create_from_registry(&other, &format!("ghcr.io/acme/web@{digest}")).is_err());
}

// Catches a GitHub rule's proof standing in for a registry pull, or the reverse.
#[test]
fn each_rule_admits_only_through_its_own_source() {
    let digest = format!("sha256:{}", "4".repeat(64));
    assert_eq!(
        create_from_registry(&approved_rule(), &format!("ghcr.io/acme/web@{digest}")),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::RuleSource
        })
    );
    assert_eq!(
        create(
            &registry_rule(),
            &built(),
            requested_json(),
            ContainerKind::ServiceContainer
        ),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::RuleSource
        })
    );
}

// Catches a registry rule that carries build settings or build secrets, which no build
// under it would ever check.
#[test]
fn a_registry_rule_builds_nothing() {
    assert_eq!(registry_rule().well_formed(), Ok(()));
    let mut policy = registry_rule();
    policy.services[0].policy = Some(self::policy());
    let mut secrets = registry_rule();
    secrets.services[0].secrets = Some(SealedDigest::from_bytes([1; 32]));
    let mut unnamed = registry_rule();
    unnamed.source = RuleSource::Registry(RegistrySource {
        host: "10.0.0.5:5000".to_owned(),
        repository: "acme/web".to_owned(),
    });
    for (rule, field) in [
        (policy, MismatchField::Policy),
        (secrets, MismatchField::Policy),
        (unnamed, MismatchField::Repository),
    ] {
        assert_eq!(
            rule.well_formed(),
            Err(AuthorityRefusal::Mismatch { field }),
            "{rule:?}"
        );
    }
    let mut github = approved_rule();
    github.services[0].policy = None;
    assert!(github.well_formed().is_err());
}
