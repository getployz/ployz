#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use serde_json::{Value, json};

use super::*;
use crate::authority::{
    keys::Signer,
    rule::{Builder, PolicyCache, PolicyMethod, RuleEvent, Runner},
    session::{Delegation, GrantBody, SessionGrant},
    trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
};
use crate::{DeviceKey, ResolvedServiceSpec};

const BUILDER_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILT: &str =
    "ployz-build/web@sha256:4444444444444444444444444444444444444444444444444444444444444444";
const SEALED: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const HEAD: &str = "fedcba9876543210fedcba9876543210fedcba98";
const PARENT_ID: &str = "11111111-1111-1111-1111-111111111111";
const PREVIEW_ID: &str = "33333333-3333-3333-3333-333333333333";
const GENERATED: &str = "${{ secret(16, \"a-z\") }}";

fn signer() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn machine() -> MachineId {
    MachineId::parse("1".repeat(32)).unwrap()
}

fn search() -> PreviewKey {
    PreviewKey::Branch("search".to_owned())
}

fn policy() -> BuildPolicy {
    BuildPolicy {
        method: PolicyMethod::Dockerfile {
            path: "Dockerfile".to_owned(),
        },
        context: ".".to_owned(),
        target: None,
        args: BTreeMap::new(),
        secrets: BTreeSet::new(),
        platforms: BTreeSet::from(["linux/amd64".to_owned()]),
        cache: PolicyCache::Off,
    }
}

fn unsigned_rule() -> PreviewRule {
    PreviewRule {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 2,
        machines: vec![machine()],
        parent: Namespace::parse("shop-production").unwrap(),
        parent_id: Uuid::parse_str(PARENT_ID).unwrap(),
        domain: "preview.example.com".to_owned(),
        source: RuleSource {
            repository_id: 1_450_985_696,
            owner_id: 278_111_222,
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
        trusted: vec![1234],
        services: vec![PreviewService {
            name: web(),
            template: PayloadDigest::parse(&"00".repeat(32)).unwrap(),
            setup: None,
            policy: policy(),
            secrets: None,
            inherit: BTreeSet::from(["DATABASE_URL".to_owned()]),
            generated: BTreeMap::from([(
                "GEN".to_owned(),
                Generator::parse(GENERATED).unwrap().unwrap(),
            )]),
        }],
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(DeviceKey::from_bytes(*signer().public().as_bytes())),
    }
}

/// `web` as a preview in `namespace` runs it: a derived hostname, a Live Node reference
/// into the parent's Namespace, an inherited parent value, a Generated Value, an Own Copy
/// Volume under its logical name and a hook running a Setup Command when `setup`.
fn preview_json(namespace: &str, generated: &str, setup: bool) -> Value {
    let command = if setup {
        json!([
            "/bin/sh",
            "-c",
            "for c do /bin/sh -c \"$c\" || exit; done",
            "sh",
            "migrate",
            "seed"
        ])
    } else {
        json!(["/bin/sh", "-c", "migrate"])
    };
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": BUILT,
            "pull_policy": "never",
            "environment": {
                "DB": "db.shop-production.internal",
                "GEN": generated,
                "DATABASE_URL": {"sealed": SEALED, "scope": "shared", "key": "DATABASE_URL",
                                 "environment": PARENT_ID}
            },
            "sealed_identity": {
                "organization": "org",
                "environment": PREVIEW_ID,
                "service": "22222222-2222-2222-2222-222222222222"
            }
        },
        "ports": [{"mode": "ingress", "hostname": format!("web.{namespace}.preview.example.com"),
                   "load_balancer_port": 443, "container_port": 80, "http_protocol": "http"}],
        "volumes": [{"reference": "vol-5", "source": {"kind": "ordinary", "name": "vol-5",
                     "driver": {"name": "local", "options": {}}}}],
        "mounts": [{"volume": "vol-5", "target": "/data"}],
        "pre_deploy": {"command": command}
    })
}

fn requested(value: Value) -> RequestedServiceSpec {
    serde_json::from_value(value).unwrap()
}

fn resolved(spec: &RequestedServiceSpec, namespace: &Namespace) -> ResolvedServiceSpec {
    let mut spec = spec.clone();
    spec.mount_graph = spec.mount_graph.scope_to_namespace(namespace).unwrap();
    spec.to_resolved(
        crate::ServiceId::parse("c".repeat(32)).unwrap(),
        crate::ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

/// The rule as the CLI signs it: templates computed from a sample preview.
fn rule() -> PreviewRule {
    let mut rule = unsigned_rule();
    let sample = preview_namespace(rule.rule, &PreviewKey::Branch("sample".to_owned()));
    let spec = |setup| requested(preview_json(sample.as_str(), "abcdefghijklmnop", setup));
    let service = rule.services[0].clone();
    rule.services[0].template = template(&spec(false), &sample, &service).unwrap();
    rule.services[0].setup = Some(template(&spec(true), &sample, &service).unwrap());
    rule
}

fn built(authority: CommitAuthority) -> BuildClaim {
    BuildClaim {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 2,
        service: web(),
        policy: PolicyDigest::of(&policy()).unwrap(),
        commit: CommitSha::parse(HEAD).unwrap(),
        authority,
        stage: BuildStage::Built {
            image: ImageDigest::from_bytes([0x44; 32]),
            attest: AttestKey::from_bytes([0x55; 32]),
        },
    }
}

struct Case {
    rule: PreviewRule,
    claim: BuildClaim,
    key: PreviewKey,
    namespace: Namespace,
    spec: Value,
    kind: ContainerKind,
    review: Option<CheckedReview>,
}

impl Case {
    fn trusted() -> Self {
        let namespace = preview_namespace(RuleId::from_bytes([0x11; 16]), &search());
        Self {
            rule: rule(),
            claim: built(CommitAuthority::Head),
            key: search(),
            spec: preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false),
            namespace,
            kind: ContainerKind::ServiceContainer,
            review: None,
        }
    }

    fn create(&self) -> Result<ImageDigest, AuthorityRefusal> {
        self.created().map(|created| created.image)
    }

    fn created(&self) -> Result<PreviewCreation, AuthorityRefusal> {
        let spec = resolved(&requested(self.spec.clone()), &self.namespace);
        let machine = machine();
        let facts = PreviewFacts {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            attest: AttestKey::from_bytes([0x55; 32]),
            machine: &machine,
            namespace: &self.namespace,
            service: &spec.name,
            key: &self.key,
            head: self
                .review
                .is_none()
                .then(|| CommitSha::parse(HEAD).unwrap()),
        };
        self.rule.verify_creation(
            &self.claim,
            &facts,
            Creation {
                service: &spec.name,
                kind: self.kind,
                spec: &spec,
            },
            self.review.as_ref(),
        )
    }
}

fn refused(field: &'static str) -> Result<ImageDigest, AuthorityRefusal> {
    Err(AuthorityRefusal::Mismatch { field })
}

#[test]
fn a_preview_namespace_is_derived_from_the_rule_and_key_and_reserved() {
    let rule = RuleId::from_bytes([0x11; 16]);
    let namespace = preview_namespace(rule, &search());
    assert!(namespace.as_str().starts_with("pv-1111111111111111-"));
    assert_eq!(namespace.as_str().len(), 32);
    assert!(is_preview_namespace(&namespace));
    assert_ne!(
        namespace,
        preview_namespace(rule, &PreviewKey::Branch("billing".to_owned()))
    );
    assert_ne!(namespace, preview_namespace(rule, &PreviewKey::Fork(5)));
    assert_ne!(
        namespace,
        preview_namespace(RuleId::from_bytes([0x12; 16]), &search())
    );
    for ordinary in [
        "shop-production",
        "shop-pr-5",
        "pv-1111",
        "pv-111111111111111g-aaaaaaaaaaaa",
    ] {
        assert!(
            !is_preview_namespace(&Namespace::parse(ordinary).unwrap()),
            "{ordinary}"
        );
    }
}

#[test]
fn a_preview_key_reads_back_as_written() {
    for key in [search(), PreviewKey::Fork(17)] {
        assert_eq!(PreviewKey::parse(&key.to_string()), Ok(key));
    }
    for bad in ["branch:", "fork:", "fork:x", "pr:5", "search"] {
        assert!(PreviewKey::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn two_previews_of_one_rule_share_a_template() {
    let rule = rule();
    let service = &rule.services[0];
    let digest = |key: &str, generated: &str| {
        let namespace = preview_namespace(rule.rule, &PreviewKey::Branch(key.to_owned()));
        template(
            &requested(preview_json(namespace.as_str(), generated, false)),
            &namespace,
            service,
        )
        .unwrap()
    };
    assert_eq!(digest("search", "aaaaaaaaaaaaaaaa"), service.template);
    assert_eq!(digest("billing", "qwertyuiopasdfgh"), service.template);
}

#[test]
fn a_trusted_preview_deploys_in_its_derived_namespace() {
    assert_eq!(
        Case::trusted().create(),
        Ok(ImageDigest::from_bytes([0x44; 32]))
    );
}

#[test]
fn a_preview_creation_outside_its_derived_namespace_is_refused() {
    let other_rule = preview_namespace(RuleId::from_bytes([0x12; 16]), &search());
    for namespace in [
        Namespace::parse("shop-production").unwrap(),
        Namespace::parse("ordinary").unwrap(),
        other_rule,
    ] {
        let case = Case {
            spec: preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false),
            namespace,
            ..Case::trusted()
        };
        assert_eq!(case.create(), refused("a Namespace"));
    }
}

#[test]
fn a_hostname_or_namespace_the_machine_did_not_derive_fails_the_template() {
    let mut case = Case::trusted();
    case.spec["ports"][0]["hostname"] = json!("web.acme.ployz.app");
    assert_eq!(case.create(), refused("a payload"));
    let mut case = Case::trusted();
    case.spec["container"]["environment"]["DB"] = json!("db.shop-production.internal.evil");
    assert_eq!(case.create(), refused("a payload"));
    let mut case = Case::trusted();
    case.spec["container"]["environment"]["DB"] = json!("db.shop-pr-5.internal");
    assert_eq!(case.create(), refused("a payload"));
}

#[test]
fn a_generated_value_must_be_one_its_generator_could_make() {
    for bad in [
        "short",
        "UPPERCASEUPPERCA",
        "https://evil.example",
        "abcdefghijklmnop ",
    ] {
        let mut case = Case::trusted();
        case.spec["container"]["environment"]["GEN"] = json!(bad);
        assert_eq!(case.create(), refused("a Generated Value"), "{bad}");
    }
    let mut case = Case::trusted();
    case.spec["container"]["environment"]
        .as_object_mut()
        .unwrap()
        .remove("GEN");
    assert_eq!(case.create(), refused("a Generated Value"));
}

#[test]
fn only_a_hook_of_the_setup_template_runs_the_setup_commands() {
    let created = |kind, setup| {
        let mut case = Case::trusted();
        case.spec = preview_json(case.namespace.as_str(), "zyxwvutsrqponmlk", setup);
        case.kind = kind;
        case.created().map(|created| created.runs_setup)
    };
    assert_eq!(created(ContainerKind::PreDeployHook, true), Ok(true));
    assert_eq!(created(ContainerKind::ServiceContainer, true), Ok(false));
    assert_eq!(created(ContainerKind::PreDeployHook, false), Ok(false));
}

#[test]
fn an_inherited_value_opens_only_for_a_key_the_rule_names_from_its_parent() {
    let mut case = Case::trusted();
    case.rule.services[0].inherit.clear();
    assert_eq!(case.create(), refused(SEALED_VALUE));

    let mut case = Case::trusted();
    case.spec["container"]["environment"]["DATABASE_URL"]["environment"] =
        json!("44444444-4444-4444-4444-444444444444");
    assert_eq!(case.create(), refused(SEALED_VALUE));

    let mut case = Case::trusted();
    case.spec["container"]["environment"]["STRIPE_KEY"] =
        json!({"sealed": SEALED, "scope": "shared", "key": "STRIPE_KEY", "environment": PARENT_ID});
    assert_eq!(case.create(), refused(SEALED_VALUE));
}

#[test]
fn a_trusted_preview_carries_no_sealed_value_of_its_own() {
    let mut case = Case::trusted();
    case.spec["container"]["environment"]["OWN"] =
        json!({"sealed": SEALED, "scope": "shared", "key": "OWN"});
    assert_eq!(case.create(), refused(SEALED_VALUE));
}

#[test]
fn an_inherited_value_or_a_preview_namespace_is_refused_outside_a_preview_rule() {
    let namespace = Namespace::parse("shop-production").unwrap();
    let inherited = resolved(
        &requested(preview_json("shop-production", "zyxwvutsrqponmlk", false)),
        &namespace,
    );
    assert_eq!(
        super::super::deploy::refuse_preview_parts(&namespace, &inherited),
        Err(AuthorityRefusal::Mismatch {
            field: SEALED_VALUE
        })
    );
    let mut plain = preview_json("shop-production", "zyxwvutsrqponmlk", false);
    plain["container"]["environment"]
        .as_object_mut()
        .unwrap()
        .remove("DATABASE_URL");
    let reserved = preview_namespace(RuleId::from_bytes([0x11; 16]), &search());
    assert_eq!(
        super::super::deploy::refuse_preview_parts(
            &reserved,
            &resolved(&requested(plain.clone()), &reserved)
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a preview Namespace"
        })
    );
    assert_eq!(
        super::super::deploy::refuse_preview_parts(
            &namespace,
            &resolved(&requested(plain), &namespace)
        ),
        Ok(())
    );
}

fn claims(edit: impl FnOnce(&mut Value)) -> GithubClaims {
    let mut value = json!({
        "iss": "https://token.actions.githubusercontent.com",
        "aud": "ployz-build:00",
        "jti": "4b6c2f9e-0000-0000-0000-000000000000",
        "iat": 1_800_000_000_u64,
        "exp": 1_800_000_300_u64,
        "nbf": 1_799_999_700_u64,
        "repository_id": "1450985696",
        "repository_owner_id": "278111222",
        "repository": "Ployz-test/lab-service",
        "ref": "refs/heads/search",
        "ref_type": "branch",
        "sha": HEAD,
        "event_name": "push",
        "workflow_ref": "Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/search",
        "job_workflow_ref": format!("Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"),
        "job_workflow_sha": BUILDER_SHA,
        "runner_environment": "github-hosted",
        "run_id": "42597355451",
        "run_attempt": "1",
        "actor_id": "1234"
    });
    edit(&mut value);
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_trusted_push_to_another_branch_is_a_preview_of_that_branch() {
    let rule = rule();
    let claims = claims(|_| {});
    assert_eq!(rule.verify_claims(&claims), Ok(()));
    assert_eq!(
        rule.preview_key(
            &claims,
            CommitSha::parse(HEAD).unwrap(),
            CommitAuthority::Head,
            None
        ),
        Ok(search())
    );
}

#[test]
fn an_untrusted_account_or_the_parents_branch_is_no_trusted_preview() {
    let rule = rule();
    let commit = CommitSha::parse(HEAD).unwrap();
    let untrusted = claims(|value| value["actor_id"] = json!("9999"));
    assert_eq!(
        rule.preview_key(&untrusted, commit, CommitAuthority::Head, None),
        Err(AuthorityRefusal::Mismatch {
            field: "an account"
        })
    );
    let parent = claims(|value| {
        value["ref"] = json!("refs/heads/main");
        value["workflow_ref"] =
            json!("Ployz-test/lab-service/.github/workflows/ployz-build.yml@refs/heads/main");
    });
    assert_eq!(rule.verify_claims(&parent), Ok(()));
    assert_eq!(
        rule.preview_key(&parent, commit, CommitAuthority::Head, None),
        Err(AuthorityRefusal::Mismatch { field: "a branch" })
    );
    let other_commit = CommitSha::parse(&"1".repeat(40)).unwrap();
    assert_eq!(
        rule.preview_key(&claims(|_| {}), other_commit, CommitAuthority::Head, None),
        Err(AuthorityRefusal::Mismatch { field: "a commit" })
    );
}

fn review_body(rule: &PreviewRule) -> PreviewReview {
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let spec = requested(preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false));
    PreviewReview {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        key: PreviewKey::Fork(7),
        commit: CommitSha::parse(HEAD).unwrap(),
        environment: Uuid::parse_str(PREVIEW_ID).unwrap(),
        services: vec![ReviewedService {
            name: web(),
            digest: template(&spec, &namespace, &rule.services[0]).unwrap(),
        }],
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        expires_at: Timestamp::from_millis(600_000).unwrap(),
    }
}

/// `body` signed by a browser key under a passkey's session grant for the fork preview.
fn session_review(rule: &PreviewRule, body: &PreviewReview, passkey: &Authenticator) -> Review {
    let browser = Signer::from_seed(&[4; 32]);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: rule.cluster,
        environment: preview_namespace(rule.rule, &body.key),
        environment_id: body.environment,
        machines: rule.machines.clone(),
        families: vec![ActionFamily::Deploy],
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let grant_body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&grant_body),
        ..HONEST
    });
    Review::Signed(
        Signed::sign_under(
            body,
            &browser,
            SessionGrant {
                body: grant_body,
                assertion,
            },
        )
        .unwrap(),
    )
}

fn passkey_review(body: &PreviewReview, passkey: &Authenticator) -> Review {
    let body = canonical::to_canonical(body).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: review_digest(&body).as_bytes(),
        ..HONEST
    });
    Review::Passkey { body, assertion }
}

#[test]
fn a_session_may_not_review_a_preview_that_opens_secrets() {
    let passkey = Authenticator::es256(4);
    let trusted = trusting(&passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    let rule = rule();
    let body = review_body(&rule);

    let session = session_review(&rule, &body, &passkey);
    assert_eq!(
        verify_review(&session, &rule, &trusted, now),
        Err(AuthorityRefusal::Mismatch {
            field: "the signer"
        })
    );
    let checked = verify_review(&passkey_review(&body, &passkey), &rule, &trusted, now).unwrap();
    assert_eq!(
        checked.signer,
        ReviewSigner::Passkey(passkey.passkey().code())
    );

    let mut bare = rule;
    bare.services[0].inherit.clear();
    let checked = verify_review(
        &session_review(&bare, &body, &passkey),
        &bare,
        &trusted,
        now,
    )
    .unwrap();
    assert!(matches!(checked.signer, ReviewSigner::Session(_)));

    let mut built = bare;
    built.services[0].secrets = Some(SealedDigest::from_bytes([3; 32]));
    assert_eq!(
        verify_review(
            &session_review(&built, &body, &passkey),
            &built,
            &trusted,
            now
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "the signer"
        })
    );
}

#[test]
fn a_reviewed_preview_deploys_only_the_reviewed_commit_and_settings() {
    let passkey = Authenticator::es256(4);
    let trusted = trusting(&passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    let rule = rule();
    let body = review_body(&rule);
    let review = passkey_review(&body, &passkey);
    let checked = verify_review(&review, &rule, &trusted, now).unwrap();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let case = |claim: BuildClaim, spec: Value| Case {
        rule: rule.clone(),
        claim,
        key: PreviewKey::Fork(7),
        namespace: namespace.clone(),
        spec,
        kind: ContainerKind::ServiceContainer,
        review: Some(checked.clone()),
    };
    let spec = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
    let reviewed = built(CommitAuthority::Review(review.digest()));
    assert_eq!(
        case(reviewed.clone(), spec.clone()).create(),
        Ok(ImageDigest::from_bytes([0x44; 32]))
    );

    let other_commit = BuildClaim {
        commit: CommitSha::parse(&"1".repeat(40)).unwrap(),
        ..reviewed.clone()
    };
    assert_eq!(
        case(other_commit, spec.clone()).create(),
        refused("a commit")
    );

    let other_review = built(CommitAuthority::Review(ReviewDigest::from_bytes([8; 32])));
    assert_eq!(
        case(other_review, spec.clone()).create(),
        refused("a commit")
    );

    let mut edited = spec;
    edited["container"]["environment"]["DB"] = json!("db.elsewhere.internal");
    assert_eq!(case(reviewed, edited).create(), refused("a payload"));
}

/// A session signs whatever review Cloud's page drafts, so the Container it admits reaches
/// nothing past its own Environment. A passkey's own prompt reviews the same settings.
#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn a_session_review_admits_no_host_reach() {
    let passkey = Authenticator::es256(4);
    let trusted = trusting(&passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    let mut rule = rule();
    rule.services[0].inherit.clear();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let mut spec = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
    spec["container"]["privileged"] = json!(true);
    spec["container"]["environment"]
        .as_object_mut()
        .unwrap()
        .remove("DATABASE_URL");
    let mut body = review_body(&rule);
    body.services[0].digest =
        template(&requested(spec.clone()), &namespace, &rule.services[0]).unwrap();
    let create = |review: &Review| {
        Case {
            rule: rule.clone(),
            claim: built(CommitAuthority::Review(review.digest())),
            key: PreviewKey::Fork(7),
            namespace: namespace.clone(),
            spec: spec.clone(),
            kind: ContainerKind::ServiceContainer,
            review: Some(verify_review(review, &rule, &trusted, now).unwrap()),
        }
        .create()
    };
    assert_eq!(
        create(&session_review(&rule, &body, &passkey)),
        refused(super::super::deploy::HOST_ACCESS)
    );
    assert_eq!(
        create(&passkey_review(&body, &passkey)),
        Ok(ImageDigest::from_bytes([0x44; 32]))
    );
}

#[test]
fn a_review_of_another_rule_or_generation_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusting(&passkey);
    let now = Timestamp::from_millis(2_000).unwrap();
    let rule = rule();
    let mut other = review_body(&rule);
    other.rule = RuleId::from_bytes([0x12; 16]);
    assert_eq!(
        verify_review(&passkey_review(&other, &passkey), &rule, &trusted, now),
        Err(AuthorityRefusal::Mismatch { field: "a rule" })
    );
    let mut older = review_body(&rule);
    older.generation = 1;
    assert_eq!(
        verify_review(&passkey_review(&older, &passkey), &rule, &trusted, now),
        Err(AuthorityRefusal::Mismatch {
            field: "a generation"
        })
    );
}

/// A preview rule is a rule, so D5's signer rule holds for it: a Device or a passkey's own
/// prompt signs it, and a browser key under a valid session grant never does.
#[test]
fn a_preview_rule_is_signed_by_a_device_or_a_passkeys_own_prompt_never_a_session() {
    let passkey = Authenticator::es256(4);
    let now = Timestamp::from_millis(2_000).unwrap();
    let trusted = TrustedKeys {
        devices: vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        ..trusting(&passkey)
    };
    let device = Signed::sign(&rule(), &signer()).unwrap();
    assert_eq!(
        super::super::rule::verify_signature(&device, &trusted, now).unwrap(),
        (rule(), Approver::Device(signer().public()))
    );

    let own = PreviewRule {
        signer: SignerKey::Passkey(crate::authority::restart::OwnPrompt::Passkey),
        ..rule()
    };
    let unsigned = Signed::unsigned(&own).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    let answered = unsigned.answered(assertion);
    assert_eq!(
        super::super::rule::verify_signature(&answered, &trusted, now)
            .unwrap()
            .1,
        Approver::Passkey(passkey.passkey().code())
    );

    let browser = Signer::from_seed(&[4; 32]);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: Namespace::parse("shop-production").unwrap(),
        environment_id: Uuid::parse_str(PARENT_ID).unwrap(),
        machines: vec![machine()],
        families: vec![ActionFamily::Deploy],
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let session = PreviewRule {
        signer: SignerKey::Browser {
            browser: browser.public(),
        },
        ..rule()
    };
    let signed = Signed::sign_under(&session, &browser, SessionGrant { body, assertion }).unwrap();
    assert!(
        signed.approvers(&trusted, now).is_ok(),
        "the grant itself verifies"
    );
    assert_eq!(
        super::super::rule::verify_signature(&signed, &trusted, now).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "the signer"
        }
    );
}

fn terms(rule: &PreviewRule) -> ReviewTerms {
    ReviewTerms {
        cluster: rule.cluster,
        key: PreviewKey::Fork(7),
        commit: CommitSha::parse(HEAD).unwrap(),
        environment: Uuid::parse_str(PREVIEW_ID).unwrap(),
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        expires_at: Timestamp::from_millis(600_000).unwrap(),
    }
}

/// What a browser drafts from the plan it read is what a Server admits at creation: the
/// body's digests are the template every Server takes of the same spec.
#[test]
fn a_drafted_review_admits_the_spec_it_was_drafted_from() {
    let passkey = Authenticator::es256(4);
    let trusted = trusting(&passkey);
    let rule = rule();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let spec = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
    let drafted =
        PreviewReview::draft(&rule, &namespace, &[requested(spec.clone())], terms(&rule)).unwrap();
    assert!(drafted.secrets, "web inherits a parent value");
    assert_eq!(
        drafted.lines,
        [
            "Review web at fedcba987654 (reads DATABASE_URL from the parent, hook /bin/sh -c migrate)",
            r#"  variables DATABASE_URL=(sealed "DATABASE_URL" from environment 11111111-1111-1111-1111-111111111111), DB=db.shop-production.internal, GEN="${{ secret(16, \"a-z\") }}""#,
            r#"  pre-deploy ["/bin/sh","-c","migrate"]"#,
            r#"  mounts [{"no_copy":false,"read_only":false,"target":"/data","volume":"vol-5"}]"#,
            r#"  ports [{"container_port":80,"hostname":"web.{preview-namespace}.preview.example.com","http_protocol":"http","load_balancer_port":443,"mode":"ingress"}]"#,
            r#"  volumes [{"reference":"vol-5","source":{"driver":{"name":"local","options":{}},"kind":"ordinary","labels":{},"name":"{preview-namespace}_vol-5"}}]"#,
        ]
    );
    let assertion = passkey.answer(&Answer {
        challenge: review_digest(&drafted.body).as_bytes(),
        ..HONEST
    });
    let review = Review::Passkey {
        body: drafted.body,
        assertion,
    };
    let checked = verify_review(
        &review,
        &rule,
        &trusted,
        Timestamp::from_millis(2_000).unwrap(),
    )
    .unwrap();
    let case = Case {
        rule,
        claim: built(CommitAuthority::Review(review.digest())),
        key: PreviewKey::Fork(7),
        namespace,
        spec,
        kind: ContainerKind::ServiceContainer,
        review: Some(checked),
    };
    assert_eq!(case.create(), Ok(ImageDigest::from_bytes([0x44; 32])));
}

/// A field Cloud plants in the preview's spec shows in the review before it is signed,
/// not only as "settings differ from the preview rule".
#[test]
fn a_planted_setting_shows_in_the_review() {
    let rule = rule();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let mut spec = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
    spec["container"]["command"] = json!(["sh", "-c", "curl x | sh"]);
    let drafted =
        PreviewReview::draft(&rule, &namespace, &[requested(spec)], terms(&rule)).unwrap();
    assert!(
        drafted.lines[0].contains("settings differ from the preview rule"),
        "{:?}",
        drafted.lines
    );
    assert!(
        drafted
            .lines
            .contains(&r#"  command ["sh","-c","curl x | sh"]"#.to_owned()),
        "{:?}",
        drafted.lines
    );
}

/// The template digest keeps a Config's bytes as they are, so a Config that names the
/// preview's Namespace reads apart from one that writes the placeholder itself.
#[test]
fn a_config_naming_the_namespace_reads_apart_from_the_placeholder() {
    let rule = rule();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let lines = |text: String| {
        let mut spec = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
        spec["configs"] = json!([{"name": "site", "content": text.into_bytes()}]);
        PreviewReview::draft(&rule, &namespace, &[requested(spec)], terms(&rule))
            .unwrap()
            .lines
    };
    assert_ne!(
        lines(format!("echo {}", namespace.as_str())),
        lines(format!("echo {NAMESPACE_PLACEHOLDER}"))
    );
}

#[test]
fn a_review_is_not_drafted_outside_the_preview_the_rule_derives() {
    let rule = rule();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let spec = requested(preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false));
    let elsewhere = Namespace::parse("shop-production").unwrap();
    assert!(matches!(
        PreviewReview::draft(&rule, &elsewhere, std::slice::from_ref(&spec), terms(&rule)),
        Err(ReviewDraftError::Namespace)
    ));
    let other = ReviewTerms {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        ..terms(&rule)
    };
    assert!(matches!(
        PreviewReview::draft(&rule, &namespace, std::slice::from_ref(&spec), other),
        Err(ReviewDraftError::Cluster)
    ));
    let parent = ReviewTerms {
        environment: rule.parent_id,
        ..terms(&rule)
    };
    assert!(matches!(
        PreviewReview::draft(&rule, &namespace, std::slice::from_ref(&spec), parent),
        Err(ReviewDraftError::Parent)
    ));
    assert!(matches!(
        PreviewReview::draft(
            &rule,
            &namespace,
            &[spec.clone(), spec.clone()],
            terms(&rule)
        ),
        Err(ReviewDraftError::Duplicate(_))
    ));
    let mut stray = spec;
    stray.name = ServiceName::parse("worker").unwrap();
    assert!(matches!(
        PreviewReview::draft(&rule, &namespace, &[stray], terms(&rule)),
        Err(ReviewDraftError::Uncovered(_))
    ));
}

/// The parent's values open in a preview only by the rule's `inherit`: a review naming
/// the parent as the preview's own Environment would open every other one of them.
#[test]
fn a_review_naming_the_parent_as_the_preview_is_refused() {
    let passkey = Authenticator::es256(4);
    let rule = rule();
    let body = PreviewReview {
        environment: rule.parent_id,
        ..review_body(&rule)
    };
    assert_eq!(
        verify_review(
            &passkey_review(&body, &passkey),
            &rule,
            &trusting(&passkey),
            Timestamp::from_millis(2_000).unwrap(),
        )
        .map(|_| ()),
        Err(AuthorityRefusal::Mismatch {
            field: "an Environment"
        })
    );
}

/// A review line says what the preview opens of its own, and when its settings aren't
/// the ones the rule approved or run the Setup Commands again.
#[test]
fn a_review_line_names_own_secrets_setup_and_changed_settings() {
    let rule = rule();
    let namespace = preview_namespace(rule.rule, &PreviewKey::Fork(7));
    let line = |spec: Value| {
        PreviewReview::draft(&rule, &namespace, &[requested(spec)], terms(&rule))
            .unwrap()
            .lines
            .remove(0)
    };
    let plain = line(preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false));
    assert!(
        !plain.contains("setup") && !plain.contains("differ"),
        "{plain}"
    );
    let setup = line(preview_json(namespace.as_str(), "zyxwvutsrqponmlk", true));
    assert!(setup.contains("runs setup commands"), "{setup}");
    let mut own = preview_json(namespace.as_str(), "zyxwvutsrqponmlk", false);
    own["container"]["environment"]["TOKEN"] = serde_json::json!({
        "sealed": "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==",
        "scope": "shared",
        "key": "TOKEN",
    });
    let own = line(own);
    assert!(own.contains("opens its own TOKEN"), "{own}");
    assert!(
        own.contains("settings differ from the preview rule"),
        "{own}"
    );
}

#[test]
fn a_rule_closes_only_its_own_previews_and_starts_nothing() {
    use super::super::restart::RestartStep;
    use super::super::teardown::{TeardownFacts, TeardownStep, verify_preview_close};
    let mut ended = rule();
    ended.ended = true;
    let signed = Signed::sign(&ended, &signer()).unwrap();
    let trusted = TrustedKeys {
        devices: vec![DeviceKey::from_bytes(*signer().public().as_bytes())],
        passkeys: Vec::new(),
    };
    let own = preview_namespace(ended.rule, &search());
    let other = preview_namespace(RuleId::from_bytes([0x22; 16]), &search());
    let parent = Namespace::parse("app").unwrap();
    let data = own.volume_name(&crate::DockerVolumeName::parse("data").unwrap());
    let theirs = other.volume_name(&crate::DockerVolumeName::parse("data").unwrap());
    let unnamed = MachineId::parse("9".repeat(32)).unwrap();
    let check = |step| {
        verify_preview_close(
            &signed,
            &TeardownFacts {
                cluster: Some(SealingPublicKey::from_bytes([9; 32])),
                machine: &unnamed,
                trusted: &trusted,
                step,
                now: Timestamp::from_millis(5_000).unwrap(),
            },
        )
        .map(drop)
    };
    let step = |namespace, step| TeardownStep::Container { namespace, step };
    assert_eq!(
        check(step(&own, RestartStep::Remove)),
        Ok(()),
        "an ended rule still closes"
    );
    assert_eq!(check(step(&own, RestartStep::Out)), Ok(()));
    assert_eq!(check(TeardownStep::Volume(&data)), Ok(()));
    for refused in [
        step(&own, RestartStep::Start),
        step(&other, RestartStep::Remove),
        step(&parent, RestartStep::Remove),
        TeardownStep::Volume(&theirs),
        TeardownStep::Leave,
    ] {
        assert!(check(refused).is_err(), "{refused:?}");
    }
}
