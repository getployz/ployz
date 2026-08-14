use ployz_core::{
    ContainerResources, PullPolicy, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceContainerSpec, SpecChange,
};

#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    // TODO(UT-006, UT-011, UT-062 through UT-071, UT-080, UT-081, UT-091, UT-092):
    // ingress, placement, unused-volume, bind-option/default, and mutable-resource changes still
    // recreate until the Machine API supports the narrower in-place updates retained by the
    // baseline TODOs.
    if requested.container.pull_policy == PullPolicy::Always
        || immutable_service_fields_changed(current, requested)
    {
        return SpecChange::NeedsRecreate;
    }
    resource_change(&current.container.resources, &requested.container.resources)
}

fn immutable_service_fields_changed(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> bool {
    let ResolvedServiceSpec {
        service_id: _,
        name: current_name,
        mode: current_mode,
        container: current_container,
        placement: current_placement,
        ports: current_ports,
        volumes: current_volumes,
        mounts: current_mounts,
        configs: current_configs,
        pre_deploy: _,
        caddy_config: current_caddy_config,
        update: _,
    } = current;
    let RequestedServiceSpec {
        name: requested_name,
        mode: requested_mode,
        container: requested_container,
        placement: requested_placement,
        ports: requested_ports,
        volumes: requested_volumes,
        mounts: requested_mounts,
        configs: requested_configs,
        pre_deploy: _,
        caddy_config: requested_caddy_config,
        update: _,
    } = requested;

    current_name != requested_name
        || std::mem::discriminant(current_mode) != std::mem::discriminant(requested_mode)
        || immutable_container_fields_changed(current_container, requested_container)
        || current_placement != requested_placement
        || !same_multiset(current_ports, requested_ports)
        || !same_multiset(current_volumes, requested_volumes)
        || !same_multiset(current_mounts, requested_mounts)
        || !same_multiset(current_configs, requested_configs)
        || current_caddy_config.as_deref().map(str::trim)
            != requested_caddy_config.as_deref().map(str::trim)
}

fn immutable_container_fields_changed(
    current: &ServiceContainerSpec,
    requested: &ServiceContainerSpec,
) -> bool {
    let ServiceContainerSpec {
        image: current_image,
        command: current_command,
        entrypoint: current_entrypoint,
        environment: current_environment,
        cap_add: current_cap_add,
        cap_drop: current_cap_drop,
        healthcheck: current_healthcheck,
        pull_policy: _,
        init: current_init,
        user: current_user,
        working_directory: current_working_directory,
        tty: current_tty,
        open_stdin: current_open_stdin,
        privileged: current_privileged,
        pid_mode: current_pid_mode,
        log_driver: current_log_driver,
        resources: _,
        stop_grace_period_millis: current_stop_grace_period_millis,
        sysctls: current_sysctls,
        config_mounts: current_config_mounts,
    } = current;
    let ServiceContainerSpec {
        image: requested_image,
        command: requested_command,
        entrypoint: requested_entrypoint,
        environment: requested_environment,
        cap_add: requested_cap_add,
        cap_drop: requested_cap_drop,
        healthcheck: requested_healthcheck,
        pull_policy: _,
        init: requested_init,
        user: requested_user,
        working_directory: requested_working_directory,
        tty: requested_tty,
        open_stdin: requested_open_stdin,
        privileged: requested_privileged,
        pid_mode: requested_pid_mode,
        log_driver: requested_log_driver,
        resources: _,
        stop_grace_period_millis: requested_stop_grace_period_millis,
        sysctls: requested_sysctls,
        config_mounts: requested_config_mounts,
    } = requested;

    current_image != requested_image
        || current_command != requested_command
        || current_entrypoint != requested_entrypoint
        || current_environment != requested_environment
        || !same_multiset(current_cap_add, requested_cap_add)
        || !same_multiset(current_cap_drop, requested_cap_drop)
        || current_healthcheck != requested_healthcheck
        || current_init != requested_init
        || current_user != requested_user
        || current_working_directory != requested_working_directory
        || current_tty != requested_tty
        || current_open_stdin != requested_open_stdin
        || current_privileged != requested_privileged
        || current_pid_mode != requested_pid_mode
        || current_log_driver != requested_log_driver
        || current_stop_grace_period_millis != requested_stop_grace_period_millis
        || current_sysctls != requested_sysctls
        || !same_multiset(current_config_mounts, requested_config_mounts)
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

fn same_multiset<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    // ponytail: O(n²) avoids requiring Ord or cloning domain values; sort if specs become large.
    let mut matched = vec![false; right.len()];
    left.iter().all(|item| {
        right
            .iter()
            .zip(&mut matched)
            .find(|(candidate, used)| !**used && item == *candidate)
            .is_some_and(|(_, used)| {
                *used = true;
                true
            })
    })
}
