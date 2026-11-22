use crate::authority::MismatchField;
use serde_json::{Value, json};

use std::collections::BTreeMap;

use super::*;
use crate::authority::{
    canonical::Timestamp,
    keys::{Approver, Purpose, Signed, Signer, SignerKey},
    proposal::ProposalRef,
    refusal::AuthorityRefusal,
    trusted_key::TrustedKeys,
    version::{self, Budget, ServiceVersion, VersionAdmitted, VersionFacts},
};
use crate::{DeviceKey, OperationId, ResolvedUpdateConfig, SealingPublicKey, ServiceId};

const IMAGE: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const OTHER: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000002";
const SEALED_A: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const SEALED_B: &str = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQg==";

fn unnamed() -> Mounts<'static> {
    Mounts::unnamed(Timestamp::from_millis(0).unwrap())
}

fn signer(seed: u8) -> Signer {
    Signer::from_seed(&[seed; 32])
}

fn namespace() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
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

fn statement(signer: &Signer, spec: &RequestedServiceSpec) -> ServiceVersion {
    ServiceVersion {
        forget_older: false,
        cluster: SealingPublicKey::from_bytes([9; 32]),
        namespace: namespace(),
        service: web(),
        digest: PayloadDigest::of(spec, &namespace()).unwrap(),
        signed_at: Timestamp::from_millis(100_000).unwrap(),
        signer: SignerKey::Device(DeviceKey::from(signer.public())),
        proposal: None,
        parent: None,
    }
}

/// What a Machine says to a creation `facts` names, presented with `signed` alone.
fn verify(
    signed: &Signed<ServiceVersion>,
    facts: &VersionFacts<'_>,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    version::verify(std::slice::from_ref(signed), facts, &mut Budget::default())
}

struct Machine {
    trusted: TrustedKeys,
    namespace: Namespace,
}

impl Machine {
    fn new(devices: &[&Signer]) -> Self {
        Self {
            trusted: TrustedKeys::new(
                devices
                    .iter()
                    .map(|signer| crate::DeviceKey::from(signer.public())),
                [],
                [],
            ),
            namespace: namespace(),
        }
    }

    fn facts<'a>(&'a self, creation: Creation<'a>) -> VersionFacts<'a> {
        VersionFacts {
            cluster: Some(SealingPublicKey::from_bytes([9; 32])),
            trusted: &self.trusted,
            namespace: &self.namespace,
            creation,
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

/// Verify a creation of `changed` against a signed version of the fixture.
fn verify_changed(changed: Value) -> Result<VersionAdmitted, AuthorityRefusal> {
    let signer = signer(1);
    let signed = Signed::sign(&statement(&signer, &requested(requested_json())), &signer).unwrap();
    let spec = resolved(&requested(changed));
    verify(&signed, &Machine::new(&[&signer]).facts(service(&spec)))
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
        PayloadDigest::of(&requested(requested_json()), &namespace()).unwrap()
    );
    assert_eq!(
        approvers(&approved),
        [Approver::Signing(signer(1).public())]
    );
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
            "extra host",
            with(&["container", "extra_hosts"], json!(["db:169.254.169.254"])),
        ),
        (
            "restart policy",
            with(&["container", "restart"], json!({"name": "always"})),
        ),
        (
            "stop timeout",
            with(&["container", "stop_timeout_secs"], json!(1)),
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
            AuthorityRefusal::Mismatch {
                field: MismatchField::Payload
            },
            "{field}"
        );
    }
}

#[test]
fn derived_fields_change_nothing() {
    let signer = signer(1);
    let base = requested(requested_json());
    let signed = Signed::sign(&statement(&signer, &base), &signer).unwrap();
    let machine = Machine::new(&[&signer]);
    let counted = requested(with(
        &["mode"],
        json!({"mode": "replicated", "replicas": 7}),
    ));
    let shown =
        |spec| ApprovedChange::of(&[spec], &namespace(), &BTreeMap::new(), &unnamed()).unwrap();
    assert_eq!(
        shown(counted.clone()),
        shown(base),
        "an approver reads only what the approval signs"
    );
    let mut spec = resolved(&counted);
    spec.service_id = ServiceId::parse("d".repeat(32)).unwrap();
    spec.update = ResolvedUpdateConfig {
        order: crate::UpdateOrder::StopFirst,
        monitor_millis: Some(5),
    };
    assert!(verify(&signed, &machine.facts(service(&spec))).is_ok());
    assert!(verify(&signed, &machine.facts(hook(&spec))).is_ok());
    let wire: ResolvedServiceSpec =
        serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
    assert!(
        verify(&signed, &machine.facts(service(&wire))).is_ok(),
        "the resolved wire keeps the scoped names the digest was taken over"
    );
}

/// Verify a creation of `changed` against a signed version of `base`.
fn verify_from(base: Value, changed: Value) -> Result<VersionAdmitted, AuthorityRefusal> {
    let signer = signer(1);
    let signed = Signed::sign(&statement(&signer, &requested(base)), &signer).unwrap();
    let spec = resolved(&requested(changed));
    verify(&signed, &Machine::new(&[&signer]).facts(service(&spec)))
}

/// `spec` with `value` written at `path`, which may be new.
fn set(mut spec: Value, path: &[&str], value: Value) -> Value {
    let mut at = &mut spec;
    for key in path {
        at = match key.parse::<usize>() {
            Ok(index) => &mut at[index],
            Err(_) => &mut at[*key],
        };
    }
    *at = value;
    spec
}

#[test]
fn cloud_changes_what_it_picks_per_run_without_a_new_signature() {
    let base = set(
        contained_json(),
        &["volumes", "0", "source"],
        json!({"kind": "provisioned", "name": "data", "maximum_bytes": 1000}),
    );
    for (what, path, value) in [
        (
            "pull policy",
            &["container", "pull_policy"][..],
            json!("never"),
        ),
        (
            "cpu",
            &["container", "resources", "cpu_nanos"],
            json!(500_000_000_u64),
        ),
        (
            "memory",
            &["container", "resources", "memory_bytes"],
            json!(1 << 30),
        ),
        (
            "memory reservation",
            &["container", "resources", "memory_reservation_bytes"],
            json!(1 << 29),
        ),
        (
            "replicas",
            &["mode"],
            json!({"mode": "replicated", "replicas": 9}),
        ),
        ("rollout order", &["update"], json!({"order": "stop_first"})),
        (
            "Volume bound",
            &["volumes", "0", "source", "maximum_bytes"],
            json!(5000),
        ),
    ] {
        assert!(
            verify_from(base.clone(), set(base.clone(), path, value)).is_ok(),
            "{what}"
        );
    }
}

#[test]
fn a_spec_that_reads_host_data_keeps_where_it_runs_signed() {
    let regional = set(
        set(
            requested_json(),
            &["placement"],
            json!({"constraints": ["node.labels.zone==a"]}),
        ),
        &["regions"],
        json!({"fsn": 2}),
    );
    assert!(
        verify_from(
            regional.clone(),
            set(regional.clone(), &["regions"], json!({"fsn": 7}))
        )
        .is_ok(),
        "a count within the signed regions moves freely"
    );
    for (what, changed) in [
        (
            "another region",
            set(regional.clone(), &["regions"], json!({"syd": 2})),
        ),
        (
            "one more region",
            set(regional.clone(), &["regions"], json!({"fsn": 2, "syd": 1})),
        ),
        ("no region", set(regional.clone(), &["regions"], json!({}))),
        (
            "another placement",
            set(
                regional.clone(),
                &["placement"],
                json!({"constraints": ["node.labels.zone==b"]}),
            ),
        ),
        (
            "no placement",
            set(regional.clone(), &["placement"], json!({})),
        ),
    ] {
        assert_eq!(
            verify_from(regional.clone(), changed).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: MismatchField::Payload
            },
            "{what}"
        );
    }
}

/// `spec` placed on the `zone` label, in the `fsn` region.
fn placed(spec: Value, zone: &str) -> Value {
    set(
        set(
            spec,
            &["placement"],
            json!({"constraints": [format!("node.labels.zone=={zone}")]}),
        ),
        &["regions"],
        json!({"fsn": 2}),
    )
}

#[test]
fn where_a_spec_runs_is_signed_unless_it_holds_nothing_per_server() {
    let provisioned = set(
        contained_json(),
        &["volumes", "0", "source"],
        json!({"kind": "provisioned", "name": "data", "maximum_bytes": 1000}),
    );
    let ordinary = set(
        contained_json(),
        &["volumes", "0", "source"],
        json!({"kind": "ordinary", "name": "data", "driver": {"name": "local"}}),
    );
    let tmpfs_named = set(
        contained_json(),
        &["volumes", "0", "source"],
        json!({"kind": "ordinary", "name": "data",
               "driver": {"name": "local", "options": {"type": "tmpfs"}}}),
    );
    let privileged = set(contained_json(), &["container", "privileged"], json!(true));
    let device = set(
        contained_json(),
        &["container", "resources"],
        json!({"devices": [{
            "machine_path": "/dev/null",
            "container_path": "/dev/null",
            "cgroup_permissions": "rwm"
        }]}),
    );
    for (what, base) in [
        ("a Provisioned Volume", provisioned),
        ("an Ordinary Volume", ordinary),
        ("a named Volume Docker backs with tmpfs", tmpfs_named),
        ("privileges", privileged),
        ("a device", device),
    ] {
        assert_eq!(
            verify_from(placed(base.clone(), "a"), placed(base, "b")).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: MismatchField::Payload
            },
            "{what}"
        );
    }

    let unused_bind = set(
        contained_json(),
        &["volumes"],
        json!([
            {"reference": "host", "source": {"kind": "bind", "machine_path": "/srv/web"}},
            {"reference": "data", "source": {"kind": "tmpfs"}}
        ]),
    );
    for (what, base) in [
        (
            "a stateless spec",
            set(
                contained_json(),
                &["volumes", "0", "source"],
                json!({"kind": "tmpfs"}),
            ),
        ),
        ("an unused Bind Mount declaration", unused_bind),
    ] {
        assert!(
            verify_from(placed(base.clone(), "a"), placed(base, "b")).is_ok(),
            "{what}"
        );
    }
}

#[test]
fn a_tag_image_is_refused_even_when_approved() {
    let signer = signer(1);
    let tagged = requested(with(&["container", "image"], json!("nginx:1.29-alpine")));
    let signed = Signed::sign(&statement(&signer, &tagged), &signer).unwrap();
    let spec = resolved(&tagged);
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Image
        }
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

/// A signed version of the fixture whose Saved State names `TAG`, pinned to `PINNED`.
fn pinned_version(signer: &Signer) -> Signed<ServiceVersion> {
    let body = statement(
        signer,
        &requested(with(&["container", "image"], json!(PINNED))),
    );
    Signed::sign(&body, signer).unwrap()
}

/// What a Machine says to a creation of the fixture running `image`.
fn pinned_creation(image: &str) -> Result<VersionAdmitted, AuthorityRefusal> {
    let signer = signer(1);
    let signed = pinned_version(&signer);
    let spec = resolved(&requested(with(&["container", "image"], json!(image))));
    verify(&signed, &Machine::new(&[&signer]).facts(service(&spec)))
}

#[test]
fn a_pinned_tag_runs_only_its_pinned_image() {
    assert!(pinned_creation(PINNED).is_ok());
    assert_eq!(
        pinned_creation(TAG).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Image
        }
    );
    for refused in [REPOINTED, IMAGE] {
        assert_eq!(
            pinned_creation(refused).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: MismatchField::Payload
            },
            "{refused}"
        );
    }
}

#[test]
fn a_pinned_change_signs_the_digest_of_the_spec_the_runner_sends() {
    let signer = signer(1);
    let tagged = requested(with(&["container", "image"], json!(TAG)));
    let changes = ApprovedChange::of(
        std::slice::from_ref(&tagged),
        &namespace(),
        &BTreeMap::from([(web(), PINNED.to_owned())]),
        &unnamed(),
    )
    .unwrap();
    let [change] = changes.as_slice() else {
        panic!("one change: {changes:?}");
    };
    assert_eq!(
        (change.tag.as_deref(), change.image.as_str()),
        (Some(TAG), PINNED)
    );
    let signed = pinned_version(&signer);
    assert_eq!(change.digest, signed.open().unwrap().digest);
    let mut sent = tagged;
    sent.container.image = change.image.clone();
    let sent = resolved(&sent);
    assert!(verify(&signed, &Machine::new(&[&signer]).facts(service(&sent))).is_ok());
}

#[test]
fn the_envelope_must_match_this_machine_and_request() {
    let signer = signer(1);
    let base = requested(requested_json());
    let spec = resolved(&base);
    let mut machine = Machine::new(&[&signer]);
    let signed = Signed::sign(&statement(&signer, &base), &signer).unwrap();

    let mut facts = machine.facts(service(&spec));
    facts.cluster = Some(SealingPublicKey::from_bytes([8; 32]));
    assert_eq!(
        verify(&signed, &facts).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Cluster
        }
    );

    let no_hook = requested({
        let mut value = requested_json();
        value.as_object_mut().unwrap().remove("pre_deploy");
        value
    });
    let hookless = Signed::sign(&statement(&signer, &no_hook), &signer).unwrap();
    let hookless_spec = resolved(&no_hook);
    assert_eq!(
        verify(&hookless, &machine.facts(hook(&hookless_spec))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Hook
        }
    );

    let other = ServiceName::parse("db").unwrap();
    let creation = Creation {
        service: &other,
        ..service(&spec)
    };
    assert_eq!(
        verify(&signed, &machine.facts(creation)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Service
        }
    );

    machine.namespace = Namespace::parse("other").unwrap();
    assert_eq!(
        verify(&signed, &machine.facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Environment
        }
    );
}

#[test]
fn a_stranger_or_a_missing_signature_is_refused() {
    let signer = signer(1);
    let base = requested(requested_json());
    let spec = resolved(&base);
    let stranger = self::signer(2);
    let signed = Signed::sign(&statement(&stranger, &base), &stranger).unwrap();
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::UnknownKey {
            key: stranger.public()
        }
    );
    let mut unsigned = Signed::sign(&statement(&signer, &base), &signer).unwrap();
    unsigned.signatures.clear();
    assert_eq!(
        verify(&unsigned, &Machine::new(&[&signer]).facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow
        }
    );
}

/// The digest of the fixture, which every implementation of [`PayloadDigest::of`] must
/// reproduce. Changing it is a wire change: a version signed before no longer verifies.
#[test]
fn the_fixture_digest_is_golden() {
    assert_eq!(
        PayloadDigest::of(&requested(requested_json()), &namespace())
            .unwrap()
            .to_string(),
        "1fe8b7a6721d43a138fd0f39e4297782933a84441b68b24db6e9ea133839b4c6"
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
            "GPU reservation",
            edit(&|spec| {
                spec["container"]["resources"] =
                    json!({"device_reservations": [{"driver": "nvidia", "count": 1}]});
            }),
        ),
        (
            "local driver binding a host path",
            edit(&|spec| {
                spec["volumes"][0]["source"]["driver"] = json!({"name": "local",
                    "options": {"type": "none", "o": "bind", "device": "/"}});
            }),
        ),
        (
            "tmpfs driver options binding a host path",
            edit(&|spec| {
                spec["volumes"][0]["source"]["driver"] = json!({"name": "local",
                    "options": {"type": "tmpfs", "o": "bind,size=1m", "device": "/"}});
            }),
        ),
        (
            "third-party driver",
            edit(&|spec| {
                spec["volumes"][0]["source"]["driver"] = json!({"name": "rexray/ebs"});
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

#[test]
fn each_reach_past_the_environment_is_named_and_a_contained_spec_names_none() {
    assert_eq!(host_reach(&requested(contained_json())), Vec::new());
    for (reach, value) in reaching_specs() {
        assert!(!host_reach(&requested(value)).is_empty(), "{reach}");
    }
}

#[test]
fn a_signed_version_that_names_a_proposal_is_refused() {
    let signer = signer(1);
    let base = requested(requested_json());
    let spec = resolved(&base);
    let mut body = statement(&signer, &base);
    body.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 0,
    });
    let signed = Signed::sign(&body, &signer).unwrap();
    assert_eq!(
        verify(&signed, &Machine::new(&[&signer]).facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal
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
) -> (ServiceVersion, Signed<crate::authority::ProposalApproval>) {
    use crate::authority::{ProposalApproval, ProposalStep, StepDigest, StepKind};
    let mut step = statement(signer, &requested(requested_json()));
    step.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 1,
    });
    let mut steps = vec![
        ProposalStep {
            index: 0,
            kind: StepKind::Version,
            digest: StepDigest::of("an earlier step"),
        },
        ProposalStep {
            index: 1,
            kind: StepKind::Version,
            digest: StepDigest::of(&canonical::to_canonical(&step).unwrap()),
        },
    ];
    listed(&mut steps);
    let bundle = ProposalApproval {
        cluster: step.cluster,
        environment: namespace(),
        operation: proposal_operation(),
        steps,
        issued_at: Timestamp::from_millis(100_000).unwrap(),
        expires_at: Timestamp::from_millis(1_000_000).unwrap(),
        signer: step.signer,
    };
    (step, Signed::sign(&bundle, signer).unwrap())
}

fn verify_step(
    step: &ServiceVersion,
    bundle: &Signed<crate::authority::ProposalApproval>,
    signer: &Signer,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    let envelope = Signed::proposed(step, bundle.clone()).unwrap();
    let spec = resolved(&requested(requested_json()));
    verify(&envelope, &Machine::new(&[signer]).facts(service(&spec)))
}

#[test]
fn a_deploy_step_whose_body_matches_its_digest_creates() {
    let signer = signer(1);
    let (step, bundle) = deploy_proposal(&signer, |_| {});
    let approved = verify_step(&step, &bundle, &signer).unwrap();
    assert_eq!(approvers(&approved), [Approver::Signing(signer.public())]);
}

#[test]
fn a_deploy_step_refuses_another_digest_index_or_kind() {
    let signer = signer(1);
    let (mut step, bundle) = deploy_proposal(&signer, |_| {});
    step.digest = PayloadDigest::parse(&"0".repeat(64)).unwrap();
    assert_eq!(
        verify_step(&step, &bundle, &signer).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Step
        }
    );

    let (mut step, bundle) = deploy_proposal(&signer, |_| {});
    step.proposal = Some(ProposalRef {
        operation: proposal_operation(),
        index: 2,
    });
    assert_eq!(
        verify_step(&step, &bundle, &signer).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Step
        }
    );
}

#[test]
fn a_deploy_step_still_checks_the_spec_it_creates() {
    let signer = signer(1);
    let (step, bundle) = deploy_proposal(&signer, |_| {});
    let envelope = Signed::proposed(&step, bundle).unwrap();
    let spec = resolved(&requested(with(&["container", "privileged"], json!(true))));
    assert_eq!(
        verify(&envelope, &Machine::new(&[&signer]).facts(service(&spec))).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Payload
        }
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

/// What the approver reads before signing `spec`, the same lines on the CLI and in a
/// browser.
fn draft_lines(spec: Value) -> Vec<String> {
    ApprovedChange::of(
        &[requested(spec)],
        &namespace(),
        &BTreeMap::new(),
        &unnamed(),
    )
    .unwrap()
    .iter()
    .flat_map(ApprovedChange::lines)
    .collect()
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
fn a_change_lists_its_settings_under_the_service_line() {
    assert_eq!(
        draft_lines(requested_json()),
        [
            "Approved web: nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001 \
             (device /dev/null:/dev/null, adds NET_ADMIN, sysctl net.core.somaxconn, host port \
             8080, binds /srv/web, hook migrate)",
            "  variables PLAIN=one, SECRET (sealed)",
            r#"  pre-deploy ["migrate"]"#,
            r#"  mounts Docker volume app_data at /data, driver {"name":"local","options":{"type":"tmpfs"}}, read-write: data from before this version of Ployz recorded where data began"#,
            r#"  configs [{"content":"hi","name":"site"}]"#,
            r#"  mounts [{"no_copy":false,"read_only":false,"target":"/srv","volume":"host"}]"#,
            r#"  ports [{"bind":{"kind":"all"},"container_port":80,"mode":"host","published_port":8080,"transport_protocol":"tcp"},{"container_port":80,"hostname":"web.example.com","http_protocol":"http","load_balancer_port":443,"mode":"ingress"}]"#,
            r#"  volumes [{"reference":"host","source":{"create_machine_path":false,"kind":"bind","machine_path":"/srv/web"}}]"#,
            r#"  cap_add ["NET_ADMIN"]"#,
            r#"  command ["nginx","-g","daemon off;"]"#,
            r#"  config_mounts [{"config_name":"site","mode":420,"target":"/etc/site.conf"}]"#,
            r#"  entrypoint ["/docker-entrypoint.sh"]"#,
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

#[test]
fn a_signed_version_is_opened_in_one_fixed_order() {
    let approver = signer(1);
    let base = requested(requested_json());
    let spec = resolved(&base);
    let host = Machine::new(&[&approver]);
    let sign = |body: &ServiceVersion| Signed::sign(body, &approver).unwrap();
    let mut body = ServiceVersion {
        forget_older: false,
        signer: SignerKey::Device(DeviceKey::from(signer(2).public())),
        proposal: Some(ProposalRef {
            operation: proposal_operation(),
            index: 0,
        }),
        ..statement(&approver, &base)
    };
    let mut facts = host.facts(service(&spec));
    facts.cluster = None;
    assert_eq!(
        verify(&sign(&body), &facts).unwrap_err(),
        AuthorityRefusal::NoClusterIdentity
    );
    facts.cluster = Some(SealingPublicKey::from_bytes([9; 32]));
    assert_eq!(
        verify(&sign(&body), &facts).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal
        }
    );
    body.proposal = None;
    body.cluster = SealingPublicKey::from_bytes([8; 32]);
    assert_eq!(
        verify(&sign(&body), &facts).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Signer
        }
    );
    body.signer = SignerKey::Device(DeviceKey::from(approver.public()));
    assert_eq!(
        verify(&sign(&body), &facts).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: MismatchField::Cluster
        }
    );
    body.cluster = SealingPublicKey::from_bytes([9; 32]);
    assert!(verify(&sign(&body), &facts).is_ok());
}

fn approvers(admitted: &crate::authority::VersionAdmitted) -> Vec<Approver> {
    admitted
        .by
        .iter()
        .map(|approved| approved.by.clone())
        .collect()
}
