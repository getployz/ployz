//! Rung 2: Cloud's preparation through the real SDK against owned Machine
//! transport, with sentinels for every application mutation. Docker execution
//! is proved at rung 4.
use ployz_build::{Stage, TargetEvidence, WorkEvidence, remote::Outcome};
use ployz_core::{MachineImages, MembershipObservation, RpcError, RpcErrorCode, ServiceName};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

#[path = "../../tests/deploy_client/support.rs"]
#[allow(dead_code)]
mod support;
use support::*;

fn fixture() -> (PathBuf, DeployService, Arc<BuildFixture>) {
    let root = std::env::temp_dir().join(format!("ployz-prepare-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut source = machine('a', "builder");
    let mut destination = machine('b', "application");
    destination.machine.accepts_builds = false;
    source.machine.accepts_services = false;
    source.machine.runtime.architecture = "x86_64".into();
    destination.machine.runtime.architecture = "x86_64".into();
    let builds = Arc::new(BuildFixture::default());
    let mut service = DeployService::new(source.clone())
        .with_machines(vec![source, destination])
        .with_exec_exit(0);
    service.builds = Some(builds.clone());
    (root, service, builds)
}

/// One Git-sourced Cloud snapshot.
fn git(name: &str, builder: &str) -> Value {
    json!({"config": {
        "version": 2, "privateDns": name, "healthcheck": {"type": "none"},
        "restartPolicy": "on-failure",
        "source": {"version": 2, "type": "git", "repository": format!("acme/{name}"),
            "repositoryId": 42, "access": {"type": "public"}, "rootDir": "/",
            "branch": {"type": "connected", "name": "main"}},
        "build": {"builder": builder, "dockerfilePath": "Dockerfile", "command": null}
    }})
}

fn input(root: &Path, snapshots: Vec<Value>) -> crate::sdk::PreparationInput {
    let names = snapshots
        .iter()
        .map(|snapshot| {
            ServiceName::parse(
                snapshot
                    .pointer("/config/privateDns")
                    .and_then(Value::as_str)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    crate::sdk::PreparationInput {
        deployment: json!({"projectName": "example", "snapshots": snapshots}),
        sources: names
            .iter()
            .map(|name| (name.clone(), root.to_owned()))
            .collect(),
        source_commits: names
            .into_iter()
            .map(|name| (name, "a".repeat(40)))
            .collect(),
        build_receipts: BTreeMap::new(),
    }
}

async fn session(
    service: DeployService,
) -> (
    crate::sdk::Session,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let (address, server) = listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    (session, server)
}

/// Prepare and confirm, as Cloud does.
async fn deploy(
    service: DeployService,
    input: crate::sdk::PreparationInput,
) -> Result<(), RpcError> {
    let (session, server) = session(service).await;
    let prepared = session.prepare(input)?.finished().await?;
    let outcome = prepared.confirm()?.finished().await?;
    assert!(
        matches!(outcome, ployz_core::DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    session.close().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn automatic_image_cleanup_reports_last_and_manual_cleanup_stays_silent() {
    for cleanup in [
        crate::sdk::ImageCleanup::Auto,
        crate::sdk::ImageCleanup::Manual,
    ] {
        let (root, service, _) = fixture();
        let destination = machine('b', "application").machine.id;
        let (session, server) = session(service).await;
        let prepared = session
            .prepare(input(&root, vec![git("one", "dockerfile")]))
            .unwrap()
            .finished()
            .await
            .unwrap();
        assert_eq!(
            prepared.prune_targets(),
            [ployz_core::PruneTarget {
                machine_id: destination,
                repository: "ployz-build/one".into(),
            }]
        );
        let running = prepared.confirm_with_log_id(None, cleanup).unwrap();
        let mut events = Vec::new();
        while let Some(event) = running.next().await {
            events.push(event);
        }
        running.finished().await.unwrap();
        let last = events.pop().unwrap();
        match cleanup {
            crate::sdk::ImageCleanup::Auto => {
                assert!(matches!(
                    events.last(),
                    Some(ployz_core::DeployEvent::Outcome { .. })
                ));
                assert_eq!(
                    last,
                    ployz_core::DeployEvent::ImagesPruned {
                        report: ployz_core::ImageCleanupReport {
                            machines: vec![ployz_core::MachineImageCleanup {
                                machine_id: destination,
                                result: ployz_core::MachineCleanupResult::Cleaned {
                                    removals: Vec::new()
                                },
                            }],
                        },
                    }
                );
            }
            crate::sdk::ImageCleanup::Manual => {
                assert!(matches!(last, ployz_core::DeployEvent::Outcome { .. }));
                let report = session
                    .prune_images(prepared.prune_targets())
                    .await
                    .unwrap();
                assert_eq!(report.machines.len(), 1);
            }
        }
        session.close().await;
        assert_eq!(
            session
                .prune_images(prepared.prune_targets())
                .await
                .unwrap_err()
                .code,
            RpcErrorCode::Unavailable
        );
        server.abort();
    }
}

#[test]
fn image_cleanup_mode_rejects_unknown_spellings() {
    assert_eq!(
        "manual".parse::<crate::sdk::ImageCleanup>().unwrap(),
        crate::sdk::ImageCleanup::Manual
    );
    assert_eq!(
        "later"
            .parse::<crate::sdk::ImageCleanup>()
            .unwrap_err()
            .code,
        RpcErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn automatic_preparation_builds_every_service_on_one_build_machine() {
    let (root, service, builds) = fixture();
    let created = service.created_specs();
    deploy(
        service,
        input(
            &root,
            vec![git("one", "dockerfile"), git("two", "dockerfile")],
        ),
    )
    .await
    .unwrap();
    let definitions = builds.definitions.lock().unwrap();
    assert_eq!(
        definitions
            .iter()
            .flat_map(|definition| &definition.targets)
            .map(|target| target.name.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    let created = created.lock().unwrap();
    assert!(!created.is_empty());
    for spec in created.iter() {
        let digest = if spec.name.as_str() == "one" {
            "1"
        } else {
            "2"
        }
        .repeat(64);
        assert!(
            spec.container.image.ends_with(&format!("@sha256:{digest}")),
            "{}",
            spec.container.image
        );
        assert_eq!(spec.container.pull_policy, ployz_core::PullPolicy::Never);
    }
    let pulls = builds.pulls.lock().unwrap();
    assert_eq!(pulls.len(), 2);
    assert!(pulls.iter().all(
        |(target, pull)| *target == machine('b', "application").machine.id
            && pull.pull.image().contains("@sha256:")
            && pull.source.management_address
                == machine('a', "builder").machine.management_address()
    ));
    assert!(
        builds
            .opened
            .lock()
            .unwrap()
            .iter()
            .all(|id| *id == machine('a', "builder").machine.id)
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_unknown_and_cancelled_builds_leave_the_deploy_unattempted() {
    for (failure, kind) in [
        ("failed", "failed"),
        ("cancelled", "failed"),
        ("unknown", "unknown"),
    ] {
        let (root, service, builds) = fixture();
        let mutations = service.mutating_rpcs();
        let work = WorkEvidence(BTreeMap::from([
            ("one".into(), TargetEvidence::Unknown),
            ("two".into(), TargetEvidence::Unattempted),
        ]));
        *builds.terminal.lock().unwrap() = Some(if failure == "unknown" {
            Outcome::Unknown {
                stage: Stage::Building,
                message: "termination unconfirmed".into(),
                work,
            }
        } else {
            Outcome::Failed {
                stage: Stage::Building,
                message: format!("Build {failure}"),
                work,
            }
        });
        let error = deploy(service, input(&root, vec![git("one", "dockerfile")]))
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .contains("No Service, hook, or volume change was attempted"),
            "{error:?}"
        );
        assert_eq!(error.details.pointer("/preparation/kind").unwrap(), kind);
        assert_eq!(
            error.details.pointer("/preparation/stage").unwrap(),
            "Building"
        );
        assert_eq!(
            error.details.pointer("/preparation/work/one").unwrap(),
            "Unknown"
        );
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        assert!(builds.pulls.lock().unwrap().is_empty());
        assert!(builds.opened.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn incompatible_application_platform_refuses_before_transfer_or_mutations() {
    for (architecture, platform) in [("aarch64", "linux/amd64"), ("armv6l", "linux/arm/v7")] {
        let (root, service, builds) = fixture();
        let mutations = service.mutating_rpcs();
        *builds.platforms.lock().unwrap() = Some(vec![platform.into()]);
        let mut destination = machine('b', "application");
        destination.machine.accepts_builds = false;
        destination.machine.runtime.architecture = architecture.into();
        let mut builder = machine('a', "builder");
        builder.machine.accepts_services = false;
        let service = service.with_machines(vec![builder, destination]);
        let error = deploy(service, input(&root, vec![git("one", "dockerfile")]))
            .await
            .unwrap_err()
            .message;
        assert!(
            error.contains(platform) && error.contains(architecture),
            "{error}"
        );
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        assert!(builds.pulls.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn remote_deploy_accepts_matching_non_primary_architectures() {
    for (architecture, platform) in [
        ("x86", "linux/386"),
        ("arm", "linux/arm/v7"),
        ("powerpc", "linux/ppc"),
        ("powerpc64", "linux/ppc64"),
        ("ppc64le", "linux/ppc64le"),
        ("s390x", "linux/s390x"),
        ("riscv64", "linux/riscv64"),
        ("mips64el", "linux/mips64le"),
        ("loongarch64", "linux/loong64"),
    ] {
        let (root, service, builds) = fixture();
        let created = service.created_specs();
        *builds.platforms.lock().unwrap() = Some(vec![platform.into()]);
        let mut builder = machine('a', "builder");
        builder.machine.accepts_services = false;
        let mut destination = machine('b', "application");
        destination.machine.accepts_builds = false;
        destination.machine.runtime.architecture = architecture.into();
        deploy(
            service.with_machines(vec![builder, destination]),
            input(&root, vec![git("one", "dockerfile")]),
        )
        .await
        .unwrap_or_else(|error| panic!("{architecture}/{platform}: {error:?}"));
        assert!(!created.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn preparation_derives_railpack_platforms_from_the_machines_a_service_may_run_on() {
    let mut builder = machine('a', "builder");
    builder.machine.runtime.architecture = "x86_64".into();
    let mut arm = machine('b', "application");
    arm.machine.accepts_builds = false;
    arm.machine.runtime.architecture = "aarch64".into();
    for (builder_runs_services, expected) in [
        (false, vec!["linux/arm64"]),
        (true, vec!["linux/amd64", "linux/arm64"]),
    ] {
        let (root, service, builds) = fixture();
        let created = service.created_specs();
        let expected = expected
            .iter()
            .map(|platform| (*platform).to_owned())
            .collect::<Vec<_>>();
        *builds.platforms.lock().unwrap() = Some(expected.clone());
        builds
            .workers
            .lock()
            .unwrap()
            .insert(builder.machine.id, expected.clone());
        fs::remove_file(root.join("Dockerfile")).unwrap();
        builder.machine.accepts_services = builder_runs_services;
        deploy(
            service.with_machines(vec![builder.clone(), arm.clone()]),
            input(&root, vec![git("one", "railpack")]),
        )
        .await
        .unwrap();
        let definitions = builds.definitions.lock().unwrap();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions
                .first()
                .unwrap()
                .targets
                .first()
                .unwrap()
                .platforms,
            expected
        );
        assert!(!created.lock().unwrap().is_empty());
        drop(definitions);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn remote_transfer_keeps_exact_source_successes_failures_and_omissions() {
    let (root, service, builds) = fixture();
    let native = |hex, name| {
        let mut machine = machine(hex, name);
        machine.machine.runtime.architecture = "x86_64".into();
        machine
    };
    let source = native('a', "builder");
    let failed = native('b', "failed");
    let success = native('c', "success");
    let missing = ployz_core::MachineObservation::new(
        machine('d', "missing").machine,
        MembershipObservation::Down,
    );
    // No built variant runs here, so the source cannot serve it.
    let mut foreign = machine('e', "foreign");
    foreign.machine.runtime.architecture = "riscv64".into();
    let image = ployz_build::BuiltImage {
        reference: format!("sha256:{}", "1".repeat(64)),
        tags: vec![
            "auxiliary.test:5000/other:extra".into(),
            "registry.invalid/shared:latest".into(),
        ],
        platforms: vec!["linux/amd64".into()],
        location: "unix:///var/run/docker.sock".into(),
    };
    builds.stores.lock().unwrap().insert(
        source.machine.id,
        MachineImages {
            containerd_store: true,
            images: vec![ployz_core::ImageSummary {
                id: format!("sha256:{}", "1".repeat(64)),
                // The requested tag now points elsewhere; only content proves identity.
                repo_tags: vec!["registry.invalid/shared:retained".into()],
                created: 0,
                size: 1,
                containers: 0,
                platforms: vec!["linux/amd64".into()],
                last_tagged: None,
            }],
            docker_root: None,
        },
    );
    builds.pull_failures.lock().unwrap().insert(
        failed.machine.id,
        RpcError {
            code: RpcErrorCode::Unsupported,
            message: "containerd image store unavailable".into(),
            details: serde_json::Value::Null,
        },
    );
    let (mut client, server) = connected(service.with_machines(vec![
        source.clone(),
        failed.clone(),
        success.clone(),
        missing.clone(),
        foreign.clone(),
    ]))
    .await;
    let result = crate::image::push_from_machine(
        &mut client,
        &image,
        "registry.invalid/shared:latest",
        source.machine.id,
        &[],
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .successes
            .iter()
            .map(|success| success.machine_id)
            .collect::<Vec<_>>(),
        [source.machine.id, success.machine.id]
    );
    assert_eq!(result.failures.len(), 2);
    assert_eq!(
        result.failures.first().unwrap().machine_id,
        failed.machine.id
    );
    assert!(
        result
            .failures
            .first()
            .unwrap()
            .error
            .to_string()
            .contains("containerd image store")
    );
    let unserved = result.failures.get(1).unwrap();
    assert_eq!(unserved.machine_id, foreign.machine.id);
    assert!(
        matches!(&unserved.error, crate::image::PushError::VariantUnavailable { platform, .. } if platform == "riscv64"),
        "{}",
        unserved.error
    );
    assert_eq!(result.omissions, [missing.machine.id]);
    let pulls = builds.pulls.lock().unwrap().clone();
    assert_eq!(
        pulls.len(),
        3,
        "the unserved Machine was never asked to pull"
    );
    assert!(pulls.iter().all(|(_, pull)| {
        pull.pull.image()
            == image
                .repository_reference("registry.invalid/shared:latest")
                .unwrap()
            && pull.source.management_address == source.machine.management_address()
            && pull.platform == "linux/amd64"
    }));
    builds
        .stores
        .lock()
        .unwrap()
        .get_mut(&source.machine.id)
        .unwrap()
        .containerd_store = false;
    assert!(matches!(
        crate::image::push_from_machine(
            &mut client,
            &image,
            "registry.invalid/shared:latest",
            source.machine.id,
            &[success.machine.id.to_string()],
            &tokio_util::sync::CancellationToken::new(),
        )
        .await,
        Err(crate::image::PushError::UnsupportedImageStore)
    ));
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_partial_source_is_refused_and_each_destination_names_its_variant() {
    let (root, service, builds) = fixture();
    let observed = |hex, name, architecture: &str| {
        let mut machine = machine(hex, name);
        machine.machine.runtime.architecture = architecture.into();
        machine
    };
    let source = observed('a', "builder", "x86_64");
    let amd64 = observed('b', "amd64", "x86_64");
    let arm64 = observed('c', "arm64", "aarch64");
    let image = ployz_build::BuiltImage {
        reference: format!("sha256:{}", "3".repeat(64)),
        tags: vec!["registry.invalid/shared:latest".into()],
        platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        location: "unix:///var/run/docker.sock".into(),
    };
    let store = |platforms: &[&str]| MachineImages {
        containerd_store: true,
        images: vec![ployz_core::ImageSummary {
            id: image.reference.clone(),
            // The requested tag now points elsewhere; only content proves identity.
            repo_tags: vec!["registry.invalid/shared:retained".into()],
            created: 0,
            size: 1,
            containers: 0,
            platforms: platforms
                .iter()
                .map(|platform| (*platform).to_owned())
                .collect(),
            last_tagged: None,
        }],
        docker_root: None,
    };
    // The prototype's partial peer: the index is listed, ARM64 data is absent.
    builds
        .stores
        .lock()
        .unwrap()
        .insert(source.machine.id, store(&["linux/amd64"]));
    let (mut client, server) =
        connected(service.with_machines(vec![source.clone(), amd64.clone(), arm64.clone()])).await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let error = crate::image::push_from_machine(
        &mut client,
        &image,
        "registry.invalid/shared:latest",
        source.machine.id,
        &[],
        &cancellation,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, crate::image::PushError::BuildIncomplete { missing, .. } if missing == &["linux/arm64"]),
        "{error}"
    );
    assert!(builds.pulls.lock().unwrap().is_empty());

    builds
        .stores
        .lock()
        .unwrap()
        .insert(source.machine.id, store(&["linux/amd64", "linux/arm64/v8"]));
    let result = crate::image::push_from_machine(
        &mut client,
        &image,
        "registry.invalid/shared:latest",
        source.machine.id,
        &[amd64.machine.id.to_string(), arm64.machine.id.to_string()],
        &cancellation,
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty() && result.omissions.is_empty());
    let pulls = builds.pulls.lock().unwrap();
    let platform = |id| {
        pulls
            .iter()
            .find(|(machine, _)| *machine == id)
            .unwrap()
            .1
            .platform
            .clone()
    };
    assert_eq!(platform(amd64.machine.id), "linux/amd64");
    assert_eq!(platform(arm64.machine.id), "linux/arm64/v8");
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn deploy_pulls_only_from_a_peer_that_holds_the_destinations_variant() {
    let (root, service, builds) = fixture();
    let observed = |hex, name, architecture: &str| {
        let mut machine = machine(hex, name);
        machine.machine.runtime.architecture = architecture.into();
        machine
    };
    let destination = observed('a', "destination", "x86_64");
    let partial = observed('b', "partial", "aarch64");
    let complete = observed('c', "complete", "x86_64");
    let image = "docker.io/library/busybox:1.37.0";
    let store = |platforms: &[&str]| MachineImages {
        containerd_store: true,
        images: vec![ployz_core::ImageSummary {
            id: format!("sha256:{}", "4".repeat(64)),
            repo_tags: vec![image.into()],
            created: 0,
            size: 1,
            containers: 0,
            platforms: platforms
                .iter()
                .map(|platform| (*platform).to_owned())
                .collect(),
            last_tagged: None,
        }],
        docker_root: None,
    };
    // The tag is visible on both peers; only one holds the AMD64 content.
    builds
        .stores
        .lock()
        .unwrap()
        .insert(partial.machine.id, store(&["linux/arm64"]));
    let (client, server) = connected(service.with_machines(vec![
        destination.clone(),
        partial.clone(),
        complete.clone(),
    ]))
    .await;
    crate::image::ensure_cluster_image(
        &client,
        &destination.machine.id,
        image,
        ployz_core::PullPolicy::Never,
    )
    .await
    .unwrap();
    assert!(
        builds.pulls.lock().unwrap().is_empty(),
        "a tag without the destination's variant is not a source"
    );

    builds
        .stores
        .lock()
        .unwrap()
        .insert(complete.machine.id, store(&["linux/amd64", "linux/arm64"]));
    crate::image::ensure_cluster_image(
        &client,
        &destination.machine.id,
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap();
    let pulls = builds.pulls.lock().unwrap().clone();
    assert_eq!(pulls.len(), 1);
    let (target, pull) = pulls.first().unwrap();
    assert_eq!(*target, destination.machine.id);
    assert_eq!(pull.pull.image(), image);
    assert_eq!(pull.platform, "linux/amd64");
    assert_eq!(
        pull.source.management_address,
        complete.machine.management_address()
    );
    assert_eq!(
        builds.opened.lock().unwrap().as_slice(),
        [complete.machine.id]
    );

    // The destination now holds it, so a second Deploy step pulls nothing.
    crate::image::ensure_cluster_image(
        &client,
        &destination.machine.id,
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap();
    assert_eq!(builds.pulls.lock().unwrap().len(), 1);
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn builder_selection_filters_observations_without_using_service_policy_or_cpu_architecture() {
    let (root, service, builds) = fixture();
    let mut builder = machine('a', "builder");
    builder.machine.accepts_services = false;
    builder.machine.accepts_ingress = false;
    builder.machine.runtime.architecture = "unreported".into();
    let mut disabled = machine('b', "disabled");
    disabled.machine.accepts_builds = false;
    let mut down = machine('c', "down");
    down.membership = MembershipObservation::Down;
    let (mut client, server) =
        connected(service.with_machines(vec![builder.clone(), disabled, down])).await;
    let visible = client.machines().await.unwrap();
    assert_eq!(
        crate::sdk::prepare::select_build_machine(
            &mut client,
            &build_targets(&["linux/amd64"]),
            &visible,
            &Default::default()
        )
        .await
        .unwrap()
        .machine
        .id,
        builder.machine.id
    );
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn builder_selection_reports_missing_capability() {
    let (root, mut service, builds) = fixture();
    service.builds = None;
    let mut disabled = machine('b', "builder");
    disabled.machine.accepts_builds = false;
    let mut down = machine('c', "down");
    down.membership = MembershipObservation::Down;
    let (mut client, server) =
        connected(service.with_machines(vec![machine('a', "builder"), disabled, down])).await;
    let visible = client.machines().await.unwrap();
    let error = crate::sdk::prepare::select_build_machine(
        &mut client,
        &build_targets(&["linux/amd64"]),
        &visible,
        &Default::default(),
    )
    .await
    .err()
    .unwrap()
    .to_string();
    for reason in [
        "does not support remote Builds",
        "does not accept Builds",
        "Down",
    ] {
        assert!(error.contains(reason), "{error}");
    }
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn builder_selection_requires_one_worker_for_every_command_target_before_upload() {
    let (root, service, builds) = fixture();
    let amd = machine('a', "amd");
    let arm = machine('b', "arm");
    *builds.workers.lock().unwrap() = BTreeMap::from([
        (amd.machine.id, vec!["linux/amd64".into()]),
        (arm.machine.id, vec!["linux/arm64".into()]),
    ]);
    let (mut client, server) =
        connected(service.with_machines(vec![amd.clone(), arm.clone()])).await;
    let visible = client.machines().await.unwrap();
    let selected = crate::sdk::prepare::select_build_machine(
        &mut client,
        &build_targets(&["linux/arm64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(selected.machine.id, arm.machine.id);
    let targets = build_targets(&["linux/amd64", "linux/arm64"]);
    let error = crate::sdk::prepare::select_build_machine(
        &mut client,
        &targets,
        &visible,
        &Default::default(),
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("cannot build linux/arm64"), "{error}");
    assert!(error.contains("cannot build linux/amd64"), "{error}");
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

fn build_targets(platforms: &[&str]) -> Vec<ployz_build::Target> {
    platforms
        .iter()
        .enumerate()
        .map(|(i, platform)| ployz_build::Target {
            name: format!("service{i}"),
            platforms: vec![(*platform).into()],
        })
        .collect()
}

#[tokio::test]
async fn sdk_reuses_unchanged_git_image_when_another_service_changes() {
    let (root, service, builds) = fixture();
    let (address, server) = listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let mut deployment = json!({
        "projectName": "example", "snapshots": [
            {"config": {"version": 2, "privateDns": "one", "source": {
                "version": 2, "type": "git", "repository": "acme/one", "repositoryId": 42,
                "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "connected", "name": "main"}
            }, "build": {"builder": "dockerfile", "dockerfilePath": "Dockerfile", "command": null},
            "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"}},
            {"config": {"version": 2, "privateDns": "other", "source": {
                "version": 1, "type": "image", "image": "redis:7", "credentials": {"type": "none"}
            }, "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"}}
        ]
    });
    let name: ployz_core::ServiceName = "one".parse().unwrap();
    let input = |deployment, receipts, commit| crate::sdk::PreparationInput {
        deployment,
        sources: BTreeMap::from([(name.clone(), root.clone())]),
        source_commits: BTreeMap::from([(name.clone(), commit)]),
        build_receipts: receipts,
    };
    let first = session
        .prepare(input(deployment.clone(), BTreeMap::new(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    let receipts = first.build_receipts().clone();
    assert_eq!(receipts.len(), 1);
    let image = &receipts.get(&name).unwrap().image.reference;
    first.close();
    assert_eq!(builds.definitions.lock().unwrap().len(), 1);
    // The original builder may lose the image; a runtime peer can still serve it.
    builds
        .stores
        .lock()
        .unwrap()
        .remove(&machine('a', "builder").machine.id);
    *deployment
        .pointer_mut("/snapshots/1/config/source/image")
        .unwrap() = json!("redis:8");
    let second = session
        .prepare(input(deployment.clone(), receipts.clone(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(
        builds.definitions.lock().unwrap().len(),
        1,
        "unchanged Git source must not start another Build"
    );
    assert_eq!(
        &second.build_receipts().get(&name).unwrap().image.reference,
        image
    );
    assert!(second.preview().operations.iter().any(|row| {
        row.operation
            .spec()
            .is_some_and(|spec| spec.name.as_str() == "other" && spec.container.image == "redis:8")
    }));
    assert_eq!(
        second.build_receipts().get(&name).unwrap().machine_id,
        machine('b', "application").machine.id
    );
    second.close();

    // A missing image must fall back to a Build, even with matching inputs.
    builds.stores.lock().unwrap().clear();
    let missing = session
        .prepare(input(deployment.clone(), receipts.clone(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 2);
    missing.close();
    let changed = session
        .prepare(input(deployment.clone(), receipts.clone(), "b".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 3);
    changed.close();
    deployment
        .pointer_mut("/snapshots/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("resolvedEnv".into(), json!({"BUILD_VALUE": "changed"}));
    let changed = session
        .prepare(input(deployment.clone(), receipts, "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 4);
    let mut uncovered = changed.build_receipts().clone();
    uncovered.get_mut(&name).unwrap().image.platforms = vec!["linux/arm64".into()];
    changed.close();
    let uncovered = session
        .prepare(input(deployment.clone(), uncovered, "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(
        builds.definitions.lock().unwrap().len(),
        5,
        "the reused image must cover runtime placement even without an explicit Dockerfile platform"
    );

    // A mixed preparation must build only the new Git Service and preserve the reused one.
    let mut two = deployment.pointer("/snapshots/0").unwrap().clone();
    *two.pointer_mut("/config/privateDns").unwrap() = json!("two");
    deployment
        .get_mut("snapshots")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(two);
    let mut mixed = input(
        deployment,
        uncovered.build_receipts().clone(),
        "a".repeat(40),
    );
    uncovered.close();
    mixed.sources.insert("two".parse().unwrap(), root.clone());
    mixed
        .source_commits
        .insert("two".parse().unwrap(), "a".repeat(40));
    let mixed = session.prepare(mixed).unwrap().finished().await.unwrap();
    assert_eq!(mixed.build_receipts().len(), 2);
    {
        let definitions = builds.definitions.lock().unwrap();
        assert_eq!(definitions.len(), 6);
        assert_eq!(
            definitions.last().unwrap().targets.first().unwrap().name,
            "two"
        );
    }
    mixed.close();
    session.close().await;
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
