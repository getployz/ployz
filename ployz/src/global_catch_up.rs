//! Global catch-up: place observed eligible Globals onto this Machine only.

use std::collections::BTreeMap;

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, EnsureGlobalSlotRequest, IngressProxyNetworkMode,
    InspectRequest, ListContainersRequest, LiveServices, Machine, MachineId, MachineTarget,
    ObservedGlobalSlotSpec, QualifiedService, RpcError, ServiceObservation,
    ServicePlacementEligibility, ingress_proxy_backend, op, service_containers,
};

use crate::{connect::Client, deploy::endpoint_capacity_error, failure::Failure};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    cause: Failure,
    missing: Vec<QualifiedService>,
}

impl CatchUpError {
    /// Record the failure and every eligible Global left missing.
    pub(crate) fn new(cause: Failure, missing: Vec<QualifiedService>) -> Self {
        Self { cause, missing }
    }
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure>;
    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure>;
    async fn ensure_global_slot(
        &mut self,
        machine_id: &MachineId,
        request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError>;
    /// List Containers directly from the joined Machine for final verification.
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
        Client::live_services(self).await.map_err(Into::into)
    }

    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        let details = self
            .read::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                &MachineTarget::from(machine_id),
            )
            .await
            .map_err(Failure::from)?;
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

    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        self.read::<op::ListContainers>(ListContainersRequest {}, &MachineTarget::from(machine_id))
            .await
            .map(|list| list.containers)
            .map_err(Failure::from)
    }
}

pub(crate) fn joined_catch_up_error(error: CatchUpError) -> String {
    let mut message = format!(
        "Machine joined, but Global catch-up is incomplete; it remains a Cluster member. {}",
        error.cause
    );
    if !error.missing.is_empty() {
        message.push_str("\nMissing eligible Globals:");
        for identity in error.missing {
            if identity == QualifiedService::system_ingress() {
                message.push_str("\n- ployz-system/ingress: run `ployz ingress deploy`.");
            } else {
                message.push_str(&format!(
                    "\n- {identity}: redeploy Project Service `{identity}`."
                ));
            }
        }
    }
    message
}

/// Globals this Machine is eligible for and does not already run.
#[must_use]
pub fn plan_global_catch_up(
    services: &[ServiceObservation],
    this_machine: &Machine,
    skip_ingress: bool,
) -> Vec<ObservedGlobalSlotSpec> {
    services
        .iter()
        .filter_map(|service| {
            let slot = eligible_catch_up_slot(service, this_machine)?;
            (!slot.is_running_on(&service.containers, this_machine)).then_some(slot)
        })
        .filter(|slot| !skip_ingress || slot.identity() != &QualifiedService::system_ingress())
        .collect()
}

fn eligible_catch_up_slot(
    service: &ServiceObservation,
    machine: &Machine,
) -> Option<ObservedGlobalSlotSpec> {
    let slot = service.observed_global_slot()?;
    (slot.resolved_spec().placement_eligibility(machine, None)
        == ServicePlacementEligibility::Eligible)
        .then_some(slot)
}

/// Copy every observed eligible Global onto `this_machine` only.
///
/// # Errors
///
/// Fails when listing Services fails, the target Machine does not answer, or
/// any eligible Global cannot be placed.
pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    skip_ingress: bool,
) -> Result<(), CatchUpError> {
    let live = client
        .live_services()
        .await
        .map_err(|error| CatchUpError::new(error, Vec::new()))?;
    let services = live.services();
    let initially_eligible = services
        .iter()
        .filter_map(|service| eligible_catch_up_slot(service, this_machine))
        .filter(|slot| !skip_ingress || slot.identity() != &QualifiedService::system_ingress())
        .map(|slot| (slot.identity().clone(), slot))
        .collect::<BTreeMap<_, _>>();
    let slots = plan_global_catch_up(&services, this_machine, skip_ingress);
    let initially_missing: Vec<_> = slots.iter().map(|slot| slot.identity().clone()).collect();
    let endpoint_creates = slots
        .iter()
        .filter(|slot| {
            let uses_bridge_endpoint = slot.identity() != &QualifiedService::system_ingress()
                || ingress_proxy_backend(slot.resolved_spec()).map_or(true, |backend| {
                    matches!(backend.network_mode(), IngressProxyNetworkMode::Bridge)
                });
            uses_bridge_endpoint
                && !service_has_slot(&services, this_machine, &slot.resolved_spec().service_id)
        })
        .count();
    if endpoint_creates > 0 {
        let capacity = client
            .bridge_capacity(&this_machine.id)
            .await
            .map_err(|error| CatchUpError::new(error, initially_missing.clone()))?;
        if let Some(error) = endpoint_capacity_error(endpoint_creates, capacity.as_ref()) {
            return Err(CatchUpError::new(
                Failure::usage(error.to_string()),
                initially_missing,
            ));
        }
    }
    if !slots.is_empty() {
        eprintln!("Placing Global Services on this Machine.");
    }
    let mut failures = Vec::new();
    for slot in slots {
        let (identity, resolved_spec) = slot.into_parts();
        let failure_identity = identity.clone();
        if let Err(error) = client
            .ensure_global_slot(
                &this_machine.id,
                EnsureGlobalSlotRequest {
                    project_name: identity.project,
                    resolved_spec,
                },
            )
            .await
        {
            failures.push((failure_identity, error));
        }
    }
    let missing_if_unverified = initially_eligible.keys().cloned().collect();
    let target_containers = client
        .target_containers(&this_machine.id)
        .await
        .map_err(|error| CatchUpError::new(error, missing_if_unverified))?;
    let target_services = service_containers(target_containers);
    let missing = initially_eligible
        .into_iter()
        .filter_map(|(identity, slot)| {
            (!slot.is_running_on(&target_services, this_machine)).then_some(identity)
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let details = failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::usage("eligible Globals are not running after catch-up")
        } else {
            Failure::usage(format!("Global catch-up RPCs failed: {details}"))
        };
        return Err(CatchUpError::new(cause, missing));
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
    use std::{
        cell::Cell,
        net::Ipv6Addr,
        num::{NonZeroU16, NonZeroU32, NonZeroU64},
    };

    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerResources,
        ContainerRuntimeObservation, DockerVolumeName, HealthObservation, HostBind, Machine,
        MachineId, MachineName, MachineTarget, ManagementAddress, Placement, PortPublication,
        ProjectName, ProvisionedVolumeMaximumBytes, PullPolicy, RequestedServiceSpec,
        ResolvedServiceSpec, ResolvedUpdateConfig, RestartPolicy, ServiceContainerSpec, ServiceId,
        ServiceMode, ServiceMount, ServiceName, ServiceObservation, ServiceVolume,
        ServiceVolumeGraph, ServiceVolumeReference, TransportProtocol, UpdateConfig, VolumeSource,
        WireGuardPublicKey, service_containers,
    };

    use super::*;

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
            target_services: None,
            capacity: None,
            ensure_calls: Cell::new(0),
        };

        assert!(catch_up_globals(&mut client, &joiner, true).await.is_err());
        assert_eq!(client.ensure_calls.get(), 0);
    }

    #[tokio::test]
    async fn zentinel_catch_up_does_not_require_a_bridge_endpoint() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let identity = QualifiedService::system_ingress();
        let requested = ployz_core::IngressProxyBackend::Zentinel
            .requested_service_spec("zentinel:test".into(), Vec::new(), None)
            .unwrap();
        let spec = requested.to_resolved(
            service_id('c'),
            ResolvedUpdateConfig {
                order: ployz_core::UpdateOrder::StopFirst,
                monitor_millis: None,
            },
        );
        let current = grouped(identity.clone(), spec.clone(), running_on(&founder, 'a'));
        let target = grouped(identity, spec, running_on(&joiner, 'b'));
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![current],
            target_services: Some(vec![target]),
            capacity: Some(BridgeEndpointCapacity::new(0, 0)),
            ensure_calls: Cell::new(0),
        };

        catch_up_globals(&mut client, &joiner, false).await.unwrap();
        assert_eq!(client.ensure_calls.get(), 1);
    }

    #[tokio::test]
    async fn envoy_catch_up_requires_a_bridge_endpoint() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let current = observed_envoy_ingress(&founder, 'c');
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![current],
            target_services: None,
            capacity: Some(BridgeEndpointCapacity::new(0, 0)),
            ensure_calls: Cell::new(0),
        };

        assert!(catch_up_globals(&mut client, &joiner, false).await.is_err());
        assert_eq!(client.ensure_calls.get(), 0);
    }

    #[tokio::test]
    async fn envoy_catch_up_succeeds_when_a_bridge_endpoint_is_available() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let identity = QualifiedService::system_ingress();
        let spec = canonical_envoy_spec('c');
        let current = grouped(identity.clone(), spec.clone(), running_on(&founder, 'a'));
        let target = grouped(identity, spec, running_on(&joiner, 'b'));
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![current],
            target_services: Some(vec![target]),
            capacity: Some(BridgeEndpointCapacity::new(10, 0)),
            ensure_calls: Cell::new(0),
        };

        catch_up_globals(&mut client, &joiner, false).await.unwrap();
        assert_eq!(client.ensure_calls.get(), 1);
    }

    #[tokio::test]
    async fn catch_up_places_observed_envoy_when_joiner_runs_noncanonical_ingress() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let identity = QualifiedService::system_ingress();
        let envoy_spec = canonical_envoy_spec('e');
        let drifted = observed_caddy_ingress(&joiner, 'd');
        let mut founder_envoy = running_on(&founder, 'e');
        founder_envoy.created_at_unix_nanos = 2;
        let current = grouped(identity.clone(), envoy_spec.clone(), founder_envoy);
        let target = grouped(identity, envoy_spec, running_on(&joiner, 'b'));
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![current, drifted],
            target_services: Some(vec![target]),
            capacity: Some(BridgeEndpointCapacity::new(10, 0)),
            ensure_calls: Cell::new(0),
        };

        catch_up_globals(&mut client, &joiner, false).await.unwrap();
        assert_eq!(client.ensure_calls.get(), 1);
    }

    #[tokio::test]
    async fn successful_ensure_is_reobserved_before_success() {
        let joiner = machine('1', "joiner");
        let service = global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            created_on(&joiner, 'a'),
        );
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![service],
            target_services: None,
            capacity: None,
            ensure_calls: Cell::new(0),
        };

        let error = catch_up_globals(&mut client, &joiner, true)
            .await
            .unwrap_err();
        assert_eq!(client.ensure_calls.get(), 1);
        assert_eq!(error.missing, [qualified("app", "api")]);
    }

    #[tokio::test]
    async fn initially_eligible_global_absent_from_target_inspection_remains_missing() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let service = global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        );
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![service],
            target_services: Some(Vec::new()),
            capacity: None,
            ensure_calls: Cell::new(0),
        };

        let error = catch_up_globals(&mut client, &joiner, true)
            .await
            .unwrap_err();
        assert_eq!(error.missing, [qualified("app", "api")]);
    }

    #[tokio::test]
    async fn initially_eligible_global_with_only_hook_visible_remains_missing() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let service = global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        );
        let mut hook = service
            .containers
            .first()
            .expect("test Global has one Service container")
            .clone()
            .into_observation();
        hook.kind = ContainerKind::PreDeployHook;
        let hook_only = ServiceObservation {
            identity: service.identity.clone(),
            service_id: service.service_id,
            containers: Vec::new(),
            hook_containers: vec![ployz_core::HookContainer::try_from(hook).unwrap()],
        };
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![service],
            target_services: Some(vec![hook_only]),
            capacity: None,
            ensure_calls: Cell::new(0),
        };

        let error = catch_up_globals(&mut client, &joiner, true)
            .await
            .unwrap_err();
        assert_eq!(error.missing, [qualified("app", "api")]);
    }

    #[tokio::test]
    async fn initially_eligible_generation_absent_from_target_inspection_remains_missing() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let stale = global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&joiner, 'a'),
        );
        let current = global_service(
            qualified("app", "api"),
            'b',
            Placement::default(),
            running_on(&founder, 'b'),
        );
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![stale.clone(), current],
            target_services: Some(vec![stale]),
            capacity: Some(BridgeEndpointCapacity::new(10, 0)),
            ensure_calls: Cell::new(0),
        };

        let error = catch_up_globals(&mut client, &joiner, true)
            .await
            .unwrap_err();
        assert_eq!(client.ensure_calls.get(), 1);
        assert_eq!(error.missing, [qualified("app", "api")]);
    }

    struct FakeCatchUpClient {
        machine_id: MachineId,
        services: Vec<ServiceObservation>,
        target_services: Option<Vec<ServiceObservation>>,
        capacity: Option<BridgeEndpointCapacity>,
        ensure_calls: Cell<usize>,
    }

    impl CatchUpClient for FakeCatchUpClient {
        async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
            Ok(LiveServices {
                containers: ployz_core::PartialResult {
                    successes: vec![ployz_core::MachineSuccess {
                        machine_id: self.machine_id,
                        value: self
                            .services
                            .iter()
                            .flat_map(ServiceObservation::members)
                            .map(|container| container.as_observation().clone())
                            .collect(),
                    }],
                    failures: Vec::new(),
                    omissions: Vec::new(),
                },
            })
        }

        async fn bridge_capacity(
            &mut self,
            _machine_id: &MachineId,
        ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
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

        async fn target_containers(
            &mut self,
            _machine_id: &MachineId,
        ) -> Result<Vec<ContainerObservation>, Failure> {
            Ok(self
                .target_services
                .as_ref()
                .unwrap_or(&self.services)
                .iter()
                .flat_map(ServiceObservation::members)
                .map(|container| container.as_observation().clone())
                .collect())
        }
    }

    #[test]
    fn two_joiners_each_plan_only_their_own_slot() {
        let founder = machine('f', "founder");
        let first = machine('1', "first");
        let second = machine('2', "second");
        let ingress = global_service(
            QualifiedService::system_ingress(),
            'c',
            Placement::default(),
            running_on(&founder, 'a'),
        );

        let first_slots = plan_global_catch_up(std::slice::from_ref(&ingress), &first, false);
        let second_slots = plan_global_catch_up(std::slice::from_ref(&ingress), &second, false);

        assert_eq!(identities(&first_slots), ["ployz-system/ingress"]);
        assert_eq!(identities(&second_slots), ["ployz-system/ingress"]);
        assert!(
            first_slots
                .iter()
                .chain(second_slots.iter())
                .all(|slot| slot.resolved_spec().service_id.as_str() == service_id('c').as_str())
        );
    }

    #[test]
    fn add_machine_inherits_each_observed_ingress_backend_spec() {
        let founder = machine('f', "founder");
        let joiner = machine('1', "joiner");
        for (command, capabilities) in [
            (
                vec!["caddy", "run", "-c", "/config/caddy/Caddyfile"],
                Vec::<&str>::new(),
            ),
            (vec!["-c", "/config/zentinel.kdl"], vec!["NET_BIND_SERVICE"]),
            (
                vec!["envoy", "-c", "/config/bootstrap.yaml"],
                Vec::<&str>::new(),
            ),
        ] {
            let identity = QualifiedService::system_ingress();
            let mut spec = requested(ServiceMode::Global);
            spec.name = identity.name.clone();
            spec.container.command = command.iter().map(|value| (*value).into()).collect();
            spec.container.cap_add = capabilities.iter().map(|value| (*value).into()).collect();
            if command == ["-c", "/config/zentinel.kdl"] {
                spec.container.cap_drop = vec!["ALL".into()];
            }
            let service = grouped(
                identity,
                spec.to_resolved(service_id('c'), ResolvedUpdateConfig::default()),
                running_on(&founder, 'a'),
            );

            let slots = plan_global_catch_up(&[service], &joiner, false);
            assert_eq!(slots.len(), 1);
            let inherited = slots.first().unwrap().resolved_spec();
            assert!(
                inherited
                    .container
                    .command
                    .iter()
                    .map(String::as_str)
                    .eq(command.iter().copied())
            );
            assert!(
                inherited
                    .container
                    .cap_add
                    .iter()
                    .map(String::as_str)
                    .eq(capabilities.iter().copied())
            );
        }
    }

    #[test]
    fn add_machine_inherits_canonical_envoy_ingress_spec() {
        let founder = machine('f', "founder");
        let joiner = machine('1', "joiner");
        let slots = plan_global_catch_up(&[observed_envoy_ingress(&founder, 'c')], &joiner, false);
        assert_eq!(slots.len(), 1);
        assert_canonical_envoy_spec(slots.first().unwrap().resolved_spec());
    }

    #[test]
    fn joiner_running_noncanonical_ingress_still_plans_the_observed_envoy_slot() {
        let founder = machine('f', "founder");
        let joiner = machine('1', "joiner");
        let identity = QualifiedService::system_ingress();
        let mut founder_envoy = running_on(&founder, 'e');
        founder_envoy.created_at_unix_nanos = 2;
        let mut observed = grouped(identity, canonical_envoy_spec('e'), founder_envoy);
        observed
            .containers
            .extend(observed_caddy_ingress(&joiner, 'd').containers);

        let slots = plan_global_catch_up(&[observed], &joiner, false);
        assert_eq!(slots.len(), 1);
        assert_canonical_envoy_spec(slots.first().unwrap().resolved_spec());
    }

    #[test]
    fn skip_ingress_omits_system_ingress_and_keeps_other_globals() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let services = [
            global_service(
                QualifiedService::system_ingress(),
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
            ["ployz-system/ingress", "app/api"]
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
            .resolved_spec();
        let encoded = serde_json::to_string(spec).unwrap();
        assert!(
            !encoded.contains(founder.id.as_str()),
            "catch-up output must not target a peer Machine, got {encoded}"
        );
        assert_eq!(identities(&slots), ["app/api"]);
    }

    #[test]
    fn machine_add_places_user_globals_not_only_ingress() {
        let added = machine('2', "edge");
        let founder = machine('f', "founder");
        let services = [
            global_service(
                QualifiedService::system_ingress(),
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
            ["ployz-system/ingress", "shop/worker"]
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
    fn provisioned_globals_defer_to_machine_local_reconciliation() {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let mut spec = requested(ServiceMode::Global);
        let reference = ServiceVolumeReference::parse("data").unwrap();
        spec.volume_graph = ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source: VolumeSource::Provisioned {
                    name: DockerVolumeName::parse("app_data").unwrap(),
                    maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                        NonZeroU64::new(100).unwrap(),
                    ),
                    labels: Default::default(),
                },
            }],
            vec![ServiceMount {
                volume: reference,
                target: ContainerPath::parse("/data").unwrap(),
                read_only: false,
                no_copy: false,
                subpath: None,
            }],
        )
        .unwrap();
        let service = grouped(
            qualified("app", "api"),
            spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default()),
            running_on(&founder, 'a'),
        );

        assert!(plan_global_catch_up(&[service], &joiner, false).is_empty());
    }

    fn canonical_envoy_spec(id: char) -> ResolvedServiceSpec {
        ployz_core::IngressProxyBackend::Envoy
            .requested_service_spec("envoy:test".into(), Vec::new(), None)
            .unwrap()
            .to_resolved(service_id(id), ResolvedUpdateConfig::default())
    }

    fn observed_envoy_ingress(machine: &Machine, id: char) -> ServiceObservation {
        grouped(
            QualifiedService::system_ingress(),
            canonical_envoy_spec(id),
            running_on(machine, id),
        )
    }

    fn observed_caddy_ingress(machine: &Machine, id: char) -> ServiceObservation {
        let spec = ployz_core::IngressProxyBackend::Caddy
            .requested_service_spec("caddy:test".into(), Vec::new(), None)
            .unwrap()
            .to_resolved(service_id(id), ResolvedUpdateConfig::default());
        let mut container = running_on(machine, id);
        container.created_at_unix_nanos = 1;
        grouped(QualifiedService::system_ingress(), spec, container)
    }

    fn assert_canonical_envoy_spec(spec: &ResolvedServiceSpec) {
        assert_eq!(
            spec.container.command,
            ["envoy", "-c", "/config/bootstrap.yaml"]
        );
        assert!(spec.container.cap_add.is_empty());
        assert_eq!(
            spec.ports,
            [
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(80).unwrap(),
                    container_port: NonZeroU16::new(8080).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(443).unwrap(),
                    container_port: NonZeroU16::new(8443).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
            ]
        );
        assert!(
            spec.volume_graph
                .volumes()
                .iter()
                .filter_map(|volume| match &volume.source {
                    VolumeSource::Bind { machine_path, .. } => Some(machine_path.as_str()),
                    VolumeSource::External { .. }
                    | VolumeSource::Ordinary { .. }
                    | VolumeSource::Provisioned { .. }
                    | VolumeSource::Tmpfs { .. } => None,
                })
                .eq(["/var/lib/ployz/ingress/envoy"])
        );
        assert_eq!(
            ingress_proxy_backend(spec).unwrap(),
            ployz_core::IngressProxyBackend::Envoy
        );
    }

    fn identities(slots: &[ObservedGlobalSlotSpec]) -> Vec<String> {
        slots
            .iter()
            .map(|slot| slot.identity().to_string())
            .collect()
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
                labels: Default::default(),
                hostname: None,
                extra_hosts: Vec::new(),
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
            ingress_proxy_fragment: None,
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
