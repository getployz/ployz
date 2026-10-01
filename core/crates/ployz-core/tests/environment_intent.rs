//! Authored document admission and compilation contracts.

use ployz_core::config::{compile_environment_intent, parse_environment_intent};
use serde_json::json;

fn intent() -> serde_json::Value {
    json!({"version":1,"environmentSlug":"production","services":[{
        "id":"00000000-0000-4000-8000-000000000002","lineageId":"00000000-0000-4000-8000-000000000003","slug":"web",
        "config":{"version":2,"privateDns":"web","source":{"type":"image","version":1,"image":"nginx","credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped"},
        "variables":[{"id":"00000000-0000-4000-8000-000000000004","key":"HOST","description":null,"exported":true,"valueFingerprint":"plain","value":{"kind":"literal","value":"db"}}],
        "volumeAttachments":[{"volumeResourceId":"00000000-0000-4000-8000-000000000006","mountPath":"/data"}]}],
        "volumes":[{"resourceId":"00000000-0000-4000-8000-000000000006","resourceLineageId":"00000000-0000-4000-8000-000000000007","name":"Data","storage":{"kind":"provisioned","maximumBytes":5000000000_i64}}]})
}

#[test]
fn authored_service_variables_compile_with_volume_mounts() {
    let compiled = serde_json::to_value(compile_environment_intent(
        "00000000-0000-4000-8000-000000000001",
        parse_environment_intent(intent()).unwrap(),
    ))
    .unwrap();
    assert_eq!(
        compiled
            .get("variableProducers")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|producer| producer.get("key").and_then(|value| value.as_str()) == Some("PORT"))
            .unwrap()
            .pointer("/value/value")
            .unwrap(),
        "8080"
    );
    assert_eq!(
        compiled
            .pointer("/nodeSnapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        compiled
            .pointer("/nodeSnapshots/0/config/env/HOST/value")
            .unwrap(),
        "db"
    );
    assert_eq!(
        compiled
            .pointer("/nodeSnapshots/0/config/mounts/0/volumeName")
            .unwrap(),
        "Data"
    );
    assert_eq!(
        compiled.pointer("/nodeSnapshots/1/config/storage").unwrap(),
        &json!({"kind":"provisioned","maximumBytes":5000000000_i64})
    );
}

#[test]
fn saved_volumes_require_an_explicit_valid_storage_choice() {
    for storage in [
        json!({"kind":"provisioned","maximumBytes":0}),
        json!({"kind":"docker","maximumBytes":5}),
    ] {
        let mut value = intent();
        *value.pointer_mut("/volumes/0/storage").unwrap() = storage;
        assert!(parse_environment_intent(value).is_err());
    }
    let mut value = intent();
    value
        .pointer_mut("/volumes/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("storage");
    assert!(parse_environment_intent(value).is_err());
}

#[test]
fn authored_documents_reject_derived_service_fields() {
    let mut value = intent();
    value
        .pointer_mut("/services/0/config")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("env".to_owned(), json!({}));
    assert!(parse_environment_intent(value).is_err());
}
