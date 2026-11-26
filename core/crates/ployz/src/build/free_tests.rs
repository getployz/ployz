use super::*;
use ployz_core::{Machine, MachineId, MachineName, MembershipObservation, WireGuardPublicKey};

/// An Up Server named `name` that advertises `capabilities`.
fn server(seed: u8, name: &str, capabilities: &[&str]) -> MachineObservation {
    MachineObservation::new(
        Machine {
            id: MachineId::parse(char::from(b'a' + seed).to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            labels: Default::default(),
            accepts_services: true,
            accepts_builds: true,
            accepts_ingress: true,
            runtime: ployz_core::MachineRuntime {
                architecture: "x86_64".into(),
                capabilities: capabilities.iter().map(|&name| name.to_owned()).collect(),
                ..Default::default()
            },
            join_approval: None,
            build_concurrency: None,
        },
        MembershipObservation::Up,
    )
}

fn web() -> Vec<ServiceName> {
    vec![ServiceName::parse("web").unwrap()]
}

fn message(error: ConnectError) -> String {
    let ConnectError::Remote(error) = error else {
        panic!("{error:?}")
    };
    assert_eq!(error.code, ployz_core::RpcErrorCode::Unsupported);
    error.message
}

/// A free Service's image runs by its receipt, which a Server from before ignores: it would
/// pull an image ID no registry has. So the Deploy stops first, naming that Server.
#[test]
fn a_free_build_names_each_server_its_service_may_run_on_that_cant_run_it() {
    let intent = super::super::tests::intent(&["web", "api"]);
    let machines = [
        server(
            1,
            "web-1",
            &[FREE_RECEIPT_CAPABILITY, BUILD_FREE_CAPABILITY],
        ),
        server(2, "web-2", &[]),
        server(3, "web-3", &[]),
    ];
    let refused = message(free_destinations(&web(), &intent, &machines).unwrap_err());
    assert!(
        refused.starts_with("web-2 and web-3 can't run web, which"),
        "{refused}"
    );
    assert!(refused.ends_with("Upgrade web-2 and web-3, then deploy again."));

    // A Server web can't be placed on is no destination.
    let mut pinned = intent.clone();
    pinned.target.get_mut(0).unwrap().placement.constraints = [
        ployz_core::PlacementConstraint::parse(format!("node.id=={}", machines[0].machine.id))
            .unwrap(),
    ]
    .into();
    free_destinations(&web(), &pinned, &machines).unwrap();
    // A Deploy that builds nothing free asks nothing of them.
    free_destinations(&[], &intent, &machines).unwrap();
}

/// Only a Server that runs Free Builds takes one. When none does, the refusal names every
/// Builder, since upgrading any one of them is enough.
#[test]
fn a_free_build_runs_only_on_a_builder_that_runs_free_builds() {
    let machines = vec![
        server(1, "web-1", &[]),
        server(2, "web-2", &[BUILD_FREE_CAPABILITY]),
    ];
    let able = free_builders(&web(), Cow::Borrowed(&machines)).unwrap();
    assert_eq!(
        able.iter()
            .map(|machine| machine.machine.name.as_str())
            .collect::<Vec<_>>(),
        ["web-2"]
    );
    assert_eq!(
        free_builders(&[], Cow::Borrowed(&machines)).unwrap().len(),
        2,
        "a Full Build keeps every Builder"
    );
    let behind = machines.get(..1).unwrap();
    let refused = message(free_builders(&web(), Cow::Borrowed(behind)).unwrap_err());
    assert!(
        refused.starts_with("web-1 can't build web, which"),
        "{refused}"
    );
}
