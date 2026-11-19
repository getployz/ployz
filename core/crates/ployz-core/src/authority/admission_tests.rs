use serde_json::{Value, json};

use super::*;
use crate::{
    ContainerKind, Namespace, RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig,
    ServiceId,
};

const SEALED: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

fn resolved(spec: Value) -> ResolvedServiceSpec {
    let mut spec: RequestedServiceSpec = serde_json::from_value(spec).unwrap();
    spec.mount_graph = spec
        .mount_graph
        .scope_to_namespace(&Namespace::parse("app").unwrap())
        .unwrap();
    spec.to_resolved(
        ServiceId::parse("c".repeat(32)).unwrap(),
        ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

fn free() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": "nginx", "pull_policy": "missing",
            "environment": {"PLAIN": "one"}},
        "ports": [{"mode": "ingress", "hostname": "web.example.com",
            "load_balancer_port": 443, "container_port": 80, "http_protocol": "http"}],
        "volumes": [{"reference": "scratch", "source": {"kind": "tmpfs"}}],
        "mounts": [{"volume": "scratch", "target": "/tmp"}],
        "pre_deploy": {"command": ["migrate"]}
    })
}

fn reach(spec: Value, image_login: bool) -> Option<DataReach> {
    let spec = resolved(spec);
    needs_signature(
        Creation {
            service: &spec.name,
            kind: ContainerKind::ServiceContainer,
            spec: &spec,
        },
        image_login,
    )
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn a_spec_needs_a_signature_exactly_when_it_reaches_data() {
    assert_eq!(reach(free(), false), None);
    assert_eq!(reach(free(), true), Some(DataReach::ImageLogin));

    let sealed = json!({"sealed": SEALED, "scope": "shared", "key": "SECRET"});
    let mut spec = free();
    spec["container"]["environment"]["SECRET"] = sealed.clone();
    assert_eq!(reach(spec, false), Some(DataReach::Sealed));
    let mut spec = free();
    spec["pre_deploy"]["environment"] = json!({"SECRET": sealed});
    assert_eq!(reach(spec, false), Some(DataReach::Sealed));

    for source in [
        json!({"kind": "ordinary", "name": "data", "driver": {"name": "local"}}),
        json!({"kind": "provisioned", "name": "data", "maximum_bytes": 1_073_741_824_u64}),
        json!({"kind": "external", "name": "data"}),
        json!({"kind": "bind", "machine_path": "/srv"}),
    ] {
        let mut spec = free();
        spec["volumes"][0]["source"] = source.clone();
        assert_eq!(reach(spec, false), Some(DataReach::Volume), "{source}");
    }

    let mut spec = free();
    spec["container"]["cap_add"] = json!(["NET_ADMIN"]);
    assert_eq!(reach(spec, false), Some(DataReach::Host));
    let mut spec = free();
    spec["container"]["network"] = json!("tunnel");
    assert_eq!(reach(spec, false), Some(DataReach::Host));
}
