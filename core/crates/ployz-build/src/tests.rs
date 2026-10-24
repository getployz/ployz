#[test]
fn later_uncertain_termination_preserves_the_first_failure_stage() {
    let first =
        super::BuildError::Result("missing target metadata".into()).at(super::Stage::Output);
    let later =
        super::BuildError::UncertainTermination("verification process still running".into());
    let combined = first.with_later_failure(later);
    assert!(combined.is_unknown());
    assert_eq!(combined.stage(), super::Stage::Output);
    assert!(combined.to_string().contains("missing target metadata"));
    assert!(
        combined
            .to_string()
            .contains("verification process still running")
    );
    let combined = super::BuildError::Cancelled
        .at(super::Stage::Building)
        .with_later_failure(combined);
    assert!(combined.is_unknown());
    assert_eq!(combined.stage(), super::Stage::Building);
}

use super::*;
use sha2::{Digest as _, Sha256};

fn wait_on_another_process(duration: Duration) {
    // real-wait: another process's exec, output burst, or signal delivery has no event this test can await
    std::thread::sleep(duration);
}

/// Write an executable stand-in and wait until it can be executed. A
/// concurrently forked process can briefly hold a just-written program
/// open, which makes the exec fail until that fork execs or exits.
pub(crate) fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    for _ in 0..100 {
        match Command::new(path).arg("--ready").status() {
            Ok(_) => return,
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                wait_on_another_process(Duration::from_millis(10));
            }
            Err(error) => panic!("stand-in {}: {error}", path.display()),
        }
    }
    panic!("stand-in {} never became executable", path.display());
}

#[test]
fn a_failed_build_is_not_blamed_on_an_earlier_command() {
    let directory = std::env::temp_dir().join(format!("ployz-diagnosis-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        "#!/bin/sh\ncase \"$1\" in\n  --ready) exit 0 ;;\n  captured) printf 'the earlier command failed\\n' >&2; exit 1 ;;\nesac\nexit 3\n",
    );
    let environment = BTreeMap::new();
    let docker = Docker {
        program: &program,
        environment: &environment,
        working_dir: &directory,
        deadline: Deadline::starting_now(EXECUTION_TIMEOUT),
        cancellation: None,
        progress: None,
    };

    // A captured command carries its own diagnosis.
    let captured = match docker.run("an earlier step", &["captured"], Streams::Captured) {
        Ok(output) => panic!("the stand-in reported success: {output}"),
        Err(error) => error.to_string(),
    };
    assert!(
        captured.contains("the earlier command failed"),
        "{captured}"
    );

    // The build's own output already reached the operator, so its failure
    // reports its status rather than the earlier command's diagnosis.
    let build = match docker.run("the build", &["build"], Streams::Inherited) {
        Ok(output) => panic!("the stand-in reported success: {output}"),
        Err(error) => error.to_string(),
    };
    assert!(build.contains("exited with"), "{build}");
    assert!(!build.contains("the earlier command failed"), "{build}");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn chatty_builds_deliver_the_final_diagnosis_without_a_disk_spool() {
    let directory = std::env::temp_dir().join(format!("ployz-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        "#!/bin/sh\ncase \"$1\" in --ready) exit 0 ;; esac\n/usr/bin/head -c 4194304 /dev/zero\nprintf final-diagnosis >&2\nexit 1\n",
    );
    let environment = BTreeMap::new();
    let output = std::sync::Mutex::new(Vec::new());
    let progress = |event| {
        if let Progress::Output(bytes) = event {
            output.lock().unwrap().extend(bytes);
        }
    };
    let docker = Docker {
        program: &program,
        environment: &environment,
        working_dir: &directory,
        deadline: Deadline::starting_now(Duration::from_secs(10)),
        cancellation: None,
        progress: Some(&progress),
    };
    // Let a file-backed writer finish its burst before the first poll.
    let error = docker
        .run_started("the build", &[], Streams::Inherited, || {
            wait_on_another_process(Duration::from_millis(200));
        })
        .unwrap_err();
    assert!(matches!(error, BuildError::Docker { .. }), "{error}");
    let output = output.into_inner().unwrap();
    assert_eq!(output.len(), 4 * 1024 * 1024 + b"final-diagnosis".len());
    assert!(output.ends_with(b"final-diagnosis"));
    let stored: u64 = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(stored < 1024 * 1024, "progress used {stored} bytes of disk");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cancellation_does_not_wait_for_a_surviving_output_writer() {
    let directory = std::env::temp_dir().join(format!("ployz-writer-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        "#!/bin/sh\ncase \"$1\" in --ready) exit 0 ;; esac\n(/usr/bin/yes output & writer=$!; sleep 4; kill \"$writer\" 2>/dev/null; wait) &\nwait\n",
    );
    let cancellation = Cancellation::default();
    let progress = |event| {
        if matches!(event, Progress::Output(_)) {
            cancellation.cancel();
            // Keep the descendant's pipe full while its parent is terminated.
            wait_on_another_process(Duration::from_millis(10));
        }
    };
    let environment = BTreeMap::new();
    let docker = Docker {
        program: &program,
        environment: &environment,
        working_dir: &directory,
        deadline: Deadline::starting_now(Duration::from_secs(10)),
        cancellation: Some(&cancellation),
        progress: Some(&progress),
    };
    let started = Instant::now();
    assert!(matches!(
        docker.run("the build", &[], Streams::Inherited),
        Err(BuildError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    std::fs::remove_dir_all(directory).unwrap();
}

fn target(name: &str, platform: Option<&str>) -> Target {
    Target {
        name: name.to_owned(),
        platforms: platform.map(ToOwned::to_owned).into_iter().collect(),
    }
}

#[test]
fn cancellation_between_pushes_leaves_later_targets_unattempted() {
    let directory = std::env::temp_dir().join(format!("ployz-push-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(
        &directory,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        &format!(
            r#"#!/bin/sh
case "$1 $2" in
  'context show') echo default ;;
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
  'buildx bake') echo invoked >> pushes ;;
esac
exit 0
"#,
            builder_name()
        ),
    );
    let targets = [target("api", None), target("web", None)];
    let environment = BTreeMap::new();
    let request = Request {
        image_contexts: &BTreeMap::new(),
        railpack: &[],
        compose_file: &directory.join("compose.json"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        build_args: &[],
        output: Output::Registry,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let admission = Admission::try_acquire_with(&HostPolicy {
        docker: program.clone(),
        state_directory: directory.clone(),
        active_timeout: EXECUTION_TIMEOUT,
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    })
    .unwrap();
    let cancellation = admission.cancellation();
    let work = std::sync::Mutex::new(WorkEvidence::new(&targets));
    let error = execute_admitted(&request, admission, &|event| {
        work.lock().unwrap().observe(&event);
        if matches!(
            event,
            Progress::Target {
                outcome: TargetEvidence::Published,
                ..
            }
        ) {
            cancellation.cancel();
        }
    })
    .unwrap_err();
    assert_eq!(error.stage(), Stage::Building);
    let work = work.into_inner().unwrap();
    assert_eq!(work.0.get("api"), Some(&TargetEvidence::Published));
    assert_eq!(work.0.get("web"), Some(&TargetEvidence::Unattempted));
    assert_eq!(
        std::fs::read_to_string(directory.join("pushes")).unwrap(),
        "invoked\n"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn each_loaded_image_builds_in_its_own_sequential_bake_run() {
    let directory = std::env::temp_dir().join(format!("ployz-runs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(
        &directory,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        &format!(
            r#"#!/bin/sh
case "$1 $2" in
  'context show') echo default ;;
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
  'buildx bake')
    while [ $# -gt 0 ]; do
      case "$1" in
        --metadata-file) shift; metadata="$1" ;;
        *.platform=*) target="${{1%%.platform=*}}" ;;
      esac
      shift
    done
    echo "$target" >> runs
    printf '{{"%s":{{"containerimage.digest":"sha256:%s","image.name":"example.test/%s"}}}}' "$target" "$target" "$target" > "$metadata" ;;
  'image inspect') printf '{{"Os":"linux","Architecture":"amd64","Descriptor":{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"%s"}}}}' "$3" ;;
esac
exit 0
"#,
            builder_name()
        ),
    );
    let targets = [target("api", None), target("web", None)];
    let environment = BTreeMap::new();
    let request = Request {
        image_contexts: &BTreeMap::new(),
        railpack: &[],
        compose_file: &directory.join("compose.json"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let admission = Admission::try_acquire_with(&HostPolicy {
        docker: program.clone(),
        state_directory: directory.clone(),
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    })
    .unwrap();
    let events = std::sync::Mutex::new(Vec::new());
    let images = execute_admitted(&request, admission, &|event| {
        if let Progress::Target { name, outcome } = event {
            let proven = matches!(outcome, TargetEvidence::Image(_));
            events.lock().unwrap().push((name, proven));
        }
    })
    .unwrap();
    assert_eq!(
        images
            .iter()
            .map(|image| image.reference.as_str())
            .collect::<Vec<_>>(),
        ["sha256:api", "sha256:web"]
    );
    // Each run names its one image before its steps, and finishes before the next.
    assert_eq!(
        events.into_inner().unwrap(),
        [
            ("api".to_owned(), false),
            ("api".to_owned(), true),
            ("web".to_owned(), false),
            ("web".to_owned(), true),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("runs")).unwrap(),
        "api\nweb\n"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn targets_that_would_share_one_build_name_are_refused() {
    let distinct = [target("api.internal", None), target("web", None)];
    let planned = plan(&distinct).unwrap();
    assert_eq!(
        planned
            .iter()
            .map(|planned| planned.bake.as_str())
            .collect::<Vec<_>>(),
        ["api_internal", "web"]
    );
    let colliding = [target("api.internal", None), target("api_internal", None)];
    let collision = match plan(&colliding) {
        Ok(_) => panic!("two targets shared one build name"),
        Err(error) => error.to_string(),
    };
    assert!(collision.contains("api_internal"), "{collision}");
}

#[test]
fn an_observed_platform_covers_a_request_without_its_variant() {
    assert!(covers("linux/arm64", "linux/arm64"));
    assert!(covers("linux/arm64/v8", "linux/arm64"));
    assert!(covers("linux/arm64", "linux/arm64/v8"));
    assert!(!covers("linux/amd64", "linux/arm64"));
    assert!(!covers("linux/arm", "linux/arm64"));
}

#[test]
fn requested_output_selects_exclusive_bake_behavior() {
    let environment = BTreeMap::new();
    let targets = [target("api", Some("linux/arm64"))];
    let planned = super::plan(&targets).unwrap();
    let planned = planned.first().unwrap();
    let metadata = Path::new("/private/build-metadata.json");
    let build_args = ["MODE=release".to_owned()];
    let image_contexts = BTreeMap::new();
    let request = |output| Request {
        image_contexts: &image_contexts,
        railpack: &[],
        compose_file: Path::new("/private/compose.yaml"),
        sealed_override: None,
        working_dir: Path::new("/private"),
        environment: &environment,
        docker: None,
        targets: &targets,
        build_args: &build_args,
        output,
        no_cache: true,
        pull: false,
        sealed: false,
    };

    let validate = bake_arguments(
        &request(Output::Validate),
        Produce::Output(Output::Validate),
        &builder_name(),
        planned,
        metadata,
        None,
    );
    assert!(validate.contains(&"--check".to_owned()));
    assert!(!validate.contains(&"--load".to_owned()));
    assert!(!validate.contains(&"--metadata-file".to_owned()));

    let load = bake_arguments(
        &request(Output::Load),
        Produce::Output(Output::Load),
        &builder_name(),
        planned,
        metadata,
        None,
    );
    assert!(load.contains(&"--load".to_owned()));
    assert!(!load.contains(&"--push".to_owned()));
    assert!(load.contains(&"--no-cache".to_owned()));
    assert!(!load.contains(&"--pull".to_owned()));
    assert!(load.contains(&"*.args.MODE=release".to_owned()));
    // The captured Compose file carries platforms; bake reads them there.
    assert!(!load.iter().any(|argument| argument.contains(".platform")));
    assert_eq!(load.last().map(String::as_str), Some("api"));

    let registry = bake_arguments(
        &request(Output::Registry),
        Produce::Output(Output::Registry),
        &builder_name(),
        planned,
        metadata,
        None,
    );
    assert!(registry.contains(&"--push".to_owned()));
    assert!(!registry.contains(&"--load".to_owned()));
    assert!(!registry.contains(&"--metadata-file".to_owned()));
    assert!(!load.iter().any(|argument| argument.contains("type=gha")));

    // Only a runner exposing its cache service gets the GitHub cache.
    let environment = BTreeMap::from([("ACTIONS_RUNTIME_TOKEN".to_owned(), "t".to_owned())]);
    let runner = bake_arguments(
        &Request {
            environment: &environment,
            ..request(Output::Load)
        },
        Produce::Output(Output::Load),
        &builder_name(),
        planned,
        metadata,
        None,
    );
    assert!(runner.contains(&"api.cache-from=type=gha,scope=api".to_owned()));
    // Uploading cache never delays the image: only a cache export writes it, and it
    // produces no image.
    assert!(!runner.iter().any(|argument| argument.contains("cache-to")));
    let export = bake_arguments(
        &Request {
            environment: &environment,
            ..request(Output::Load)
        },
        Produce::Cache,
        &builder_name(),
        planned,
        metadata,
        None,
    );
    assert!(export.contains(&"api.cache-to=type=gha,scope=api,mode=max".to_owned()));
    assert!(export.contains(&"api.output=type=cacheonly".to_owned()));
    assert!(!export.contains(&"--load".to_owned()));
    assert!(!export.contains(&"--metadata-file".to_owned()));

    let merged = bake_arguments(
        &Request {
            sealed_override: Some(Path::new("private/sealed/args.compose.yaml")),
            ..request(Output::Load)
        },
        Produce::Output(Output::Load),
        &builder_name(),
        planned,
        metadata,
        Some(Path::new("private/railpack.compose.yaml")),
    );
    let files: Vec<_> = merged
        .windows(2)
        .filter(|pair| pair.first().is_some_and(|arg| arg == "--file"))
        .filter_map(|pair| pair.get(1).map(String::as_str))
        .collect();
    assert_eq!(
        files,
        [
            "/private/compose.yaml",
            "private/sealed/args.compose.yaml",
            "private/railpack.compose.yaml"
        ]
    );
}

#[test]
fn cancellation_terminates_the_process_before_returning() {
    let cancellation = Cancellation::default();
    cancellation.cancel();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let result = wait_controlled(
        &mut child,
        Deadline::starting_now(EXECUTION_TIMEOUT),
        Some(&cancellation),
        &mut || {},
    );
    assert!(matches!(result, Err(BuildError::Cancelled)));
    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn a_command_that_outlasts_its_budget_is_terminated() {
    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let waited = Instant::now();
    assert!(matches!(
        wait_controlled(
            &mut child,
            Deadline::starting_now(Duration::from_millis(200)),
            None,
            &mut || {}
        ),
        Err(BuildError::TimedOut(_))
    ));
    assert!(waited.elapsed() < Duration::from_secs(5));
}

#[test]
fn executing_a_build_does_not_change_process_signal_handlers() {
    const CHILD: &str = "PLOYZ_BUILD_SIGNAL_TEST";
    if let Ok(signal) = std::env::var(CHILD) {
        let directory =
            std::env::temp_dir().join(format!("ployz-build-signal-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let program = directory.join("docker");
        executable(
            &program,
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
  'context show') echo default ;;
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
esac
"#,
                builder_name()
            ),
        );
        let environment = BTreeMap::new();
        let targets = [target("api", None)];
        let request = Request {
            image_contexts: &BTreeMap::new(),
            compose_file: Path::new("compose.yaml"),
            sealed_override: None,
            working_dir: &directory,
            environment: &environment,
            docker: Some(&program),
            targets: &targets,
            railpack: &[],
            build_args: &[],
            output: Output::Validate,
            no_cache: false,
            pull: false,
            sealed: false,
        };
        execute(&request, &Cancellation::new(), &|_| ()).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        Command::new("kill")
            .args([&signal, &std::process::id().to_string()])
            .status()
            .unwrap();
        wait_on_another_process(Duration::from_millis(200));
        panic!("the build swallowed {signal} after returning");
    }
    use std::os::unix::process::ExitStatusExt as _;
    for (signal, number) in [("-INT", 2), ("-TERM", 15)] {
        let directory =
            std::env::temp_dir().join(format!("ployz-signal-home-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::executing_a_build_does_not_change_process_signal_handlers",
            ])
            .env(CHILD, signal)
            .env("HOME", &directory)
            .status()
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(status.signal(), Some(number), "{signal}: {status}");
    }
}

#[test]
fn docker_preflight_holds_quarantine_and_clears_confirmed_failures() {
    let directory = std::env::temp_dir().join(format!("ployz-preflight-{}", uuid::Uuid::new_v4()));
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let program = directory.join("docker");
    let policy = HostPolicy {
        state_directory: directory.clone(),
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    };
    let targets = [target("api", None)];
    let environment = BTreeMap::new();
    let request = Request {
        image_contexts: &BTreeMap::new(),
        railpack: &[],
        compose_file: &directory.join("compose.json"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    for stop_at in ["context show", "buildx version"] {
        executable(
            &program,
            &format!(
                r#"#!/bin/sh
[ "$1" = --ready ] && exit 0
[ -s '{}.lock' ] || {{ echo missing-quarantine >&2; exit 1; }}
if [ "$*" = '{stop_at}' ]; then
    echo observed-quarantine >&2
    exit 1
fi
[ "$*" = 'context show' ] && echo default
exit 0
"#,
                builder_name()
            ),
        );
        let admission = Admission::try_acquire_with(&policy).unwrap();
        let error = execute_admitted(&request, admission, &|_| {}).unwrap_err();
        assert!(
            error.to_string().contains("observed-quarantine"),
            "{stop_at}: {error}"
        );
        assert_eq!(error.stage(), Stage::Preparation);
        assert!(!error.is_unknown());
        assert!(Admission::try_acquire_with(&policy).is_ok());
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cross_platform_image_context_reaches_buildkit() {
    use std::os::unix::fs::DirBuilderExt as _;

    let directory = std::env::temp_dir().join(format!("ployz-context-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        &format!(
            r#"#!/bin/sh
case "$1 $2" in
  'context show') echo default ;;
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"arm64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/arm64"]}}]}}' ;;
  'buildx bake') printf '%s\n' "$@" > bake-arguments ;;
esac
exit 0
"#,
            builder_name()
        ),
    );
    std::fs::write(
        directory.join("compose.yaml"),
        "services: {app: {build: {context: ., additional_contexts: {base: 'service:base'}}}}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("Dockerfile"),
        "FROM --platform=linux/amd64 base\n",
    )
    .unwrap();
    let contexts = BTreeMap::from([(
        "base".into(),
        ImageContext {
            reference: format!("example.test/base@sha256:{}", "1".repeat(64)),
            platforms: vec!["linux/amd64".into()],
            source: ployz_core::ImageIngestDestination {
                management_address: ployz_core::ManagementAddress(
                    std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
                ),
                port: 5000,
            },
        },
    )]);
    let environment = BTreeMap::new();
    let targets = [target("app", Some("linux/arm64"))];
    let request = Request {
        image_contexts: &contexts,
        compose_file: Path::new("compose.yaml"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        railpack: &[],
        build_args: &[],
        output: Output::Validate,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let admission = Admission::try_acquire_with(&HostPolicy {
        state_directory: directory.clone(),
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    })
    .unwrap();
    execute_admitted(&request, admission, &|_| {}).unwrap();
    assert!(
        std::fs::read_to_string(directory.join("bake-arguments"))
            .unwrap()
            .contains("app.platform=linux/arm64")
    );
    assert!(
        std::fs::read_to_string(directory.join("compose.yaml"))
            .unwrap()
            .contains("docker-image://127.0.0.1:5000/example.test/base@sha256:")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_cgroup_v1_host_is_refused_before_anything_builds() {
    use std::os::unix::fs::DirBuilderExt as _;

    let directory = std::env::temp_dir().join(format!("ployz-cgroup-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let program = directory.join("docker");
    executable(
        &program,
        r#"#!/bin/sh
case "$1 $2" in
  'context show') echo default ;;
  'info --format') echo '{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"1"}' ;;
  'buildx bake') touch baked ;;
esac
exit 0
"#,
    );
    std::fs::write(
        directory.join("compose.yaml"),
        "services: {app: {build: .}}\n",
    )
    .unwrap();
    let environment = BTreeMap::new();
    let targets = [target("app", None)];
    let request = Request {
        image_contexts: &BTreeMap::new(),
        compose_file: Path::new("compose.yaml"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        railpack: &[],
        build_args: &[],
        output: Output::Validate,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let admission = Admission::try_acquire_with(&HostPolicy {
        state_directory: directory.clone(),
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    })
    .unwrap();
    let error = execute_admitted(&request, admission, &|_| {}).unwrap_err();
    assert!(error.to_string().contains("cgroup v2"), "{error}");
    assert!(!directory.join("baked").exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn repository_reference_rejects_a_malformed_service_image() {
    let image = BuiltImage {
        reference: format!("sha256:{}", "1".repeat(64)),
        tags: vec!["example.test/api:latest".into()],
        platforms: vec!["linux/amd64".into()],
        location: "unix:///var/run/docker.sock".into(),
    };
    assert!(matches!(
        image.repository_reference("not a repository"),
        Err(BuildError::Result(_))
    ));
}

#[test]
fn a_cache_export_without_a_runner_cache_runs_nothing() {
    let environment = BTreeMap::new();
    let targets = [target("api", None)];
    let request = Request {
        image_contexts: &BTreeMap::new(),
        compose_file: Path::new("compose.yaml"),
        sealed_override: None,
        working_dir: Path::new("/nonexistent"),
        environment: &environment,
        docker: Some(Path::new("/nonexistent/docker")),
        targets: &targets,
        railpack: &[],
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    // Any Docker call would fail: this Docker does not exist.
    assert_eq!(export_cache(&request, &Cancellation::new()), Ok(()));
}

#[test]
#[ignore = "requires Docker and network access to pull the pinned Railpack images"]
fn railpack_prepares_a_python_requirements_app() {
    let directory = std::env::temp_dir().join(format!("ployz-railpack-{}", uuid::Uuid::new_v4()));
    let app = directory.join("source/app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir(directory.join("private")).unwrap();
    std::fs::write(app.join("requirements.txt"), "flask==3.0.0\n").unwrap();
    std::fs::write(app.join("Procfile"), "web: python -m flask run\n").unwrap();
    let targets = [target("web", None)];
    let railpack = [Railpack {
        name: "web".into(),
        context: "source/app".into(),
        variables: BTreeMap::new(),
        sealed: BTreeMap::new(),
        refresh_cache: false,
    }];
    let environment = BTreeMap::new();
    let request = Request {
        image_contexts: &BTreeMap::new(),
        railpack: &railpack,
        compose_file: &directory.join("compose.json"),
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: None,
        targets: &targets,
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let docker = Docker {
        program: Path::new("docker"),
        environment: &environment,
        working_dir: &directory,
        deadline: Deadline::starting_now(Duration::from_secs(600)),
        cancellation: None,
        progress: None,
    };
    let builder = format!("ployz-test-{}", uuid::Uuid::new_v4().simple());
    let preparation = railpack::prepare(
        &docker,
        &request,
        "linux/amd64",
        &builder,
        &policy::Resources::default(),
    )
    .unwrap()
    .unwrap();
    let plan = std::fs::read_to_string(
        preparation
            .override_file()
            .with_file_name("0")
            .join("plan.json"),
    )
    .unwrap();
    assert!(plan.contains("pip install -r requirements.txt"), "{plan}");
    drop(preparation);
    std::fs::remove_dir_all(directory).unwrap();
}

/// The whole Railpack path for one Sealed value: the name rides through the
/// opened file and a secret mount, a build step reads the value, and nothing
/// BuildKit keeps shows it.
#[test]
#[ignore = "requires Docker and network access to pull the pinned Railpack images"]
fn railpack_delivers_a_sealed_value_to_a_build_step_and_keeps_it_out_of_the_image() {
    let value = "opened-token-0123456789";
    let digest: String = Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let directory = std::env::temp_dir().join(format!("ployz-railpack-{}", uuid::Uuid::new_v4()));
    let app = directory.join("source/app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir(directory.join("private")).unwrap();
    std::fs::write(app.join("requirements.txt"), "").unwrap();
    std::fs::write(app.join("Procfile"), "web: cat /app/token.sha\n").unwrap();
    let image = format!("ployz-test-sealed:{}", uuid::Uuid::new_v4().simple());
    std::fs::write(
        directory.join("compose.yaml"),
        format!("services:\n  web:\n    image: {image}\n    build:\n      context: source/app\n"),
    )
    .unwrap();
    let opened = BTreeMap::from([("NPM_TOKEN".to_owned(), value.to_owned())]);
    let _files = crate::sealed::OpenedFiles::write(&directory, &opened).unwrap();
    let targets = [target("web", None)];
    let railpack = [Railpack {
        name: "web".into(),
        context: "source/app".into(),
        variables: BTreeMap::from([(
            "RAILPACK_BUILD_CMD".to_owned(),
            "printf %s \"$NPM_TOKEN\" | sha256sum | cut -d\" \" -f1 | tee /app/token.sha"
                .to_owned(),
        )]),
        sealed: BTreeMap::from([("NPM_TOKEN".to_owned(), "f".repeat(64))]),
        refresh_cache: false,
    }];
    // The environment a received Build gives Docker, so the CLI finds its program and config.
    let environment = crate::upload::environment(&directory);
    let compose_file = directory.join("compose.yaml");
    let request = Request {
        image_contexts: &BTreeMap::new(),
        railpack: &railpack,
        compose_file: &compose_file,
        sealed_override: None,
        working_dir: &directory,
        environment: &environment,
        docker: None,
        targets: &targets,
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
        sealed: true,
    };
    let reported = std::sync::Mutex::new(String::new());
    let progress = |progress: Progress| {
        use std::fmt::Write as _;
        let mut reported = reported.lock().unwrap();
        match progress {
            Progress::Output(bytes) => reported.push_str(&String::from_utf8_lossy(&bytes)),
            Progress::StepOutput { text, .. } => reported.push_str(&text),
            other @ (Progress::Stage(_)
            | Progress::Step(_)
            | Progress::Timing { .. }
            | Progress::Target { .. }) => {
                let _ = writeln!(reported, "{other:?}");
            }
        }
    };
    let docker = Docker {
        program: Path::new("docker"),
        environment: &environment,
        working_dir: &directory,
        deadline: Deadline::starting_now(Duration::from_secs(900)),
        cancellation: None,
        progress: Some(&progress),
    };
    let native = if cfg!(target_arch = "aarch64") {
        "linux/arm64"
    } else {
        "linux/amd64"
    };
    let builder = format!("ployz-test-{}", uuid::Uuid::new_v4().simple());
    let preparation = railpack::prepare(
        &docker,
        &request,
        native,
        &builder,
        &policy::Resources::default(),
    )
    .unwrap()
    .unwrap();
    let plan = std::fs::read_to_string(
        preparation
            .override_file()
            .with_file_name("0")
            .join("plan.json"),
    )
    .unwrap();
    assert!(plan.contains("NPM_TOKEN"), "{plan}");
    assert!(!plan.contains(value), "{plan}");
    // The same Bake run the Build makes, on a BuildKit worker like the one it acquires.
    docker
        .run(
            "create the test builder",
            &[
                "buildx",
                "create",
                "--name",
                &builder,
                "--driver",
                "docker-container",
            ],
            Streams::Captured,
        )
        .unwrap();
    let planned = super::plan(&targets).unwrap();
    let metadata = directory.join("build-metadata-web.json");
    let mut arguments = bake_arguments(
        &request,
        Produce::Output(Output::Load),
        &builder,
        planned.first().unwrap(),
        &metadata,
        Some(&preparation.override_file()),
    );
    arguments.insert(arguments.len() - 1, "--set".into());
    arguments.insert(arguments.len() - 1, format!("web.platform={native}"));
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    if let Err(error) = docker.run(
        "bake the sealed Railpack target",
        &arguments,
        Streams::Inherited,
    ) {
        panic!("{error}\n{}", reported.lock().unwrap());
    }
    let built = docker
        .run(
            "read what the build step wrote",
            &[
                "run",
                "--rm",
                "--entrypoint",
                "cat",
                &image,
                "/app/token.sha",
            ],
            Streams::Captured,
        )
        .unwrap();
    assert_eq!(built.trim(), digest);
    let kept = docker
        .run(
            "read what the image keeps",
            &["image", "inspect", &image],
            Streams::Captured,
        )
        .unwrap()
        + &docker
            .run(
                "read the image history",
                &[
                    "history",
                    "--no-trunc",
                    "--format",
                    "{{.CreatedBy}}",
                    &image,
                ],
                Streams::Captured,
            )
            .unwrap();
    assert!(!kept.contains(value), "{kept}");
    let reported = reported.lock().unwrap();
    assert!(reported.contains(&digest), "{reported}");
    assert!(!reported.contains(value), "{reported}");
    docker
        .run("remove the test image", &["rmi", &image], Streams::Captured)
        .unwrap();
    docker
        .run(
            "remove the test builder",
            &["buildx", "rm", "--force", &builder],
            Streams::Captured,
        )
        .unwrap();
    drop(preparation);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn the_builder_cache_is_not_user_data() {
    let cache = format!("buildx_buildkit_{}0_state", super::builder_name());
    assert!(super::is_builder_cache(&cache), "{cache}");
    assert!(super::is_builder_cache("buildx_buildkit_ployz-00_state"));
    // Concurrent builds use numbered slots after slot 0.
    let slot = format!("buildx_buildkit_{}-10_state", super::builder_name());
    assert!(super::is_builder_cache(&slot), "{slot}");
    assert!(super::is_builder_cache("buildx_buildkit_ployz-0-20_state"));
    assert!(super::is_builder_cache(
        "buildx_buildkit_ployz-0-2550_state"
    ));
    for data in [
        "shop-production_pgdata",
        "buildx_buildkit_ployz-00",
        "buildx_buildkit_other-00_state",
        "buildx_buildkit_ployz-_state",
        "buildx_buildkit_ployz-0-_state",
        "buildx_buildkit_ployz--10_state",
        "buildx_buildkit_ployz-0-1-20_state",
        "buildx_buildkit_ployz-0-x0_state",
        "buildx_buildkit_ployz-1000-11_state",
        "buildx_buildkit_ployz-1000-0_state",
        "buildx_buildkit_ployz-1000-00_state",
        "buildx_buildkit_ployz-1000-010_state",
        "buildx_buildkit_ployz-1000-2560_state",
        "buildx_buildkit_ployz-01000-10_state",
        "buildx_buildkit_ployz-99999999999-10_state",
        "buildx_buildkit_ployz-4294967295-10_state",
    ] {
        assert!(!super::is_builder_cache(data), "{data}");
    }
}

#[test]
#[ignore = "requires local Docker with the containerd image store and network access"]
fn dockerfile_sealed_args_reach_the_image_and_invalidate_the_cache() {
    fn docker(args: &[&str]) -> String {
        let output = Command::new("docker").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn no_plaintext(root: &Path, value: &str) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with("private/sealed") {
                continue;
            }
            if path.is_dir() {
                no_plaintext(&path, value);
            } else {
                assert!(
                    !std::fs::read(&path)
                        .unwrap()
                        .windows(value.len())
                        .any(|bytes| bytes == value.as_bytes()),
                    "{}",
                    path.display()
                );
            }
        }
    }
    let directory =
        std::env::temp_dir().join(format!("ployz-sealed-arg-receipt-{}", uuid::Uuid::new_v4()));
    let capture = directory.join("capture");
    std::fs::create_dir_all(capture.join("source/app")).unwrap();
    std::fs::create_dir_all(capture.join("private/docker")).unwrap();
    std::fs::write(capture.join("private/docker/config.json"), "{}").unwrap();
    std::fs::write(
        capture.join("private/sealed-args.json"),
        r#"{"api":["NPM_TOKEN"]}"#,
    )
    .unwrap();
    std::fs::write(capture.join("source/app/Dockerfile"), "FROM alpine:latest\nARG NPM_TOKEN\nRUN printf %s \"$NPM_TOKEN\" | sha256sum > /token.sha\nRUN --mount=type=secret,id=NPM_TOKEN cat /run/secrets/NPM_TOKEN | sha256sum > /mount.sha\n").unwrap();
    let image = format!("ployz-test-sealed-arg:{}", uuid::Uuid::new_v4().simple());
    std::fs::write(capture.join("compose.yaml"), format!("services:\n  api:\n    image: {image}\n    build:\n      context: source/app\n      args: {{PLAIN: captured}}\n      secrets: [NPM_TOKEN]\nsecrets:\n  NPM_TOKEN:\n    file: private/sealed/NPM_TOKEN\n")).unwrap();
    let definition = remote::Definition {
        targets: vec![target("api", None)],
        retained_tags: Vec::new(),
        image_contexts: BTreeMap::new(),
        output: Output::Load,
        no_cache: false,
        pull: false,
    };
    let policy = HostPolicy {
        state_directory: directory.join("state"),
        configuration_file: directory.join("build.yaml"),
        ..Default::default()
    };
    let values = [
        "dummy-ruling40-first",
        "dummy-ruling40-$NEXT-${HOME}-$$-second",
    ];
    let mut previous = None;
    for value in values {
        let admission = Admission::try_acquire_slot(&policy, 40).unwrap();
        let upload_root = admission.lock.upload_directory();
        let mut upload = admission.upload().unwrap();
        remote::upload(&capture, |input| upload.accept(input)).unwrap();
        let steps = std::sync::Mutex::new(Vec::new());
        let images = upload
            .execute(
                &definition,
                None,
                &BTreeMap::from([("NPM_TOKEN".into(), value.into())]),
                &|progress| match progress {
                    Progress::Step(step) => steps.lock().unwrap().push(step),
                    Progress::Target { .. } => no_plaintext(&upload_root, value),
                    Progress::Stage(
                        Stage::Admission
                        | Stage::Building
                        | Stage::Queued
                        | Stage::Preparation
                        | Stage::Output
                        | Stage::Cleanup
                        | Stage::Upload
                        | Stage::Push,
                    )
                    | Progress::Output(_)
                    | Progress::StepOutput { .. }
                    | Progress::Timing { .. } => {}
                },
            )
            .unwrap();
        assert_eq!(images.images.len(), 1);
        let digest: String = Sha256::digest(value.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let result = docker(&["run", "--rm", &image, "cat", "/token.sha", "/mount.sha"]);
        assert!(
            result.lines().all(|line| line.starts_with(&digest)),
            "{result}"
        );
        assert_eq!(result.lines().count(), 2);
        assert_ne!(previous.as_ref(), Some(&digest));
        let steps = steps.into_inner().unwrap();
        let step = steps
            .iter()
            .find(|step| step.name.contains("/token.sha") && step.completed.is_some())
            .unwrap();
        if previous.is_some() {
            assert!(!step.cached, "changed ARG reused the cached step");
        }
        previous = Some(digest.clone());
        let history = docker(&[
            "history",
            "--no-trunc",
            "--format",
            "{{.CreatedBy}}",
            &image,
        ]);
        assert!(history.contains(value), "{history}");
        assert!(!upload_root.exists());
        no_plaintext(&capture, value);
        eprintln!(
            "ARG receipt sha256={digest} step_cached={} upload_root_removed=true history_contains_value=true",
            step.cached
        );
        eprintln!(
            "{}",
            history
                .lines()
                .find(|line| line.contains("/token.sha"))
                .unwrap()
        );
        drop(images);
    }
    docker(&["rmi", "--force", &image]);
    docker(&[
        "volume",
        "rm",
        &format!("buildx_buildkit_{}-400_state", builder_name()),
    ]);
    std::fs::remove_dir_all(directory).unwrap();
}
