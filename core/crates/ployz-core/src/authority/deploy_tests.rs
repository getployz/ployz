use serde_json::{Value, json};

use super::*;
use crate::authority::{
    keys::Signer,
    session::{ActionFamily, Delegation, GrantBody, SessionGrant},
    trusted_key::{
        TrustedKeys,
        tests::{Answer, Authenticator, HONEST, trusting},
    },
};
use crate::{DeviceKey, ResolvedUpdateConfig, ServiceId};

const IMAGE: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const OTHER: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000002";
const SEALED_A: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const SEALED_B: &str = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQg==";

fn signer(seed: u8) -> Signer {
    Signer::from_seed(&[seed; 32])
}

fn namespace() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

/// The Environment the fixture's Sealed values were sealed for.
fn environment_id() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111)
}

fn machine_id(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

/// One Service holding every field kind the digest must cover.
fn requested_json() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 2},
        "container": {
            "image": IMAGE,
            "pull_policy": "missing",
            "command": ["nginx", "-g", "daemon off;"],
            "entrypoint": ["/docker-entrypoint.sh"],
            "environment": {
                "PLAIN": "one",
                "SECRET": {"sealed": SEALED_A, "scope": "shared", "key": "SECRET"}
            },
            "sealed_identity": {
                "organization": "org",
                "environment": "11111111-1111-1111-1111-111111111111",
                "service": "22222222-2222-2222-2222-222222222222"
            },
            "cap_add": ["NET_ADMIN"],
            "resources": {"devices": [{
                "machine_path": "/dev/null",
                "container_path": "/dev/null",
                "cgroup_permissions": "rwm"
            }]},
            "sysctls": {"net.core.somaxconn": "1024"},
            "config_mounts": [{"config_name": "site", "target": "/etc/site.conf", "mode": 420}]
        },
        "ports": [
            {"mode": "host", "bind": {"kind": "all"}, "published_port": 8080,
             "container_port": 80, "transport_protocol": "tcp"},
            {"mode": "ingress", "hostname": "web.example.com", "load_balancer_port": 443,
             "container_port": 80, "http_protocol": "http"}
        ],
        "volumes": [
            {"reference": "host", "source": {"kind": "bind", "machine_path": "/srv/web"}},
            {"reference": "data", "source": {"kind": "ordinary", "name": "data",
             "driver": {"name": "local", "options": {"type": "tmpfs"}}}}
        ],
        "mounts": [
            {"volume": "host", "target": "/srv"},
            {"volume": "data", "target": "/data"}
        ],
        "configs": [{"name": "site", "content": [104, 105]}],
        "pre_deploy": {"command": ["migrate"], "privileged": false},
        "update": {"order": "start_first"}
    })
}

fn requested(value: Value) -> RequestedServiceSpec {
    serde_json::from_value(value).unwrap()
}

/// The fixture with nothing that reaches past its own Environment, as a session approves
/// it: no capability, device, sysctl, host port or Bind Mount.
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn contained_json() -> Value {
    let mut spec = requested_json();
    let container = spec["container"].as_object_mut().unwrap();
    for key in ["cap_add", "resources", "sysctls"] {
        container.remove(key);
    }
    for list in ["ports", "volumes", "mounts"] {
        spec[list].as_array_mut().unwrap().remove(0);
    }
    spec
}

/// What a Machine is handed for `spec`: scoped and resolved, as the client places it.
fn resolved(spec: &RequestedServiceSpec) -> ResolvedServiceSpec {
    let mut spec = spec.clone();
    spec.mount_graph = spec.mount_graph.scope_to_namespace(&namespace()).unwrap();
    spec.to_resolved(
        ServiceId::parse("c".repeat(32)).unwrap(),
        ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

fn approval(signer: &Signer, spec: &RequestedServiceSpec) -> DeployApproval {
    DeployApproval {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
        namespace: namespace(),
        machines: vec![machine_id('1'), machine_id('2')],
        services: vec![ApprovedService {
            name: web(),
            digest: PayloadDigest::of(spec, &namespace()).unwrap(),
            image: None,
        }],
        issued_at: Timestamp::from_millis(100_000).unwrap(),
        expires_at: Timestamp::from_millis(1_000_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from(signer.public())),
        proposal: None,
    }
}

struct Machine {
    id: MachineId,
    trusted: TrustedKeys,
    namespace: Namespace,
}

impl Machine {
    fn new(devices: &[&Signer]) -> Self {
        Self {
            id: machine_id('2'),
            trusted: TrustedKeys {
                devices: devices
                    .iter()
                    .map(|signer| signer.public().into())
                    .collect(),
                passkeys: Vec::new(),
            },
            namespace: namespace(),
        }
    }

    fn facts<'a>(&'a self, creation: Option<Creation<'a>>) -> DeployFacts<'a> {
        DeployFacts {
            cluster: Some(SealingPublicKey::from_bytes([9; 32])),
            machine: &self.id,
            trusted: &self.trusted,
            namespace: &self.namespace,
            creation,
            now: Timestamp::from_millis(500_000).unwrap(),
        }
    }
}

fn service(spec: &ResolvedServiceSpec) -> Creation<'_> {
    Creation {
        service: &spec.name,
        kind: ContainerKind::ServiceContainer,
        spec,
    }
}

fn hook(spec: &ResolvedServiceSpec) -> Creation<'_> {
    Creation {
        kind: ContainerKind::PreDeployHook,
        ..service(spec)
    }
}

/// Verify a creation of `changed` against an approval of the fixture.
fn verify_changed(changed: Value) -> Result<DeployApproved, AuthorityRefusal> {
    let signer = signer(1);
    let signed = Signed::sign(&approval(&signer, &requested(requested_json())), &signer).unwrap();
    let spec = resolved(&requested(changed));
    verify(
        &signed,
        &Machine::new(&[&signer]).facts(Some(service(&spec))),
    )
}

fn with(path: &[&str], value: Value) -> Value {
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
fn the_approved_spec_verifies_with_its_digest() {
    let approved = verify_changed(requested_json()).unwrap();
    assert_eq!(
        approved.digest,
        Some(PayloadDigest::of(&requested(requested_json()), &namespace()).unwrap())
    );
    assert_eq!(approved.by, Approver::Device(signer(1).public()));
}

#[test]
fn each_payload_field_changed_alone_is_refused() {
    let cases: Vec<(&str, Value)> = vec![
        ("image digest", with(&["container", "image"], json!(OTHER))),
        ("command", with(&["container", "command", "0"], json!("sh"))),
        (
            "entrypoint",
            with(&["container", "entrypoint"], json!(["/bin/sh"])),
        ),
        (
            "plain variable",
            with(&["container", "environment", "PLAIN"], json!("two")),
        ),
        (
            "Sealed ciphertext",
            with(
                &["container", "environment", "SECRET", "sealed"],
                json!(SEALED_B),
            ),
        ),
        (
            "Sealed identity's Environment",
            with(
                &["container", "sealed_identity", "environment"],
                json!("33333333-3333-3333-3333-333333333333"),
            ),
        ),
        (
            "Bind Mount source",
            with(&["volumes", "0", "source", "machine_path"], json!("/")),
        ),
        (
            "Ordinary Volume driver option",
            with(
                &["volumes", "1", "source", "driver", "options"],
                json!({"type": "none", "o": "bind", "device": "/"}),
            ),
        ),
        (
            "Config file bytes",
            with(&["configs", "0", "content"], json!([104, 106])),
        ),
        (
            "Config Mount mode",
            with(&["container", "config_mounts", "0", "mode"], json!(511)),
        ),
        (
            "Host port",
            with(&["ports", "0", "published_port"], json!(22)),
        ),
        (
            "Ingress hostname",
            with(&["ports", "1", "hostname"], json!("other.example.com")),
        ),
        (
            "privileged",
            with(&["container", "privileged"], json!(true)),
        ),
        (
            "cap_add",
            with(&["container", "cap_add"], json!(["NET_ADMIN", "SYS_ADMIN"])),
        ),
        (
            "device mapping",
            with(
                &["container", "resources", "devices", "0", "machine_path"],
                json!("/dev/sda"),
            ),
        ),
        ("pid_mode", with(&["container", "pid_mode"], json!("host"))),
        (
            "sysctl",
            with(
                &["container", "sysctls", "net.core.somaxconn"],
                json!("4096"),
            ),
        ),
        (
            "network",
            with(&["container", "network"], json!("ingress_holder")),
        ),
        (
            "hook command",
            with(&["pre_deploy", "command"], json!(["sh", "-c", "id"])),
        ),
        (
            "hook privileged",
            with(&["pre_deploy", "privileged"], json!(true)),
        ),
    ];
    for (field, changed) in cases {
        assert_eq!(
            verify_changed(changed).unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a payload" },
            "{field}"
        );
    }
}

#[test]
fn derived_fields_change_nothing() {
    let signer = signer(1);
    let base = requested(requested_json());
    let signed = Signed::sign(&approval(&signer, &base), &signer).unwrap();
    let machine = Machine::new(&[&signer]);
    let mut spec = resolved(&requested(with(
        &["mode"],
        json!({"mode": "replicated", "replicas": 7}),
    )));
    spec.service_id = ServiceId::parse("d".repeat(32)).unwrap();
    spec.update = ResolvedUpdateConfig {
        order: crate::UpdateOrder::StopFirst,
        monitor_millis: Some(5),
    };
    assert!(verify(&signed, &machine.facts(Some(service(&spec)))).is_ok());
    assert!(verify(&signed, &machine.facts(Some(hook(&spec)))).is_ok());
    let wire: ResolvedServiceSpec =
        serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
    assert!(
        verify(&signed, &machine.facts(Some(service(&wire)))).is_ok(),
        "the resolved wire keeps the scoped names the digest was taken over"
    );
}

#[test]
fn a_tag_image_is_refused_even_when_approved() {
    let signer = signer(1);
    let tagged = requested(with(&["container", "image"], json!("nginx:1.29-alpine")));
    let signed = Signed::sign(&approval(&signer, &tagged), &signer).unwrap();
    let spec = resolved(&tagged);
    assert_eq!(
        verify(
            &signed,
            &Machine::new(&[&signer]).facts(Some(service(&spec)))
        )
        .unwrap_err(),
        AuthorityRefusal::Mismatch { field: "an image" }
    );
    for (image, pinned) in [
        (IMAGE, true),
        (
            "ghcr.io/org/app:1@sha256:00000000000000000000000000000000000000000000000000000000000000ff",
            true,
        ),
        (
            "sha256:00000000000000000000000000000000000000000000000000000000000000ff",
            true,
        ),
        ("nginx", false),
        ("nginx@sha256:abc", false),
        (
            "@sha256:00000000000000000000000000000000000000000000000000000000000000ff",
            false,
        ),
        (
            "nginx@sha256:00000000000000000000000000000000000000000000000000000000000000FF",
            false,
        ),
    ] {
        assert_eq!(content_addressed(image), pinned, "{image}");
    }
}

const TAG: &str = "nginx:1.29-alpine";
const PINNED: &str =
    "nginx:1.29-alpine@sha256:0000000000000000000000000000000000000000000000000000000000000003";
const REPOINTED: &str =
    "nginx:1.29-alpine@sha256:0000000000000000000000000000000000000000000000000000000000000004";

/// An approval of the fixture whose Saved State names `TAG`, pinned to `PINNED`.
fn pinned_approval(signer: &Signer) -> Signed<DeployApproval> {
    let mut approval = approval(
        signer,
        &requested(with(&["container", "image"], json!(PINNED))),
    );
    approval.services.first_mut().unwrap().image = Some(PINNED.to_owned());
    Signed::sign(&approval, signer).unwrap()
}

/// What a Machine says to a creation of the fixture running `image`.
fn pinned_creation(image: &str) -> Result<DeployApproved, AuthorityRefusal> {
    let signer = signer(1);
    let signed = pinned_approval(&signer);
    let spec = resolved(&requested(with(&["container", "image"], json!(image))));
    verify(
        &signed,
        &Machine::new(&[&signer]).facts(Some(service(&spec))),
    )
}

#[test]
fn a_pinned_tag_runs_only_its_pinned_image() {
    assert!(pinned_creation(PINNED).is_ok());
    for refused in [TAG, REPOINTED, IMAGE] {
        assert_eq!(
            pinned_creation(refused).unwrap_err(),
            AuthorityRefusal::Mismatch { field: "an image" },
            "{refused}"
        );
    }
}

#[test]
fn the_runner_writes_the_pinned_image_into_the_spec_the_digest_covers() {
    let signer = signer(1);
    let body = pinned_approval(&signer).open().unwrap();
    let mut spec = requested(with(&["container", "image"], json!(TAG)));
    body.pin(&mut spec);
    assert_eq!(spec.container.image, PINNED);
    assert_eq!(
        PayloadDigest::of(&spec, &namespace()).unwrap(),
        body.digest(&web()).unwrap()
    );
    let mut by_digest = requested(requested_json());
    let digest_only = DeployApproval {
        services: vec![ApprovedService {
            image: None,
            ..body.services.first().unwrap().clone()
        }],
        ..body
    };
    digest_only.pin(&mut by_digest);
    assert_eq!(by_digest.container.image, IMAGE);
}

#[test]
fn a_drafted_pin_is_in_the_body_and_its_digest_and_the_pinned_spec_creates() {
    let signer = signer(1);
    let fixture = approval(&signer, &requested(requested_json()));
    let terms = DeployTerms {
        cluster: fixture.cluster,
        operation: fixture.operation,
        machines: fixture.machines,
        issued_at: fixture.issued_at,
        expires_at: fixture.expires_at,
        signer: fixture.signer,
        proposal: None,
    };
    let tagged = requested(with(&["container", "image"], json!(TAG)));
    let (body, changes) = DeployApproval::draft(
        terms,
        namespace(),
        std::slice::from_ref(&tagged),
        &BTreeMap::from([(web(), PINNED.to_owned())]),
    )
    .unwrap();
    assert_eq!(body, pinned_approval(&signer).open().unwrap());
    let [change] = changes.as_slice() else {
        panic!("one change: {changes:?}");
    };
    assert_eq!(
        (change.tag.as_deref(), change.image.as_str()),
        (Some(TAG), PINNED)
    );
    let mut sent = tagged;
    body.pin(&mut sent);
    let sent = resolved(&sent);
    let signed = Signed::sign(&body, &signer).unwrap();
    assert!(
        verify(
            &signed,
            &Machine::new(&[&signer]).facts(Some(service(&sent)))
        )
        .is_ok()
    );
}

#[test]
fn an_approval_without_an_image_signs_the_same_bytes_as_before() {
    let signer = signer(1);
    let base = requested(requested_json());
    let body = approval(&signer, &base);
    assert!(!serde_json::to_string(&body).unwrap().contains("\"image\""));
    let signed = Signed::sign(&body, &signer).unwrap();
    assert_eq!(signed.open().unwrap(), body);
}

#[test]
fn the_envelope_must_match_this_machine_and_request() {
    let signer = signer(1);
    let base = requested(requested_json());
    let spec = resolved(&base);
    let mut machine = Machine::new(&[&signer]);
    let signed = Signed::sign(&approval(&signer, &base), &signer).unwrap();

    let mut facts = machine.facts(Some(service(&spec)));
    facts.cluster = Some(SealingPublicKey::from_bytes([8; 32]));
    assert_eq!(
        verify(&signed, &facts).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a Cluster" }
    );
    facts.now =
        Timestamp::from_millis(1_000_001 + super::super::validity::CLOCK_SKEW_MILLIS).unwrap();
    facts.cluster = Some(SealingPublicKey::from_bytes([9; 32]));
    assert_eq!(
        verify(&signed, &facts).unwrap_err(),
        AuthorityRefusal::Expired
    );

    let no_hook = requested({
        let mut value = requested_json();
        value.as_object_mut().unwrap().remove("pre_deploy");
        value
    });
    let hookless = Signed::sign(&approval(&signer, &no_hook), &signer).unwrap();
    let hookless_spec = resolved(&no_hook);
    assert_eq!(
        verify(&hookless, &machine.facts(Some(hook(&hookless_spec)))).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a hook" }
    );

    let other = ServiceName::parse("db").unwrap();
    let creation = Creation {
        service: &other,
        ..service(&spec)
    };
    assert_eq!(
        verify(&signed, &machine.facts(Some(creation))).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a Service" }
    );

    machine.namespace = Namespace::parse("other").unwrap();
    assert_eq!(
        verify(&signed, &machine.facts(None)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "an Environment"
        }
    );
    machine.namespace = namespace();
    machine.id = machine_id('3');
    assert_eq!(
        verify(&signed, &machine.facts(None)).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "Machines" }
    );
}

#[test]
fn a_lifetime_past_fifteen_minutes_is_refused() {
    let signer = signer(1);
    let base = requested(requested_json());
    let mut body = approval(&signer, &base);
    body.expires_at = body.issued_at.plus(16 * 60 * 1000);
    let signed = Signed::sign(&body, &signer).unwrap();
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(None)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "a lifetime"
        }
    );
}

#[test]
fn a_stranger_or_a_missing_signature_is_refused() {
    let signer = signer(1);
    let base = requested(requested_json());
    let stranger = self::signer(2);
    let signed = Signed::sign(&approval(&stranger, &base), &stranger).unwrap();
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(None)).unwrap_err(),
        AuthorityRefusal::UnknownKey {
            key: stranger.public()
        }
    );
    let mut unsigned = Signed::sign(&approval(&signer, &base), &signer).unwrap();
    unsigned.signatures.clear();
    assert_eq!(
        verify(&unsigned, &Machine::new(&[&signer]).facts(None)).unwrap_err(),
        AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow
        }
    );
}

fn browser_approval(families: Vec<ActionFamily>) -> (Signed<DeployApproval>, Machine) {
    browser_approval_of(families, &requested(contained_json()))
}

/// A deploy of `spec` a browser key signed under a passkey's grant of `families` for the
/// fixture's Environment, and a Machine that trusts the passkey.
fn browser_approval_of(
    families: Vec<ActionFamily>,
    spec: &RequestedServiceSpec,
) -> (Signed<DeployApproval>, Machine) {
    let browser = signer(4);
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: namespace(),
        environment_id: environment_id(),
        machines: vec![machine_id('1'), machine_id('2')],
        families,
        issued_at: Timestamp::from_millis(0).unwrap(),
        expires_at: Timestamp::from_millis(3_000_000).unwrap(),
        delegation: Delegation::None,
    };
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let mut deploy = approval(&browser, spec);
    deploy.signer = SignerKey::Browser {
        browser: browser.public(),
    };
    let signed = Signed::sign_under(&deploy, &browser, SessionGrant { body, assertion }).unwrap();
    let mut machine = Machine::new(&[]);
    machine.trusted = trusting(&passkey);
    (signed, machine)
}

#[test]
fn a_grant_must_name_the_deploy_family() {
    let (signed, machine) = browser_approval(vec![ActionFamily::Deploy]);
    assert!(verify(&signed, &machine.facts(None)).is_ok());
    let (signed, machine) = browser_approval(vec![ActionFamily::Restart]);
    assert_eq!(
        verify(&signed, &machine.facts(None)).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "an action" }
    );
}

/// Who approved a deploy under a grant for the fixture's Environment.
fn browser_approver() -> Approver {
    let (signed, machine) = browser_approval(vec![ActionFamily::Deploy]);
    verify(&signed, &machine.facts(None)).unwrap().by
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn sealed_for(environment: &str, edit: impl FnOnce(&mut Value)) -> ResolvedServiceSpec {
    let mut spec = requested_json();
    spec["container"]["sealed_identity"]["environment"] = json!(environment);
    edit(&mut spec);
    resolved(&requested(spec))
}

const OTHER_ENVIRONMENT: &str = "33333333-3333-3333-3333-333333333333";

#[test]
fn a_browser_approval_opens_only_its_own_environments_sealed_values() {
    let browser = browser_approver();
    let own = sealed_for("11111111-1111-1111-1111-111111111111", |_| {});
    assert!(sealed_scope_allows(&browser, &own));
    let other = sealed_for(OTHER_ENVIRONMENT, |_| {});
    assert!(!sealed_scope_allows(&browser, &other));
    let device = Approver::Device(signer(1).public());
    assert!(sealed_scope_allows(&device, &other), "a Device opens any");
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn a_browser_approval_of_a_spec_without_sealed_values_names_no_environment() {
    let plain = sealed_for(OTHER_ENVIRONMENT, |spec| {
        spec["container"]["environment"] = json!({"PLAIN": "one"});
        spec["container"]
            .as_object_mut()
            .unwrap()
            .remove("sealed_identity");
    });
    assert!(sealed_scope_allows(&browser_approver(), &plain));
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn a_hooks_sealed_values_for_another_environment_are_refused() {
    let hook_only = sealed_for(OTHER_ENVIRONMENT, |spec| {
        spec["container"]["environment"] = json!({"PLAIN": "one"});
        spec["pre_deploy"]["environment"] =
            json!({"SECRET": {"sealed": SEALED_B, "scope": "shared", "key": "SECRET"}});
    });
    assert!(!sealed_scope_allows(&browser_approver(), &hook_only));
}

#[test]
fn sealed_values_with_no_identity_are_refused_to_a_browser() {
    let unnamed = sealed_for(OTHER_ENVIRONMENT, |spec| {
        spec["container"]
            .as_object_mut()
            .unwrap()
            .remove("sealed_identity");
    });
    assert!(!sealed_scope_allows(&browser_approver(), &unnamed));
}

#[test]
fn unsorted_services_or_machines_do_not_parse() {
    let signer = signer(1);
    let base = requested(requested_json());
    let mut body = approval(&signer, &base);
    let first = body.services.first().cloned().unwrap();
    body.services.push(first);
    assert!(Signed::sign(&body, &signer).unwrap().open().is_err());
    let mut body = approval(&signer, &base);
    body.machines.reverse();
    assert!(Signed::sign(&body, &signer).unwrap().open().is_err());
}

/// The digest of the fixture, which every implementation of [`PayloadDigest::of`] must
/// reproduce. Changing it is a wire change: an approval made before no longer verifies.
#[test]
fn the_fixture_digest_is_golden() {
    assert_eq!(
        PayloadDigest::of(&requested(requested_json()), &namespace())
            .unwrap()
            .to_string(),
        "cb8d3c6ef656772fbe9764188fc4ff7b7059e682de60a7686263ab386cd4ffa5"
    );
}

#[test]
fn a_browser_approval_under_a_deploy_grant_creates_and_another_environment_is_refused() {
    let (signed, machine) = browser_approval(vec![ActionFamily::Deploy]);
    let spec = resolved(&requested(contained_json()));
    let approved = verify(&signed, &machine.facts(Some(service(&spec)))).unwrap();
    assert!(matches!(approved.by, Approver::Browser { .. }));

    let (mut signed, machine) = browser_approval(vec![ActionFamily::Deploy]);
    let mut grant: GrantBody =
        canonical::from_canonical(&signed.grants.first().unwrap().body).unwrap();
    grant.environment = Namespace::parse("other").unwrap();
    let passkey = Authenticator::es256(4);
    let body = canonical::to_canonical(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    signed.grants = vec![SessionGrant { body, assertion }];
    assert_eq!(
        verify(&signed, &machine.facts(Some(service(&spec)))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "an Environment"
        }
    );
}

/// Each way a spec reaches past its own Environment, set on the contained fixture.
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn reaching_specs() -> Vec<(&'static str, Value)> {
    let edit = |edit: &dyn Fn(&mut Value)| {
        let mut spec = contained_json();
        edit(&mut spec);
        spec
    };
    vec![
        (
            "privileged",
            edit(&|spec| spec["container"]["privileged"] = json!(true)),
        ),
        (
            "cap_add",
            edit(&|spec| spec["container"]["cap_add"] = json!(["SYS_ADMIN"])),
        ),
        (
            "device",
            edit(&|spec| {
                spec["container"]["resources"] = json!({"devices": [{
                    "machine_path": "/dev/sda",
                    "container_path": "/dev/sda",
                    "cgroup_permissions": "rwm"
                }]});
            }),
        ),
        (
            "pid_mode",
            edit(&|spec| spec["container"]["pid_mode"] = json!("host")),
        ),
        (
            "sysctl",
            edit(&|spec| spec["container"]["sysctls"] = json!({"net.ipv4.ip_forward": "1"})),
        ),
        (
            "host port",
            edit(&|spec| {
                spec["ports"].as_array_mut().unwrap().push(json!({
                    "mode": "host", "bind": {"kind": "all"}, "published_port": 2222,
                    "container_port": 22, "transport_protocol": "tcp"
                }));
            }),
        ),
        (
            "Bind Mount",
            edit(&|spec| {
                spec["volumes"].as_array_mut().unwrap().push(
                    json!({"reference": "root", "source": {"kind": "bind", "machine_path": "/"}}),
                );
                spec["mounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"volume": "root", "target": "/host"}));
            }),
        ),
        (
            "External Volume",
            edit(&|spec| {
                spec["volumes"].as_array_mut().unwrap().push(json!({
                    "reference": "theirs",
                    "source": {"kind": "external", "name": "other-data"}
                }));
                spec["mounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"volume": "theirs", "target": "/theirs"}));
            }),
        ),
        (
            "Ingress holder network",
            edit(&|spec| spec["container"]["network"] = json!("ingress_holder")),
        ),
        (
            "privileged hook",
            edit(&|spec| spec["pre_deploy"]["privileged"] = json!(true)),
        ),
    ]
}

/// A browser key signs whatever plan Cloud's page hands it, so the Container it approves
/// reaches nothing past its own Environment, whether made or started again. A Device
/// approves the same spec.
#[test]
fn a_session_approves_no_host_reach() {
    for (reach, value) in reaching_specs() {
        let spec = requested(value);
        let made = resolved(&spec);
        let (signed, machine) = browser_approval_of(vec![ActionFamily::Deploy], &spec);
        for creation in [service(&made), hook(&made)] {
            assert_eq!(
                verify(&signed, &machine.facts(Some(creation))).unwrap_err(),
                AuthorityRefusal::Mismatch { field: HOST_ACCESS },
                "{reach}"
            );
        }
        assert!(
            verify(&signed, &machine.facts(None)).is_ok(),
            "{reach}: a step that creates nothing names no spec"
        );
        let device = signer(1);
        let signed = Signed::sign(&approval(&device, &spec), &device).unwrap();
        let machine = Machine::new(&[&device]);
        assert!(
            verify(&signed, &machine.facts(Some(service(&made)))).is_ok(),
            "{reach}: a Device approves it"
        );
    }
}

/// The dashboard refuses before it signs, naming every Service and what each reaches.
#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn a_browser_draft_refuses_host_reach_and_names_each_service() {
    let terms = draft_terms();
    let reaching = reaching_specs();
    for (reach, value) in &reaching {
        assert!(
            matches!(
                DeployApproval::browser_draft(
                    terms.clone(),
                    namespace(),
                    &[requested(value.clone())]
                ),
                Err(BrowserDraftError::HostReach(_))
            ),
            "{reach}"
        );
    }
    let mut db = with(&["name"], json!("db"));
    db["container"]["privileged"] = json!(true);
    let refusal = DeployApproval::browser_draft(
        terms.clone(),
        namespace(),
        &[requested(requested_json()), requested(db)],
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        refusal,
        "These services reach the server itself, so only an approved device can approve them: \
         web (device /dev/null:/dev/null, adds NET_ADMIN, sysctl net.core.somaxconn, host port \
         8080, binds /srv/web), db (privileged, device /dev/null:/dev/null, adds NET_ADMIN, \
         sysctl net.core.somaxconn, host port 8080, binds /srv/web). Run ployz deploy on that \
         device."
    );
    let one =
        DeployApproval::browser_draft(terms, namespace(), &[requested(reaching[0].1.clone())])
            .unwrap_err()
            .to_string();
    assert_eq!(
        one,
        "web (privileged) reaches the server itself, so only an approved device can approve it. \
         Run ployz deploy on that device."
    );
}

#[test]
fn a_signed_approval_that_names_a_proposal_is_refused() {
    let signer = signer(1);
    let mut body = approval(&signer, &requested(requested_json()));
    body.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 0,
    });
    let signed = Signed::sign(&body, &signer).unwrap();
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(None)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "a proposal"
        }
    );
}

fn proposal_operation() -> OperationId {
    OperationId::parse("e".repeat(32)).unwrap()
}

/// A proposal whose step 1 deploys the fixture and whose step 0 restarts a Container.
/// Returns the deploy step's body and the bundle, signed by `signer`.
fn deploy_proposal(
    signer: &Signer,
    listed: impl FnOnce(&mut Vec<crate::authority::ProposalStep>),
) -> (DeployApproval, Signed<crate::authority::ProposalApproval>) {
    use crate::authority::{ProposalApproval, ProposalStep, StepDigest, StepKind};
    let mut step = approval(signer, &requested(requested_json()));
    step.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 1,
    });
    let mut steps = vec![
        ProposalStep {
            index: 0,
            kind: StepKind::Restart,
            digest: StepDigest::of("a restart step"),
        },
        ProposalStep {
            index: 1,
            kind: StepKind::Deploy,
            digest: StepDigest::of(&canonical::to_canonical(&step).unwrap()),
        },
    ];
    listed(&mut steps);
    let bundle = ProposalApproval {
        cluster: step.cluster,
        environment: namespace(),
        operation: proposal_operation(),
        steps,
        issued_at: step.issued_at,
        expires_at: step.expires_at,
        signer: step.signer,
    };
    (step, Signed::sign(&bundle, signer).unwrap())
}

fn verify_step(
    step: &DeployApproval,
    bundle: &Signed<crate::authority::ProposalApproval>,
    signer: &Signer,
) -> Result<DeployApproved, AuthorityRefusal> {
    let envelope = Signed::proposed(step, bundle.clone()).unwrap();
    let spec = resolved(&requested(requested_json()));
    verify(
        &envelope,
        &Machine::new(&[signer]).facts(Some(service(&spec))),
    )
}

#[test]
fn a_deploy_step_whose_body_matches_its_digest_creates() {
    let signer = signer(1);
    let (step, bundle) = deploy_proposal(&signer, |_| {});
    let approved = verify_step(&step, &bundle, &signer).unwrap();
    assert_eq!(approved.body, step);
    assert_eq!(approved.by, Approver::Device(signer.public()));
}

#[test]
fn a_deploy_step_refuses_another_digest_index_or_kind() {
    use crate::authority::StepKind;
    let signer = signer(1);
    let (mut step, bundle) = deploy_proposal(&signer, |_| {});
    step.services.first_mut().unwrap().digest = PayloadDigest::parse(&"0".repeat(64)).unwrap();
    assert_eq!(
        verify_step(&step, &bundle, &signer).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a step" }
    );

    let (mut step, bundle) = deploy_proposal(&signer, |_| {});
    step.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 2,
    });
    assert_eq!(
        verify_step(&step, &bundle, &signer).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a step" }
    );

    let (step, bundle) = deploy_proposal(&signer, |steps| {
        steps.get_mut(1).unwrap().kind = StepKind::Restart;
    });
    assert_eq!(
        verify_step(&step, &bundle, &signer).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a step" }
    );
}

#[test]
fn a_deploy_step_still_checks_the_spec_it_creates() {
    let signer = signer(1);
    let (step, bundle) = deploy_proposal(&signer, |_| {});
    let envelope = Signed::proposed(&step, bundle).unwrap();
    let spec = resolved(&requested(with(&["container", "privileged"], json!(true))));
    assert_eq!(
        verify(
            &envelope,
            &Machine::new(&[&signer]).facts(Some(service(&spec)))
        )
        .unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a payload" }
    );
}

#[test]
fn a_deploy_step_in_a_bundle_no_trusted_key_signed_is_refused() {
    let stranger = signer(2);
    let (step, bundle) = deploy_proposal(&stranger, |_| {});
    assert_eq!(
        verify_step(&step, &bundle, &signer(1)).unwrap_err(),
        AuthorityRefusal::UnknownKey {
            key: stranger.public()
        }
    );
}

#[test]
fn a_browser_draft_names_the_golden_digest_and_refuses_a_build_or_a_tag() {
    let terms = draft_terms();
    let draft =
        DeployApproval::browser_draft(terms.clone(), namespace(), &[requested(contained_json())])
            .unwrap();
    let body: DeployApproval = canonical::from_canonical(&draft.body).unwrap();
    assert_eq!(
        body.digest(&web())
            .map(|digest| digest.to_string())
            .as_deref(),
        Some(CONTAINED_GOLDEN)
    );
    assert_eq!(draft.lines, draft.changes.first().unwrap().lines());

    let built = requested(with(
        &["container", "image"],
        json!(format!("ployz-build/{IMAGE}")),
    ));
    assert!(matches!(
        DeployApproval::browser_draft(terms.clone(), namespace(), &[built]),
        Err(BrowserDraftError::Built(_))
    ));
    let tagged = requested(with(&["container", "image"], json!(TAG)));
    assert!(matches!(
        DeployApproval::browser_draft(terms.clone(), namespace(), std::slice::from_ref(&tagged)),
        Err(BrowserDraftError::Tag(_))
    ));

    // Every tagged Service is named at once, so a person fixes them in one pass.
    let mut db = with(&["container", "image"], json!("postgres:18"));
    if let Some(name) = db.get_mut("name") {
        *name = json!("db");
    }
    let refusal = DeployApproval::browser_draft(terms, namespace(), &[tagged, requested(db)])
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("web's nginx:1.29-alpine, db's postgres:18"),
        "{refusal}"
    );
}

/// The digest of [`contained_json`], which the browser's draft must reproduce.
const CONTAINED_GOLDEN: &str = "b38204e1d5b8a6d94b145b97d8dcbff4b50f5218b1cd9969324a1108aed6541b";

/// The fixture approval's terms, as a client sets them around a plan.
fn draft_terms() -> DeployTerms {
    let fixture = approval(&signer(1), &requested(requested_json()));
    DeployTerms {
        cluster: fixture.cluster,
        operation: fixture.operation,
        machines: fixture.machines,
        issued_at: fixture.issued_at,
        expires_at: fixture.expires_at,
        signer: fixture.signer,
        proposal: None,
    }
}

/// What the approver reads before signing `spec`, the same lines on the CLI and in a
/// browser.
fn draft_lines(spec: Value) -> Vec<String> {
    let (_, changes) = DeployApproval::draft(
        draft_terms(),
        namespace(),
        &[requested(spec)],
        &BTreeMap::new(),
    )
    .unwrap();
    changes.iter().flat_map(ApprovedChange::lines).collect()
}

/// Each field Cloud could plant in the plan it relays changes what the approver reads
/// before signing, since the digest covers it. A Sealed ciphertext shows by name only, and
/// its identity not at all: neither is a value a person can read.
#[test]
fn every_field_cloud_could_plant_shows_before_signing() {
    let base = draft_lines(requested_json());
    let cases: Vec<(&str, Value)> = vec![
        ("image digest", with(&["container", "image"], json!(OTHER))),
        ("command", with(&["container", "command", "0"], json!("sh"))),
        (
            "entrypoint",
            with(&["container", "entrypoint"], json!(["/bin/sh"])),
        ),
        ("user", with(&["container", "user"], json!("root"))),
        (
            "working directory",
            with(&["container", "working_directory"], json!("/tmp")),
        ),
        (
            "plain variable",
            with(&["container", "environment", "PLAIN"], json!("two")),
        ),
        (
            "new variable",
            with(&["container", "environment", "LD_PRELOAD"], json!("/x.so")),
        ),
        (
            "label",
            with(&["container", "labels"], json!({"traefik.enable": "true"})),
        ),
        ("hostname", with(&["container", "hostname"], json!("db"))),
        ("cap_drop", with(&["container", "cap_drop"], json!(["ALL"]))),
        ("init", with(&["container", "init"], json!(true))),
        ("tty", with(&["container", "tty"], json!(true))),
        (
            "restart",
            with(&["container", "restart"], json!({"name": "always"})),
        ),
        (
            "pull policy",
            with(&["container", "pull_policy"], json!("always")),
        ),
        (
            "Bind Mount source",
            with(&["volumes", "0", "source", "machine_path"], json!("/")),
        ),
        (
            "Ordinary Volume driver option",
            with(
                &["volumes", "1", "source", "driver", "options"],
                json!({"type": "none", "o": "bind", "device": "/"}),
            ),
        ),
        (
            "mount target",
            with(&["mounts", "1", "target"], json!("/etc")),
        ),
        (
            "Config file bytes",
            with(&["configs", "0", "content"], json!([104, 106])),
        ),
        (
            "Config Mount mode",
            with(&["container", "config_mounts", "0", "mode"], json!(511)),
        ),
        (
            "Host port",
            with(&["ports", "0", "published_port"], json!(22)),
        ),
        (
            "Ingress hostname",
            with(&["ports", "1", "hostname"], json!("other.example.com")),
        ),
        (
            "privileged",
            with(&["container", "privileged"], json!(true)),
        ),
        (
            "cap_add",
            with(&["container", "cap_add"], json!(["NET_ADMIN", "SYS_ADMIN"])),
        ),
        (
            "device mapping",
            with(
                &["container", "resources", "devices", "0", "machine_path"],
                json!("/dev/sda"),
            ),
        ),
        ("pid_mode", with(&["container", "pid_mode"], json!("host"))),
        (
            "sysctl",
            with(
                &["container", "sysctls", "net.core.somaxconn"],
                json!("4096"),
            ),
        ),
        (
            "network",
            with(&["container", "network"], json!("ingress_holder")),
        ),
        (
            "hook command",
            with(&["pre_deploy", "command"], json!(["sh", "-c", "id"])),
        ),
        (
            "hook privileged",
            with(&["pre_deploy", "privileged"], json!(true)),
        ),
        ("hook user", with(&["pre_deploy", "user"], json!("root"))),
        (
            "hook variable",
            with(&["pre_deploy", "environment"], json!({"TOKEN": "x"})),
        ),
    ];
    for (field, changed) in cases {
        let lines = draft_lines(changed);
        assert_ne!(lines, base, "{field}: {lines:#?}");
    }
}

#[test]
fn an_approval_lists_its_settings_under_the_service_line() {
    assert_eq!(
        draft_lines(requested_json()),
        [
            "Approved web: nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001 \
             (device /dev/null:/dev/null, adds NET_ADMIN, sysctl net.core.somaxconn, host port \
             8080, binds /srv/web, hook migrate)",
            "  variables PLAIN=one, SECRET (sealed)",
            r#"  pre-deploy ["migrate"]"#,
            r#"  configs [{"content":"hi","name":"site"}]"#,
            r#"  mounts [{"no_copy":false,"read_only":false,"target":"/srv","volume":"host"},{"no_copy":false,"read_only":false,"target":"/data","volume":"data"}]"#,
            r#"  ports [{"bind":{"kind":"all"},"container_port":80,"mode":"host","published_port":8080,"transport_protocol":"tcp"},{"container_port":80,"hostname":"web.example.com","http_protocol":"http","load_balancer_port":443,"mode":"ingress"}]"#,
            r#"  volumes [{"reference":"host","source":{"create_machine_path":false,"kind":"bind","machine_path":"/srv/web"}},{"reference":"data","source":{"driver":{"name":"local","options":{"type":"tmpfs"}},"kind":"ordinary","labels":{},"name":"data"}}]"#,
            r#"  cap_add ["NET_ADMIN"]"#,
            r#"  command ["nginx","-g","daemon off;"]"#,
            r#"  config_mounts [{"config_name":"site","mode":420,"target":"/etc/site.conf"}]"#,
            r#"  entrypoint ["/docker-entrypoint.sh"]"#,
            r#"  pull_policy "missing""#,
            r#"  resources {"device_reservations":[],"devices":[{"cgroup_permissions":"rwm","container_path":"/dev/null","machine_path":"/dev/null"}],"ulimits":{}}"#,
            r#"  sysctls {"net.core.somaxconn":"1024"}"#,
            "  pre-deploy privileged false",
        ]
    );
}

/// A value that would move the cursor, recolour or reorder text shows as its escape, so
/// it can't redraw the line that names the command.
#[test]
fn a_planted_escape_shows_as_text() {
    let lines = draft_lines(with(
        &["container", "environment", "PLAIN"],
        json!("one\u{1b}[1A\u{1b}[2K\u{9b}2K\u{202e}"),
    ));
    assert!(
        lines.contains(
            &r#"  variables PLAIN="one\u{1b}[1A\u{1b}[2K\u{9b}2K\u{202e}", SECRET (sealed)"#
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line.chars().all(|c| matches!(c, ' '..='~' | '→' | '…'))),
        "{lines:#?}"
    );
}
