use std::fs;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::build::{
    BuildAdapter, BuildContextPath, BuildExecutorAssignment, BuildExecutorReadiness,
    BuildExecutorReadinessAnswer, BuildExecutorReadinessRequest, BuildExecutorStartDomainError,
    BuildExecutorStartRequest, BuildExecutorStartResponse, GitSource,
};
use ployz_core::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser,
};
use ployz_nats::connect::connect_authenticated;
use ployz_nats::subjects::{BuildExecutorServiceEndpoint, build_executor_service};
use ployz_test_support::nats::{SecuredTestNats, TEST_NATS_CONNECT_TIMEOUT};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn watch_service_recovers_after_real_secured_nats_disconnect_and_stops_bounded() {
    let mut fixture = WatchFixture::start().await;
    let controller =
        connect_authenticated(&fixture.nats.controller_config(), TEST_NATS_CONNECT_TIMEOUT)
            .await
            .expect("controller connects");
    let mut process = fixture.spawn_watch();

    request_readiness(&controller, &fixture.readiness_subject, &mut process).await;
    request_endpoint(&controller, &fixture.start_subject, &mut process).await;
    request_endpoint(&controller, &fixture.cancel_subject, &mut process).await;
    let queued_request = start_request(&fixture.pool_id, &fixture.executor_id);
    let queued = request_start_across_outage(&controller, &fixture.start_subject, &queued_request);
    let restart = fixture.nats.restart_after(Duration::from_millis(500));
    let (restart, queued) = tokio::join!(restart, queued);
    restart.expect("real NATS restarts");
    let (queued, observed_outage) = queued;
    assert!(
        observed_outage,
        "start attempt overlaps the secured NATS outage"
    );
    request_readiness(&controller, &fixture.readiness_subject, &mut process).await;
    let reopened = if matches!(
        queued,
        BuildExecutorStartResponse::DomainError {
            error: BuildExecutorStartDomainError::RuntimeStopped
        }
    ) {
        request_start_when_admitted(
            &controller,
            &fixture.start_subject,
            &queued_request,
            &mut process,
        )
        .await
    } else {
        queued
    };
    assert!(!matches!(
        reopened,
        BuildExecutorStartResponse::DomainError {
            error: BuildExecutorStartDomainError::RuntimeStopped
        }
    ));

    signal(process.id(), "TERM");
    let output = process.wait_bounded(PROCESS_TIMEOUT);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).matches("Build Executor ready").count() >= 2
            || stderr(&output).matches("Build Executor degraded").count() >= 2,
        "startup and reconnect both report freshly probed health: {}",
        stderr(&output)
    );
}

#[tokio::test]
async fn watch_exits_when_real_secured_nats_revokes_its_credential() {
    let fixture = WatchFixture::start().await;
    let controller =
        connect_authenticated(&fixture.nats.controller_config(), TEST_NATS_CONNECT_TIMEOUT)
            .await
            .expect("controller connects");
    let mut process = fixture.spawn_watch();
    request_readiness(&controller, &fixture.readiness_subject, &mut process).await;
    request_endpoint(&controller, &fixture.start_subject, &mut process).await;
    request_endpoint(&controller, &fixture.cancel_subject, &mut process).await;

    fixture
        .nats
        .replace_external_credentials(&[])
        .expect("credential revocation renders");
    signal(fixture.nats.server_pid(), "HUP");

    tokio::time::sleep(Duration::from_millis(200)).await;
    connect_authenticated(&fixture.nats.user_config(), TEST_NATS_CONNECT_TIMEOUT)
        .await
        .expect("founder Operator remains authorized after external credential replacement");

    let output = process.wait_bounded(PROCESS_TIMEOUT);
    assert!(!output.status.success(), "revocation must be terminal");
    assert!(
        stderr(&output).contains("credential") || stderr(&output).contains("authorization"),
        "revocation remains a typed credential failure: {}",
        stderr(&output)
    );
}

struct WatchFixture {
    nats: SecuredTestNats,
    _directory: tempfile::TempDir,
    config_home: std::path::PathBuf,
    state_home: std::path::PathBuf,
    pool_id: BuildPoolId,
    executor_id: BuildExecutorId,
    readiness_subject: String,
    start_subject: String,
    cancel_subject: String,
}

impl WatchFixture {
    async fn start() -> Self {
        let pool_id = BuildPoolId::try_new("pool-watch").expect("pool id");
        let executor_id = BuildExecutorId::try_new("executor-watch").expect("executor id");
        let minted = MintedNatsUser::generate().expect("executor key mints");
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs()
            + 300;
        let credential = CredentialGrant {
            public_key: minted.public.clone(),
            name: CredentialName::try_new("Watch integration executor").expect("credential name"),
            role: CredentialRole::BuildExecutor {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
                expires_at: BuildExecutorCredentialExpiresAt::try_new(expires_at)
                    .expect("future expiry"),
            },
        };
        let nats = SecuredTestNats::start_with_machines_and_credentials(&[], &[credential])
            .await
            .expect("secured NATS starts");
        let directory = tempfile::tempdir().expect("fixture directory");
        let config_home = directory.path().join("config");
        let state_home = directory.path().join("state");
        let identity = config_home
            .join("ployz/build-executors")
            .join(pool_id.as_str())
            .join(executor_id.as_str());
        let materials = identity.join("materials/material-test");
        fs::create_dir_all(&materials).expect("context directories create");
        fs::write(identity.join("nkey.seed"), minted.seed.secret()).expect("seed writes");
        fs::copy(nats.ca_path(), materials.join("ca.pem")).expect("CA writes");
        fs::write(
            identity.join("context.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "organization_id": "org-watch",
                "pool_id": pool_id.as_str(),
                "executor_id": executor_id.as_str(),
                "runtime_nats_url": nats.client_url().as_str(),
                "nats_ca_file": "materials/material-test/ca.pem",
                "credential_expires_at": expires_at.to_string(),
            }))
            .expect("context encodes"),
        )
        .expect("context writes");
        let readiness_subject = build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::ReadinessGet,
        );
        let start_subject = build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        );
        let cancel_subject = build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildCancel,
        );
        Self {
            nats,
            _directory: directory,
            config_home,
            state_home,
            pool_id,
            executor_id,
            readiness_subject,
            start_subject,
            cancel_subject,
        }
    }

    fn spawn_watch(&self) -> GuardedChild {
        let child = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "build",
                "watch",
                "--pool-id",
                self.pool_id.as_str(),
                "--executor-id",
                self.executor_id.as_str(),
            ])
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("DO_NOT_TRACK", "1")
            .env_remove("HOME")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("watch process starts");
        GuardedChild(Some(child))
    }
}

async fn request_endpoint(client: &async_nats::Client, subject: &str, process: &mut GuardedChild) {
    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Ok(Ok(_)) = tokio::time::timeout(
            Duration::from_millis(500),
            client.request(subject.to_owned(), b"{}".as_slice().into()),
        )
        .await
        {
            return;
        }
        assert_process_running(process);
        assert!(
            tokio::time::Instant::now() < deadline,
            "executor endpoint {subject} did not answer"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn request_start(
    client: &async_nats::Client,
    subject: &str,
    request: &BuildExecutorStartRequest,
) -> Option<BuildExecutorStartResponse> {
    let payload = serde_json::to_vec(request).expect("start request encodes");
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        client.request(subject.to_owned(), payload.into()),
    )
    .await
    .ok()?
    .ok()?;
    serde_json::from_slice(&response.payload).ok()
}

async fn request_start_across_outage(
    client: &async_nats::Client,
    subject: &str,
    request: &BuildExecutorStartRequest,
) -> (BuildExecutorStartResponse, bool) {
    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    let mut observed_outage = false;
    loop {
        if let Some(response) = request_start(client, subject, request).await {
            return (response, observed_outage);
        }
        observed_outage = true;
        assert!(
            tokio::time::Instant::now() < deadline,
            "queued start answers"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn request_start_when_admitted(
    client: &async_nats::Client,
    subject: &str,
    request: &BuildExecutorStartRequest,
    process: &mut GuardedChild,
) -> BuildExecutorStartResponse {
    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    loop {
        let Some(response) = request_start(client, subject, request).await else {
            assert_process_running(process);
            assert!(tokio::time::Instant::now() < deadline, "admission reopens");
            continue;
        };
        if !matches!(
            response,
            BuildExecutorStartResponse::DomainError {
                error: BuildExecutorStartDomainError::RuntimeStopped
            }
        ) {
            return response;
        }
        assert_process_running(process);
        assert!(tokio::time::Instant::now() < deadline, "admission reopens");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn start_request(
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> BuildExecutorStartRequest {
    BuildExecutorStartRequest {
        operation_id: OperationId::try_new("op-watch-reconnect").expect("operation id"),
        assignment: BuildExecutorAssignment::External {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
            image_seed: MachineId::try_new("missing-image-seed").expect("machine id"),
        },
        source: GitSource::try_new(
            "https://git.example/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "builder",
            "secret",
            None::<String>,
        )
        .expect("Git source"),
        adapter: BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").expect("Dockerfile path"),
            target: None,
        },
        platform: OciPlatform::try_new(std::env::consts::OS, std::env::consts::ARCH)
            .expect("native platform"),
        timeout_millis: 1_000,
    }
}

struct GuardedChild(Option<Child>);

impl GuardedChild {
    fn id(&self) -> u32 {
        self.0.as_ref().expect("child present").id()
    }

    fn wait_bounded(&mut self, timeout: Duration) -> Output {
        use wait_timeout::ChildExt as _;
        let child = self.0.as_mut().expect("child present");
        let status = child
            .wait_timeout(timeout)
            .expect("process wait succeeds")
            .unwrap_or_else(|| {
                child.kill().expect("timed out process kills");
                panic!("watch process did not exit within {timeout:?}")
            });
        assert_eq!(child.try_wait().expect("status reads"), Some(status));
        self.0
            .take()
            .expect("child present")
            .wait_with_output()
            .expect("process output reads")
    }
}

impl Drop for GuardedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn request_readiness(client: &async_nats::Client, subject: &str, process: &mut GuardedChild) {
    let payload = serde_json::to_vec(&BuildExecutorReadinessRequest {}).expect("request encodes");
    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Ok(Ok(response)) = tokio::time::timeout(
            Duration::from_millis(500),
            client.request(subject.to_owned(), payload.clone().into()),
        )
        .await
        {
            match serde_json::from_slice::<BuildExecutorReadinessAnswer<BuildExecutorReadiness>>(
                &response.payload,
            ) {
                Ok(_) => return,
                Err(error) => eprintln!(
                    "readiness response did not decode: {error}; payload={}",
                    String::from_utf8_lossy(&response.payload)
                ),
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "readiness service did not answer"
        );
        assert_process_running(process);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn assert_process_running(process: &mut GuardedChild) {
    if process
        .0
        .as_mut()
        .expect("child present")
        .try_wait()
        .expect("child status reads")
        .is_some()
    {
        let output = process.wait_bounded(Duration::ZERO);
        panic!("watch exited before service readiness: {}", stderr(&output));
    }
}

fn signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
        .expect("signal command runs");
    assert!(status.success(), "signal {name} reaches process {pid}");
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
