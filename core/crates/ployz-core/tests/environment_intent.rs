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

#[test]
fn documents_from_before_configs_keep_their_shape() {
    let parsed = parse_environment_intent(intent()).unwrap();
    assert!(parsed.configs.is_empty());
    let written = serde_json::to_value(&parsed).unwrap();
    assert!(written.get("configs").is_none());
    assert!(
        written
            .pointer("/services/0")
            .unwrap()
            .get("configAttachments")
            .is_none()
    );
    assert_eq!(parse_environment_intent(written).unwrap(), parsed);
}

#[test]
fn a_document_from_before_configs_writes_the_same_bytes_as_one_with_empty_configs() {
    let canonical = serde_json::to_vec(&parse_environment_intent(intent()).unwrap()).unwrap();
    let text = String::from_utf8(canonical.clone()).unwrap();
    assert!(
        !text.contains("configs") && !text.contains("configAttachments"),
        "{text}"
    );
    let reparsed = parse_environment_intent(serde_json::from_slice(&canonical).unwrap()).unwrap();
    assert_eq!(serde_json::to_vec(&reparsed).unwrap(), canonical);

    let mut empty = intent();
    empty
        .as_object_mut()
        .unwrap()
        .insert("configs".into(), json!([]));
    empty
        .pointer_mut("/services/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("configAttachments".into(), json!([]));
    let written = serde_json::to_vec(&parse_environment_intent(empty).unwrap()).unwrap();
    assert_eq!(written, canonical);
}

fn with_config(files: serde_json::Value, mounts: serde_json::Value) -> serde_json::Value {
    let mut value = intent();
    value.as_object_mut().unwrap().insert(
        "configs".into(),
        json!([{"resourceId":"00000000-0000-4000-8000-000000000008","resourceLineageId":"00000000-0000-4000-8000-000000000009","name":"sentry","files":files}]),
    );
    value
        .pointer_mut("/services/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("configAttachments".into(), mounts);
    value
}

fn file(content: serde_json::Value) -> serde_json::Value {
    json!({"content":content,"mode":"0444","uid":0,"gid":0})
}

fn mount(dir: &str) -> serde_json::Value {
    json!([{"configResourceId":"00000000-0000-4000-8000-000000000008","mountDir":dir}])
}

#[test]
fn configs_compile_with_references_kept_by_service_lineage() {
    let reference = json!([{"kind":"text","value":"host: "},
        {"kind":"ref","owner":{"scope":"service","lineageId":"00000000-0000-4000-8000-000000000003"},"key":"HOST"}]);
    let value = with_config(
        json!({"config.yml":file(reference.clone()),".htpasswd":file(json!([])),"conf.d/a.xml":file(json!([]))}),
        mount("/etc/sentry"),
    );
    let compiled = serde_json::to_value(compile_environment_intent(
        "e",
        parse_environment_intent(value).unwrap(),
    ))
    .unwrap();
    let at = |path: &str| {
        compiled
            .pointer(&format!("/nodeSnapshots/2{path}"))
            .unwrap()
    };
    assert_eq!(at("/nodeType"), "config");
    assert_eq!(at("/config/name"), "sentry");
    assert_eq!(at("/config/files/config.yml/content"), &reference);
    assert_eq!(at("/config/files/config.yml/mode"), "0444");
}

#[test]
fn configs_refuse_bare_references_and_colliding_mounts() {
    let bare = json!([{"kind":"ref","owner":{"scope":"self"},"key":"HOST"}]);
    let error =
        parse_environment_intent(with_config(json!({"a":file(bare)}), json!([]))).unwrap_err();
    assert_eq!(error.path, "configs.files.content");

    let error = parse_environment_intent(with_config(json!({"a":file(json!([]))}), mount("/data")))
        .unwrap_err();
    assert_eq!(error.path, "mountPath");
    for alias in ["/data/", "/etc//a", "/etc/./a", "/etc/x/../a", "/"] {
        let error =
            parse_environment_intent(with_config(json!({"a":file(json!([]))}), mount(alias)))
                .unwrap_err();
        assert_eq!(error.path, "configAttachments.mountDir", "{alias}");
    }

    let mut value = with_config(json!({"a":file(json!([]))}), json!([]));
    let configs = value.get_mut("configs").unwrap().as_array_mut().unwrap();
    configs.push(json!({"resourceId":"00000000-0000-4000-8000-000000000010","resourceLineageId":"00000000-0000-4000-8000-000000000011","name":"other","files":{"b":file(json!([]))}}));
    *value.pointer_mut("/services/0/configAttachments").unwrap() = json!([
        {"configResourceId":"00000000-0000-4000-8000-000000000008","mountDir":"/etc"},
        {"configResourceId":"00000000-0000-4000-8000-000000000010","mountDir":"/etc/a"}]);
    let error = parse_environment_intent(value.clone()).unwrap_err();
    assert_eq!(error.path, "configAttachments.mountDir");
    *value
        .pointer_mut("/services/0/configAttachments/1/mountDir")
        .unwrap() = json!("/etc/x");
    parse_environment_intent(value).unwrap();

    for (volume, refused) in [
        ("/etc/a", true),
        ("/etc/a/", true),
        ("/etc/a/b", true),
        ("/etc//a", true),
        ("/etc/./a", true),
        ("/etc/x/../a", true),
        ("//etc/.", true),
        ("/etc/b", false),
    ] {
        let mut value = with_config(json!({"a":file(json!([]))}), mount("/etc"));
        *value
            .pointer_mut("/services/0/volumeAttachments/0/mountPath")
            .unwrap() = json!(volume);
        match parse_environment_intent(value) {
            Err(error) => assert!(refused && error.path == "mountPath", "{volume}: {error:?}"),
            Ok(_) => assert!(!refused, "{volume} mounted over a Config file"),
        }
    }

    for mode in ["1755", "4444", "0800", "x"] {
        let mut value = with_config(json!({"a":file(json!([]))}), json!([]));
        *value.pointer_mut("/configs/0/files/a/mode").unwrap() = json!(mode);
        assert!(parse_environment_intent(value).is_err(), "{mode}");
    }
}
