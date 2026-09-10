//! Rung 2: real native Tailcat helper, private DERP, and Machine RPC contracts.
//! Requires Go; builds the pinned native prerequisites before exercising RPC.
//! Sustained retention: PLOYZ_TAILCAT_CHURN_SECONDS=120 cargo test -p ployz
//! --test tailcat_connect -- --nocapture (1–180 seconds; default eight pairs).
//! Reports successful/invalid dials, sampled peak TCP/peers, and settled retention.
#![cfg(unix)]

use std::{
    collections::BTreeSet, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, sync::Arc,
    time::Duration,
};

use ployz::{
    connect::{Connector, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections, SshDestination},
};
use ployz_core::{
    MachineRpcClient, MachineRpcServer, OpaquePayload, RuntimeWatchFrame, RuntimeWatchRequest,
    encode_runtime_watch_frame, op,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::UnixListener,
    process::Command,
};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

#[allow(dead_code, unused_imports)]
#[path = "connect/support.rs"]
mod support;

#[tokio::test]
async fn native_tailcat_confirms_machine_identity_and_performs_read_only_rpc() {
    let churn_seconds = std::env::var("PLOYZ_TAILCAT_CHURN_SECONDS")
        .map(|value| {
            let seconds: u64 = value.parse().expect("churn seconds must be an integer");
            assert!((1..=180).contains(&seconds), "churn seconds must be 1–180");
            seconds
        })
        .unwrap_or(0);
    let dir = tempfile::tempdir().unwrap();
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../native/tailcat");
    let helper = dir.path().join("ployz-tailcat");
    let fixture = dir.path().join("rpcfixture");
    assert!(
        Command::new("bash")
            .arg("build.sh")
            .arg("--test")
            .arg(&helper)
            .current_dir(&native)
            .status()
            .await
            .expect("building Tailcat requires bash and Go")
            .success()
    );
    assert!(
        Command::new("go")
            .args(["build", "-o"])
            .arg(&fixture)
            .arg("./testdata/rpcfixture.go")
            .current_dir(&native)
            .status()
            .await
            .expect("building private DERP fixture requires Go")
            .success()
    );
    let socket = dir.path().join("rpc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let description = support::test_description();
    let service = support::DiscoveryService::new(description.clone());
    let rpc = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service.clone()))
            .serve_with_incoming(UnixListenerStream::new(listener)),
    );
    let mut endpoint = Command::new(fixture)
        .arg(&socket)
        .env("IN_TS_TEST", "true")
        // Disable endpoint UDP sockets: every RPC byte must traverse local DERP.
        .env("TS_DEBUG_ALWAYS_USE_DERP", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = tokio::io::BufReader::new(endpoint.stdout.take().unwrap()).lines();
    let capability = tokio::time::timeout(Duration::from_secs(20), output.next_line())
        .await
        .expect("private DERP fixture startup timed out")
        .unwrap()
        .expect("private DERP fixture exited before publishing its capability");
    assert!(
        capability.starts_with("tc"),
        "fixture must generate the real capability format"
    );
    let parse_error = capability.parse::<Connection>().unwrap_err();
    assert!(!format!("{parse_error:?}").contains(&capability));
    let cli = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["--connect", &capability, "machine", "ls"])
        .output()
        .await
        .unwrap();
    assert!(!cli.status.success());
    assert!(!String::from_utf8_lossy(&cli.stdout).contains(&capability));
    let stderr = String::from_utf8_lossy(&cli.stderr);
    assert!(!stderr.contains(&capability));
    assert!(
        stderr.contains("Tailcat"),
        "CLI must report the fixed Tailcat configuration error"
    );
    let connector =
        Arc::new(SystemConnector::new("/ployz-missing-ssh-client").with_tailcat_program(&helper));
    let selected = |connection| SelectedConnections {
        source: ConnectionSource::Direct,
        connections: vec![connection],
    };
    let connection = Connection::tailcat(capability.clone()).unwrap();
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![
                Connection::ssh(SshDestination::parse("user@example.com").unwrap()),
                connection.clone().with_machine_id(description.machine_id),
            ],
        },
        connector.clone(),
    )
    .await
    .expect("missing SSH must fall back to native Tailcat and confirm Machine RPC");
    assert_eq!(
        client.machines().await.unwrap(),
        vec![support::machine('a', "one")]
    );

    let mismatch = connect_selected_with(
        selected(connection.with_machine_id(support::machine_id('b'))),
        connector.clone(),
    )
    .await;
    assert!(
        mismatch.is_err(),
        "unexpected Machine identity must be rejected"
    );
    let error = mismatch.err().unwrap().to_string();
    assert!(
        error.contains("identity"),
        "identity rejection must identify its cause: {error}"
    );
    assert!(!error.contains(&capability));

    let invalid = "invalid-capability-secret";
    let rejected =
        connect_selected_with(selected(Connection::tailcat(invalid).unwrap()), connector).await;
    assert!(rejected.is_err(), "invalid capability must fail closed");
    assert!(!rejected.err().unwrap().to_string().contains(invalid));
    drop(client);
    let input = endpoint.stdin.as_mut().unwrap();
    input.write_all(b"reset-clients\n").await.unwrap();
    assert_eq!(
        output.next_line().await.unwrap().as_deref(),
        Some("reset-ok")
    );

    let connection = Connection::tailcat(capability).unwrap();
    let frame: RuntimeWatchFrame = serde_json::from_str(include_str!(
        "../../ployz-core/tests/fixtures/runtime_watch_frame.json"
    ))
    .unwrap();
    service.emit_watch_frame_on_open(frame.clone());
    let expected = encode_runtime_watch_frame(&frame).unwrap();
    let mut clients = Vec::new();
    let mut streams = Vec::new();
    let mut pids = BTreeSet::new();
    for index in 0..10 {
        // exec preserves the recorded PID; each connector owns a distinct real helper.
        let wrapper = dir.path().join(format!("client-{index}"));
        std::fs::write(&wrapper, "#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec \"$(dirname \"$0\")/ployz-tailcat\" \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let channel = SystemConnector::default()
            .with_tailcat_program(wrapper.clone())
            .connect(&connection)
            .await
            .unwrap();
        let mut client = MachineRpcClient::new(channel);
        let mut siblings = Vec::new();
        for _ in 0..5 {
            let request = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
                .encode()
                .unwrap();
            siblings.push(client.runtime_watch(request).await.unwrap().into_inner());
        }
        let pid = std::fs::read_to_string(wrapper.with_extension("pid")).unwrap();
        assert!(
            pids.insert(pid),
            "clients must use independent helper processes"
        );
        clients.push(client);
        streams.push(siblings);
    }
    assert_frames(&mut streams, &expected).await;
    assert_eq!(service.live_watch_senders(), 50);
    input.write_all(b"clients\n").await.unwrap();
    let count: usize = output.next_line().await.unwrap().unwrap().parse().unwrap();
    assert_eq!(
        count, 10,
        "each helper must have its own key-derived remote address"
    );
    input.write_all(b"churn\n").await.unwrap();

    // Cancel one HTTP/2 stream, leaving its four siblings and other processes live.
    drop(streams.first_mut().unwrap().remove(0));
    wait_for_watch_count(&service, 49).await;
    service.push_watch_frame(frame.clone());
    assert_frames(&mut streams, &expected).await;

    let killed_pid = std::fs::read_to_string(dir.path().join("client-1.pid")).unwrap();
    assert!(
        Command::new("kill")
            .args(["-KILL", killed_pid.trim()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let dead_streams = streams.remove(1);
    for mut stream in dead_streams {
        let end = tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .expect("terminated helper streams must end promptly");
        assert!(
            !matches!(end, Ok(Some(_))),
            "terminated helper cannot deliver another frame"
        );
    }
    clients.remove(1);
    // Successful and wrong-PSK churn runs while these 44 streams remain open.
    let report = tokio::time::timeout(Duration::from_secs(churn_seconds + 30), output.next_line())
        .await
        .expect("bounded churn timed out")
        .unwrap()
        .expect("fixture exited during churn");
    let counts: Vec<usize> = report
        .strip_prefix("churn-ok ")
        .expect("fixture must finish churn")
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    let [
        successful,
        invalid,
        peak_tcp,
        peak_peers,
        settled_tcp,
        settled_peers,
    ]: [usize; 6] = counts.try_into().expect("fixture must report six counts");
    assert!(successful >= 8);
    assert_eq!(successful, invalid);
    eprintln!(
        "Tailcat churn: successful={} invalid={} sampled_peak_tcp={} sampled_peak_peers={} settled_tcp={} settled_peers={}",
        successful, invalid, peak_tcp, peak_peers, settled_tcp, settled_peers
    );
    assert!(
        peak_tcp <= 12,
        "transient TCP overlap exceeded one dial plus cleanup"
    );
    assert!(peak_peers <= 16, "peer admission exceeded limit");
    assert_eq!(
        settled_tcp, 9,
        "TCP churn connections were retained after DrainTCP"
    );
    assert_eq!(
        settled_peers, 9,
        "peer churn connections were retained after DrainTCP"
    );
    wait_for_endpoint_state(input, &mut output, 9).await;
    service.push_watch_frame(frame);
    assert_frames(&mut streams, &expected).await;
    drop(streams);
    drop(clients);
    wait_for_endpoint_state(input, &mut output, 0).await;
    endpoint.start_kill().unwrap();
    endpoint.wait().await.unwrap();
    rpc.abort();
}

async fn wait_for_endpoint_state(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    expected: usize,
) {
    let mut last = String::new();
    let complete = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            input.write_all(b"state\n").await.unwrap();
            last = output.next_line().await.unwrap().expect("fixture exited");
            if last == format!("{expected} {expected}") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        complete.is_ok(),
        "server must retain exactly {expected} active TCP connections and peers; last state: {last}"
    );
    eprintln!("Tailcat settled retention: tcp={expected} peers={expected}");
}

async fn assert_frames(
    streams: &mut [Vec<tonic::Streaming<OpaquePayload>>],
    expected: &OpaquePayload,
) {
    for siblings in streams {
        for stream in siblings {
            let frame = tokio::time::timeout(Duration::from_secs(5), stream.message())
                .await
                .expect("live sibling stream stalled")
                .unwrap()
                .expect("live sibling stream ended");
            assert_eq!(&frame, expected);
        }
    }
}

async fn wait_for_watch_count(service: &support::DiscoveryService, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.live_watch_senders() != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stream cancellation must reach the RPC server");
}
