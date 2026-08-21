//! Global catch-up: place observed eligible Globals onto this Machine only.

use ployz_core::{
    BridgeEndpointCapacity, ContainerRuntimeObservation, EnsureGlobalSlotRequest, InspectRequest,
    LiveServices, Machine, MachineId, MachineTarget, QualifiedService, ResolvedServiceSpec,
    RpcError, ServiceContainer, ServiceMode, ServiceObservation, machine_matches_placement, op,
};

use crate::{
    connect::{Client, ConnectError},
    deploy::endpoint_capacity_error,
    failure::Failure,
};

/// One Global slot this Machine still needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalCatchUpSlot {
    pub identity: QualifiedService,
    pub spec: ResolvedServiceSpec,
}

/// Catch-up failed. Caddy failures fail the membership command.
#[derive(Debug)]
pub enum CatchUpError {
    /// Eligible Caddy could not be observed, placed, or confirmed running.
    Caddy(Failure),
    /// Listing or a non-Caddy Global failed hard.
    Other(Failure),
}

impl From<CatchUpError> for Failure {
    fn from(error: CatchUpError) -> Self {
        match error {
            CatchUpError::Caddy(failure) | CatchUpError::Other(failure) => failure,
        }
    }
}

impl From<ConnectError> for CatchUpError {
    fn from(error: ConnectError) -> Self {
        Self::Other(error.into())
    }
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, CatchUpError>;
    async fn bridge_capacity(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, CatchUpError>;
    async fn ensure_global_slot(
        &mut self,
        machine_id: &MachineId,
        request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, CatchUpError> {
        Client::live_services(self).await.map_err(Into::into)
    }

    async fn bridge_capacity(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, CatchUpError> {
        let details = self
            .invoke::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                &MachineTarget::from(machine_id),
                None,
            )
            .await
            .map_err(|error| CatchUpError::Other(error.into()))?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }

    async fn ensure_global_slot(
        &mut self,
        machine_id: &MachineId,
        request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError> {
        self.call::<op::EnsureGlobalSlot>(request, Some(&MachineTarget::from(machine_id)))
            .await
            .map(|_| ())
            .map_err(Into::into)
    }
}

pub(crate) fn joined_catch_up_error(error: CatchUpError) -> String {
    let recovery = match error {
        CatchUpError::Caddy(error) => {
            format!(" {error}. Run `ployz caddy deploy` to complete catch-up.")
        }
        CatchUpError::Other(error) => format!(" {error}"),
    };
    format!(
        "Machine joined, but Global catch-up failed afterward; it remains a Cluster member.{recovery}"
    )
}

/// Globals this Machine is eligible for and does not already run.
#[must_use]
pub fn plan_global_catch_up(
    services: &[ServiceObservation],
    this_machine: &Machine,
    skip_caddy: bool,
) -> Vec<GlobalCatchUpSlot> {
    services
        .iter()
        .filter_map(|service| slot_for(service, this_machine, skip_caddy))
        .collect()
}

fn slot_for(
    service: &ServiceObservation,
    this_machine: &Machine,
    skip_caddy: bool,
) -> Option<GlobalCatchUpSlot> {
    if skip_caddy && service.identity == QualifiedService::system_caddy() {
        return None;
    }
    let newest = newest_service_container(service)?;
    let spec = &newest.as_observation().resolved_spec;
    if spec.mode != ServiceMode::Global {
        return None;
    }
    if !machine_matches_placement(this_machine, &spec.placement) {
        return None;
    }
    if service.containers.iter().any(|container| {
        let observation = container.as_observation();
        observation.machine_id == this_machine.id
            && observation.service_id == spec.service_id
            && is_running(&observation.runtime)
    }) {
        return None;
    }
    Some(GlobalCatchUpSlot {
        identity: service.identity.clone(),
        spec: spec.clone(),
    })
}

fn newest_service_container(service: &ServiceObservation) -> Option<&ServiceContainer> {
    service.containers.iter().max_by_key(|container| {
        let observation = container.as_observation();
        (
            observation.created_at_unix_nanos,
            observation.container_id.as_str(),
        )
    })
}

fn is_running(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(runtime, ContainerRuntimeObservation::Running { .. })
}

fn caddy_expected(
    services: &[ServiceObservation],
    this_machine: &Machine,
    skip_caddy: bool,
    slots: &[GlobalCatchUpSlot],
) -> bool {
    !skip_caddy
        && (slots
            .iter()
            .any(|slot| slot.identity == QualifiedService::system_caddy())
            || caddy_running_on(services, this_machine))
}

fn caddy_running_on(services: &[ServiceObservation], this_machine: &Machine) -> bool {
    services.iter().any(|service| {
        service.identity == QualifiedService::system_caddy()
            && service.containers.iter().any(|container| {
                let observation = container.as_observation();
                observation.machine_id == this_machine.id && is_running(&observation.runtime)
            })
    })
}

fn partial_observation_warning(live: &LiveServices<RpcError>) -> Option<String> {
    (!live.containers.all_targets_succeeded())
        .then(|| crate::failure::partial_failure_details(&live.containers))
}

/// Copy every observed eligible Global onto `this_machine` only.
///
/// # Errors
///
/// Fails when listing Services fails, when expected Caddy cannot be placed, or
/// when Caddy was expected and is still not running after catch-up. Other
/// Global failures are warnings.
pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    skip_caddy: bool,
) -> Result<(), CatchUpError> {
    let live = client.live_services().await?;
    if let Some(warning) = partial_observation_warning(&live) {
        eprintln!("WARNING: Global catch-up used partial Service observations: {warning}");
    }
    let services = live.services();
    let slots = plan_global_catch_up(&services, this_machine, skip_caddy);
    let endpoint_creates = slots
        .iter()
        .filter(|slot| !service_has_slot(&services, this_machine, &slot.spec.service_id))
        .count();
    if endpoint_creates > 0 {
        let capacity = client.bridge_capacity(&this_machine.id).await?;
        if let Some(error) = endpoint_capacity_error(endpoint_creates, capacity.as_ref()) {
            return Err(CatchUpError::Other(Failure::usage(error.to_string())));
        }
    }
    let expect_caddy = caddy_expected(&services, this_machine, skip_caddy, &slots);
    if !slots.is_empty() {
        eprintln!("Placing Global Services on this Machine.");
    }
    for slot in slots {
        let identity = slot.identity.clone();
        match client
            .ensure_global_slot(
                &this_machine.id,
                EnsureGlobalSlotRequest {
                    project_name: slot.identity.project,
                    resolved_spec: slot.spec,
                },
            )
            .await
        {
            Ok(_) => {}
            Err(error) if identity == QualifiedService::system_caddy() => {
                return Err(CatchUpError::Caddy(Failure::usage(format!(
                    "Caddy is not running on this Machine: {error}"
                ))));
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    Failure::warned(format!("Global catch-up failed for {identity}"), error)
                );
            }
        }
    }
    if expect_caddy {
        let live = client.live_services().await?;
        if !caddy_running_on(&live.services(), this_machine) {
            return Err(CatchUpError::Caddy(Failure::usage(
                "Caddy is not running on this Machine".to_owned(),
            )));
        }
    }
    Ok(())
}

fn service_has_slot(
    services: &[ServiceObservation],
    machine: &Machine,
    service_id: &ployz_core::ServiceId,
) -> bool {
    services
        .iter()
        .flat_map(|service| &service.containers)
        .any(|container| {
            let observation = container.as_observation();
            observation.machine_id == machine.id && observation.service_id == *service_id
        })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, net::Ipv6Addr, num::NonZeroU32};

    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerResources,
        ContainerRuntimeObservation, HealthObservation, Machine, MachineId, MachineName,
        MachineTarget, ManagementAddress, Placement, ProjectName, PullPolicy, RequestedServiceSpec,
        ResolvedUpdateConfig, RestartPolicy, ServiceContainerSpec, ServiceId, ServiceMode,
        ServiceName, ServiceObservation, UpdateConfig, WireGuardPublicKey, service_containers,
    };

    use super::*;

    #[test]
    fn partial_observation_names_failures_and_omissions() {
        let joiner = machine('1', "joiner");
        let reachable = machine('2', "reachable");
        let failed = MachineId::parse("a".repeat(32)).unwrap();
        let omitted = MachineId::parse("b".repeat(32)).unwrap();
        let caddy = global_service(
            QualifiedService::system_caddy(),
            'c',
            Placement::default(),
            running_on(&reachable, 'c'),
        );
        let live = LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: reachable.id,
                    value: caddy
                        .containers
                        .iter()
                        .cloned()
                        .map(ServiceContainer::into_observation)
                        .collect(),
                }],
                failures: vec![ployz_core::MachineFailure {
                    machine_id: failed,
                    error: RpcError {
                        code: ployz_core::RpcErrorCode::Unavailable,
                        message: "unreachable".to_owned(),
                        details: serde_json::Value::Null,
                    },
                }],
                omissions: vec![omitted],
            },
        };
        let warning = partial_observation_warning(&live).unwrap();
        assert!(warning.contains("unreachable"));
        assert!(warning.contains("no terminal response"));
        assert_eq!(
            identities(&plan_global_catch_up(&live.services(), &joiner, false)),
            ["ployz-system/caddy"]
        );
    }

    #[tokio::test]
    async fn stale_local_generation_checks_capacity_before_ensuring_current_slot() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let stale = global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            created_on(&joiner, 'a'),
        );
        let current = global_service(
            qualified("app", "api"),
            'b',
            Placement::default(),
            running_on(&founder, 'b'),
        );
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![stale, current],
            capacity: None,
            ensure_calls: Cell::new(0),
        };

        assert!(matches!(
            catch_up_globals(&mut client, &joiner, true).await,
            Err(CatchUpError::Other(_))
        ));
        assert_eq!(client.ensure_calls.get(), 0);
    }

    struct FakeCatchUpClient {
        machine_id: MachineId,
        services: Vec<ServiceObservation>,
        capacity: Option<BridgeEndpointCapacity>,
        ensure_calls: Cell<usize>,
    }

    impl CatchUpClient for FakeCatchUpClient {
        async fn live_services(&mut self) -> Result<LiveServices<RpcError>, CatchUpError> {
            Ok(LiveServices {
                containers: ployz_core::PartialResult {
                    successes: vec![ployz_core::MachineSuccess {
                        machine_id: self.machine_id,
                        value: self
                            .services
                            .iter()
                            .flat_map(|service| service.containers.iter().cloned())
                            .map(ServiceContainer::into_observation)
                            .collect(),
                    }],
                    failures: Vec::new(),
                    omissions: Vec::new(),
                },
            })
        }

        async fn bridge_capacity(
            &self,
            _machine_id: &MachineId,
        ) -> Result<Option<BridgeEndpointCapacity>, CatchUpError> {
            Ok(self.capacity.clone())
        }

        async fn ensure_global_slot(
            &mut self,
            _machine_id: &MachineId,
            _request: EnsureGlobalSlotRequest,
        ) -> Result<(), RpcError> {
            self.ensure_calls.set(self.ensure_calls.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn two_joiners_each_plan_only_their_own_slot() {
        let founder = machine('f', "founder");
        let first = machine('1', "first");
        let second = machine('2', "second");
        let caddy = global_service(
            QualifiedService::system_caddy(),
            'c',
            Placement::default(),
            running_on(&founder, 'a'),
        );

        let first_slots = plan_global_catch_up(std::slice::from_ref(&caddy), &first, false);
        let second_slots = plan_global_catch_up(std::slice::from_ref(&caddy), &second, false);

        assert_eq!(identities(&first_slots), ["ployz-system/caddy"]);
        assert_eq!(identities(&second_slots), ["ployz-system/caddy"]);
        assert!(
            first_slots
                .iter()
                .chain(second_slots.iter())
                .all(|slot| slot.spec.service_id.as_str() == service_id('c').as_str())
        );
    }

    #[test]
    fn skip_caddy_omits_system_caddy_and_keeps_other_globals() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let services = [
            global_service(
                QualifiedService::system_caddy(),
                'c',
                Placement::default(),
                running_on(&founder, 'a'),
            ),
            global_service(
                qualified("app", "api"),
                'a',
                Placement::default(),
                running_on(&founder, 'b'),
            ),
        ];

        assert_eq!(
            identities(&plan_global_catch_up(&services, &joiner, true)),
            ["app/api"]
        );
        assert_eq!(
            identities(&plan_global_catch_up(&services, &joiner, false)),
            ["ployz-system/caddy", "app/api"]
        );
    }

    #[test]
    fn x_machines_excluding_this_joiner_plans_no_slot() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let services = [global_service(
            qualified("app", "api"),
            'a',
            Placement {
                machines: vec![MachineTarget::parse("founder").unwrap()],
            },
            running_on(&founder, 'a'),
        )];
        assert!(plan_global_catch_up(&services, &joiner, false).is_empty());
    }

    #[test]
    fn x_machines_including_this_joiner_plans_a_slot() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let services = [global_service(
            qualified("app", "api"),
            'a',
            Placement {
                machines: vec![
                    MachineTarget::parse("founder").unwrap(),
                    MachineTarget::parse("joiner").unwrap(),
                ],
            },
            running_on(&founder, 'a'),
        )];
        assert_eq!(
            identities(&plan_global_catch_up(&services, &joiner, false)),
            ["app/api"]
        );
    }

    #[test]
    fn catch_up_never_names_a_peer_machine() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let slots = plan_global_catch_up(
            &[global_service(
                qualified("app", "api"),
                'a',
                Placement::default(),
                running_on(&founder, 'a'),
            )],
            &joiner,
            false,
        );
        let spec = &slots
            .first()
            .expect("eligible Global must produce a slot")
            .spec;
        let encoded = serde_json::to_string(spec).unwrap();
        assert!(
            !encoded.contains(founder.id.as_str()),
            "catch-up output must not target a peer Machine, got {encoded}"
        );
        assert_eq!(identities(&slots), ["app/api"]);
    }

    #[test]
    fn machine_add_places_user_globals_not_only_caddy() {
        let added = machine('2', "edge");
        let founder = machine('f', "founder");
        let services = [
            global_service(
                QualifiedService::system_caddy(),
                'c',
                Placement::default(),
                running_on(&founder, 'a'),
            ),
            global_service(
                qualified("shop", "worker"),
                'b',
                Placement::default(),
                running_on(&founder, 'b'),
            ),
        ];
        assert_eq!(
            identities(&plan_global_catch_up(&services, &added, false)),
            ["ployz-system/caddy", "shop/worker"]
        );
    }

    #[test]
    fn created_not_started_on_this_machine_is_still_a_slot() {
        let joiner = machine('1', "joiner");
        let services = [global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            created_on(&joiner, 'a'),
        )];
        assert_eq!(
            identities(&plan_global_catch_up(&services, &joiner, false)),
            ["app/api"]
        );
    }

    #[test]
    fn running_on_this_machine_is_not_a_slot() {
        let joiner = machine('1', "joiner");
        let services = [global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&joiner, 'a'),
        )];
        assert!(plan_global_catch_up(&services, &joiner, false).is_empty());
    }

    #[test]
    fn replicated_services_are_not_catch_up_slots() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let mut spec = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        spec.name = ServiceName::parse("api").unwrap();
        let services = [grouped(
            qualified("app", "api"),
            spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default()),
            running_on(&founder, 'a'),
        )];
        assert!(plan_global_catch_up(&services, &joiner, false).is_empty());
    }

    #[test]
    fn caddy_is_expected_when_observed_eligible_and_not_skipped() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let services = [global_service(
            QualifiedService::system_caddy(),
            'c',
            Placement::default(),
            running_on(&founder, 'a'),
        )];
        let slots = plan_global_catch_up(&services, &joiner, false);
        assert!(caddy_expected(&services, &joiner, false, &slots));
        let skipped = plan_global_catch_up(&services, &joiner, true);
        assert!(!caddy_expected(&services, &joiner, true, &skipped));
        assert!(!caddy_running_on(&services, &joiner));
    }

    fn identities(slots: &[GlobalCatchUpSlot]) -> Vec<String> {
        slots.iter().map(|slot| slot.identity.to_string()).collect()
    }

    fn machine(hex: char, name: &str) -> Machine {
        Machine {
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }

    fn qualified(project: &str, name: &str) -> QualifiedService {
        QualifiedService::new(
            ProjectName::parse(project).unwrap(),
            ServiceName::parse(name).unwrap(),
        )
    }

    fn service_id(hex: char) -> ServiceId {
        ServiceId::parse(hex.to_string().repeat(32)).unwrap()
    }

    fn container_id(hex: char) -> ContainerId {
        ContainerId::parse(hex.to_string().repeat(64)).unwrap()
    }

    fn requested(mode: ServiceMode) -> RequestedServiceSpec {
        RequestedServiceSpec {
            name: ServiceName::parse("api").unwrap(),
            mode,
            container: ServiceContainerSpec {
                image: "ghcr.io/getployz/api:1".into(),
                command: Vec::new(),
                entrypoint: Vec::new(),
                environment: Default::default(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                healthcheck: None,
                pull_policy: PullPolicy::Missing,
                init: None,
                user: None,
                working_directory: None,
                tty: false,
                open_stdin: false,
                privileged: false,
                pid_mode: None,
                log_driver: None,
                resources: ContainerResources::default(),
                stop_timeout_secs: None,
                sysctls: Default::default(),
                restart: RestartPolicy::default(),
            },
            placement: Placement::default(),
            ports: Vec::new(),
            volume_graph: Default::default(),
            config_graph: Default::default(),
            pre_deploy: None,
            caddy_config: None,
            update: UpdateConfig::default(),
        }
    }

    fn global_service(
        identity: QualifiedService,
        id: char,
        placement: Placement,
        container: ContainerObservation,
    ) -> ServiceObservation {
        let mut spec = requested(ServiceMode::Global);
        spec.name = identity.name.clone();
        spec.placement = placement;
        grouped(
            identity,
            spec.to_resolved(service_id(id), ResolvedUpdateConfig::default()),
            container,
        )
    }

    fn grouped(
        identity: QualifiedService,
        spec: ResolvedServiceSpec,
        mut container: ContainerObservation,
    ) -> ServiceObservation {
        container.project_name = identity.project.clone();
        container.service_name = identity.name.clone();
        container.service_id = spec.service_id;
        container.resolved_spec = spec.clone();
        ServiceObservation {
            identity,
            service_id: spec.service_id,
            containers: service_containers([container]),
            hook_containers: Vec::new(),
        }
    }

    fn running_on(machine: &Machine, hex: char) -> ContainerObservation {
        container_on(
            machine,
            hex,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
        )
    }

    fn created_on(machine: &Machine, hex: char) -> ContainerObservation {
        container_on(machine, hex, ContainerRuntimeObservation::Created)
    }

    fn container_on(
        machine: &Machine,
        hex: char,
        runtime: ContainerRuntimeObservation,
    ) -> ContainerObservation {
        ContainerObservation {
            container_id: container_id(hex),
            display_name: format!("slot-{hex}"),
            created_at_unix_nanos: 0,
            machine_id: machine.id,
            project_name: ProjectName::parse("app").unwrap(),
            service_id: service_id('a'),
            service_name: ServiceName::parse("api").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime,
            effective_healthcheck: None,
            resolved_spec: requested(ServiceMode::Global)
                .to_resolved(service_id('a'), ResolvedUpdateConfig::default()),
            address: None,
            labels: Default::default(),
        }
    }
}
