use serde_json::json;

use super::*;

const ELIGIBLE: &str = r#"{
  "assessment": {
    "blockers": [],
    "warnings": [],
    "zfs": {
      "Eligible": {
        "prepare_minutes": 1,
        "targets": [
          "MainDisk",
          {
            "Disk": {
              "by_id": "virtio-data"
            }
          }
        ]
      }
    }
  },
  "facts": {
    "arch": "x86_64",
    "cgroup": "V2",
    "cpu_count": 4,
    "disks": [
      {
        "by_id": "virtio-root",
        "path": "/dev/vda",
        "role": {
          "Root": {
            "available": 85899345920
          }
        },
        "size_bytes": 107374182400
      },
      {
        "by_id": "virtio-data",
        "path": "/dev/vdb",
        "role": "Empty",
        "size_bytes": 214748364800
      }
    ],
    "docker": {
      "Present": {
        "api": "1.53",
        "buildx": "v0.37.1",
        "containerd_store": true,
        "error": null,
        "version": "29.1.0"
      }
    },
    "kernel": "6.8.0-41-generic",
    "memory_total_bytes": 8589934592,
    "os": {
      "id": "ubuntu",
      "name": "Ubuntu",
      "version_id": "24.04"
    },
    "root_filesystem": {
      "available_bytes": 85899345920,
      "size_bytes": 107374182400
    },
    "secure_boot": false,
    "systemd": true,
    "unread": [],
    "virt": {
      "Vm": {
        "kind": "kvm"
      }
    },
    "zpool_present": false
  }
}"#;

const REFUSED: &str = r#"{
  "assessment": {
    "blockers": [
      {
        "code": "docker_stopped",
        "message": "Docker is installed but not running. Start it (systemctl start docker) and run this again."
      },
      {
        "code": "no_systemd",
        "message": "Ployz requires systemd"
      }
    ],
    "warnings": [
      {
        "code": "cgroup_v1",
        "message": "This Server uses cgroup v1. Ployz expects cgroup v2, so container limits may not apply."
      }
    ],
    "zfs": {
      "Ineligible": {
        "reason": {
          "code": "openvz",
          "message": "OpenVZ does not allow this Machine to load the host ZFS kernel module"
        }
      }
    }
  },
  "facts": {
    "arch": "x86_64",
    "cgroup": "Hybrid",
    "cpu_count": null,
    "disks": null,
    "docker": "Stopped",
    "kernel": "6.8.0-41-generic",
    "memory_total_bytes": 8589934592,
    "os": {
      "id": "ubuntu",
      "name": "Ubuntu",
      "version_id": "24.04"
    },
    "root_filesystem": {
      "available_bytes": 85899345920,
      "size_bytes": 107374182400
    },
    "secure_boot": null,
    "systemd": false,
    "unread": [
      "read CPU count: no processor lines in cpuinfo"
    ],
    "virt": "OpenVz",
    "zpool_present": null
  }
}"#;

/// The bytes ployzd prints are the bytes of the document it reads back.
#[test]
fn a_document_prints_as_ployzd_always_has() {
    for printed in [ELIGIBLE, REFUSED] {
        let document: ProbeDocument = serde_json::from_str(printed).unwrap();
        assert_eq!(document.to_json().unwrap(), printed);
    }
}

#[test]
fn a_document_reads_back_what_was_written() {
    let document = ProbeDocument {
        facts: HostFacts {
            os: OsRelease {
                id: "amzn".into(),
                name: "Amazon Linux".into(),
                version_id: Some("2023".into()),
            },
            kernel: "6.1.0".into(),
            arch: "aarch64".into(),
            virt: Virtualization::Lxc { privileged: None },
            secure_boot: None,
            cgroup: Some(CgroupVersion::Hybrid),
            systemd: true,
            cpu_count: Some(8),
            memory_total_bytes: None,
            root_filesystem: Filesystem {
                size_bytes: 10,
                available_bytes: 4,
            },
            disks: Some(vec![Disk {
                path: "/dev/nvme1n1".into(),
                by_id: None,
                size_bytes: 3,
                role: DiskRole::Empty,
            }]),
            docker: DockerFacts::Present {
                version: "29.1.0".into(),
                api: "1.53".into(),
                buildx: None,
                containerd_store: Some(false),
                error: Some("docker buildx version failed".into()),
            },
            zpool_present: Some(true),
            unread: vec!["why".into()],
        },
        assessment: Assessment {
            blockers: vec![Blocker::new(BlockerCode::DockerUnreadable, "m")],
            warnings: vec![Blocker::new(BlockerCode::from("from_the_future"), "w")],
            zfs: ZfsEligibility::Eligible {
                targets: vec![ZfsTarget::MainDisk, ZfsTarget::Disk { by_id: "x".into() }],
                prepare_minutes: 20,
            },
        },
    };
    let printed = document.to_json().unwrap();
    assert_eq!(
        serde_json::from_str::<ProbeDocument>(&printed).unwrap(),
        document
    );
}

#[test]
fn a_blocker_code_keeps_the_text_of_one_this_release_does_not_name() {
    let known = [
        "docker_stopped",
        "docker_too_old",
        "docker_unreadable",
        "docker_containerd_store_off",
        "buildx_too_old",
        "no_systemd",
        "disk_too_small",
        "cgroup_v1",
        "unsupported_os",
        "secure_boot",
        "secure_boot_unknown",
        "openvz",
        "unprivileged_lxc",
        "lxc_privilege_unknown",
        "root_reserve",
        "device_not_empty",
    ];
    for code in known {
        let parsed: BlockerCode = serde_json::from_value(json!(code)).unwrap();
        assert!(!matches!(parsed, BlockerCode::Unknown(_)), "{code}");
        assert_eq!(parsed.as_str(), code);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(code));
    }
    let newer: BlockerCode = serde_json::from_value(json!("kernel_too_old")).unwrap();
    assert_eq!(newer, BlockerCode::Unknown("kernel_too_old".into()));
    assert_eq!(newer.as_str(), "kernel_too_old");
    assert_eq!(
        serde_json::to_value(&newer).unwrap(),
        json!("kernel_too_old")
    );
    assert!(serde_json::from_value::<BlockerCode>(json!(5)).is_err());
}

/// An older ployzd's document lacks fields this release's type carries; each reads as its default.
#[test]
fn a_document_from_an_older_ployzd_reads_with_defaults() {
    let document: ProbeDocument = serde_json::from_value(json!({
        "facts": {
            "os": {"id": "ubuntu", "version_id": "24.04"},
            "kernel": "6.8.0",
            "arch": "x86_64",
            "virt": "BareMetal",
            "docker": "Absent",
        },
        "assessment": {
            "blockers": [],
            "warnings": [],
            "zfs": {"Eligible": {"targets": [], "prepare_minutes": 5}},
        },
    }))
    .unwrap();
    let facts = document.facts;
    assert_eq!(facts.os.name, "");
    assert!(!facts.systemd);
    assert_eq!(facts.root_filesystem, Filesystem::default());
    assert!(facts.unread.is_empty());
    assert_eq!(
        (
            facts.secure_boot,
            facts.cgroup,
            facts.cpu_count,
            facts.disks
        ),
        (None, None, None, None)
    );
}

#[test]
fn an_os_release_names_itself() {
    let os = |id: &str, version_id: Option<&str>| OsRelease {
        id: id.into(),
        name: "Amazon Linux".into(),
        version_id: version_id.map(Into::into),
    };
    assert_eq!(os("amzn", Some("2023")).display(), "Amazon Linux 2023");
    assert_eq!(os("amzn", None).display(), "Amazon Linux");
    assert!(os("amzn", None).is_amazon_linux());
    assert!(!os("ubuntu", None).is_amazon_linux());
}
