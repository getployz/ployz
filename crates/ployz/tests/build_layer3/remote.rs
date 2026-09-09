//! Informing evidence for the product remote path. Uses real BuildKit and
//! executes the resulting image on the explicitly selected Machine.

use ployz_build::{
    Output, Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::{MachineId, MachineRpcClient, MachineTarget, OpaquePayload, apply_one_target};
use ployz_testkit::{Cluster, ClusterPlan};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
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
    let root = std::env::temp_dir().join(format!("ployz-build-802-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let image = format!("ployz-remote-802-{}:built", std::process::id());
    fs::write(
        root.join("compose.yaml"),
        format!("name: remote\nservices:\n  app:\n    image: {image}\n    build: .\n"),
    )
    .unwrap();
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"cat\", \"/payload\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "remote-image-ran\n").unwrap();
    fs::write(root.join(".dockerignore"), "docker\n").unwrap();
    fs::write(
        root.join("docker"),
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 99\n",
            root.join("local-docker-called").display()
        ),
    )
    .unwrap();
    fs::set_permissions(root.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .env("DOCKER_HOST", "unix:///no-local-docker.sock")
            .args([
                "--connect",
                &address,
                "build",
                &format!("--remote={selected}"),
                "app",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("local-docker-called").exists());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(selected.as_str()), "{stdout}");
    let exact = stdout
        .split_whitespace()
        .find(|word| word.contains("@sha256:"))
        .expect("remote exact image identity");
    assert_eq!(
        cluster
            .machine_shell(1, &format!("docker run --rm {exact}"))
            .unwrap(),
        "remote-image-ran\n"
    );
    assert!(
        cluster
            .machine_shell(0, &format!("docker image inspect {image}"))
            .is_err(),
        "standalone Build transferred its image to the entry Machine"
    );

    let (first, mut response) = request(&address, selected).await;
    assert!(
        matches!(event(&mut response).await, Event::Admitted { machine_id } if machine_id == selected)
    );
    let (_competitor, mut rejected) = request(&address, selected).await;
    assert!(
        matches!(terminal(&mut rejected).await, Outcome::Failed { stage: Stage::Admission, message, .. } if message.contains("busy"))
    );
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

    let client = ployz::connect::connect(
        Path::new("/missing-ployz-test-config"),
        Some(&address),
        None,
    )
    .await
    .unwrap();
    for uncertain in [false, true] {
        fs::write(
            root.join("Dockerfile"),
            "FROM alpine:3.23.3\nRUN echo REMOTE_BUILD_ACTIVE; sleep 300\n",
        )
        .unwrap();
        if uncertain {
            // A fault injector on this disposable Machine refuses cleanup;
            // preparation and the active build still use actual Docker.
            cluster.machine_shell(1, "mv /usr/local/bin/docker /usr/local/bin/docker-real; printf '%s\n' '#!/bin/sh' 'if [ \"$1 $2\" = \"buildx rm\" ] && [ -f /tmp/refuse-build-cleanup ]; then exit 1; fi' 'exec /usr/local/bin/docker-real \"$@\"' > /usr/local/bin/docker; chmod +x /usr/local/bin/docker").unwrap();
        }
        let load = ployz::compose::LoadOptions {
            command: "build".into(),
            working_dir: Some(root.clone()),
            ..Default::default()
        };
        let mut project = ployz::compose::load_project(&load).unwrap();
        let options = ployz::compose::BuildOptions {
            no_cache: true,
            ..Default::default()
        };
        let plan = ployz::compose::plan_build(&project, &options).unwrap();
        let capture = ployz::compose::capture_build(&plan, &options, &mut project).unwrap();
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            capture.execute_remote(&client, selected, cancellation, |event| {
                if let Progress::Output(bytes) = event
                    && bytes
                        .windows(b"REMOTE_BUILD_ACTIVE".len())
                        .any(|part| part == b"REMOTE_BUILD_ACTIVE")
                    && !cancel.is_cancelled()
                {
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
        if uncertain {
            assert!(matches!(result, Outcome::Unknown { .. }), "{result:?}");
            let (_attempt, mut blocked) = request(&address, selected).await;
            assert!(matches!(
                terminal(&mut blocked).await,
                Outcome::Unknown {
                    stage: Stage::Admission,
                    ..
                }
            ));
        } else {
            assert!(matches!(result, Outcome::Failed { .. }), "{result:?}");
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
    fs::remove_dir_all(root).unwrap();
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
                targets: vec![ployz_build::Target {
                    name: "app".into(),
                    platform: None,
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
