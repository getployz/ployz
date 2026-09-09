//! Host policy and retained-cache ownership at the public executor boundary.

use ployz_build::{
    Admission, BuildError, HostPolicy, Output, Railpack, Request, Stage, Target, clear_cache,
    execute_admitted,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    time::{Duration, Instant},
};

struct Host {
    policy: HostPolicy,
}
impl Host {
    fn new(script: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ployz-policy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("private")).unwrap();
        fs::set_permissions(
            &root,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let docker = root.join("docker");
        fs::write(&docker, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            policy: HostPolicy {
                configuration_file: root.join("build.yaml"),
                state_directory: root,
                docker,
                active_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.policy.state_directory.join(name)
    }
    fn configure(&self, yaml: &str) {
        fs::write(self.file("build.yaml"), yaml).unwrap();
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.policy.state_directory);
    }
}

#[test]
fn cache_clearing_and_builds_share_exclusive_ownership_and_quarantine() {
    let host = Host::new(
        r#"
root=$(dirname "$0")
if [ "$1 $2" = 'context show' ]; then echo default; exit 0; fi
if [ "$1 $2" = 'buildx prune' ]; then
    touch "$root/clearing"
    while [ ! -f "$root/release" ]; do sleep 0.02; done
fi
if [ "$1 $2" = 'buildx rm' ] && [ -f "$root/refuse-cleanup" ]; then
    echo 'injected removal failure' >&2; exit 1
fi
"#,
    );
    let admission = Admission::try_acquire_with(&host.policy).unwrap();
    assert!(matches!(clear_cache(&host.policy), Err(BuildError::Busy)));
    assert!(!host.file("clearing").exists());
    drop(admission);

    let policy = host.policy.clone();
    let clearing = std::thread::spawn(move || clear_cache(&policy));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.file("clearing").exists() {
        assert!(Instant::now() < deadline, "cache clear did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        Admission::try_acquire_with(&host.policy),
        Err(BuildError::Busy)
    ));
    assert!(matches!(clear_cache(&host.policy), Err(BuildError::Busy)));
    fs::write(host.file("release"), "").unwrap();
    clearing.join().unwrap().unwrap();
    drop(Admission::try_acquire_with(&host.policy).unwrap());

    fs::write(host.file("refuse-cleanup"), "").unwrap();
    assert!(clear_cache(&host.policy).unwrap_err().is_unknown());
    assert!(
        Admission::try_acquire_with(&host.policy)
            .err()
            .unwrap()
            .is_unknown()
    );
    assert!(clear_cache(&host.policy).unwrap_err().is_unknown());
}

#[test]
fn invalid_host_configuration_refuses_admission_before_execution() {
    let host = Host::new("exit 99");
    host.configure("cpu_cores: -1");
    assert!(
        Admission::try_acquire_with(&host.policy)
            .err()
            .unwrap()
            .to_string()
            .contains("cpu_cores")
    );
    assert!(
        clear_cache(&host.policy)
            .unwrap_err()
            .to_string()
            .contains("cpu_cores")
    );
}

#[test]
fn admitted_limits_cover_worker_and_railpack_preparation_despite_later_policy_edits() {
    // This stand-in checks the upstream launch contract. Real workload
    // enforcement is exercised separately at rung 4.
    let host = Host::new(&format!(
        r#"
case "$1 $2" in
 'context show') echo default ;;
 'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CpuCfsPeriod":true,"CpuCfsQuota":true,"MemoryLimit":true,"SwapLimit":true}}' ;;
 'version --format') echo linux/amd64 ;;
 'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64"]}}]}}' ;;
 'buildx create')
    case " $* " in *' cpu-quota=50000 '*' memory=536870912 '*' memory-swap=536870912 '*) ;; *) echo 'worker limits missing' >&2; exit 1 ;; esac ;;
 'create --name')
    for expected in '--cpu-quota 50000' '--memory 536870912' '--memory-swap 536870912'; do
        case " $* " in *" $expected "*) ;; *) echo 'preparation limits missing' >&2; exit 1 ;; esac
    done ;;
 'buildx prune')
    for expected in '--max-used-space 1073741824' '--min-free-space 2147483648'; do
        case " $* " in *" $expected "*) ;; *) echo 'GC targets missing' >&2; exit 1 ;; esac
    done ;;
esac
if [ "$1" = cp ]; then
    case "$2" in *:/plan.json) echo '{{}}' > "$3" ;; esac
fi
exit 0
"#,
        ployz_build::builder_name()
    ));
    host.configure("cpu_cores: 0.5\nmemory_bytes: 536870912\ncache_bytes: 1073741824\nmin_free_bytes: 2147483648");
    let admission = Admission::try_acquire_with(&host.policy).unwrap();
    host.configure("cpu_cores: 8\nmemory_bytes: 8589934592");
    let targets = [Target {
        name: "app".into(),
        platforms: Vec::new(),
    }];
    let railpack = [Railpack {
        name: "app".into(),
        context: "source".into(),
        variables: BTreeMap::from([("cpu_cores".into(), "8".into())]),
        refresh_cache: false,
    }];
    let request = Request {
        compose_file: &host.file("compose.yaml"),
        working_dir: &host.policy.state_directory,
        environment: &BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        docker: Some(&host.policy.docker),
        targets: &targets,
        railpack: &railpack,
        build_args: &[],
        output: Output::Registry,
        no_cache: false,
        pull: false,
    };
    execute_admitted(&request, admission, &|_| {}).unwrap();
    assert!(Admission::try_acquire_with(&host.policy).is_ok());
}

#[test]
fn resource_launch_failure_reports_preparation_and_releases_confirmed_ownership() {
    let host = Host::new(
        "if [ \"$1 $2\" = 'context show' ]; then echo default; fi\nif [ \"$1 $2\" = 'buildx create' ]; then echo 'CPU quota unsupported' >&2; exit 1; fi\nexit 0",
    );
    host.configure("cpu_cores: 0.5");
    let targets = [Target {
        name: "app".into(),
        platforms: Vec::new(),
    }];
    let error = execute_admitted(
        &Request {
            compose_file: &host.file("compose.yaml"),
            working_dir: &host.policy.state_directory,
            environment: &BTreeMap::new(),
            docker: Some(&host.policy.docker),
            targets: &targets,
            railpack: &[],
            build_args: &[],
            output: Output::Validate,
            no_cache: false,
            pull: false,
        },
        Admission::try_acquire_with(&host.policy).unwrap(),
        &|_| {},
    )
    .unwrap_err();
    assert_eq!(error.stage(), Stage::Preparation);
    assert!(error.to_string().contains("CPU quota unsupported"));
    assert!(!error.is_unknown());
    assert!(Admission::try_acquire_with(&host.policy).is_ok());
}

#[test]
fn changing_home_cannot_fork_default_builder_ownership() {
    if let Some(output) = std::env::var_os("PLOYZ_POLICY_TEST_CHILD") {
        fs::write(
            output,
            HostPolicy::default()
                .state_directory
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        return;
    }
    let host = Host::new("exit 99");
    let output = host.file("child-state");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "changing_home_cannot_fork_default_builder_ownership",
        ])
        .env("HOME", &host.policy.state_directory)
        .env("PLOYZ_POLICY_TEST_CHILD", &output)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        fs::read(output).unwrap(),
        HostPolicy::default()
            .state_directory
            .as_os_str()
            .as_encoded_bytes()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn missing_home_uses_the_accounts_build_policy() {
    if let Some(output) = std::env::var_os("PLOYZ_POLICY_HOME_CHILD") {
        fs::write(
            output,
            HostPolicy::default()
                .configuration_file
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        return;
    }
    let uid = rustix::process::getuid().as_raw().to_string();
    let accounts = fs::read_to_string("/etc/passwd").unwrap();
    let account_home = accounts
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.nth(2) == Some(uid.as_str()))
                .then(|| fields.nth(2))
                .flatten()
        })
        .expect("test user has a passwd entry");
    let host = Host::new("exit 99");
    let output = host.file("configuration-path");
    for home in [
        None,
        Some(""),
        Some(host.policy.state_directory.to_str().unwrap()),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "missing_home_uses_the_accounts_build_policy"])
            .env_remove("HOME")
            .env("PLOYZ_POLICY_HOME_CHILD", &output);
        if let Some(home) = home {
            command.env("HOME", home);
        }
        assert!(command.status().unwrap().success());
        let expected = PathBuf::from(home.filter(|home| !home.is_empty()).unwrap_or(account_home))
            .join(".ployz/build.yaml");
        assert_eq!(
            fs::read(&output).unwrap(),
            expected.as_os_str().as_encoded_bytes()
        );
    }
}

#[test]
fn builds_refuse_remote_routing_before_builder_mutation() {
    let host = Host::new(
        r#"
root=$(dirname "$0")
if [ "$1 $2" = 'context show' ]; then echo "${PLOYZ_POLICY_CONTEXT:-default}"; exit 0; fi
echo "$*" >> "$root/mutations"
"#,
    );
    let targets = [Target {
        name: "api".into(),
        platforms: Vec::new(),
    }];
    for (key, value) in [
        ("DOCKER_HOST", "ssh://remote"),
        ("DOCKER_HOST", "tcp://remote:2375"),
        ("DOCKER_CONTEXT", "remote"),
        ("PLOYZ_POLICY_CONTEXT", "remote"),
    ] {
        let environment = BTreeMap::from([
            (key.into(), value.into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        let error = execute_admitted(
            &Request {
                compose_file: &host.file("compose.yaml"),
                working_dir: &host.policy.state_directory,
                environment: &environment,
                docker: Some(&host.policy.docker),
                targets: &targets,
                railpack: &[],
                build_args: &[],
                output: Output::Validate,
                no_cache: false,
                pull: false,
            },
            Admission::try_acquire_with(&host.policy).unwrap(),
            &|_| {},
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("local Docker"),
            "{key}={value}: {error}"
        );
        assert!(!host.file("mutations").exists(), "{key}={value}");
        assert!(Admission::try_acquire_with(&host.policy).is_ok());
    }
}

#[test]
fn cache_clearing_refuses_remote_routing_before_builder_mutation() {
    if let Some(root) = std::env::var_os("PLOYZ_POLICY_CACHE_CHILD") {
        let root = PathBuf::from(root);
        let policy = HostPolicy {
            docker: root.join("docker"),
            configuration_file: root.join("build.yaml"),
            state_directory: root.join("state"),
            ..HostPolicy::default()
        };
        let error = clear_cache(&policy).unwrap_err();
        assert!(error.to_string().contains("local Docker"), "{error}");
        assert!(Admission::try_acquire_with(&policy).is_ok());
        return;
    }
    let host = Host::new(
        r#"
root=$(dirname "$0")
if [ "$1 $2" = 'context show' ]; then echo "${PLOYZ_POLICY_CONTEXT:-default}"; exit 0; fi
echo "$*" >> "$root/mutations"
"#,
    );
    for (key, value) in [
        ("DOCKER_HOST", "ssh://remote"),
        ("DOCKER_HOST", "tcp://remote:2375"),
        ("DOCKER_CONTEXT", "remote"),
        ("PLOYZ_POLICY_CONTEXT", "remote"),
    ] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache_clearing_refuses_remote_routing_before_builder_mutation",
            ])
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .env_remove("PLOYZ_POLICY_CONTEXT")
            .env("PLOYZ_POLICY_CACHE_CHILD", &host.policy.state_directory)
            .env(key, value)
            .status()
            .unwrap();
        assert!(status.success(), "{key}={value}");
        assert!(!host.file("mutations").exists(), "{key}={value}");
    }
}
