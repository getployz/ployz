//! A Build whose process is killed mid-solve, as a cancelled GitHub Actions job is (#4174).

use ployz_build::{
    Admission, BuildError, HostPolicy, Output, Request, Target, builder_name, clear_cache,
    execute_admitted,
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

/// Start a Build in another process, then kill it mid-solve, and its solve too
/// unless `solve_survives`. Returns the solve's PID.
fn kill_a_build_mid_solve(root: &Path, solve_survives: bool) -> (HostPolicy, String) {
    docker(root);
    let policy = policy(root);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "a_build_solves_until_it_is_killed",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_ROOT, root)
        .spawn()
        .unwrap();
    let bake = root.join("bake.pid");
    let started = Instant::now();
    while !fs::read_to_string(&bake).is_ok_and(|pid| pid.ends_with('\n')) {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the build never started its solve"
        );
        // real-wait: the Docker stand-in is another process and signals only through a file
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        matches!(Admission::try_acquire_with(&policy), Err(BuildError::Busy)),
        "a running Build lost its slot"
    );

    // The runner ends a cancelled job's process tree with SIGKILL once its grace runs out.
    let bake = fs::read_to_string(&bake).unwrap().trim().to_owned();
    let killed = [child.id().to_string(), bake.clone()];
    for pid in killed.iter().take(if solve_survives { 1 } else { 2 }) {
        assert!(
            Command::new("kill")
                .args(["-KILL", pid])
                .status()
                .unwrap()
                .success()
        );
    }
    child.wait().unwrap();
    let started = Instant::now();
    while !solve_survives
        && fs::read_to_string(format!("/proc/{bake}/stat"))
            .is_ok_and(|stat| !stat.rsplit_once(") ").unwrap().1.starts_with('Z'))
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "SIGKILL never landed"
        );
        // real-wait: signal delivery to another process has no event to await
        std::thread::sleep(Duration::from_millis(20));
    }
    let marker = root.join(format!("{}.lock", builder_name()));
    assert!(
        !fs::read_to_string(&marker).unwrap().is_empty(),
        "the killed Build left no mark"
    );
    (policy, bake)
}

fn quarantined_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn a_build_killed_mid_solve_does_not_wedge_every_later_build() {
    let root = quarantined_root();
    let (policy, _) = kill_a_build_mid_solve(root.path(), false);
    // Every process of that Build is gone, and so is its builder.
    fs::write(root.path().join("builder-gone"), "").unwrap();
    let next = Admission::try_acquire_with(&policy);
    assert!(
        next.is_ok(),
        "the next Build is refused although nothing of the killed one survives: {}",
        next.err().unwrap()
    );
}

#[test]
fn daemon_start_recovers_a_slot_whose_build_was_killed() {
    let root = quarantined_root();
    let (policy, _) = kill_a_build_mid_solve(root.path(), false);
    fs::write(root.path().join("builder-gone"), "").unwrap();
    Admission::cleanup_abandoned(&policy).unwrap();
    let marker = root.path().join(format!("{}.lock", builder_name()));
    assert!(fs::read_to_string(marker).unwrap().is_empty());
}

#[test]
fn cache_clear_recovers_a_slot_whose_build_was_killed() {
    let root = quarantined_root();
    let (policy, _) = kill_a_build_mid_solve(root.path(), false);
    fs::write(root.path().join("builder-gone"), "").unwrap();
    clear_cache(&policy).unwrap();
    let calls = fs::read_to_string(root.path().join("calls")).unwrap();
    assert!(
        calls.lines().any(|call| call.starts_with("buildx prune")),
        "{calls}"
    );
}

#[test]
fn a_killed_build_whose_solve_survives_stays_quarantined() {
    let root = quarantined_root();
    let (policy, solve) = kill_a_build_mid_solve(root.path(), true);
    fs::write(root.path().join("builder-gone"), "").unwrap();
    let refused = Admission::try_acquire_with(&policy).err().unwrap();
    let still_running = refused.to_string();
    assert!(
        still_running.contains(&format!("Docker CLI process {solve} the Build started")),
        "{still_running}"
    );
    assert!(
        Admission::cleanup_abandoned(&policy)
            .unwrap_err()
            .is_unknown()
    );
    assert!(clear_cache(&policy).unwrap_err().is_unknown());

    assert!(
        Command::new("kill")
            .args(["-KILL", &solve])
            .status()
            .unwrap()
            .success()
    );
    let started = Instant::now();
    while let Err(error) = Admission::try_acquire_with(&policy) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the slot stayed quarantined after its solve ended: {error}"
        );
        // real-wait: signal delivery to another process has no event to await
        std::thread::sleep(Duration::from_millis(20));
    }
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
