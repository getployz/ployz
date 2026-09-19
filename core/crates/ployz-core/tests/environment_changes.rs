use ployz_core::config::config_request;
use serde_json::{Value, json};

fn project(node_type: &str, current: Value, baseline: Value) -> Value {
    config_request(json!({"operation":"project_changes","value":{
        "working":{"token":"working","nodes":[{"node":{"type":node_type,"id":"stable-id"},"config":current}]},
        "saved":{"token":"saved","nodes":[{"node":{"type":node_type,"id":"stable-id"},"config":baseline}]},
        "applied":{"token":"applied","nodes":[{"node":{"type":node_type,"id":"stable-id"},"config":baseline}]},
        "nodeIntroductions":{"token":"none","nodes":[]},"submitted":null}})).unwrap()
}

fn service(replicas: u8) -> Value {
    json!({"version":2,"name":"API","privateDns":"api","source":{"version":1,"type":"image","image":"nginx:stable","autoUpdate":{"type":"off"},"credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped","replicas":replicas})
}

#[test]
fn service_and_volume_changes_remain_reviewable() {
    let service_review = project("service", service(2), service(1));
    assert_eq!(
        service_review.pointer("/groups/0/lifecycle").unwrap(),
        "update"
    );
    assert_eq!(
        service_review.pointer("/groups/0/settings/0/path").unwrap(),
        "replicas"
    );
    let volume_review = project(
        "volume",
        json!({"version":2,"name":"Renamed"}),
        json!({"version":2,"name":"Data"}),
    );
    assert_eq!(
        volume_review.pointer("/groups/0/settings/0/path").unwrap(),
        "name"
    );
}
