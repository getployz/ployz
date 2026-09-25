//! Informing evidence for the product remote path. Uses real BuildKit and
//! executes the resulting image on the Build Machine or its destination.

use ployz::build::{Recipe, RemoteBuildFailure};
use ployz_build::{
    Output, Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::{MachineId, MachineRpcClient, MachineTarget, OpaquePayload, apply_one_target};
use ployz_testkit::{Cluster, ClusterPlan};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn remote_dockerfile_runs_on_selected_machine_and_bounds_abandoned_attempts() {
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-build-802-{}", std::process::id()), 2).unwrap(),
    )
    .unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let selected = machines.get(1).unwrap().id;
    let address = cluster.api_address(0).unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let image = format!("ployz-remote-802-{}:built", std::process::id());
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nARG MESSAGE\nCOPY . /source\nRUN printf '%s\\n' \"$MESSAGE\" > /message\nCMD [\"cat\", \"/source/payload\", \"/message\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "remote-image-ran\n").unwrap();
    fs::write(root.join("ignored"), "excluded source").unwrap();
    fs::write(root.join(".dockerignore"), "ignored\n").unwrap();
    let client = ployz::connect::connect(
        Path::new("/missing-ployz-test-config"),
        Some(&address),
        None,
    )
    .await
    .unwrap();
    // A literal dollar must survive Buildx interpolation on the Build Machine.
    let built = super::capture(
        root,
        &image,
        serde_json::json!({"MESSAGE": "literal-$LATER"}),
        Recipe::Dockerfile(root.join("Dockerfile")),
    )
    .execute_remote_images(&client, selected, None, CancellationToken::new(), |_| {})
    .await
    .unwrap();
    let [built] = built.as_slice() else {
        panic!("expected one built Service, got {built:?}");
    };
    assert_eq!(built.machine_id, selected);
    let exact = &built.built.reference;
    assert_eq!(
        cluster
            .machine_shell(1, &format!("docker run --rm {exact}"))
            .unwrap(),
        "remote-image-ran\nliteral-$LATER\n"
    );
    cluster
        .machine_shell(
            1,
            &format!("docker run --rm {exact} test ! -e /source/ignored"),
        )
        .unwrap();
    assert!(
        cluster
            .machine_shell(0, &format!("docker image inspect {image}"))
            .is_err(),
        "a Build transferred its image to the entry Machine"
    );

    let (first, mut response) = request(&address, selected).await;
    assert!(
        matches!(event(&mut response).await, Event::Admitted { machine_id, .. } if machine_id == selected)
    );
    let (competitor, mut queued) = request(&address, selected).await;
    assert!(matches!(
        event(&mut queued).await,
        Event::Progress(Progress::Stage(Stage::Queued))
    ));
    drop(competitor);
    assert!(matches!(
        terminal(&mut queued).await,
        Outcome::Failed {
            stage: Stage::Queued,
            ..
        }
    ));
    for frame in [
        Input::Entry {
            path: b"source".to_vec(),
            kind: remote::Kind::Directory,
            mode: 0o755,
        },
        Input::Entry {
            path: b"source/incomplete".to_vec(),
            kind: remote::Kind::File { size: 64 },
            mode: 0o644,
        },
        Input::Data(vec![1; 32]),
    ] {
        first.send(remote::encode(&frame).unwrap()).await.unwrap();
    }
    drop(first);
    assert!(matches!(
        terminal(&mut response).await,
        Outcome::Failed {
            stage: Stage::Upload,
            ..
        }
    ));

    for uncertain in [false, true] {
        // A distinct step per attempt, so no attempt resumes from the other's cache.
        fs::write(
            root.join("Dockerfile"),
            format!("FROM alpine:3.23.3\nRUN echo REMOTE_BUILD_ACTIVE {uncertain}; sleep 300\n"),
        )
        .unwrap();
        if uncertain {
            // A fault injector on this disposable Machine refuses cleanup;
            // preparation and the active build still use actual Docker.
            cluster.machine_shell(1, "mv /usr/local/bin/docker /usr/local/bin/docker-real; printf '%s\n' '#!/bin/sh' 'if [ \"$1 $2\" = \"buildx rm\" ] && [ -f /tmp/refuse-build-cleanup ]; then exit 1; fi' 'exec /usr/local/bin/docker-real \"$@\"' > /usr/local/bin/docker; chmod +x /usr/local/bin/docker").unwrap();
        }
        let capture = super::capture(
            root,
            &image,
            serde_json::json!({}),
            Recipe::Dockerfile(root.join("Dockerfile")),
        );
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            capture.execute_remote_images(&client, selected, None, cancellation, |event| {
                let active = match &event {
                    Progress::Output(bytes) => {
                        String::from_utf8_lossy(bytes).contains("REMOTE_BUILD_ACTIVE")
                    }
                    Progress::StepOutput { text, .. } => text.contains("REMOTE_BUILD_ACTIVE"),
                    Progress::Stage(_)
                    | Progress::Step(_)
                    | Progress::Timing { .. }
                    | Progress::Target { .. } => false,
                };
                if active && !cancel.is_cancelled() {
                    if uncertain {
                        cluster
                            .machine_shell(1, "touch /tmp/refuse-build-cleanup")
                            .unwrap();
                    }
                    cancel.cancel();
                }
            }),
        )
        .await
        .unwrap();
        let Err(ployz::build::Error::RemoteBuild { outcome }) = result else {
            panic!("{result:?}");
        };
        if uncertain {
            assert!(
                matches!(*outcome, RemoteBuildFailure::Unknown { .. }),
                "{outcome:?}"
            );
            let (_attempt, mut blocked) = request(&address, selected).await;
            assert!(matches!(
                terminal(&mut blocked).await,
                Outcome::Unknown {
                    stage: Stage::Admission,
                    ..
                }
            ));
        } else {
            assert!(
                matches!(*outcome, RemoteBuildFailure::Failed { .. }),
                "{outcome:?}"
            );
            assert!(
                cluster
                    .machine_shell(
                        1,
                        "docker ps -a --filter name=buildx_buildkit_ployz-00 --format '{{.ID}}'"
                    )
                    .unwrap()
                    .trim()
                    .is_empty()
            );
        }
    }
}

async fn request(
    address: &str,
    selected: MachineId,
) -> (mpsc::Sender<OpaquePayload>, tonic::Streaming<OpaquePayload>) {
    let mut rpc = MachineRpcClient::connect(address.replace("tcp://", "http://"))
        .await
        .unwrap();
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(
            remote::encode(&Input::Start(Definition {
                retained_tags: Vec::new(),
                image_contexts: Default::default(),
                targets: vec![ployz_build::Target {
                    name: "app".into(),
                    platforms: Vec::new(),
                }],
                output: Output::Load,
                no_cache: false,
                pull: false,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let mut request = tonic::Request::new(ReceiverStream::new(receiver));
    apply_one_target(request.metadata_mut(), &MachineTarget::from(&selected));
    (sender, rpc.build(request).await.unwrap().into_inner())
}
async fn event(response: &mut tonic::Streaming<OpaquePayload>) -> Event {
    let payload = tokio::time::timeout(Duration::from_secs(15), response.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    remote::decode(&payload).unwrap()
}
async fn terminal(response: &mut tonic::Streaming<OpaquePayload>) -> Outcome {
    loop {
        if let Event::Finished(outcome) = event(response).await {
            return outcome;
        }
    }
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn queue_upload_timeout_and_daemon_restart_discard_waiters_before_a_safe_build() {
    let mut plan = ClusterPlan::new(&format!("l3-build-807-{}", std::process::id()), 1).unwrap();
    plan.machines
        .first_mut()
        .unwrap()
        .environment
        .insert("PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS".into(), "1".into());
    let cluster = Cluster::create(plan).unwrap();
    let selected = cluster.initialize_entry().await.unwrap().id;
    let address = cluster.api_address(0).unwrap();
    let (uploading, mut active) = request(&address, selected).await;
    assert!(matches!(event(&mut active).await, Event::Admitted { .. }));
    let (disconnected, mut queued) = request(&address, selected).await;
    assert!(matches!(
        event(&mut queued).await,
        Event::Progress(Progress::Stage(Stage::Queued))
    ));
    drop(disconnected);
    assert!(matches!(
        terminal(&mut queued).await,
        Outcome::Failed {
            stage: Stage::Queued,
            ..
        }
    ));
    assert!(
        matches!(terminal(&mut active).await, Outcome::Failed { stage: Stage::Upload, message, .. } if message.contains("timeout"))
    );
    drop(uploading);

    // Keep an admitted partial upload and another waiting connection across
    // restart. Restore the normal active budget for the subsequent real build.
    cluster.machine_shell(0, "mv /usr/local/bin/ployzd /usr/local/bin/ployzd-real; printf '%s\n' '#!/bin/sh' 'unset PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS' 'exec /usr/local/bin/ployzd-real \"$@\"' > /usr/local/bin/ployzd; chmod +x /usr/local/bin/ployzd").unwrap();
    cluster.restart(0).unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    let (interrupted, mut active) = request(&address, selected).await;
    assert!(matches!(event(&mut active).await, Event::Admitted { .. }));
    interrupted
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
    let (_waiting, mut queued) = request(&address, selected).await;
    assert!(matches!(
        event(&mut queued).await,
        Event::Progress(Progress::Stage(Stage::Queued))
    ));
    cluster.restart(0).unwrap();
    for response in [&mut active, &mut queued] {
        let result = tokio::time::timeout(Duration::from_secs(10), response.message())
            .await
            .unwrap();
        if let Ok(Some(payload)) = result {
            assert!(matches!(
                remote::decode::<Event>(&payload).unwrap(),
                Event::Finished(Outcome::Failed { .. } | Outcome::Unknown { .. })
            ));
        }
    }
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"cat\", \"/payload\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "safe-after-restart\n").unwrap();
    let client = ployz::connect::connect(
        Path::new("/missing-ployz-test-config"),
        Some(&address),
        None,
    )
    .await
    .unwrap();
    let built = tokio::time::timeout(
        Duration::from_secs(180),
        super::capture(
            root,
            "ployz-restart:built",
            serde_json::json!({}),
            Recipe::Dockerfile(root.join("Dockerfile")),
        )
        .execute_remote_images(&client, selected, None, CancellationToken::new(), |_| {}),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        cluster
            .machine_shell(
                0,
                &format!("docker run --rm {}", built.first().unwrap().built.reference)
            )
            .unwrap(),
        "safe-after-restart\n"
    );
}

/// The Build Machine accepts no Services and the destination accepts no
/// Builds, so the running image can only have arrived by Direct Image Transfer.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn prepared_build_is_delivered_to_its_destination_without_a_registry() {
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-build-803-{}", std::process::id()), 2).unwrap(),
    )
    .unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let destination = machines.first().unwrap().id;
    let builder = machines.get(1).unwrap().id;
    let address = cluster.api_address(0).unwrap();
    for (machine, policy) in [
        (destination, "--accepts-builds=false"),
        (builder, "--accepts-services=false"),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "--connect",
                &address,
                "machine",
                "update",
                machine.as_str(),
                policy,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"sleep\", \"3600\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "remote-application-ran\n").unwrap();
    let session = super::session(&cluster).await;
    let prepared = tokio::time::timeout(
        Duration::from_secs(240),
        super::prepare(
            &session,
            super::git_deployment("dockerfile", "unused"),
            root,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        prepared
            .build_receipts()
            .values()
            .next()
            .unwrap()
            .machine_id,
        builder
    );
    let outcome = prepared.confirm().unwrap().finished().await.unwrap();
    assert!(
        matches!(outcome, ployz_core::DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    drop(prepared);
    assert_temporary_tags_released(&cluster).await;
    let container = cluster
        .machine_shell(0, "docker ps -q --filter label=ployz.service.name=app")
        .unwrap();
    let container = container.trim();
    assert!(!container.is_empty(), "application was not running");
    assert_eq!(
        cluster
            .machine_shell(0, &format!("docker exec {container} cat /payload"))
            .unwrap(),
        "remote-application-ran\n"
    );
    let spec_image = cluster
        .machine_shell(
            0,
            &format!("docker inspect {container} --format '{{{{.Config.Image}}}}'"),
        )
        .unwrap();
    assert!(spec_image.contains("@sha256:"), "{spec_image}");
    assert_eq!(
        cluster
            .machine_shell(1, "docker ps -q --filter label=ployz.service.name=app")
            .unwrap(),
        ""
    );
    session.close().await;
}

async fn assert_temporary_tags_released(cluster: &Cluster) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut empty = true;
            for machine in 0..2 {
                let tags = cluster
                    .machine_shell(machine, "docker image ls --format '{{.Tag}}'")
                    .unwrap();
                empty &= !tags.lines().any(|tag| tag.starts_with("ployz-build-"));
            }
            if empty {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("command left temporary Build tags on a Machine");
}

/// Both testkit Machines run this host's kernel, so the derived platform is
/// native for both. Native AMD64 plus native ARM64 Machines belong to the
/// qualification path.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn railpack_preparation_derives_machine_platforms() {
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-build-806-{}", std::process::id()), 2).unwrap(),
    )
    .unwrap();
    cluster.initialize_two().await.unwrap();
    let native = cluster
        .machine_shell(
            0,
            "docker version --format '{{.Server.Os}}/{{.Server.Arch}}'",
        )
        .unwrap();
    let native = native.trim();
    let native_arch = match native {
        "linux/amd64" => "x64",
        "linux/arm64" => "arm64",
        platform => panic!("Railpack does not build for {platform}"),
    };
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fs::write(root.join("package.json"), r#"{"name":"mixed","version":"1.0.0","engines":{"node":"22.16.0"},"scripts":{"start":"node index.js"}}"#).unwrap();
    fs::write(
        root.join("index.js"),
        "console.log(process.arch); setInterval(() => {}, 1000);",
    )
    .unwrap();
    // No platform is authored; both Machines may run the Service, so
    // preparation derives the platform they run and delivers that variant.
    let mut deployment = super::git_deployment("railpack", "unused");
    deployment
        .pointer_mut("/snapshots/0/config")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .insert("replicas".into(), serde_json::json!(2));
    let session = super::session(&cluster).await;
    let name: ployz_core::ServiceName = "app".parse().unwrap();
    let running = session
        .prepare(ployz::sdk::PreparationInput {
            deployment,
            sources: BTreeMap::from([(name, root.to_owned())]),
            source_commits: BTreeMap::new(),
            build_receipts: BTreeMap::new(),
            build_index: 0,
            preferred_machine: None,
        })
        .unwrap();
    let mut platforms = None;
    while let Some(event) = running.next().await {
        if let Some(derived) = event.get("Platforms") {
            platforms = Some(derived.clone());
        }
    }
    assert_eq!(platforms, Some(serde_json::json!([native])));
    let prepared = tokio::time::timeout(Duration::from_secs(600), running.finished())
        .await
        .unwrap()
        .unwrap();
    let outcome = prepared.confirm().unwrap().finished().await.unwrap();
    assert!(
        matches!(outcome, ployz_core::DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut running_containers = 0;
    for index in 0..2 {
        let containers = cluster
            .machine_shell(index, "docker ps -q --filter label=ployz.service.name=app")
            .unwrap();
        for container in containers.lines() {
            running_containers += 1;
            let logs = cluster
                .machine_shell(index, &format!("docker logs {container}"))
                .unwrap();
            // npm echoes the start script before the process prints its architecture.
            assert_eq!(logs.lines().last(), Some(native_arch), "Machine {index}");
            let spec_image = cluster
                .machine_shell(
                    index,
                    &format!("docker inspect {container} --format '{{{{.Config.Image}}}}'"),
                )
                .unwrap();
            assert!(spec_image.contains("@sha256:"), "{spec_image}");
        }
    }
    assert_eq!(running_containers, 2);
    session.close().await;
}
