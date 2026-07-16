//! Hosted-build journeys over the product's authenticated operator and machine RPCs.

use super::*;
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildContextPath, BuildPlatforms, GitSource,
};
use ployz_core::deploy::{ImageSource, PlatformImage, PushedImageReceipt};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::{BuildCleanupEvidence, BuildOperationState, CancellationReason};
use ployz_e2e::dind::write_file_in_container;
use ployz_sdk_types::{BuildCancelRequest, BuildSubmitRequest};
use support::dind::assert::assert_events_in_order;

const BUILD_BUDGET: Duration = Duration::from_secs(15 * 60);
const GIT_PORT: u16 = 9443;
const GIT_USERNAME: &str = "builder";
const GIT_SECRET: &str = "build-secret-370";
const DOCKERFILE_LOG_MARKER: &str = "ployz-dockerfile-build-marker";

struct GitFixture {
    url: String,
    commit: String,
}

pub(super) async fn assert_authenticated_build_journeys(core: &CoreContext) {
    with_evidence(&core.cluster, async {
        let git = start_authenticated_git_fixture(core).await;
        let platform = native_platform();

        let dockerfile = submit_build(
            core,
            &git,
            "op_dind_build_dockerfile",
            "dockerfile",
            BuildAdapter::Dockerfile {
                dockerfile: BuildContextPath::try_new("Dockerfile").expect("Dockerfile path"),
                target: None,
            },
            &platform,
        )
        .await;
        assert_success_evidence(
            core,
            &operation_id("op_dind_build_dockerfile"),
            &git.commit,
            DOCKERFILE_LOG_MARKER,
        )
        .await;
        deploy_receipt(core, "dockerfile", dockerfile).await;

        let railpack = submit_build(
            core,
            &git,
            "op_dind_build_railpack",
            "railpack",
            BuildAdapter::Railpack {
                cache_scope: BuildCacheScope::try_new("dind-build-railpack").expect("cache scope"),
            },
            &platform,
        )
        .await;
        assert_success_evidence(
            core,
            &operation_id("op_dind_build_railpack"),
            &git.commit,
            "",
        )
        .await;
        deploy_receipt(core, "railpack", railpack).await;

        assert_build_cancellation(core, &git, &platform).await;
        assert_secret_and_ephemeral_state_are_absent(core).await;
    })
    .await;
}

async fn submit_build(
    core: &CoreContext,
    git: &GitFixture,
    operation: &str,
    subdir: &str,
    adapter: BuildAdapter,
    platform: &OciPlatform,
) -> PushedImageReceipt {
    let operation_id = operation_id(operation);
    let accepted = core
        .api
        .build_submit(&BuildSubmitRequest {
            operation_id: operation_id.clone(),
            source: GitSource::try_new(
                &git.url,
                &git.commit,
                GIT_USERNAME,
                GIT_SECRET,
                Some(subdir),
            )
            .expect("authenticated exact-commit source"),
            adapter,
            platforms: BuildPlatforms::try_new([platform.clone()]).expect("native platform"),
        })
        .await
        .unwrap_or_else(|error| panic!("build {operation} submits: {error}"));
    assert_eq!(accepted.operation_id, operation_id);
    let status = wait_for_terminal_status(&core.api, &operation_id, BUILD_BUDGET).await;
    let encoded_status = serde_json::to_string(&status).expect("build status serializes");
    assert!(
        !encoded_status.contains(GIT_SECRET),
        "Git secret entered durable status"
    );
    let OperationStatus::Build {
        source,
        state: BuildOperationState::Completed { receipt },
        ..
    } = status
    else {
        panic!("build {operation} did not complete: {status:?}");
    };
    assert_eq!(source.commit.as_str(), git.commit);
    assert!(source.credential_supplied);
    let platform_images = receipt.platforms().collect::<Vec<_>>();
    let [(actual_platform, image)] = platform_images.as_slice() else {
        panic!("build {operation} did not return exactly one native image");
    };
    assert_eq!(*actual_platform, platform);
    assert_eq!(image.seed, machine_id("core_1"));
    receipt
}

async fn assert_success_evidence(
    core: &CoreContext,
    operation_id: &ployz_core::ids::OperationId,
    expected_commit: &str,
    log_marker: &str,
) {
    let events = terminal_operation_events(core, operation_id).await;
    assert_events_in_order(
        "machine build",
        &events,
        vec![
            (
                "submitted",
                Box::new(|event| matches!(event, OperationEvent::BuildSubmitted { .. })),
            ),
            (
                "placed",
                Box::new(|event| matches!(event, OperationEvent::BuildPlatformPlaced { .. })),
            ),
            (
                "commit verified",
                Box::new(|event| matches!(event, OperationEvent::BuildCommitVerified { .. })),
            ),
            (
                "toolchain verified",
                Box::new(|event| {
                    matches!(event, OperationEvent::BuildPlatformToolchainVerified { .. })
                }),
            ),
            (
                "platform completed",
                Box::new(|event| matches!(event, OperationEvent::BuildPlatformCompleted { .. })),
            ),
            (
                "completed",
                Box::new(|event| matches!(event, OperationEvent::BuildCompleted { .. })),
            ),
        ],
    );
    assert!(events.iter().any(|event| matches!(
        event,
        OperationEvent::BuildCommitVerified { commit, .. }
            if commit.commit.as_str() == expected_commit
    )));
    if !log_marker.is_empty() {
        let mut logs = String::new();
        for event in &events {
            if let OperationEvent::BuildPlatformLog { chunk, .. } = event {
                logs.push_str(chunk.as_str());
            }
        }
        assert!(
            logs.contains(log_marker),
            "build logs omitted {log_marker:?}"
        );
    }
    let encoded = serde_json::to_string(&events).expect("events serialize");
    assert!(
        !encoded.contains(GIT_SECRET),
        "Git secret entered durable evidence"
    );
}

async fn deploy_receipt(core: &CoreContext, label: &str, receipt: PushedImageReceipt) {
    let target = DeployRequest {
        namespace_id: namespace_id(&format!("build-{label}")),
        origin: None,
        volumes: BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id(&format!("svc-build-{label}")),
            image: ImageReference::try_new(format!("local/build-{label}:exact"))
                .expect("logical build image reference"),
            image_source: ImageSource::PushedToSeed(receipt),
            replicas: ReplicaCount::try_new(1).expect("one replica"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    };
    let accepted = core
        .api
        .deploy_submit(
            &reserved_deploy_request(core, &format!("idem-dind-build-{label}"), target).await,
        )
        .await
        .unwrap_or_else(|error| panic!("deploy built {label} image: {error}"));
    let status = wait_for_terminal_deploy_status(core, &accepted.operation_id, BUILD_BUDGET).await;
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { .. },
                ..
            }
        ),
        "built {label} image did not deploy: {status:?}"
    );
}

async fn assert_build_cancellation(core: &CoreContext, git: &GitFixture, platform: &OciPlatform) {
    let operation_id = operation_id("op_dind_build_cancel");
    core.api
        .build_submit(&BuildSubmitRequest {
            operation_id: operation_id.clone(),
            source: GitSource::try_new(
                &git.url,
                &git.commit,
                GIT_USERNAME,
                GIT_SECRET,
                Some("slow"),
            )
            .expect("slow source"),
            adapter: BuildAdapter::Dockerfile {
                dockerfile: BuildContextPath::try_new("Dockerfile").expect("Dockerfile path"),
                target: None,
            },
            platforms: BuildPlatforms::try_new([platform.clone()]).expect("native platform"),
        })
        .await
        .expect("slow build submits");
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let status = operation_status(core, &operation_id).await;
        let OperationStatus::Build { state, .. } = status else {
            panic!("wrong operation status for build: {status:?}");
        };
        match state {
            BuildOperationState::Building => break,
            BuildOperationState::Accepted | BuildOperationState::Placing => {}
            state @ (BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. }
            | BuildOperationState::Interrupted { .. }) => {
                panic!("slow build became terminal before cancellation: {state:?}");
            }
        }
        assert!(Instant::now() < deadline, "slow build never started");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    wait_for_active_builder(core).await;
    core.api
        .build_cancel(&BuildCancelRequest {
            operation_id: operation_id.clone(),
            reason: CancellationReason::try_new("DinD cancellation proof").expect("reason"),
        })
        .await
        .expect("build cancellation accepts");
    let status = wait_for_terminal_status(&core.api, &operation_id, BUILD_BUDGET).await;
    assert!(matches!(
        status,
        OperationStatus::Build {
            state: BuildOperationState::Cancelled {
                cleanup: BuildCleanupEvidence::Completed { .. },
                ..
            },
            ..
        }
    ));
    let events = terminal_operation_events(core, &operation_id).await;
    let encoded = serde_json::to_string(&events).expect("cancellation events serialize");
    assert!(
        !encoded.contains(GIT_SECRET),
        "Git secret entered cancellation evidence"
    );
}

async fn wait_for_active_builder(core: &CoreContext) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let outcome = core
            .exec_sh(
                core.cluster.core(),
                "test -n \"$(docker ps -q --filter label=com.getployz.build=true)\"",
            )
            .await;
        if outcome.success() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "machine builder never became active"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn assert_secret_and_ephemeral_state_are_absent(core: &CoreContext) {
    let outcome = core
        .exec_sh(
            core.cluster.core(),
            &format!(
                "set -eu; \
                 ! grep -R -F -- '{GIT_SECRET}' /var/lib/ployz /etc/ployz 2>/dev/null; \
                 test -z \"$(docker ps -aq --filter label=com.getployz.build=true)\"; \
                 test -z \"$(find /var/lib/ployz/builds/op_dind_build_cancel -mindepth 1 -print -quit 2>/dev/null)\""
            ),
        )
        .await;
    assert!(
        outcome.success(),
        "build secret or ephemeral state persisted: {outcome:?}"
    );
    let auth_log = core
        .exec_on(
            core.cluster.core(),
            &["cat", "/tmp/ployz-build-git/auth.log"],
        )
        .await;
    assert!(auth_log.success() && auth_log.stdout.contains("authorized"));
}

async fn start_authenticated_git_fixture(core: &CoreContext) -> GitFixture {
    let core_ip = core.core_ip();
    let server = r#"import base64
import http.server
import os
import ssl

expected = "Basic " + base64.b64encode(
    (os.environ["GIT_USERNAME"] + ":" + os.environ["GIT_PASSWORD"]).encode()
).decode()

class Authenticated(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.headers.get("Authorization") != expected:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="ployz-build"')
            self.end_headers()
            return
        with open("/tmp/ployz-build-git/auth.log", "a", encoding="utf-8") as log:
            log.write("authorized\n")
        super().do_GET()

    def log_message(self, format, *args):
        return

os.chdir("/tmp/ployz-build-git")
httpd = http.server.ThreadingHTTPServer(("0.0.0.0", 9443), Authenticated)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("server.crt", "server.key")
httpd.socket = context.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
"#;
    write_file_in_container(
        &core.docker,
        &core.cluster.core().container_id,
        "/tmp/ployz-build-git/server.py",
        server,
        "0700",
    )
    .await
    .expect("write authenticated Git server");
    let setup = format!(
        r#"set -eu
rm -rf /tmp/ployz-build-git/work /tmp/ployz-build-git/repo.git
mkdir -p /tmp/ployz-build-git/work/dockerfile /tmp/ployz-build-git/work/railpack /tmp/ployz-build-git/work/slow
printf '%s\n' 'FROM alpine:3.20' 'RUN echo {DOCKERFILE_LOG_MARKER}' 'COPY marker /marker' 'CMD ["sh", "-c", "while true; do sleep 600; done"]' > /tmp/ployz-build-git/work/dockerfile/Dockerfile
printf '%s\n' 'exact dockerfile commit' > /tmp/ployz-build-git/work/dockerfile/marker
printf '%s\n' '{{"scripts":{{"start":"node server.js"}},"engines":{{"node":"22"}}}}' > /tmp/ployz-build-git/work/railpack/package.json
printf '%s\n' 'require("http").createServer((_, res) => res.end("railpack exact commit\\n")).listen(process.env.PORT || 3000);' > /tmp/ployz-build-git/work/railpack/server.js
printf '%s\n' 'FROM alpine:3.20' 'RUN echo slow-build-start && sleep 600' 'CMD ["true"]' > /tmp/ployz-build-git/work/slow/Dockerfile
git -C /tmp/ployz-build-git/work init -q -b main
git -C /tmp/ployz-build-git/work config user.name 'Ployz DinD'
git -C /tmp/ployz-build-git/work config user.email 'dind@example.invalid'
git -C /tmp/ployz-build-git/work add .
GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z' git -C /tmp/ployz-build-git/work commit -qm fixture
git clone -q --bare /tmp/ployz-build-git/work /tmp/ployz-build-git/repo.git
git -C /tmp/ployz-build-git/repo.git update-server-info
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj '/CN={core_ip}' -addext 'subjectAltName=IP:{core_ip}' -keyout /tmp/ployz-build-git/server.key -out /tmp/ployz-build-git/server.crt >/dev/null 2>&1
install -m 0644 /tmp/ployz-build-git/server.crt /usr/local/share/ca-certificates/ployz-build-git.crt
update-ca-certificates >/dev/null
systemctl stop ployz-test-build-git.service 2>/dev/null || true
systemd-run --unit=ployz-test-build-git --setenv=GIT_USERNAME={GIT_USERNAME} --setenv=GIT_PASSWORD={GIT_SECRET} /usr/bin/python3 /tmp/ployz-build-git/server.py >/dev/null
for _ in $(seq 1 50); do curl -fsS -u {GIT_USERNAME}:{GIT_SECRET} https://{core_ip}:{GIT_PORT}/repo.git/HEAD >/dev/null && exit 0; sleep 0.1; done
exit 1
"#
    );
    let outcome = core.exec_sh(core.cluster.core(), &setup).await;
    assert!(outcome.success(), "authenticated Git fixture: {outcome:?}");
    let commit = core
        .exec_on(
            core.cluster.core(),
            &[
                "git",
                "-C",
                "/tmp/ployz-build-git/work",
                "rev-parse",
                "HEAD",
            ],
        )
        .await;
    assert!(commit.success(), "read fixture commit: {commit:?}");
    GitFixture {
        url: format!("https://{core_ip}:{GIT_PORT}/repo.git"),
        commit: commit.stdout.trim().to_owned(),
    }
}

fn native_platform() -> OciPlatform {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        architecture => panic!("unsupported DinD build architecture {architecture}"),
    };
    OciPlatform::try_new("linux", architecture).expect("native OCI platform")
}

#[test]
fn mixed_architecture_receipt_identity_is_stable_across_seed_relocation() {
    let amd64 = OciPlatform::try_new("linux", "amd64").expect("amd64");
    let arm64 = OciPlatform::try_new("linux", "arm64").expect("arm64");
    let image = |seed: &str, manifest: char, config: char| PlatformImage {
        seed: machine_id(seed),
        manifest_digest: digest(manifest),
        image_id: digest(config),
    };
    let original = PushedImageReceipt::try_new([
        (amd64.clone(), image("amd64-seed", 'a', 'b')),
        (arm64.clone(), image("arm64-seed", 'c', 'd')),
    ])
    .expect("mixed receipt");
    let relocated = PushedImageReceipt::try_new([
        (amd64, image("new-amd64-seed", 'a', 'b')),
        (arm64, image("new-arm64-seed", 'c', 'd')),
    ])
    .expect("relocated receipt");

    assert_eq!(original.index_digest(), relocated.index_digest());
}

fn digest(value: char) -> OciDigest {
    OciDigest::try_new(format!("sha256:{}", value.to_string().repeat(64))).expect("digest")
}
