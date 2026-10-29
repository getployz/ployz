//! Departure runs on every path that resets this Machine or installs it afresh, and a
//! plain Deploy mounts a Volume only on its writer.

use std::collections::BTreeMap;

use ployz_core::{
    AdvertisedEndpoint, DockerVolume, EnrollAs, EnrollRequest, Enrolled, LeaveRequest,
    LocalMachinePhase, Machine, MachineId, MachineName, Registered, RpcErrorCode, VolumeGen,
};
use serde_json::json;

use super::{Error, LocalMachine};
use crate::{
    corrosion::{AdminClient, fake_cluster},
    docker::test_support::{FakeDocker, fake_runtime_with, provisioned_source, spec_with_sources},
    machine::{LocalMachineStore, RecordOwner},
    storage::test_support::FakePlugin,
};

struct Harness {
    local: LocalMachine,
    plugin: FakePlugin,
    docker: FakeDocker,
    data_dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

fn initialize_request() -> EnrollRequest {
    EnrollRequest {
        role: EnrollAs::Founder {
            initial_policy: Default::default(),
            name: MachineName::parse("local").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

/// A Machine over a fake plugin that demotes `data`, a fake Docker, a fake Cluster, and
/// ZFS ready.
async fn harness(initialized: bool) -> Harness {
    let data_dir = std::env::temp_dir().join(format!("ployzd-departure-{}", MachineId::random()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    if initialized {
        store.enroll(initialize_request()).unwrap();
    }
    let plugin = FakePlugin::default();
    for route in ["Storage.Demote", "Storage.Seal"] {
        plugin.reply(
            route,
            json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
        );
    }
    let (client, _server) = plugin.serve(&data_dir);
    let (runtime, docker) = fake_runtime_with(FakeDocker::default()).await;
    let (replicated, _cluster) = fake_cluster::store().await;
    let owner = RecordOwner::spawn(store).unwrap();
    let local = LocalMachine::new(owner)
        .with_containers(Some(runtime))
        .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))))
        .with_plugin(client)
        // `true` lists no pools, which reads as ZFS ready whatever the host has.
        .with_zpool("true");
    Harness {
        local,
        plugin,
        docker,
        data_dir,
    }
}

fn forgot_data(docker: &FakeDocker) -> bool {
    docker
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|(method, path)| method == "DELETE" && path.ends_with("/volumes/data"))
}

fn join_request(record: &crate::machine::LocalMachineRecord) -> EnrollRequest {
    let assigned_machine = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: record.id(),
        name: MachineName::parse("joining").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        public_key: record.private_key().public_key(),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
        build_concurrency: None,
    };
    let mut peer = assigned_machine.clone();
    peer.id = MachineId::random();
    EnrollRequest {
        role: EnrollAs::Member {
            registration: Registered {
                assigned_machine,
                visible_peers: vec![peer],
                target_versions: BTreeMap::new(),
                sealing_key: None,
                devices: Vec::new(),
            },
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

#[tokio::test]
async fn leave_departs_storage_before_the_phase_commits() {
    let test = harness(true).await;
    test.local.leave(LeaveRequest::default()).await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Demote"]);
    assert!(forgot_data(&test.docker));
    assert_eq!(test.local.record().phase(), LocalMachinePhase::Resetting);
}

#[tokio::test]
async fn fence_departs_keeping_every_copy() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Seal",
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
    );
    test.local.fence_forgotten().await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Seal"]);
    assert!(forgot_data(&test.docker));
    assert_eq!(test.local.record().phase(), LocalMachinePhase::Resetting);
}

#[tokio::test]
async fn leave_forgets_every_volume_floor() {
    let test = harness(true).await;
    let containers = test.local.containers.as_ref().unwrap();
    let data = "data".parse().unwrap();
    let raised = containers
        .floor_turn()
        .await
        .admit(&data, VolumeGen::new(7))
        .await
        .unwrap();
    assert_eq!(raised, Ok(()));

    test.local.leave(LeaveRequest::default()).await.unwrap();
    let after = containers
        .floor_turn()
        .await
        .admit(&data, VolumeGen::new(0))
        .await
        .unwrap();
    assert_eq!(after, Ok(()));
}

#[tokio::test]
async fn initialize_departs_storage_a_prior_identity_left_behind_keeping_every_copy() {
    let test = harness(false).await;
    test.local.enroll(initialize_request()).await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Seal"]);
    assert!(forgot_data(&test.docker));
}

/// A forgotten Server that fenced keeps its mirrors, and one may be a Volume's last copy.
/// Joining it again must not destroy them (#3591): the join departs through the seal, as
/// the fence did, never through the demote that destroys pulled mirrors.
#[tokio::test]
async fn join_departs_storage_a_prior_identity_left_behind_keeping_every_mirror() {
    let test = harness(false).await;
    let request = join_request(&test.local.record());
    test.local.enroll(request).await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Seal"]);
    assert!(forgot_data(&test.docker));
}

#[tokio::test]
async fn a_failed_departure_keeps_the_machine_in_place() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Demote",
        json!({"Err": {"code": "internal", "message": "zfs rename failed", "details": null}}),
    );
    let error = test.local.leave(LeaveRequest::default()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Cleanup(message) if message.contains("zfs rename failed")),
        "{error}"
    );
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
    assert!(!forgot_data(&test.docker));
}

#[tokio::test]
async fn a_departure_with_a_stuck_copy_forgets_the_demoted_and_fails_naming_it() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Demote",
        json!({"Ok": {"demoted": ["data"], "stuck": ["seed"], "failed": []}}),
    );
    let error = test.local.leave(LeaveRequest::default()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Cleanup(message) if message.contains("seed")),
        "{error}"
    );
    assert!(forgot_data(&test.docker));
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
}

#[tokio::test]
async fn a_name_docker_keeps_still_forgets_the_rest_and_reports_beside_the_stuck() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Demote",
        json!({"Ok": {"demoted": ["held", "data"], "stuck": ["seed"], "failed": []}}),
    );
    test.docker.volume_users.lock().unwrap().insert(
        "held".into(),
        vec![json!({"Id": "a".repeat(64), "Labels": {}})],
    );
    let error = test.local.leave(LeaveRequest::default()).await.unwrap_err();
    let Error::Cleanup(message) = &error else {
        panic!("{error}");
    };
    assert!(message.contains("Docker kept Volume held"), "{message}");
    assert!(message.contains("seed did not stop"), "{message}");
    assert!(forgot_data(&test.docker));
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
}

#[tokio::test]
async fn a_departure_with_a_failed_volume_forgets_the_demoted_and_fails_with_its_reason() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Demote",
        json!({"Ok": {
            "demoted": ["data"],
            "stuck": ["web"],
            "failed": [{"name": "seed", "reason": "zfs rename failed: dataset is busy"}],
        }}),
    );
    let error = test.local.leave(LeaveRequest::default()).await.unwrap_err();
    let Error::Cleanup(message) = &error else {
        panic!("{error}");
    };
    assert!(message.contains("web did not stop"), "{message}");
    assert!(
        message.contains("seed did not depart: zfs rename failed: dataset is busy"),
        "{message}"
    );
    assert!(!message.contains("seed did not stop"), "{message}");
    assert!(forgot_data(&test.docker));
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
}

fn held(machine_id: MachineId, role: &str) -> DockerVolume {
    serde_json::from_value(json!({
        "id": {"machine_id": machine_id, "name": "app_data"},
        "storage": {
            "kind": "provisioned",
            "mountpoint": "/var/lib/ployz-mirror/app_data/fs",
            "bound_bytes": 1_073_741_824_u64,
            "used_bytes": 0,
            "role": role,
        },
    }))
    .unwrap()
}

/// A participating Machine whose Cluster knows `other` as a peer named `fsn-2`.
async fn participating() -> (Harness, MachineId) {
    let test = harness(true).await;
    let replicated = test.local.replicated().unwrap().clone();
    let me = test.local.record().machine().unwrap().clone();
    replicated.publish_local_machine(&me).await.unwrap();
    let other = MachineId::random();
    let mut peer = me.clone();
    peer.id = other;
    peer.name = MachineName::parse("fsn-2").unwrap();
    replicated.publish_local_machine(&peer).await.unwrap();
    test.plugin
        .reply("Storage.Prepare", json!({"Ok": ["app_data"]}));
    (test, other)
}

#[tokio::test]
async fn a_plain_deploy_mounts_only_on_the_writer() {
    let root = |readonly: bool| {
        json!({"Ok": {
            "copy": {"kind": "root", "readonly": readonly, "newest": null},
        }})
    };
    let slot = json!({"Ok": {
        "copy": {"kind": "slot", "slot": "1", "readonly": true, "newest": null, "resume_token": null},
    }});
    let nothing = json!({"Ok": {"copy": null}});
    /// Case name, the plugin's inspection, a copy elsewhere (role, Machine still listed),
    /// and the refusal text when the mount is refused.
    type Case = (
        &'static str,
        serde_json::Value,
        Option<(&'static str, bool)>,
        Option<&'static str>,
    );
    let cases: [Case; 7] = [
        ("fresh name", nothing.clone(), None, None),
        ("writable root", root(false), None, None),
        (
            "slot elsewhere",
            nothing.clone(),
            Some(("slot", true)),
            Some("Volume data has no writer; fsn-2 holds its only copy"),
        ),
        (
            "slot on a Machine the Cluster no longer lists",
            nothing.clone(),
            Some(("slot", false)),
            Some("no writer"),
        ),
        (
            "writer elsewhere",
            nothing,
            Some(("writer", true)),
            Some("Volume data's writer is on fsn-2, so this Server cannot mount it"),
        ),
        (
            "slot held",
            slot,
            None,
            Some("Volume data has no writer; local holds its only copy"),
        ),
        ("read-only root", root(true), None, Some("mid-run")),
    ];
    for (case, inspect, elsewhere, refusal) in cases {
        let (test, other) = participating().await;
        test.plugin.reply("Volume.Inspect", inspect);
        if let Some((role, listed)) = elsewhere {
            let holder = if listed { other } else { MachineId::random() };
            test.local
                .replicated()
                .unwrap()
                .publish_volume(&held(holder, role))
                .await
                .unwrap();
        }
        let spec = spec_with_sources(vec![provisioned_source("data", 1_073_741_824)]);
        let result = test
            .local
            .prepare_volumes(vec![ployz_core::ServiceStorageSpec::from(&spec)])
            .await;
        let prepared = test
            .plugin
            .routes_called()
            .contains(&"Storage.Prepare".to_owned());
        match refusal {
            None => {
                assert!(result.is_ok(), "{case}: {result:?}");
                assert!(prepared, "{case}: the plugin prepares an admitted mount");
            }
            Some(text) => {
                let Err(Error::StoragePreparation(error)) = result else {
                    panic!("{case}: {result:?}");
                };
                assert_eq!(error.code, RpcErrorCode::Conflict, "{case}: {error:?}");
                assert!(error.message.contains(text), "{case}: {}", error.message);
                // Only the operator knows whether `ployz volume restore` can run here.
                assert!(
                    !error.message.contains("restore"),
                    "{case}: {}",
                    error.message
                );
                assert!(!prepared, "{case}: a refused mount is never prepared");
            }
        }
    }
}

#[tokio::test]
async fn a_resumed_join_does_not_depart_again() {
    let test = harness(false).await;
    let request = join_request(&test.local.record());
    let first = test.local.enroll(request.clone()).await.unwrap();
    let again = test.local.enroll(request).await.unwrap();
    assert_eq!(again, first);
    assert_eq!(test.plugin.routes_called(), ["Storage.Seal"]);
}

#[tokio::test]
async fn a_refused_enroll_does_not_depart() {
    let test = harness(true).await;
    let mut request = initialize_request();
    if let EnrollAs::Founder { name, .. } = &mut request.role {
        *name = MachineName::parse("other").unwrap();
    }
    let refused = test.local.enroll(request).await.unwrap();
    assert!(matches!(refused, Enrolled::Refused { .. }), "{refused:?}");
    assert!(test.plugin.routes_called().is_empty());
    assert!(!forgot_data(&test.docker));
}

#[tokio::test]
async fn a_join_whose_departure_fails_stays_uninitialized_and_retries() {
    let test = harness(false).await;
    let request = join_request(&test.local.record());
    test.plugin.reply(
        "Storage.Seal",
        json!({"Err": {"code": "internal", "message": "zfs rename failed", "details": null}}),
    );
    let error = test.local.enroll(request.clone()).await.unwrap_err();
    assert!(matches!(error, Error::Cleanup(_)), "{error}");
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Uninitialized
    );

    test.plugin.reply(
        "Storage.Seal",
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
    );
    test.local.enroll(request).await.unwrap();
    assert_eq!(
        test.plugin.routes_called(),
        ["Storage.Seal", "Storage.Seal"]
    );
    assert_eq!(test.local.record().phase(), LocalMachinePhase::Joining);
}

/// While a Leave runs, this Machine's row says so, so a deploy plans no Ingress Proxy
/// here and the ingress check doesn't call it unchecked (#3277). The row goes with the
/// Leave.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leave_marks_its_row_leaving_until_the_row_goes() {
    let (test, _) = participating().await;
    let replicated = test.local.replicated().unwrap().clone();
    let me = test.local.record().machine().unwrap().clone();
    let seen_leaving = std::sync::Arc::new(std::sync::Mutex::new(None));
    test.plugin.on("Storage.Demote", {
        let (replicated, seen_leaving) = (replicated.clone(), seen_leaving.clone());
        move || {
            let row = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(replicated.machine(me.id.as_str()))
            });
            *seen_leaving.lock().unwrap() = Some(row.unwrap().map(|row| row.runtime.leaving));
        }
    });

    test.local.leave(LeaveRequest::default()).await.unwrap();

    assert_eq!(
        *seen_leaving.lock().unwrap(),
        Some(Some(true)),
        "the row says leaving while storage departs"
    );
    assert_eq!(replicated.machine(me.id.as_str()).await.unwrap(), None);
}

/// A Leave that fails leaves the Machine in place, and its row says so again.
#[tokio::test]
async fn a_failed_leave_unmarks_its_row() {
    let (test, _) = participating().await;
    let replicated = test.local.replicated().unwrap().clone();
    let me = test.local.record().machine().unwrap().clone();
    test.plugin.reply(
        "Storage.Demote",
        json!({"Err": {"code": "internal", "message": "zfs rename failed", "details": null}}),
    );

    test.local.leave(LeaveRequest::default()).await.unwrap_err();

    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
    assert_eq!(
        replicated.machine(me.id.as_str()).await.unwrap(),
        Some(me),
        "the row is as published, with no leaving mark"
    );
}

/// The mark updates a row in place: one the Cluster already forgot, as when this Machine
/// fences itself after a Forget, stays gone rather than coming back marked.
#[tokio::test]
async fn a_leaving_mark_never_brings_back_a_forgotten_row() {
    let (test, _) = participating().await;
    let replicated = test.local.replicated().unwrap().clone();
    let forgotten = MachineId::random();

    replicated
        .machine_publication()
        .await
        .mark_leaving(&forgotten, true)
        .await
        .unwrap();

    assert_eq!(replicated.machine(forgotten.as_str()).await.unwrap(), None);
}
