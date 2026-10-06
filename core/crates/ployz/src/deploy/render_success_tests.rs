use super::*;
use ployz_core::DeployOutcome;
use ployz_core::{ContainerId, MachineId, ResolvedServiceSpec};

#[test]
fn success_with_ingress_deduplicates_endpoints() {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": "a".repeat(32),
        "name": "excalidraw",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "excalidraw/excalidraw:latest", "pull_policy": "missing" },
        "ports": [{
            "mode": "ingress",
            "hostname": "excalidraw.acme.ployz.app",
            "load_balancer_port": 443,
            "container_port": 80,
            "http_protocol": "https"
        }]
    }))
    .unwrap();
    spec.ports.extend(spec.ports.clone());
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let outcome = DeployOutcome::Success {
        completed: vec![
            DeployOperation::RunContainer {
                machine_id,
                spec: spec.clone(),
                skip_health_monitor: true,
            },
            DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id,
                old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
                spec,
                skip_health_monitor: false,
            }),
        ],
    };
    let DeployOutcome::<ployz_core::ExecutionError>::Success { completed } = outcome else {
        panic!("success fixture");
    };
    let text = endpoints_footer(&completed);
    assert_eq!(
        text.matches("https://excalidraw.acme.ployz.app").count(),
        1,
        "{text}"
    );
    assert!(text.contains("excalidraw → :80\n  https://excalidraw.acme.ployz.app"));
}

#[test]
fn success_groups_unique_urls_by_service_and_target_port_in_hostname_order() {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": "a".repeat(32),
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 3 },
        "container": { "image": "nginx", "pull_policy": "missing" }
    }))
    .unwrap();
    for (host, target, published, protocol) in [
        ("web.project.example", 8080, 443, "https"),
        ("z.example.com", 8080, 443, "https"),
        ("a.example.com", 8080, 443, "https"),
        ("a.example.com", 8080, 443, "https"),
        ("a.example.com", 9090, 443, "https"),
        ("a.example.com", 8080, 80, "http"),
        ("a.example.com", 8080, 8443, "https"),
    ] {
        spec.ports.push(
            serde_json::from_value(serde_json::json!({
                "mode": "ingress",
                "hostname": host,
                "load_balancer_port": published,
                "container_port": target,
                "http_protocol": protocol
            }))
            .unwrap(),
        );
    }
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let mut completed = vec![
        DeployOperation::RunContainer {
            machine_id,
            spec: spec.clone(),
            skip_health_monitor: false,
        };
        5
    ];
    completed.extend(vec![
        DeployOperation::ReplaceContainer(
            ReplacementOperation {
                machine_id,
                old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
                spec,
                skip_health_monitor: false,
            }
        );
        4
    ]);
    let text = endpoints_footer(&completed);
    assert_eq!(
        text,
        "
web → :8080
  http://a.example.com
  https://a.example.com
  https://a.example.com:8443
  https://web.project.example
  https://z.example.com

web → :9090
  https://a.example.com
"
    );
}
