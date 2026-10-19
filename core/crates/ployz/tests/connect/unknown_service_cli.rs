//! `logs` and `metrics` of a Service the selected Environment does not have now: a
//! removed Service still reads what its Servers kept, and a name nothing answers
//! for is refused with the Environment's Services.

use std::path::Path;

use ployz_core::metrics::{MetricHistoryReply, MetricPoints, ServicePoint};

use super::*;

/// `ployz ARGS` over `address`, with its own home and the in-process Store in `store`.
async fn ployz(store: &Path, address: std::net::SocketAddr, args: &[&str]) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .arg("--connect")
        .arg(format!("tcp://{address}"))
        .args(args)
        .output()
        .await
        .unwrap()
}

/// The `--json` error that ends `output`, which exited `code`.
fn refusal(output: &std::process::Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().last().unwrap_or_default();
    serde_json::from_str::<Value>(last).unwrap()["error"].clone()
}

fn readings(points: Vec<ServicePoint>) -> MetricHistoryReply {
    MetricHistoryReply {
        points: MetricPoints::Service(points),
        step_nanos: 60_000_000_000,
        gaps: Vec::new(),
        markers: Vec::new(),
    }
}

#[tokio::test]
async fn a_removed_service_still_reads_and_a_name_nothing_answers_for_is_refused() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("store.db");
    let mut service = DiscoveryService::new(test_description());
    service.kept_services = Some(["gone".to_owned()].into());
    let metrics = service.metric_replies.clone();
    let (address, server) = serve_discovery(service).await;
    for args in [
        &["project", "new", "app"][..],
        &["service", "add", "api", "--image", "nginx:1"],
    ] {
        let output = ployz(&db, address, args).await;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    }
    let refused = json!({
        "code": "not_found",
        "message": "No Service named nosvc in Environment production",
        "cause": [],
        "details": { "valid_children": ["api"] },
    });

    // Every Server's Log Store keeps a line of `gone`, which the Environment removed.
    let output = ployz(&db, address, &["logs", "gone"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with(" | shutting down\n"), "{stdout}");

    let output = ployz(&db, address, &["--json", "logs", "nosvc"]).await;
    assert_eq!(refusal(&output, 1), refused);
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    // `gone` reads, then `nosvc` is named on stderr: a partial result.
    let output = ployz(&db, address, &["--json", "logs", "gone", "nosvc"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: No Service named nosvc in Environment production\nvalid: api\n"
    );
    // The same without --json: the line printed, so the refusal makes it partial.
    let output = ployz(&db, address, &["logs", "gone", "nosvc"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with(" | shutting down\n"),
        "{output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .ends_with("error: No Service named nosvc in Environment production\nvalid: api\n"),
        "{output:?}"
    );

    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap();
    let point = ServicePoint {
        ts: now - 60_000_000_000,
        cpu_usec: Some(1.0),
        mem_bytes: Some(1_024),
        mem_limit: None,
        rx: None,
        tx: None,
        io_r: None,
        io_w: None,
    };
    metrics
        .lock()
        .unwrap()
        .insert(machine_id('a'), Ok(readings(vec![point])));
    let output = ployz(&db, address, &["metrics", "gone"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8(output.stdout).unwrap().contains("Memory"),
        "a removed Service's readings show"
    );

    metrics
        .lock()
        .unwrap()
        .insert(machine_id('a'), Ok(readings(Vec::new())));
    let output = ployz(&db, address, &["--json", "metrics", "nosvc"]).await;
    assert_eq!(refusal(&output, 1), refused);
    server.abort();
}
