//! Railpack recipe selection, exclusions, and private variable capture.

use super::*;

#[test]
fn railpack_selection_keeps_explicit_dockerfiles_and_refuses_check() {
    let root = std::env::temp_dir().join(format!("ployz-recipe-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for (declaration, has_file, railpack) in [
        ("build: .", false, true),
        ("build: .", true, false),
        ("build: {context: ., dockerfile: Dockerfile}", false, false),
        ("build: {context: ., x-recipe: railpack}", true, true),
        ("build: {context: ., x-recipe: dockerfile}", false, false),
        (
            "build: {context: ., dockerfile_inline: 'FROM scratch'}",
            false,
            false,
        ),
    ] {
        let _ = fs::remove_file(root.join("Dockerfile"));
        if has_file {
            fs::write(root.join("Dockerfile"), "FROM scratch").unwrap();
        }
        fs::write(
            root.join("compose.yaml"),
            format!("services:\n  app:\n    {declaration}\n"),
        )
        .unwrap();
        let load = LoadOptions {
            working_dir: Some(root.clone()),
            ..Default::default()
        };
        let mut project = load_project(&load).unwrap();
        let options = BuildOptions {
            output: Output::Validate,
            ..Default::default()
        };
        let plan = plan_build(&project, &options).unwrap();
        let error = capture_build(&plan, &options, &mut project)
            .err()
            .map(|error| error.to_string());
        assert_eq!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("Railpack") && error.contains("--check")),
            railpack,
            "{declaration}: {error:?}"
        );
        if !railpack && !has_file && !declaration.contains("inline") {
            assert!(error.is_some());
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed capture fixture")]
fn railpack_capture_filters_before_transfer_and_keeps_effective_variables_private() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-railpack-capture-{}", std::process::id()));
    fs::create_dir_all(root.join("src/config")).unwrap();
    fs::write(
        root.join("src/.dockerignore"),
        "*.txt\nconfig\n.dockerignore\n",
    )
    .unwrap();
    fs::write(
        root.join("src/config/custom.json"),
        "{ // Railpack permits comments\n\"exclude\": [\"!keep.txt\", \"drop.log\",],\n}",
    )
    .unwrap();
    for name in ["drop.txt", "drop.log"] {
        fs::write(root.join("src").join(name), "excluded-private-bytes").unwrap();
    }
    fs::write(root.join("src/keep.txt"), "captured").unwrap();
    let mut project = parse_normalized(r#"
services:
  api:
    image: example.test/api:check
    build: {context: ./src, args: {MODE: compose}}
    environment: {TOKEN: 'secret://token', MODE: runtime, DEFAULT: service, RAILPACK_CONFIG_FILE: config/custom.json}
secrets:
  token: {x-command: "sh -c 'echo once >> provider-calls; printf private-token'"}
"#, &root).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    fs::write(root.join("image"), "example.test/api:check").unwrap();
    let mut captures = Vec::new();
    for value in ["first'$\nline", "changed"] {
        let options = BuildOptions {
            build_args: vec![format!("MODE={value}")],
            ..Default::default()
        };
        let plan = plan_build(&project, &options).unwrap();
        captures.push((value, capture_build(&plan, &options, &mut project).unwrap()));
    }
    project.resolve_secrets().unwrap();
    assert_eq!(
        project.services["api"].container.environment["MODE"],
        "runtime"
    );
    assert_eq!(
        project.services["api"].container.environment["TOKEN"],
        "private-token"
    );
    assert_eq!(
        fs::read_to_string(root.join("provider-calls")).unwrap(),
        "once\n"
    );
    fs::remove_dir_all(root.join("src")).unwrap();
    let mut hashes = Vec::new();
    for (value, capture) in captures {
        fs::write(root.join("preparation-fails"), "").unwrap();
        let failed = capture
            .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
            .unwrap_err()
            .to_string();
        assert!(failed.contains("Railpack preparation"), "{failed}");
        fs::remove_file(root.join("preparation-fails")).unwrap();
        let result = one_built(
            capture
                .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
                .unwrap(),
        );
        assert_eq!(
            one_built(
                capture
                    .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
                    .unwrap()
            ),
            result
        );
        assert!(!format!("{result:?}").contains("private-token"));
        let received = root.join("received");
        assert_eq!(
            fs::read_to_string(received.join("keep.txt")).unwrap(),
            "captured"
        );
        assert!(received.join("config/custom.json").exists());
        assert!(received.join(".dockerignore").exists());
        for file in ["drop.txt", "drop.log"] {
            assert!(!received.join(file).exists());
        }
        let bake: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("override.yaml")).unwrap()).unwrap();
        let args = &bake["target"]["api"]["args"];
        assert_eq!(args.as_object().unwrap().len(), 2);
        assert!(!args.to_string().contains(value));
        hashes.push(args["secrets-hash"].as_str().unwrap().to_owned());
        let staged = root.join("relocated/private/railpack/0");
        // BTreeMap order is not the claim: verify the collection of supplied values.
        let mut values = fs::read_dir(&staged)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("secret-"))
            .map(|entry| {
                assert_eq!(
                    entry.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
                fs::read_to_string(entry.path()).unwrap()
            })
            .collect::<Vec<_>>();
        values.sort();
        let mut expected = vec!["config/custom.json", "private-token", "service", value];
        expected.sort();
        assert_eq!(values, expected);
        let source_compose = fs::read_to_string(root.join("relocated/compose.yaml")).unwrap();
        assert!(!source_compose.contains("private-token"));
        assert!(
            !fs::read_to_string(root.join("calls"))
                .unwrap()
                .contains("private-token")
        );
        let captured_root = fs::read_to_string(root.join("capture-root")).unwrap();
        assert!(
            !Path::new(captured_root.trim())
                .join("private/railpack")
                .exists()
        );
        drop(capture);
        assert!(!Path::new(captured_root.trim()).exists());
    }
    assert_ne!(
        hashes[0], hashes[1],
        "changed variables must invalidate frontend cache"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn railpack_refuses_frontend_options_it_cannot_honor() {
    let root = std::env::temp_dir().join(format!("ployz-railpack-options-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for field in [
        "target: build",
        "labels: {hello: world}",
        "ssh: [default]",
        "network: none",
        "additional_contexts: {other: .}",
        "x-bake: {no-cache-filter: [build]}",
    ] {
        let mut project = parse_normalized(
            &format!("services:\n  api:\n    build: {{context: ., x-recipe: railpack, {field}}}\n"),
            &root,
        )
        .unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let error = capture_build(&plan, &options, &mut project)
            .err()
            .expect("ignored option must be refused")
            .to_string();
        assert!(
            error.contains("Railpack") && error.contains(field.split(':').next().unwrap()),
            "{error}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn railpack_accepts_explicit_linux_architectures_and_rejects_other_platforms() {
    let root =
        std::env::temp_dir().join(format!("ployz-railpack-platforms-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for (platforms, accepted) in [
        ("linux/amd64, linux/arm64", true),
        ("linux/arm64", true),
        ("linux/amd64, windows/amd64", false),
        ("linux/arm/v7", false),
    ] {
        let mut project = parse_normalized(&format!("services:\n  api:\n    build: {{context: ., x-recipe: railpack, platforms: [{platforms}]}}\n"), &root).unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let result = capture_build(&plan, &options, &mut project);
        assert_eq!(result.is_ok(), accepted, "{platforms}: {:?}", result.err());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn multi_platform_railpack_refuses_requested_attestations() {
    let root = std::env::temp_dir().join(format!(
        "ployz-railpack-attestations-{}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    for (field, platforms, value, accepted) in [
        ("provenance", "linux/amd64, linux/arm64", "true", false),
        ("provenance", "linux/amd64, linux/arm64", "mode=max", false),
        ("provenance", "linux/amd64, linux/arm64", "false", true),
        ("provenance", "linux/amd64", "true", true),
        ("provenance", "linux/arm64", "mode=max", true),
        ("sbom", "linux/amd64, linux/arm64", "true", false),
        (
            "sbom",
            "linux/amd64, linux/arm64",
            "generator=docker/scout-sbom-indexer",
            false,
        ),
        ("sbom", "linux/amd64, linux/arm64", "false", true),
        ("sbom", "linux/amd64", "true", true),
    ] {
        let mut project = parse_normalized(&format!("services:\n  api:\n    build: {{context: ., x-recipe: railpack, platforms: [{platforms}], {field}: {value}}}\n"), &root).unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let result = capture_build(&plan, &options, &mut project);
        if accepted {
            assert!(
                result.is_ok(),
                "{field}, {platforms}, {value}: {:?}",
                result.err()
            );
        } else {
            let error = result
                .err()
                .expect("requested attestation must not be discarded")
                .to_string();
            assert!(
                error.contains("multi-platform Railpack")
                    && error.contains(&format!("build.{field}")),
                "{error}"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn multi_platform_build_requires_complete_content_and_cleans_up_failed_attempts() {
    let root = std::env::temp_dir().join(format!("ployz-railpack-outcomes-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let docker = write_multi_docker(&root);
    let mut project = parse_normalized("services:\n  api:\n    image: example.test/api:check\n    build: {context: ., x-recipe: railpack, platforms: [linux/amd64, linux/arm64]}\n", &root).unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let captured = capture_build(&plan, &options, &mut project).unwrap();
    for (failure, diagnostic) in [
        ("create-fails", "assembly image unavailable"),
        (
            "create-fails-after-creation",
            "assembly create response lost",
        ),
        ("assembly-fails", "assemble Railpack"),
        ("import-fails", "load assembled"),
        ("missing-content", "verify Railpack platform content"),
        ("solve-fails", "the build"),
        ("worker-missing", "reported no capability"),
    ] {
        fs::write(root.join(failure), "").unwrap();
        let error = captured
            .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains(diagnostic), "{error}");
        assert!(!error.contains("cleanup could not be confirmed"), "{error}");
        assert!(!root.join("assembly").exists());
        assert!(!root.join("builder").exists());
        let capture_root = fs::read_to_string(root.join("capture-root")).unwrap();
        assert!(
            !Path::new(capture_root.trim())
                .join("private/railpack")
                .exists()
        );
        fs::remove_file(root.join(failure)).unwrap();
    }
    let first = one_built(
        captured
            .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
            .unwrap(),
    );
    assert_eq!(first.built.platforms, ["linux/amd64", "linux/arm64"]);
    assert_eq!(first.built.reference, FIRST_CONTENT);
    assert_eq!(first.built.tags, ["example.test/api:check"]);
    fs::write(root.join("digest"), SECOND_CONTENT).unwrap();
    let second = one_built(
        captured
            .execute(Some(&docker), &tokio_util::sync::CancellationToken::new())
            .unwrap(),
    );
    assert_ne!(first.built.reference, second.built.reference);
    assert_eq!(first.built.tags, second.built.tags);
    fs::remove_dir_all(root).unwrap();
}

fn write_multi_docker(root: &Path) -> std::path::PathBuf {
    let base = root.join("base-docker");
    write_docker(&base, root);
    fs::write(root.join("image"), "example.test/api:check").unwrap();
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    fs::write(
        root.join("media"),
        "application/vnd.oci.image.index.v1+json",
    )
    .unwrap();
    let docker = root.join("docker");
    fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
root='{}'
case "$1" in --ready) exit 0 ;; esac
case "$*" in
  'buildx bake '*)
    if [ -f "$root/cancel-solve" ]; then pwd > "$root/capture-root"; touch "$root/active"; exec sleep 60; fi
    if [ -f "$root/solve-fails" ]; then pwd > "$root/capture-root"; exit 1; fi ;;
  'buildx ls '*)
    if [ -f "$root/worker-missing" ]; then printf '{{}}'; exit 0; fi ;;
  'create --name '*-assemble*)
    pwd > "$root/capture-root"
    if [ -f "$root/cancel-create" ]; then touch "$root/active"; exec sleep 60; fi
    if [ -f "$root/create-fails" ]; then printf 'assembly image unavailable' >&2; exit 1; fi
    touch "$root/assembly"
    if [ -f "$root/cancel-create-after-creation" ]; then touch "$root/active"; exec sleep 60; fi
    if [ -f "$root/create-fails-after-creation" ]; then printf 'assembly create response lost' >&2; exit 1; fi ;;
  'rm --force '*-assemble)
    if [ -f "$root/cleanup-fails" ] && [ -f "$root/active" ]; then printf 'assembly still running' >&2; exit 1; fi
    if [ ! -f "$root/assembly" ]; then printf "Error response from daemon: No such container: %s" "$3" >&2; exit 1; fi
    rm -f "$root/assembly" ;;
  'start '*-assemble) exit 0 ;;
  'exec '*-assemble' regctl index create '*)
    if [ -f "$root/cancel-assembly" ]; then touch "$root/active"; exec sleep 60; fi
    test ! -f "$root/assembly-fails"; exit $? ;;
  'exec '*-assemble' regctl image digest '*) cat "$root/digest"; exit 0 ;;
  'exec '*-assemble' regctl '*) exit 0 ;;
  'cp '*:/tmp/variant.tar) exit 0 ;;
  'image load '*) test ! -f "$root/import-fails"; exit $? ;;
  'image save '*) test ! -f "$root/missing-content"; exit $? ;;
  'image tag '*) if [ -f "$root/cleanup-fails" ]; then touch "$root/active"; fi; exit 0 ;;
esac
exec "$root/base-docker" "$@"
"#,
            root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    docker
}

#[test]
fn railpack_cancellation_stops_work_and_releases_private_inputs_and_admission() {
    const CHILD_ROOT: &str = "PLOYZ_TEST_CANCEL_BUILD_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = std::path::PathBuf::from(root);
        use ployz_build::{Admission, HostPolicy, Railpack, Request, Target};
        use std::collections::BTreeMap;
        use std::os::unix::fs::DirBuilderExt as _;
        fs::create_dir_all(root.join("private")).unwrap();
        fs::create_dir_all(root.join("source")).unwrap();
        let state = root.join(format!("state-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        let policy = HostPolicy {
            state_directory: state,
            configuration_file: root.join("build.yaml"),
            docker: root.join("docker"),
            ..HostPolicy::default()
        };
        let targets = [Target {
            name: "api".into(),
            platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        }];
        let recipes = [Railpack {
            name: "api".into(),
            context: "source".into(),
            variables: BTreeMap::new(),
            refresh_cache: false,
        }];
        let request = Request {
            image_contexts: &BTreeMap::new(),
            compose_file: &root.join("compose.yaml"),
            working_dir: &root,
            environment: &BTreeMap::from([("PATH".into(), std::env::var("PATH").unwrap())]),
            docker: Some(&policy.docker),
            targets: &targets,
            railpack: &recipes,
            build_args: &[],
            output: ployz_build::Output::Load,
            no_cache: false,
            pull: false,
        };
        let admission = Admission::try_acquire_with(&policy).unwrap();
        let cancelled = admission.cancellation();
        let marker = root.join("interrupt");
        std::thread::spawn(move || {
            while !marker.exists() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            cancelled.cancel();
        });
        let error = ployz_build::execute_admitted(&request, admission, &|_| {})
            .unwrap_err()
            .to_string();
        if root.join("cleanup-fails").exists() {
            assert!(
                error.contains("cleanup could not be confirmed")
                    && error.contains("assembly still running"),
                "{error}"
            );
            assert!(root.join("assembly").exists());
        } else {
            assert!(error.contains("cancelled"), "{error}");
            assert!(!root.join("assembly").exists());
        }
        assert!(!root.join("builder").exists());
        let capture_root = fs::read_to_string(root.join("capture-root")).unwrap();
        assert!(
            !Path::new(capture_root.trim())
                .join("private/railpack")
                .exists()
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-railpack-cancel-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    write_multi_docker(&root);
    for (phase, cleanup_fails) in [
        ("create", false),
        ("create-after-creation", false),
        ("solve", false),
        ("assembly", false),
        ("assembly", true),
    ] {
        if cleanup_fails {
            fs::write(root.join("cleanup-fails"), "").unwrap();
        }
        let marker = root.join(format!("cancel-{phase}"));
        fs::write(&marker, "").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "railpack::railpack_cancellation_stops_work_and_releases_private_inputs_and_admission", "--nocapture"])
            .env(CHILD_ROOT, &root)
            .env("HOME", &root).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !root.join("active").exists() {
            if child.try_wait().unwrap().is_some() || std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("build did not reach its {phase} workload");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        fs::write(root.join("interrupt"), "").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("cancelled build did not finish cleanup");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        fs::remove_file(root.join("interrupt")).unwrap();
        fs::remove_file(marker).unwrap();
        fs::remove_file(root.join("active")).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn multi_platform_failures_preserve_output_stage_and_completed_image_evidence() {
    use ployz_build::{
        Admission, HostPolicy, Progress, Request, Stage, Target, TargetEvidence, WorkEvidence,
    };
    use std::collections::BTreeMap;
    let root = std::env::temp_dir().join(format!("ployz-railpack-evidence-{}", std::process::id()));
    let capture = root.join("capture");
    fs::create_dir_all(capture.join("private")).unwrap();
    fs::create_dir(capture.join("source")).unwrap();
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join("state"))
        .unwrap();
    fs::write(capture.join("compose.yaml"), "services: {}\n").unwrap();
    let docker = write_multi_docker(&root);
    let targets = [Target {
        name: "api".into(),
        platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
    }];
    let recipes = [ployz_build::Railpack {
        name: "api".into(),
        context: "source".into(),
        variables: BTreeMap::new(),
        refresh_cache: false,
    }];
    let environment = BTreeMap::from([("PATH".into(), std::env::var("PATH").unwrap())]);
    let policy = HostPolicy {
        configuration_file: root.join("build.yaml"),
        state_directory: root.join("state"),
        docker,
        active_timeout: std::time::Duration::from_secs(15),
        ..HostPolicy::default()
    };
    let request = Request {
        image_contexts: &BTreeMap::new(),
        compose_file: &capture.join("compose.yaml"),
        working_dir: &capture,
        environment: &environment,
        docker: Some(&policy.docker),
        targets: &targets,
        railpack: &recipes,
        build_args: &[],
        output: ployz_build::Output::Load,
        no_cache: false,
        pull: false,
    };
    for (failure, stage, completed) in [
        ("assembly-fails", Stage::Output, false),
        ("import-fails", Stage::Output, false),
        ("cleanup-fails", Stage::Cleanup, true),
    ] {
        fs::write(root.join(failure), "").unwrap();
        let evidence = std::sync::Mutex::new(WorkEvidence::new(&targets));
        let error = ployz_build::execute_admitted(
            &request,
            Admission::try_acquire_with(&policy).unwrap(),
            &|event: Progress| {
                evidence.lock().unwrap().observe(&event);
            },
        )
        .unwrap_err();
        assert_eq!(error.stage(), stage, "{error}");
        assert_eq!(error.is_unknown(), completed, "{error}");
        let evidence = evidence.into_inner().unwrap();
        let evidence = evidence.0.get("api").unwrap();
        if completed {
            let TargetEvidence::Image(image) = evidence else {
                panic!("completed image evidence was lost: {evidence:?}");
            };
            assert_eq!(image.reference, FIRST_CONTENT);
            assert_eq!(image.platforms, ["linux/amd64", "linux/arm64"]);
        } else {
            assert_eq!(evidence, &TargetEvidence::Unknown);
        }
        fs::remove_file(root.join(failure)).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
