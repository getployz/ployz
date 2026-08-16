use std::{collections::BTreeMap, net::IpAddr};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, ContainerSelector, ContainerSelectorError, FanoutSelector,
    Machine, MachineId, MachineName, MachineRuntime, MachineSelector, MachineSelectorError,
    MachineTarget, ManagementAddress, NameMatches, Placement, ServiceId, ServiceName,
    ServiceSelector, WireGuardPublicKey, derive_services, resolve_container_selector,
    resolve_machine_selector, resolve_machine_selectors, select_service,
};
use serde_json::json;

#[test]
fn machine_target_is_non_empty_and_cannot_be_a_wildcard() {
    assert_eq!(
        MachineTarget::parse("").unwrap_err().to_string(),
        "invalid Machine Target \"\": a non-empty Machine identity that is not a wildcard"
    );
    assert_eq!(
        MachineTarget::parse("*").unwrap_err().to_string(),
        "invalid Machine Target \"*\": a non-empty Machine identity that is not a wildcard"
    );
    assert_eq!(MachineTarget::parse("all").unwrap().as_str(), "all");
    assert_eq!(
        MachineTarget::parse("München edge").unwrap().as_str(),
        "München edge"
    );
}

#[test]
fn fanout_selector_treats_star_as_the_only_wildcard() {
    assert_eq!(FanoutSelector::parse("*").unwrap(), FanoutSelector::All);
    assert_eq!(
        FanoutSelector::parse("all").unwrap(),
        FanoutSelector::One(MachineTarget::parse("all").unwrap())
    );
    assert_eq!(
        FanoutSelector::parse("edge-1").unwrap(),
        FanoutSelector::One(MachineTarget::parse("edge-1").unwrap())
    );
    assert_eq!(
        FanoutSelector::parse("").unwrap_err().to_string(),
        "invalid Machine Target \"\": a non-empty Machine identity that is not a wildcard"
    );
    assert_eq!(MachineSelector::parse("*").unwrap().as_str(), "*");
    assert_eq!(MachineSelector::parse("all").unwrap().as_str(), "all");
}

#[test]
fn machine_target_resolution_prefers_ids_and_keeps_name_ambiguity() {
    let first = machine('1', "duplicate", 1);
    let second = machine('2', "duplicate", 2);
    let id_collision = machine('3', first.id.as_str(), 3);
    let visible = [first.clone(), second.clone(), id_collision];

    assert_eq!(
        MachineTarget::from(&first.id).resolve(&visible),
        NameMatches::One(&first)
    );
    assert_eq!(
        MachineTarget::parse("duplicate").unwrap().resolve(&visible),
        NameMatches::Ambiguous(vec![&first, &second])
    );
    assert_eq!(
        resolve_machine_selector(&MachineSelector::from(&first.id), &visible),
        NameMatches::One(&first)
    );
}

#[test]
fn fanout_resolution_treats_star_as_all_and_all_as_a_name() {
    let named_all = machine('1', "all", 1);
    let other = machine('2', "edge", 2);
    let visible = [named_all.clone(), other.clone()];

    assert_eq!(
        resolve_machine_selectors(&visible, &[FanoutSelector::All]).unwrap(),
        vec![named_all.clone(), other.clone()]
    );
    assert_eq!(
        resolve_machine_selectors(&visible, &[FanoutSelector::parse("all").unwrap()]).unwrap(),
        vec![named_all.clone()]
    );
    assert_eq!(
        resolve_machine_selectors(&visible, &[FanoutSelector::parse("*").unwrap()]).unwrap(),
        vec![named_all, other]
    );
    assert!(matches!(
        resolve_machine_selectors(&visible, &[FanoutSelector::parse("missing").unwrap()]),
        Err(MachineSelectorError::NotFound(missing))
            if missing == [MachineTarget::parse("missing").unwrap()]
    ));
    assert_eq!(
        resolve_machine_selectors(&[], &[FanoutSelector::All]).unwrap_err(),
        MachineSelectorError::NoVisibleMachines
    );
    assert_eq!(
        resolve_machine_selectors(&visible, &[]).unwrap_err(),
        MachineSelectorError::NoTargets
    );
}

#[test]
fn placement_accepts_only_machine_identities() {
    let placement = Placement {
        machines: vec![MachineTarget::parse("all").unwrap()],
    };
    assert_eq!(placement.machines.first().unwrap().as_str(), "all");
    assert!(Placement::default().machines.is_empty());
    assert!(serde_json::from_value::<Placement>(json!({"machines": ["*"]})).is_err());
    assert_eq!(
        serde_json::from_value::<Placement>(json!({"machines": ["all"]}))
            .unwrap()
            .machines
            .first()
            .unwrap()
            .as_str(),
        "all"
    );
    assert_eq!(
        serde_json::to_value(&placement).unwrap(),
        json!({"machines": ["all"]})
    );
}

#[test]
fn unicode_machine_text_round_trips_on_the_wire() {
    let target = MachineTarget::parse("München edge").unwrap();
    let fanout = FanoutSelector::parse("München edge").unwrap();

    assert_eq!(serde_json::to_string(&target).unwrap(), "\"München edge\"");
    assert_eq!(
        serde_json::from_str::<MachineTarget>("\"München edge\"").unwrap(),
        target
    );
    assert_eq!(serde_json::to_string(&fanout).unwrap(), "\"München edge\"");
    assert_eq!(
        serde_json::to_string(&FanoutSelector::All).unwrap(),
        "\"*\""
    );
    assert_eq!(
        serde_json::from_str::<FanoutSelector>("\"*\"").unwrap(),
        FanoutSelector::All
    );
}

#[test]
fn service_selector_resolution_prefers_ids_and_reports_ambiguous_names() {
    let first_id = ServiceId::parse("a".repeat(32)).unwrap();
    let second_id = ServiceId::parse("b".repeat(32)).unwrap();
    let collision_id = ServiceId::parse("c".repeat(32)).unwrap();
    let unique_id = ServiceId::parse("d".repeat(32)).unwrap();
    let services = derive_services([
        observation('1', &first_id, "shared", ContainerKind::ServiceContainer),
        observation('2', &second_id, "shared", ContainerKind::ServiceContainer),
        observation(
            '3',
            &collision_id,
            first_id.as_str(),
            ContainerKind::ServiceContainer,
        ),
        observation('4', &unique_id, "unique", ContainerKind::ServiceContainer),
    ]);

    assert_eq!(
        ServiceSelector::parse(first_id.as_str())
            .unwrap()
            .resolve(&services)
            .unwrap()
            .service_id,
        first_id
    );
    assert_eq!(
        select_service(&services, first_id.as_str())
            .unwrap()
            .service_id,
        first_id
    );
    assert_eq!(
        ServiceSelector::parse("unique")
            .unwrap()
            .resolve(&services)
            .unwrap()
            .service_id,
        unique_id
    );
    assert_eq!(
        serde_json::to_string(&ServiceSelector::parse("unique").unwrap()).unwrap(),
        "\"unique\""
    );
    assert!(matches!(
        ServiceSelector::parse("shared").unwrap().resolve(&services),
        Err(ployz_core::ServiceSelectorError::NameAmbiguity { service_ids, .. })
            if service_ids.len() == 2
                && service_ids.contains(&first_id)
                && service_ids.contains(&second_id)
    ));
    assert!(matches!(
        ServiceSelector::parse("missing")
            .unwrap()
            .resolve(&services),
        Err(ployz_core::ServiceSelectorError::NotFound { .. })
    ));
    assert!(ServiceSelector::parse("").is_err());
}

#[test]
fn container_selector_uses_exact_id_then_display_name_then_prefix() {
    let service_id = ServiceId::parse("1".repeat(32)).unwrap();
    let first = container(
        &"a".repeat(64),
        "api-one",
        service_id,
        "api",
        ContainerKind::ServiceContainer,
    );
    let second = container(
        &"b".repeat(64),
        "api-two",
        service_id,
        "api",
        ContainerKind::ServiceContainer,
    );
    let named_b = container(
        &format!("{}c", "b".repeat(63)),
        "b",
        service_id,
        "api",
        ContainerKind::ServiceContainer,
    );
    let containers = [&first, &second, &named_b];

    assert_eq!(
        ContainerSelector::parse("b".repeat(64))
            .unwrap()
            .resolve(containers)
            .unwrap()
            .display_name,
        "api-two"
    );
    assert_eq!(
        resolve_container_selector(containers, &ContainerSelector::parse("b").unwrap())
            .unwrap()
            .display_name,
        "b"
    );
    assert!(matches!(
        ContainerSelector::parse("bb").unwrap().resolve(containers),
        Err(ContainerSelectorError::Ambiguous { .. })
    ));
    assert_eq!(
        serde_json::to_string(&ContainerSelector::parse("api-one").unwrap()).unwrap(),
        "\"api-one\""
    );
    assert!(matches!(
        ContainerSelector::parse("missing")
            .unwrap()
            .resolve(containers),
        Err(ContainerSelectorError::NotFound { .. })
    ));
    assert!(ContainerSelector::parse("").is_err());
}

fn machine(id: char, name: &str, seed: u8) -> Machine {
    Machine {
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: Some(IpAddr::from([192, 0, 2, seed])),
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{seed}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
    }
}

fn observation(
    id: char,
    service_id: &ServiceId,
    name: &str,
    kind: ContainerKind,
) -> ContainerObservation {
    container(
        &id.to_string().repeat(64),
        &format!("{name}-{id}"),
        *service_id,
        name,
        kind,
    )
}

fn container(
    id: &str,
    display_name: &str,
    service_id: ServiceId,
    name: &str,
    kind: ContainerKind,
) -> ContainerObservation {
    let service_name = ServiceName::parse(name).unwrap();
    let resolved_spec = serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api", "pull_policy": "missing" }
    }))
    .unwrap();
    ContainerObservation {
        container_id: ContainerId::parse(id).unwrap(),
        display_name: display_name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse("2".repeat(32)).unwrap(),
        service_id,
        service_name,
        kind,
        runtime: ContainerRuntimeObservation::Created,
        effective_healthcheck: None,
        resolved_spec,
        address: None,
        labels: BTreeMap::new(),
    }
}
