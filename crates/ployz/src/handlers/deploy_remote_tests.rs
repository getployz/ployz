//! Rung 2: the product handler against owned Machine transport, with sentinels
//! for every application mutation. Docker execution is proved at rung 4.
use ployz_build::{Stage, TargetEvidence, WorkEvidence, remote::Outcome};
use ployz_core::{MachineImages, MembershipObservation, RpcError, RpcErrorCode};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

#[path = "../../tests/deploy_client/support.rs"]
#[allow(dead_code)]
mod support;
use support::*;

#[path = "local_push_tests.rs"]
mod local_push_tests;

fn fixture() -> (PathBuf, DeployService, Arc<BuildFixture>) {
    let root = std::env::temp_dir().join(format!("ployz-deploy-803-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut source = machine('a', "builder");
    let mut destination = machine('b', "application");
    destination.machine.accepts_builds = false;
    source.machine.accepts_services = false;
    source.machine.runtime.architecture = "x86_64".into();
    destination.machine.runtime.architecture = "x86_64".into();
    fs::write(root.join("compose.yaml"), format!(
        "name: example\nservices:\n  one:\n    image: registry.invalid/shared:latest\n    build: .\n    deploy: {{placement: {{constraints: [node.id=={}]}}}}\n    x-pre_deploy: {{command: ['true']}}\n    volumes: [data:/data]\n  two:\n    image: registry.invalid/shared:latest\n    build: .\n    deploy: {{placement: {{constraints: [node.id=={}]}}}}\nvolumes:\n  data: {{}}\n",
        destination.machine.id, destination.machine.id,
    )).unwrap();
    let builds = Arc::new(BuildFixture::default());
    let mut service = DeployService::new(source.clone())
        .with_machines(vec![source, destination])
        .with_exec_exit(0);
    service.builds = Some(builds.clone());
    (root, service, builds)
}

async fn deploy(root: &Path, service: DeployService) -> Result<(), crate::failure::Failure> {
    let (address, server) = listening(service).await;
    let config = root.join("config.yaml");
    let file = root.join("compose.yaml");
    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--connect",
            &format!("tcp://{address}"),
            "--ployz-config",
            config.to_str().unwrap(),
            "deploy",
            "--yes",
            "--skip-health",
            "--file",
            file.to_str().unwrap(),
        ])
        .unwrap();
    let result = tokio::task::spawn_blocking(move || super::deploy(&matches))
        .await
        .unwrap();
    server.abort();
    result
}

#[tokio::test]
async fn automatic_deploy_uses_one_build_only_source_for_all_services() {
    let (root, service, builds) = fixture();
    *builds.platforms.lock().unwrap() = Some(vec!["linux/arm64".into(), "linux/amd64".into()]);
    let created = service.created_specs();
    let yaml = fs::read_to_string(root.join("compose.yaml")).unwrap();
    fs::write(
        root.join("compose.yaml"),
        yaml.replace("    x-pre_deploy: {command: ['true']}\n", "").replace(
            "  two:\n    image: registry.invalid/shared:latest\n    build: .",
            "  two:\n    image: registry.invalid/shared:latest\n    build: {context: ., additional_contexts: {base: 'service:one'}}",
        ),
    )
    .unwrap();
    deploy(&root, service).await.unwrap();
    let definitions = builds.definitions.lock().unwrap();
    assert_eq!(
        definitions
            .iter()
            .flat_map(|definition| &definition.targets)
            .map(|target| target.name.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        definitions
            .get(1)
            .unwrap()
            .image_contexts
            .get("one")
            .unwrap()
            .reference,
        format!("registry.invalid/shared@sha256:{}", "1".repeat(64))
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
        assert_eq!(
            spec.container.image,
            format!("registry.invalid/shared@sha256:{digest}")
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
async fn failed_unknown_and_cancelled_builds_leave_hooks_containers_and_volumes_unattempted() {
    for failure in ["failed", "cancelled", "unknown"] {
        let (root, service, builds) = fixture();
        let mutations = service.mutating_rpcs();
        let work = WorkEvidence(std::collections::BTreeMap::from([
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
        let error = deploy(&root, service).await.unwrap_err().to_string();
        assert!(
            error.contains("No Service, hook, or volume change was attempted"),
            "{error}"
        );
        assert!(error.contains("Unattempted"), "{error}");
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
        let service = service.with_machines(vec![machine('a', "builder"), destination]);
        let error = deploy(&root, service).await.unwrap_err().to_string();
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
            }],
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
async fn service_context_uses_the_completed_dependency_on_a_different_build_machine() {
    let (root, service, builds) = fixture();
    let source = machine('a', "builder").machine.id;
    let consumer = machine('b', "application").machine.id;
    let mut project = crate::compose::parse_normalized(
        "services: {base: {image: 'registry.invalid/shared:latest', build: .}, app: {image: 'registry.invalid/shared:latest', build: {context: ., additional_contexts: {base: 'service:base'}}}}",
        &root,
    ).unwrap();
    let options = crate::compose::BuildOptions {
        services: vec!["app".into()],
        ..Default::default()
    };
    let plan = crate::compose::plan_build(&project, &options).unwrap();
    let captured = crate::compose::capture_build(&plan, &options, &mut project).unwrap();
    // Later edits must not change either Service's admitted recipe or source.
    fs::write(root.join("Dockerfile"), "invalid later edit\n").unwrap();
    let (client, server) = connected(service).await;
    let images = captured
        .execute_on_machines(
            &client,
            &std::collections::BTreeMap::from([("base".into(), source), ("app".into(), consumer)]),
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        images
            .iter()
            .map(|image| image.name.as_str())
            .collect::<Vec<_>>(),
        ["base", "app"]
    );
    assert_eq!(
        images.first().unwrap().location,
        crate::compose::BuildLocation::Machine(source)
    );
    assert_eq!(
        images.get(1).unwrap().location,
        crate::compose::BuildLocation::Machine(consumer)
    );
    let definitions = builds.definitions.lock().unwrap();
    assert_eq!(definitions.len(), 2);
    let dependency = definitions
        .get(1)
        .unwrap()
        .image_contexts
        .get("base")
        .unwrap();
    assert_eq!(
        dependency.reference,
        format!("registry.invalid/shared@sha256:{}", "1".repeat(64))
    );
    assert_eq!(
        dependency.source.management_address,
        machine('a', "builder").machine.management_address()
    );
    assert_eq!(builds.opened.lock().unwrap().as_slice(), [source]);
    assert!(
        builds.pulls.lock().unwrap().is_empty(),
        "BuildKit consumes the source directly"
    );
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn incomplete_build_locations_refuse_before_source_submission() {
    let (root, service, builds) = fixture();
    let mut project =
        crate::compose::parse_normalized("services: {api: {build: .}}", &root).unwrap();
    let options = crate::compose::BuildOptions::default();
    let plan = crate::compose::plan_build(&project, &options).unwrap();
    let captured = crate::compose::capture_build(&plan, &options, &mut project).unwrap();
    let (client, server) = connected(service).await;
    let error = captured
        .execute_on_machines(
            &client,
            &Default::default(),
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("every Build must have a selected Machine"),
        "{error}"
    );
    assert!(error.contains("Unattempted"), "{error}");
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
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
        let mut destination = machine('b', "application");
        destination.machine.accepts_builds = false;
        destination.machine.runtime.architecture = architecture.into();
        let yaml = fs::read_to_string(root.join("compose.yaml")).unwrap();
        fs::write(
            root.join("compose.yaml"),
            yaml.replace("    x-pre_deploy: {command: ['true']}\n", ""),
        )
        .unwrap();
        deploy(
            &root,
            service.with_machines(vec![machine('a', "builder"), destination]),
        )
        .await
        .unwrap_or_else(|error| panic!("{architecture}/{platform}: {error}"));
        assert!(!created.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
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
        }],
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
async fn deploy_derives_railpack_platforms_from_the_machines_a_service_may_run_on() {
    let mut builder = machine('a', "builder");
    builder.machine.runtime.architecture = "x86_64".into();
    let mut arm = machine('b', "application");
    arm.machine.runtime.architecture = "aarch64".into();
    for (placement, expected) in [
        (
            format!(
                "    deploy: {{placement: {{constraints: [node.id=={}]}}}}\n",
                arm.machine.id
            ),
            vec!["linux/arm64"],
        ),
        (String::new(), vec!["linux/amd64", "linux/arm64"]),
    ] {
        let (root, service, builds) = fixture();
        let created = service.created_specs();
        *builds.platforms.lock().unwrap() = Some(
            expected
                .iter()
                .map(|platform| (*platform).to_owned())
                .collect(),
        );
        builds.workers.lock().unwrap().insert(
            builder.machine.id,
            expected
                .iter()
                .map(|platform| (*platform).to_owned())
                .collect(),
        );
        fs::remove_file(root.join("Dockerfile")).unwrap();
        fs::write(
            root.join("compose.yaml"),
            format!(
                "name: example\nservices:\n  one:\n    image: registry.invalid/shared:latest\n    build: {{context: ., x-recipe: railpack}}\n{placement}"
            ),
        )
        .unwrap();
        deploy(
            &root,
            service.with_machines(vec![builder.clone(), arm.clone()]),
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
            expected,
            "{placement:?}"
        );
        assert!(!created.lock().unwrap().is_empty());
        drop(definitions);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn explicit_platforms_that_miss_a_placement_machine_refuse_before_any_upload() {
    let (root, service, builds) = fixture();
    let mutations = service.mutating_rpcs();
    let mut builder = machine('a', "builder");
    builder.machine.runtime.architecture = "x86_64".into();
    let mut arm = machine('b', "application");
    arm.machine.runtime.architecture = "aarch64".into();
    fs::remove_file(root.join("Dockerfile")).unwrap();
    fs::write(
        root.join("compose.yaml"),
        format!(
            "name: example\nservices:\n  one:\n    image: registry.invalid/shared:latest\n    build: {{context: ., x-recipe: railpack, platforms: [linux/amd64]}}\n    deploy: {{placement: {{constraints: [node.id=={}]}}}}\n",
            arm.machine.id
        ),
    )
    .unwrap();
    let error = deploy(&root, service.with_machines(vec![builder, arm]))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("linux/arm64")
            && error.contains("build.platforms")
            && error.contains("No Service, hook, or volume change was attempted"),
        "{error}"
    );
    assert!(builds.definitions.lock().unwrap().is_empty());
    assert!(builds.pulls.lock().unwrap().is_empty());
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
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
        }],
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
async fn pinned_builder_cannot_bypass_build_acceptance() {
    let (root, service, builds) = fixture();
    let mut source = machine('a', "builder");
    source.machine.accepts_builds = false;
    let (mut client, server) = connected(service.with_machines(vec![source])).await;
    let visible = client.machines().await.unwrap();
    let error = super::super::build::select_build_machine(
        &mut client,
        Some(&ployz_core::MachineTarget::parse("builder").unwrap()),
        &build_targets(&["linux/amd64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("does not accept Builds"), "{error}");
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

// Rung 1: selection through the existing Machine transport boundary.
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
        super::super::build::select_build_machine(
            &mut client,
            None,
            &build_targets(&["linux/amd64"]),
            &visible,
            &Default::default()
        )
        .await
        .unwrap()
        .id,
        builder.machine.id
    );
    let error = super::super::build::select_build_machine(
        &mut client,
        Some(&ployz_core::MachineTarget::parse("down").unwrap()),
        &build_targets(&["linux/amd64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("Down"), "{error}");
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn builder_selection_reports_missing_capability_and_preserves_pin_ambiguity() {
    let (root, mut service, builds) = fixture();
    service.builds = None;
    let mut disabled = machine('b', "builder");
    disabled.machine.accepts_builds = false;
    let mut down = machine('c', "down");
    down.membership = MembershipObservation::Down;
    let (mut client, server) =
        connected(service.with_machines(vec![machine('a', "builder"), disabled, down])).await;
    let visible = client.machines().await.unwrap();
    let error = super::super::build::select_build_machine(
        &mut client,
        None,
        &build_targets(&["linux/amd64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    for reason in [
        "does not support remote Builds",
        "does not accept Builds",
        "Down",
    ] {
        assert!(error.contains(reason), "{error}");
    }
    let error = super::super::build::select_build_machine(
        &mut client,
        Some(&ployz_core::MachineTarget::parse("builder").unwrap()),
        &build_targets(&["linux/amd64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("ambiguous"), "{error}");
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
async fn builder_selection_requires_one_worker_for_every_command_target_before_upload() {
    let (root, service, builds) = fixture();
    let amd = machine('a', "amd");
    let arm = machine('b', "arm");
    *builds.workers.lock().unwrap() = std::collections::BTreeMap::from([
        (amd.machine.id, vec!["linux/amd64".into()]),
        (arm.machine.id, vec!["linux/arm64".into()]),
    ]);
    let (mut client, server) =
        connected(service.with_machines(vec![amd.clone(), arm.clone()])).await;
    let visible = client.machines().await.unwrap();
    let selected = super::super::build::select_build_machine(
        &mut client,
        None,
        &build_targets(&["linux/arm64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(selected.id, arm.machine.id);
    let targets = build_targets(&["linux/amd64", "linux/arm64"]);
    let error = super::super::build::select_build_machine(
        &mut client,
        None,
        &targets,
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot build linux/arm64"), "{error}");
    assert!(error.contains("cannot build linux/amd64"), "{error}");
    let error = super::super::build::select_build_machine(
        &mut client,
        Some(&ployz_core::MachineTarget::from(&amd.machine.id)),
        &build_targets(&["linux/arm64"]),
        &visible,
        &Default::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot build linux/arm64"), "{error}");
    assert!(builds.definitions.lock().unwrap().is_empty());
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
