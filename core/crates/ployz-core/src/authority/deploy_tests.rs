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
        }],
        issued_at: Timestamp::from_millis(100_000).unwrap(),
        expires_at: Timestamp::from_millis(1_000_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from(signer.public())),
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
    let browser = signer(4);
    let passkey = Authenticator::es256(4);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: namespace(),
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
    let mut deploy = approval(&browser, &requested(requested_json()));
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
