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
        stderr.contains("one unreachable; its record is unchanged."),
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
