use ployz_core::host_preparation::PrepSteps;
use serde_json::{Value, json};

use super::host_report;
use ployz_core::probe_document::ProbeDocument;

/// What `ployzd install --probe --json` prints for a ready host: `assessment` first, keys sorted.
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

/// The same, for a host that stops short everywhere the probe can refuse or fail to read.
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

fn read(document: &str) -> Result<ProbeDocument, serde_json::Error> {
    serde_json::from_str(document)
}

fn report(document: &Value) -> Value {
    let probe: ProbeDocument = serde_json::from_value(document.clone()).unwrap();
    serde_json::to_value(host_report(
        probe,
        "host".into(),
        &PrepSteps::default(),
        false,
        false,
        None,
    ))
    .unwrap()
}

fn eligible() -> Value {
    serde_json::from_str(ELIGIBLE).unwrap()
}

fn ready_report() -> Value {
    json!({
        "hostname": "host",
        "os": {"id": "ubuntu", "version": "24.04"},
        "kernel": "6.8.0-41-generic",
        "arch": "x86_64",
        "virt": "vm:kvm",
        "cpuCount": 4,
        "memoryTotalBytes": 8_589_934_592_u64,
        "disks": [
            {
                "path": "/dev/vda",
                "byId": "virtio-root",
                "sizeBytes": 107_374_182_400_u64,
                "role": "root",
                "availableBytes": 85_899_345_920_u64,
            },
            {
                "path": "/dev/vdb",
                "byId": "virtio-data",
                "sizeBytes": 214_748_364_800_u64,
                "role": "empty",
            },
        ],
        "docker": {"kind": "present", "version": "29.1.0", "api": "1.53", "compatible": true},
        "cgroup": "v2",
        "zfs": {
            "kind": "eligible",
            "targets": [{"kind": "main_disk"}, {"kind": "disk", "byId": "virtio-data"}],
            "buildMinutes": 1,
        },
        "blockers": [],
        "warnings": [],
    })
}

#[test]
fn a_ready_hosts_document_reads_as_ployzd_prints_it() {
    assert_eq!(report(&eligible()), ready_report());
}

#[test]
fn a_refused_hosts_document_reads_as_ployzd_prints_it() {
    let refused: Value = serde_json::from_str(REFUSED).unwrap();
    assert_eq!(
        report(&refused),
        json!({
            "hostname": "host",
            "os": {"id": "ubuntu", "version": "24.04"},
            "kernel": "6.8.0-41-generic",
            "arch": "x86_64",
            "virt": "openvz",
            "cpuCount": 0,
            "memoryTotalBytes": 8_589_934_592_u64,
            "disks": [],
            "docker": {"kind": "absent"},
            "cgroup": "hybrid",
            "zfs": {
                "kind": "ineligible",
                "code": "openvz",
                "message": "OpenVZ does not allow this Machine to load the host ZFS kernel module",
            },
            "blockers": [
                {
                    "code": "docker_stopped",
                    "message": "Docker is installed but not running. Start it (systemctl start docker) and run this again.",
                },
                {"code": "no_systemd", "message": "Ployz requires systemd"},
            ],
            "warnings": [{
                "code": "cgroup_v1",
                "message": "This Server uses cgroup v1. Ployz expects cgroup v2, so container limits may not apply.",
            }],
        })
    );
}

/// A release before ployzd printed `os.name`, `secure_boot`, `systemd`, `root_filesystem`,
/// `zpool_present` and `unread`, and before Docker said more than its version.
#[test]
fn a_document_from_an_older_ployzd_reads() {
    let legacy = json!({
        "facts": {
            "os": {"id": "debian", "version_id": "12"},
            "kernel": "6.1.0",
            "arch": "aarch64",
            "virt": {"Lxc": {"privileged": false}},
            "cgroup": "V1",
            "cpu_count": 2,
            "memory_total_bytes": 4_294_967_296_u64,
            "disks": [{
                "path": "/dev/sda",
                "by_id": null,
                "size_bytes": 50_000_000_000_u64,
                "role": {"Root": {"available": 20_000_000_000_u64}},
            }],
            "docker": {"Present": {"version": "24.0.5", "api": "1.43"}},
        },
        "assessment": {
            "blockers": [{"code": "kernel_too_old", "message": "Kernel 4.4 is too old."}],
            "warnings": [],
            "zfs": {"Ineligible": {"reason": {"code": "zfs_off", "message": "Off."}}},
        },
    });
    assert_eq!(
        report(&legacy),
        json!({
            "hostname": "host",
            "os": {"id": "debian", "version": "12"},
            "kernel": "6.1.0",
            "arch": "aarch64",
            "virt": "lxc:unprivileged",
            "cpuCount": 2,
            "memoryTotalBytes": 4_294_967_296_u64,
            "disks": [{
                "path": "/dev/sda",
                "sizeBytes": 50_000_000_000_u64,
                "role": "root",
                "availableBytes": 20_000_000_000_u64,
            }],
            "docker": {"kind": "present", "version": "24.0.5", "api": "1.43", "compatible": true},
            "cgroup": "v1",
            "zfs": {"kind": "ineligible", "code": "zfs_off", "message": "Off."},
            "blockers": [{"code": "kernel_too_old", "message": "Kernel 4.4 is too old."}],
            "warnings": [],
        })
    );
}

fn at<'a>(document: &'a mut Value, pointer: &str) -> &'a mut Value {
    document.pointer_mut(pointer).unwrap()
}

/// Names a newer ployzd might add, at every level the reader reads.
#[test]
fn fields_a_newer_ployzd_adds_are_ignored() {
    let mut document = eligible();
    for pointer in [
        "",
        "/facts",
        "/facts/os",
        "/facts/root_filesystem",
        "/facts/disks/0",
        "/facts/disks/0/role/Root",
        "/facts/docker/Present",
        "/assessment",
        "/assessment/zfs/Eligible",
        "/assessment/zfs/Eligible/targets/1/Disk",
    ] {
        at(&mut document, pointer)
            .as_object_mut()
            .unwrap()
            .insert("added_later".into(), json!({"any": ["shape", 1, null]}));
    }
    let mut refused: Value = serde_json::from_str(REFUSED).unwrap();
    for pointer in [
        "/assessment/blockers/0",
        "/assessment/warnings/0",
        "/assessment/zfs/Ineligible",
        "/assessment/zfs/Ineligible/reason",
    ] {
        at(&mut refused, pointer)
            .as_object_mut()
            .unwrap()
            .insert("added_later".into(), json!(true));
    }
    assert_eq!(report(&document), ready_report());
    assert_eq!(
        report(&refused),
        report(&serde_json::from_str(REFUSED).unwrap())
    );
}

/// Each fact the probe couldn't read prints `null`; a ployzd that omits it reads the same.
#[test]
fn optional_facts_read_as_null_or_missing() {
    let unread = |document: &mut Value, omit: bool| {
        for pointer in [
            "/facts/cgroup",
            "/facts/cpu_count",
            "/facts/memory_total_bytes",
            "/facts/disks",
            "/facts/os/version_id",
        ] {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            let parent = at(document, parent).as_object_mut().unwrap();
            if omit {
                parent.remove(key);
            } else {
                parent.insert(key.into(), Value::Null);
            }
        }
    };
    let mut nulled = eligible();
    unread(&mut nulled, false);
    let mut omitted = eligible();
    unread(&mut omitted, true);
    let expected = {
        let mut expected = ready_report();
        for (pointer, value) in [
            ("/os/version", json!("")),
            ("/cgroup", json!("v1")),
            ("/cpuCount", json!(0)),
            ("/memoryTotalBytes", json!(0)),
            ("/disks", json!([])),
        ] {
            *at(&mut expected, pointer) = value;
        }
        expected
    };
    assert_eq!(report(&nulled), expected);
    assert_eq!(report(&omitted), expected);

    let mut disk = eligible();
    at(&mut disk, "/facts/disks/1")
        .as_object_mut()
        .unwrap()
        .remove("by_id");
    assert_eq!(report(&disk).pointer("/disks/1/byId"), None);
}

#[test]
fn a_document_this_release_cannot_read_is_an_error() {
    let mut cases = vec![String::new(), "[]".into(), "{}".into(), "null".into()];
    for pointer in [
        "/facts",
        "/assessment",
        "/facts/os",
        "/facts/os/id",
        "/facts/kernel",
        "/facts/arch",
        "/facts/virt",
        "/facts/docker",
        "/facts/disks/0/path",
        "/facts/disks/0/size_bytes",
        "/facts/disks/0/role",
        "/facts/docker/Present/version",
        "/facts/docker/Present/api",
        "/assessment/blockers",
        "/assessment/warnings",
        "/assessment/zfs",
        "/assessment/zfs/Eligible/targets",
        "/assessment/zfs/Eligible/prepare_minutes",
    ] {
        let mut document = eligible();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        at(&mut document, parent)
            .as_object_mut()
            .unwrap()
            .remove(key);
        cases.push(document.to_string());
    }
    for (pointer, wrong) in [
        ("/facts/kernel", json!(6)),
        ("/facts/cpu_count", json!("4")),
        ("/facts/cpu_count", json!(-1)),
        ("/facts/memory_total_bytes", json!(1.5)),
        ("/facts/cgroup", json!("V3")),
        ("/facts/cgroup", json!("v2")),
        ("/facts/virt", json!("Hypervisor")),
        ("/facts/virt", json!({"Vm": {}})),
        ("/facts/docker", json!("Weird")),
        ("/facts/docker", json!({"Present": {"version": "1"}})),
        ("/facts/disks", json!({})),
        ("/facts/disks/0/role", json!("Root")),
        ("/facts/disks/0/size_bytes", json!("100")),
        ("/assessment/blockers", json!({})),
        ("/assessment/blockers", json!([{"code": 5, "message": "m"}])),
        ("/assessment/blockers", json!([{"code": "c"}])),
        ("/assessment/zfs", json!("Maybe")),
        ("/assessment/zfs", json!({"Ineligible": {}})),
        ("/assessment/zfs/Eligible/targets/0", json!("SomeDisk")),
    ] {
        let mut document = eligible();
        *at(&mut document, pointer) = wrong;
        cases.push(document.to_string());
    }
    for case in cases {
        assert!(read(&case).is_err(), "{case}");
    }
}

#[test]
fn docker_is_compatible_until_a_docker_or_buildx_blocker() {
    let compatible = |blockers: Value| {
        let mut document = eligible();
        *at(&mut document, "/assessment/blockers") = blockers;
        report(&document)
            .pointer("/docker/compatible")
            .cloned()
            .unwrap()
    };
    let blocker = |code: &str| json!([{"code": code, "message": "m"}]);
    assert_eq!(compatible(json!([])), json!(true));
    assert_eq!(compatible(blocker("no_systemd")), json!(true));
    assert_eq!(compatible(blocker("docker_too_old")), json!(false));
    assert_eq!(compatible(blocker("docker_a_newer_code")), json!(false));
    assert_eq!(compatible(blocker("buildx_too_old")), json!(false));
    assert_eq!(compatible(blocker("buildx")), json!(true));
}

#[test]
fn a_stopped_or_socket_activated_docker_reports_as_absent() {
    for docker in ["Absent", "Stopped", "SocketActivated"] {
        let mut document = eligible();
        *at(&mut document, "/facts/docker") = json!(docker);
        assert_eq!(
            report(&document).pointer("/docker"),
            Some(&json!({"kind": "absent"}))
        );
    }
}
