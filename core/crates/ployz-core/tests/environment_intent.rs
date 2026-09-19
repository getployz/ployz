//! Authored document admission and compilation contracts.

use ployz_core::config::config_request;
use serde_json::json;

fn intent() -> serde_json::Value {
    json!({"version":1,"environmentSlug":"production","services":[{
        "id":"00000000-0000-4000-8000-000000000002","lineageId":"00000000-0000-4000-8000-000000000003","slug":"web",
        "config":{"version":2,"name":"Web","privateDns":"web","source":{"type":"image","version":1,"image":"nginx","autoUpdate":{"type":"off"},"credentials":{"type":"none"}},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped"},
        "variables":[{"id":"00000000-0000-4000-8000-000000000004","key":"HOST","description":null,"exported":true,"valueFingerprint":"plain","value":{"kind":"literal","value":"db"}}],
        "volumeAttachments":[{"volumeResourceId":"00000000-0000-4000-8000-000000000006","mountPath":"/data"}],"encryptedRegistryUsername":null,"encryptedRegistrySecret":null}],
        "volumes":[{"resourceId":"00000000-0000-4000-8000-000000000006","resourceLineageId":"00000000-0000-4000-8000-000000000007","name":"Data"}]})
}

#[test]
fn authored_service_variables_compile_with_volume_mounts() {
    let compiled = config_request(json!({"operation":"compile_environment","environment_id":"00000000-0000-4000-8000-000000000001","value":intent()})).unwrap();
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
    assert!(config_request(json!({"operation":"parse_environment","value":value})).is_err());
}
