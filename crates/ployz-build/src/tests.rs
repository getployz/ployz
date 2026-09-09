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
                std::thread::sleep(Duration::from_millis(10));
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
            std::thread::sleep(Duration::from_millis(200));
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
        "#!/bin/sh\ncase \"$1\" in --ready) exit 0 ;; esac\n/usr/bin/timeout 4 /usr/bin/yes output &\nwait\n",
    );
    let cancellation = Cancellation::default();
    let progress = |event| {
        if matches!(event, Progress::Output(_)) {
            cancellation.cancel();
            // Keep the descendant's pipe full while its parent is terminated.
            std::thread::sleep(Duration::from_millis(10));
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
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux"}}' ;;
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
        railpack: &[],
        compose_file: &directory.join("compose.json"),
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        build_args: &[],
        output: Output::Registry,
        no_cache: false,
        pull: false,
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
    let targets = [target("api", Some("linux/arm64")), target("web", None)];
    let planned = plan(&targets).unwrap();
    let metadata = Path::new("/private/build-metadata.json");
    let build_args = ["MODE=release".to_owned()];
    let request = |output| Request {
        railpack: &[],
        compose_file: Path::new("/private/compose.yaml"),
        working_dir: Path::new("/private"),
        environment: &environment,
        docker: None,
        targets: &targets,
        build_args: &build_args,
        output,
        no_cache: true,
        pull: false,
    };

    let validate = bake_arguments(&request(Output::Validate), &planned, metadata, None);
    assert!(validate.contains(&"--check".to_owned()));
    assert!(!validate.contains(&"--load".to_owned()));
    assert!(!validate.contains(&"--metadata-file".to_owned()));

    let load = bake_arguments(&request(Output::Load), &planned, metadata, None);
    assert!(load.contains(&"--load".to_owned()));
    assert!(!load.contains(&"--push".to_owned()));
    assert!(load.contains(&"--no-cache".to_owned()));
    assert!(!load.contains(&"--pull".to_owned()));
    assert!(load.contains(&"*.args.MODE=release".to_owned()));
    // The captured Compose file carries platforms; bake reads them there.
    assert!(!load.iter().any(|argument| argument.contains(".platform")));
    assert_eq!(load.last().map(String::as_str), Some("web"));

    let registry = bake_arguments(&request(Output::Registry), &planned, metadata, None);
    assert!(registry.contains(&"--push".to_owned()));
    assert!(!registry.contains(&"--load".to_owned()));
    assert!(!registry.contains(&"--metadata-file".to_owned()));
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
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
esac
"#,
                builder_name()
            ),
        );
        let environment = BTreeMap::new();
        let targets = [target("api", None)];
        let request = Request {
            compose_file: Path::new("compose.yaml"),
            working_dir: &directory,
            environment: &environment,
            docker: Some(&program),
            targets: &targets,
            railpack: &[],
            build_args: &[],
            output: Output::Validate,
            no_cache: false,
            pull: false,
        };
        execute(&request, &Cancellation::new()).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        Command::new("kill")
            .args([&signal, &std::process::id().to_string()])
            .status()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
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
        railpack: &[],
        compose_file: &directory.join("compose.json"),
        working_dir: &directory,
        environment: &environment,
        docker: Some(&program),
        targets: &targets,
        build_args: &[],
        output: Output::Load,
        no_cache: false,
        pull: false,
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
