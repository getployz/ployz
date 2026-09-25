#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
use serde_json::{Value, json};

#[test]
fn lowering_owns_port_defaults_and_domain_overrides() {
    let config = json!({"version":2,"privateDns":"api",
        "source":{"version":1,"type":"image","image":"nginx:stable","credentials":{"type":"none"}},
        "healthcheck":{"type":"http","path":"/health","timeoutSeconds":10},"restartPolicy":"on-failure",
        "routes":[{"id":"00000000-0000-4000-8000-000000000001","hostname":"app.example.com","targetPort":null}]
    });
    let lower = |config: Value, env: Value| {
        config_request(json!({"operation":"lower_deployment","value":{
            "projectName":"production","snapshots":[{"config":config,"resolvedEnv":env}]
        }}))
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
    let error = config_request(json!({"operation":"lower_deployment","value":{
        "projectName":"production","snapshots":[{"config":config,"resolvedEnv":{}}]
    }}))
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
        config_request(json!({"operation":"lower_deployment","value":{
            "projectName":"production","snapshots":[{"config":config,"resolvedEnv":{"TOKEN":"authorized-secret","PORT":"8080"}}],
            "volumes":[{"volumeResourceId":"00000000-0000-4000-8000-000000000002"}]
        }}))
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
    let mut http = config;
    http["healthcheck"] = json!({"type":"http","path":"/health","timeoutSeconds":10});
    assert_eq!(
        lower(http).unwrap()["target"][0]["container"]["healthcheck"],
        json!({"state":"http","path":"/health","port":8080,"timeout_seconds":10})
    );
}
