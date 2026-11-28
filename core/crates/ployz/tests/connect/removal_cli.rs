use super::*;
use std::time::Duration;
use tokio::time::timeout;

/// The last Server is forgotten with its Cluster: nothing is sent to it, and the output says
/// it still runs its containers and how to stop them. A failed context cleanup comes after
/// the removal, which already went through.
#[tokio::test]
async fn removing_the_last_server_sends_it_nothing_and_says_it_still_runs() {
    for invalid_config in [false, true] {
        let service = DiscoveryService::new(test_description());
        let resets = service.reset_machines.clone();
        let removals = service.removed_machines.clone();
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
            .arg("--why=test")
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
        assert_eq!(output.status.success(), !invalid_config, "{output:?}");
        assert!(resets.lock().unwrap().is_empty());
        assert!(removals.lock().unwrap().is_empty());
        assert!(erased.lock().unwrap().is_empty());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("Removed Server one."), "{stdout}");
        assert!(
            stdout.contains("Volume data was only on one; the Cluster no longer has it."),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Server one still runs its containers. To stop them, run sudo ployz-uninstall on it, or ployz server add --reset to use it again."
            ),
            "{stdout}"
        );
        if invalid_config {
            assert!(
                stderr.contains("error: Server removed; local context cleanup failed."),
                "{stderr}"
            );
        }
        assert!(!stderr.contains("No changes made"), "{stderr}");
        server.abort();
    }
}

/// Removing a Server takes it out of the Cluster: another Server forgets it, which deletes
/// its row and refuses its key, and nothing is sent to the Server itself. It asks only for
/// the Volumes no other Server holds.
#[tokio::test]
async fn removing_a_server_forgets_it_through_another_and_never_resets_it() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service.storage_capacity = Some(empty_storage());
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let output = remove_server(
        &service,
        Dial::Connect,
        "two",
        &[],
        &["--confirm", "two", "--accept-volume-loss", "data"],
    )
    .await;
    assert!(output.status.success(), "{output:?}");
    assert!(resets.lock().unwrap().is_empty());
    assert_eq!(*removals.lock().unwrap(), [machine_id('c')]);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.pointer("/data_loss/0/id/name"), Some(&json!("data")));
    assert_eq!(result.get("still_running"), Some(&json!(false)), "{result}");
}

/// A Server holding no Volume only it has asks only for its name.
#[tokio::test]
async fn removing_a_server_without_a_sole_copy_needs_only_its_name() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service.storage_capacity = Some(empty_storage());
    service
        .listed_volumes
        .lock()
        .unwrap()
        .insert(machine_id('c'), vec![]);
    let removals = service.removed_machines.clone();
    let output = remove_server(&service, Dial::Connect, "two", &[], &["--confirm", "two"]).await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(*removals.lock().unwrap(), [machine_id('c')]);
    assert!(service.reset_machines.lock().unwrap().is_empty());
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
                .arg("--why=test")
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
    assert_eq!(at("/error/details/answered"), true, "{error}");
    assert_eq!(at("/error/details/data_loss/0/id/name"), "data", "{error}");
    let next = at("/error/details/retry");
    let next = next.as_str().unwrap();
    assert!(next.starts_with("ployz --connect "), "{next}");
    assert!(
        next.ends_with(" server rm one --confirm one --accept-volume-loss data"),
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
    leave_cloud_server("rm", service, server, env, extra).await
}

async fn forget_cloud_server(
    service: DiscoveryService,
    server: &str,
    env: &[(&str, &str)],
    extra: &[&str],
) -> (std::process::Output, DiscoveryService) {
    leave_cloud_server("forget", service, server, env, extra).await
}

async fn leave_cloud_server(
    verb: &str,
    service: DiscoveryService,
    server: &str,
    env: &[(&str, &str)],
    extra: &[&str],
) -> (std::process::Output, DiscoveryService) {
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let output = leave_server(verb, &service, Dial::Connect, server, env, extra).await;
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
    leave_server("rm", service, dial, server, env, extra).await
}

async fn leave_server(
    verb: &str,
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
        .arg("--why=test")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .envs(env.iter().copied())
        .args(&connect)
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "--json",
            "server",
            verb,
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
type Reads = std::sync::Arc<tokio::sync::Notify>;

const ASKED: &str = r#"{"error":{"code":"approval_required","message":"A human must approve this first: this removal takes Server two out","details":{"approval":"remove:abc","effects":[{"kind":"removes_server","name":"two","node":"b","path":"servers/b"}],"operation":{"verb":"remove","name":"two"}}}}"#;
const ASKED_FORGET: &str = r#"{"error":{"code":"approval_required","message":"A human must approve this first: this forget takes Server two out","details":{"approval":"forget:abc","effects":[{"kind":"removes_server","name":"two","node":"b","path":"servers/b"}],"operation":{"verb":"forget","name":"two"}}}}"#;
fn fake_cloud(settled: &'static str, gated: bool) -> (String, Asked, Reads) {
    fake_cloud_settling("200 OK", settled, gated)
}

/// [`fake_cloud`] whose removal reads answer `status`, as Cloud refuses a removal that ended without succeeding.
fn fake_cloud_settling(
    status: &'static str,
    settled: &'static str,
    gated: bool,
) -> (String, Asked, Reads) {
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
            let starts = line.starts_with("DELETE /api/cli/servers/")
                || (line.starts_with("POST /api/cli/servers/") && line.contains("/forget "));
            let (status, reply) = if starts {
                let body = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push((line.trim().to_owned(), body));
                if gated && !approved {
                    let forgets = line.contains("/forget ");
                    ("409 Conflict", if forgets { ASKED_FORGET } else { ASKED })
                } else {
                    ("200 OK", r#"{"id":"r1"}"#)
                }
            } else if line.starts_with("GET /api/cli/server-removals/r1 ") {
                read.notify_one();
                (status, settled)
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
        r#"{"state":"succeeded","release":{"kind":"released"}}"#,
        false,
    );
    let (output, service) = remove_last_cloud_server(
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // Cloud let go of it, not the CLI, and nothing was sent to the Server.
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
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
    assert_eq!(body.get("server_name"), Some(&json!("one")), "{body}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.get("next"),
        Some(&json!("ployz server add")),
        "{result}"
    );
    assert_eq!(result.get("cloud_released"), Some(&json!(true)), "{result}");
    assert_eq!(result.get("still_running"), Some(&json!(true)), "{result}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is the last Server: removing it lets go of the Cluster"),
        "{stderr}"
    );
}

/// `server rm` of the last Server whose Cloud removal ended `failed / retry_exhausted`, as
/// Cloud refuses it on `GET server-removals/:id`. Its error, after a failed exit.
async fn removal_ran_out(refusal: &'static str) -> serde_json::Value {
    let (cloud, _, _) = fake_cloud_settling("503 Service Unavailable", refusal, false);
    let (output, _) = remove_last_cloud_server(
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    assert_ne!(output.status.code(), Some(0), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("unavailable")),
        "{error}"
    );
    error
}

fn message(error: &serde_json::Value) -> &str {
    error
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn a_sent_cloud_removal_whose_retries_ran_out_checks_the_server_before_forgetting_it() {
    let error = removal_ran_out(
        r#"{"error":{"code":"unavailable","message":"Machine remove retries were exhausted.","details":{"unfinished":true}}}"#,
    )
    .await;
    let message = message(&error);
    assert!(message.contains("didn't finish"), "{message}");
    // A step after the forget may be the one that ran out: the Server may be gone and
    // Cloud's row dropped, so the CLI claims neither way.
    for unbacked in ["unknown", "answered", "Cloud still lists", "Removed"] {
        assert!(!message.contains(unbacked), "{message}");
    }
    assert!(
        message.contains("If the Server is gone but `ployz server ls` still lists it"),
        "{message}"
    );
    assert_eq!(
        error.pointer("/error/details/next"),
        Some(&json!("ployz server ls")),
        "{error}"
    );
}

#[tokio::test]
async fn a_cloud_removal_that_sent_nothing_says_only_its_failure() {
    let error = removal_ran_out(
        r#"{"error":{"code":"unavailable","message":"Machine remove retries were exhausted.","details":null}}"#,
    )
    .await;
    assert_eq!(message(&error), "Machine remove retries were exhausted.");
    assert_eq!(error.pointer("/error/details/next"), None, "{error}");
}

#[tokio::test]
async fn a_server_cloud_manages_is_forgotten_through_cloud() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","release":{"kind":"others_remain"}}"#,
        false,
    );
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service.storage_capacity = Some(empty_storage());
    let (output, service) = forget_cloud_server(
        service,
        "two",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "two", "--accept-volume-loss", "data"],
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
        line.starts_with(&format!(
            "POST /api/cli/servers/{}/forget ",
            machine_id('c')
        )),
        "{line}"
    );
    assert_eq!(body, &json!({ "server_name": "two" }));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.get("cloud_released"),
        Some(&json!(false)),
        "{result}"
    );
    assert_eq!(result.get("next"), Some(&json!(null)), "{result}");
    // Ployz no longer warns about what a forgotten Server keeps on its disk.
    assert_eq!(result.get("warnings"), None, "{result}");
}

#[tokio::test]
async fn under_json_a_cloud_removal_names_the_retry_that_carries_its_digest() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","release":{"kind":"others_remain"}}"#,
        true,
    );
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service.storage_capacity = Some(empty_storage());
    let (output, service) = forget_cloud_server(
        service,
        "two",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "two", "--accept-volume-loss", "data"],
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
        retry.starts_with("ployz --connect tcp://")
            && retry.ends_with(
                " server forget two --confirm two --accept-volume-loss data --approval forget:abc"
            ),
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
async fn a_context_still_removes_a_server_cloud_manages_through_cloud_and_its_digest() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","release":{"kind":"released"}}"#,
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
        retry.contains("--context lab") && retry.ends_with(" --approval remove:abc"),
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
        r#"{"state":"succeeded","release":{"kind":"released"}}"#,
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
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(
        removals.lock().unwrap().is_empty(),
        "{:?}",
        removals.lock().unwrap()
    );
}

#[tokio::test]
async fn the_entry_server_is_removed_through_another_server() {
    let daemon = |id: char| {
        let mut service = DiscoveryService::new(ContractDescription {
            machine_id: machine_id(id),
            ..test_description()
        });
        service.machines = vec![machine('a', "one"), machine('b', "two")];
        service
    };
    let (entry, other) = (daemon('a'), daemon('b'));
    let (entry_address, _entry) = serve_discovery(entry.clone()).await;
    let (other_address, _other) = serve_discovery(other.clone()).await;
    let config = std::env::temp_dir().join(format!("ployz-entry-{}.yaml", MachineId::random()));
    std::fs::write(
        &config,
        format!(
            "contexts:\n  lab:\n    connections:\n      - tcp: {entry_address}\n        machine_id: {}\n      - tcp: {other_address}\n        machine_id: {}\n",
            machine_id('a'),
            machine_id('b'),
        ),
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .args(["--ployz-config", config.to_str().unwrap(), "--json"])
        .args(["server", "rm", "one", "--context", "lab"])
        .args(["--confirm", "one", "--accept-volume-loss", "data"])
        .output()
        .await
        .unwrap();
    let context = std::fs::read_to_string(&config).unwrap();
    let _ = std::fs::remove_file(&config);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(entry.reset_machines.lock().unwrap().is_empty());
    assert!(other.reset_machines.lock().unwrap().is_empty());
    assert_eq!(*other.removed_machines.lock().unwrap(), [machine_id('a')]);
    assert!(!context.contains(&machine_id('a').to_string()), "{context}");
    assert!(context.contains(&machine_id('b').to_string()), "{context}");
}

#[tokio::test]
async fn an_entry_server_dialled_alone_names_connecting_through_another() {
    let mut service = DiscoveryService::new(ContractDescription {
        machine_id: machine_id('a'),
        ..test_description()
    });
    service.machines = vec![machine('a', "one"), machine('b', "two")];
    let output = remove_server(
        &service,
        Dial::Connect,
        "one",
        &[],
        &["--confirm", "one", "--accept-volume-loss", "data"],
    )
    .await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error.pointer("/error/code"), Some(&json!("conflict")));
    assert!(
        error
            .pointer("/error/message")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Connect through another Server"),
        "{error}"
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stopping_a_followed_cloud_removal_keeps_the_server_in_the_context() {
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
        .arg("--why=test")
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
    let followed = timeout(Duration::from_secs(30), reads.notified()).await;
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
        context.contains(&removed.to_string()),
        "Cloud's removal may still fail, so the context keeps the Server: {context}"
    );
    assert!(
        context.contains(&test_description().machine_id.to_string()),
        "{context}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn at_a_terminal_a_managed_removal_dialled_directly_shows_what_cloud_loses() {
    use tokio::io::AsyncWriteExt;
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","release":{"kind":"released"}}"#,
        false,
    );
    let service = DiscoveryService::new(test_description());
    *service.management_clients.lock().unwrap() =
        vec![ployz_core::ManagementClientLabel::parse("cloud").unwrap()];
    let (address, server) = serve_discovery(service.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let command = shell_words::join([
        env!("CARGO_BIN_EXE_ployz").to_owned(),
        "--why=test".into(),
        "--connect".into(),
        format!("tcp://{address}"),
        "--ployz-config".into(),
        home.path()
            .join("config.yaml")
            .to_string_lossy()
            .into_owned(),
        "server".into(),
        "rm".into(),
        "one".into(),
    ]);
    let mut child = tokio::process::Command::new("script")
        .kill_on_drop(true)
        .env("TERM", "xterm")
        .env_remove("CI")
        .env("PLOYZ_TOKEN", "ployz_acme")
        .env("PLOYZ_CLOUD_URL", cloud)
        .args(["--quiet", "--return", "--command", &command, "/dev/null"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"one\n")
        .await
        .unwrap();
    let output = timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let screen = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{screen}");
    assert!(
        screen.contains("Removing Server one through Ployz Cloud:"),
        "{screen}"
    );
    assert!(!screen.contains("through a direct connection"), "{screen}");
    assert!(!screen.contains("connected Server can see"), "{screen}");
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert!(service.reset_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn a_server_that_does_not_answer_is_forgotten_only_when_dead_is_typed() {
    let mut service = DiscoveryService::new(test_description());
    let mut gone = machine('b', "two");
    gone.membership = MembershipObservation::Down;
    service.machines.push(gone);
    service.machines.push(machine('c', "three"));
    service.storage_capacity = Some(empty_storage());
    let resets = service.reset_machines.clone();
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let connect = format!("tcp://{address}");
    let run = |server: &'static str, extra: &'static [&'static str]| {
        let connect = connect.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .arg("--why=test")
                .env(TARGET_READ_TIMEOUT, "1")
                .args(["--json", "--connect", &connect, "server", "forget", server])
                .args(extra)
                .output()
                .await
                .unwrap()
        }
    };
    let error_of = |output: &std::process::Output| -> Value {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"))
    };

    let output = run("two", &[]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error = error_of(&output);
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "confirmation_required",
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/message").unwrap(),
        DEAD_REFUSAL,
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/details/answered").unwrap(),
        false,
        "{error}"
    );
    let retry = error
        .pointer("/error/details/retry")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(retry.starts_with("ployz --connect "), "{retry}");
    assert!(
        retry.ends_with(" server forget two --confirm dead"),
        "{retry}"
    );

    let output = run("two", &["--confirm", "two"]).await;
    let error = error_of(&output);
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{error}"
    );
    assert!(
        error
            .pointer("/error/message")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("is not dead"),
        "{error}"
    );

    let output = run("three", &["--confirm", "dead"]).await;
    let error = error_of(&output);
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "invalid_argument",
        "{error}"
    );
    assert!(removals.lock().unwrap().is_empty());

    let output = run("two", &["--confirm", "dead"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("warnings"), None, "{result}");
    assert_eq!(*removals.lock().unwrap(), [machine_id('b')]);
    assert!(resets.lock().unwrap().is_empty());
    server.abort();
}

/// The entry no longer lists the Server, so an earlier forget finished.
#[tokio::test]
async fn a_server_the_entry_no_longer_lists_is_already_forgotten() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('b', "two"));
    service.storage_unlisted = Some(machine_id('b'));
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args([
            "--json",
            "--connect",
            &format!("tcp://{address}"),
            "server",
            "forget",
            "two",
            "--confirm",
            "two",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Server two was already forgotten. Nothing to do.\n"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.get("already_forgotten"),
        Some(&json!(true)),
        "{result}"
    );
    assert_eq!(result.pointer("/server/machine/name"), Some(&json!("two")));
    let mut fields = result
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    fields.sort();
    assert_eq!(fields, ["already_forgotten", "server"], "{result}");
    assert!(removals.lock().unwrap().is_empty());
    server.abort();
}

/// Each forget gives a silent Server one second, not the default ten.
const TARGET_READ_TIMEOUT: &str = "PLOYZ_TARGET_READ_TIMEOUT_SECONDS";

const DEAD_REFUSAL: &str = "Server two did not answer in 1s, so Ployz can't list the Volumes only it holds, and needs dead typed. No changes made.";

#[tokio::test]
async fn a_server_listed_up_that_never_replies_is_forgotten_when_dead_is_typed() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('b', "two"));
    service.storage_hangs_after = Some(0);
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;
    let connect = format!("tcp://{address}");
    let run = |extra: &'static [&'static str]| {
        let connect = connect.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
                .arg("--why=test")
                .env(TARGET_READ_TIMEOUT, "1")
                .args(["--json", "--connect", &connect, "server", "forget", "two"])
                .args(extra)
                .output()
                .await
                .unwrap()
        }
    };

    let output = run(&[]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"));
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "confirmation_required",
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/message").unwrap(),
        DEAD_REFUSAL,
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/details/answered").unwrap(),
        false,
        "{error}"
    );
    assert!(removals.lock().unwrap().is_empty());

    let output = run(&["--confirm", "dead"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(*removals.lock().unwrap(), [machine_id('b')]);
    server.abort();
}

#[tokio::test]
async fn a_server_that_stops_answering_after_its_name_is_typed_is_kept() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service.storage_capacity = Some(empty_storage());
    service.storage_hangs_after = Some(1);
    let removals = service.removed_machines.clone();
    let (address, server) = serve_discovery(service).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .env(TARGET_READ_TIMEOUT, "1")
        .args(["--json", "--connect", &format!("tcp://{address}")])
        .args([
            "server",
            "forget",
            "two",
            "--confirm",
            "two",
            "--accept-volume-loss",
            "data",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"));
    assert_eq!(
        error.pointer("/error/code").unwrap(),
        "confirmation_required",
        "{error}"
    );
    assert_eq!(
        error.pointer("/error/details/answered").unwrap(),
        false,
        "{error}"
    );
    let retry = error
        .pointer("/error/details/retry")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(
        retry.ends_with(" server forget two --confirm dead"),
        "{retry}"
    );
    assert!(removals.lock().unwrap().is_empty());
    server.abort();
}

/// A Server that answers holding Volume copies is forgotten like any other: Ployz no longer
/// guards what is left on its disk.
#[tokio::test]
async fn a_server_that_answers_holding_copies_is_forgotten_through_cloud() {
    let (cloud, asked, _) = fake_cloud(
        r#"{"state":"succeeded","release":{"kind":"others_remain"}}"#,
        false,
    );
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('c', "two"));
    service
        .listed_volumes
        .lock()
        .unwrap()
        .insert(machine_id('c'), vec![]);
    let bytes = ployz_core::ProvisionedVolumeMaximumBytes::new(
        std::num::NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
    );
    let docker = |name: &str| ployz_core::DockerVolumeName::parse(name).unwrap();
    service.storage_capacity = Some(ployz_core::StorageCapacity {
        backing: ployz_core::StorageBacking::Unallocated {
            host_total_bytes: 100 * ployz_core::STORAGE_GIB,
            host_available_bytes: 90 * ployz_core::STORAGE_GIB,
        },
        unmanaged_used_bytes: 0,
        slot_bound_bytes: 0,
        volumes: BTreeMap::new(),
        copies: BTreeMap::from([(
            docker("app-prod_logs"),
            ployz_core::ProvisionedCopy::new(ployz_core::CopyPlace::Slot(None), bytes, 0),
        )]),
    });
    let (output, service) = forget_cloud_server(
        service,
        "two",
        &[("PLOYZ_TOKEN", "ployz_acme"), ("PLOYZ_CLOUD_URL", &cloud)],
        &["--confirm", "two"],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(asked.lock().unwrap().len(), 1, "Cloud was asked {asked:?}");
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn server_rm_input_errors_name_servers_and_volumes() {
    let mut service = DiscoveryService::new(test_description());
    service.machines = vec![machine('a', "machine-1")];
    let refused = remove_server(&service, Dial::Connect, "machine-1", &[], &[]).await;
    cli_transcript(&refused);
    let json: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused.status.code(), Some(2), "{json}");
    assert!(
        message(&json).contains("machine-1 holds the only copy of Volumes: data"),
        "{json}"
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn server_rm_without_volumes_explains_unneeded_acceptance() {
    let mut service = DiscoveryService::new(test_description());
    service.machines = vec![machine('a', "machine-1")];
    service
        .listed_volumes
        .lock()
        .unwrap()
        .insert(machine_id('a'), vec![]);
    let refused = remove_server(
        &service,
        Dial::Connect,
        "machine-1",
        &[],
        &["--confirm", "machine-1", "--accept-volume-loss", "data"],
    )
    .await;
    cli_transcript(&refused);
    let json: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused.status.code(), Some(2), "{json}");
    assert_eq!(
        json.pointer("/error/code"),
        Some(&json!("invalid_argument"))
    );
    assert!(
        message(&json).contains("Server machine-1 has no Volumes requiring loss acceptance"),
        "{json}"
    );
    assert!(
        message(&json).contains("drop --accept-volume-loss"),
        "{json}"
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn server_rm_warns_when_approval_does_not_apply() {
    let mut service = DiscoveryService::new(test_description());
    service.machines = vec![machine('a', "machine-1")];
    let output = remove_server(
        &service,
        Dial::Connect,
        "machine-1",
        &[],
        &[
            "--confirm",
            "machine-1",
            "--accept-volume-loss",
            "data",
            "--approval",
            "xyz",
        ],
    )
    .await;
    cli_transcript(&output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{json}");
    assert!(
        json.get("warnings")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .any(|warning| warning.as_str()
                == Some("--approval applies only when Ployz Cloud manages the Server.")),
        "{json}"
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
}
