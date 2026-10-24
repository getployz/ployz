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
        Self::with(update, None).await
    }
    async fn with(update: impl FnOnce(&mut HostPolicy), sealing_key_dir: Option<PathBuf>) -> Self {
        let root =
            std::env::temp_dir().join(format!("ployz-remote-build-test-{}", MachineId::random()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(
            &root,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let mut store = LocalMachineStore::open(root.join("machine")).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: ployz_core::MachineName::parse("builder").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "127.0.0.1:7569".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        let machine = store.record().machine().unwrap().clone();
        let (runtime, _) = crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let runtime = match sealing_key_dir {
            Some(dir) => runtime.with_sealing_key(dir),
            None => runtime,
        };
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
        self.capture_with(None)
    }
    /// A capture whose env holds `NPM_TOKEN` sealed for `cluster`, when given.
    fn capture_with(
        &self,
        sealed: Option<&crate::sealed_env::test_support::Cluster>,
    ) -> ployz::build::CapturedBuild {
        let dockerfile = if sealed.is_some() {
            "FROM scratch\nARG NPM_TOKEN\nCOPY payload /payload\n"
        } else {
            "FROM scratch\nCOPY payload /payload\n"
        };
        self.capture_from(sealed, dockerfile)
    }
    fn capture_from(
        &self,
        sealed: Option<&crate::sealed_env::test_support::Cluster>,
        dockerfile: &str,
    ) -> ployz::build::CapturedBuild {
        let project_root = self.root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        fs::write(project_root.join("Dockerfile"), dockerfile).unwrap();
        fs::write(project_root.join("payload"), "captured-before-edit").unwrap();
        fs::write(project_root.join("ignored"), "excluded-private-value").unwrap();
        fs::write(project_root.join(".dockerignore"), "ignored\n").unwrap();
        let mut deployment = serde_json::json!({"namespace": "demo", "snapshots": [{
            "config": {"version": 2, "privateDns": "api", "healthcheck": {"type": "none"},
                "restartPolicy": "on-failure", "source": {"type": "image", "version": 1,
                "image": "example.test/api:built", "credentials": {"type": "none"}}},
            "resolvedEnv": {"PRIVATE_VALUE": "secret-value"}
        }]});
        if let Some(cluster) = sealed {
            let identity = &cluster.identity;
            fn object<'a>(
                value: &'a mut serde_json::Value,
                pointer: &str,
            ) -> &'a mut serde_json::Map<String, serde_json::Value> {
                value
                    .pointer_mut(pointer)
                    .and_then(serde_json::Value::as_object_mut)
                    .unwrap()
            }
            object(&mut deployment, "").insert(
                "sealedFor".into(),
                serde_json::json!({
                    "organization": identity.organization,
                    "environment": identity.environment,
                }),
            );
            object(&mut deployment, "/snapshots/0")
                .insert("serviceId".into(), serde_json::json!(identity.service));
            object(&mut deployment, "/snapshots/0/resolvedEnv").insert(
                "NPM_TOKEN".into(),
                serde_json::to_value(ployz_core::EnvValue::Sealed(cluster.seal(
                    "NPM_TOKEN",
                    ployz_core::SealedScope::Service(identity.service),
                    OPENED_TOKEN,
                )))
                .unwrap(),
            );
        }
        let intent =
            ployz_core::config::lower_deployment(serde_json::from_value(deployment).unwrap())
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
            None,
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

/// A dummy value the tests seal; it never leaves the test root.
const OPENED_TOKEN: &str = "opened-token-0123456789";

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

async fn until_fake_docker(
    limit: Duration,
    mut done: impl FnMut() -> bool,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(limit, async {
        while !done() {
            // real-wait: the fake docker script is a child process whose only signal is the files it writes.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
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
    assert!(matches!(
        event(&mut second_response).await,
        Event::Admitted { .. }
    ));
    let (third, mut third_response) = fixture.request(Output::Load).await;
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
        .request_upgrade(
            ployz_core::RequestMachineUpgradeRequest {
                attempt_id: ployz_core::MachineUpgradeAttemptId::random(),
                release: ployz_core::MachineRelease::parse("1.2.3").unwrap(),
            },
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::machine::LocalMachineError::Admission(crate::mutation::Error::Busy)
    ));

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
        .request_upgrade(
            ployz_core::RequestMachineUpgradeRequest {
                attempt_id: ployz_core::MachineUpgradeAttemptId::random(),
                release: ployz_core::MachineRelease::parse("1.2.3").unwrap(),
            },
            false,
        )
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
  'info --format') printf '%s\n' '{{"OSType":"linux","CgroupVersion":"2","Architecture":"x86_64","DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]]}}' ;;
  'buildx rm')
    if [ -f "$root/fail-cleanup" ] && {{ [ -f "$root/executed" ] || [ -f "$root/checking" ]; }}; then exit 1; fi ;;
  'buildx version'|'buildx create') exit 0 ;;
  'buildx inspect')
    if [ -f "$root/slow-check" ]; then : > "$root/checking"; exec sleep 30; fi ;;
  'buildx ls') printf '%s\n' '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
  'buildx bake')
    : > "$root/executed"
    printf '%s\n' "$@" > "$root/bake-args"
    printf '%s' "$BUILDX_METADATA_PROVENANCE" > "$root/metadata-provenance"
    while [ -f "$root/hold-build" ]; do sleep 0.01; done
    find source -type f > "$root/source-list"
    find source -name payload -exec cp '{{}}' "$root/received-payload" \;
    if [ -f private/sealed/NPM_TOKEN ]; then
      stat -c '%a' private/sealed/NPM_TOKEN > "$root/seen-mode"
      cp private/sealed/NPM_TOKEN "$root/seen-secret"
      if [ -f private/sealed/args.compose.yaml ]; then
        python3 -c 'import json; print(json.load(open("private/sealed/args.compose.yaml"))["services"]["api"]["build"]["args"]["NPM_TOKEN"], end="")' > "$root/seen-arg"
        stat -c '%a' private/sealed/args.compose.yaml > "$root/seen-arg-mode"
      fi
      printf 'token is %s here\n' "$(cat private/sealed/NPM_TOKEN)"
    fi
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
        None,
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
        policy.queue_timeout = Some(Duration::from_millis(200));
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
    // Only the queue wait runs on paused time; the upload and fake docker after it are real IO.
    // The active build holds a blocking task, which stops auto-advance, so the clock steps by hand.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    tokio::time::resume();
    // An early upload crosses a blocking task, gRPC, and a server-side write on other threads.
    let upload = fixture.root.join("build-upload/source");
    ployz_testkit::hold_for(
        Duration::from_secs(1),
        "the queued client kept waiting without uploading",
        async || {
            if execution.is_finished() {
                Err("client abandoned the configured queue wait")
            } else if upload.exists() {
                Err("queued capture uploaded early")
            } else {
                Ok(())
            }
        },
    )
    .await
    .unwrap();
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
        until_fake_docker(Duration::from_secs(3), || {
            fs::read_to_string(fixture.root.join("released-tags"))
                .unwrap_or_default()
                .lines()
                .count()
                >= 2
        })
        .await
        .expect("completed Build leaked its retention tag");
        let released = fs::read_to_string(fixture.root.join("released-tags")).unwrap();
        assert!(released.contains(":ployz-build-"), "{released}");
        assert!(!released.contains("example.test/api:built"), "{released}");
    }
}

#[tokio::test]
async fn a_sealed_value_opens_into_a_private_file_and_never_into_the_output() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
    let capture = fixture.capture_with(Some(&cluster));
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    let output = Mutex::new(Vec::new());
    let built = capture
        .execute_remote_images(
            &client,
            fixture.machine.id,
            None,
            tokio_util::sync::CancellationToken::new(),
            |progress| {
                if let Progress::Output(bytes) = progress {
                    output.lock().unwrap().extend(bytes);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(built.len(), 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-arg")).unwrap(),
        OPENED_TOKEN
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-arg-mode"))
            .unwrap()
            .trim(),
        "600"
    );
    let args = fs::read_to_string(fixture.root.join("bake-args")).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("metadata-provenance")).unwrap(),
        "disabled"
    );
    assert!(args.contains("args.compose.yaml"));
    assert!(!args.contains(OPENED_TOKEN));
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-secret")).unwrap(),
        OPENED_TOKEN
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-mode"))
            .unwrap()
            .trim(),
        "600"
    );
    let output = String::from_utf8(output.into_inner().unwrap()).unwrap();
    assert!(
        output.contains("token is [secret NPM_TOKEN] here"),
        "{output}"
    );
    assert!(!output.contains(OPENED_TOKEN), "{output}");
    let mut leftovers = Vec::new();
    files_named(&fixture.root, "NPM_TOKEN", &mut leftovers);
    files_named(&fixture.root, "args.compose.yaml", &mut leftovers);
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn a_sealed_value_read_only_by_mount_builds_with_no_arg_override() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
    let capture = fixture.capture_from(Some(&cluster), "FROM scratch\nCOPY payload /payload\n");
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    capture
        .execute_remote_images(
            &client,
            fixture.machine.id,
            None,
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-secret")).unwrap(),
        OPENED_TOKEN
    );
    assert!(!fixture.root.join("seen-arg").exists());
    assert!(
        !fs::read_to_string(fixture.root.join("bake-args"))
            .unwrap()
            .contains("args.compose.yaml")
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("metadata-provenance")).unwrap(),
        ""
    );
}

#[tokio::test]
async fn a_failed_sealed_arg_build_removes_its_private_override() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
    fs::write(fixture.root.join("fail-after-output"), "").unwrap();
    let capture = fixture.capture_with(Some(&cluster));
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
    assert!(matches!(
        result,
        ployz::build::RemoteBuildFailure::Failed {
            stage: Stage::Building,
            ..
        }
    ));
    assert_eq!(
        fs::read_to_string(fixture.root.join("seen-arg")).unwrap(),
        OPENED_TOKEN
    );
    let mut leftovers = Vec::new();
    files_named(&fixture.root, "NPM_TOKEN", &mut leftovers);
    files_named(&fixture.root, "args.compose.yaml", &mut leftovers);
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn a_cancelled_sealed_build_leaves_no_opened_file_even_in_a_retained_root() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    for uncertain in [false, true] {
        let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
        fs::write(fixture.root.join("slow"), "").unwrap();
        if uncertain {
            fs::write(fixture.root.join("fail-cleanup"), "").unwrap();
        }
        let capture = fixture.capture_with(Some(&cluster));
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
        assert_eq!(
            fs::read_to_string(fixture.root.join("seen-secret")).unwrap(),
            OPENED_TOKEN
        );
        let upload_root = fixture.policy.state_directory.join("build-upload");
        if uncertain {
            assert!(
                matches!(result, ployz::build::RemoteBuildFailure::Unknown { .. }),
                "{result:?}"
            );
            assert!(upload_root.is_dir(), "an uncertain upload root is retained");
        } else {
            assert!(
                matches!(
                    result,
                    ployz::build::RemoteBuildFailure::Failed {
                        stage: Stage::Building,
                        ..
                    }
                ),
                "{result:?}"
            );
            assert!(!upload_root.exists());
        }
        let mut leftovers = Vec::new();
        files_named(&fixture.root, "NPM_TOKEN", &mut leftovers);
        files_named(&fixture.root, "args.compose.yaml", &mut leftovers);
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}

fn files_named(root: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_named(&path, name, found);
        } else if entry.file_name() == name {
            found.push(path);
        }
    }
}

#[tokio::test]
async fn a_sealed_value_this_machine_cannot_open_fails_closed_before_docker() {
    let sealed_by = crate::sealed_env::test_support::Cluster::new();
    let other_key = crate::sealed_env::test_support::Cluster::new();
    for key_dir in [None, Some(other_key.dir.0.clone())] {
        let fixture = Fixture::with(|_| {}, key_dir).await;
        let capture = fixture.capture_with(Some(&sealed_by));
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
        let ployz::build::RemoteBuildFailure::Failed { stage, message, .. } = result else {
            panic!("{result:?}");
        };
        assert_eq!(stage, Stage::Preparation);
        assert!(
            message.contains("NPM_TOKEN") || message.contains("Sealing Key"),
            "{message}"
        );
        assert!(!message.contains(OPENED_TOKEN), "{message}");
        assert!(!fixture.root.join("executed").exists());
        assert!(!fixture.root.join("seen-secret").exists());
    }
}
