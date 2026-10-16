use std::{env, ffi::OsString, fs, io, path::PathBuf};

use ployz_build::{MINIMUM_BUILDX, MINIMUM_DOCKER_API};
use serde_json::json;

use super::*;
use crate::installer::test_support::{fixture, run_contract_child_with_environment, write_script};

fn host() -> HostFacts {
    let root_filesystem = Filesystem {
        size_bytes: 100 * GIB,
        available_bytes: 80 * GIB,
    };
    HostFacts {
        os: OsRelease {
            id: "ubuntu".into(),
            name: "Ubuntu".into(),
            version_id: Some("24.04".into()),
        },
        kernel: "6.8.0-41-generic".into(),
        arch: "x86_64".into(),
        virt: Virtualization::Vm { kind: "kvm".into() },
        secure_boot: Some(false),
        cgroup: Some(CgroupVersion::V2),
        systemd: true,
        cpu_count: Some(4),
        memory_total_bytes: Some(8 * GIB),
        root_filesystem,
        disks: Some(vec![
            Disk {
                path: "/dev/vda".into(),
                by_id: Some("virtio-root".into()),
                size_bytes: 100 * GIB,
                role: DiskRole::Root {
                    available: root_filesystem.available_bytes,
                },
            },
            Disk {
                path: "/dev/vdb".into(),
                by_id: Some("virtio-data".into()),
                size_bytes: 200 * GIB,
                role: DiskRole::Empty,
            },
        ]),
        docker: DockerFacts::Present {
            version: "29.1.0".into(),
            api: "1.53".into(),
            buildx: Some("v0.37.1".into()),
            containerd_store: Some(true),
            error: None,
        },
        zpool_present: Some(false),
        unread: Vec::new(),
    }
}

fn codes(blockers: &[Blocker]) -> Vec<&'static str> {
    blockers.iter().map(|blocker| blocker.code).collect()
}

fn zfs_refusal(facts: &HostFacts) -> Option<&'static str> {
    zfs_reason(facts).map(|reason| reason.code)
}

fn zfs_reason(facts: &HostFacts) -> Option<Blocker> {
    match assess(facts).zfs {
        ZfsEligibility::Eligible { .. } => None,
        ZfsEligibility::Ineligible { reason } => Some(reason),
    }
}

#[test]
fn a_ready_host_has_no_blockers_and_both_disks_as_zfs_targets() {
    let assessment = assess(&host());
    assert!(assessment.blockers.is_empty(), "{:?}", assessment.blockers);
    assert!(assessment.warnings.is_empty(), "{:?}", assessment.warnings);
    let ZfsEligibility::Eligible {
        targets,
        prepare_minutes,
    } = assessment.zfs
    else {
        panic!("ZFS ineligible: {:?}", assessment.zfs);
    };
    assert_eq!(
        targets,
        [
            ZfsTarget::MainDisk,
            ZfsTarget::Disk {
                by_id: "virtio-data".into()
            }
        ]
    );
    assert_eq!(prepare_minutes, 1);
}

#[test]
fn a_stopped_docker_blocks() {
    let mut facts = host();
    facts.docker = DockerFacts::Stopped;
    assert_eq!(
        assess(&facts).blockers,
        [Blocker::new(
            "docker_stopped",
            "Docker is installed but not running. Start it (systemctl start docker) and run this again."
        )]
    );
}

#[test]
fn an_absent_docker_does_not_block() {
    let mut facts = host();
    facts.docker = DockerFacts::Absent;
    assert!(assess(&facts).blockers.is_empty());
}

#[test]
fn an_old_docker_engine_blocks() {
    let mut facts = host();
    facts.docker = DockerFacts::Present {
        version: "25.0.3".into(),
        api: "1.44".into(),
        buildx: None,
        containerd_store: None,
        error: None,
    };
    let blockers = assess(&facts).blockers;
    let [blocker] = blockers.as_slice() else {
        panic!("{blockers:?}");
    };
    assert_eq!(blocker.code, "docker_too_old");
    assert!(
        blocker
            .message
            .starts_with("Docker 25.0.3 is too old: Ployz needs Docker "),
        "{}",
        blocker.message
    );
}

#[test]
fn an_old_buildx_blocks() {
    let mut facts = host();
    facts.docker = DockerFacts::Present {
        version: "29.1.0".into(),
        api: "1.53".into(),
        buildx: Some("v0.17.1".into()),
        containerd_store: None,
        error: None,
    };
    let blockers = assess(&facts).blockers;
    let [blocker] = blockers.as_slice() else {
        panic!("{blockers:?}");
    };
    assert_eq!(blocker.code, "buildx_too_old");
    let (major, minor) = MINIMUM_BUILDX;
    assert!(
        blocker.message.starts_with(&format!(
            "Docker Buildx v0.17.1 is too old: Ployz needs Buildx {major}.{minor} or newer."
        )),
        "{}",
        blocker.message
    );
}

#[test]
fn docker_without_the_containerd_store_blocks() {
    let mut facts = host();
    facts.docker = DockerFacts::Present {
        version: "29.1.0".into(),
        api: "1.53".into(),
        buildx: Some("v0.37.1".into()),
        containerd_store: Some(false),
        error: None,
    };
    assert_eq!(
        codes(&assess(&facts).blockers),
        ["docker_containerd_store_off"]
    );
}

#[test]
fn a_docker_that_stopped_answering_blocks_with_its_error() {
    let mut facts = host();
    facts.docker = DockerFacts::Present {
        version: "29.2.0".into(),
        api: "1.53".into(),
        buildx: None,
        containerd_store: None,
        error: Some("checking Docker: docker: 'buildx' is not a docker command.".into()),
    };
    assert_eq!(
        assess(&facts).blockers,
        [Blocker::new(
            "docker_unreadable",
            "checking Docker: docker: 'buildx' is not a docker command."
        )]
    );
    assert_eq!(zfs_refusal(&facts), None);
}

#[test]
fn no_systemd_blocks() {
    let mut facts = host();
    facts.systemd = false;
    assert_eq!(
        assess(&facts).blockers,
        [Blocker::new("no_systemd", "Ployz requires systemd")]
    );
}

#[test]
fn a_root_disk_under_30_gib_blocks() {
    for (gib, blocked) in [(29, true), (31, false)] {
        let mut facts = host();
        if let Some(disk) = facts.disks.iter_mut().flatten().next() {
            disk.size_bytes = gib * GIB;
        }
        let blockers = assess(&facts).blockers;
        if blocked {
            assert_eq!(
                blockers,
                [Blocker::new(
                    "disk_too_small",
                    "Disk is 29 GiB. Ployz needs 30 GiB."
                )]
            );
        } else {
            assert!(blockers.is_empty(), "{gib} GiB: {blockers:?}");
        }
    }
}

#[test]
fn unread_disks_do_not_block() {
    let mut facts = host();
    facts.disks = None;
    let assessment = assess(&facts);
    assert!(assessment.blockers.is_empty());
    let ZfsEligibility::Eligible { targets, .. } = assessment.zfs else {
        panic!("ZFS ineligible: {:?}", assessment.zfs);
    };
    assert_eq!(targets, [ZfsTarget::MainDisk]);
}

#[test]
fn cgroup_v1_and_hybrid_warn_without_blocking() {
    for cgroup in [CgroupVersion::V1, CgroupVersion::Hybrid] {
        let mut facts = host();
        facts.cgroup = Some(cgroup);
        let assessment = assess(&facts);
        assert!(assessment.blockers.is_empty());
        assert_eq!(codes(&assessment.warnings), ["cgroup_v1"], "{cgroup:?}");
    }
}

#[test]
fn openvz_cannot_prepare_zfs() {
    let mut facts = host();
    facts.virt = Virtualization::OpenVz;
    assert_eq!(
        zfs_reason(&facts),
        Some(Blocker::new(
            "openvz",
            "OpenVZ does not allow this Machine to load the host ZFS kernel module"
        ))
    );
    assert!(assess(&facts).blockers.is_empty());
}

#[test]
fn an_openvz_container_is_read_from_proc() {
    let fixture = fixture("openvz");
    fs::create_dir_all(fixture.path().join("vz")).unwrap();
    let (virt, unread) = read_virtualization(fixture.path());
    assert!(matches!(virt, Virtualization::OpenVz), "{virt:?}");
    assert!(unread.is_none());
}

#[test]
fn only_unprivileged_lxc_cannot_prepare_zfs() {
    let mut facts = host();
    facts.virt = Virtualization::Lxc {
        privileged: Some(false),
    };
    assert_eq!(
        zfs_reason(&facts),
        Some(Blocker::new(
            "unprivileged_lxc",
            "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module"
        ))
    );
    facts.virt = Virtualization::Lxc {
        privileged: Some(true),
    };
    assert_eq!(zfs_refusal(&facts), None);
    facts.virt = Virtualization::Lxc { privileged: None };
    assert_eq!(zfs_refusal(&facts), Some("lxc_privilege_unknown"));
}

#[test]
fn zpool_does_not_run_while_the_zfs_module_is_unloaded() {
    let fixture = fixture("no-zfs-module");
    let (present, unread) = read_zpool_present(&fixture.path().join("sys/module/zfs"));
    assert_eq!(present, Some(false));
    assert!(unread.is_none());
}

#[test]
fn an_existing_zpool_neither_blocks_nor_stops_zfs() {
    let mut facts = host();
    facts.zpool_present = Some(true);
    assert!(assess(&facts).blockers.is_empty());
    assert_eq!(zfs_refusal(&facts), None);
}

#[test]
fn zfs_needs_a_supported_distribution() {
    let mut facts = host();
    facts.os = OsRelease {
        id: "fedora".into(),
        name: "Fedora Linux".into(),
        version_id: Some("41".into()),
    };
    let ZfsEligibility::Ineligible { reason } = assess(&facts).zfs else {
        panic!("Fedora was eligible");
    };
    assert_eq!(reason.code, "unsupported_os");
    assert!(
        reason.message.contains("this Server runs Fedora Linux 41."),
        "{}",
        reason.message
    );
}

#[test]
fn secure_boot_stops_zfs_only_where_the_module_is_built() {
    let debian = OsRelease {
        id: "debian".into(),
        name: "Debian".into(),
        version_id: Some("13".into()),
    };
    for (secure_boot, refusal) in [
        (Some(false), None),
        (
            Some(true),
            Some(Blocker::new(
                "secure_boot",
                "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for Debian. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
            )),
        ),
        (
            None,
            Some(Blocker::new(
                "secure_boot_unknown",
                "Couldn't tell whether Secure Boot is on, and under it this Server can't load the ZFS module Ployz builds for Debian. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
            )),
        ),
    ] {
        let mut facts = host();
        facts.secure_boot = secure_boot;
        assert_eq!(zfs_refusal(&facts), None, "Ubuntu, {secure_boot:?}");
        facts.os = OsRelease {
            id: debian.id.clone(),
            name: debian.name.clone(),
            version_id: debian.version_id.clone(),
        };
        assert_eq!(zfs_reason(&facts), refusal, "Debian, {secure_boot:?}");
    }
}

#[test]
fn zfs_validation_preserves_the_host_root_reserve() {
    let size_bytes = 100 * GIB;
    let reserve = ployz_core::storage_host_reserve(size_bytes);
    let mut facts = host();
    facts.root_filesystem = Filesystem {
        size_bytes,
        available_bytes: reserve + ZFS_SMOKE_BYTES - 1,
    };
    let ZfsEligibility::Ineligible { reason } = assess(&facts).zfs else {
        panic!("eligible without the reserve");
    };
    assert_eq!(
        reason,
        Blocker::new(
            ROOT_RESERVE,
            format!(
                "Host root has {} bytes available; ZFS validation needs {ZFS_SMOKE_BYTES} bytes while preserving the {reserve}-byte host-root reserve",
                reserve + ZFS_SMOKE_BYTES - 1
            )
        )
    );
    facts.root_filesystem.available_bytes = reserve + ZFS_SMOKE_BYTES;
    assert_eq!(zfs_refusal(&facts), None);
}

#[test]
fn lsblk_keeps_the_root_disk_and_empty_disks() {
    let listing = json!({"blockdevices": [
        {"name": "vda", "path": "/dev/vda", "size": 107_374_182_400_u64, "type": "disk", "fstype": null,
         "mountpoints": [null], "pkname": null, "children": [
            {"name": "vda1", "path": "/dev/vda1", "size": 107_373_133_824_u64, "type": "part",
             "fstype": "ext4", "mountpoints": ["/"], "pkname": "vda"}
        ]},
        {"name": "vdb", "path": "/dev/vdb", "size": "214748364800", "type": "disk", "fstype": null,
         "mountpoints": [null], "pkname": null},
        {"name": "vdc", "path": "/dev/vdc", "size": 1_073_741_824, "type": "disk", "fstype": "zfs_member",
         "mountpoints": [null], "pkname": null},
        {"name": "vdd", "path": "/dev/vdd", "size": 1_073_741_824, "type": "disk", "fstype": null,
         "mountpoints": [null], "pkname": null, "children": [
            {"name": "vdd1", "path": "/dev/vdd1", "size": 1_072_693_248, "type": "part",
             "fstype": null, "mountpoints": [null], "pkname": "vdd"}
        ]},
        {"name": "vde", "path": "/dev/vde", "size": 1_073_741_824, "type": "disk", "fstype": null,
         "pttype": "gpt", "mountpoints": [null], "pkname": null},
        {"name": "vdf", "path": "/dev/vdf", "size": "1.5T", "type": "disk", "fstype": null,
         "mountpoints": [null], "pkname": null},
        {"name": "sr0", "path": "/dev/sr0", "size": 1_048_576, "type": "rom", "fstype": null,
         "mountpoints": [null], "pkname": null}
    ]});
    let root = Filesystem {
        size_bytes: 100 * GIB,
        available_bytes: 60 * GIB,
    };
    let by_id = [(PathBuf::from("/dev/vdb"), "virtio-data".to_owned())];
    let (disks, errors) = disks_from_lsblk(&listing, root, &by_id);
    let summary: Vec<_> = disks
        .iter()
        .map(|disk| {
            (
                disk.path.as_str(),
                disk.by_id.as_deref(),
                disk.size_bytes,
                match disk.role {
                    DiskRole::Root { available } => Some(available),
                    DiskRole::Empty => None,
                },
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("/dev/vda", None, 107_374_182_400, Some(60 * GIB)),
            ("/dev/vdb", Some("virtio-data"), 214_748_364_800, None),
        ]
    );
    let errors: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(errors, [r#"list disks: /dev/vdf has size "1.5T""#]);
}

#[test]
fn lsblk_before_util_linux_2_37_reports_one_mountpoint() {
    let listing = json!({"blockdevices": [
        {"name": "sda", "path": "/dev/sda", "size": 107_374_182_400_u64, "type": "disk", "fstype": null,
         "mountpoint": null, "pkname": null, "children": [
            {"name": "sda1", "path": "/dev/sda1", "size": 107_373_133_824_u64, "type": "part",
             "fstype": "ext4", "mountpoint": "/", "pkname": "sda"}
        ]},
        {"name": "sdb", "path": "/dev/sdb", "size": 214_748_364_800_u64, "type": "disk", "fstype": null,
         "mountpoint": null, "pkname": null},
        {"name": "sdc", "path": "/dev/sdc", "size": 214_748_364_800_u64, "type": "disk", "fstype": null,
         "mountpoint": "/srv", "pkname": null}
    ]});
    let root = Filesystem {
        size_bytes: 100 * GIB,
        available_bytes: 60 * GIB,
    };
    let (disks, _) = disks_from_lsblk(&listing, root, &[]);
    let roles: Vec<_> = disks
        .iter()
        .map(|disk| {
            (
                disk.path.as_str(),
                matches!(disk.role, DiskRole::Root { .. }),
            )
        })
        .collect();
    assert_eq!(roles, [("/dev/sda", true), ("/dev/sdb", false)]);
}

#[test]
fn cgroup_layout_comes_from_the_mount_table() {
    let v2 = "cgroup2 /sys/fs/cgroup cgroup2 rw,nosuid 0 0\n";
    let v1 =
        "tmpfs /sys/fs/cgroup tmpfs ro 0 0\ncgroup /sys/fs/cgroup/memory cgroup rw,memory 0 0\n";
    let hybrid =
        "tmpfs /sys/fs/cgroup tmpfs ro 0 0\ncgroup2 /sys/fs/cgroup/unified cgroup2 rw 0 0\n";
    assert_eq!(cgroup_version(v2), Some(CgroupVersion::V2));
    assert_eq!(cgroup_version(v1), Some(CgroupVersion::V1));
    assert_eq!(cgroup_version(hybrid), Some(CgroupVersion::Hybrid));
    assert_eq!(cgroup_version("proc /proc proc rw 0 0\n"), None);
    let remounted = "tmpfs /sys/fs/cgroup tmpfs ro 0 0\ncgroup2 /sys/fs/cgroup cgroup2 rw 0 0\n";
    assert_eq!(cgroup_version(remounted), Some(CgroupVersion::V2));
}

#[test]
fn docker_and_buildx_must_be_at_least_the_minimum() {
    for (version, minimum, supported) in [
        ("1.44", MINIMUM_DOCKER_API, false),
        ("1.52", MINIMUM_DOCKER_API, false),
        ("1.53", MINIMUM_DOCKER_API, true),
        ("1.54", MINIMUM_DOCKER_API, true),
        ("", MINIMUM_DOCKER_API, false),
        ("0.17.1", MINIMUM_BUILDX, false),
        ("0.18.0", MINIMUM_BUILDX, true),
        ("0.37.1+ds1", MINIMUM_BUILDX, true),
    ] {
        assert_eq!(at_least(version, minimum), supported, "{version}");
    }
}

#[test]
fn secure_boot_reading_follows_test_root() {
    let fixture = fixture("secure-boot");
    let variable = fixture.path().join("efivars/SecureBoot");
    assert!(!read_secure_boot_variable(&variable).unwrap());
    fs::create_dir_all(fixture.path().join("efivars")).unwrap();
    for (bytes, enabled) in [([6, 0, 0, 0, 1], true), ([6, 0, 0, 0, 0], false)] {
        fs::write(&variable, bytes).unwrap();
        assert_eq!(read_secure_boot_variable(&variable).unwrap(), enabled);
    }
}

#[test]
fn an_unreadable_secure_boot_variable_is_an_error_not_off() {
    let fixture = fixture("secure-boot-unreadable");
    let variable = fixture.path().join("efivars/SecureBoot");
    fs::create_dir_all(&variable).unwrap();
    let error = read_secure_boot_variable(&variable).unwrap_err();
    assert!(
        matches!(&error, Error::Io { stage: "read Secure Boot state", source }
            if source.kind() != io::ErrorKind::NotFound),
        "{error:?}"
    );
}

#[test]
fn missing_proc_files_say_why_their_facts_are_null() {
    let fixture = fixture("proc-missing");
    let proc_dir = fixture.path();
    let stages = [
        read_cgroup(proc_dir).unwrap_err(),
        read_cpu_count(proc_dir).unwrap_err(),
        read_memory_total(proc_dir).unwrap_err(),
    ]
    .map(|error| error.to_string());
    assert_eq!(
        stages,
        ["read cgroup layout", "read CPU count", "read memory size"]
    );
    fs::create_dir_all(proc_dir.join("self")).unwrap();
    fs::write(proc_dir.join("self/mounts"), "proc /proc proc rw 0 0\n").unwrap();
    fs::write(proc_dir.join("cpuinfo"), "").unwrap();
    fs::write(proc_dir.join("meminfo"), "MemFree: 1 kB\n").unwrap();
    let messages = [
        read_cgroup(proc_dir).unwrap_err(),
        read_cpu_count(proc_dir).unwrap_err(),
        read_memory_total(proc_dir).unwrap_err(),
    ]
    .map(|error| error.to_string());
    assert_eq!(
        messages,
        [
            "read cgroup layout: no cgroup mount at /sys/fs/cgroup",
            "read CPU count: no processor lines in cpuinfo",
            "read memory size: no MemTotal line in meminfo",
        ]
    );
}

/// The probe reads a socket-activated Docker without touching its socket, which would start it,
/// keeps reading when `lsblk` is missing, and names every read it couldn't make.
#[test]
fn probe_contract() {
    const CASE: &str = "PLOYZ_PROBE_CONTRACT";
    if env::var(CASE).is_ok() {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        let facts = read_facts(&InstallPaths::at(&root)).unwrap();
        assert!(
            matches!(facts.docker, DockerFacts::SocketActivated),
            "{facts:?}"
        );
        assert_eq!(docker_blocker(&facts.docker), None);
        assert!(facts.disks.is_none(), "{facts:?}");
        assert_eq!(
            facts.unread,
            [
                "start systemd-detect-virt to detect virtualization: No such file or directory (os error 2)",
                "read cgroup layout: No such file or directory (os error 2)",
                "read CPU count: No such file or directory (os error 2)",
                "read memory size: No such file or directory (os error 2)",
                "start lsblk to list disks: No such file or directory (os error 2)",
                "read Docker version: docker.socket is listening but docker.service isn't running, and asking Docker would start it",
            ]
        );
        fs::write(root.join("child-completed"), "probe").unwrap();
        return;
    }

    let fixture = fixture("probe");
    let root = fixture.path();
    let commands = root.join("commands");
    fs::create_dir_all(&commands).unwrap();
    fs::write(
        root.join("os-release"),
        "NAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
    )
    .unwrap();
    write_script(&commands.join("dockerd"), "exit 0");
    write_script(
        &commands.join("docker"),
        r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/docker.log""#,
    );
    // docker.socket listens; docker.service is stopped.
    write_script(
        &commands.join("systemctl"),
        r#"[ "$1 $2 $3" = "is-active --quiet docker.socket" ]"#,
    );
    run_contract_child_with_environment(
        "installer::assess::tests::probe_contract",
        root,
        OsString::from(CASE),
        OsString::from("probe"),
        "probe",
        [],
    );
    assert!(!root.join("docker.log").exists());
}

/// A running Docker whose Buildx can't be started blocks the probe with the error install refuses
/// with, cause included.
#[test]
fn probe_docker_unreadable_contract() {
    const CASE: &str = "PLOYZ_PROBE_DOCKER_UNREADABLE_CONTRACT";
    const REASON: &str = "start host command: Exec format error (os error 8)";
    // Answers `docker version`, then empties itself so `docker buildx` can't start. PATH holds
    // only the stubs, so the shell's own `:` does it, not `rm`.
    const DOCKER: &str = r#"[ "$1" = version ] && echo "29.7.0 1.53"
: > "$0""#;
    if env::var(CASE).is_ok() {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        let docker = root.join("commands/docker");
        let facts = read_facts(&InstallPaths::at(&root)).unwrap();
        let DockerFacts::Present { error, .. } = &facts.docker else {
            panic!("{facts:?}");
        };
        assert_eq!(error.as_deref(), Some(REASON));
        let blocker = docker_blocker(&facts.docker).unwrap();
        assert_eq!(
            (blocker.code, blocker.message.as_str()),
            ("docker_unreadable", REASON)
        );
        write_script(&docker, DOCKER);
        let refusal = crate::installer::host::verify_docker().unwrap_err();
        assert_eq!(error_text(&refusal), REASON);
        fs::write(root.join("child-completed"), "docker-unreadable").unwrap();
        return;
    }

    let fixture = fixture("probe-docker-unreadable");
    let root = fixture.path();
    let commands = root.join("commands");
    fs::create_dir_all(&commands).unwrap();
    fs::write(
        root.join("os-release"),
        "NAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
    )
    .unwrap();
    write_script(&commands.join("dockerd"), "exit 0");
    write_script(&commands.join("docker"), DOCKER);
    run_contract_child_with_environment(
        "installer::assess::tests::probe_docker_unreadable_contract",
        root,
        OsString::from(CASE),
        OsString::from("docker-unreadable"),
        "docker-unreadable",
        [],
    );
}

/// A `docker.service` that failed behind a listening socket fails again when install asks, so the
/// probe blocks on it as stopped, still without running `docker`.
#[test]
fn probe_failed_docker_service_contract() {
    const CASE: &str = "PLOYZ_PROBE_FAILED_DOCKER_CONTRACT";
    if env::var(CASE).is_ok() {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        let facts = read_facts(&InstallPaths::at(&root)).unwrap();
        assert!(matches!(facts.docker, DockerFacts::Stopped), "{facts:?}");
        assert_eq!(
            docker_blocker(&facts.docker).map(|blocker| blocker.code),
            Some("docker_stopped")
        );
        assert!(
            !facts
                .unread
                .iter()
                .any(|reason| reason.starts_with("read Docker version")),
            "{:?}",
            facts.unread
        );
        fs::write(root.join("child-completed"), "failed-docker").unwrap();
        return;
    }

    let fixture = fixture("probe-failed-docker");
    let root = fixture.path();
    let commands = root.join("commands");
    fs::create_dir_all(&commands).unwrap();
    fs::write(
        root.join("os-release"),
        "NAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
    )
    .unwrap();
    write_script(&commands.join("dockerd"), "exit 0");
    write_script(
        &commands.join("docker"),
        r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/docker.log""#,
    );
    // docker.socket listens; docker.service has failed.
    write_script(
        &commands.join("systemctl"),
        r#"case "$1 $2 $3" in
"is-active --quiet docker.socket" | "is-failed --quiet docker.service") exit 0 ;;
*) exit 1 ;;
esac"#,
    );
    run_contract_child_with_environment(
        "installer::assess::tests::probe_failed_docker_service_contract",
        root,
        OsString::from(CASE),
        OsString::from("failed-docker"),
        "failed-docker",
        [],
    );
    assert!(!root.join("docker.log").exists());
}

/// An unreadable Secure Boot variable leaves the state unknown, as install reads it, even when
/// mokutil would say it's off.
#[test]
fn probe_secure_boot_unreadable_contract() {
    const CASE: &str = "PLOYZ_PROBE_SECURE_BOOT_CONTRACT";
    if env::var(CASE).is_ok() {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        let facts = read_facts(&InstallPaths::at(&root)).unwrap();
        assert_eq!(facts.secure_boot, None);
        assert!(
            facts
                .unread
                .iter()
                .any(|reason| reason == "read Secure Boot state: Is a directory (os error 21)"),
            "{:?}",
            facts.unread
        );
        assert_eq!(zfs_refusal(&facts), Some("secure_boot_unknown"));
        fs::write(root.join("child-completed"), "secure-boot").unwrap();
        return;
    }

    let fixture = fixture("probe-secure-boot");
    let root = fixture.path();
    let commands = root.join("commands");
    fs::create_dir_all(&commands).unwrap();
    fs::create_dir_all(root.join("efivars/SecureBoot")).unwrap();
    fs::write(
        root.join("os-release"),
        "NAME=\"Debian GNU/Linux\"\nID=debian\nVERSION_ID=\"12\"\n",
    )
    .unwrap();
    write_script(&commands.join("mokutil"), "echo 'SecureBoot disabled'");
    run_contract_child_with_environment(
        "installer::assess::tests::probe_secure_boot_unreadable_contract",
        root,
        OsString::from(CASE),
        OsString::from("secure-boot"),
        "secure-boot",
        [],
    );
}
