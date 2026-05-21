use std::io::Read;
use std::net::TcpStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use mvp_deploy::{InstanceId, RevisionId};
use mvp_projection::ServiceName;
use mvp_runtime::{ProcessInstanceSpec, ProcessRuntime, RuntimeState};

#[test]
fn process_runtime_starts_drains_stops_and_rediscovers_http_service() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime =
        ProcessRuntime::managed_http(temp.path(), env!("CARGO_BIN_EXE_mvp-runtime-http-child"));
    let spec = ProcessInstanceSpec::new(
        InstanceId::new("instance-a"),
        ServiceName::new("web"),
        RevisionId::new("rev-1"),
    );

    let prepared = runtime.prepare(&spec).expect("prepare");
    assert_eq!(prepared.state, RuntimeState::Prepared);

    let started = runtime.start(&spec).expect("start");
    assert_eq!(started.state, RuntimeState::Running);
    assert_response_contains(&started.address, "instance=instance-a");

    let restarted_runtime =
        ProcessRuntime::managed_http(temp.path(), env!("CARGO_BIN_EXE_mvp-runtime-http-child"));
    let discovered = restarted_runtime.list().expect("list");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].address, started.address);

    let drained = restarted_runtime
        .drain(&InstanceId::new("instance-a"))
        .expect("drain")
        .expect("drained instance");
    assert_eq!(drained.state, RuntimeState::Draining);
    assert_response_contains(&drained.address, "revision=rev-1");

    let stopped = restarted_runtime
        .stop(&InstanceId::new("instance-a"))
        .expect("stop")
        .expect("stopped instance");
    assert_eq!(stopped.state, RuntimeState::Stopped);
    assert!(stopped.pid.is_none());
    assert!(TcpStream::connect(&started.address).is_err());
}

#[test]
fn process_runtime_stop_clears_metadata_when_recorded_pid_is_already_gone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime =
        ProcessRuntime::managed_http(temp.path(), env!("CARGO_BIN_EXE_mvp-runtime-http-child"));
    let spec = ProcessInstanceSpec::new(
        InstanceId::new("instance-a"),
        ServiceName::new("web"),
        RevisionId::new("rev-1"),
    );

    let started = runtime.start(&spec).expect("start");
    let pid = started.pid.expect("started pid");
    Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status()
        .expect("kill child");
    wait_until_unreachable(&started.address);

    let stopped = runtime
        .stop(&InstanceId::new("instance-a"))
        .expect("stop")
        .expect("stopped instance");
    assert_eq!(stopped.state, RuntimeState::Stopped);
    assert!(stopped.pid.is_none());
}

fn assert_response_contains(address: &str, expected: &str) {
    let mut stream = TcpStream::connect(address).expect("connect service");
    stream
        .write_all(b"GET / HTTP/1.1\r\nhost: local\r\n\r\n")
        .expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    assert!(
        response.contains(expected),
        "response {response:?} did not contain {expected:?}"
    );
}

fn wait_until_unreachable(address: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(address).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{address} remained reachable");
}

use std::io::Write;
