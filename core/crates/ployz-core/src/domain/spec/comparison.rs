//! Runtime planning impact of a requested spec against an observed one.

use super::*;
use crate::{ByteQuantity, ContainerObservation, LOG_DRIVER};

/// Return the runtime action needed to reconcile the requested serving shape.
#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    if requested.container.pull_policy == PullPolicy::Always
        // A new hook runs with new Containers; it isn't serving shape, so traffic stays on the old ones meanwhile.
        || current.pre_deploy != requested.pre_deploy
        || stop_changed(&current.container, &requested.container)
        // Which Volumes a Container may share is fixed on its labels at create, so a
        // change replaces it and the Machine never weighs a stale consent.
        || current.container.shared_writes != requested.container.shared_writes
    {
        return SpecChange::NeedsRecreate;
    }
    let resources = resource_change(&current.container.resources, &requested.container.resources);
    if current.serving_shape() == requested.serving_shape() && current.regions == requested.regions
    {
        resources
    } else if resources == SpecChange::UpToDate && differs_in_placement_only(current, requested) {
        SpecChange::NeedsPlacementUpdate
    } else {
        SpecChange::NeedsRecreate
    }
}

/// Docker fixes a Container's stop signal and grace at creation. A Container created
/// before Services had stop settings carries neither, and keeps Docker's own until it is
/// next replaced: the defaults alone never replace every Container of every Service.
fn stop_changed(current: &ServiceContainerSpec, requested: &ServiceContainerSpec) -> bool {
    let stop = |spec: &ServiceContainerSpec| (spec.stop_signal.clone(), spec.stop_timeout_secs);
    let defaults = |spec: &ServiceContainerSpec| {
        (
            spec.default_stop_signal().map(str::to_owned),
            Some(i64::from(crate::config::STOP_GRACE_PERIOD_DEFAULT)),
        )
    };
    stop(current) != stop(requested)
        && !(stop(current) == (None, None) && stop(requested) == defaults(requested))
}

/// [`compare_specs`] on a Container's retained spec, recreating one that runs on another log
/// driver, as a Container started before the Collector shipped does.
#[must_use]
pub fn compare_observed(
    observation: &ContainerObservation,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    if observation
        .log_driver
        .as_deref()
        .is_some_and(|driver| driver != LOG_DRIVER)
    {
        return SpecChange::NeedsRecreate;
    }
    compare_specs(&observation.resolved_spec, requested)
}

/// The counts by region are placement too: they say which Machines may hold a
/// replica. A Global Container's Creation Key is fixed when it is created and
/// carries its serving shape, placement included, so a Global Service's placement
/// changes only with new Containers.
fn differs_in_placement_only(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> bool {
    matches!(current.mode, ServiceMode::Replicated { .. })
        && ServingShape::of_resolved_placed(current, &requested.placement)
            == requested.serving_shape()
}

/// Docker applies a raised, lowered or newly set CPU or memory limit to a running
/// Container in place. It cannot clear one: zero means "unchanged" on update, and
/// shm size, devices and ulimits are fixed at creation.
fn resource_change(current: &ContainerResources, requested: &ContainerResources) -> SpecChange {
    let ContainerResources {
        cpu_nanos: current_cpu_nanos,
        memory_bytes: current_memory_bytes,
        memory_reservation_bytes: current_memory_reservation_bytes,
        shared_memory_bytes: current_shared_memory_bytes,
        devices: current_devices,
        device_reservations: current_device_reservations,
        ulimits: current_ulimits,
    } = current;
    let ContainerResources {
        cpu_nanos: requested_cpu_nanos,
        memory_bytes: requested_memory_bytes,
        memory_reservation_bytes: requested_memory_reservation_bytes,
        shared_memory_bytes: requested_shared_memory_bytes,
        devices: requested_devices,
        device_reservations: requested_device_reservations,
        ulimits: requested_ulimits,
    } = requested;
    if current_devices != requested_devices
        || current_device_reservations != requested_device_reservations
        || current_ulimits != requested_ulimits
        || current_shared_memory_bytes != requested_shared_memory_bytes
    {
        return SpecChange::NeedsRecreate;
    }
    let limits = [
        limit_change(current_cpu_nanos.as_ref(), requested_cpu_nanos.as_ref()),
        memory_change(
            current_memory_bytes.as_ref(),
            requested_memory_bytes.as_ref(),
        ),
        limit_change(
            current_memory_reservation_bytes.as_ref(),
            requested_memory_reservation_bytes.as_ref(),
        ),
    ];
    if limits.contains(&LimitChange::Recreate) {
        return SpecChange::NeedsRecreate;
    }
    if limits.contains(&LimitChange::InPlace) {
        return SpecChange::NeedsUpdate;
    }
    SpecChange::UpToDate
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitChange {
    Same,
    /// Docker applies the new value to the running Container.
    InPlace,
    /// Only a new Container runs with the requested value.
    Recreate,
}

/// A limit appears or takes any new value in place; only its removal recreates.
fn limit_change<T: PartialEq>(current: Option<&T>, requested: Option<&T>) -> LimitChange {
    match (current, requested) {
        (Some(_), None) => LimitChange::Recreate,
        (current, requested) if current == requested => LimitChange::Same,
        (_, _) => LimitChange::InPlace,
    }
}

/// A memory limit only rises in place. Lowering it below what the Container
/// already uses would have the kernel kill it at once, and a Container that
/// never had one can hold more than any limit set afterwards.
fn memory_change(current: Option<&ByteQuantity>, requested: Option<&ByteQuantity>) -> LimitChange {
    match (current, requested) {
        (current, requested) if current == requested => LimitChange::Same,
        (Some(current), Some(requested)) if requested > current => LimitChange::InPlace,
        (_, _) => LimitChange::Recreate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_container_on_the_local_log_driver_is_recreated_and_one_on_json_file_is_kept() {
        let resolved: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "api:1", "pull_policy": "missing"}
        }))
        .unwrap();
        let requested = resolved.to_requested();
        let observed = |log_driver: Option<&str>| {
            ContainerObservation::try_from(crate::ContainerObservationParts {
                rotation: None,
                r#gen: None,
                container_id: crate::ContainerId::parse("1".repeat(64)).unwrap(),
                display_name: "api-1".into(),
                created_at_unix_nanos: 0,
                machine_id: crate::MachineId::parse("1".repeat(32)).unwrap(),
                namespace: crate::Namespace::parse("app").unwrap(),
                kind: crate::ContainerKind::ServiceContainer,
                runtime: crate::ContainerRuntimeObservation::Created,
                effective_healthcheck: None,
                resolved_spec: resolved.clone(),
                address: None,
                labels: Default::default(),
                stopped: false,
                run: None,
                log_driver: log_driver.map(str::to_owned),
                admission: None,
                replaced_by: None,
            })
            .unwrap()
        };
        assert_eq!(
            compare_observed(&observed(Some("local")), &requested),
            SpecChange::NeedsRecreate
        );
        assert_eq!(
            compare_observed(&observed(Some("json-file")), &requested),
            SpecChange::UpToDate
        );
        assert_eq!(
            compare_observed(&observed(None), &requested),
            SpecChange::UpToDate
        );
    }

    #[test]
    fn command_and_hook_edits_recreate_and_resource_edits_update() {
        let current: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "api:1", "pull_policy": "missing", "command": ["serve"]}
        }))
        .unwrap();
        let mut candidate = current.to_requested();
        assert_eq!(compare_specs(&current, &candidate), SpecChange::UpToDate);
        candidate.container.command = vec!["serve".into(), "--workers=2".into()];
        assert_eq!(
            compare_specs(&current, &candidate),
            SpecChange::NeedsRecreate
        );
        candidate = current.to_requested();
        candidate.container.pull_policy = PullPolicy::Always;
        assert_eq!(
            compare_specs(&current, &candidate),
            SpecChange::NeedsRecreate
        );
        candidate = current.to_requested();
        candidate.container.resources.cpu_nanos =
            Some(crate::CpuNanos::try_from(500_000_000).unwrap());
        assert_eq!(compare_specs(&current, &candidate), SpecChange::NeedsUpdate);
        candidate = current.to_requested();
        candidate.container.resources.shared_memory_bytes =
            Some(crate::ByteQuantity::try_from(64).unwrap());
        assert_eq!(
            compare_specs(&current, &candidate),
            SpecChange::NeedsRecreate
        );
        candidate = current.to_requested();
        candidate.pre_deploy =
            Some(serde_json::from_value(json!({"command": ["migrate"]})).unwrap());
        assert_eq!(
            compare_specs(&current, &candidate),
            SpecChange::NeedsRecreate
        );
    }

    #[test]
    fn the_stop_defaults_alone_keep_a_container_from_before_them_and_a_set_stop_recreates() {
        for command in [json!([]), json!(["php", "artisan", "horizon"])] {
            let current: ResolvedServiceSpec = serde_json::from_value(json!({
                "service_id": "a".repeat(32), "name": "worker",
                "mode": {"mode": "replicated", "replicas": 1},
                "container": {"image": "php:8-fpm", "pull_policy": "missing", "command": command}
            }))
            .unwrap();
            let mut defaults = current.to_requested();
            defaults.container.stop_timeout_secs = Some(30);
            defaults.container.stop_signal =
                defaults.container.default_stop_signal().map(str::to_owned);
            assert_eq!(compare_specs(&current, &defaults), SpecChange::UpToDate);

            let mut longer = defaults.clone();
            longer.container.stop_timeout_secs = Some(120);
            assert_eq!(compare_specs(&current, &longer), SpecChange::NeedsRecreate);
            let mut signal = defaults.clone();
            signal.container.stop_signal = Some("SIGINT".into());
            assert_eq!(compare_specs(&current, &signal), SpecChange::NeedsRecreate);
        }
    }

    #[test]
    fn a_placement_only_edit_keeps_a_replicated_container_and_anything_more_recreates() {
        let spec = |mode: serde_json::Value| -> ResolvedServiceSpec {
            serde_json::from_value(json!({
                "service_id": "a".repeat(32), "name": "api", "mode": mode,
                "container": {"image": "api:1", "pull_policy": "missing"}
            }))
            .unwrap()
        };
        let in_fsn = |spec: &ResolvedServiceSpec| {
            let mut placed = spec.to_requested();
            placed.placement = serde_json::from_value(json!({
                "constraints": ["node.labels.region==fsn"]
            }))
            .unwrap();
            placed
        };
        let current = spec(json!({"mode": "replicated", "replicas": 2}));
        let placed = in_fsn(&current);
        assert_eq!(
            compare_specs(&current, &placed),
            SpecChange::NeedsPlacementUpdate
        );
        let mut cleared = current.clone();
        cleared.placement = placed.placement.clone();
        assert_eq!(
            compare_specs(&cleared, &current.to_requested()),
            SpecChange::NeedsPlacementUpdate
        );

        let mut with_image = placed.clone();
        with_image.container.image = "api:2".into();
        assert_eq!(
            compare_specs(&current, &with_image),
            SpecChange::NeedsRecreate
        );
        let mut with_limit = placed.clone();
        with_limit.container.resources.cpu_nanos =
            Some(crate::CpuNanos::try_from(500_000_000).unwrap());
        assert_eq!(
            compare_specs(&current, &with_limit),
            SpecChange::NeedsRecreate
        );
        let mut with_hook = placed;
        with_hook.pre_deploy =
            Some(serde_json::from_value(json!({"command": ["migrate"]})).unwrap());
        assert_eq!(
            compare_specs(&current, &with_hook),
            SpecChange::NeedsRecreate
        );

        let global = spec(json!({"mode": "global"}));
        assert_eq!(
            compare_specs(&global, &in_fsn(&global)),
            SpecChange::NeedsRecreate
        );
    }

    #[test]
    fn a_change_to_the_counts_by_region_alone_keeps_the_container() {
        let plain: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 2},
            "container": {"image": "api:1", "pull_policy": "missing"}
        }))
        .unwrap();
        let counted = |counts: serde_json::Value| {
            let mut spec = plain.to_requested();
            spec.regions = serde_json::from_value(counts).unwrap();
            spec
        };
        let in_fsn = counted(json!({"fsn": 2}));
        let mut stored = plain.clone();
        stored.regions = in_fsn.regions.clone();

        assert_eq!(
            compare_specs(&plain, &in_fsn),
            SpecChange::NeedsPlacementUpdate
        );
        assert_eq!(compare_specs(&stored, &in_fsn), SpecChange::UpToDate);
        assert_eq!(
            compare_specs(&stored, &counted(json!({"fsn": 1, "syd": 1}))),
            SpecChange::NeedsPlacementUpdate
        );
        assert_eq!(
            compare_specs(&stored, &plain.to_requested()),
            SpecChange::NeedsPlacementUpdate
        );
        let mut with_limit = plain.to_requested();
        with_limit.container.resources.cpu_nanos =
            Some(crate::CpuNanos::try_from(500_000_000).unwrap());
        assert_eq!(
            compare_specs(&stored, &with_limit),
            SpecChange::NeedsRecreate
        );
    }

    #[test]
    fn a_grown_provisioned_bound_is_up_to_date() {
        let mounting = |maximum_bytes: u64| -> ResolvedServiceSpec {
            serde_json::from_value(json!({
                "service_id": "a".repeat(32), "name": "db",
                "mode": {"mode": "replicated", "replicas": 1},
                "container": {"image": "postgres:17", "pull_policy": "missing"},
                "volumes": [{"reference": "data", "source": {
                    "kind": "provisioned", "name": "app_data", "maximum_bytes": maximum_bytes,
                    "scope": {"namespace": "app", "logical_name": "data"}
                }}],
                "mounts": [{"volume": "data", "target": "/data"}],
            }))
            .unwrap()
        };
        assert_eq!(
            compare_specs(
                &mounting(5_000_000_000),
                &mounting(8_000_000_000).to_requested()
            ),
            SpecChange::UpToDate
        );
    }

    fn limited() -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {
                "image": "api:1", "pull_policy": "missing",
                "resources": {"cpu_nanos": 500_000_000, "memory_bytes": 64}
            }
        }))
        .unwrap()
    }

    #[test]
    fn clearing_a_limit_recreates_because_docker_update_cannot_remove_one() {
        let limited = limited();
        let mut raised = limited.to_requested();
        raised.container.resources.memory_bytes = Some(crate::ByteQuantity::try_from(128).unwrap());
        assert_eq!(compare_specs(&limited, &raised), SpecChange::NeedsUpdate);
        let mut cleared = limited.to_requested();
        cleared.container.resources.cpu_nanos = None;
        assert_eq!(compare_specs(&limited, &cleared), SpecChange::NeedsRecreate);
        let mut cleared_and_raised = raised;
        cleared_and_raised.container.resources.cpu_nanos = None;
        assert_eq!(
            compare_specs(&limited, &cleared_and_raised),
            SpecChange::NeedsRecreate
        );
    }

    #[test]
    fn memory_only_rises_in_place_while_cpu_moves_both_ways() {
        let limited = limited();
        let mut lowered = limited.to_requested();
        lowered.container.resources.memory_bytes = Some(crate::ByteQuantity::try_from(32).unwrap());
        assert_eq!(compare_specs(&limited, &lowered), SpecChange::NeedsRecreate);
        let mut less_cpu = limited.to_requested();
        less_cpu.container.resources.cpu_nanos =
            Some(crate::CpuNanos::try_from(250_000_000).unwrap());
        assert_eq!(compare_specs(&limited, &less_cpu), SpecChange::NeedsUpdate);
        let mut unlimited = limited.clone();
        unlimited.container.resources.memory_bytes = None;
        let first_limit = limited.to_requested();
        assert_eq!(
            compare_specs(&unlimited, &first_limit),
            SpecChange::NeedsRecreate,
            "a first memory limit may sit below what the Container already holds"
        );
    }
}
