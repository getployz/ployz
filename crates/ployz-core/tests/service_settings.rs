#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{compare_service_settings, parse_service_config, restore_service_setting};
use serde_json::{Value, json};

fn config() -> Value {
    json!({
        "version": 2, "name": "API", "privateDns": "api",
        "source": {"version": 2, "type": "git", "repository": "acme/api",
            "repositoryId": 42, "installationId": 7, "rootDir": "/apps/api",
            "branch": {"type": "connected", "name": "main"},
            "autoDeploy": true, "waitForCi": false},
        "preDeployCommand": null, "startCommand": null,
        "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped"
    })
}

#[test]
fn service_comparison_and_restore_preserve_authored_source_identity() {
    let baseline = parse_service_config(config()).unwrap();
    let mut edited = config();
    edited["source"]["installationId"] = json!(9);
    edited["source"]["branch"] = json!({"type": "disconnected", "previousName": "main"});
    edited["startCommand"] = json!("npm start");
    let current = parse_service_config(edited).unwrap();
    let rows = compare_service_settings(&current, Some(&baseline));
    assert_eq!(
        rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
        ["source.repository", "source.branch", "startCommand"]
    );
    let restored =
        restore_service_setting(current.clone(), &baseline, "source.repository").unwrap();
    assert_eq!(
        serde_json::to_value(&restored).unwrap()["source"]["installationId"],
        7
    );
    assert_eq!(
        serde_json::to_value(&restored).unwrap()["source"]["branch"]["type"],
        "disconnected"
    );
    assert_eq!(
        serde_json::to_value(restored).unwrap()["startCommand"],
        "npm start"
    );

    let mut image = config();
    image["source"] = json!({"version": 1, "type": "image", "image": "api:latest",
        "autoUpdate": {"type": "off"}, "credentials": {"type": "configured", "revision": "opaque-revision"}});
    let image = parse_service_config(image).unwrap();
    assert_eq!(
        compare_service_settings(&image, Some(&baseline))[0].path,
        "source"
    );
    assert_eq!(
        restore_service_setting(image.clone(), &baseline, "source.branch").unwrap(),
        baseline
    );
    assert!(compare_service_settings(&image, Some(&image)).is_empty());
    assert!(restore_service_setting(current, &baseline, "unrecognized.setting").is_err());
}

#[test]
fn service_validation_normalizes_input_and_rejects_invalid_settings() {
    let mut input = config();
    input["source"]["rootDir"] = json!(" /apps/api/ ");
    let config = parse_service_config(input.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(config).unwrap()["source"]["rootDir"],
        "/apps/api"
    );
    for (path, invalid) in [
        ("replicas", json!(51)),
        ("cpuLimit", json!(0)),
        ("maxRetries", json!(-1)),
        ("startCommand", json!(" ")),
        ("privateDns", json!("Invalid_DNS")),
        ("unexpected", json!(true)),
    ] {
        let mut bad = input.clone();
        bad[path] = invalid;
        assert!(parse_service_config(bad).is_err(), "accepted {path}");
    }
}

#[test]
fn related_settings_preserve_ownership_redact_secrets_and_restore_stable_routes() {
    let mut before = config();
    before["env"] = json!({"TOKEN": {"kind":"secret","fingerprint":"before-private-fingerprint","encryptedValue":{"version":1,"iv":"iv","tag":"tag","ciphertext":"private-ciphertext"}}});
    before["mounts"] = json!([{"volumeResourceId":"11111111-1111-4111-8111-111111111111","volumeName":"data","mountPath":"/data"}]);
    before["routes"] = json!([{"id":"22222222-2222-4222-8222-222222222222","hostname":"old.example.com","targetPort":3000}]);
    let baseline = parse_service_config(before.clone()).unwrap();
    let mut after = before;
    after["env"]["TOKEN"]["fingerprint"] = json!("after-private-fingerprint");
    after["env"]["TOKEN"]["source"] = json!({"kind":"variable_group","resourceId":"33333333-3333-4333-8333-333333333333","resourceName":"Shared","variableGroupId":"44444444-4444-4444-8444-444444444444","key":"TOKEN"});
    after["mounts"][0]["volumeName"] = json!("renamed");
    after["routes"][0]["hostname"] = json!("new.example.com");
    let current = parse_service_config(after).unwrap();
    let rows = compare_service_settings(&current, Some(&baseline));
    assert_eq!(rows.len(), 2, "volume rename is not a mount edit");
    let secret = rows.iter().find(|row| row.path == "env.TOKEN").unwrap();
    assert!(secret.derived_from.is_some());
    assert!(!secret.can_restore);
    let output = serde_json::to_string(&rows).unwrap();
    assert!(!output.contains("private-"));
    let restored = restore_service_setting(
        current,
        &baseline,
        "routes.22222222-2222-4222-8222-222222222222",
    )
    .unwrap();
    assert_eq!(restored.settings.routes, baseline.settings.routes);
    assert_eq!(restored.mounts[0].volume_name, "renamed");
}

#[test]
fn canonical_references_survive_renames_and_mount_changes_keep_one_owner() {
    let mut before = config();
    before["env"] = json!({"URL":{"kind":"literal","value":"${{ old.HOST }}","parts":[{"kind":"ref","owner":{"scope":"service","lineageId":"same-owner"},"key":"HOST"}]}});
    let baseline = parse_service_config(before.clone()).unwrap();
    let mut after = before.clone();
    after["env"]["URL"]["value"] = json!("${{ renamed.HOST }}");
    assert!(
        compare_service_settings(
            &parse_service_config(after.clone()).unwrap(),
            Some(&baseline)
        )
        .is_empty()
    );
    after["env"]["URL"]["parts"][0]["owner"]["lineageId"] = json!("other-owner");
    assert_eq!(
        compare_service_settings(&parse_service_config(after).unwrap(), Some(&baseline))[0].path,
        "env.URL"
    );
    for value in [
        json!({"prefix":"api","targetPort":null}),
        json!({"prefix":"api","targetPort":8080}),
    ] {
        let mut edited = before.clone();
        edited["managedHostname"] = value;
        let current = parse_service_config(edited).unwrap();
        assert_eq!(
            compare_service_settings(&current, Some(&baseline))[0].path,
            "managedHostname"
        );
    }
    before["mounts"] =
        json!([{"volumeResourceId":"volume","volumeName":"data","mountPath":"/data"}]);
    let mounted = parse_service_config(before.clone()).unwrap();
    let added = compare_service_settings(&mounted, Some(&baseline));
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].kind, ployz_core::config::ChangeKind::Add);
    before["mounts"][0]["mountPath"] = json!("/other");
    let moved = compare_service_settings(&parse_service_config(before).unwrap(), Some(&mounted));
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].kind, ployz_core::config::ChangeKind::Update);
    let removed = compare_service_settings(&baseline, Some(&mounted));
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].kind, ployz_core::config::ChangeKind::Remove);
}
