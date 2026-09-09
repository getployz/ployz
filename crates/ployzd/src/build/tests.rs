//! Owned transport tests. The Docker stand-in proves orchestration and
//! termination evidence; the informing test proves a real remote image.

use super::*;
use crate::{
    machine::{FoundingCluster, LocalMachineStore},
    machine_api::{MachineApi, MachineService},
};
use ployz_core::MachineRpcClient;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

struct Fixture {
    root: PathBuf,
    policy: HostPolicy,
    cluster: tokio::task::JoinHandle<()>,
    machine: ployz_core::Machine,
    address: String,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ployz-remote-build-test-{}", MachineId::random()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(
            &root,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let mut store = LocalMachineStore::open(root.join("machine")).unwrap();
        let machine = store
            .initialize(
                ployz_core::MachineName::parse("builder").unwrap(),
                FoundingCluster {
                    network: "10.210.0.0/16".parse().unwrap(),
                },
                None,
                vec![ployz_core::AdvertisedEndpoint(
                    "127.0.0.1:7569".parse().unwrap(),
                )],
                None,
                None,
            )
            .unwrap();
        let (restart, _) = tokio::sync::watch::channel(false);
        let (runtime, _) = crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let (replicated, cluster) = crate::corrosion::fake_cluster::store().await;
        replicated.publish_local_machine(&machine).await.unwrap();
        let mut service = MachineService::with_cluster(
            Arc::new(Mutex::new(store)),
            restart,
            Some((replicated, crate::corrosion::AdminClient::new("/no/admin"))),
        )
        .with_optional_containers(Some(runtime));
        let policy = HostPolicy {
            state_directory: root.clone(),
            docker: root.join("docker"),
            active_timeout: Duration::from_secs(30),
            configuration_file: root.join("build.yaml"),
        };
        service.build_policy = policy.clone();
        write_docker(&policy.docker, &root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let api = MachineApi::from_local(service);
        let server = tokio::spawn(
            Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)),
        );
        Self {
            root,
            policy,
            cluster,
            machine,
            address,
            server,
        }
    }
    async fn request(
        &self,
        output: Output,
    ) -> (mpsc::Sender<OpaquePayload>, tonic::Streaming<OpaquePayload>) {
        let mut client = MachineRpcClient::connect(self.address.clone())
            .await
            .unwrap();
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(
                remote::encode(&Input::Start(Definition {
                    targets: vec![ployz_build::Target {
                        name: "api".into(),
                        platform: None,
                    }],
                    output,
                    no_cache: false,
                    pull: false,
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let response = client
            .build(ReceiverStream::new(receiver))
            .await
            .unwrap()
            .into_inner();
        (sender, response)
    }
    fn capture(&self) -> ployz::compose::CapturedBuild {
        let project_root = self.root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        fs::write(
            project_root.join("Dockerfile"),
            "FROM scratch\nCOPY payload /payload\n",
        )
        .unwrap();
        fs::write(project_root.join("payload"), "captured-before-edit").unwrap();
        fs::write(project_root.join("ignored"), "excluded-private-value").unwrap();
        fs::write(project_root.join(".dockerignore"), "ignored\n").unwrap();
        let mut project = ployz::compose::parse_normalized("name: demo\nservices:\n  api:\n    image: example.test/api:built\n    environment:\n      PRIVATE_VALUE: secret-value\n    build:\n      context: .\n", &project_root).unwrap();
        let options = ployz::compose::BuildOptions::default();
        let plan = ployz::compose::plan_build(&project, &options).unwrap();
        ployz::compose::capture_build(&plan, &options, &mut project).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.cluster.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn event(response: &mut tonic::Streaming<OpaquePayload>) -> Event {
    let frame = tokio::time::timeout(Duration::from_secs(10), response.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    remote::decode(&frame).unwrap()
}
async fn terminal(response: &mut tonic::Streaming<OpaquePayload>) -> Outcome {
    loop {
        if let Event::Finished(outcome) = event(response).await {
            return outcome;
        }
    }
}

#[tokio::test]
async fn admitted_upload_refuses_competitors_and_disconnect_releases_unused_ownership() {
    let fixture = Fixture::new().await;
    let (first, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    assert!(!fixture.root.join("executed").exists());
    let (_second, mut rejected) = fixture.request(Output::Load).await;
    assert!(
        matches!(terminal(&mut rejected).await, Outcome::Failed { stage: Stage::Admission, message, .. } if message.contains("busy"))
    );
    first
        .send(
            remote::encode(&Input::Entry {
                path: b"source".to_vec(),
                kind: remote::Kind::Directory,
                mode: 0o755,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    drop(first);
    assert!(matches!(
        terminal(&mut response).await,
        Outcome::Failed {
            stage: Stage::Upload,
            ..
        }
    ));
    assert!(!fixture.root.join("executed").exists());
    let (_third, mut admitted) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut admitted).await, Event::Admitted { .. }));
}

#[tokio::test]
async fn captured_build_crosses_owned_rpc_and_returns_only_remote_image_evidence() {
    let fixture = Fixture::new().await;
    let capture = fixture.capture();
    fs::write(fixture.root.join("project/payload"), "later-edit").unwrap();
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let events = Mutex::new(Vec::new());
    let result = capture
        .execute_remote(
            &client,
            fixture.machine.id,
            tokio_util::sync::CancellationToken::new(),
            |event| events.lock().unwrap().push(event),
        )
        .await;
    let Outcome::Images { machine_id, images } = result else {
        panic!("{result:?}")
    };
    assert_eq!(machine_id, fixture.machine.id);
    let [image] = images.as_slice() else {
        panic!("expected one image: {images:?}")
    };
    assert_eq!(
        image.reference,
        format!("example.test/api@sha256:{}", "1".repeat(64))
    );
    assert_eq!(image.platform, "linux/amd64");
    assert_eq!(
        fs::read_to_string(fixture.root.join("received-payload")).unwrap(),
        "captured-before-edit"
    );
    assert!(
        !fs::read_to_string(fixture.root.join("source-list"))
            .unwrap()
            .contains("ignored")
    );
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Progress::Stage(Stage::Building)))
    );
    assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
}

#[tokio::test]
async fn oversized_diagnostics_still_return_a_definite_terminal_failure() {
    let fixture = Fixture::new().await;
    let capture = fixture.capture();
    fs::write(fixture.root.join("oversized-error"), "").unwrap();
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let result = capture
        .execute_remote(
            &client,
            fixture.machine.id,
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await;
    let Outcome::Failed {
        stage,
        message,
        work,
    } = result
    else {
        panic!("a known failure lost its terminal report: {result:?}")
    };
    assert_eq!(stage, Stage::Preparation);
    assert!(message.contains("response size limit"), "{message}");
    assert_eq!(
        work.0.get("api"),
        Some(&ployz_build::TargetEvidence::Unattempted)
    );
    assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
}

fn write_docker(path: &Path, root: &Path) {
    let script = format!(
        r#"#!/bin/sh
root='{}'
if [ -f "$root/oversized-error" ] && [ "$1 $2" = 'info --format' ]; then
  /usr/bin/head -c 262144 /dev/zero | /usr/bin/tr '\000' x >&2
  exit 1
fi
case "$1 $2" in
  'context show') echo default ;;
  'info --format') printf '%s\n' '{{"OSType":"linux","Architecture":"x86_64","DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]]}}' ;;
  'buildx rm')
    if [ -f "$root/fail-cleanup" ] && [ -f "$root/executed" ]; then exit 1; fi ;;
  'buildx version'|'buildx create'|'buildx inspect') exit 0 ;;
  'buildx ls') printf '%s\n' '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
  'buildx bake')
    : > "$root/executed"
    if [ -f "$root/registry-attempt" ]; then
      for arg in "$@"; do
        case "$arg" in
          api) : > "$root/published-api" ;;
          web)
            if [ -f "$root/cancel-publication" ]; then printf 'publishing web\n'; exec sleep 30; fi
            exit 1 ;;
          zzz) : > "$root/published-zzz" ;;
        esac
      done
      exit 0
    fi
    find source -type f > "$root/source-list"
    find source -name payload -exec cp '{{}}' "$root/received-payload" \;
    if [ -f "$root/slow" ]; then printf 'building\n'; exec sleep 30; fi
    previous=
    for arg in "$@"; do
      if [ "$previous" = --metadata-file ]; then printf '%s\n' '{{"api":{{"containerimage.digest":"sha256:{}","image.name":"example.test/api:built"}}}}' > "$arg"; fi
      previous=$arg
    done
    if [ -f "$root/fail-after-output" ]; then exit 1; fi ;;
  'image inspect') printf '%s\n' '{{"Os":"linux","Architecture":"amd64","Descriptor":{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{}"}}}}' ;;
  *) exit 1 ;;
esac
"#,
        root.display(),
        ployz_build::builder_name(),
        "1".repeat(64),
        "1".repeat(64)
    );
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn active_cancellation_confirms_cleanup_or_quarantines_uncertain_termination() {
    for uncertain in [false, true] {
        let fixture = Fixture::new().await;
        fs::write(fixture.root.join("slow"), "").unwrap();
        if uncertain {
            fs::write(fixture.root.join("fail-cleanup"), "").unwrap();
        }
        let capture = fixture.capture();
        let client = ployz::connect::connect(
            Path::new("/missing-test-config"),
            Some(&fixture.address.replace("http://", "tcp://")),
            None,
        )
        .await
        .unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let cancel = cancellation.clone();
        let result = capture
            .execute_remote(&client, fixture.machine.id, cancellation, |progress| {
                if matches!(progress, Progress::Output(_)) {
                    cancel.cancel();
                }
            })
            .await;
        if uncertain {
            assert!(matches!(result, Outcome::Unknown { .. }), "{result:?}");
            assert!(
                matches!(Admission::try_acquire_with(&fixture.policy), Err(error) if error.is_unknown())
            );
        } else {
            assert!(
                matches!(result, Outcome::Failed { stage: Stage::Building, message, .. } if message.contains("cancel"))
            );
            assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
        }
    }
}

#[tokio::test]
async fn upload_timeout_stops_before_execution_and_releases_admission() {
    let fixture = Fixture::new().await;
    let mut policy = fixture.policy.clone();
    policy.active_timeout = Duration::from_millis(100);
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(remote::encode(&Input::Start(Definition {
            targets: vec![ployz_build::Target {
                name: "api".into(),
                platform: None,
            }],
            output: Output::Load,
            no_cache: false,
            pull: false,
        }))
        .unwrap()))
        .await
        .unwrap();
    let mut responses = start(fixture.machine.id, ReceiverStream::new(receiver), policy);
    assert!(matches!(
        remote::decode::<Event>(&responses.next().await.unwrap().unwrap()).unwrap(),
        Event::Admitted { .. }
    ));
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Event::Finished(outcome) =
                remote::decode(&responses.next().await.unwrap().unwrap()).unwrap()
            {
                break outcome;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        terminal,
        Outcome::Failed {
            stage: Stage::Upload,
            ..
        }
    ));
    assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
    assert!(!fixture.root.join("executed").exists());
}

#[tokio::test]
async fn terminal_failures_preserve_completed_images_and_uncertain_targets() {
    for failure in ["fail-after-output", "missing-second", "fail-cleanup"] {
        let fixture = Fixture::new().await;
        let capture = fixture.capture();
        let capture = if failure == "fail-cleanup" {
            capture
        } else {
            drop(capture);
            let root = fixture.root.join("project");
            let mut project = ployz::compose::parse_normalized(
                "name: demo\nservices:\n  api:\n    image: example.test/api:built\n    build: .\n  web:\n    image: example.test/web:built\n    build: .\n", &root).unwrap();
            let options = ployz::compose::BuildOptions::default();
            let plan = ployz::compose::plan_build(&project, &options).unwrap();
            ployz::compose::capture_build(&plan, &options, &mut project).unwrap()
        };
        fs::write(fixture.root.join(failure), "").unwrap();
        let client = ployz::connect::connect(
            Path::new("/missing-test-config"),
            Some(&fixture.address.replace("http://", "tcp://")),
            None,
        )
        .await
        .unwrap();
        let result = capture
            .execute_remote(
                &client,
                fixture.machine.id,
                tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await;
        let work = match result {
            Outcome::Unknown { work, .. } if failure == "fail-cleanup" => work,
            Outcome::Failed { work, .. } if failure != "fail-cleanup" => work,
            other @ (Outcome::Images { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. }
            | Outcome::Failed { .. }
            | Outcome::Unknown { .. }) => panic!("unexpected terminal result: {other:?}"),
        };
        assert!(
            matches!(work.0.get("api"), Some(ployz_build::TargetEvidence::Image(image)) if image.reference.ends_with(&"1".repeat(64)))
        );
        if failure != "fail-cleanup" {
            assert_eq!(
                work.0.get("web"),
                Some(&ployz_build::TargetEvidence::Unknown)
            );
        }
    }
}

#[tokio::test]
async fn registry_publication_keeps_completed_and_unattempted_targets() {
    for cancelled in [false, true] {
        let fixture = Fixture::new().await;
        drop(fixture.capture());
        let root = fixture.root.join("project");
        let mut project = ployz::compose::parse_normalized(
            "name: demo\nservices:\n  api:\n    image: example.test/api:built\n    build: .\n  web:\n    image: example.test/web:built\n    build: .\n  zzz:\n    image: example.test/zzz:built\n    build: .\n", &root).unwrap();
        let options = ployz::compose::BuildOptions {
            output: Output::Registry,
            ..Default::default()
        };
        let plan = ployz::compose::plan_build(&project, &options).unwrap();
        let capture = ployz::compose::capture_build(&plan, &options, &mut project).unwrap();
        fs::write(fixture.root.join("registry-attempt"), "").unwrap();
        if cancelled {
            fs::write(fixture.root.join("cancel-publication"), "").unwrap();
        }
        let client = ployz::connect::connect(
            Path::new("/missing-test-config"),
            Some(&fixture.address.replace("http://", "tcp://")),
            None,
        )
        .await
        .unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let result = capture
            .execute_remote(&client, fixture.machine.id, cancellation.clone(), |event| {
                if cancelled && matches!(event, Progress::Output(_)) {
                    cancellation.cancel();
                }
            })
            .await;
        let Outcome::Failed { work, .. } = result else {
            panic!("{result:?}")
        };
        assert!(fixture.root.join("published-api").exists());
        assert_eq!(
            work.0.get("api"),
            Some(&ployz_build::TargetEvidence::Published)
        );
        assert_eq!(
            work.0.get("web"),
            Some(&ployz_build::TargetEvidence::Unknown)
        );
        assert_eq!(
            work.0.get("zzz"),
            Some(&ployz_build::TargetEvidence::Unattempted)
        );
        assert!(!fixture.root.join("published-zzz").exists());
    }
}

#[tokio::test]
async fn host_configuration_and_cache_clearing_share_remote_admission() {
    let fixture = Fixture::new().await;
    fs::write(&fixture.policy.configuration_file, "cpu_cores: -1").unwrap();
    let (_request, mut response) = fixture.request(Output::Validate).await;
    assert!(matches!(terminal(&mut response).await,
        Outcome::Failed { stage: Stage::Admission, message, .. } if message.contains("cpu_cores")));

    fs::write(&fixture.policy.configuration_file, "cpu_cores: 0.5").unwrap();
    let (request, mut response) = fixture.request(Output::Validate).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    assert!(matches!(
        ployz_build::clear_cache(&fixture.policy),
        Err(BuildError::Busy)
    ));
    assert!(matches!(
        Admission::try_acquire_with(&fixture.policy),
        Err(BuildError::Busy)
    ));
    drop(request);
    assert!(matches!(
        terminal(&mut response).await,
        Outcome::Failed {
            stage: Stage::Upload,
            ..
        }
    ));
    assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
}
