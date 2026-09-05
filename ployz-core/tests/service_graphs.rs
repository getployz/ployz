use std::collections::BTreeMap;

use ployz_core::{
    ConfigMount, ConfigSpec, ContainerPath, DockerVolumeName, PullPolicy, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, ServiceConfigGraph, ServiceConfigGraphError,
    ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceSpecGraphError,
    ServiceVolume, ServiceVolumeGraph, ServiceVolumeGraphError, ServiceVolumeReference,
    UpdateOrder, VolumeDriver,
};

#[test]
fn volume_graph_rejects_duplicate_references_and_dangling_mounts() {
    let data = reference("data");
    let duplicate = ServiceVolumeGraph::parse(
        vec![
            named_volume("data", "disk-a"),
            named_volume("data", "disk-b"),
        ],
        vec![],
    );
    assert_eq!(
        duplicate,
        Err(ServiceVolumeGraphError::DuplicateVolumeReference {
            reference: data.clone(),
        })
    );

    let dangling = ServiceVolumeGraph::parse(vec![], vec![mount("missing", "/missing")]);
    assert_eq!(
        dangling,
        Err(ServiceVolumeGraphError::UnknownVolumeReference {
            reference: reference("missing"),
        })
    );
}

#[test]
fn volume_graph_rejects_incompatible_docker_volume_aliases() {
    let ordinary = named_volume("data", "shared");
    let external = ServiceVolume {
        reference: reference("data-alias"),
        source: ployz_core::RawVolumeSource::External {
            name: DockerVolumeName::parse("shared").unwrap(),
        }
        .admit()
        .expect("valid volume declaration"),
    };
    let error = ServiceVolumeGraph::parse(vec![ordinary, external], vec![])
        .expect_err("incompatible aliases used one Docker Volume");
    assert_eq!(
        error,
        ServiceVolumeGraphError::IncompatibleVolumeAliases {
            name: DockerVolumeName::parse("shared").unwrap(),
        }
    );
}

#[test]
fn config_graph_rejects_duplicate_names_and_dangling_mounts() {
    let duplicate = ServiceConfigGraph::parse(
        vec![config("settings", b"a"), config("settings", b"b")],
        vec![],
    );
    assert_eq!(
        duplicate,
        Err(ServiceConfigGraphError::DuplicateConfigName {
            name: "settings".into(),
        })
    );

    let dangling = ServiceConfigGraph::parse(vec![], vec![config_mount("missing")]);
    assert_eq!(
        dangling,
        Err(ServiceConfigGraphError::UnknownConfigName {
            name: "missing".into(),
        })
    );
}

#[test]
fn legal_unused_definitions_repeated_mounts_and_volume_aliases_remain_representable() {
    let unused = named_volume("logs", "logs");
    let data = named_volume("data", "shared");
    let alias = named_volume("data-alias", "shared");
    let first = mount("data", "/var/data");
    let repeated = mount("data", "/var/data");
    let alias_mount = mount("data-alias", "/alias");
    let graph = ServiceVolumeGraph::parse(
        vec![data.clone(), alias.clone(), unused.clone()],
        vec![first.clone(), repeated.clone(), alias_mount.clone()],
    )
    .unwrap();

    assert_eq!(graph.volume_for(&first), &data);
    assert_eq!(graph.volume_for(&alias_mount), &alias);
    assert_eq!(graph.volumes(), &[data, alias, unused]);
    assert_eq!(graph.mounts(), &[first, repeated, alias_mount]);

    let unused_config = config("unused", b"keep");
    let settings = config("settings", b"port = 8080");
    let first_mount = config_mount("settings");
    let repeated_mount = config_mount("settings");
    let configs = ServiceConfigGraph::parse(
        vec![settings.clone(), unused_config.clone()],
        vec![first_mount.clone(), repeated_mount.clone()],
    )
    .unwrap();

    assert_eq!(configs.config_for(&first_mount), &settings);
    assert_eq!(configs.config_for(&repeated_mount), &settings);
    assert_eq!(configs.configs(), &[settings, unused_config]);
    assert_eq!(configs.mounts(), &[first_mount, repeated_mount]);
}

#[test]
fn graph_deserialization_runs_the_same_validation() {
    let valid_volumes = ServiceVolumeGraph::parse(
        vec![named_volume("data", "data")],
        vec![mount("data", "/var/data")],
    )
    .unwrap();
    let volumes_json = serde_json::to_value(&valid_volumes).unwrap();
    assert_eq!(
        serde_json::from_value::<ServiceVolumeGraph>(volumes_json.clone()).unwrap(),
        valid_volumes
    );

    let mut duplicate_volumes = volumes_json.clone();
    duplicate_first_item(&mut duplicate_volumes, "volumes");
    assert_eq!(
        serde_json::from_value::<ServiceVolumeGraph>(duplicate_volumes)
            .unwrap_err()
            .to_string(),
        ServiceVolumeGraphError::DuplicateVolumeReference {
            reference: reference("data"),
        }
        .to_string()
    );

    let mut dangling_volume = volumes_json;
    clear_array(&mut dangling_volume, "volumes");
    assert_eq!(
        serde_json::from_value::<ServiceVolumeGraph>(dangling_volume)
            .unwrap_err()
            .to_string(),
        ServiceVolumeGraphError::UnknownVolumeReference {
            reference: reference("data"),
        }
        .to_string()
    );

    let valid_configs = ServiceConfigGraph::parse(
        vec![config("settings", b"x")],
        vec![config_mount("settings")],
    )
    .unwrap();
    let configs_json = serde_json::to_value(&valid_configs).unwrap();
    assert_eq!(
        serde_json::from_value::<ServiceConfigGraph>(configs_json.clone()).unwrap(),
        valid_configs
    );

    let mut duplicate_configs = configs_json.clone();
    duplicate_first_item(&mut duplicate_configs, "configs");
    assert_eq!(
        serde_json::from_value::<ServiceConfigGraph>(duplicate_configs)
            .unwrap_err()
            .to_string(),
        ServiceConfigGraphError::DuplicateConfigName {
            name: "settings".into(),
        }
        .to_string()
    );

    let mut dangling_config = configs_json;
    clear_array(&mut dangling_config, "configs");
    assert_eq!(
        serde_json::from_value::<ServiceConfigGraph>(dangling_config)
            .unwrap_err()
            .to_string(),
        ServiceConfigGraphError::UnknownConfigName {
            name: "settings".into(),
        }
        .to_string()
    );
}

#[test]
fn requested_and_resolved_conversions_preserve_graph_invariants() {
    let mut requested = requested_with_graphs(
        vec![
            named_volume("data", "shared"),
            named_volume("data-alias", "shared"),
            named_volume("logs", "logs"),
        ],
        vec![
            mount("data", "/var/data"),
            mount("data", "/var/data-copy"),
            mount("data-alias", "/alias"),
        ],
        vec![
            config("settings", b"port = 8080"),
            config("unused", b"keep"),
        ],
        vec![
            config_mount("settings"),
            ConfigMount {
                target: Some(ContainerPath::parse("/settings-copy").unwrap()),
                ..config_mount("settings")
            },
        ],
    );
    assert!(
        requested
            .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
            .is_err()
    );
    requested
        .set_volume_graph(
            requested
                .volume_graph()
                .clone()
                .scope_to_project(&ployz_core::ProjectName::parse("shop").unwrap())
                .unwrap(),
        )
        .unwrap();
    let volumes = requested.volume_graph().clone();
    let configs = requested.config_graph().clone();
    let update = ResolvedUpdateConfig {
        order: UpdateOrder::StopFirst,
        monitor_millis: Some(1_000),
    };

    let resolved = requested
        .to_resolved(ServiceId::random(), update.clone())
        .expect("volume graph is scoped");
    assert_eq!(resolved.volume_graph().to_requested(), volumes);
    assert_eq!(resolved.config_graph(), &configs);
    assert_eq!(resolved.update, update);

    let back = resolved.to_requested();
    assert_eq!(back.volume_graph(), &volumes);
    assert_eq!(back.config_graph(), &configs);
    assert_eq!(back.update.order, Some(UpdateOrder::StopFirst));
    assert_eq!(back.update.monitor_millis, Some(1_000));
}

#[test]
fn persisted_and_rpc_decoded_specs_validate_graph_invariants_on_entry() {
    let mut valid = requested_with_graphs(
        vec![named_volume("data", "data")],
        vec![mount("data", "/var/data")],
        vec![config("settings", b"x")],
        vec![config_mount("settings")],
    );
    let json = serde_json::to_value(&valid).unwrap();
    let decoded = serde_json::from_value::<RequestedServiceSpec>(json.clone()).unwrap();
    assert_eq!(decoded.volume_graph(), valid.volume_graph());
    assert_eq!(decoded.config_graph(), valid.config_graph());

    let mut dangling_volume = json.clone();
    set_field(
        &mut dangling_volume,
        "mounts",
        serde_json::json!([{"volume":"missing","target":"/missing"}]),
    );
    assert_eq!(
        serde_json::from_value::<RequestedServiceSpec>(dangling_volume)
            .unwrap_err()
            .to_string(),
        ServiceSpecGraphError::Volume(ServiceVolumeGraphError::UnknownVolumeReference {
            reference: reference("missing"),
        })
        .to_string()
    );

    let mut dangling_config = json;
    *dangling_config
        .get_mut("container")
        .and_then(|container| container.get_mut("config_mounts"))
        .expect("serialized spec has config mounts") =
        serde_json::json!([{"config_name":"missing"}]);
    assert_eq!(
        serde_json::from_value::<RequestedServiceSpec>(dangling_config)
            .unwrap_err()
            .to_string(),
        ServiceSpecGraphError::Config(ServiceConfigGraphError::UnknownConfigName {
            name: "missing".into(),
        })
        .to_string()
    );

    valid
        .set_volume_graph(
            valid
                .volume_graph()
                .clone()
                .scope_to_project(&ployz_core::ProjectName::parse("shop").unwrap())
                .unwrap(),
        )
        .unwrap();
    let resolved = valid
        .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
        .expect("volume graph is scoped");
    let mut resolved_json = serde_json::to_value(&resolved).unwrap();
    set_field(&mut resolved_json, "volumes", serde_json::json!([]));
    assert_eq!(
        serde_json::from_value::<ResolvedServiceSpec>(resolved_json)
            .unwrap_err()
            .to_string(),
        ServiceSpecGraphError::Volume(ServiceVolumeGraphError::UnknownVolumeReference {
            reference: reference("data"),
        })
        .to_string()
    );
}

fn duplicate_first_item(value: &mut serde_json::Value, key: &str) {
    let first = value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .expect("serialized graph has an item");
    value
        .get_mut(key)
        .and_then(serde_json::Value::as_array_mut)
        .expect("serialized graph has an array")
        .push(first);
}

fn clear_array(value: &mut serde_json::Value, key: &str) {
    set_field(value, key, serde_json::json!([]));
}

fn set_field(value: &mut serde_json::Value, key: &str, replacement: serde_json::Value) {
    *value.get_mut(key).expect("serialized spec has the field") = replacement;
}

fn requested_with_graphs(
    volumes: Vec<ServiceVolume>,
    mounts: Vec<ServiceMount>,
    configs: Vec<ConfigSpec>,
    config_mounts: Vec<ConfigMount>,
) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode: ServiceMode::Global,
        container: ServiceContainerSpec {
            image: "ghcr.io/getployz/api:1".into(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            environment: BTreeMap::new(),
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
            resources: Default::default(),
            stop_timeout_secs: None,
            sysctls: BTreeMap::new(),
            restart: Default::default(),
        },
        placement: Default::default(),
        ports: Vec::new(),
        mount_graph: ployz_core::ServiceMountGraph::new(
            ServiceVolumeGraph::parse(volumes, mounts).unwrap(),
            ServiceConfigGraph::parse(configs, config_mounts).unwrap(),
        )
        .unwrap(),
        pre_deploy: None,
        ingress_proxy_fragment: None,
        update: Default::default(),
    }
}

fn named_volume(volume: &str, name: &str) -> ServiceVolume {
    ServiceVolume {
        reference: reference(volume),
        source: ployz_core::RawVolumeSource::Ordinary {
            name: DockerVolumeName::parse(name).unwrap(),
            driver: VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
        }
        .admit()
        .expect("valid volume declaration"),
    }
}

fn mount(volume: &str, target: &str) -> ServiceMount {
    ServiceMount {
        volume: reference(volume),
        target: ContainerPath::parse(target).unwrap(),
        read_only: false,
        no_copy: false,
        subpath: None,
    }
}

fn config(name: &str, content: &[u8]) -> ConfigSpec {
    ConfigSpec {
        name: name.into(),
        content: content.to_vec(),
    }
}

fn config_mount(name: &str) -> ConfigMount {
    ConfigMount {
        config_name: name.into(),
        target: None,
        uid: None,
        gid: None,
        mode: None,
    }
}

fn reference(value: &str) -> ServiceVolumeReference {
    ServiceVolumeReference::parse(value).unwrap()
}

#[test]
fn spec_rejects_colliding_effective_mount_destinations() {
    let valid = requested_with_graphs(
        vec![named_volume("data", "data")],
        vec![mount("data", "/data")],
        vec![config("data", b"x")],
        vec![],
    );
    for target in [None, Some("/data"), Some("/x/../data/")] {
        let mut wire = serde_json::to_value(&valid).unwrap();
        *wire
            .get_mut("container")
            .and_then(|container| container.get_mut("config_mounts"))
            .unwrap() = serde_json::json!([{"config_name":"data", "target": target}]);
        assert!(serde_json::from_value::<RequestedServiceSpec>(wire).is_err());
    }
}

#[test]
fn combined_mount_admission_cleans_targets_and_preserves_distinct_mounts() {
    let graph = ployz_core::ServiceMountGraph::new(
        ServiceVolumeGraph::parse(
            vec![named_volume("data", "data")],
            vec![mount("data", "/x/../data/"), mount("data", "/data/child")],
        )
        .unwrap(),
        ServiceConfigGraph::parse(
            vec![config("settings", b"x")],
            vec![
                config_mount("settings"),
                ConfigMount {
                    target: Some(ContainerPath::parse("/etc//./settings/").unwrap()),
                    uid: Some(42),
                    ..config_mount("settings")
                },
            ],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        graph
            .volume_graph()
            .mounts()
            .first()
            .unwrap()
            .target
            .as_str(),
        "/data"
    );
    assert_eq!(
        graph
            .volume_graph()
            .mounts()
            .get(1)
            .unwrap()
            .target
            .as_str(),
        "/data/child"
    );
    assert_eq!(
        graph
            .config_graph()
            .mounts()
            .first()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .as_str(),
        "/settings"
    );
    assert_eq!(
        graph
            .config_graph()
            .mounts()
            .get(1)
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .as_str(),
        "/etc/settings"
    );
    assert_eq!(graph.config_graph().mounts().get(1).unwrap().uid, Some(42));

    for alias in [
        "/data",
        "/data/",
        "/./data",
        "/data/.",
        "//data",
        "/x/../data",
        "/../../data",
    ] {
        let error = ployz_core::ServiceMountGraph::new(
            ServiceVolumeGraph::parse(
                vec![named_volume("data", "data")],
                vec![mount("data", "/data"), mount("data", alias)],
            )
            .unwrap(),
            ServiceConfigGraph::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ServiceSpecGraphError::DuplicateMountTarget {
                target: ContainerPath::parse("/data").unwrap()
            }
        );
    }
    for root in ["/", "/./", "/data/..", "/../../"] {
        assert_eq!(
            ployz_core::ServiceMountGraph::new(
                ServiceVolumeGraph::parse(
                    vec![named_volume("data", "data")],
                    vec![mount("data", root)]
                )
                .unwrap(),
                ServiceConfigGraph::default(),
            ),
            Err(ServiceSpecGraphError::RootMountTarget)
        );
    }
    assert!(
        ContainerPath::parse("/").is_ok(),
        "root remains a legal working directory"
    );
}

#[test]
fn mount_admission_is_atomic_and_runs_on_resolved_observation_import() {
    let mut spec = requested_with_graphs(
        vec![named_volume("data", "data")],
        vec![mount("data", "/data")],
        vec![config("data", b"x")],
        vec![],
    );
    let before = spec.clone();
    assert!(
        spec.set_config_graph(
            ServiceConfigGraph::parse(vec![config("data", b"x")], vec![config_mount("data")])
                .unwrap()
        )
        .is_err()
    );
    assert_eq!(spec, before, "failed replacement preserves admitted mounts");
    spec.mount_graph = spec
        .mount_graph
        .scope_to_project(&ployz_core::ProjectName::parse("shop").unwrap())
        .unwrap();
    let resolved = spec
        .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
        .unwrap();
    let mut wire = serde_json::to_value(&resolved).unwrap();
    *wire
        .get_mut("container")
        .and_then(|container| container.get_mut("config_mounts"))
        .unwrap() = serde_json::json!([{"config_name":"data"}]);
    assert!(serde_json::from_value::<ResolvedServiceSpec>(wire).is_err());
    let back = resolved.to_requested();
    assert_eq!(back.mount_graph, spec.mount_graph);

    let configs = ServiceConfigGraph::parse(
        vec![config("data", b"x")],
        vec![
            config_mount("data"),
            ConfigMount {
                target: Some(ContainerPath::parse("/data/").unwrap()),
                ..config_mount("data")
            },
        ],
    )
    .unwrap();
    assert!(ployz_core::ServiceMountGraph::new(ServiceVolumeGraph::default(), configs).is_err());
}
