//! Authored document admission, compilation, redaction, and baseline restoration contracts.

#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
use serde_json::json;

#[test]
fn saved_intent_rejects_ambiguous_identity_and_derived_artifacts() {
    let empty = json!({"version":1,"environmentSlug":"production","services":[],"variableGroups":[],"volumes":[]});
    assert!(config_request(json!({"operation":"parse_environment","value":empty})).is_ok());
    let volume = json!({"resourceId":"00000000-0000-4000-8000-000000000009","resourceLineageId":"00000000-0000-4000-8000-000000000010","name":"Data"});
    let mut duplicate = empty.clone();
    duplicate["volumes"] = json!([volume, volume]);
    assert!(config_request(json!({"operation":"parse_environment","value":duplicate})).is_err());
    let mut artifact = empty;
    artifact["variableProducers"] = json!([]);
    assert!(config_request(json!({"operation":"parse_environment","value":artifact})).is_err());
}

#[test]
fn compilation_preserves_owner_references_precedence_and_encrypted_values() {
    let id = |n| format!("00000000-0000-4000-8000-{n:012}");
    let variable = |n, value| json!({"id":id(n),"key":"URL","description":null,"exported":true,"valueFingerprint":"existing-fingerprint","value":value});
    let config = json!({"version":2,"name":"Web","privateDns":"web","source":{"type":"empty","version":1,"rootDir":"/"},"preDeployCommand":null,"startCommand":null,"healthcheck":{"type":"none"},"restartPolicy":"unless-stopped"});
    let mut intent = json!({
        "version":1,"environmentSlug":"production",
        "services":[{"id":id(2),"lineageId":id(3),"slug":"web","config":config,
            "variables":[variable(8,json!({"kind":"literal","value":"own"}))],
            "variableGroupAttachments":[{"variableGroupId":id(6),"sortOrder":1},{"variableGroupId":id(16),"sortOrder":0}],
            "volumeAttachments":[{"volumeResourceId":id(9),"mountPath":"/data"}],
            "encryptedRegistryUsername":null,"encryptedRegistrySecret":null}],
        "variableGroups":[
            {"resourceId":id(4),"resourceLineageId":id(5),"variableGroupId":id(6),"variableGroupLineageId":id(7),"slug":"shared","name":"Shared",
                "variables":[variable(18,json!({"kind":"template","parts":[{"kind":"text","value":"${{ literal }}"},{"kind":"ref","owner":{"scope":"self"},"key":"HOST"}]}))]},
            {"resourceId":id(14),"resourceLineageId":id(15),"variableGroupId":id(16),"variableGroupLineageId":id(17),"slug":"earlier","name":"Earlier",
                "variables":[variable(28,json!({"kind":"literal","value":"earlier"}))]}
        ],
        "volumes":[{"resourceId":id(9),"resourceLineageId":id(10),"name":"Renamed data"}]
    });
    let compiled = config_request(
        json!({"operation":"compile_environment","environment_id":id(1),"value":intent}),
    )
    .unwrap();
    let env = &compiled["nodeSnapshots"][0]["config"]["env"]["URL"];
    assert_eq!(env["value"], "$${{ literal }}${{ HOST }}");
    assert_eq!(
        env["parts"][1]["owner"],
        json!({"scope":"variable_group","lineageId":id(7)})
    );
    assert_eq!(env["source"]["resourceId"], id(4));
    assert_eq!(
        compiled["nodeSnapshots"][0]["config"]["mounts"][0]["volumeName"],
        "Renamed data"
    );
    let producer = compiled["variableProducers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["ownerId"] == id(6) && p["key"] == "URL")
        .unwrap();
    assert_eq!(
        producer["value"]["parts"][1]["owner"],
        json!({"scope":"self"})
    );
    intent["variableGroups"][0]["variables"][0]["value"] = json!({"kind":"secret","encryptedValue":{"version":1,"iv":"iv","tag":"tag","ciphertext":"sealed"}});
    let compiled = config_request(
        json!({"operation":"compile_environment","environment_id":id(1),"value":intent}),
    )
    .unwrap();
    assert_eq!(
        compiled["nodeSnapshots"][0]["config"]["env"]["URL"]["encryptedValue"]["ciphertext"],
        "sealed"
    );
    let parsed = config_request(json!({"operation":"parse_environment","value":intent})).unwrap();
    assert!(parsed["services"][0]["config"].get("env").is_none());
    assert!(parsed["services"][0]["config"].get("mounts").is_none());
    let baseline = intent.clone();
    let mut deleted = intent.clone();
    deleted["variableGroups"].as_array_mut().unwrap().remove(0);
    deleted["services"][0]["variableGroupAttachments"]
        .as_array_mut()
        .unwrap()
        .retain(|a| a["variableGroupId"] != id(6));
    let restored = config_request(json!({"operation":"restore_environment","current":deleted,"baseline":baseline,"node_type":"variable_group","node_id":id(4),"path":null})).unwrap();
    assert!(
        restored["services"][0]["variableGroupAttachments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["variableGroupId"] == id(6))
    );
    let recompiled = config_request(
        json!({"operation":"compile_environment","environment_id":id(1),"value":restored}),
    )
    .unwrap();
    assert_eq!(
        recompiled["nodeSnapshots"][0]["config"]["env"]["URL"]["encryptedValue"]["ciphertext"],
        "sealed"
    );
    assert!(config_request(json!({"operation":"restore_environment","current":intent,"baseline":baseline,"node_type":"service","node_id":id(2),"path":"env.URL"})).is_err());
    intent["services"][0]["config"]["env"] = json!({});
    assert!(config_request(json!({"operation":"parse_environment","value":intent})).is_err());
}

#[test]
fn resource_comparison_redacts_secrets_and_preserves_owner_identity() {
    let baseline = json!({"version":1,"name":"Shared","variables":[{"key":"TOKEN","description":null,"exported":true,"value":{"type":"sealed","hasValue":true,"fingerprint":"old","encryptedValue":{"version":1,"iv":"iv","tag":"tag","ciphertext":"old-cipher"}}}]});
    let mut current = baseline.clone();
    current["variables"][0]["value"]["encryptedValue"]["ciphertext"] = json!("new-cipher");
    let compare = |current: serde_json::Value| {
        config_request(json!({"operation":"compare_resource","node_type":"variable_group","current":current,"baseline":baseline})).unwrap()
    };
    assert_eq!(compare(current.clone()), json!([]));
    current["variables"][0]["value"]["fingerprint"] = json!("new");
    let changes = compare(current);
    assert_eq!(changes[0]["path"], "variables.TOKEN");
    assert_eq!(changes[0]["canRestore"], false);
    assert_eq!(changes[0]["before"], json!({"kind":"secret"}));
    assert!(!changes.to_string().contains("cipher"));
    assert!(!changes.to_string().contains("fingerprint"));
    for node_type in ["volume", "variable_group"] {
        let current = if node_type == "volume" {
            json!({"version":2,"name":"Data"})
        } else {
            json!({"version":1,"name":"Shared","variables":[]})
        };
        let added = config_request(json!({"operation":"compare_resource","node_type":node_type,"current":current,"baseline":null})).unwrap();
        assert_eq!(added[0]["path"], "node");
        assert_eq!(added[0]["kind"], "add");
        assert_eq!(added[0]["canRestore"], true);
        assert_eq!(config_request(json!({"operation":"compare_resource","node_type":node_type,"current":current,"baseline":current})).unwrap(), json!([]));
    }
    let volume = config_request(json!({"operation":"compare_resource","node_type":"volume","current":{"version":2,"name":"Renamed"},"baseline":{"version":2,"name":"Data"}})).unwrap();
    assert_eq!(volume.as_array().unwrap().len(), 1);
    assert_eq!(volume[0]["path"], "name");
    assert_eq!(volume[0]["kind"], "update");
}

fn authored_fixture() -> serde_json::Value {
    json!({
        "version": 1, "environmentSlug": "production",
        "services": [{
            "id": "00000000-0000-4000-8000-000000000002",
            "lineageId": "00000000-0000-4000-8000-000000000003",
            "slug": "web",
            "config": {
                "version": 2, "name": "Web", "privateDns": "web",
                "source": {"type": "image", "version": 1, "image": "nginx",
                    "autoUpdate": {"type": "off"},
                    "credentials": {"type": "configured", "revision": "before"}},
                "preDeployCommand": null, "startCommand": null,
                "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped"
            },
            "variables": [], "variableGroupAttachments": [], "volumeAttachments": [],
            "encryptedRegistryUsername": null, "encryptedRegistrySecret": null
        }],
        "variableGroups": [{
            "resourceId": "00000000-0000-4000-8000-000000000004",
            "resourceLineageId": "00000000-0000-4000-8000-000000000005",
            "variableGroupId": "00000000-0000-4000-8000-000000000006",
            "variableGroupLineageId": "00000000-0000-4000-8000-000000000007",
            "slug": "shared", "name": "Shared", "variables": []
        }],
        "volumes": [{
            "resourceId": "00000000-0000-4000-8000-000000000008",
            "resourceLineageId": "00000000-0000-4000-8000-000000000009",
            "name": "Data"
        }]
    })
}

#[test]
fn authored_configuration_rejects_derived_fields_without_serialization_loss() {
    use ployz_core::config::{SavedEnvironmentIntent, parse_environment_intent};
    let authored = parse_environment_intent(authored_fixture()).unwrap();
    let wire = serde_json::to_value(&authored).unwrap();
    assert_eq!(
        serde_json::from_value::<SavedEnvironmentIntent>(wire.clone()).unwrap(),
        authored
    );
    for (field, value) in [
        ("env", json!({})),
        ("mounts", json!([])),
        ("variableGroupAttachments", json!([])),
    ] {
        assert!(wire["services"][0]["config"].get(field).is_none());
        let mut invalid = wire.clone();
        invalid["services"][0]["config"][field] = value;
        assert!(serde_json::from_value::<SavedEnvironmentIntent>(invalid).is_err());
    }
}

#[test]
fn compiled_snapshot_discriminator_and_version_follow_the_configuration() {
    use ployz_core::config::{
        CompiledEnvironmentIntent, CompiledEnvironmentNode, compile_environment_intent,
        parse_environment_intent,
    };
    let compiled = compile_environment_intent(
        "00000000-0000-4000-8000-000000000001",
        parse_environment_intent(authored_fixture()).unwrap(),
    );
    let wire = serde_json::to_value(&compiled).unwrap();
    assert_eq!(
        serde_json::from_value::<CompiledEnvironmentIntent>(wire.clone()).unwrap(),
        compiled
    );
    for node in wire["nodeSnapshots"].as_array().unwrap() {
        for (field, value) in [
            (
                "nodeType",
                json!(if node["nodeType"] == "service" {
                    "volume"
                } else {
                    "service"
                }),
            ),
            ("configVersion", json!(99)),
        ] {
            let mut invalid = node.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<CompiledEnvironmentNode>(invalid).is_err());
        }
    }
    assert_eq!(wire["nodeSnapshots"][0]["nodeType"], "service");
    assert_eq!(wire["nodeSnapshots"][0]["configVersion"], 1);
    assert_eq!(wire["nodeSnapshots"][0]["config"]["version"], 2);
}

#[test]
fn credential_restore_keeps_private_values_with_the_restored_reference() {
    let sealed = |value| json!({"version": 1, "iv": "iv", "tag": "tag", "ciphertext": value});
    for path in ["source.credentials", "source"] {
        for configured in [true, false] {
            let mut baseline = authored_fixture();
            if configured {
                baseline["services"][0]["encryptedRegistryUsername"] = sealed("old-user");
                baseline["services"][0]["encryptedRegistrySecret"] = sealed("old-secret");
            } else {
                baseline["services"][0]["config"]["source"]["credentials"] =
                    json!({"type": "none"});
            }
            let mut current = authored_fixture();
            current["services"][0]["config"]["source"]["credentials"]["revision"] = json!("after");
            current["services"][0]["encryptedRegistryUsername"] = sealed("new-user");
            current["services"][0]["encryptedRegistrySecret"] = sealed("new-secret");
            if path == "source" {
                current["services"][0]["config"]["source"] =
                    json!({"type": "empty", "version": 1, "rootDir": "/"});
            }
            let restored = config_request(json!({
                "operation": "restore_environment", "current": current, "baseline": baseline,
                "node_type": "service", "node_id": "00000000-0000-4000-8000-000000000002",
                "path": path
            }))
            .unwrap();
            assert_eq!(
                restored["services"][0]["config"]["source"],
                baseline["services"][0]["config"]["source"]
            );
            assert_eq!(
                restored["services"][0]["encryptedRegistryUsername"],
                baseline["services"][0]["encryptedRegistryUsername"]
            );
            assert_eq!(
                restored["services"][0]["encryptedRegistrySecret"],
                baseline["services"][0]["encryptedRegistrySecret"]
            );
        }
    }
}

#[test]
fn owned_credentials_preserve_redacted_and_disabled_states_and_reject_orphan_values() {
    use ployz_core::config::{
        SavedEnvironmentIntent, parse_environment_intent, redact_environment_intent,
    };
    let sealed = json!({"version": 1, "iv": "iv", "tag": "tag", "ciphertext": "retained"});
    let mut wire = authored_fixture();
    wire["services"][0]["config"]["source"]["credentials"] = json!({"type": "none"});
    wire["services"][0]["encryptedRegistrySecret"] = sealed.clone();
    let disabled = parse_environment_intent(wire.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&disabled).unwrap()["services"][0]["encryptedRegistrySecret"],
        sealed
    );

    let redacted = serde_json::to_value(redact_environment_intent(disabled.clone())).unwrap();
    assert_eq!(
        redacted["services"][0]["config"]["source"]["credentials"],
        json!({"type": "none"})
    );
    assert!(redacted["services"][0]["encryptedRegistrySecret"].is_null());
    assert!(serde_json::from_value::<SavedEnvironmentIntent>(redacted).is_ok());

    let mut configuration = disabled.services[0].configuration.clone();
    let before = configuration.clone();
    assert!(configuration.restore_setting(&before, "env.TOKEN").is_err());
    assert_eq!(configuration, before);
    assert_eq!(configuration.settings().name, "Web");

    wire["services"][0]["encryptedRegistryUsername"] = sealed;
    wire["services"][0]["encryptedRegistrySecret"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<SavedEnvironmentIntent>(wire.clone()).is_err());
    wire["services"][0]["encryptedRegistrySecret"] =
        json!({"version": 99, "iv": "iv", "tag": "tag", "ciphertext": "unknown"});
    assert!(serde_json::from_value::<SavedEnvironmentIntent>(wire).is_err());
}
