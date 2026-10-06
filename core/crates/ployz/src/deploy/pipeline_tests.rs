use ployz_core::{
    MachineFailure, MachineId, MachineSuccess, PartialResult, RequestedServiceSpec, RpcError,
    RpcErrorCode,
};
use serde_json::Value;

use super::*;
use crate::deploy::{ObservationKind, observation_warnings};

#[test]
fn deploy_warning_display_is_the_cli_line_body() {
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "missing" },
        "ports": [{
            "mode": "ingress",
            "hostname": "app.example.com",
            "load_balancer_port": 443,
            "container_port": 8080,
            "http_protocol": "https"
        }]
    }))
    .unwrap();
    let warning =
        crate::dns::ingress_dns_warnings([&spec], &["192.0.2.1".parse().unwrap()], |_| {
            ployz_core::HostnameVerdict::Refused(ployz_core::Refusal::DoesNotResolve)
        })
        .into_iter()
        .map(DeployWarning::from)
        .next()
        .expect("unresolved Ingress Hostname warns");
    assert_eq!(
        warning.to_string(),
        "app.example.com does not resolve. Add a DNS record pointing at 192.0.2.1. A certificate cannot be issued until then."
    );
    assert_eq!(
        DeployWarning::ObservationOmitted {
            gap: None,
            kind: ObservationKind::Volume,
            machine_id: MachineId::parse("c".repeat(32)).unwrap(),
        }
        .to_string(),
        format!(
            "volume observation omitted {}",
            MachineId::parse("c".repeat(32)).unwrap()
        )
    );
    assert_eq!(
        DeployWarning::ObserverRelativeHostnameConflict.to_string(),
        "Hostname conflict detection is observer-relative to this Machine's current visible fan-out and does not claim uniqueness."
    );
}

#[test]
fn observation_warnings_keep_failures_and_omissions_distinct() {
    let result = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            value: (),
        }],
        failures: vec![MachineFailure {
            machine_id: MachineId::parse("b".repeat(32)).unwrap(),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "container listing failed".into(),
                details: Value::Null,
                cause: Vec::new(),
            },
        }],
        omissions: vec![MachineId::parse("c".repeat(32)).unwrap()],
    };

    assert_eq!(
        observation_warnings(
            ObservationKind::Container,
            &result.failures,
            &result.omissions,
        ),
        [
            DeployWarning::ObservationFailed {
                gap: None,
                kind: ObservationKind::Container,
                machine_id: MachineId::parse("b".repeat(32)).unwrap(),
                message: "container listing failed".into(),
            },
            DeployWarning::ObservationOmitted {
                gap: None,
                kind: ObservationKind::Container,
                machine_id: MachineId::parse("c".repeat(32)).unwrap(),
            },
        ]
    );
}
