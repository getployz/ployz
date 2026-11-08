use ployz_core::authority::PayloadDigest;
use ployz_core::{Namespace, RequestedServiceSpec};
use serde_json::json;

use super::{Approved, host_sensitive, line, unpinned};

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
        "Approved web: nginx@sha256:0123456789ab… (privileged, adds NET_ADMIN, host port \
         8080, binds /srv, hook migrate --up)"
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
            "host": ["privileged", "adds NET_ADMIN", "host port 8080", "binds /srv",
                     "hook migrate --up"],
        })
    );
}

#[test]
fn a_tag_image_fails_before_admission_naming_the_fix() {
    let matches = crate::handlers::deploy::deploy_command(clap::Command::new("deploy"))
        .try_get_matches_from(["deploy"])
        .unwrap();
    let error = unpinned(&matches, &spec("nginx:1.29-alpine"));
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "Pin web's image by digest to deploy to Servers that enforce."
    );
    assert!(
        error
            .hints()
            .iter()
            .any(|hint| matches!(hint, crate::ui::Hint::Next(next) if next == "ployz service set 'web.image=…'")),
        "{:?}",
        error.hints()
    );
}
