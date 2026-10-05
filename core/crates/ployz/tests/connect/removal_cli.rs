use super::*;

#[tokio::test]
async fn machine_removal_reports_complete_and_partial_results() {
    for (warning, invalid_config) in [
        (None, false),
        (Some("Docker reset failed: busy volume"), false),
        (None, true),
        (Some("Docker reset failed: busy volume"), true),
    ] {
        let service = DiscoveryService::new(test_description());
        *service.reset_warning.lock().unwrap() = warning.map(str::to_owned);
        let resets = service.reset_machines.clone();
        let erased = service.removed_volumes.clone();
        let (address, server) = serve_discovery(service).await;
        let config = std::env::temp_dir().join(format!(
            "ployz-removal-invalid-{}.yaml",
            MachineId::random()
        ));
        if invalid_config {
            std::fs::write(&config, "contexts: [invalid]").unwrap();
        }
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "--ployz-config",
                config.to_str().unwrap(),
                "server",
                "rm",
                "one",
                "--confirm",
                "one",
                "--accept-volume-loss",
                "data",
            ])
            .output()
            .await
            .unwrap();
        if invalid_config {
            std::fs::remove_file(config).unwrap();
        }
        assert_eq!(
            output.status.success(),
            warning.is_none() && !invalid_config,
            "{output:?}"
        );
        assert_eq!(*resets.lock().unwrap(), [machine_id('a')]);
        assert!(erased.lock().unwrap().is_empty());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("Removed Server one"), "{stdout}");
        assert!(
            stdout.contains(
                "Volumes the Cluster loses (1); only the Server's disk keeps their data:"
            ),
            "{stdout}"
        );
        assert!(
            !stdout.contains("Permanently delete") && !stdout.contains("Deleted volume"),
            "{stdout}"
        );
        if invalid_config {
            assert!(
                stderr.contains("error: Server removed; local context cleanup failed."),
                "{stderr}"
            );
        }
        if let Some(warning) = warning {
            assert!(stderr.contains(warning), "{stderr}");
            assert!(!stdout.contains("Deleted volume"), "{stdout}");
        } else {
            assert!(
                stdout.contains("Volume data was not erased by reset: data on"),
                "{stdout}"
            );
        }
        assert!(!stderr.contains("No changes made"), "{stderr}");
        server.abort();
    }
}

#[tokio::test]
async fn machine_reset_refuses_failed_service_observation_before_mutation() {
    let service = DiscoveryService::new(test_description());
    service.container_list_outcomes.lock().unwrap().insert(
        machine_id('a'),
        VecDeque::from([Err(Status::internal("container inventory failed"))]),
    );
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "server",
            "rm",
            "one",
            "--confirm",
            "one",
            "--accept-volume-loss",
            "data",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(resets.lock().unwrap().is_empty());
    assert!(removals.lock().unwrap().is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("container inventory failed"), "{stderr}");
    assert!(stderr.contains(machine_id('a').as_str()), "{stderr}");
    assert!(stderr.contains("No changes made"), "{stderr}");
    server.abort();
}

#[tokio::test]
async fn server_removal_without_the_typed_name_names_what_goes_and_the_retry() {
    let service = DiscoveryService::new(test_description());
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let connect = format!("tcp://{address}");
    let run = |extra: &'static [&'static str]| {
        let connect = connect.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .args(["--json", "--connect", &connect, "server", "rm", "one"])
                .args(extra)
                .output()
                .await
                .unwrap()
        }
    };

    let output = run(&[]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let at = |pointer: &str| error.pointer(pointer).unwrap().clone();
    assert_eq!(at("/error/code"), "confirmation_required", "{error}");
    assert_eq!(at("/error/details/server/name"), "one", "{error}");
    assert_eq!(at("/error/details/data_loss/0/id/name"), "data", "{error}");
    let next = at("/error/details/retry");
    let next = next.as_str().unwrap();
    assert!(next.starts_with("ployz server rm one --connect"), "{next}");
    assert!(
        next.ends_with("--confirm one --accept-volume-loss data"),
        "{next}"
    );

    let output = run(&["--confirm", "One"]).await;
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{error}"
    );

    assert!(resets.lock().unwrap().is_empty());
    assert!(removals.lock().unwrap().is_empty());
    server.abort();
}

/// `server rm` of the last Server, which Cloud holds, run with `env` on top of a clean one.
async fn remove_last_cloud_server(
    env: &[(&str, &str)],
    extra: &[&str],
) -> (std::process::Output, DiscoveryService) {
    remove_cloud_server(DiscoveryService::new(test_description()), "one", env, extra).await
}

/// `server rm SERVER` in `service`'s Cluster, every Server of which Cloud holds.
async fn remove_cloud_server(
    service: DiscoveryService,
    server: &str,
    env: &[(&str, &str)],
    extra: &[&str],
) -> (std::process::Output, DiscoveryService) {
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let (address, _server) = serve_discovery(service.clone()).await;
    let config = std::env::temp_dir().join(format!("ployz-last-{}.yaml", MachineId::random()));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .envs(env.iter().copied())
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "--ployz-config",
            config.to_str().unwrap(),
            "--json",
            "server",
            "rm",
            server,
        ])
        .args(extra)
        .output()
        .await
        .unwrap();
    (output, service)
}

#[tokio::test]
async fn last_cloud_managed_server_asks_for_cloud_before_any_confirmation() {
    let (output, service) = remove_last_cloud_server(&[], &[]).await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("conflict")),
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/details/next"),
        Some(&json!("ployz login"))
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

/// Cloud's `/api/cli`: a Server removal it's asked for (recorded with its body) settles
/// at once as `settled`; anything else (the Store's Volume names) isn't offered.
type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;
fn fake_cloud(settled: &'static str) -> (String, Asked) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    let asked = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<(String, serde_json::Value)>::new(),
    ));
    let seen = asked.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Read, Write};
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let (status, reply) = if line.starts_with("DELETE /api/cli/servers/") {
                let body = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push((line.trim().to_owned(), body));
                ("200 OK", r#"{"id":"r1"}"#)
            } else if line.starts_with("GET /api/cli/server-removals/r1 ") {
                ("200 OK", settled)
            } else {
                ("404 Not Found", r#"{"code":"NOT_FOUND"}"#)
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });

    (cloud, asked)
}

#[tokio::test]
async fn cloud_removes_its_last_server_for_the_cli() {
    let (cloud, asked) =
        fake_cloud(r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"released"}}"#);
    let (output, service) = remove_last_cloud_server(
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // Cloud reset it, not the CLI, so Cloud saw it go and let go of it.
    assert!(service.reset_machines.lock().unwrap().is_empty());
    let asked = asked.lock().unwrap();
    let [(line, body)] = asked.as_slice() else {
        panic!("Cloud was asked {asked:?}");
    };
    assert!(
        line.starts_with(&format!("DELETE /api/cli/servers/{} ", machine_id('a'))),
        "{line}"
    );
    assert_eq!(
        body.pointer("/confirm_data_loss/confirmed/0/id/name"),
        Some(&json!("data")),
        "{body}"
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.get("next"),
        Some(&json!("ployz server add")),
        "{result}"
    );
    assert_eq!(result.get("cloud_released"), Some(&json!(true)), "{result}");
    assert!(
        result
            .pointer("/warnings/0")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|warning| warning.contains("is the last Server")),
        "{result}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is the last Server: whatever runs on it stops"),
        "{stderr}"
    );
}

#[tokio::test]
async fn a_server_cloud_manages_leaves_through_cloud_even_without_a_reset() {
    let (cloud, asked) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"others_remain"}}"#,
    );
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('b', "two"));
    let (output, service) = remove_cloud_server(
        service,
        "two",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--no-reset", "--confirm", "two"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // Cloud took it out, not the CLI, so Cloud dropped its row for it.
    assert!(service.removed_machines.lock().unwrap().is_empty());
    let asked = asked.lock().unwrap();
    let [(line, body)] = asked.as_slice() else {
        panic!("Cloud was asked {asked:?}");
    };
    assert!(
        line.starts_with(&format!("DELETE /api/cli/servers/{} ", machine_id('b'))),
        "{line}"
    );
    assert_eq!(body, &json!({ "no_reset": true }));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.get("cloud_released"),
        Some(&json!(false)),
        "{result}"
    );
    assert_eq!(result.get("next"), Some(&json!(null)), "{result}");
}
