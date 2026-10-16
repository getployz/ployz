use super::*;
use crate::installer::test_support::{fixture, run_contract_child_with_environment, write_script};
use std::{cell::Cell, env, ffi::OsString, os::unix::fs::symlink};

const ROOT: &str = "PLOYZ_INSTALLER_CONTRACT_ROOT";
const DEBIAN_KERNEL: &str = "6.1.0-28-amd64";
const AMAZON_KERNEL: &str = "6.1.186-228.376.amzn2023.x86_64";
const DEBIAN_12: &str = "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nID=debian\n";
const UBUNTU_24: &str = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\n";
const AMAZON_LINUX_2023: &str = "NAME=\"Amazon Linux\"\nVERSION=\"2023\"\nID=\"amzn\"\nID_LIKE=\"fedora\"\nVERSION_ID=\"2023\"\n";

fn contract_root() -> PathBuf {
    PathBuf::from(env::var_os(ROOT).unwrap())
}

fn read_state(root: &Path) -> PrepState {
    serde_json::from_slice(&fs::read(root.join("data/prepare/state.json")).unwrap()).unwrap()
}

fn read_log(root: &Path) -> String {
    fs::read_to_string(root.join("commands.log")).unwrap_or_default()
}

fn host(name: &str, os_release: &str, fakes: &[(&str, &str)]) -> tempfile::TempDir {
    let fixture = fixture(name);
    let root = fixture.path();
    let commands = root.join("commands");
    fs::create_dir_all(&commands).unwrap();
    fs::create_dir_all(root.join("apt/sources.list.d")).unwrap();
    fs::create_dir_all(root.join("usr/src")).unwrap();
    fs::write(root.join("os-release"), os_release).unwrap();
    for (command, body) in fakes {
        write_script(
            &commands.join(command),
            &format!(
                "echo \"{command} $*\" >> \"$PLOYZ_INSTALLER_CONTRACT_ROOT/commands.log\"\n{body}"
            ),
        );
    }
    write_script(&commands.join("systemd-detect-virt"), "echo kvm");
    write_script(
        &commands.join("modprobe"),
        "echo \"modprobe $*\" >> \"$PLOYZ_INSTALLER_CONTRACT_ROOT/commands.log\"; exit 97",
    );
    for tool in ["mkdir", "cp", "cat", "tar", "gzip"] {
        symlink(format!("/usr/bin/{tool}"), commands.join(tool)).unwrap();
    }
    fixture
}

fn assert_nothing_installed_or_loaded(log: &str) {
    let forbidden = |line: &&str| {
        line.starts_with("dkms install")
            || line.starts_with("modprobe")
            || (line.starts_with("dnf install") && line.contains(".rpm"))
            || (line.starts_with("apt-get") && line.contains(" install -y") && line.contains("zfs"))
    };
    assert_eq!(log.lines().find(forbidden), None, "{log}");
}

fn dpkg_query(installed: &[&str]) -> String {
    let installed = installed.join("|");
    format!("case \"$3\" in {installed}) printf 'install ok installed' ;; *) exit 1 ;; esac")
}

#[tokio::test]
async fn debian_prepare_builds_the_module_without_installing_it() {
    const CASE: &str = "PLOYZ_PREPARE_DEBIAN_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        prepare_at(&InstallPaths::at(&root), false, async || Ok(()))
            .await
            .unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for case in ["fresh", "already-built", "foreign"] {
        let dkms = match case {
            "already-built" => {
                format!(
                    "case \"$1\" in status) echo 'zfs/2.1.11, {DEBIAN_KERNEL}, x86_64: built' ;; esac"
                )
            }
            "foreign" => {
                "case \"$*\" in 'status -m zfs -v 2.1.11') echo 'zfs/2.1.11: added' ;; esac"
                    .to_owned()
            }
            _ => String::new(),
        };
        let fixture = host(
            case,
            DEBIAN_12,
            &[
                ("uname", &format!("echo {DEBIAN_KERNEL}")),
                ("modinfo", "exit 1"),
                ("dpkg", ""),
                ("dpkg-query", &dpkg_query(&["dkms", "perl"])),
                (
                    "apt-get",
                    "case \"$*\" in *'download zfs-dkms') : > zfs-dkms_2.1.11-1_all.deb ;; esac",
                ),
                (
                    "dpkg-deb",
                    "mkdir -p \"$3/usr/src/zfs-2.1.11\"; printf 'PACKAGE_NAME=\"zfs\"\\nAUTOINSTALL=\"yes\"\\n' > \"$3/usr/src/zfs-2.1.11/dkms.conf\"",
                ),
                ("dkms", &dkms),
            ],
        );
        let root = fixture.path();
        run_contract_child_with_environment(
            "installer::prepare::tests::debian_prepare_builds_the_module_without_installing_it",
            root,
            OsString::from(CASE),
            OsString::from(case),
            case,
            [],
        );

        let log = read_log(root);
        assert_nothing_installed_or_loaded(&log);
        let state = read_state(root);
        assert_eq!(state.docker.step, DockerStep::Ready, "{case}");
        assert_eq!(state.zfs.step, ZfsStep::Ready, "{case}");
        assert!(state.zfs.started_at.is_some() && state.zfs.finished_at.is_some());
        if case == "already-built" {
            assert!(!log.contains("apt-get"), "{case} rebuilt:\n{log}");
            assert!(state.added_packages.is_empty());
            continue;
        }
        let commands: Vec<&str> = log
            .lines()
            .filter(|line| !line.starts_with("uname") && !line.starts_with("modinfo"))
            .filter(|line| !line.starts_with("dpkg-query") && !line.starts_with("dkms status"))
            .collect();
        let headers = format!("linux-headers-{DEBIAN_KERNEL} linux-headers-amd64");
        // A registration this Machine did not make is only built, never removed or replaced.
        let register = if case == "foreign" {
            vec![]
        } else {
            vec!["dkms add -m zfs -v 2.1.11".to_owned()]
        };
        assert_eq!(
            commands,
            [
                "dpkg --configure -a".to_owned(),
                "apt-get -o DPkg::Lock::Timeout=300 update -qq".to_owned(),
                format!(
                    "apt-get -o DPkg::Lock::Timeout=300 install -y -qq --no-install-recommends {headers} file libc6-dev lsb-release"
                ),
                "apt-get -o DPkg::Lock::Timeout=300 download zfs-dkms".to_owned(),
                format!(
                    "dpkg-deb -x {0}/data/prepare/zfs/zfs-dkms_2.1.11-1_all.deb {0}/data/prepare/zfs/root",
                    root.display()
                ),
            ]
            .into_iter()
            .chain(register)
            .chain([format!("dkms build -m zfs -v 2.1.11 -k {DEBIAN_KERNEL}")])
            .collect::<Vec<_>>(),
            "{case}"
        );
        assert_eq!(
            state.added_packages,
            [
                format!("linux-headers-{DEBIAN_KERNEL}"),
                "linux-headers-amd64".into(),
                "file".into(),
                "libc6-dev".into(),
                "lsb-release".into(),
            ]
        );
        assert!(state.added_contrib_source);
        if case == "foreign" {
            assert_eq!(state.zfs_build, None);
            assert!(!root.join("usr/src/zfs-2.1.11").exists());
            continue;
        }
        assert_eq!(
            state.zfs_build,
            Some(ZfsBuild::Dkms {
                module: "zfs".into(),
                version: "2.1.11".into(),
                source_dir: root.join("usr/src/zfs-2.1.11"),
            })
        );
        assert_eq!(
            fs::read_to_string(root.join("usr/src/zfs-2.1.11/dkms.conf")).unwrap(),
            "PACKAGE_NAME=\"zfs\"\nAUTOINSTALL=\"no\"\n"
        );
        assert!(!root.join("data/machine.json").exists());
    }
}

#[tokio::test]
async fn ubuntu_prepare_downloads_zfs_without_installing_it() {
    const CASE: &str = "PLOYZ_PREPARE_UBUNTU_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        prepare_at(&InstallPaths::at(&root), false, async || Ok(()))
            .await
            .unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "ubuntu",
        UBUNTU_24,
        &[
            ("uname", "echo 6.8.0-45-generic"),
            ("modinfo", "exit 0"),
            ("dpkg", ""),
            ("apt-get", ""),
        ],
    );
    let root = fixture.path();
    run_contract_child_with_environment(
        "installer::prepare::tests::ubuntu_prepare_downloads_zfs_without_installing_it",
        root,
        OsString::from(CASE),
        OsString::from("ubuntu"),
        "ubuntu",
        [],
    );

    let log = read_log(root);
    assert_nothing_installed_or_loaded(&log);
    let apt: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("apt-get") || line.starts_with("dpkg "))
        .collect();
    assert_eq!(
        apt,
        [
            "dpkg --configure -a",
            "apt-get -o DPkg::Lock::Timeout=300 update -qq",
            "apt-get -o DPkg::Lock::Timeout=300 install --download-only -y -qq zfsutils-linux",
        ]
    );
    let state = read_state(root);
    assert_eq!(state.zfs.step, ZfsStep::Ready);
    assert!(state.added_packages.is_empty());
    assert_eq!(
        state.zfs_build,
        Some(ZfsBuild::Ubuntu {
            downloaded: vec!["zfsutils-linux".into()]
        })
    );
}

#[test]
fn amazon_linux_build_stops_at_dkms_build() {
    use sha2::{Digest, Sha256};

    const CASE: &str = "PLOYZ_PREPARE_AMAZON_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let os = OsRelease::read(&paths.os_release).unwrap();
        let sha256 = hex::encode(Sha256::digest(fs::read(root.join("tarball")).unwrap()));
        let mut job = Job {
            path: state_path(&paths),
            state: PrepState::default(),
        };
        fs::create_dir_all(prepare_dir(&paths)).unwrap();
        build_amazon_zfs(&paths, &mut job, &os, AMAZON_KERNEL, &sha256).unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "amazon",
        AMAZON_LINUX_2023,
        &[
            (
                "dnf",
                "case \"$*\" in *repoquery*) echo kernel-devel-1:6.1.186 ;; esac",
            ),
            ("rpm", "case \"$3\" in make) exit 0 ;; *) exit 1 ;; esac"),
            (
                "curl",
                "while [ \"$1\" != -o ]; do shift; done\ncat < \"$PLOYZ_INSTALLER_CONTRACT_ROOT/tarball\" > \"$2\"",
            ),
            (
                "make",
                "for package in zfs-2.4.4-1.x86_64 zfs-dkms-2.4.4-1.noarch libzfs7-2.4.4-1.x86_64 libzpool7-2.4.4-1.x86_64 libnvpair3-2.4.4-1.x86_64 libuutil3-2.4.4-1.x86_64; do : > \"$package.rpm\"; done",
            ),
            ("rpm2cpio", "echo archive"),
            (
                "cpio",
                "cat > /dev/null\nmkdir -p \"$3/usr/src/zfs-2.4.4\"\n: > \"$3/usr/src/zfs-2.4.4/dkms.conf\"",
            ),
            ("dkms", ""),
        ],
    );
    let root = fixture.path();
    let release = root.join("release/zfs-2.4.4");
    fs::create_dir_all(&release).unwrap();
    write_script(&release.join("configure"), "exit 0");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(root.join("tarball"))
        .arg("-C")
        .arg(root.join("release"))
        .arg("zfs-2.4.4")
        .status()
        .unwrap();
    assert!(status.success());
    run_contract_child_with_environment(
        "installer::prepare::tests::amazon_linux_build_stops_at_dkms_build",
        root,
        OsString::from(CASE),
        OsString::from("amazon"),
        "amazon",
        [],
    );

    let log = read_log(root);
    assert_nothing_installed_or_loaded(&log);
    let dnf_installs: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("dnf install"))
        .collect();
    let [dnf_install] = dnf_installs.as_slice() else {
        panic!("expected one dnf install: {log}");
    };
    assert!(
        dnf_install.starts_with(&format!(
            "dnf install -y kernel-devel-uname-r = {AMAZON_KERNEL}"
        )) && !dnf_install.contains(" make"),
        "{log}"
    );
    assert!(log.contains("dkms add -m zfs -v 2.4.4"), "{log}");
    assert!(
        log.contains(&format!("dkms build -m zfs -v 2.4.4 -k {AMAZON_KERNEL}")),
        "{log}"
    );
    let state = read_state(root);
    assert!(state.added_packages.contains(&kernel_devel(AMAZON_KERNEL)));
    assert!(!state.added_packages.contains(&"make".to_owned()));
}

#[tokio::test]
async fn a_held_lock_exits_without_doing_anything() {
    let fixture = fixture("held-lock");
    let paths = InstallPaths::at(fixture.path());
    let held = lock(&paths, Wait::No).unwrap().unwrap();
    let ran = Cell::new(false);
    prepare_at(&paths, false, async || {
        ran.set(true);
        Ok(())
    })
    .await
    .unwrap();
    drop(held);
    assert!(!ran.get());
    assert!(!state_path(&paths).exists());
}

#[tokio::test]
async fn a_refused_host_records_zfs_skipped() {
    const CASE: &str = "PLOYZ_PREPARE_REFUSED_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        prepare_at(&InstallPaths::at(&root), false, async || Ok(()))
            .await
            .unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "refused",
        "PRETTY_NAME=\"Debian GNU/Linux 11 (bullseye)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"11\"\nID=debian\n",
        &[("dpkg", "")],
    );
    let root = fixture.path();
    run_contract_child_with_environment(
        "installer::prepare::tests::a_refused_host_records_zfs_skipped",
        root,
        OsString::from(CASE),
        OsString::from("refused"),
        "refused",
        [],
    );
    assert_eq!(read_log(root), "dpkg --configure -a\n");
    let state = read_state(root);
    assert_eq!(state.docker.step, DockerStep::Ready);
    assert_eq!(
        state.zfs.step,
        ZfsStep::Skipped {
            reason: "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs Debian 11. Use one of those, or add `--storage none`.".into()
        }
    );
}

#[tokio::test]
async fn skip_zfs_records_off_and_a_docker_failure_fails_the_job() {
    let fixture = fixture("skip-zfs");
    let paths = InstallPaths::at(fixture.path());
    let result = prepare_at(&paths, true, async || Err("no systemd".into())).await;
    assert!(
        matches!(&result, Err(Error::Command { message, .. }) if message == "Docker failed (no systemd), ZFS off"),
        "{result:?}"
    );
    let state = read_state(fixture.path());
    assert_eq!(
        state.docker.step,
        DockerStep::Failed {
            message: "no systemd".into()
        }
    );
    assert_eq!(state.zfs.step, ZfsStep::Off);
}

#[test]
fn discard_removes_only_what_preparation_added() {
    const CASE: &str = "PLOYZ_PREPARE_DISCARD_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        discard_at(&InstallPaths::at(&root)).unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "discard",
        DEBIAN_12,
        &[
            ("apt-get", ""),
            (
                "dkms",
                "case \"$1\" in status) echo 'zfs/2.1.11: added' ;; esac",
            ),
        ],
    );
    let root = fixture.path();
    let paths = InstallPaths::at(root);
    let source_dir = root.join("usr/src/zfs-2.1.11");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(root.join("usr/src/linux-headers-6.1.0-28-amd64")).unwrap();
    fs::create_dir_all(prepare_dir(&paths).join("zfs/root")).unwrap();
    let contrib = debian_contrib_source(&paths.apt_dir);
    fs::write(&contrib, "Components: contrib\n").unwrap();
    let job = Job {
        path: state_path(&paths),
        state: PrepState {
            docker: Timed {
                step: DockerStep::Ready,
                started_at: None,
                finished_at: None,
            },
            zfs: Timed {
                step: ZfsStep::Ready,
                started_at: None,
                finished_at: None,
            },
            added_packages: vec!["linux-headers-amd64".into(), "lsb-release".into()],
            zfs_build: Some(ZfsBuild::Dkms {
                module: "zfs".into(),
                version: "2.1.11".into(),
                source_dir: source_dir.clone(),
            }),
            added_contrib_source: true,
            interrupted: vec![],
        },
    };
    job.save().unwrap();
    run_contract_child_with_environment(
        "installer::prepare::tests::discard_removes_only_what_preparation_added",
        root,
        OsString::from(CASE),
        OsString::from("discard"),
        "discard",
        [],
    );

    let log = read_log(root);
    let changes: Vec<&str> = log
        .lines()
        .filter(|line| !line.starts_with("dkms status"))
        .collect();
    assert_eq!(
        changes,
        [
            "dkms remove -m zfs -v 2.1.11 --all",
            "apt-get -o DPkg::Lock::Timeout=300 remove -y -qq linux-headers-amd64 lsb-release",
        ]
    );
    assert!(!source_dir.exists());
    assert!(!prepare_dir(&paths).join("zfs").exists());
    assert!(!contrib.exists());
    assert!(root.join("usr/src/linux-headers-6.1.0-28-amd64").exists());
    let state = read_state(root);
    assert_eq!(state.docker.step, DockerStep::Ready);
    assert_eq!(state.zfs.step, ZfsStep::Off);
    assert!(state.added_packages.is_empty());
    assert_eq!(state.zfs_build, None);
}

fn claimed_build(paths: &InstallPaths, source_dir: &Path) -> Job {
    Job {
        path: state_path(paths),
        state: PrepState {
            added_packages: vec!["dkms".into(), "lsb-release".into()],
            zfs_build: Some(ZfsBuild::Dkms {
                module: "zfs".into(),
                version: "2.1.11".into(),
                source_dir: source_dir.to_owned(),
            }),
            ..PrepState::default()
        },
    }
}

/// A full install that put zfs-dkms in place owns the module, so a discard that still finds the
/// build claimed leaves it, its source and its packages alone.
#[test]
fn discard_leaves_an_installed_zfs_package_alone() {
    const CASE: &str = "PLOYZ_PREPARE_DISCARD_INSTALLED_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        discard_at(&InstallPaths::at(&root)).unwrap();
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "discard-installed",
        DEBIAN_12,
        &[
            ("apt-get", ""),
            ("dpkg-query", &dpkg_query(&["zfs-dkms", "dkms"])),
            (
                "dkms",
                "case \"$1\" in status) echo 'zfs/2.1.11, 6.1.0-28-amd64, x86_64: installed' ;; esac",
            ),
        ],
    );
    let root = fixture.path();
    let paths = InstallPaths::at(root);
    let source_dir = root.join("usr/src/zfs-2.1.11");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(prepare_dir(&paths)).unwrap();
    claimed_build(&paths, &source_dir).save().unwrap();
    run_contract_child_with_environment(
        "installer::prepare::tests::discard_leaves_an_installed_zfs_package_alone",
        root,
        OsString::from(CASE),
        OsString::from("discard-installed"),
        "discard-installed",
        [],
    );

    let log = read_log(root);
    let changes: Vec<&str> = log
        .lines()
        .filter(|line| !line.starts_with("dkms status") && !line.starts_with("dpkg-query"))
        .collect();
    assert!(changes.is_empty(), "{log}");
    assert!(source_dir.exists());
}

/// Discard removes the contrib source only when preparation wrote it; one a full install left
/// stays.
#[test]
fn discard_keeps_a_contrib_source_preparation_did_not_add() {
    let fixture = fixture("discard-foreign-contrib");
    let paths = InstallPaths::at(fixture.path());
    fs::create_dir_all(paths.apt_dir.join("sources.list.d")).unwrap();
    let contrib = debian_contrib_source(&paths.apt_dir);
    fs::write(&contrib, "Components: contrib\n").unwrap();
    discard_at(&paths).unwrap();
    assert!(contrib.exists());
}

/// A child that outlives its preparation run keeps the lock, so a retry can't start a second
/// build beside it.
#[test]
fn the_lock_outlives_its_holder_in_a_child() {
    let fixture = fixture("lock-child");
    let paths = InstallPaths::at(fixture.path());
    let held = lock(&paths, Wait::No).unwrap().unwrap();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    drop(held);
    let contended = lock(&paths, Wait::No).unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(contended, "the lock fell with its holder while a child ran");
    assert!(lock(&paths, Wait::No).unwrap().is_some());
}

/// A Docker that `docker.socket` would start but that doesn't answer isn't ready: the step asks
/// it the way install does.
#[tokio::test]
async fn docker_step_asks_a_socket_activated_docker() {
    const CASE: &str = "PLOYZ_PREPARE_DOCKER_SOCKET_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        let result = docker_step(&InstallPaths::at(&root)).await;
        assert_eq!(
            result,
            Err("checking Docker: Docker is installed but not running. Start it (systemctl start docker) and run this again.".into())
        );
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    let fixture = host(
        "docker-socket",
        UBUNTU_24,
        &[
            ("dockerd", "exit 0"),
            ("docker", "exit 1"),
            // docker.socket listens; docker.service failed the last time it ran.
            (
                "systemctl",
                "case \"$1 $2 $3\" in \"is-active --quiet docker.socket\"|\"is-failed --quiet docker.service\") exit 0 ;; *) exit 3 ;; esac",
            ),
        ],
    );
    let root = fixture.path();
    run_contract_child_with_environment(
        "installer::prepare::tests::docker_step_asks_a_socket_activated_docker",
        root,
        OsString::from(CASE),
        OsString::from("docker-socket"),
        "docker-socket",
        [],
    );
    assert!(
        read_log(root).contains("docker version"),
        "{}",
        read_log(root)
    );
}

#[tokio::test]
async fn a_step_left_installing_is_redone_and_recorded() {
    let fixture = fixture("interrupted");
    let paths = InstallPaths::at(fixture.path());
    fs::create_dir_all(prepare_dir(&paths)).unwrap();
    let started = Some(now());
    Job {
        path: state_path(&paths),
        state: PrepState {
            docker: Timed {
                step: DockerStep::Installing,
                started_at: started.clone(),
                finished_at: None,
            },
            zfs: Timed {
                step: ZfsStep::Installing,
                started_at: started,
                finished_at: None,
            },
            ..PrepState::default()
        },
    }
    .save()
    .unwrap();
    let ran = Cell::new(false);
    prepare_at(&paths, true, async || {
        ran.set(true);
        Ok(())
    })
    .await
    .unwrap();
    assert!(ran.get());
    let state = read_state(fixture.path());
    assert_eq!(state.interrupted, ["docker", "zfs"]);
    assert_eq!(state.docker.step, DockerStep::Ready);
    assert_eq!(state.zfs.step, ZfsStep::Off);
}

#[test]
fn a_package_install_takes_over_the_build_preparation_claimed() {
    let fixture = fixture("forget");
    let paths = InstallPaths::at(fixture.path());
    forget_zfs_build(&paths).unwrap();
    assert!(!prepare_dir(&paths).exists());

    fs::create_dir_all(prepare_dir(&paths)).unwrap();
    let mut job = claimed_build(&paths, &fixture.path().join("usr/src/zfs-2.1.11"));
    job.state.added_contrib_source = true;
    job.state.zfs.step = ZfsStep::Ready;
    job.save().unwrap();
    forget_zfs_build(&paths).unwrap();
    let state = read_state(fixture.path());
    assert!(state.added_packages.is_empty());
    assert_eq!(state.zfs_build, None);
    assert!(!state.added_contrib_source);
    assert_eq!(state.zfs.step, ZfsStep::Ready);
}
