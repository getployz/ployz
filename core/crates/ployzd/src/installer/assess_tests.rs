use std::{fs, path::PathBuf};

use ployz_build::{MINIMUM_BUILDX, MINIMUM_DOCKER_API};
use serde_json::json;

use super::*;
use crate::installer::test_support::fixture;

/// An Ubuntu VM with a current Docker, a 100 GiB root disk and one empty disk.
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
        },
        zpool_present: false,
    }
}

fn codes(blockers: &[Blocker]) -> Vec<&'static str> {
    blockers.iter().map(|blocker| blocker.code).collect()
}

fn zfs_refusal(facts: &HostFacts) -> Option<&'static str> {
    match assess(facts).zfs {
        ZfsEligibility::Eligible { .. } => None,
        ZfsEligibility::Ineligible { reason } => Some(reason.code),
    }
}

#[test]
fn a_ready_host_has_no_blockers_and_both_disks_as_zfs_targets() {
    let assessment = assess(&host());
    assert!(assessment.blockers.is_empty(), "{:?}", assessment.blockers);
    assert!(assessment.warnings.is_empty(), "{:?}", assessment.warnings);
    let ZfsEligibility::Eligible {
        targets,
        build_minutes,
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
    assert_eq!(build_minutes, 1);
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
    };
    assert_eq!(
        codes(&assess(&facts).blockers),
        ["docker_containerd_store_off"]
    );
}

#[test]
fn no_systemd_blocks() {
    let mut facts = host();
    facts.systemd = false;
    assert_eq!(
        assess(&facts).blockers,
        [Blocker::new(
            "no_systemd",
            "This Server doesn't run systemd. Ployz needs systemd to run its daemon."
        )]
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
                    "Disk is 29 GB. Ployz needs 30 GB."
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
    assert_eq!(zfs_refusal(&facts), Some("openvz"));
    assert!(assess(&facts).blockers.is_empty());
}

#[test]
fn only_unprivileged_lxc_cannot_prepare_zfs() {
    let mut facts = host();
    facts.virt = Virtualization::Lxc { privileged: false };
    assert_eq!(zfs_refusal(&facts), Some("unprivileged_lxc"));
    facts.virt = Virtualization::Lxc { privileged: true };
    assert_eq!(zfs_refusal(&facts), None);
}

#[test]
fn an_existing_zpool_neither_blocks_nor_stops_zfs() {
    let mut facts = host();
    facts.zpool_present = true;
    let assessment = assess(&facts);
    assert!(assessment.blockers.is_empty());
    assert!(matches!(assessment.zfs, ZfsEligibility::Eligible { .. }));
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
        (Some(true), Some("secure_boot")),
        (None, Some("secure_boot_unknown")),
    ] {
        let mut facts = host();
        facts.secure_boot = secure_boot;
        assert_eq!(zfs_refusal(&facts), None, "Ubuntu, {secure_boot:?}");
        facts.os = OsRelease {
            id: debian.id.clone(),
            name: debian.name.clone(),
            version_id: debian.version_id.clone(),
        };
        assert_eq!(zfs_refusal(&facts), refusal, "Debian, {secure_boot:?}");
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
        {"name": "sr0", "path": "/dev/sr0", "size": 1_048_576, "type": "rom", "fstype": null,
         "mountpoints": [null], "pkname": null}
    ]});
    let root = Filesystem {
        size_bytes: 100 * GIB,
        available_bytes: 60 * GIB,
    };
    let by_id = [(PathBuf::from("/dev/vdb"), "virtio-data".to_owned())];
    let disks = disks_from_lsblk(&listing, root, &by_id);
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
    assert_eq!(read_secure_boot(&variable), Some(false));
    fs::create_dir_all(fixture.path().join("efivars")).unwrap();
    for (bytes, enabled) in [([6, 0, 0, 0, 1], true), ([6, 0, 0, 0, 0], false)] {
        fs::write(&variable, bytes).unwrap();
        assert_eq!(read_secure_boot(&variable), Some(enabled));
    }
}
