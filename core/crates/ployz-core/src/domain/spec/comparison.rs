//! Runtime planning impact of a requested spec against an observed one.

use super::*;

/// Return the runtime action needed to reconcile the requested serving shape.
#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    if requested.container.pull_policy == PullPolicy::Always
        || current.serving_shape() != requested.serving_shape()
    {
        SpecChange::NeedsRecreate
    } else {
        resource_change(&current.container.resources, &requested.container.resources)
    }
}

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
    {
        return SpecChange::NeedsRecreate;
    }
    if current_cpu_nanos != requested_cpu_nanos
        || current_memory_bytes != requested_memory_bytes
        || current_memory_reservation_bytes != requested_memory_reservation_bytes
        || current_shared_memory_bytes != requested_shared_memory_bytes
    {
        return SpecChange::NeedsUpdate;
    }
    SpecChange::UpToDate
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_edits_recreate_and_resource_edits_update() {
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
        candidate.container.resources.memory_bytes =
            Some(crate::ByteQuantity::try_from(64).unwrap());
        assert_eq!(compare_specs(&current, &candidate), SpecChange::NeedsUpdate);
    }
}
