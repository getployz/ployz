use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use ployz::{
    compose::parse_normalized,
    deploy::{
        DeployIntent, DeployOperation, DeploySnapshot, EliminatingConstraint, IngressContext,
        PlanError, PlanOptions, VolumeSnapshot, preview_deploy,
    },
};
use ployz_core::{
    DockerVolumeId, DockerVolumeName, HostBind, HttpProtocol, IngressHostname,
    IngressProxyFragment, MANAGED_LABEL, MachineFailure, MachineStorageObservation,
    PROJECT_NAME_LABEL, PortPublication, ProjectName, RestartPolicy, RpcError, RpcErrorCode,
    ServiceMode, TransportProtocol, UpdateOrder, VolumeObservationFailure, VolumeSource,
};

#[path = "compose/support.rs"]
mod support;
use support::*;

#[test]
fn normalized_surface_reaches_requested_specs() {
    let directory = TestDir::new();
    fs::write(
        directory.path.join("Caddyfile"),
        "app.example { reverse_proxy :80 }\n",
    )
    .unwrap();
    fs::write(directory.path.join("app.conf"), "enabled=true\n").unwrap();
    let yaml = r#"
name: demo
x-context: production
services:
  db:
    image: postgres:17
    networks: {default: null}
  api:
    image: registry.example/api
    build: {context: ./api, dockerfile: Dockerfile.release}
    command: [serve, --port, "8080"]
    entrypoint: [/entrypoint]
    environment: {BOOL: "true", EMPTY: "", OMITTED: null}
    cap_add: [NET_ADMIN]
    cap_drop: [ALL]
    cpus: 0.5
    mem_limit: "104857600"
    mem_reservation: "52428800"
    shm_size: "268435456"
    devices:
      - {source: /dev/sda, target: /dev/xvda, permissions: rw}
      - vendor.example/device=one
      - vendor.example/device=two
    gpus: [{count: -1}]
    ulimits:
      nofile: {soft: 20000, hard: 40000}
      nproc: 65535
    healthcheck:
      test: [CMD, curl, -f, http://localhost]
      interval: 1m30s
      timeout: 10s
      retries: 5
      start_period: 15s
      start_interval: 2s
    init: true
    user: "1000:1000"
    working_dir: /app
    tty: true
    stdin_open: true
    privileged: true
    pid: host
    restart: on-failure:5
    stop_grace_period: 30s
    sysctls: {net.ipv4.ip_forward: "1"}
    deploy:
      replicas: 3
      update_config: {order: stop-first, monitor: 45s}
    depends_on: {db: {condition: service_started}}
    x-machines: "machine-1, machine-2"
    x-ports: [api.example.com:8443:8080/https, 5000:3000/tcp@host]
    x-pre_deploy:
      command: [sh, -c, migrate]
      environment: {DB_HOST: db}
      privileged: true
      timeout: 2m30s
    volumes:
      - {type: bind, source: /srv/api, target: /host, bind: {create_host_path: true, propagation: rprivate, recursive: disabled}}
      - {type: volume, source: data, target: /data, volume: {nocopy: true, subpath: current}}
      - {type: tmpfs, target: /tmp, tmpfs: {size: "10485760", mode: 1770}}
    configs:
      - {source: inline, target: /etc/inline, uid: "1000", gid: "1001", mode: "0640"}
      - {source: file, target: /etc/file}
  caddy:
    image: caddy:2
    x-caddy: Caddyfile
    x-ports: [8080:8080/tcp@host]
volumes:
  data: {name: demo_data, driver: local, driver_opts: {type: tmpfs}, labels: {tier: app}}
configs:
  inline: {content: "hello"}
  file: {file: app.conf}
"#;

    let project = parse_normalized(yaml, &directory.path).unwrap();
    assert_eq!(project.context.as_deref(), Some("production"));
    assert_eq!(project.selected_context(None, None), Some("production"));
    assert_eq!(
        project.selected_context(Some("explicit"), None),
        Some("explicit")
    );
    assert_eq!(project.selected_context(None, Some("ssh://machine")), None);
    assert_eq!(
        project
            .services
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["api", "caddy", "db"]
    );
    assert_eq!(
        project
            .dependency_order()
            .unwrap()
            .into_iter()
            .map(|service| service.name.as_str())
            .collect::<Vec<_>>(),
        ["db", "api", "caddy"]
    );

    let api = service(&project, "api");
    assert_eq!(api.container.command, ["serve", "--port", "8080"]);
    assert_eq!(api.container.entrypoint, ["/entrypoint"]);
    assert_eq!(
        api.container.environment,
        BTreeMap::from([
            ("BOOL".into(), "true".into()),
            ("EMPTY".into(), String::new())
        ])
    );
    assert_eq!(api.container.resources.cpu_nanos, Some(500_000_000));
    assert_eq!(api.container.resources.memory_bytes, Some(104_857_600));
    assert_eq!(
        api.container.resources.memory_reservation_bytes,
        Some(52_428_800)
    );
    assert_eq!(
        api.container.resources.shared_memory_bytes,
        Some(268_435_456)
    );
    assert_eq!(api.container.resources.devices.len(), 1);
    assert_eq!(api.container.resources.device_reservations.len(), 2);
    assert_eq!(
        api.container
            .resources
            .device_reservations
            .first()
            .unwrap()
            .device_ids,
        ["vendor.example/device=one", "vendor.example/device=two"]
    );
    assert_eq!(
        api.container.resources.ulimits.get("nproc").unwrap().hard,
        65_535
    );
    assert_eq!(
        api.container
            .healthcheck
            .as_ref()
            .and_then(ployz_core::HealthcheckSpec::as_configured)
            .and_then(|healthcheck| healthcheck.interval_millis),
        Some(90_000)
    );
    assert_eq!(api.container.log_driver.as_ref().unwrap().name, "local");
    assert_eq!(api.container.stop_timeout_secs, Some(30));
    assert_eq!(api.container.pid_mode, Some(ployz_core::PidMode::Host));
    assert_eq!(
        api.container.restart,
        RestartPolicy::OnFailure {
            maximum_retry_count: Some(5),
        }
    );
    assert_eq!(
        api.placement.machines.first().unwrap().as_str(),
        "machine-1"
    );
    assert_eq!(api.placement.machines.get(1).unwrap().as_str(), "machine-2");
    assert_eq!(api.update.order, Some(UpdateOrder::StopFirst));
    assert_eq!(api.update.monitor_millis, Some(45_000));
    assert!(matches!(api.mode, ServiceMode::Replicated { replicas } if replicas.get() == 3));
    assert!(matches!(
        api.ports.first().unwrap(),
        PortPublication::Ingress {
            hostname,
            load_balancer_port,
            container_port,
            http_protocol: HttpProtocol::Https,
        } if *hostname == IngressHostname::explicit("api.example.com").unwrap()
            && load_balancer_port.get() == 8443
            && container_port.get() == 8080
    ));
    assert!(matches!(
        api.ports.get(1).unwrap(),
        PortPublication::Host {
            bind: HostBind::All,
            published_port,
            container_port,
            transport_protocol: TransportProtocol::Tcp,
        } if published_port.get() == 5000 && container_port.get() == 3000
    ));
    let pre_deploy = api.pre_deploy.as_ref().unwrap();
    assert_eq!(pre_deploy.command, ["sh", "-c", "migrate"]);
    assert_eq!(
        pre_deploy.environment,
        BTreeMap::from([("DB_HOST".into(), "db".into())])
    );
    assert_eq!(pre_deploy.privileged, Some(true));
    assert_eq!(pre_deploy.timeout_millis, Some(150_000));
    assert_eq!(api.configs().first().unwrap().content, b"enabled=true\n");
    assert_eq!(api.configs().get(1).unwrap().content, b"hello");
    assert_eq!(api.config_mounts().first().unwrap().mode, Some(0o640));
    assert_eq!(api.volumes().len(), 3);
    assert!(api.volumes().iter().any(|volume| matches!(
        &volume.source,
        VolumeSource::Bind {
            create_machine_path: true,
            propagation: Some(ployz_core::BindPropagation::Rprivate),
            recursive: Some(ployz_core::BindRecursive::Disabled),
            ..
        }
    )));
    assert!(api.volumes().iter().any(|volume| matches!(
        &volume.source,
        VolumeSource::Ordinary { name, .. } if name.as_str() == "data"
    )));
    assert!(api.container.image.starts_with("registry.example/api:"));
    assert_eq!(
        project
            .builds
            .get("api")
            .unwrap()
            .raw
            .as_mapping()
            .and_then(|map| {
                map.get(serde_norway::Value::String("dockerfile".into()))
                    .and_then(serde_norway::Value::as_str)
            }),
        Some("Dockerfile.release")
    );
    assert_eq!(
        service(&project, "caddy")
            .ingress_proxy_fragment
            .as_ref()
            .map(IngressProxyFragment::as_str),
        Some("app.example { reverse_proxy :80 }")
    );
}

#[test]
fn one_named_volume_can_have_different_options_per_mount() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app:1
    volumes:
      - {type: volume, source: data, target: /current, volume: {nocopy: true, subpath: current}}
      - {type: volume, source: data, target: /archive, volume: {subpath: archive}}
volumes: {data: {}}
"#,
        ".",
    )
    .unwrap();

    let app = service(&project, "app");
    assert_eq!(app.volumes().len(), 1);
    assert_eq!(
        app.mounts()
            .iter()
            .map(|mount| (
                mount.target.as_str(),
                mount.no_copy,
                mount.subpath.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            ("/current", true, Some("current")),
            ("/archive", false, Some("archive")),
        ]
    );
}

#[test]
fn compose_maps_disabled_and_sentinel_healthchecks() {
    for yaml in [
        "services: {app: {image: app, healthcheck: {disable: true}}}",
        "services: {app: {image: app, healthcheck: {disable: true, test: [CMD, true]}}}",
        "services: {app: {image: app, healthcheck: {test: [NONE]}}}",
        "services: {app: {image: app, healthcheck: {test: [NONE, CMD, true]}}}",
        "services: {app: {image: app, healthcheck: {test: NONE}}}",
    ] {
        let project = parse_normalized(yaml, ".").unwrap();
        assert_eq!(
            service(&project, "app").container.healthcheck,
            Some(ployz_core::HealthcheckSpec::Disabled),
            "{yaml}"
        );
    }
}

#[test]
fn compose_maps_native_container_metadata_without_changing_service_identity() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app:1
    hostname: Shared.Host
    labels:
      example.empty:
      example.equals: a=b
      example.number: 3
    extra_hosts:
      app: 192.0.2.10
      gateway: host-gateway
      ipv6:
        - "::1"
        - "2001:db8::1"
      bracketed: "[::1]"
    deploy: {replicas: 2}
  worker:
    image: worker:1
    labels: [example.bare, example.value=unchanged]
    extra_hosts: [app=198.51.100.2, legacy:203.0.113.4, "bracketed=[::1]"]
"#,
        ".",
    )
    .unwrap();

    let app = service(&project, "app");
    assert_eq!(app.name.as_str(), "app");
    assert!(matches!(app.mode, ServiceMode::Replicated { replicas } if replicas.get() == 2));
    assert_eq!(app.placement, Default::default());
    assert!(app.ports.is_empty());
    assert!(app.ingress_proxy_fragment.is_none());
    assert_eq!(
        app.container.hostname.as_ref().unwrap().as_str(),
        "Shared.Host"
    );
    assert_eq!(
        app.container.labels.as_map(),
        &BTreeMap::from([
            ("example.empty".into(), String::new()),
            ("example.equals".into(), "a=b".into()),
            ("example.number".into(), "3".into()),
        ])
    );
    assert_eq!(
        app.container
            .extra_hosts
            .iter()
            .map(ployz_core::ExtraHost::as_str)
            .collect::<Vec<_>>(),
        [
            "app:192.0.2.10",
            "gateway:host-gateway",
            "ipv6:::1",
            "ipv6:2001:db8::1",
            "bracketed:::1",
        ]
    );

    let worker = service(&project, "worker");
    assert_eq!(
        worker.container.labels.as_map(),
        &BTreeMap::from([
            ("example.bare".into(), String::new()),
            ("example.value".into(), "unchanged".into()),
        ])
    );
    assert_eq!(
        worker
            .container
            .extra_hosts
            .iter()
            .map(ployz_core::ExtraHost::as_str)
            .collect::<Vec<_>>(),
        ["app:198.51.100.2", "legacy:203.0.113.4", "bracketed:::1"]
    );
    assert!(project.warnings.is_empty());
}

#[test]
fn compose_rejects_reserved_labels_and_invalid_container_hostnames() {
    let label_error = parse_normalized(
        "services: {app: {image: app, labels: {ployz.future: mine}}}",
        ".",
    )
    .unwrap_err();
    assert_eq!(
        label_error.to_string(),
        "invalid normalized Compose project: service 'app': invalid container label key \"ployz.future\": outside the reserved 'ployz.*' management namespace"
    );

    for hostname in ["bad_name", "-leading", "trailing-", "two..dots"] {
        let error = parse_normalized(
            &format!("services: {{app: {{image: app, hostname: {hostname:?}}}}}"),
            ".",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "invalid normalized Compose project: service 'app': invalid container hostname {hostname:?}: a 1-64 character RFC 1123 hostname"
            )
        );
    }

    let too_long = format!("{}.b", "a".repeat(63));
    let error = parse_normalized(
        &format!("services: {{app: {{image: app, hostname: {too_long:?}}}}}"),
        ".",
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "invalid normalized Compose project: service 'app': invalid container hostname {too_long:?}: a 1-64 character RFC 1123 hostname"
        )
    );

    for entry in ["api:not-an-address", "=192.0.2.1"] {
        let error = parse_normalized(
            &format!("services: {{app: {{image: app, extra_hosts: [{entry:?}]}}}}"),
            ".",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("invalid normalized Compose project: invalid extra_hosts entry '{entry}'")
        );
    }
}

#[test]
fn classifier_keeps_the_warning_error_split_and_incomplete_boundary() {
    let warning_yaml = r#"
name: demo
services:
  app:
    image: app:1
    dns: 8.8.8.8
    dns_search: example.test
    extra_hosts: ["db:1.2.3.4"]
    labels: {a: b}
    links: [db]
    mem_swappiness: 10
    memswap_limit: 20
    networks: {custom: null}
    secrets: [{source: token}]
    storage_opt: {size: 1G}
    unsupported_future_field: accepted
    deploy:
      restart_policy: {condition: on-failure}
  db: {image: db:1, networks: {default: null}}
secrets:
  token: {file: /tmp/token}
"#;
    let project = parse_normalized(warning_yaml, ".").unwrap();
    assert_eq!(
        project.warnings,
        [
            "service 'app': unsupported feature 'dns'",
            "service 'app': unsupported feature 'dns_search'",
            "service 'app': unsupported feature 'links'",
            "service 'app': unsupported feature 'storage_opt'",
            "service 'app': unsupported feature 'mem_swappiness'",
            "service 'app': unsupported feature 'memswap_limit'",
            "service 'app': unsupported feature 'networks'",
            "service 'app': unsupported feature 'secrets'",
            "service 'app': unsupported feature 'deploy.restart_policy'",
        ]
    );
    assert!(
        !project
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported_future"))
    );

    let cases = [
        (
            "services: {app: {image: app, ports: ['80:80'], x-ports: ['80/http']}}",
            "both 'ports' and 'x-ports'",
        ),
        (
            "services: {app: {image: app, volumes: ['./config:/config']}}",
            "relative",
        ),
        (
            "services: {app: {image: app, ports: [{target: 80, published: '8000-9000'}]}}",
            "ranges",
        ),
        (
            "services: {job: {image: app}, app: {image: app, depends_on: {job: {condition: service_completed_successfully}}}}",
            "service_completed_successfully",
        ),
        (
            "services: {app: {image: app}}\nconfigs: {bad: {external: true}}",
            "external configs",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {external: true}}",
            "external secrets",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {driver: vault, driver_opts: {key: x}}}",
            "unsupported driver",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {file: a, environment: B}}",
            "must define exactly one of file or environment",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {file: a, x-command: printf x}}",
            "x-command cannot be combined with file or environment",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {x-command: printf x, driver: exec, driver_opts: {command: printf y}}}",
            "x-command cannot be combined with driver or driver_opts",
        ),
        (
            "services: {app: {image: app}}\nsecrets: {bad: {file: a, driver: exec, driver_opts: {command: printf x}}}",
            "a secret using a driver cannot also define file or environment",
        ),
        (
            "services: {app: {image: app, x-pre_deploy: {}}}",
            "required attribute 'command'",
        ),
        (
            "services: {app: {image: app, configs: [settings]}}\nconfigs: {settings: {content: ok}}",
            "short-syntax configs",
        ),
        (
            "services: {app: {image: app, cpus: not-a-number}}",
            "cpus must be numeric",
        ),
        (
            "services: {app: {image: app, healthcheck: {test: [CMD, true], interval: eventually}}}",
            "invalid duration",
        ),
        (
            "services: {app: {image: app, healthcheck: {interval: 10s}}}",
            "non-empty command",
        ),
        (
            "services: {app: {image: app, volumes: [{type: tmpfs, target: /tmp, tmpfs: {size: huge}}]}}",
            "tmpfs.size",
        ),
        (
            "services: {app: {image: app, volumes: [data:/data]}}\nvolumes: {data: {driver_opts: {type: tmpfs}}}",
            "driver_opts requires driver",
        ),
        (
            "services: {app: {image: app, volumes: [data:/data]}}\nvolumes: {data: {driver: ployz}}",
            "reserved 'ployz' driver",
        ),
    ];
    for (yaml, expected) in cases {
        let error = parse_normalized(yaml, ".").unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "{error:?} did not contain {expected:?}"
        );
    }
    let caddy_conflict = r#"
services:
  app:
    image: app
    x-caddy: |
      app.example {
        reverse_proxy :80
      }
    x-ports: [80/http]
"#;
    assert!(
        parse_normalized(caddy_conflict, ".")
            .unwrap_err()
            .to_string()
            .contains("cannot be specified simultaneously")
    );
    let empty_caddy = parse_normalized(
        "services: {app: {image: app, x-caddy: {}, x-ports: ['80/http']}}",
        ".",
    )
    .unwrap();
    assert!(
        service(&empty_caddy, "app")
            .ingress_proxy_fragment
            .is_none()
    );
    assert!(
        parse_normalized(
            "services: {app: {image: app, x-caddy: {config: inline, unknown: bad}}}",
            ".",
        )
        .unwrap_err()
        .to_string()
        .contains("invalid x-caddy key")
    );

    let ipv6 = parse_normalized(
        "services: {app: {image: app, ports: [{target: 80, published: 8080, host_ip: '[2001:db8::]/64', mode: host}]}}",
        ".",
    )
    .unwrap();
    assert!(matches!(
        service(&ipv6, "app").ports.first().unwrap(),
        PortPublication::Host { bind: HostBind::Prefix { prefix }, .. }
            if prefix.to_string() == "2001:db8::/64"
    ));
}

#[test]
fn compose_normalizes_an_omitted_ordinary_volume_driver_to_local() {
    let project = parse_normalized(
        "services: {app: {image: app, volumes: [data:/data]}}\nvolumes: {data: {}}",
        ".",
    )
    .unwrap();
    let source = &service(&project, "app").volumes().first().unwrap().source;
    assert!(matches!(
        source,
        VolumeSource::Ordinary { driver, .. }
            if driver.name() == "local" && driver.options().is_empty()
    ));
}

#[test]
fn singular_ployz_extensions_warn_and_remain_ignored() {
    for (typo, correction) in [("x-port", "x-ports"), ("x-machine", "x-machines")] {
        let project = parse_normalized(
            &format!("services: {{app: {{image: app, {typo}: ignored}}}}"),
            ".",
        )
        .unwrap();

        assert_eq!(
            project.warnings,
            [format!(
                "service 'app': unsupported feature '{typo}'; use {correction}"
            )]
        );
        assert!(service(&project, "app").ports.is_empty());
        assert!(service(&project, "app").placement.machines.is_empty());
    }
}

#[test]
fn extension_namespace_stays_open() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app
    x-machines: one
    x-ports: [80/http]
    x-caddy: {}
    x-pre_deploy: {command: [echo, ready]}
    x-caddyy: ignored
    x-predeploy: ignored
    x-third-party: {enabled: true}
"#,
        ".",
    )
    .unwrap();

    assert!(project.warnings.is_empty());
}

#[test]
fn pre_deploy_rejects_every_unknown_key() {
    for key in ["timout", "user"] {
        let error = parse_normalized(
            &format!(
                "services: {{app: {{image: app, x-pre_deploy: {{command: [echo], {key}: ignored}}}}}}"
            ),
            ".",
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains(&format!("invalid x-pre_deploy key: {key}")),
            "{error}"
        );
    }
}

#[test]
fn extensions_accept_machine_scalar_and_list_and_preserve_external_volumes() {
    let project = parse_normalized(
        r#"
services:
  scalar: {image: app, x-machines: one, volumes: [{type: volume, source: shared, target: /data}]}
  list: {image: app, x-machines: [two, three]}
volumes:
  shared: {name: shared, external: true}
"#,
        ".",
    )
    .unwrap();
    assert_eq!(
        service(&project, "scalar")
            .placement
            .machines
            .first()
            .unwrap()
            .as_str(),
        "one"
    );
    assert_eq!(
        service(&project, "list")
            .placement
            .machines
            .iter()
            .map(|machine| machine.as_str())
            .collect::<Vec<_>>(),
        ["two", "three"]
    );
    assert!(matches!(
        service(&project, "scalar")
            .volumes()
            .first()
            .unwrap()
            .source,
        VolumeSource::External { .. }
    ));
    let existing = machine('a', "one");
    let plan = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![existing.clone(), machine('b', "two")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![snapshot_volume(
                existing.machine.id,
                "shared",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(plan.volumes_to_create.is_empty());
}

#[test]
fn missing_external_volume_is_deferred_to_the_target_container_operation() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: ext-vol, target: /data}]}
volumes:
  ext-vol: {external: true}
"#,
        ".",
    )
    .unwrap();
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine('a', "one")],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn unavailable_external_volume_detail_is_deferred_to_container_ensure() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: ext-vol, target: /data}]}
volumes:
  ext-vol: {external: true}
"#,
        ".",
    )
    .unwrap();
    let machine = machine('a', "one");
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine.clone()],
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                vec![VolumeObservationFailure {
                    id: DockerVolumeId {
                        machine_id: machine.machine.id,
                        name: DockerVolumeName::parse("ext-vol").unwrap(),
                    },
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "inspect failed".into(),
                        details: Default::default(),
                    },
                }],
                Vec::new(),
                Vec::new(),
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn failed_external_volume_inventory_does_not_block_planning() {
    let project = parse_normalized(
        "services: {app: {image: app, volumes: [{type: volume, source: ext-vol, target: /data}]}}\nvolumes: {ext-vol: {external: true}}\n",
        ".",
    )
    .unwrap();
    let machine = machine('a', "one");
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine.clone()],
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                Vec::new(),
                vec![MachineFailure {
                    machine_id: machine.machine.id,
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "list failed".into(),
                        details: Default::default(),
                    },
                }],
                Vec::new(),
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn omitted_external_volume_inventory_does_not_block_planning() {
    let project = parse_normalized(
        "services: {app: {image: app, volumes: [{type: volume, source: ext-vol, target: /data}]}}\nvolumes: {ext-vol: {external: true}}\n",
        ".",
    )
    .unwrap();
    let machine = machine('a', "one");
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine.clone()],
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![machine.machine.id],
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn unused_external_volume_has_no_runtime_effect() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app}
volumes:
  orphan: {external: true}
"#,
        ".",
    )
    .unwrap();
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine('a', "one")],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn existing_external_volume_is_not_created() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: ext-vol, target: /data}]}
volumes:
  ext-vol: {external: true}
"#,
        ".",
    )
    .unwrap();
    let existing = machine('a', "one");
    let plan = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![existing.clone(), machine('b', "two")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![snapshot_volume(
                existing.machine.id,
                "ext-vol",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(plan.volumes_to_create.is_empty());
}

#[test]
fn unused_external_volumes_are_inert() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app}
volumes:
  beta: {external: true}
  alpha: {external: true}
"#,
        ".",
    )
    .unwrap();
    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine('a', "one")],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn select_services_keeps_unused_external_volumes_inert() {
    let project = parse_normalized(
        r#"
services:
  web: {image: web}
  api: {image: api, volumes: [{type: volume, source: data, target: /data}]}
volumes:
  data: {external: true}
"#,
        ".",
    )
    .unwrap();
    let selected = project.select_services(&["web".into()]).unwrap();
    let preview = plan_compose(
        &selected,
        &DeploySnapshot {
            machines: vec![machine('a', "one")],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { spec, .. }] if spec.name.as_str() == "web"
    ));
    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn global_service_never_previews_external_volume_creation() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app
    deploy: {mode: global}
    volumes: [{type: volume, source: shared, target: /data}]
volumes:
  shared: {name: shared, external: true}
"#,
        ".",
    )
    .unwrap();
    let first = machine('a', "one");
    let second = machine('b', "two");
    let plan = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![first.clone(), second.clone()],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![snapshot_volume(
                first.machine.id,
                "shared",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { machine_id: run_a, .. },
            DeployOperation::RunContainer { machine_id: run_b, .. },
        ] if run_a != run_b
            && [*run_a, *run_b].contains(&first.machine.id)
            && [*run_a, *run_b].contains(&second.machine.id)
    ));
    assert!(plan.volumes_to_create.is_empty());
}

#[test]
fn x_machines_rejects_star_and_keeps_all_as_identity() {
    let rejected =
        parse_normalized("services: {api: {image: app, x-machines: [\"*\"]}}", ".").unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("a non-empty Machine identity that is not a wildcard")
    );

    let project =
        parse_normalized("services: {api: {image: app, x-machines: [all]}}", ".").unwrap();
    assert_eq!(
        service(&project, "api")
            .placement
            .machines
            .first()
            .unwrap()
            .as_str(),
        "all"
    );
}

#[test]
fn secrets_are_plaintext_lazy_cached_and_redacted_from_errors() {
    let directory = TestDir::new();
    fs::write(directory.path.join("file-secret"), "from-file\n").unwrap();
    let yaml = r#"
name: demo
services:
  a: {image: app, environment: {TOKEN: secret://command, FILE: secret://file}}
  b: {image: app, environment: {TOKEN: secret://command}}
secrets:
  command: {x-command: "sh -c 'printf x >> count; echo command-value'"}
  file: {file: file-secret}
  unused: {x-command: "sh -c 'printf evaluated > unused'"}
"#;
    let mut project = parse_normalized(yaml, &directory.path).unwrap();
    project.resolve_secrets().unwrap();
    project.resolve_secrets().unwrap();
    assert_eq!(
        service(&project, "a")
            .container
            .environment
            .get("TOKEN")
            .unwrap(),
        "command-value"
    );
    assert_eq!(
        service(&project, "b")
            .container
            .environment
            .get("TOKEN")
            .unwrap(),
        "command-value"
    );
    assert_eq!(
        service(&project, "a")
            .container
            .environment
            .get("FILE")
            .unwrap(),
        "from-file\n"
    );
    assert_eq!(
        fs::read_to_string(directory.path.join("count")).unwrap(),
        "x"
    );
    assert!(!directory.path.join("unused").exists());

    let failing = r#"
services: {app: {image: app, environment: {TOKEN: secret://bad}}}
secrets:
  bad: {x-command: "sh -c 'printf supersecret; printf diagnostic >&2; exit 1'"}
"#;
    let mut project = parse_normalized(failing, &directory.path).unwrap();
    let error = project.resolve_secrets().unwrap_err().to_string();
    assert!(error.contains("diagnostic"));
    assert!(!error.contains("supersecret"));

    let oversized = r#"
services: {app: {image: app, environment: {TOKEN: secret://large}}}
secrets:
  large: {x-command: "sh -c 'head -c 1048577 /dev/zero'"}
"#;
    let mut project = parse_normalized(oversized, &directory.path).unwrap();
    assert!(
        project
            .resolve_secrets()
            .unwrap_err()
            .to_string()
            .contains("output exceeded 1048576 bytes")
    );

    let atomic = r#"
services:
  app: {image: app, environment: {A: secret://first, B: secret://missing}}
secrets:
  first: {x-command: "sh -c 'printf x >> atomic-count; printf first'"}
"#;
    let mut project = parse_normalized(atomic, &directory.path).unwrap();
    assert!(project.resolve_secrets().is_err());
    assert_eq!(
        service(&project, "app").container.environment,
        BTreeMap::from([
            ("A".into(), "secret://first".into()),
            ("B".into(), "secret://missing".into())
        ])
    );
    assert!(project.resolve_secrets().is_err());
    assert_eq!(
        fs::read_to_string(directory.path.join("atomic-count")).unwrap(),
        "x",
        "successful secret sources stay cached across a later resolution failure"
    );
}

#[test]
fn compose_deploy_pulls_untagged_images_as_latest() {
    let project = parse_normalized(
        r#"
name: demo
services:
  untagged: {image: alpine}
  tagged: {image: alpine:3.20}
  digest: {image: 'alpine@sha256:0000000000000000000000000000000000000000000000000000000000000000'}
  registry: {image: localhost:5000/foo}
"#,
        ".",
    )
    .unwrap();
    assert_eq!(
        service(&project, "untagged").container.image,
        "alpine:latest"
    );
    assert_eq!(service(&project, "tagged").container.image, "alpine:3.20");
    assert_eq!(
        service(&project, "digest").container.image,
        "alpine@sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    assert_eq!(
        service(&project, "registry").container.image,
        "localhost:5000/foo:latest"
    );
}

#[test]
fn git_image_templates_keep_explicit_references_and_show_dirty_state() {
    let directory = TestDir::new();
    git(&directory.path, &["init"]);
    git(
        &directory.path,
        &["config", "user.email", "test@example.com"],
    );
    git(&directory.path, &["config", "user.name", "Test"]);
    fs::write(directory.path.join("tracked"), "clean").unwrap();
    git(&directory.path, &["add", "tracked"]);
    let output = Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&directory.path)
        .env("GIT_AUTHOR_DATE", "2025-08-16T13:07:34Z")
        .env("GIT_COMMITTER_DATE", "2025-08-16T13:07:34Z")
        .output()
        .unwrap();
    assert!(output.status.success());
    let sha = git_output(&directory.path, &["rev-parse", "--short=7", "HEAD"]);
    let yaml = r#"
name: project
services:
  generated: {build: {context: .}}
  untagged: {image: example/app, build: {context: .}}
  tagged: {image: example/app:v1, build: {context: .}}
  digest: {image: 'example/app@sha256:0000000000000000000000000000000000000000000000000000000000000000', build: {context: .}}
  template: {image: 'example/app:{{gitdate "2006-01-02"}}-{{gitsha 7}}'}
  conditional: {image: 'example/app:{{if .Git.IsRepo}}git-{{gitsha 7}}{{else}}no-git{{end}}'}
  long: {image: '{{.Project}}/{{.Service}}:{{if .Git.IsRepo}}{{gitdate "2006-01-02-150405"}}.{{gitsha 7}}{{if .Git.IsDirty}}.dirty{{end}}{{else}}{{date "2006-01-02-150405"}}{{end}}', build: {context: .}}
"#;
    let clean = parse_normalized(yaml, &directory.path).unwrap();
    let tag = format!("2025-08-16-130734.{sha}");
    assert_eq!(
        service(&clean, "generated").container.image,
        format!("project/generated:{tag}")
    );
    assert_eq!(
        service(&clean, "untagged").container.image,
        format!("example/app:{tag}")
    );
    assert_eq!(service(&clean, "tagged").container.image, "example/app:v1");
    assert_eq!(
        service(&clean, "digest").container.image,
        "example/app@sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    assert_eq!(
        service(&clean, "template").container.image,
        format!("example/app:2025-08-16-{sha}")
    );
    assert_eq!(
        service(&clean, "conditional").container.image,
        format!("example/app:git-{sha}")
    );
    assert_eq!(
        service(&clean, "long").container.image,
        format!("project/long:{tag}")
    );

    fs::write(directory.path.join("dirty"), "dirty").unwrap();
    let dirty = parse_normalized(yaml, &directory.path).unwrap();
    assert!(
        service(&dirty, "generated")
            .container
            .image
            .ends_with(".dirty")
    );
    assert!(service(&dirty, "long").container.image.ends_with(".dirty"));

    let non_git_directory = TestDir::new();
    let non_git = parse_normalized(yaml, &non_git_directory.path).unwrap();
    let generated = service(&non_git, "generated")
        .container
        .image
        .strip_prefix("project/generated:")
        .unwrap();
    assert!(chrono::NaiveDateTime::parse_from_str(generated, "%Y-%m-%d-%H%M%S").is_ok());
    assert_eq!(
        service(&non_git, "conditional").container.image,
        "example/app:no-git"
    );

    let invalid = parse_normalized(
        "name: project\nservices: {app: {image: 'example/app@sha256:abc', build: .}}",
        &directory.path,
    )
    .unwrap_err();
    assert!(invalid.to_string().contains("parse image reference"));
}

#[test]
fn compose_plan_separates_volume_preview_from_service_operations() {
    let yaml = r#"
name: demo
services:
  db:
    image: db:1
    environment: {TOKEN: secret://token}
    volumes: [{type: volume, source: data, target: /data}]
    x-pre_deploy: {command: [sh, -c, migrate]}
  api:
    image: api:1
    depends_on: {db: {condition: service_started}}
volumes: {data: {name: demo_data}}
secrets: {token: {x-command: "printf resolved"}}
"#;
    let project = parse_normalized(yaml, ".").unwrap();
    let requested_volume = service(&project, "db").volumes().first().unwrap().clone();
    let snapshot = DeploySnapshot {
        machines: vec![machine('a', "one"), machine('b', "two")],
        ..Default::default()
    };
    let original = snapshot.clone();
    let plan = plan_compose(&project, &snapshot).unwrap();
    assert_eq!(snapshot, original);
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunHook { .. },
            DeployOperation::RunContainer { spec: db, .. },
            DeployOperation::RunContainer { spec: api, .. },
        ] if db.name.as_str() == "db"
            && db.container.environment.get("TOKEN").map(String::as_str) == Some("resolved")
            && api.name.as_str() == "api"
    ));
    let previewed_volume = plan
        .volumes_to_create
        .first()
        .expect("missing managed Volume is previewed");
    let plan_operations = operations(&plan);
    let volume = plan_operations
        .iter()
        .filter_map(DeployOperation::spec)
        .flat_map(|spec| spec.volume_graph.volumes())
        .find(|volume| matches!(&volume.source, VolumeSource::Ordinary { .. }))
        .expect("run operation carries the managed Volume");
    assert!(matches!(
        &volume.source,
        VolumeSource::Ordinary { name, labels, .. }
            if name.as_str() == "app_data"
                && labels.get(MANAGED_LABEL) == Some(&String::new())
                && labels.get(PROJECT_NAME_LABEL) == Some(&"app".to_string())
    ));
    assert_eq!(volume.reference, requested_volume.reference);
    assert_eq!(previewed_volume.name.as_str(), "app_data");
    assert_eq!(
        service(&project, "db")
            .container
            .environment
            .get("TOKEN")
            .map(String::as_str),
        Some("secret://token"),
        "planning must not mutate the unresolved project"
    );
}

#[test]
fn compose_provisioned_volume_preview_matches_equivalent_sdk_intent() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app
    volumes:
      - {type: volume, source: data, target: /data}
      - {type: volume, source: cache, target: /cache}
volumes: {cache: {}}
x-volumes: {data: 10G}
"#,
        ".",
    )
    .unwrap();
    let mut ready = machine('a', "one");
    ready.storage = Some(MachineStorageObservation::Ready);
    let snapshot = DeploySnapshot {
        machines: vec![ready],
        ..Default::default()
    };

    let mut compose_preview = plan_compose(&project, &snapshot).unwrap();
    let sdk_intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        project.services.values(),
        PlanOptions::default(),
    );
    let sdk_preview = preview_deploy(&sdk_intent, &snapshot, IngressContext::default()).unwrap();
    let sdk_row = sdk_preview
        .operations
        .iter()
        .find(|row| matches!(row.operation, DeployOperation::RunContainer { .. }))
        .unwrap();
    let DeployOperation::RunContainer { spec: sdk_spec, .. } = &sdk_row.operation else {
        unreachable!("matched a RunContainer row")
    };
    let compose_row = compose_preview
        .operations
        .iter_mut()
        .find(|row| matches!(row.operation, DeployOperation::RunContainer { .. }))
        .unwrap();
    let DeployOperation::RunContainer {
        spec: compose_spec, ..
    } = &mut compose_row.operation
    else {
        unreachable!("matched a RunContainer row")
    };
    compose_spec.service_id = sdk_spec.service_id;

    assert_eq!(compose_preview, sdk_preview);
    assert!(
        compose_preview
            .volumes_to_create
            .iter()
            .any(|item| item.maximum_bytes.is_some())
    );
    assert!(
        compose_preview
            .volumes_to_create
            .iter()
            .any(|item| item.name.as_str() == "app_cache")
    );
}

#[test]
fn compose_x_volume_scalar_and_object_forms_are_equivalent() {
    let yaml = |declaration: &str| {
        format!(
            "services: {{app: {{image: app, volumes: [data:/data]}}}}\nx-volumes: {{data: {declaration}}}\n"
        )
    };

    let scalar = parse_normalized(&yaml("10G"), ".").unwrap();
    let object = parse_normalized(&yaml("{size: 10G}"), ".").unwrap();

    assert_eq!(scalar, object);
}

#[test]
fn mounted_x_volume_resolves_to_a_provisioned_source() {
    let project = parse_normalized(
        "services: {app: {image: app, volumes: [data:/data]}}\nx-volumes: {data: 10G}\n",
        ".",
    )
    .unwrap();

    let volume = service(&project, "app")
        .volumes()
        .first()
        .expect("fixture mounts one volume");
    assert!(matches!(
        &volume.source,
        VolumeSource::Provisioned { name, maximum_bytes, labels }
            if name.as_str() == "data"
                && maximum_bytes.get() == 10 * 1024_u64.pow(3)
                && labels.is_empty()
    ));
}

#[test]
fn unused_x_volume_definition_is_inert() {
    let project = parse_normalized(
        "services: {app: {image: app}}\nx-volumes: {data: 10G}\n",
        ".",
    )
    .unwrap();
    let mut stateless = machine('a', "one");
    stateless.storage = Some(MachineStorageObservation::Stateless);

    let preview = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![stateless],
            ..Default::default()
        },
    )
    .unwrap();

    assert!(service(&project, "app").volumes().is_empty());
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
}

#[test]
fn compose_x_volume_size_stays_in_resolved_service_spec() {
    let load = |size: &str| {
        parse_normalized(
            &format!(
                "services: {{app: {{image: app, volumes: [data:/data]}}}}\nx-volumes: {{data: {size}}}\n"
            ),
            ".",
        )
        .unwrap()
    };
    let ten_gib = load("10G");
    let twenty_gib = load("20G");
    assert_ne!(ten_gib.services, twenty_gib.services);

    let mut ready = machine('a', "one");
    ready.storage = Some(MachineStorageObservation::Ready);
    let snapshot = DeploySnapshot {
        machines: vec![ready],
        ..Default::default()
    };
    let ten_preview = plan_compose(&ten_gib, &snapshot).unwrap();
    let twenty_preview = plan_compose(&twenty_gib, &snapshot).unwrap();
    let run_spec = |preview: &ployz::deploy::DeployPreview| {
        let row = preview
            .operations
            .iter()
            .find(|row| matches!(row.operation, DeployOperation::RunContainer { .. }))
            .unwrap();
        let DeployOperation::RunContainer { spec, .. } = &row.operation else {
            unreachable!("matched a RunContainer row")
        };
        spec.clone()
    };
    let ten_spec = run_spec(&ten_preview);
    let mut twenty_spec = run_spec(&twenty_preview);
    twenty_spec.service_id = ten_spec.service_id;
    assert_ne!(ten_spec, twenty_spec);

    let source_bound = |spec: &ployz_core::ResolvedServiceSpec| {
        let volume = spec
            .volume_graph
            .volumes()
            .first()
            .expect("fixture mounts one volume");
        let VolumeSource::Provisioned { maximum_bytes, .. } = &volume.source else {
            panic!("x-volume did not remain provisioned in resolved spec")
        };
        maximum_bytes.get()
    };
    assert_eq!(source_bound(&ten_spec), 10 * 1024_u64.pow(3));
    assert_eq!(source_bound(&twenty_spec), 20 * 1024_u64.pow(3));

    let bound = |preview: &ployz::deploy::DeployPreview| {
        preview
            .volumes_to_create
            .iter()
            .find_map(|item| item.maximum_bytes)
            .unwrap()
            .get()
    };
    assert_eq!(bound(&ten_preview), 10 * 1024_u64.pow(3));
    assert_eq!(bound(&twenty_preview), 20 * 1024_u64.pow(3));
}

#[test]
fn compose_x_volume_declarations_only_attach_to_named_mounts() {
    let project = parse_normalized(
        r#"
services:
  app:
    image: app
    volumes: [{type: bind, source: /srv/data, target: /data}]
x-volumes:
  bind-bd47413b5c03dfc6e4b5b8143fe205eae10ecaf0c28b426be98d74cff48c7453: 10G
"#,
        ".",
    )
    .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('a', "one")],
        ..Default::default()
    };

    let preview = plan_compose(&project, &snapshot).unwrap();

    assert!(preview.volumes_to_create.is_empty());
}

#[test]
fn compose_provisioning_metadata_follows_the_current_services() {
    let mut project = parse_normalized(
        "services: {app: {image: app, volumes: [data:/data]}}\nx-volumes: {data: 10G}\n",
        ".",
    )
    .unwrap();
    project.services.clear();

    let preview = plan_compose(&project, &DeploySnapshot::default()).unwrap();

    assert!(preview.operations.is_empty());
}

#[test]
fn compose_rejects_invalid_x_volume_declarations() {
    for (case, declaration) in [
        ("unknown key", "{size: 10G, typo: true}"),
        ("external", "{size: 10G, external: true}"),
        ("empty object", "{}"),
        ("missing size", "{description: data}"),
        ("bare number", "10"),
        ("zero", "0G"),
        ("overflow", "16777216T"),
    ] {
        let yaml =
            format!("services: {{app: {{image: app}}}}\nx-volumes: {{data: {declaration}}}\n");
        assert!(parse_normalized(&yaml, ".").is_err(), "accepted {case}");
    }
}

#[test]
fn compose_rejects_plain_and_provisioned_volume_name_collisions() {
    let error = match parse_normalized(
        "services: {app: {image: app, volumes: [data:/data]}}\nvolumes: {data: {}}\nx-volumes: {data: 10G}\n",
        ".",
    ) {
        Err(error) => error,
        Ok(_) => panic!("accepted a Plain and Provisioned Volume name collision"),
    };

    assert!(error.to_string().contains("both volumes and x-volumes"));
}

#[test]
fn compose_rejects_x_volumes_below_a_service() {
    let error = match parse_normalized(
        "services: {app: {image: app, x-volumes: {data: 10G}}}\n",
        ".",
    ) {
        Err(error) => error,
        Ok(_) => panic!("accepted x-volumes below a Service"),
    };

    assert!(error.to_string().contains("Compose top level"));
}

#[test]
fn depends_on_health_validation_rejects_unsupported_edges() {
    for (yaml, expected) in [
        (
            "services: {db: {image: db}, web: {image: web, depends_on: {db: {condition: service_healthy}}}}",
            "no configured healthcheck",
        ),
        (
            "services: {db: {image: db, healthcheck: {disable: true}}, web: {image: web, depends_on: {db: {condition: service_healthy}}}}",
            "no configured healthcheck",
        ),
        (
            "services: {db: {image: db}, web: {image: web, depends_on: {db: {restart: true}}}}",
            "unsupported 'restart'",
        ),
        (
            "services: {db: {image: db}, web: {image: web, depends_on: {db: {required: false}}}}",
            "unsupported 'required: false'",
        ),
        (
            "services: {db: {image: db}, web: {image: web, depends_on: {db: {required: sometimes}}}}",
            "must be true when present",
        ),
    ] {
        let error = parse_normalized(yaml, ".").unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }

    for yaml in [
        "services: {db: {image: db}, web: {image: web, depends_on: [db]}}",
        "services: {db: {image: db}, web: {image: web, depends_on: {db: {condition: service_started}}}}",
        "services: {db: {image: db, healthcheck: {test: [CMD, true]}}, web: {image: web, depends_on: {db: {condition: service_healthy, required: true}}}}",
        "services: {db: {image: db, healthcheck: {test: [CMD, true]}}, web: {image: web, depends_on: {db: {condition: service_healthy}}}}",
    ] {
        parse_normalized(yaml, ".").unwrap();
    }
}

#[test]
fn two_projects_with_the_same_logical_volume_get_distinct_physical_volumes() {
    let project = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: data, target: /data}]}
volumes: {data: {}}
"#,
        ".",
    )
    .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('a', "one")],
        ..Default::default()
    };
    let production = plan_compose_for(&project, &snapshot, "shop-production").unwrap();
    let staging = plan_compose_for(&project, &snapshot, "shop-staging").unwrap();
    let production_volume = created_named_volume(&production, "shop-production");
    let staging_volume = created_named_volume(&staging, "shop-staging");
    assert_eq!(production_volume.as_str(), "shop-production_data");
    assert_eq!(staging_volume.as_str(), "shop-staging_data");
    assert_ne!(production_volume, staging_volume);
}

fn created_named_volume(
    plan: &ployz::deploy::DeployPreview,
    project: &str,
) -> ployz_core::DockerVolumeName {
    let previewed = plan
        .volumes_to_create
        .iter()
        .find(|item| item.maximum_bytes.is_none())
        .expect("plan creates a Docker Volume");
    let plan_operations = operations(plan);
    let volume = plan_operations
        .iter()
        .filter_map(DeployOperation::spec)
        .flat_map(|spec| spec.volume_graph.volumes())
        .find(|volume| matches!(&volume.source, VolumeSource::Ordinary { name, .. } if name == &previewed.name))
        .expect("container operation carries the previewed Volume");
    let VolumeSource::Ordinary { name, labels, .. } = &volume.source else {
        panic!(
            "expected an ordinary Docker Volume, got {:?}",
            volume.source
        );
    };
    assert_eq!(labels.get(MANAGED_LABEL), Some(&String::new()));
    assert_eq!(
        labels.get(PROJECT_NAME_LABEL).map(String::as_str),
        Some(project)
    );
    assert_eq!(name, &previewed.name);
    previewed.name.clone()
}

#[test]
fn omitted_owned_volume_is_listed_as_preserved() {
    let machine = machine('a', "one");
    let with_volume = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: data, target: /data}]}
volumes: {data: {}}
"#,
        ".",
    )
    .unwrap();
    let without_volume = parse_normalized("services: {app: {image: app}}\n", ".").unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine.clone()],
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![owned_volume(
            machine.machine.id,
            "data",
        )])
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };
    let plan = plan_compose(&without_volume, &snapshot).unwrap();
    let preserved = plan
        .preserved_volumes
        .first()
        .expect("omitted owned volume is listed");
    assert_eq!(preserved.id.name.as_str(), "app_data");
    assert_eq!(
        preserved.machine_name,
        Some(ployz_core::MachineName::parse("one").unwrap())
    );
    assert!(plan.volumes_to_create.is_empty());
    let still_declared = plan_compose(&with_volume, &snapshot).unwrap();
    assert!(still_declared.preserved_volumes.is_empty());
}

#[test]
fn unprefixed_snapshot_volume_is_not_reused() {
    let machine = machine('a', "one");
    let project = parse_normalized(
        r#"
services:
  app: {image: app, volumes: [{type: volume, source: data, target: /data}]}
volumes: {data: {}}
"#,
        ".",
    )
    .unwrap();
    let plan = plan_compose(
        &project,
        &DeploySnapshot {
            machines: vec![machine.clone()],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![snapshot_volume(
                machine.machine.id,
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    let created = created_named_volume(&plan, "app");
    assert_eq!(created.as_str(), "app_data");
}

#[test]
fn service_selection_keeps_transitive_runtime_dependencies_only() {
    let project = parse_normalized(
        r#"
name: selection
services:
  database: {image: database}
  api: {image: api, depends_on: [database]}
  web: {image: web, depends_on: [api]}
  unrelated: {image: unrelated}
"#,
        ".",
    )
    .unwrap();
    let selected = project.select_services(&["web".into()]).unwrap();
    assert_eq!(
        selected
            .services
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["api", "database", "web"]
    );
    assert!(project.select_services(&["missing".into()]).is_err());
}

#[test]
fn loaded_compose_keeps_profiles_and_enabled_names_without_dropping_services() {
    let project = parse_normalized(
        r#"
name: demo
services:
  web: {image: nginx}
  worker: {image: busybox, profiles: [tools]}
"#,
        ".",
    )
    .unwrap();
    assert_eq!(
        project.service_profiles.get("worker"),
        Some(&vec!["tools".to_string()])
    );
    assert_eq!(project.enabled_service_names(&[]), vec!["web".to_string()]);
    assert_eq!(
        project.enabled_service_names(&["tools".into()]),
        vec!["web".to_string(), "worker".to_string()]
    );
    let selected = project.select_services(&["web".into()]).unwrap();
    assert!(selected.service_profiles.is_empty());
}

#[test]
fn compose_plan_anchors_shared_replicated_volumes_and_rejects_mixed_modes() {
    let replicated = parse_normalized(
        r#"
name: demo
services:
  first: {image: app, x-machines: [one, two], volumes: [{type: volume, source: data, target: /first}]}
  second: {image: app, x-machines: two, volumes: [{type: volume, source: data, target: /second}]}
volumes: {data: {name: demo_data}}
"#,
        ".",
    )
    .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('a', "one"), machine('b', "two")],
        ..Default::default()
    };
    let plan = plan_compose(&replicated, &snapshot).unwrap();
    let ops = operations(&plan);
    let previewed = plan
        .volumes_to_create
        .first()
        .unwrap_or_else(|| panic!("missing Volume preview: {plan:?}"));
    let anchor = previewed.machine_id;
    let existing_name = &previewed.name;
    assert_eq!(anchor, machine('b', "two").machine.id);
    assert!(ops.iter().all(|operation| matches!(
        operation,
        DeployOperation::RunContainer { machine_id, .. } if *machine_id == anchor
    )));

    let mut existing_snapshot = snapshot.clone();
    existing_snapshot.volume_snapshot = VolumeSnapshot::try_from_observations(
        snapshot
            .machines
            .iter()
            .map(|machine| managed_snapshot_volume(machine.machine.id, existing_name.as_str())),
    )
    .expect("valid Volume Snapshot fixture");
    let existing_on_both = plan_compose(&replicated, &existing_snapshot).unwrap();
    assert!(
        operations(&existing_on_both)
            .iter()
            .all(|operation| matches!(
                operation,
                DeployOperation::RunContainer { machine_id, .. } if *machine_id == anchor
            ))
    );

    let connected = parse_normalized(
        r#"
services:
  a-peer: {image: app, x-machines: [one, two], volumes: [{type: volume, source: a, target: /a}]}
  b-peer: {image: app, x-machines: two, volumes: [{type: volume, source: b, target: /b}]}
  flexible:
    image: app
    x-machines: [one, two]
    volumes:
      - {type: volume, source: a, target: /a}
      - {type: volume, source: b, target: /b}
volumes: {a: {}, b: {}}
"#,
        ".",
    )
    .unwrap();
    let connected_plan = plan_compose(&connected, &snapshot).unwrap();
    assert_eq!(connected_plan.volumes_to_create.len(), 2);
    assert!(
        connected_plan
            .volumes_to_create
            .iter()
            .all(|item| item.machine_id == anchor)
    );
    assert!(
        operations(&connected_plan)
            .iter()
            .all(|operation| match operation {
                DeployOperation::RunContainer { machine_id, .. } => *machine_id == anchor,
                other @ (DeployOperation::WaitHealthy { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::ReplaceContainer(..)
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }
                | DeployOperation::RemoveVolume { .. }) => {
                    panic!("unexpected operation: {other:?}")
                }
            })
    );

    let constrained = parse_normalized(
        r#"
services:
  first:
    image: app
    volumes:
      - {type: volume, source: shared, target: /shared}
      - {type: volume, source: existing, target: /existing}
  second: {image: app, volumes: [{type: volume, source: shared, target: /shared}]}
volumes:
  shared: {name: shared}
  existing: {name: existing}
"#,
        ".",
    )
    .unwrap();
    let existing_machine = machine('b', "two").machine.id;
    let constrained_plan = plan_compose(
        &constrained,
        &DeploySnapshot {
            machines: snapshot.machines.clone(),
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                existing_machine,
                "existing",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        constrained_plan
            .volumes_to_create
            .first()
            .expect("missing managed Volume is previewed")
            .machine_id,
        existing_machine
    );
    assert!(
        operations(&constrained_plan)
            .iter()
            .all(|operation| matches!(
                operation,
                DeployOperation::RunContainer { machine_id, .. } if machine_id == &existing_machine
            ))
    );

    let different_replica_counts = parse_normalized(
        r#"
services:
  first:
    image: app
    deploy: {replicas: 1}
    volumes: [{type: volume, source: data, target: /first}]
  second:
    image: app
    deploy: {replicas: 2}
    volumes: [{type: volume, source: data, target: /second}]
volumes: {data: {name: shared}}
"#,
        ".",
    )
    .unwrap();
    assert!(plan_compose(&different_replica_counts, &snapshot).is_ok());

    let disjoint = parse_normalized(
        r#"
services:
  first: {image: app, x-machines: one, volumes: [{type: volume, source: data, target: /first}]}
  second: {image: app, x-machines: two, volumes: [{type: volume, source: data, target: /second}]}
volumes: {data: {name: shared}}
"#,
        ".",
    )
    .unwrap();
    let disjoint_error = plan_compose(&disjoint, &snapshot).unwrap_err();
    let display = disjoint_error.to_string();
    assert!(
        matches!(
            &disjoint_error,
            PlanError::Service { source, .. }
                if matches!(
                    source.as_ref(),
                    PlanError::NoEligibleMachines { constraints }
                        if matches!(
                            constraints.as_slice(),
                            [EliminatingConstraint::SharedVolumeNoCommonMachine {
                                volume,
                                requested,
                            }]
                                if volume.as_str() == "app_data"
                                    && requested.iter().map(|target| target.as_str()).eq(["one", "two"])
                        )
                )
        ),
        "{disjoint_error:?}"
    );
    assert!(
        display.contains(
            "x-machines 'one', 'two' have no Machine in common for Docker Volume 'app_data'"
        ),
        "{display}"
    );

    let mixed = parse_normalized(
        r#"
services:
  everywhere:
    image: app
    deploy: {mode: global}
    volumes: [{type: volume, source: data, target: /global}]
  singleton:
    image: app
    volumes: [{type: volume, source: data, target: /replicated}]
volumes: {data: {name: shared}}
"#,
        ".",
    )
    .unwrap();
    assert!(matches!(
        plan_compose(&mixed, &snapshot),
        Err(PlanError::MixedVolumeModes { name, global, replicated })
            if name.as_str() == "app_data" && global == "everywhere" && replicated == "singleton"
    ));
}

fn git(directory: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn git_output(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ployz-compose-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
