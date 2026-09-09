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
        .find(|word| word.starts_with("sha256:"))
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
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    let selected = cluster.initialize_first().await.unwrap().id;
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
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
    let root = std::env::temp_dir().join(format!("ployz-build-807-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("compose.yaml"),
        "name: restart\nservices:\n  app:\n    image: ployz-restart:built\n    build: .\n",
    )
    .unwrap();
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"cat\", \"/payload\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "safe-after-restart\n").unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let exact = stdout
        .split_whitespace()
        .find(|word| word.starts_with("sha256:"))
        .unwrap();
    assert_eq!(
        cluster
            .machine_shell(0, &format!("docker run --rm {exact}"))
            .unwrap(),
        "safe-after-restart\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn remote_build_delivers_dependency_content_and_deploys_without_a_registry() {
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-build-803-{}", std::process::id()), 2).unwrap(),
    )
    .unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let selected = machines.get(1).unwrap().id;
    let destination = machines.first().unwrap().id;
    let address = cluster.api_address(0).unwrap();
    let root = std::env::temp_dir().join(format!("ployz-build-803-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("base")).unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    let image = format!("registry.invalid/ployz-803-{}:shared", std::process::id());
    fs::write(root.join("compose.yaml"), format!(
        "name: remote\nservices:\n  base:\n    image: {image}\n    build: ./base\n    profiles: [build-only]\n  app:\n    image: {image}\n    pull_policy: never\n    deploy: {{placement: {{constraints: [node.id=={destination}]}}}}\n    build:\n      context: ./app\n      additional_contexts:\n        base: service:base\n"
    )).unwrap();
    fs::write(
        root.join("base/Dockerfile"),
        "FROM alpine:3.23.3\nRUN echo dependency-output > /dependency\n",
    )
    .unwrap();
    fs::write(
        root.join("app/Dockerfile"),
        "FROM base\nCOPY payload /payload\nCMD [\"sleep\", \"3600\"]\n",
    )
    .unwrap();
    fs::write(root.join("app/payload"), "remote-application-ran\n").unwrap();
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
        Duration::from_secs(240),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .env("DOCKER_HOST", "unix:///no-local-docker.sock")
            .args([
                "--connect",
                &address,
                "deploy",
                &format!("--remote={selected}"),
                "--yes",
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
    assert_temporary_tags_released(&cluster).await;
    let container = cluster
        .machine_shell(0, "docker ps -q --filter label=ployz.service.name=app")
        .unwrap();
    let container = container.trim();
    assert!(!container.is_empty(), "application was not running");
    assert_eq!(
        cluster
            .machine_shell(
                0,
                &format!("docker exec {container} cat /dependency /payload")
            )
            .unwrap(),
        "dependency-output\nremote-application-ran\n"
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
    // The same captured graph also works when each Build runs on a different Machine.
    let load = ployz::compose::LoadOptions {
        command: "build".into(),
        all_profiles: true,
        working_dir: Some(root.clone()),
        ..Default::default()
    };
    let mut project = ployz::compose::load_project(&load).unwrap();
    let options = ployz::compose::BuildOptions {
        services: vec!["app".into()],
        ..Default::default()
    };
    let plan = ployz::compose::plan_build(&project, &options).unwrap();
    let captured = ployz::compose::capture_build(&plan, &options, &mut project).unwrap();
    let mut client = ployz::connect::connect(&root.join("config.yaml"), Some(&address), None)
        .await
        .unwrap();
    let images = captured
        .execute_on_machines(
            &client,
            &std::collections::BTreeMap::from([
                ("base".into(), selected),
                ("app".into(), destination),
            ]),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let app = images.iter().find(|image| image.name == "app").unwrap();
    let result = ployz::image::push_from_machine(
        &mut client,
        &app.built,
        &app.image,
        destination,
        &[selected.to_string()],
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.successes.len(), 1);
    assert!(result.failures.is_empty() && result.omissions.is_empty());
    assert_eq!(
        cluster
            .machine_shell(
                1,
                &format!(
                    "docker run --rm --pull=never {} cat /dependency /payload",
                    app.built.reference,
                )
            )
            .unwrap(),
        "dependency-output\nremote-application-ran\n"
    );
    drop(images);
    assert_temporary_tags_released(&cluster).await;
    fs::remove_dir_all(root).unwrap();
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

/// Two compatible candidates: automatic selection, an explicit pin, and the
/// local override.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn build_location_selects_automatically_honours_a_pin_and_yields_to_local() {
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-build-804-{}", std::process::id()), 2).unwrap(),
    )
    .unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let first = machines.first().unwrap().id;
    let second = machines.get(1).unwrap().id;
    let address = cluster.api_address(0).unwrap();
    let root = std::env::temp_dir().join(format!("ployz-build-804-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let image = format!("registry.invalid/ployz-804-{}:built", std::process::id());
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"sleep\", \"3600\"]\n",
    )
    .unwrap();
    fs::write(root.join("payload"), "selected-machine-ran\n").unwrap();
    fs::write(root.join(".dockerignore"), "docker\nconfig.yaml\n").unwrap();
    fs::write(
        root.join("docker"),
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 99\n",
            root.join("local-docker-called").display()
        ),
    )
    .unwrap();
    fs::set_permissions(root.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
    let compose = |preference: &str| {
        fs::write(
            root.join("compose.yaml"),
            format!(
                "name: remote\n{preference}services:\n  app:\n    image: {image}\n    pull_policy: never\n    deploy:\n      placement:\n        constraints: [node.id == {first}]\n    build: .\n"
            ),
        )
        .unwrap();
    };
    let run = |args: Vec<String>| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .env("DOCKER_HOST", "unix:///no-local-docker.sock")
            .args(["--connect", &address])
            .args(args)
            .kill_on_drop(true);
        command
    };
    let succeed = async |args: Vec<String>| {
        let output = tokio::time::timeout(Duration::from_secs(240), run(args).output())
            .await
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    };

    // Plain --remote selects a compatible Machine and leaves the image there.
    compose("");
    let stdout = succeed(vec!["build".into(), "--remote".into(), "app".into()]).await;
    let automatic_index = [first, second]
        .iter()
        .position(|id| stdout.contains(&format!("({id})")))
        .expect("selected Machine is one of the two eligible builders");
    let pinned_index = 1 - automatic_index;
    let pinned = *[first, second].get(pinned_index).unwrap();
    let built = stdout
        .split_whitespace()
        .find(|word| word.starts_with("sha256:"))
        .expect("remote exact image identity")
        .to_owned();
    assert!(
        cluster
            .machine_shell(automatic_index, &format!("docker image inspect {built}"))
            .is_ok()
    );
    assert!(
        cluster
            .machine_shell(pinned_index, &format!("docker image inspect {built}"))
            .is_err(),
        "automatic selection left the image on both Machines"
    );

    // An explicit pin builds on the other Machine and still deploys from it.
    let stdout = succeed(vec![
        "deploy".into(),
        format!("--remote={pinned}"),
        "--yes".into(),
        "app".into(),
    ])
    .await;
    assert!(stdout.contains(pinned.as_str()), "{stdout}");
    let container = cluster
        .machine_shell(0, "docker ps -q --filter label=ployz.service.name=app")
        .unwrap();
    let container = container.trim();
    assert!(!container.is_empty(), "application was not running");
    assert_eq!(
        cluster
            .machine_shell(0, &format!("docker exec {container} cat /payload"))
            .unwrap(),
        "selected-machine-ran\n"
    );

    // --local runs on this host's Docker even with the Cluster reachable, which
    // the fixture stub refuses.
    assert!(!root.join("local-docker-called").exists());
    let output = run(vec!["build".into(), "--local".into(), "app".into()])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(root.join("local-docker-called").exists());
    assert_temporary_tags_released(&cluster).await;
    fs::remove_dir_all(root).unwrap();
}

/// Mixed-architecture delivery through the product path. Both testkit Machines
/// run this host's kernel, so the derived platform is native for both and the
/// second Railpack variant runs EMULATED under binfmt. This is not evidence for
/// native AMD64 plus native ARM64 Machines; that needs an ARM64 Machine and
/// belongs to the qualification path.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx and host Docker with containerd storage and AMD64/ARM64 worker support"]
async fn railpack_deploy_derives_machine_platforms_and_partial_peers_never_serve_missing_variants()
{
    let plan = ClusterPlan::new(&format!("l3-build-806-{}", std::process::id()), 2).unwrap();
    let first_container = plan.machine_name(0);
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let first = machines.first().unwrap().id;
    let second = machines.get(1).unwrap().id;
    let address = cluster.api_address(0).unwrap();
    let root = std::env::temp_dir().join(format!("ployz-build-806-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("app")).unwrap();
    let native = super::host_platform();
    let native = native.as_str();
    let (other, native_arch, other_arch) = match native {
        "linux/amd64" => ("linux/arm64", "x64", "arm64"),
        "linux/arm64" => ("linux/amd64", "arm64", "x64"),
        platform => panic!("Railpack does not build for {platform}"),
    };
    eprintln!("Execution host and both Machines: {native}; {other} below is EMULATED");

    // Part 1: the product path. No build.platforms; the Service may run on
    // both Machines, so Deploy derives the platform they run and every
    // destination receives that variant from the Build Machine.
    let image = format!("registry.invalid/ployz-806-{}:app", std::process::id());
    fs::write(root.join("compose.yaml"), format!(
        "name: mixed\nservices:\n  app:\n    image: {image}\n    pull_policy: never\n    deploy: {{mode: global}}\n    build:\n      context: ./app\n      x-recipe: railpack\n"
    )).unwrap();
    fs::write(root.join("app/package.json"), r#"{"name":"mixed","version":"1.0.0","engines":{"node":"22.16.0"},"scripts":{"start":"node index.js"}}"#).unwrap();
    fs::write(
        root.join("app/index.js"),
        "console.log(process.arch); setInterval(() => {}, 1000);",
    )
    .unwrap();
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
        Duration::from_secs(600),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .env("PATH", &root)
            .env("HOME", &root)
            .env("PLOYZ_CONFIG", root.join("config.yaml"))
            .env("DOCKER_HOST", "unix:///no-local-docker.sock")
            .args(["--connect", &address, "deploy", "--remote", "--yes", "app"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!root.join("local-docker-called").exists());
    assert!(
        stderr.contains(&format!("Build platforms: {native}")),
        "{stderr}"
    );
    for index in 0..2 {
        let container = cluster
            .machine_shell(index, "docker ps -q --filter label=ployz.service.name=app")
            .unwrap();
        let container = container.trim();
        assert!(!container.is_empty(), "application not running on {index}");
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

    // Part 2: the prototype's partial-peer observation as a regression case.
    // A complete two-platform image reaches Machine 0; Machine 1 pulls only
    // the emulated variant, keeps the whole index, and must never serve the
    // native variant it does not hold.
    let multi = format!("railpack.invalid/ployz-806-{}:multi", std::process::id());
    let mut cleanup = super::LocalBuild {
        images: vec![multi.clone()],
    };
    fs::create_dir_all(root.join("multi")).unwrap();
    fs::write(root.join("multi/compose.yaml"), format!("services:\n  app:\n    image: {multi}\n    build:\n      context: .\n      platforms: [linux/amd64, linux/arm64]\n")).unwrap();
    fs::write(root.join("multi/package.json"), r#"{"name":"multi","version":"1.0.0","engines":{"node":"22.16.0"},"scripts":{"start":"node index.js"}}"#).unwrap();
    fs::write(root.join("multi/index.js"), "console.log(process.arch);").unwrap();
    let load = ployz::compose::LoadOptions {
        command: "build".into(),
        working_dir: Some(root.join("multi")),
        ..Default::default()
    };
    let mut project = ployz::compose::load_project(&load).unwrap();
    let options = ployz::compose::BuildOptions::default();
    let plan = ployz::compose::plan_build(&project, &options).unwrap();
    let built = tokio::task::spawn_blocking(move || {
        ployz::compose::execute_build(
            &plan,
            &options,
            &load,
            &mut project,
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap()
    .remove(0);
    cleanup.images.push(built.built.reference.clone());
    assert_eq!(built.built.platforms, ["linux/amd64", "linux/arm64"]);
    let mut client = ployz::connect::connect(&root.join("config.yaml"), Some(&address), None)
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    // A direct push from this host needs a proxied connection the testkit's
    // plain TCP endpoint does not offer, so Machine 0 loads the exact archive
    // Docker would have pushed: the complete index with both variants.
    let archive = root.join("multi.tar");
    super::command([
        "image",
        "save",
        "--output",
        archive.to_str().unwrap(),
        &built.built.reference,
    ]);
    super::command([
        "cp",
        archive.to_str().unwrap(),
        &format!("{first_container}:/multi.tar"),
    ]);
    cluster
        .machine_shell(0, "docker load --input /multi.tar")
        .unwrap();
    let lister = client.clone();
    let listed = |machine: MachineId| {
        let mut client = lister.clone();
        async move {
            let targets = client
                .machines()
                .await
                .unwrap()
                .into_iter()
                .filter(|observed| observed.machine.id == machine)
                .map(|observed| observed.machine)
                .collect::<Vec<_>>();
            client
                .list_images(None, &targets)
                .await
                .successes
                .remove(0)
                .value
                .images
        }
    };
    let holds = |store: &ployz_core::MachineImages, platform: &str| {
        store.images.iter().any(|stored| {
            stored.id == built.built.reference && stored.platforms.iter().any(|p| p == platform)
        })
    };
    let complete = listed(first).await;
    assert!(
        holds(&complete, native) && holds(&complete, other),
        "{complete:?}"
    );
    let opened = client
        .call::<ployz_core::op::EnsureImageIngest>(
            ployz_core::EnsureImageIngestRequest {},
            Some(&MachineTarget::from(&first)),
        )
        .await
        .unwrap();
    client
        .call::<ployz_core::op::PullImageFromMachine>(
            ployz_core::PullImageFromMachineRequest {
                tag: None,
                image: built.built.repository_reference(&built.image).unwrap(),
                source: opened.destination,
                platform: other.into(),
            },
            Some(&MachineTarget::from(&second)),
        )
        .await
        .unwrap();
    let partial = listed(second).await;
    assert!(holds(&partial, other), "{partial:?}");
    assert!(
        !holds(&partial, native),
        "the unselected variant's content must be absent on the peer: {partial:?}"
    );
    // The partial peer is excluded as a source for what it does not hold.
    let refused = ployz::image::push_from_machine(
        &mut client,
        &built.built,
        &built.image,
        second,
        &[first.to_string()],
        &cancellation,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&refused, ployz::image::PushError::BuildIncomplete { missing, .. } if missing == &[native]),
        "{refused}"
    );
    // The complete Build host serves the peer's native variant with its platform named.
    let served = ployz::image::push_from_machine(
        &mut client,
        &built.built,
        &built.image,
        first,
        &[second.to_string()],
        &cancellation,
    )
    .await
    .unwrap();
    assert_eq!(served.successes.len(), 1, "{:?}", served.failures);
    let completed = listed(second).await;
    assert!(
        holds(&completed, native) && holds(&completed, other),
        "{completed:?}"
    );
    for (platform, expected) in [(native, native_arch), (other, other_arch)] {
        let printed = cluster
            .machine_shell(
                1,
                &format!(
                    "docker run --rm --pull=never --platform {platform} {}",
                    built.built.reference
                ),
            )
            .unwrap();
        // npm echoes the start script before the process prints its architecture.
        assert_eq!(printed.lines().last(), Some(expected), "{platform}");
    }
    drop(built);
    assert_temporary_tags_released(&cluster).await;
    drop(cleanup);
    fs::remove_dir_all(root).unwrap();
}
