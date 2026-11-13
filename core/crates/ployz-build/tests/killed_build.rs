//! A Build whose process is killed mid-solve, as a cancelled GitHub Actions job is (#4174).

use ployz_build::{
    Admission, HostPolicy, Output, Request, Target, builder_name, clear_cache, execute_admitted,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const CHILD_ROOT: &str = "PLOYZ_KILLED_BUILD_ROOT";

fn policy(root: &Path) -> HostPolicy {
    HostPolicy {
        configuration_file: root.join("build.yaml"),
        state_directory: root.to_owned(),
        docker: root.join("docker"),
        active_timeout: Duration::from_secs(120),
        ..Default::default()
    }
}

/// Docker as a host with no builder left: `buildx rm` finds nothing to remove.
fn docker(root: &Path) -> PathBuf {
    let docker = root.join("docker");
    fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
root=$(dirname "$0")
printf '%s\n' "$*" >> "$root/calls"
case "$1 $2" in
 'context show') echo default ;;
 'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
 'buildx ls') echo '{{"Name":"{name}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
 'buildx rm') [ -f "$root/builder-gone" ] && {{ echo 'ERROR: no builder "{name}" found' >&2; exit 1; }} ;;
 'buildx bake') echo $$ > "$root/bake.pid"; exec sleep 600 ;;
esac
exit 0
"#,
            name = builder_name()
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    docker
}

#[test]
fn a_build_killed_mid_solve_does_not_wedge_every_later_build() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    docker(root.path());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "a_build_solves_until_it_is_killed",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_ROOT, root.path())
        .spawn()
        .unwrap();
    let bake = root.path().join("bake.pid");
    let started = Instant::now();
    while !fs::read_to_string(&bake).is_ok_and(|pid| pid.ends_with('\n')) {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the build never started its solve"
        );
        // real-wait: the Docker stand-in is another process and signals only through a file
        std::thread::sleep(Duration::from_millis(20));
    }

    // The runner ends a cancelled job's process tree with SIGKILL once its grace runs out.
    let bake = fs::read_to_string(&bake).unwrap();
    for pid in [child.id().to_string(), bake.trim().to_owned()] {
        assert!(
            Command::new("kill")
                .args(["-KILL", &pid])
                .status()
                .unwrap()
                .success()
        );
    }
    child.wait().unwrap();
    let marker = root.path().join(format!("{}.lock", builder_name()));
    eprintln!(
        "left behind {}: {:?}",
        marker.display(),
        fs::read_to_string(&marker).unwrap()
    );

    // Every process of that Build is gone, and so is its builder.
    fs::write(root.path().join("builder-gone"), "").unwrap();
    let policy = policy(root.path());
    let restarted = Admission::cleanup_abandoned(&policy);
    eprintln!("daemon-start cleanup: {restarted:?}");
    let cleared = clear_cache(&policy);
    eprintln!("ployz server build-cache clear: {cleared:?}");
    let next = Admission::try_acquire_with(&policy);
    assert!(
        next.is_ok(),
        "the next Build is refused although nothing of the killed one survives: {}",
        next.err().unwrap()
    );
}

#[test]
#[ignore = "child of a_build_killed_mid_solve_does_not_wedge_every_later_build"]
fn a_build_solves_until_it_is_killed() {
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("run by its parent"));
    let policy = policy(&root);
    let admission = Admission::try_acquire_with(&policy).unwrap();
    let targets = [Target {
        name: "app".into(),
        platforms: Vec::new(),
    }];
    let request = Request {
        image_contexts: &BTreeMap::new(),
        compose_file: &root.join("compose.yaml"),
        sealed_override: None,
        working_dir: &root,
        environment: &BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        docker: Some(&policy.docker),
        targets: &targets,
        railpack: &[],
        build_args: &[],
        output: Output::Registry,
        no_cache: false,
        pull: false,
        sealed: false,
    };
    let _ = execute_admitted(&request, admission, &|_| {});
    unreachable!("the parent kills this process mid-solve");
}
