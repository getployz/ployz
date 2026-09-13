#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
use serde_json::{Value, json};

#[test]
fn lowering_retains_commands_limits_restart_and_network_ownership() {
    let config = json!({"version":2,"name":"API","privateDns":"api",
        "source":{"version":1,"type":"image","image":"registry.test/api@sha256:captured","autoUpdate":{"type":"off"},"credentials":{"type":"none"}},
        "startCommand":"exec app","preDeployCommand":"migrate","healthcheck":{"type":"none"},
        "restartPolicy":"on-failure","maxRetries":7,"cpuLimit":0.5,"memLimit":2,"replicas":3,
        "routes":[{"id":"00000000-0000-4000-8000-000000000001","hostname":"app.example.com","targetPort":8080}],
        "managedHostname":{"prefix":"api-production","targetPort":null},
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
    assert_eq!(spec["ports"].as_array().unwrap().len(), 2);
    assert_eq!(
        spec["ports"][1]["hostname"],
        json!({"kind":"cluster_domain","label":"api-production"})
    );
    assert_eq!(
        spec["mounts"][0]["volume"],
        "vol-00000000-0000-4000-8000-000000000002"
    );
    assert_eq!(intent["options"]["selected"], json!([{"name":"api"}]));
    let mut http = config.clone();
    http["healthcheck"] = json!({"type":"http","path":"/health","timeoutSeconds":10});
    assert_eq!(
        lower(http).unwrap()["target"][0]["container"]["healthcheck"],
        json!({"state":"http","path":"/health","port":8080,"timeout_seconds":10})
    );
    let mut cron = config;
    cron["cron"] = json!("* * * * *");
    let error = lower(cron).unwrap_err();
    assert_eq!(error.path, "cron");
    assert!(!error.to_string().contains("authorized-secret"));
}
