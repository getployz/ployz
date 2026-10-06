#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{ConfigError, lower_deployment};
use serde_json::{Value, json};

/// Lower a JSON deployment input, answering JSON as Cloud's worker reads it.
fn lowered(value: Value) -> Result<Value, ConfigError> {
    lower_deployment(serde_json::from_value(value).unwrap())
        .map(|intent| serde_json::to_value(intent).unwrap())
}

#[test]
fn lowering_owns_port_defaults_and_domain_overrides() {
    let config = json!({"version":2,"privateDns":"api",
        "source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},
        "healthcheck":{"type":"http","path":"/health","timeoutSeconds":10},"restartPolicy":"on-failure",
        "routes":[{"id":"00000000-0000-4000-8000-000000000001","hostname":"app.example.com","targetPort":null}]
    });
    let lower = |config: Value, env: Value| {
        lowered(json!({
            "namespace":"production","snapshots":[{"config":config,"resolvedEnv":env}]
        }))
    };

    for (env, expected_port) in [(json!({}), 8080), (json!({"PORT":"3000"}), 3000)] {
        let intent = lower(config.clone(), env).unwrap();
        let spec = &intent["target"][0];
        assert_eq!(
            spec["container"]["environment"]["PORT"],
            expected_port.to_string()
        );
        assert_eq!(spec["container"]["healthcheck"]["port"], expected_port);
        assert_eq!(spec["ports"][0]["container_port"], expected_port);
    }

    let mut explicit = config.clone();
    explicit["routes"][0]["targetPort"] = json!(80);
    let intent = lower(explicit.clone(), json!({"PORT":"3000"})).unwrap();
    assert_eq!(intent["target"][0]["ports"][0]["container_port"], 80);
    assert_eq!(
        intent["target"][0]["container"]["environment"]["PORT"],
        "3000"
    );
    assert_eq!(
        intent["target"][0]["container"]["healthcheck"]["port"],
        3000
    );

    for invalid in ["", "0", "65536", "not-a-port"] {
        assert_eq!(
            lower(config.clone(), json!({"PORT":invalid}))
                .unwrap_err()
                .path,
            "healthcheck"
        );
        let mut automatic = config.clone();
        automatic["healthcheck"] = json!({"type":"none"});
        assert_eq!(
            lower(automatic, json!({"PORT":invalid})).unwrap_err().path,
            "routes"
        );
    }

    explicit["healthcheck"] = json!({"type":"none"});
    assert!(lower(explicit, json!({"PORT":"not-a-port"})).is_ok());
}

#[test]
fn lowering_refuses_unexpanded_managed_hostnames() {
    let config = json!({"version":2,"privateDns":"api",
        "source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},
        "healthcheck":{"type":"none"},"restartPolicy":"on-failure",
        "managedHostnames":[{"prefix":"api-production","targetPort":null}]
    });
    let error = lowered(json!({
        "namespace":"production","snapshots":[{"config":config,"resolvedEnv":{}}]
    }))
    .unwrap_err();
    assert_eq!(error.path, "managedHostnames");
}

#[test]
fn lowering_retains_commands_limits_restart_and_network_ownership() {
    let config = json!({"version":2,"privateDns":"api",
        "source":{"version":1,"type":"image","image":"registry.test/api@sha256:captured","credentials":{"type":"none"}},
        "startCommand":"exec app","preDeployCommand":"migrate","healthcheck":{"type":"none"},
        "restartPolicy":"on-failure","maxRetries":7,"cpuLimit":0.5,"memLimit":2,"replicas":3,
        "routes":[{"id":"00000000-0000-4000-8000-000000000001","hostname":"app.example.com","targetPort":8080}],
        "mounts":[{"volumeResourceId":"00000000-0000-4000-8000-000000000002","volumeName":"Renamed","mountPath":"/data"}]
    });
    let lower = |config: Value| {
        lowered(json!({
            "namespace":"production","snapshots":[{"config":config,"resolvedEnv":{"TOKEN":"authorized-secret","PORT":"8080"}}],
            "volumes":[{"volumeResourceId":"00000000-0000-4000-8000-000000000002","storage":{"kind":"docker"}}]
        }))
    };
    let intent = lower(config.clone()).unwrap();
    let spec = &intent["target"][0];
    assert_eq!(
        spec["container"]["image"],
        "registry.test/api@sha256:captured"
    );
    assert_eq!(
        spec["container"]["command"],
        json!(["/bin/sh", "-c", "exec app"])
    );
    assert_eq!(
        spec["pre_deploy"]["command"],
        json!(["/bin/sh", "-c", "migrate"])
    );
    assert_eq!(spec["container"]["resources"]["cpu_nanos"], 500_000_000);
    assert_eq!(
        spec["container"]["resources"]["memory_bytes"],
        2_000_000_000_i64
    );
    assert_eq!(
        spec["container"]["restart"],
        json!({"name":"on-failure","maximum_retry_count":7})
    );
    assert_eq!(spec["mode"]["replicas"], 3);
    assert_eq!(spec["ports"].as_array().unwrap().len(), 1);
    assert_eq!(
        spec["mounts"][0]["volume"],
        "vol-00000000-0000-4000-8000-000000000002"
    );
    assert_eq!(intent["options"]["selected"], json!([{"name":"api"}]));
    let healthcheck = |authored: Value| {
        let mut config = config.clone();
        config["healthcheck"] = authored;
        lower(config).unwrap()["target"][0]["container"]["healthcheck"].take()
    };
    assert_eq!(
        healthcheck(json!({"type":"http","path":"/health","timeoutSeconds":10})),
        json!({"state":"http","path":"/health","port":8080,"timeout_seconds":10})
    );
    assert_eq!(
        healthcheck(json!({"type":"none"})),
        json!({"state":"disabled"}),
        "an image's own HEALTHCHECK never runs under Cloud"
    );
    assert_eq!(
        healthcheck(
            json!({"type":"command","command":"pg_isready -h 127.0.0.1","timeoutSeconds":90})
        ),
        json!({
            "state":"configured","test":["CMD-SHELL","pg_isready -h 127.0.0.1"],
            "interval_millis":10_000,"timeout_millis":5_000,"start_period_millis":90_000,
            "start_interval_millis":1_000,"retries":3,"deadline_millis":90_000
        })
    );
}

#[test]
fn lowering_explains_root_and_colliding_mount_destinations() {
    for (paths, message) in [
        (
            vec!["/data/.."],
            "A volume cannot mount at the container root /",
        ),
        (
            vec!["/data", "/x/../data"],
            "Two mounts resolve to the same container path",
        ),
    ] {
        let ids = [
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000003",
        ];
        let mounts: Vec<_> = paths
            .iter()
            .zip(ids)
            .map(|(path, id)| {
                json!({
                    "volumeResourceId": id, "volumeName": "data", "mountPath": path,
                })
            })
            .collect();
        let error = lowered(json!({
            "namespace": "production",
            "snapshots": [{ "config": {
                "version": 2, "privateDns": "api",
                "source": { "version": 1, "type": "image", "image": "nginx:stable", "credentials": { "type": "none" } },
                "healthcheck": { "type": "none" }, "restartPolicy": "on-failure",
                "mounts": mounts,
            } }],
            "volumes": ids.map(|id| json!({ "volumeResourceId": id, "storage": { "kind": "docker" } })),
        })).unwrap_err();
        assert_eq!(error.path, "mounts", "{paths:?}: {error}");
        assert_eq!(error.message, message);
    }
}

fn lower_hook(pre_deploy: Option<&str>, setup: Value) -> Result<Value, ConfigError> {
    let config = json!({"version":2,"privateDns":"api",
        "source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},
        "preDeployCommand":pre_deploy,"healthcheck":{"type":"none"},"restartPolicy":"on-failure"
    });
    let mut snapshot = json!({"config":config});
    if !setup.is_null() {
        snapshot["setupCommands"] = setup;
    }
    lowered(json!({
        "namespace":"production","snapshots":[snapshot]
    }))
    .map(|intent| intent["target"][0]["pre_deploy"]["command"].clone())
}

/// Run a lowered hook command the way the Hook Container does: argv, no extra shell.
fn run(command: &Value) -> (bool, String) {
    let argv: Vec<&str> = command
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let output = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn setup_commands_run_after_the_own_command_each_as_it_would_alone() {
    // Without Setup Commands the hook is exactly today's.
    assert_eq!(
        lower_hook(Some("migrate"), Value::Null).unwrap(),
        json!(["/bin/sh", "-c", "migrate"])
    );
    assert_eq!(lower_hook(None, json!([])).unwrap(), Value::Null);

    // A trailing comment and quotes in either command change nothing else.
    let hook = lower_hook(
        Some(r#"echo "own 'one'" # trailing comment"#),
        json!([r#"echo 'setup "two"'"#]),
    )
    .unwrap();
    assert_eq!(run(&hook), (true, "own 'one'\nsetup \"two\"\n".into()));

    let hook = lower_hook(None, json!(["echo alone # note"])).unwrap();
    assert_eq!(hook, json!(["/bin/sh", "-c", "echo alone # note"]));
    assert_eq!(run(&hook), (true, "alone\n".into()));

    let hook = lower_hook(None, json!(["echo 1", "echo 2 #", "echo 3"])).unwrap();
    assert_eq!(run(&hook), (true, "1\n2\n3\n".into()));

    let hook = lower_hook(Some("echo own; exit 3"), json!(["echo setup"])).unwrap();
    assert_eq!(run(&hook), (false, "own\n".into()));
    let hook = lower_hook(Some("true"), json!(["echo 1; false", "echo 2"])).unwrap();
    assert_eq!(run(&hook), (false, "1\n".into()));
}

#[test]
fn setup_commands_follow_pre_deploy_command_rules() {
    let hook = lower_hook(None, json!(["  seed  "])).unwrap();
    assert_eq!(hook, json!(["/bin/sh", "-c", "seed"]));
    for refused in [json!(["  "]), json!(["x".repeat(2001)])] {
        assert_eq!(
            lower_hook(Some("migrate"), refused).unwrap_err().path,
            "setupCommands"
        );
    }
}

/// Snapshots whose `PORT` references name other Services by lineage; each Service owns one producer.
fn referencing(edges: &[(&str, &[&str])]) -> Value {
    let snapshots: Vec<Value> = edges
        .iter()
        .map(|(name, references)| {
            let env: serde_json::Map<String, Value> = references
                .iter()
                .enumerate()
                .map(|(index, dependency)| {
                    (format!("REF_{index}"), json!({"kind":"literal","value":"display-only","parts":[
                        {"kind":"ref","owner":{"scope":"service","lineageId":format!("lineage-{dependency}")},"key":"PORT"}
                    ]}))
                })
                .collect();
            json!({"serviceId":format!("id-{name}"),"resolvedEnv":{"PORT":"3000"},"config":{
                "version":2,"privateDns":name,
                "source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},
                "healthcheck":{"type":"none"},"restartPolicy":"unless-stopped","env":env
            }})
        })
        .collect();
    let lineages: serde_json::Map<String, Value> = edges
        .iter()
        .map(|(name, _)| (format!("lineage-{name}"), json!(format!("id-{name}"))))
        .collect();
    json!({"namespace":"production","snapshots":snapshots,"lineages":lineages})
}

fn dependencies(value: &Value) -> Value {
    lowered(value.clone()).unwrap()["dependencies"].clone()
}

#[test]
fn frozen_references_order_the_deploy_by_identity_not_display_text() {
    let mut input = referencing(&[
        ("app", &["postgres", "postgres", "app", "absent"]),
        ("postgres", &[]),
    ]);
    input["snapshots"][0]["config"]["env"]["LITERAL"] =
        json!({"kind":"literal","value":"${{postgres.PORT}}"});
    assert_eq!(
        dependencies(&input),
        json!({"app":[{"service":"postgres","condition":"service_started"}]})
    );
    input["snapshots"][1]["config"]["healthcheck"] =
        json!({"type":"http","path":"/health","timeoutSeconds":10});
    assert_eq!(
        dependencies(&input)["app"],
        json!([{"service":"postgres","condition":"service_healthy"}])
    );
    input["snapshots"][1]["config"]["healthcheck"] =
        json!({"type":"command","command":"pg_isready","timeoutSeconds":10});
    assert_eq!(
        dependencies(&input)["app"],
        json!([{"service":"postgres","condition":"service_healthy"}])
    );
    input["snapshots"][1]["config"]["source"] = json!({"version":1,"type":"empty","rootDir":"/"});
    assert_eq!(dependencies(&input), json!({}));
}

#[test]
fn reference_cycles_drop_only_their_own_edges() {
    let input = referencing(&[
        ("app", &["a"]),
        ("a", &["b", "db"]),
        ("b", &["c"]),
        ("c", &["a"]),
        ("db", &[]),
    ]);
    assert_eq!(
        dependencies(&input),
        json!({
            "app":[{"service":"a","condition":"service_started"}],
            "a":[{"service":"db","condition":"service_started"}]
        })
    );
}

/// Services mounting Configs; each Config is `(id, name, references, files)`.
fn mounting(services: Value, configs: &[(&str, &str, &[&str], Value)]) -> Value {
    let configs: Vec<Value> = configs
        .iter()
        .map(|(id, name, references, files)| {
            json!({"configResourceId": id, "name": name, "files": files,
                "references": references.iter().map(|lineage| format!("lineage-{lineage}")).collect::<Vec<_>>()})
        })
        .collect();
    json!({"namespace": "production", "snapshots": services, "configs": configs})
}

fn web(configs: Value) -> Value {
    json!({"serviceId": "id-web", "config": {
        "version": 2, "privateDns": "web",
        "source": {"version": 1, "type": "image", "image": "nginx:stable", "credentials": {"type": "none"}},
        "healthcheck": {"type": "none"}, "restartPolicy": "on-failure", "configs": configs,
    }})
}

#[test]
fn each_config_file_mounts_read_only_at_its_own_path_with_its_mode_and_owner() {
    let files = json!({
        "config.yml": {"content": "dsn: redis.internal\n", "mode": "0444", "uid": 0, "gid": 0},
        "certs/ca.pem": {"content": "pem", "mode": "0600", "uid": 1000, "gid": 1001},
    });
    let intent = lowered(mounting(
        json!([web(json!([
            {"configResourceId": "cfg-sentry", "configName": "sentry", "mountDir": "/etc/sentry"},
            {"configResourceId": "cfg-sentry", "configName": "sentry", "mountDir": "/srv/sentry"},
            {"configResourceId": "cfg-gone", "configName": "gone", "mountDir": "/etc/gone"},
        ]))]),
        &[("cfg-sentry", "sentry", &[], files)],
    ))
    .unwrap();
    let service = &intent["target"][0];
    assert_eq!(
        service["configs"],
        json!([
            {"name": "sentry/certs/ca.pem", "content": b"pem".to_vec()},
            {"name": "sentry/config.yml", "content": b"dsn: redis.internal\n".to_vec()},
        ])
    );
    let mount = |name: &str, target: &str, mode: u32, uid: u32, gid: u32| json!({"config_name": name, "target": target, "mode": mode, "uid": uid, "gid": gid});
    assert_eq!(
        service["container"]["config_mounts"],
        json!([
            mount(
                "sentry/certs/ca.pem",
                "/etc/sentry/certs/ca.pem",
                0o600,
                1000,
                1001
            ),
            mount("sentry/config.yml", "/etc/sentry/config.yml", 0o444, 0, 0),
            mount(
                "sentry/certs/ca.pem",
                "/srv/sentry/certs/ca.pem",
                0o600,
                1000,
                1001
            ),
            mount("sentry/config.yml", "/srv/sentry/config.yml", 0o444, 0, 0),
        ])
    );
}

#[test]
fn a_config_file_and_a_volume_cannot_share_a_container_path() {
    let mut input = mounting(
        json!([web(json!([
            {"configResourceId": "cfg-sentry", "configName": "sentry", "mountDir": "/data"},
        ]))]),
        &[(
            "cfg-sentry",
            "sentry",
            &[],
            json!({"x": {"content": "", "mode": "0444", "uid": 0, "gid": 0}}),
        )],
    );
    let volume = "00000000-0000-4000-8000-000000000002";
    input["snapshots"][0]["config"]["mounts"] =
        json!([{"volumeResourceId": volume, "volumeName": "data", "mountPath": "/data/x"}]);
    input["volumes"] = json!([{"volumeResourceId": volume, "storage": {"kind": "docker"}}]);
    let error = lowered(input).unwrap_err();
    assert_eq!(error.path, "mounts");
    assert_eq!(
        error.message,
        "Two mounts resolve to the same container path"
    );
}

/// `referencing`, with `mounts` naming the Configs each Service mounts and `configs`
/// the Services each Config's files reference.
fn referencing_configs(
    edges: &[(&str, &[&str])],
    mounts: &[(&str, &[&str])],
    configs: &[(&str, &[&str])],
) -> Value {
    let mut input = referencing(edges);
    for (service, mounted) in mounts {
        let index = edges.iter().position(|(name, _)| name == service).unwrap();
        input["snapshots"][index]["config"]["configs"] = mounted
            .iter()
            .map(|config| json!({"configResourceId": format!("cfg-{config}"), "configName": config, "mountDir": format!("/etc/{config}")}))
            .collect();
    }
    input["configs"] = configs
        .iter()
        .map(|(config, references)| {
            json!({"configResourceId": format!("cfg-{config}"), "name": config,
                "references": references.iter().map(|lineage| format!("lineage-{lineage}")).collect::<Vec<_>>(),
                "files": {"f": {"content": "", "mode": "0444", "uid": 0, "gid": 0}}})
        })
        .collect();
    input
}

#[test]
fn a_service_waits_for_what_its_mounted_configs_reference() {
    let mut input = referencing_configs(
        &[("web", &[]), ("postgres", &[]), ("redis", &[])],
        &[("web", &["sentry"])],
        &[("sentry", &["postgres", "redis", "web"])],
    );
    input["snapshots"][1]["config"]["healthcheck"] =
        json!({"type":"http","path":"/health","timeoutSeconds":10});
    assert_eq!(
        dependencies(&input),
        json!({"web":[
            {"service":"postgres","condition":"service_healthy"},
            {"service":"redis","condition":"service_started"},
        ]})
    );
}

#[test]
fn config_reference_cycles_drop_only_their_own_edges() {
    let input = referencing_configs(
        &[("app", &["a"]), ("a", &[]), ("b", &["a"]), ("db", &[])],
        &[("a", &["shared"])],
        &[("shared", &["b", "db"])],
    );
    let intent = lowered(input).unwrap();
    assert_eq!(
        intent["dependencies"],
        json!({
            "app":[{"service":"a","condition":"service_started"}],
            "a":[{"service":"db","condition":"service_started"}]
        })
    );
    let names: Vec<&Value> = intent["target"]
        .as_array()
        .unwrap()
        .iter()
        .map(|service| &service["name"])
        .collect();
    assert_eq!(
        names,
        [&json!("app"), &json!("a"), &json!("b"), &json!("db")]
    );
}
