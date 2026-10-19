use super::*;

/// An entry Server `id` named `name`, seeing `one`, `two` and `three`.
fn entry(id: char) -> DiscoveryService {
    let mut description = test_description();
    description.machine_id = machine_id(id);
    let mut service = DiscoveryService::new(description);
    service.machines = vec![
        machine('a', "one"),
        machine('b', "two"),
        machine('c', "three"),
    ];
    service.update_unavailable = true;
    service
}

#[tokio::test]
async fn a_new_endpoint_reaches_every_other_server_when_the_target_is_off_the_mesh() {
    let two = entry('b');
    let three = entry('c');
    let (two_taken, three_taken) = (
        two.selected_endpoints.clone(),
        three.selected_endpoints.clone(),
    );
    let (two_address, _two) = serve_discovery(two).await;
    let (three_address, _three) = serve_discovery(three).await;
    let unreachable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let one_address = unreachable.local_addr().unwrap();
    drop(unreachable);
    let config = std::env::temp_dir().join(format!("ployz-endpoint-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!(
            "contexts:\n  lab:\n    connections:\n      - tcp: {one_address}\n      - tcp: {two_address}\n      - tcp: {three_address}\n"
        ),
    )
    .unwrap();
    let run = |json: bool| {
        let config = config.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .env_remove("PLOYZ_TOKEN")
                .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
                .args(["--ployz-config", config.to_str().unwrap()])
                .args(json.then_some("--json"))
                .args([
                    "server",
                    "set",
                    "one",
                    "--wg-endpoint",
                    "203.0.113.9:51820",
                    "--context",
                    "lab",
                ])
                .output()
                .await
                .unwrap()
        }
    };

    let output = run(false).await;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout.trim(),
        "two, three now dial one at 203.0.113.9:51820."
    );
    assert!(
        stderr.contains("one didn't answer, so its record may or may not have changed."),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("tcp://{one_address} unreachable.")),
        "{stderr}"
    );
    let wanted = ployz_core::SelectEndpointRequest {
        peer: machine_id('a'),
        endpoint: ployz_core::SelectedEndpoint("203.0.113.9:51820".parse().unwrap()),
    };
    assert_eq!(*two_taken.lock().unwrap(), std::slice::from_ref(&wanted));
    assert_eq!(*three_taken.lock().unwrap(), [wanted]);

    let output = run(true).await;
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.pointer("/server"), Some(&Value::Null), "{result}");
    assert_eq!(
        result.pointer("/endpoint/dialed_by"),
        Some(&json!([
            { "id": machine_id('b'), "name": "two" },
            { "id": machine_id('c'), "name": "three" },
        ])),
        "{result}"
    );
    assert_eq!(
        result.pointer("/endpoint/address"),
        Some(&json!("203.0.113.9:51820"))
    );
    assert_eq!(
        result.pointer("/endpoint/failures/0/entry"),
        Some(&json!(format!("tcp://{one_address}"))),
        "{result}"
    );
    assert_eq!(
        result.pointer("/target_error/code"),
        Some(&json!("unavailable")),
        "{result}"
    );
    let _ = std::fs::remove_file(&config);
}

#[tokio::test]
async fn a_target_that_does_not_answer_is_named_with_an_unknown_outcome() {
    let (address, _two) = serve_discovery(entry('b')).await;
    let config = std::env::temp_dir().join(format!("ployz-endpoint-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!("contexts:\n  lab:\n    connections:\n      - tcp: {address}\n"),
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .args(["--ployz-config", config.to_str().unwrap(), "--json"])
        .args([
            "server",
            "set",
            "one",
            "--label-add",
            "a=b",
            "--context",
            "lab",
        ])
        .output()
        .await
        .unwrap();
    let _ = std::fs::remove_file(&config);
    assert!(!output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.pointer("/error/message"),
        Some(&json!(
            "Server one didn't answer, so the change may or may not have landed. Check it with ployz server ls once it's back, or run this on the Server."
        )),
        "{result}"
    );
    assert_eq!(result.pointer("/error/code"), Some(&json!("unavailable")));
}

#[tokio::test]
async fn a_target_that_refuses_says_nothing_changed() {
    let mut refusing = entry('b');
    refusing.update_unavailable = false;
    refusing.update_refusal = Some(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "Cluster store is not available".into(),
        details: Value::Null,
        cause: Vec::new(),
    });
    let (address, _two) = serve_discovery(refusing).await;
    let config = std::env::temp_dir().join(format!("ployz-endpoint-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!("contexts:\n  lab:\n    connections:\n      - tcp: {address}\n"),
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .args(["--ployz-config", config.to_str().unwrap(), "--json"])
        .args([
            "server",
            "set",
            "one",
            "--label-add",
            "a=b",
            "--context",
            "lab",
        ])
        .output()
        .await
        .unwrap();
    let _ = std::fs::remove_file(&config);
    assert!(!output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.pointer("/error/message"),
        Some(&json!(
            "Server one answered but couldn't take the change, so nothing changed."
        )),
        "{result}"
    );
    assert_eq!(
        result.pointer("/error/cause/0"),
        Some(&json!("Cluster store is not available")),
        "{result}"
    );
    let retry = result
        .pointer("/error/details/retry")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        retry.contains(" server set one --label-add 'a=b' --context lab"),
        "{result}"
    );
}

#[tokio::test]
async fn an_older_entry_still_updates_the_target_it_could_not_seed() {
    let mut older = entry('b');
    older.update_unavailable = false;
    older.select_unimplemented = true;
    let (address, _older) = serve_discovery(older).await;
    let config = std::env::temp_dir().join(format!("ployz-endpoint-{}.yaml", MachineId::random()));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .args(["--ployz-config", config.to_str().unwrap()])
        .args([
            "server",
            "set",
            "one",
            "--wg-endpoint",
            "203.0.113.9:51820",
            "--connect",
            &format!("tcp://{address}"),
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stdout.trim(), "Updated Server one.", "{stderr}");
    assert!(
        stderr.contains("two needs a daemon upgrade to take it."),
        "{stderr}"
    );
}
