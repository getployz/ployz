//! Owned transport tests. The Docker stand-in proves orchestration and
//! termination evidence; the informing test proves a real remote image.

mod admission;

use super::*;
use crate::{
    machine::{LocalMachineStore, RecordOwner},
    machine_api::{MachineApi, MachineService},
};
use ployz_core::MachineRpcClient;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

struct Fixture {
    shutdown: tokio_util::sync::CancellationToken,
    root: PathBuf,
    policy: HostPolicy,
    cluster: tokio::task::JoinHandle<()>,
    local: crate::machine::LocalMachine,
    machine: ployz_core::Machine,
    address: String,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    running_builds: tokio::sync::watch::Receiver<u32>,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_policy(|_| {}).await
    }
    async fn with_policy(update: impl FnOnce(&mut HostPolicy)) -> Self {
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
            .initialize(ployz_core::InitializeRequest {
                initial_policy: Default::default(),
                name: ployz_core::MachineName::parse("builder").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "127.0.0.1:7569".parse().unwrap(),
                )],
                wireguard_mtu: None,
            })
            .unwrap();
        let (runtime, _) = crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let (replicated, cluster) = crate::corrosion::fake_cluster::store().await;
        replicated.publish_local_machine(&machine).await.unwrap();
        let store = RecordOwner::spawn(store).unwrap();
        let records = store.watch();
        let mut service = MachineService::with_cluster(
            store,
            Some((replicated, crate::corrosion::AdminClient::new("/no/admin"))),
        )
        .with_optional_containers(Some(runtime));
        let mut policy = HostPolicy {
            state_directory: root.clone(),
            docker: root.join("docker"),
            active_timeout: Duration::from_secs(30),
            configuration_file: root.join("build.yaml"),
            ..Default::default()
        };
        update(&mut policy);
        let shutdown = tokio_util::sync::CancellationToken::new();
        let builds = Runner::new(policy.clone(), shutdown.clone()).unwrap();
        let running_builds = builds.running_builds();
        builds.follow(records);
        service.builds = builds;
        let local = service.local();
        write_docker(&policy.docker, &root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let api = MachineApi::from_local(service);
        let server = tokio::spawn(
            Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)),
        );
        Self {
            shutdown,
            root,
            policy,
            cluster,
            local,
            machine,
            address,
            server,
            running_builds,
        }
    }
    async fn request(
        &self,
        output: Output,
    ) -> (mpsc::Sender<OpaquePayload>, tonic::Streaming<OpaquePayload>) {
        self.request_frame(Input::Start(Definition {
            retained_tags: Vec::new(),
            image_contexts: Default::default(),
            targets: vec![ployz_build::Target {
                name: "api".into(),
                platforms: Vec::new(),
            }],
            output,
            no_cache: false,
            pull: false,
        }))
        .await
    }
    async fn request_frame(
        &self,
        frame: Input,
    ) -> (mpsc::Sender<OpaquePayload>, tonic::Streaming<OpaquePayload>) {
        let mut client = MachineRpcClient::connect(self.address.clone())
            .await
            .unwrap();
        let (sender, receiver) = mpsc::channel(2);
        sender.send(remote::encode(&frame).unwrap()).await.unwrap();
        let response = client
            .build(ReceiverStream::new(receiver))
            .await
            .unwrap()
            .into_inner();
        (sender, response)
    }
    fn capture(&self) -> ployz::build::CapturedBuild {
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
        let intent = ployz_core::config::lower_deployment(
            serde_json::from_value(serde_json::json!({"projectName": "demo", "snapshots": [{
                "config": {"version": 2, "privateDns": "api", "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure", "source": {"type": "image", "version": 1,
                    "image": "example.test/api:built", "credentials": {"type": "none"}}},
                "resolvedEnv": {"PRIVATE_VALUE": "secret-value"}
            }]}))
            .unwrap(),
        )
        .unwrap();
        ployz::build::capture(
            &intent,
            [(
                ployz_core::ServiceName::parse("api").unwrap(),
                ployz::build::BuildSpec {
                    recipe: ployz::build::Recipe::Dockerfile(project_root.join("Dockerfile")),
                    context: project_root,
                },
            )]
            .into(),
        )
        .unwrap()
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

fn failure(
    result: Result<Vec<ployz::build::BuiltService>, ployz::build::Error>,
) -> ployz::build::RemoteBuildFailure {
    match result {
        Err(ployz::build::Error::RemoteBuild { outcome }) => *outcome,
        other => panic!("expected remote Build failure evidence: {other:?}"),
    }
}

#[tokio::test]
async fn admitted_upload_queues_competitors_and_disconnect_releases_unused_ownership() {
    let fixture = Fixture::new().await;
    let (first, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    assert!(!fixture.root.join("executed").exists());
    let (second, mut queued) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut queued).await, Event::Progress(_)));
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
    assert!(matches!(event(&mut queued).await, Event::Admitted { .. }));
    drop(second);
    let _ = terminal(&mut queued).await;
    let (_third, mut admitted) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut admitted).await, Event::Admitted { .. }));
}

#[tokio::test]
async fn running_builds_are_counted_until_they_end() {
    let fixture = Fixture::new().await;
    let mut running = fixture.running_builds.clone();
    let count = async |running: &mut tokio::sync::watch::Receiver<u32>, count| {
        tokio::time::timeout(
            Duration::from_secs(5),
            running.wait_for(|now| *now == count),
        )
        .await
        .unwrap_or_else(|_| panic!("running Builds never read {count}"))
        .unwrap();
    };
    count(&mut running, 0).await;
    let (first, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    count(&mut running, 1).await;
    drop(first);
    let _ = terminal(&mut response).await;
    count(&mut running, 0).await;
}

#[tokio::test]
async fn build_concurrency_bounds_simultaneous_builds_on_separate_slots() {
    let fixture = Fixture::new().await;
    // Automatic: a Machine that accepts Services builds one at a time.
    let (first, mut first_response) = fixture.request(Output::Load).await;
    assert!(matches!(
        event(&mut first_response).await,
        Event::Admitted { .. }
    ));
    let (second, mut second_response) = fixture.request(Output::Load).await;
    assert!(matches!(
        event(&mut second_response).await,
        Event::Progress(_)
    ));

    fixture
        .local
        .update(
            serde_json::from_value(serde_json::json!({
                "update": {"build_concurrency": {"action": "set", "value": 2}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    // The next arrival reads the explicit limit: the queued Build is admitted
    // first, on its own build slot, and the arrival waits for a third slot.
    let (third, mut third_response) = fixture.request(Output::Load).await;
    assert!(matches!(
        event(&mut second_response).await,
        Event::Admitted { .. }
    ));
    assert!(matches!(
        event(&mut third_response).await,
        Event::Progress(_)
    ));

    drop(first);
    let _ = terminal(&mut first_response).await;
    assert!(matches!(
        event(&mut third_response).await,
        Event::Admitted { .. }
    ));
    drop((second, third));
    let _ = terminal(&mut second_response).await;
    let _ = terminal(&mut third_response).await;
}

#[tokio::test]
async fn active_build_refuses_upgrade_but_not_machine_mutations() {
    let fixture = Fixture::new().await;
    let (sender, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));

    let updated = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.local.update(
            serde_json::from_value(serde_json::json!({
                "update": {
                    "label_changes": {"pool": "build"},
                    "accepts_builds": false,
                    "accepts_services": false,
                    "accepts_ingress": false
                }
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("policy update waited for active Build")
    .unwrap();
    assert!(!updated.machine.accepts_builds);
    assert!(!updated.machine.accepts_services);
    assert!(!updated.machine.accepts_ingress);
    assert_eq!(
        serde_json::to_value(&updated.machine.labels).unwrap(),
        serde_json::json!({"pool": "build"})
    );

    let error = fixture
        .local
        .request_upgrade(ployz_core::RequestMachineUpgradeRequest {
            attempt_id: ployz_core::MachineUpgradeAttemptId::random(),
            release: ployz_core::MachineRelease::parse("1.2.3").unwrap(),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::machine::LocalMachineError::Admission(crate::mutation::Error::Busy)
    ));

    // A Build holds only its build slot, never the Machine mutation lock, so
    // deploys and ordinary mutations on the building Machine proceed.
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.local.update(
            serde_json::from_value(serde_json::json!({"update": {"name": "renamed"}})).unwrap(),
        ),
    )
    .await
    .expect("Machine rename waited for active Build execution")
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture
            .local
            .set_management_client(ployz_core::SetManagementClientRequest::Clear {
                label: ployz_core::ManagementClientLabel::parse("cloud").unwrap(),
            }),
    )
    .await
    .expect("ordinary mutation waited for active Build execution")
    .unwrap();
    drop(sender);
    let _ = terminal(&mut response).await;
}

#[tokio::test]
async fn durable_upgrade_marker_refuses_build_before_execution() {
    let fixture = Fixture::new().await;
    let marker = fixture.root.join("machine/.upgrade-active");
    fs::write(&marker, "active-upgrade").unwrap();

    let policy = serde_json::from_value(serde_json::json!({
        "update": {"label_changes": {"pool": "build"}, "accepts_builds": false, "accepts_services": false, "accepts_ingress": false}
    }))
    .unwrap();
    assert!(matches!(
        fixture.local.update(policy).await,
        Err(crate::machine::LocalMachineError::Admission(
            crate::mutation::Error::Busy
        ))
    ));

    for input in [
        Input::Start(Definition {
            targets: vec![ployz_build::Target {
                name: "api".into(),
                platforms: vec!["linux/amd64".into()],
            }],
            output: Output::Load,
            retained_tags: Vec::new(),
            image_contexts: Default::default(),
            no_cache: false,
            pull: false,
        }),
        Input::Check(vec![ployz_build::Target {
            name: "api".into(),
            platforms: vec!["linux/amd64".into()],
        }]),
    ] {
        let (_sender, mut rejected) = fixture.request_frame(input).await;
        assert!(matches!(terminal(&mut rejected).await,
            Outcome::Failed { stage: Stage::Admission, message, .. }
                if message.contains("installation or upgrade is active")));
        assert!(!fixture.root.join("executed").exists());
    }

    fs::remove_file(marker).unwrap();
    let (sender, mut admitted) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut admitted).await, Event::Admitted { .. }));
    drop(sender);
    let _ = terminal(&mut admitted).await;
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
    let images = capture
        .execute_remote_images(
            &client,
            fixture.machine.id,
            None,
            tokio_util::sync::CancellationToken::new(),
            |event| events.lock().unwrap().push(event),
        )
        .await
        .unwrap();
    let [built] = images.as_slice() else {
        panic!("expected one image: {images:?}")
    };
    assert_eq!(built.machine_id, fixture.machine.id);
    let image = &built.built;
    assert_eq!(image.reference, format!("sha256:{}", "1".repeat(64)));
    assert_eq!(image.platforms, ["linux/amd64"]);
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
async fn retained_build_images_do_not_block_same_machine_mutation() {
    let fixture = Fixture::new().await;
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let retained = fixture
        .capture()
        .execute_remote_images(
            &client,
            fixture.machine.id,
            None,
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

    tokio::time::timeout(
        Duration::from_secs(2),
        fixture
            .local
            .set_management_client(ployz_core::SetManagementClientRequest::Clear {
                label: ployz_core::ManagementClientLabel::parse("cloud").unwrap(),
            }),
    )
    .await
    .expect("retained Build stream held the local mutation mutex")
    .unwrap();

    let error = fixture
        .local
        .request_upgrade(ployz_core::RequestMachineUpgradeRequest {
            attempt_id: ployz_core::MachineUpgradeAttemptId::random(),
            release: ployz_core::MachineRelease::parse("1.2.3").unwrap(),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::machine::LocalMachineError::Admission(crate::mutation::Error::Busy)
    ));
    drop(retained);
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
    let result = failure(
        capture
            .execute_remote_images(
                &client,
                fixture.machine.id,
                None,
                tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await,
    );
    let ployz::build::RemoteBuildFailure::Failed {
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
    if [ -f "$root/fail-cleanup" ] && {{ [ -f "$root/executed" ] || [ -f "$root/checking" ]; }}; then exit 1; fi ;;
  'buildx version'|'buildx create') exit 0 ;;
  'buildx inspect')
    if [ -f "$root/slow-check" ]; then : > "$root/checking"; exec sleep 30; fi ;;
  'buildx ls') printf '%s\n' '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
  'buildx bake')
    : > "$root/executed"
    while [ -f "$root/hold-build" ]; do sleep 0.01; done
    find source -type f > "$root/source-list"
    find source -name payload -exec cp '{{}}' "$root/received-payload" \;
    if [ -f "$root/slow" ]; then printf 'building\n'; exec sleep 30; fi
    previous=
    for arg in "$@"; do
      if [ "$previous" = --metadata-file ]; then printf '%s\n' '{{"api":{{"containerimage.digest":"sha256:{}","image.name":"example.test/api:built"}}}}' > "$arg"; fi
      previous=$arg
    done
    if [ -f "$root/fail-after-output" ]; then exit 1; fi ;;
  'image rm') printf '%s\n' "$*" >> "$root/released-tags" ;;
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
        let result = failure(
            capture
                .execute_remote_images(
                    &client,
                    fixture.machine.id,
                    None,
                    cancellation,
                    |progress| {
                        if matches!(progress, Progress::Output(_)) {
                            cancel.cancel();
                        }
                    },
                )
                .await,
        );
        if uncertain {
            assert!(
                matches!(result, ployz::build::RemoteBuildFailure::Unknown { .. }),
                "{result:?}"
            );
            assert!(
                matches!(Admission::try_acquire_with(&fixture.policy), Err(error) if error.is_unknown())
            );
        } else {
            assert!(
                matches!(result, ployz::build::RemoteBuildFailure::Failed { stage: Stage::Building, message, .. } if message.contains("cancel"))
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
            retained_tags: Vec::new(),
            image_contexts: Default::default(),
            targets: vec![ployz_build::Target {
                name: "api".into(),
                platforms: Vec::new(),
            }],
            output: Output::Load,
            no_cache: false,
            pull: false,
        }))
        .unwrap()))
        .await
        .unwrap();
    let mut responses = start(
        fixture.local.clone(),
        ReceiverStream::new(receiver),
        Runner::new(policy, Default::default()).unwrap(),
    );
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
    for failed in ["fail-after-output", "fail-cleanup"] {
        let fixture = Fixture::new().await;
        let capture = fixture.capture();
        fs::write(fixture.root.join(failed), "").unwrap();
        let client = ployz::connect::connect(
            Path::new("/missing-test-config"),
            Some(&fixture.address.replace("http://", "tcp://")),
            None,
        )
        .await
        .unwrap();
        let result = failure(
            capture
                .execute_remote_images(
                    &client,
                    fixture.machine.id,
                    None,
                    tokio_util::sync::CancellationToken::new(),
                    |_| {},
                )
                .await,
        );
        let work = match result {
            ployz::build::RemoteBuildFailure::Unknown { work, .. } if failed == "fail-cleanup" => {
                work
            }
            ployz::build::RemoteBuildFailure::Failed { work, .. } if failed != "fail-cleanup" => {
                work
            }
            other @ (ployz::build::RemoteBuildFailure::Failed { .. }
            | ployz::build::RemoteBuildFailure::Unknown { .. }) => {
                panic!("unexpected terminal result: {other:?}")
            }
        };
        assert!(
            matches!(work.0.get("api"), Some(ployz_build::TargetEvidence::Image(image)) if image.reference.ends_with(&"1".repeat(64)))
        );
    }
}

#[tokio::test]
async fn queued_disconnect_cancellation_expiry_and_full_queue_never_upload() {
    let fixture = Fixture::with_policy(|policy| {
        policy.queue_capacity = 1;
        policy.queue_timeout = Duration::from_millis(200);
    })
    .await;
    let (active, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    for action in ["disconnect", "cancel", "early-upload", "expire"] {
        let (waiting, mut queued) = fixture.request(Output::Load).await;
        assert!(matches!(
            event(&mut queued).await,
            Event::Progress(Progress::Stage(Stage::Queued))
        ));
        let (_full, mut rejected) = fixture.request(Output::Load).await;
        assert!(
            matches!(terminal(&mut rejected).await, Outcome::Failed { stage: Stage::Queued, message, work } if message.contains("full") && work.0.get("api") == Some(&ployz_build::TargetEvidence::Unattempted))
        );
        match action {
            "disconnect" => drop(waiting),
            "cancel" => waiting
                .send(remote::encode(&Input::Cancel).unwrap())
                .await
                .unwrap(),
            "early-upload" => waiting
                .send(remote::encode(&Input::Data(vec![1])).unwrap())
                .await
                .unwrap(),
            "expire" => {}
            _ => unreachable!(),
        }
        let Outcome::Failed {
            stage: Stage::Queued,
            message,
            work,
        } = terminal(&mut queued).await
        else {
            panic!("queued work ran")
        };
        let expected = match action {
            "disconnect" => "disconnected",
            "cancel" => "cancelled",
            "early-upload" => "before admission",
            "expire" => "expired",
            _ => unreachable!(),
        };
        assert!(message.contains(expected), "{message}");
        assert_eq!(
            work.0.get("api"),
            Some(&ployz_build::TargetEvidence::Unattempted)
        );
        assert!(!fixture.root.join("executed").exists());
    }
    drop(active);
    let _ = terminal(&mut response).await;
}

#[tokio::test]
async fn captured_client_cancels_in_queue_without_uploading() {
    let fixture = Fixture::new().await;
    let (_active, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    let capture = fixture.capture();
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let result = failure(
        capture
            .execute_remote_images(
                &client,
                fixture.machine.id,
                None,
                cancellation.clone(),
                |event| {
                    if matches!(event, Progress::Stage(Stage::Queued)) {
                        cancellation.cancel();
                    }
                    assert!(!matches!(event, Progress::Stage(Stage::Upload)));
                },
            )
            .await,
    );
    assert!(
        matches!(result, ployz::build::RemoteBuildFailure::Failed { stage: Stage::Queued, message, .. } if message.contains("cancelled"))
    );
    assert!(!fixture.root.join("executed").exists());
}

#[tokio::test]
async fn client_waits_past_connection_deadline_and_uploads_only_after_admission() {
    let fixture = Fixture::new().await;
    let (active, mut response) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
    let capture = fixture.capture();
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let (waiting, mut observed) = mpsc::channel(1);
    let selected = fixture.machine.id;
    let execution = tokio::spawn(async move {
        capture
            .execute_remote_images(&client, selected, None, Default::default(), |event| {
                if matches!(event, Progress::Stage(Stage::Queued)) {
                    waiting.try_send(()).unwrap();
                }
            })
            .await
    });
    observed.recv().await.unwrap();
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert!(
        !execution.is_finished(),
        "client abandoned the configured queue wait"
    );
    assert!(
        !fixture.root.join("build-upload/source").exists(),
        "queued capture uploaded early"
    );
    drop(active);
    let _ = terminal(&mut response).await;
    assert_eq!(execution.await.unwrap().unwrap().len(), 1);
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

#[tokio::test]
async fn dropping_completed_build_releases_only_its_temporary_tags() {
    for stopping in [false, true] {
        let fixture = Fixture::new().await;
        let client = ployz::connect::connect(
            Path::new("/missing-test-config"),
            Some(&fixture.address.replace("http://", "tcp://")),
            None,
        )
        .await
        .unwrap();
        let images = fixture
            .capture()
            .execute_remote_images(
                &client,
                fixture.machine.id,
                None,
                tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        // Holding completed images must not occupy the Machine's active Build slot.
        let next = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.capture().execute_remote_images(
                &client,
                fixture.machine.id,
                None,
                tokio_util::sync::CancellationToken::new(),
                |_| {},
            ),
        )
        .await
        .expect("image retention held queue admission")
        .unwrap();
        assert!(!fixture.root.join("released-tags").exists());
        drop(next);
        if stopping {
            fixture.shutdown.cancel();
        } else {
            drop(images);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while fs::read_to_string(fixture.root.join("released-tags"))
                .unwrap_or_default()
                .lines()
                .count()
                < 2
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("completed Build leaked its retention tag");
        let released = fs::read_to_string(fixture.root.join("released-tags")).unwrap();
        assert!(released.contains(":ployz-build-"), "{released}");
        assert!(!released.contains("example.test/api:built"), "{released}");
    }
}
