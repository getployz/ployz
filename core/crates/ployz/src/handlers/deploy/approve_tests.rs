use std::collections::BTreeMap;

use ployz_core::authority::{ApprovedChange, PayloadDigest};
use ployz_core::{Namespace, RequestedServiceSpec, ServiceName};
use serde_json::json;

use super::{Enforcing, Probe, consent, enforced, pin_tags, signable, unpinnable};
use crate::image::PinError;

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

fn approved(spec: &RequestedServiceSpec) -> ApprovedChange {
    change(spec, &BTreeMap::new())
}

fn change(spec: &RequestedServiceSpec, pins: &BTreeMap<ServiceName, String>) -> ApprovedChange {
    let [change] = ApprovedChange::of(
        std::slice::from_ref(spec),
        &Namespace::parse("app").unwrap(),
        pins,
    )
    .unwrap()
    .try_into()
    .unwrap();
    change
}

/// `spec`'s tag as this CLI pins it: the Servers run `PINNED`, the Saved State says `tag`.
fn pinned(tag: &str) -> ApprovedChange {
    let spec = spec(tag);
    change(
        &spec,
        &BTreeMap::from([(spec.name.clone(), PINNED.to_owned())]),
    )
}

#[test]
fn a_pinned_tag_prints_its_tag_and_the_digest_it_resolved_to() {
    assert_eq!(
        pinned("nginx:1.29-alpine").line(),
        "Approved web: nginx:1.29-alpine → @sha256:0123456789ab… (privileged, device \
         /dev/fuse:/dev/fuse, adds NET_ADMIN, host port 8080, binds /srv, hook migrate --up)"
    );
    let value = serde_json::to_value(pinned("nginx:1.29-alpine")).unwrap();
    assert_eq!(value.get("tag"), Some(&json!("nginx:1.29-alpine")));
    assert_eq!(value.get("image"), Some(&json!(PINNED)));
}

#[test]
fn the_approved_lines_name_the_whole_digest_what_the_host_gives_and_every_setting() {
    assert_eq!(
        approved(&spec(PINNED)).lines(),
        [
            "Approved web: nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
             (privileged, device /dev/fuse:/dev/fuse, adds NET_ADMIN, host port 8080, binds /srv, \
             hook migrate --up)",
            r#"  pre-deploy ["migrate","--up"]"#,
            r#"  mounts [{"no_copy":false,"read_only":false,"target":"/srv","volume":"root"}]"#,
            r#"  ports [{"bind":{"kind":"all"},"container_port":80,"mode":"host","published_port":8080,"transport_protocol":"tcp"}]"#,
            r#"  volumes [{"reference":"root","source":{"create_machine_path":false,"kind":"bind","machine_path":"/srv"}}]"#,
            r#"  cap_add ["NET_ADMIN"]"#,
            r#"  privileged true"#,
            r#"  pull_policy "missing""#,
            r#"  resources {"device_reservations":[],"devices":[{"cgroup_permissions":"rwm","container_path":"/dev/fuse","machine_path":"/dev/fuse"}],"ulimits":{}}"#,
        ]
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
            "settings": approved(&spec).settings,
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
fn a_private_image_fails_before_admission_asking_for_a_digest() {
    let error = unpinnable(
        &matches(),
        &spec("ghcr.io/acme/web:1.4"),
        PinError::NeedsCredentials {
            registry: "ghcr.io".to_owned(),
            reference: "ghcr.io/acme/web:1.4".to_owned(),
        },
    );
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "ghcr.io wants credentials to read web's image, so ployz can't pin it. Write its digest."
    );
    assert_eq!(
        next_hints(&error),
        ["ployz set 'web.image=ghcr.io/acme/web@sha256:…'"]
    );
}

#[test]
fn a_registry_that_cannot_be_asked_is_named_with_the_fix() {
    // Nothing listens on this port, so the lookup fails at once, as with no route out.
    let tagged = spec("127.0.0.1:1/acme/web:1.4");
    let error = pin_tags(&matches(), std::slice::from_ref(&tagged)).unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        rpc.message,
        "Couldn't ask 127.0.0.1:1 for web's image digest."
    );
    assert!(!error.causes().is_empty());
    assert_eq!(
        next_hints(&error),
        ["ployz set 'web.image=127.0.0.1:1/acme/web@sha256:…'"]
    );
}

#[test]
fn an_image_written_by_digest_is_not_looked_up() {
    assert!(pin_tags(&matches(), &[spec(PINNED)]).unwrap().is_empty());
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
    let error = signable(Some(vec![spec("ployz-build/web:pending")])).unwrap_err();
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
    let error = signable(None).unwrap_err();
    assert_eq!(error.report().code, ployz_core::RpcErrorCode::Unsupported);
}

#[test]
fn pinned_specs_are_signable() {
    assert_eq!(signable(Some(vec![spec(PINNED)])).unwrap().len(), 1);
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

/// `--yes` signs without asking, for a run nobody watches.
#[test]
fn yes_signs_without_asking() {
    let approved = [approved(&spec(PINNED))];
    consent(&approved, true, |question, _, _| {
        panic!("--yes asked {question:?}")
    })
    .unwrap();
}

/// Without a terminal, and without `--yes`, nothing is signed: the refusal names the
/// `--yes` retry.
#[test]
fn no_terminal_refuses_before_signing_and_names_yes() {
    let approved = [approved(&spec(PINNED))];
    let error = consent(&approved, false, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })
    .unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::ConfirmationRequired);
    assert!(rpc.message.contains("add --yes"), "{}", rpc.message);
    assert!(
        rpc.message.ends_with("Nothing deployed."),
        "{}",
        rpc.message
    );
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            crate::ui::Hint::Retry(retry) if retry.ends_with(" --yes")
        )),
        "{:?}",
        error.hints()
    );
}

/// A person asked names each Service, and their no fails the deploy before it signs.
#[test]
fn declining_signs_nothing_and_fails() {
    let approved = [approved(&spec(PINNED))];
    let mut asked = None;
    let error = consent(&approved, false, |question, _, declined| {
        asked = Some((question.to_owned(), declined));
        Err(crate::handlers::Error::cancelled())
    })
    .unwrap_err();
    assert_eq!(
        asked,
        Some((
            "Approve web?".to_owned(),
            "Cancelled. Nothing signed or deployed."
        ))
    );
    assert!(error.is_interrupted(), "a no exits 130, not 0");
}
