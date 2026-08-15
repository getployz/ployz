use super::support::*;
#[test]
fn spec_comparison_distinguishes_mutable_resources_from_recreation() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');
    let current = container('b', '1', &requested, &current_service_id).resolved_spec;

    assert_eq!(compare_specs(&current, &requested), SpecChange::UpToDate);

    let mut resources_changed = requested.clone();
    resources_changed.container.resources.memory_bytes = Some(512 * 1024 * 1024);
    assert_eq!(
        compare_specs(&current, &resources_changed),
        SpecChange::NeedsUpdate
    );

    let mut image_changed = requested.clone();
    image_changed.container.image = "ghcr.io/getployz/api:2".into();
    assert_eq!(
        compare_specs(&current, &image_changed),
        SpecChange::NeedsRecreate
    );

    let mut always_pull = requested;
    always_pull.container.pull_policy = PullPolicy::Always;
    assert_eq!(
        compare_specs(&current, &always_pull),
        SpecChange::NeedsRecreate
    );
}

#[test]
fn spec_comparison_covers_upstream_immutable_field_families() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current = container('b', '1', &requested, &service_id('a')).resolved_spec;
    let mut changes = Vec::new();

    let mut changed = requested.clone();
    changed.container.cap_add.push("NET_ADMIN".into());
    changes.push(("cap_add", changed));
    let mut changed = requested.clone();
    changed.container.cap_drop.push("ALL".into());
    changes.push(("cap_drop", changed));
    let mut changed = requested.clone();
    changed.container.pid_mode = Some(PidMode::Host);
    changes.push(("pid", changed));
    let mut changed = requested.clone();
    changed.container.tty = true;
    changed.container.open_stdin = true;
    changes.push(("tty", changed));
    let mut changed = requested.clone();
    changed.container.log_driver = Some(LogDriver {
        name: "json-file".into(),
        options: Default::default(),
    });
    changes.push(("log driver", changed));
    let mut changed = requested.clone();
    changed.container.privileged = true;
    changes.push(("privileged", changed));
    let mut changed = requested.clone();
    changed
        .container
        .sysctls
        .insert("net.ipv4.ip_forward".into(), "1".into());
    changes.push(("sysctls", changed));
    let mut changed = requested.clone();
    changed.container.user = Some("1000:1000".into());
    changes.push(("user", changed));
    let mut changed = requested.clone();
    changed.placement.machines = vec![MachineSelector::parse("first").unwrap()];
    changes.push(("placement", changed));
    let mut changed = requested.clone();
    changed.container.resources.devices.push(DeviceMapping {
        machine_path: MachinePath::parse("/dev/fuse").unwrap(),
        container_path: ContainerPath::parse("/dev/fuse").unwrap(),
        cgroup_permissions: "rwm".into(),
    });
    changes.push(("device", changed));
    let mut changed = requested.clone();
    changed
        .container
        .resources
        .device_reservations
        .push(DeviceReservation {
            driver: Some("nvidia".into()),
            count: Some(1),
            device_ids: Vec::new(),
            capabilities: vec![vec!["gpu".into()]],
            options: Default::default(),
        });
    changes.push(("device reservation", changed));
    let mut changed = requested.clone();
    changed.container.resources.ulimits.insert(
        "nofile".into(),
        Ulimit {
            soft: 1024,
            hard: 2048,
        },
    );
    changes.push(("ulimit", changed));
    let mut changed = requested.clone();
    changed.container.restart = RestartPolicy::no();
    changes.push(("restart", changed));

    for (name, changed) in changes {
        assert_eq!(
            compare_specs(&current, &changed),
            SpecChange::NeedsRecreate,
            "{name}"
        );
    }
}

#[test]
fn spec_comparison_handles_resource_precedence_and_unordered_volumes() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "first");
    add_named_volume(&mut requested, "second");
    let current = container('b', '1', &requested, &service_id('a')).resolved_spec;

    let mut reordered = requested.clone();
    reordered.volumes.reverse();
    reordered.mounts.reverse();
    assert_eq!(compare_specs(&current, &reordered), SpecChange::UpToDate);

    let mut mutable = requested.clone();
    mutable.container.resources.cpu_nanos = Some(1_000_000_000);
    assert_eq!(compare_specs(&current, &mutable), SpecChange::NeedsUpdate);

    mutable.container.cap_add.push("NET_ADMIN".into());
    assert_eq!(compare_specs(&current, &mutable), SpecChange::NeedsRecreate);
}
