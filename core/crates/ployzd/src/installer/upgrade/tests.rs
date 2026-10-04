//! Upgrade attempt and worker contracts against fake systemd.

use std::{
    collections::BTreeSet,
    convert::Infallible,
    task::{Context, Poll},
};

use ployz_core::{
    ContractDescription, MachineId, MachineVersion, OpaquePayload, PROTOCOL_MAJOR, RpcResponse,
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

use super::super::test_support::{fixture, run_contract_child_with_environment, write_script};
use super::super::tests::{create_installation_fixture, write_existing_daemon};
use super::*;

const CONTRACT_CASE: &str = "PLOYZ_UPGRADE_CONTRACT_CASE";
const CONTRACT_ROOT: &str = "PLOYZ_UPGRADE_CONTRACT_ROOT";
const WORKER_CASE: &str = "PLOYZ_UPGRADE_WORKER_CASE";

/// systemd for one fake daemon. `restart` starts whatever `bin/ployzd` is installed as a new
/// main process, whose `/proc` entry links that executable as the kernel's would. A version
/// with a `broken-<version>` marker never becomes active; one with `crashing-<version>` runs
/// under a new main PID whenever asked. A volume plugin unit is active only while an
/// `active-<unit>` marker exists.
const SYSTEMCTL: &str = r#"root="$PLOYZ_INSTALLER_CONTRACT_ROOT"
echo "$*" >> "$root/systemctl.log"
start() {
  pid=1000
  if [ -f "$root/main-pid" ]; then read -r pid < "$root/main-pid"; fi
  pid=$((pid + 1))
  /bin/mkdir -p "$root/proc/$pid"
  /bin/ln "$root/bin/ployzd" "$root/proc/$pid/exe"
  "$root/bin/ployzd" version > "$root/running-version"
  echo "$pid" > "$root/main-pid"
}
version=none
if [ -f "$root/running-version" ]; then read -r version < "$root/running-version"; fi
case "$1" in
  restart) start ;;
  is-active)
    case "$3" in
      ployz-volume-plugin.*) [ -e "$root/active-$3" ] || exit 3 ;;
      *) if [ -e "$root/broken-$version" ]; then exit 3; fi ;;
    esac ;;
  show)
if [ -e "$root/crashing-$version" ]; then start; fi
read -r pid < "$root/main-pid"; echo "$pid" ;;
esac"#;

#[tokio::test]
async fn worker_rejects_nonstandard_paths_before_reading_local_state() {
    let root = tempfile::Builder::new()
        .prefix("ployzd-upgrade-worker-paths-")
        .tempdir()
        .unwrap();
    let data_dir = root.path().join("data");
    let run_dir = root.path().join("run");

    let error = run_worker(MachineUpgradeAttemptId::random(), &data_dir, &run_dir)
        .await
        .unwrap_err();

    assert!(matches!(error, Error::NonstandardPaths(_)));
    assert!(!data_dir.exists());
    assert!(!run_dir.exists());
}

#[test]
fn upgrade_attempt_contract() {
    if let Ok(case) = env::var(CONTRACT_CASE) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(run_contract_case(&case));
        let root = PathBuf::from(env::var_os(CONTRACT_ROOT).unwrap());
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }

    for case in [
        "retry-active",
        "interrupted",
        "inspection-unknown",
        "launch-failed",
        "cancelled-launch",
        "cancelled-launch-failed",
    ] {
        let root = tempfile::Builder::new()
            .prefix(&format!("ployzd-upgrade-{case}-"))
            .tempdir()
            .unwrap();
        let commands = root.path().join("commands");
        fs::create_dir(&commands).unwrap();
        write_script(
            &commands.join("systemd-run"),
            if case.starts_with("cancelled-launch") {
                "echo started > \"$PLOYZ_UPGRADE_CONTRACT_ROOT/launch-started\"\nwhile [ ! -f \"$PLOYZ_UPGRADE_CONTRACT_ROOT/launch-release\" ]; do /bin/sleep 0.01; done\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_UPGRADE_COMMAND_LOG\"\nif [ \"$PLOYZ_UPGRADE_CONTRACT_CASE\" = cancelled-launch-failed ]; then echo worker launch refused >&2; exit 1; fi"
            } else if case == "launch-failed" {
                "echo worker launch refused >&2; exit 1"
            } else {
                "printf '%s\\n' \"$*\" >> \"$PLOYZ_UPGRADE_COMMAND_LOG\""
            },
        );
        write_script(
            &commands.join("systemctl"),
            match case {
                "interrupted" => "printf 'LoadState=loaded\\nActiveState=inactive\\n'",
                "inspection-unknown" => "echo manager unavailable >&2; exit 1",
                _ => "printf 'LoadState=loaded\\nActiveState=active\\n'",
            },
        );
        let output = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "installer::upgrade::tests::upgrade_attempt_contract",
                "--nocapture",
            ])
            .env(CONTRACT_CASE, case)
            .env(CONTRACT_ROOT, root.path())
            .env(
                "PLOYZ_UPGRADE_COMMAND_LOG",
                root.path().join("commands.log"),
            )
            .env("PATH", &commands)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.path().join("child-completed")).unwrap(),
            case
        );
    }
}

async fn run_contract_case(case: &str) {
    let root = PathBuf::from(env::var_os(CONTRACT_ROOT).unwrap());
    let data = root.join("data");
    let run = root.join("run");
    let admission = mutation::MutationGate::new(&run, &data);
    let attempt_id = MachineUpgradeAttemptId::parse("a".repeat(32)).unwrap();
    let request = RequestMachineUpgradeRequest {
        attempt_id,
        release: MachineRelease::parse("1.2.3").unwrap(),
    };
    let guard = admission.try_installation().unwrap();
    if case.starts_with("cancelled-launch") {
        let launch = tokio::spawn({
            let data = data.clone();
            let run = run.clone();
            let request = request.clone();
            async move { super::request(request, data, run, guard).await }
        });
        timeout(Duration::from_secs(5), async {
            while !root.join("launch-started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("launch did not reach the cancellation window");
        launch.abort();
        assert!(launch.await.unwrap_err().is_cancelled());
        assert!(admission.active().unwrap());
        assert!(matches!(
            admission.try_installation(),
            Err(mutation::Error::Busy)
        ));
        fs::write(root.join("launch-release"), "continue").unwrap();
        timeout(Duration::from_secs(5), async {
            while admission.try_installation().is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("detached launch did not finish");
        let observed = existing_request(&request, &data).unwrap().unwrap();
        if case == "cancelled-launch-failed" {
            assert!(matches!(
                observed.outcome,
                MachineUpgradeOutcome::Failed {
                    stage: MachineUpgradeStage::Launching,
                    ..
                }
            ));
            assert!(!admission.active().unwrap());
            assert!(admission.try_mutation().is_ok());
        } else {
            assert!(matches!(observed.outcome, MachineUpgradeOutcome::Accepted));
            assert!(admission.active().unwrap());
        }
        assert_eq!(
            fs::read_to_string(root.join("commands.log"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        return;
    }
    let accepted = super::request(request.clone(), data.clone(), run.clone(), guard)
        .await
        .unwrap();

    match case {
        "retry-active" => {
            assert!(matches!(accepted.outcome, MachineUpgradeOutcome::Accepted));
            assert_eq!(
                existing_request(&request, &data).unwrap(),
                Some(accepted.clone())
            );
            assert!(matches!(
                admission.try_mutation(),
                Err(mutation::Error::Busy)
            ));
            let conflict = RequestMachineUpgradeRequest {
                attempt_id,
                release: MachineRelease::parse("1.2.4").unwrap(),
            };
            assert!(matches!(
                existing_request(&conflict, &data),
                Err(Error::AttemptConflict(id)) if id == attempt_id
            ));

            let guard = admission.try_installation().unwrap();
            assert_eq!(
                super::request(request, data.clone(), run.clone(), guard)
                    .await
                    .unwrap(),
                accepted
            );
            let guard = admission.try_installation().unwrap();
            let other = RequestMachineUpgradeRequest {
                attempt_id: MachineUpgradeAttemptId::parse("b".repeat(32)).unwrap(),
                release: MachineRelease::parse("1.2.3").unwrap(),
            };
            assert!(matches!(
                super::request(other, data.clone(), run.clone(), guard).await,
                Err(Error::Busy)
            ));
            let log = fs::read_to_string(root.join("commands.log")).unwrap();
            assert_eq!(log.lines().count(), 1, "{log}");
            for required in [
                "--property=Type=exec",
                "--property=RuntimeMaxSec=15min",
                "--property=NoNewPrivileges=yes",
                "--property=ProtectSystem=full",
                "upgrade-worker",
                "--attempt",
                attempt_id.as_str(),
            ] {
                assert!(log.contains(required), "missing {required}: {log}");
            }
        }
        "interrupted" => {
            let observed = inspect(Some(attempt_id), &data, &run).await.unwrap();
            assert!(matches!(
                observed.outcome,
                MachineUpgradeOutcome::Interrupted {
                    stage: MachineUpgradeStage::Launching,
                }
            ));
            assert!(!admission.active().unwrap());
            assert!(admission.try_mutation().is_ok());
        }
        "inspection-unknown" => {
            assert!(matches!(
                inspect(Some(attempt_id), &data, &run).await,
                Err(Error::WorkerEvidence(ref message))
                    if message.contains("manager unavailable")
            ));
            assert!(admission.active().unwrap());
            assert!(matches!(
                admission.try_mutation(),
                Err(mutation::Error::Busy)
            ));
        }
        "launch-failed" => {
            assert!(matches!(
                accepted.outcome,
                MachineUpgradeOutcome::Failed {
                    stage: MachineUpgradeStage::Launching,
                    ref error,
                } if error == "launch Machine upgrade worker: worker launch refused"
            ));
            assert!(!admission.active().unwrap());
            assert_eq!(
                inspect(Some(attempt_id), &data, &run).await.unwrap(),
                accepted
            );
        }
        other => panic!("unknown upgrade contract case {other}"),
    }
}

/// The worker upgrades an installed daemon to a local release of 1.2.3; markers tell the fake
/// systemd which release misbehaves.
#[tokio::test]
async fn upgrade_worker_contract() {
    if let Ok(case) = env::var(WORKER_CASE) {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        run_worker_case(&root, &case).await;
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }

    for case in [
        "succeeded",
        "readiness-restored",
        "soak-restored",
        "restore-failed",
        "before-activation",
        "already-installed",
        "other-line",
    ] {
        let fixture = fixture(case);
        let root = fixture.path();
        let release = if case == "before-activation" {
            "corrupt"
        } else {
            "success"
        };
        create_installation_fixture(root, release);
        write_script(&root.join("commands/systemctl"), SYSTEMCTL);
        run_contract_child_with_environment(
            "installer::upgrade::tests::upgrade_worker_contract",
            root,
            WORKER_CASE.into(),
            case.into(),
            case,
            [],
        );
    }
}

async fn run_worker_case(root: &Path, case: &str) {
    let paths = InstallPaths::at(root);
    let mark = |marker: &str| fs::write(root.join(marker), "").unwrap();
    write_existing_daemon(
        &paths,
        if case == "other-line" {
            "0.9.9"
        } else {
            "1.2.2"
        },
    );
    match case {
        "readiness-restored" => {
            mark("broken-1.2.3");
            mark("active-ployz-volume-plugin.socket");
            mark("active-ployz-volume-plugin.service");
        }
        "other-line" => mark("broken-1.2.3"),
        "soak-restored" => mark("crashing-1.2.3"),
        "restore-failed" => {
            mark("broken-1.2.3");
            mark("broken-1.2.2");
        }
        "already-installed" => {
            // An earlier Upgrade retained 1.2.2; this one replaces nothing.
            fs::rename(paths.daemon(), paths.bin_dir.join("ployzd.previous")).unwrap();
            write_existing_daemon(&paths, "1.2.3");
            mark("broken-1.2.3");
        }
        _ => {}
    }
    let before = fs::read(paths.daemon()).unwrap();
    let attempt_id = MachineUpgradeAttemptId::parse("a".repeat(32)).unwrap();
    let target = MachineVersion::parse("1.2.3").unwrap();
    write(
        &paths.data_dir,
        &StoredAttempt {
            requested: MachineRelease::Exact(target.clone()),
            attempt: MachineUpgradeAttempt {
                attempt_id,
                target: target.clone(),
                outcome: MachineUpgradeOutcome::Accepted,
            },
        },
    )
    .unwrap();
    mutation::MutationGate::new(&paths.run_dir, &paths.data_dir)
        .mark_active(attempt_id.as_str())
        .unwrap();
    fs::create_dir_all(&paths.run_dir).unwrap();
    let api = UnixListener::bind(paths.run_dir.join("ployz.sock")).unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(FakeMachineApi(root.join("running-version")))
            .serve_with_incoming(UnixListenerStream::new(api)),
    );

    let result = work(
        &ReleaseSource::Local(root.join("release")),
        &paths,
        attempt_id,
    )
    .await;

    let outcome = read(&paths.data_dir).unwrap().attempt.outcome;
    match (&result, &outcome) {
        (Ok(()), MachineUpgradeOutcome::Succeeded { .. }) => {}
        (Err(Error::Upgrade(returned)), MachineUpgradeOutcome::Failed { error, .. }) => {
            assert_eq!(&returned.to_string(), error);
        }
        unexpected => panic!("{case}: worker returned and recorded {unexpected:?}"),
    }
    let installed = fs::read(paths.daemon()).unwrap();
    let upgraded = fs::read(root.join("payload/ployzd")).unwrap();
    let unready = "check daemon readiness: exited with exit status: 3";
    let failed = |error: &str| MachineUpgradeOutcome::Failed {
        stage: MachineUpgradeStage::Readiness,
        error: error.into(),
    };
    let started = [
        "restart ployz.socket ployz.service",
        "try-restart ployz-volume-plugin.service",
    ];
    let restored = [
        "restart ployz.socket ployz.service",
        "try-restart ployz-volume-plugin.service",
        "reset-failed ployz.socket ployz.service",
        "restart ployz.socket ployz.service",
    ];
    match case {
        "succeeded" => {
            assert_eq!(
                outcome,
                MachineUpgradeOutcome::Succeeded { version: target }
            );
            assert_eq!(installed, upgraded);
            assert_eq!(transitions(root), started);
        }
        "readiness-restored" => {
            assert_eq!(outcome, failed(&format!("{unready}; restored 1.2.2")));
            assert_eq!(installed, before);
            // The plugin units were running before the Upgrade, so they come back even if the
            // bad release stopped them.
            assert_eq!(
                transitions(root),
                [
                    "restart ployz.socket ployz.service",
                    "try-restart ployz-volume-plugin.service",
                    "reset-failed ployz.socket ployz.service ployz-volume-plugin.socket \
                     ployz-volume-plugin.service",
                    "restart ployz.socket ployz.service",
                    "restart ployz-volume-plugin.socket ployz-volume-plugin.service",
                ]
            );
        }
        "soak-restored" => {
            assert!(matches!(
                outcome,
                MachineUpgradeOutcome::Failed {
                    stage: MachineUpgradeStage::Readiness,
                    ref error,
                } if error.starts_with("soak daemon: ployz.service restarted (main PID ")
                    && error.ends_with("; restored 1.2.2")
            ));
            assert_eq!(installed, before);
            assert_eq!(transitions(root), restored);
        }
        "restore-failed" => {
            assert_eq!(
                outcome,
                failed(&format!("{unready}; restore failed: {unready}"))
            );
        }
        "before-activation" => {
            assert!(matches!(
                outcome,
                MachineUpgradeOutcome::Failed {
                    stage: MachineUpgradeStage::Verifying,
                    ref error,
                } if error.starts_with("artifact verification: ") && !error.contains("restore")
            ));
            assert_eq!(installed, before);
            assert!(transitions(root).is_empty());
        }
        "already-installed" => {
            assert_eq!(outcome, failed(unready));
            assert_eq!(installed, before);
            assert_eq!(transitions(root), started);
        }
        "other-line" => {
            assert_eq!(outcome, failed(unready));
            assert_eq!(installed, upgraded);
            assert_eq!(transitions(root), started);
        }
        other => panic!("unknown upgrade worker case {other}"),
    }
    assert!(
        !mutation::MutationGate::new(&paths.run_dir, &paths.data_dir)
            .active()
            .unwrap()
    );
}

/// The systemd commands that start, stop, or reset units, in order.
fn transitions(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("systemctl.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            matches!(
                line.split_whitespace().next(),
                Some("restart" | "try-restart" | "reset-failed")
            )
        })
        .map(str::to_owned)
        .collect()
}

/// A Machine API that answers every call as `DescribeContract` for the daemon the fake
/// systemd last started.
#[derive(Clone)]
struct FakeMachineApi(PathBuf);

impl tonic::server::NamedService for FakeMachineApi {
    const NAME: &'static str = "ployz.rpc.v1.MachineRpc";
}

impl tonic::codegen::Service<http::Request<tonic::body::Body>> for FakeMachineApi {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Infallible>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let running = fs::read_to_string(&self.0).unwrap_or_default();
        Box::pin(async move {
            Ok(
                tonic::server::Grpc::new(tonic::codec::ProstCodec::default())
                    .unary(Contract(running.trim().to_owned()), request)
                    .await,
            )
        })
    }
}

struct Contract(String);

impl tonic::server::UnaryService<OpaquePayload> for Contract {
    type Response = OpaquePayload;
    type Future = std::future::Ready<Result<tonic::Response<OpaquePayload>, tonic::Status>>;

    fn call(&mut self, _request: tonic::Request<OpaquePayload>) -> Self::Future {
        std::future::ready(Ok(tonic::Response::new(
            RpcResponse::from(ContractDescription {
                machine_id: MachineId::random(),
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: self.0.clone(),
                capabilities: BTreeSet::new(),
            })
            .encode()
            .unwrap(),
        )))
    }
}
