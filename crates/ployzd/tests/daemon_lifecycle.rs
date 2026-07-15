use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use std::process::{Child, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use ployz_nats::connect::NatsClientAuth;
#[cfg(unix)]
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
#[cfg(unix)]
use ployz_test_support::ids::machine_id;
#[cfg(unix)]
use wait_timeout::ChildExt;

const PLOYZ_MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
const PLOYZ_NATS_NKEY_SEED_FILE_ENV: &str = "PLOYZ_NATS_NKEY_SEED_FILE";
const TEST_SEED: &str = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";
static TEMP_SEED_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_seed_file(name: &str) -> String {
    let index = TEMP_SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ployzd-role-seed-{}-{index}", std::process::id()));
    fs::create_dir_all(&dir).expect("seed dir can be created");
    let path = dir.join(name);
    fs::write(&path, format!("{TEST_SEED}\n")).expect("seed file can be written");
    path.to_str().expect("temp path is utf-8").to_owned()
}

#[test]
fn machine_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-machine.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .env("DO_NOT_TRACK", "1")
        .args(["machine", "--id", "machine_7"])
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn gateway_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-gateway.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .env("DO_NOT_TRACK", "1")
        .args(["gateway"])
        .env(PLOYZ_MACHINE_ID_ENV, "machine_7")
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dns_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-dns.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .env("DO_NOT_TRACK", "1")
        .args(["dns"])
        .env(PLOYZ_MACHINE_ID_ENV, "machine_7")
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn machine_role_sigterm_exits_cleanly_and_restarts_after_nats_loss() {
    let machine_id = machine_id("machine_sigterm");
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(std::slice::from_ref(&machine_id))
            .await;
    let controller = nats.controller.clone();
    let connect = nats
        .server
        .machine_config(&machine_id)
        .expect("machine credentials exist");
    let NatsClientAuth::NkeySeed(seed) = connect.auth;
    let dir = tempfile::tempdir().expect("machine role temp dir");
    let seed_file = dir.path().join("machine.seed");
    fs::write(&seed_file, format!("{}\n", seed.secret())).expect("machine seed writes");
    let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);

    let mut first = TestChild::spawn_machine(
        &machine_id,
        connect.url.as_str(),
        nats.server.ca_path(),
        &seed_file,
    );
    wait_for_machine_service_interest(&mut first, &controller, &subject).await;
    let status = first.terminate();
    assert!(status.success(), "ployzd machine exited with {status}");

    let mut second = TestChild::spawn_machine(
        &machine_id,
        connect.url.as_str(),
        nats.server.ca_path(),
        &seed_file,
    );
    wait_for_machine_service_interest(&mut second, &controller, &subject).await;
    drop(nats);
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.connection_state() == async_nats::connection::State::Connected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("controller observes NATS shutdown");
    let status = second.terminate();
    assert!(
        status.success() || status.code() == Some(3),
        "ployzd machine must exit cleanly for SIGTERM or request restart after NATS loss, got {status}"
    );
}

#[cfg(unix)]
struct TestChild(Child);

#[cfg(unix)]
impl TestChild {
    fn spawn_machine(
        machine_id: &ployz_core::ids::MachineId,
        nats_url: &str,
        ca_file: &std::path::Path,
        seed_file: &std::path::Path,
    ) -> Self {
        Self(
            Command::new(env!("CARGO_BIN_EXE_ployzd"))
                .env("DO_NOT_TRACK", "1")
                .args(["machine", "--id", machine_id.as_str()])
                .env(PLOYZ_NATS_URL_ENV, nats_url)
                .env(PLOYZ_NATS_CA_FILE_ENV, ca_file)
                .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, seed_file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("ployzd machine process starts"),
        )
    }

    fn terminate(&mut self) -> std::process::ExitStatus {
        let signal = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .expect("send SIGTERM with native kill");
        assert!(signal.success(), "native kill sends SIGTERM");
        let status = self
            .0
            .wait_timeout(Duration::from_secs(6))
            .expect("wait for ployzd machine exit");
        let Some(status) = status else {
            panic!("ployzd machine did not exit within six seconds of SIGTERM");
        };
        status
    }
}

#[cfg(unix)]
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
async fn wait_for_machine_service_interest(
    child: &mut TestChild,
    client: &async_nats::Client,
    subject: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().expect("read ployzd machine status") {
            panic!("ployzd machine exited before service readiness with {status}");
        }
        let request = async_nats::Request::new()
            .payload(vec![b'{'].into())
            .timeout(Some(Duration::from_millis(100)));
        match client.send_request(subject.to_owned(), request).await {
            Ok(_) => return,
            Err(error)
                if error.kind() == async_nats::RequestErrorKind::NoResponders
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("machine service did not become ready: {error}"),
        }
    }
}

#[test]
fn machine_role_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .env("DO_NOT_TRACK", "1")
        .args(["machine", "--id", "machine_7"])
        .env_remove(PLOYZ_NATS_URL_ENV)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "PLOYZ_NATS_URL is required for ployzd machine\n"
    );
}
