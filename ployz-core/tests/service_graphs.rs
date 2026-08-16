use std::collections::BTreeMap;

use ployz_core::{
    ConfigMount, ConfigSpec, ContainerPath, DockerVolumeName, PullPolicy, RequestedServiceSpec,
    ServiceConfigGraph, ServiceConfigGraphError, ServiceContainerSpec, ServiceMode, ServiceMount,
    ServiceName, ServiceVolume, ServiceVolumeGraph, ServiceVolumeGraphError,
    ServiceVolumeReference, VolumeSource,
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
fn existing_spec_fields_still_accept_unvalidated_parallel_arrays() {
    let requested = RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode: ServiceMode::Global,
        container: ServiceContainerSpec {
            image: "ghcr.io/getployz/api:1".into(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            environment: BTreeMap::new(),
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
            config_mounts: vec![config_mount("missing")],
            restart: Default::default(),
        },
        placement: Default::default(),
        ports: Vec::new(),
        volumes: Vec::new(),
        mounts: vec![mount("missing", "/missing")],
        configs: Vec::new(),
        pre_deploy: None,
        caddy_config: None,
        update: Default::default(),
    };

    let json = serde_json::to_value(&requested).unwrap();
    let decoded = serde_json::from_value::<RequestedServiceSpec>(json).unwrap();
    assert_eq!(decoded.volumes, requested.volumes);
    assert_eq!(decoded.mounts, requested.mounts);
    assert_eq!(decoded.configs, requested.configs);
    assert_eq!(
        decoded.container.config_mounts,
        requested.container.config_mounts
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
    *value.get_mut(key).expect("serialized graph has the field") = serde_json::json!([]);
}

fn named_volume(volume: &str, name: &str) -> ServiceVolume {
    ServiceVolume {
        reference: reference(volume),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse(name).unwrap(),
            external: false,
            driver: None,
            labels: BTreeMap::new(),
            no_copy: false,
            subpath: None,
        },
    }
}

fn mount(volume: &str, target: &str) -> ServiceMount {
    ServiceMount {
        volume: reference(volume),
        target: ContainerPath::parse(target).unwrap(),
        read_only: false,
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
