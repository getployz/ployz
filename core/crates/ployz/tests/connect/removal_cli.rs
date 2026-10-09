use super::*;
use std::time::Duration;
use tokio::time::timeout;

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
        // Flags that accept everything ask nothing, so the loss isn't listed.
        assert!(!stderr.contains("Volumes the Cluster loses"), "{stderr}");
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
                stdout.contains("Reset left the data of Volume data on one."),
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
    assert!(stderr.contains("Server one"), "{stderr}");
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
    let output = remove_server(&service, Dial::Connect, server, env, extra).await;
    (output, service)
}

#[derive(Clone, Copy)]
enum Dial {
    Connect,
    Context,
}

async fn remove_server(
    service: &DiscoveryService,
    dial: Dial,
    server: &str,
    env: &[(&str, &str)],
    extra: &[&str],
) -> std::process::Output {
    let (address, _server) = serve_discovery(service.clone()).await;
    let config = std::env::temp_dir().join(format!("ployz-last-{}.yaml", MachineId::random()));
    let (connect, context) = match dial {
        Dial::Connect => (
            vec!["--connect".to_owned(), format!("tcp://{address}")],
            vec![],
        ),
        Dial::Context => {
            std::fs::write(
                &config,
                format!("contexts:\n  lab:\n    connections:\n      - tcp: {address}\n"),
            )
            .unwrap();
            (vec![], vec!["--context".to_owned(), "lab".to_owned()])
        }
    };
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .envs(env.iter().copied())
        .args(&connect)
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "--json",
            "server",
            "rm",
            server,
        ])
        .args(&context)
        .args(extra)
        .output()
        .await
        .unwrap();
    let _ = std::fs::remove_file(&config);
    output
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

type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;
type Reads = std::sync::Arc<std::sync::atomic::AtomicUsize>;

const ASKED: &str = r#"{"error":{"code":"approval_required","message":"A human must approve this first: this removal takes Server two out","details":{"approval_id":"apr_9","approval":"remove:abc","effects":[{"kind":"removes_server","name":"two","node":"b","path":"servers/b"}],"operation":{"verb":"remove","name":"two"}}}}"#;
fn fake_cloud(settled: &'static str, gated: bool) -> (String, Asked, Reads) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    let asked = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<(String, serde_json::Value)>::new(),
    ));
    let seen = asked.clone();
    let reads = Reads::default();
    let read = reads.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Read, Write};
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut length, mut approved) = (0, false);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                let header = header.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                approved |= header.starts_with("x-ployz-approval:");
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let (status, reply) = if line.starts_with("DELETE /api/cli/servers/") {
                let body = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push((line.trim().to_owned(), body));
                if gated && !approved {
                    ("409 Conflict", ASKED)
                } else {
                    ("200 OK", r#"{"id":"r1"}"#)
                }
            } else if line.starts_with("GET /api/cli/server-removals/r1 ") {
                read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    (cloud, asked, reads)
}

#[tokio::test]
async fn cloud_removes_its_last_server_for_the_cli() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"released"}}"#,
        false,
    );
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
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"others_remain"}}"#,
        false,
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

#[tokio::test]
async fn an_agent_gets_the_approval_a_cloud_removal_needs() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"others_remain"}}"#,
        true,
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
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("approval_required")),
        "{error}"
    );
    let retry = error
        .pointer("/error/details/retry")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        retry.starts_with("ployz server rm two --no-reset --connect tcp://")
            && retry.ends_with(" --confirm two --approval apr_9"),
        "{retry}"
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "one removal asked, none retried"
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_context_still_removes_a_server_cloud_manages_through_cloud_and_its_approval() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"released"}}"#,
        true,
    );
    let service = DiscoveryService::new(test_description());
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let output = remove_server(
        &service,
        Dial::Context,
        "one",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("approval_required")),
        "{error}"
    );
    let retry = error
        .pointer("/error/details/retry")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        retry.contains("--context lab") && retry.ends_with(" --approval apr_9"),
        "{retry}"
    );
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
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_context_removes_a_server_cloud_does_not_manage_without_cloud() {
    let (cloud, removals, _) = fake_cloud(
        r#"{"state":"succeeded","reset_warning":null,"release":{"kind":"released"}}"#,
        true,
    );
    let service = DiscoveryService::new(test_description());
    let output = remove_server(
        &service,
        Dial::Context,
        "one",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(*service.reset_machines.lock().unwrap(), [machine_id('a')]);
    assert!(
        removals.lock().unwrap().is_empty(),
        "{:?}",
        removals.lock().unwrap()
    );
}

#[tokio::test]
async fn stopping_a_followed_cloud_removal_still_drops_the_server_from_the_context() {
    let (cloud, _, reads) = fake_cloud(r#"{"state":"running"}"#, false);
    let service = DiscoveryService::new(test_description());
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let (address, _server) = serve_discovery(service.clone()).await;
    let removed = machine_id('a');
    let config = std::env::temp_dir().join(format!("ployz-stopped-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!(
            "contexts:\n  lab:\n    connections:\n      - tcp: {address}\n        machine_id: {entry}\n      - tcp: {address}\n        machine_id: {removed}\n",
            entry = test_description().machine_id,
        ),
    )
    .unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env("PLOYZ_TOKEN", "ployz_acme")
        .env("PLOYZ_CLOUD_URL", &cloud)
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "--json",
            "server",
            "rm",
            "one",
        ])
        .args([
            "--context",
            "lab",
            "--confirm",
            "one",
            "--accept-volume-loss",
            "data",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let followed = timeout(Duration::from_secs(30), async {
        while reads.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if followed.is_err() {
        let _ = std::fs::remove_file(&config);
        panic!(
            "the CLI never followed Cloud's removal: {:?}",
            child.wait_with_output().await
        );
    }
    let pid = rustix::process::Pid::from_raw(child.id().unwrap().try_into().unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let output = child.wait_with_output().await.unwrap();
    let context = std::fs::read_to_string(&config).unwrap();
    let _ = std::fs::remove_file(&config);
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("keeps running in Ployz Cloud"),
        "{output:?}"
    );
    assert!(
        !context.contains(&removed.to_string()),
        "Cloud finishes the removal, so the context drops the Server now: {context}"
    );
    assert!(
        context.contains(&test_description().machine_id.to_string()),
        "{context}"
    );
}
