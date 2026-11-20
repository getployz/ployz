//! Where a drain may send a Service's copy.

use ployz_core::{
    AdvertisedEndpoint, MachineObservation, MembershipObservation, ServiceId, WireGuardPublicKey,
};
use serde_json::json;

use super::*;

/// A Server that takes Services, advertising `capabilities`. None claims nothing.
fn server(hex: char, capabilities: Option<&[&str]>) -> MachineObservation {
    let runtime = ployz_core::MachineRuntime {
        capabilities: capabilities
            .unwrap_or_default()
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        ..Default::default()
    };
    MachineObservation::new(
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(format!("server-{hex}")).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime,
            join_approval: None,
            build_concurrency: None,
        },
        MembershipObservation::Up,
    )
}

fn spec(image: &str) -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": ServiceId::random(),
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "missing"},
    }))
    .unwrap()
}

fn place(image: &str, servers: Vec<MachineObservation>) -> Result<MachineId, StayReason> {
    let spec = spec(image);
    let snapshot = DeploySnapshot {
        machines: servers,
        ..Default::default()
    };
    destination(
        &spec,
        &spec.to_requested(),
        &Namespace::parse("app").unwrap(),
        &snapshot,
    )
}

const PINNED: &[&str] = &[ENSURE_PINNED_CONTAINER_CAPABILITY];
const OLDER: &[&str] = &["ployz.container.ensure.v1"];

/// A tag spec moves only to a Server that runs its copy by image ID. An older Server
/// would start whatever its tag names, so it is passed over, and when only older Servers
/// fit the drain says to upgrade them rather than move the copy.
#[test]
fn a_tag_spec_moves_only_to_a_server_that_pins_its_image() {
    let older = server('a', Some(OLDER));
    let current = server('b', Some(PINNED));
    assert_eq!(
        place("app:v1", vec![older.clone(), current.clone()]),
        Ok(current.machine.id)
    );
    assert_eq!(
        place("app:v1", vec![older.clone()]),
        Err(StayReason::NeedsUpgrade {
            servers: vec![MachineRef::from(&older.machine)]
        })
    );
    // A digest names its content, so any Server runs it.
    let digest = format!("app@sha256:{}", "7".repeat(64));
    assert_eq!(place(&digest, vec![older.clone()]), Ok(older.machine.id));
}

/// A Server whose row claims nothing may still be asked. If its daemon then turns the
/// pinned route down, the move fails as a refusal from the source, with nothing left on
/// the destination, so another Server's drain is not held up by it. When a send may
/// still have made a Container there, the failure says so and holds that drain.
#[test]
fn an_unclaimed_server_that_turns_the_pin_down_needs_upgrading() {
    let unclaimed = server('c', None);
    assert_eq!(
        place("app:v1", vec![server('a', Some(OLDER)), unclaimed.clone()]),
        Ok(unclaimed.machine.id)
    );
    let (from, to) = (
        MachineRef::from(&server('d', Some(PINNED)).machine),
        MachineRef::from(&unclaimed.machine),
    );
    let failure = MoveFailure::of(
        MoveError {
            end: MoveEnd::NeedsUpgrade,
            replacement_removed: true,
        },
        from.clone(),
        to.clone(),
    );
    assert!(failure.clean_elsewhere(&unclaimed.machine.id));
    assert!(!failure.clean_elsewhere(&from.id));
    let unsettled = MoveFailure::of(
        MoveError {
            end: MoveEnd::NeedsUpgrade,
            replacement_removed: false,
        },
        from,
        to,
    );
    assert!(!unsettled.clean_elsewhere(&unclaimed.machine.id));
    assert!(unsettled.to_string().contains("upgrade"), "{unsettled}");
}
