use ployz_core::authority::PayloadDigest;
use ployz_core::{Namespace, RequestedServiceSpec};
use serde_json::json;

use super::{Approved, Enforcing, Probe, enforced, host_sensitive, line, signable, unpinned};

const PINNED: &str =
    "nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn spec(image: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": image,
            "pull_policy": "missing",
            "privileged": true,
            "cap_add": ["NET_ADMIN"],
            "resources": {"devices": [{"machine_path": "/dev/fuse", "container_path": "/dev/fuse",
                                       "cgroup_permissions": "rwm"}]},
        },
        "ports": [{"mode": "host", "bind": {"kind": "all"}, "published_port": 8080,
                   "container_port": 80, "transport_protocol": "tcp"}],
        "volumes": [{"reference": "root", "source": {"kind": "bind", "machine_path": "/srv"}}],
        "mounts": [{"volume": "root", "target": "/srv"}],
        "pre_deploy": {"command": ["migrate", "--up"]},
    }))
    .unwrap()
}

fn approved(spec: &RequestedServiceSpec) -> Approved {
    Approved {
        service: spec.name.clone(),
        image: spec.container.image.clone(),
        digest: PayloadDigest::of(spec, &Namespace::parse("app").unwrap()).unwrap(),
        host: host_sensitive(spec),
    }
}

#[test]
fn the_approved_line_names_the_image_digest_and_what_the_host_gives() {
    assert_eq!(
        line(&approved(&spec(PINNED))),
        "Approved web: nginx@sha256:0123456789ab… (privileged, device /dev/fuse:/dev/fuse, adds \
         NET_ADMIN, host port 8080, binds /srv, hook migrate --up)"
    );
}

#[test]
fn json_carries_what_was_approved() {
    let spec = spec(PINNED);
    let value = serde_json::to_value(approved(&spec)).unwrap();
    let digest = PayloadDigest::of(&spec, &Namespace::parse("app").unwrap()).unwrap();
    assert_eq!(
        value,
        json!({
            "service": "web",
            "image": PINNED,
            "digest": digest.to_string(),
            "host": ["privileged", "device /dev/fuse:/dev/fuse", "adds NET_ADMIN", "host port 8080", "binds /srv",
                     "hook migrate --up"],
        })
    );
}

fn matches() -> clap::ArgMatches {
    crate::handlers::deploy::deploy_command(clap::Command::new("deploy"))
        .try_get_matches_from(["deploy"])
        .unwrap()
}

fn next_hints(error: &crate::handlers::Error) -> Vec<String> {
    error
        .hints()
        .iter()
        .filter_map(|hint| {
            if let crate::ui::Hint::Next(next) = hint {
                Some(next.clone())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_tag_image_fails_before_admission_naming_the_fix_for_its_repository() {
    let error = unpinned(&matches(), &spec("ghcr.io/acme/web:1.4"));
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "Pin web's image by digest to deploy to servers that enforce device approval."
    );
    assert_eq!(
        next_hints(&error),
        ["ployz set 'web.image=ghcr.io/acme/web@sha256:…'"]
    );
}

#[test]
fn a_registry_port_is_not_a_tag() {
    assert_eq!(super::repository("registry:5000/web"), "registry:5000/web");
    assert_eq!(
        super::repository("registry:5000/web:2"),
        "registry:5000/web"
    );
    assert_eq!(super::repository(PINNED), "nginx");
}

#[test]
fn a_built_service_is_refused_plainly_with_no_pin_hint() {
    let error = signable(&matches(), Some(vec![spec("ployz-build/web:pending")])).unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "web builds from source, and servers that enforce device approval can't run a build \
         yet. Deploy it from an image pinned by digest."
    );
    assert!(next_hints(&error).is_empty(), "{:?}", error.hints());
}

#[test]
fn a_store_that_lowers_no_target_is_refused() {
    let error = signable(&matches(), None).unwrap_err();
    assert_eq!(error.report().code, ployz_core::RpcErrorCode::Unsupported);
}

#[test]
fn pinned_specs_are_signable() {
    assert_eq!(
        signable(&matches(), Some(vec![spec(PINNED)]))
            .unwrap()
            .len(),
        1
    );
}

fn server(seed: u8, enforces: Option<bool>, key: Option<u8>) -> ployz_core::MachineObservation {
    let mut runtime = serde_json::Map::new();
    if let Some(enforces) = enforces {
        runtime.insert("device_enforcement".into(), json!(enforces));
    }
    if let Some(key) = key {
        runtime.insert(
            "sealing".into(),
            json!({"kind": "held", "public_key": base64(&[key; 32])}),
        );
    }
    serde_json::from_value(json!({
        "machine": {
            "id": format!("{seed:02x}").repeat(16),
            "name": format!("machine-{seed}"),
            "subnet": format!("10.210.{seed}.0/24"),
            "public_key": vec![seed; 32],
            "labels": {},
            "accepts_builds": true,
            "accepts_services": true,
            "accepts_ingress": true,
            "advertised_endpoints": [],
            "runtime": runtime,
        },
        "membership": "up",
    }))
    .unwrap()
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn servers_that_do_not_enforce_leave_the_deploy_unsigned() {
    assert_eq!(
        enforced(&[server(1, None, Some(9)), server(2, Some(false), Some(9))]),
        Probe::Off
    );
}

#[test]
fn an_enforcing_cluster_with_no_reported_key_is_named() {
    assert_eq!(
        enforced(&[server(1, Some(true), None)]),
        Probe::NoClusterKey
    );
}

#[test]
fn an_enforcing_cluster_signs_for_every_server_listed() {
    let Probe::Enforcing(Enforcing { cluster, machines }) =
        enforced(&[server(2, Some(true), None), server(1, Some(false), Some(9))])
    else {
        panic!("expected enforcing");
    };
    assert_eq!(cluster, ployz_core::SealingPublicKey::from_bytes([9; 32]));
    assert_eq!(machines.len(), 2);
    assert!(machines.is_sorted());
}
