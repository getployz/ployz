//! Departure runs on every path that resets this Machine or installs it afresh, and a
//! plain Deploy mounts a Volume only on its writer.

use std::collections::BTreeMap;

use ployz_core::{
    AdvertisedEndpoint, DockerVolume, InitializeRequest, JoinRequest, LocalMachinePhase, Machine,
    MachineId, MachineName, Registered, RemoveLocalMachineRequest, RpcErrorCode,
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

fn initialize_request() -> InitializeRequest {
    InitializeRequest {
        initial_policy: Default::default(),
        name: MachineName::parse("local").unwrap(),
        cluster_network: "10.210.0.0/16".parse().unwrap(),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        wireguard_mtu: None,
    }
}

/// A Machine over a fake plugin that demotes `data`, a fake Docker, and a fake Cluster.
async fn harness(initialized: bool) -> Harness {
    let data_dir = std::env::temp_dir().join(format!("ployzd-departure-{}", MachineId::random()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    if initialized {
        store.initialize(initialize_request()).unwrap();
    }
    let plugin = FakePlugin::default();
    plugin.reply("Storage.Demote", json!({"Ok": ["data"]}));
    let (client, _server) = plugin.serve(&data_dir);
    let (runtime, docker) = fake_runtime_with(FakeDocker::default()).await;
    let (replicated, _cluster) = fake_cluster::store().await;
    let owner = RecordOwner::spawn(store).unwrap();
    let local = LocalMachine::new(owner)
        .with_containers(Some(runtime))
        .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))))
        .with_plugin(client);
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

fn join_request(record: &crate::machine::LocalMachineRecord) -> JoinRequest {
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
    JoinRequest {
        registration: Registered {
            assigned_machine,
            visible_peers: vec![peer],
            target_versions: BTreeMap::new(),
        },
        wireguard_mtu: None,
    }
}

#[tokio::test]
async fn reset_departs_storage_before_the_phase_commits() {
    let test = harness(true).await;
    test.local.reset().await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Demote"]);
    assert!(forgot_data(&test.docker));
    assert_eq!(test.local.record().phase(), LocalMachinePhase::Resetting);
}

#[tokio::test]
async fn local_removal_departs_storage_before_the_phase_commits() {
    let test = harness(true).await;
    test.local
        .remove_local(RemoveLocalMachineRequest {
            restart_on_cleanup_failure: false,
        })
        .await
        .unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Demote"]);
    assert!(forgot_data(&test.docker));
    assert_eq!(test.local.record().phase(), LocalMachinePhase::Resetting);
}

#[tokio::test]
async fn initialize_departs_storage_a_prior_identity_left_behind() {
    let test = harness(false).await;
    test.local.initialize(initialize_request()).await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Demote"]);
    assert!(forgot_data(&test.docker));
}

#[tokio::test]
async fn join_departs_storage_a_prior_identity_left_behind() {
    let test = harness(false).await;
    let request = join_request(&test.local.record());
    test.local.join(request).await.unwrap();
    assert_eq!(test.plugin.routes_called(), ["Storage.Demote"]);
    assert!(forgot_data(&test.docker));
}

#[tokio::test]
async fn a_failed_departure_keeps_the_machine_in_place() {
    let test = harness(true).await;
    test.plugin.reply(
        "Storage.Demote",
        json!({"Err": {"code": "internal", "message": "zfs rename failed", "details": null}}),
    );
    let error = test.local.reset().await.unwrap_err();
    assert!(
        matches!(&error, Error::Cleanup(message) if message.contains("zfs rename failed")),
        "{error}"
    );
    assert_eq!(
        test.local.record().phase(),
        LocalMachinePhase::Participating
    );
    assert!(!forgot_data(&test.docker));

    let error = test
        .local
        .remove_local(RemoveLocalMachineRequest {
            restart_on_cleanup_failure: false,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Cleanup(_)), "{error}");
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
    let root = |writer: &str, cycle: &str| {
        json!({"Ok": {
            "copy": {"kind": "root", "writer": {"phase": writer}, "readonly": false, "newest": null},
            "lease": {"lease": 2, "pos": {"seq": 4, "round": 0, "sub": 0}, "cycle": cycle},
        }})
    };
    let slot = json!({"Ok": {
        "copy": {"kind": "slot", "mirror": {"phase": "idle"}, "readonly": true, "newest": null, "resume_token": null},
        "lease": null,
    }});
    let nothing = json!({"Ok": {"copy": null, "lease": null}});
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
        ("idle writer", root("idle", "closed"), None, None),
        (
            "slot elsewhere",
            nothing.clone(),
            Some(("slot", true)),
            Some("ployz volume restore app_data --from fsn-2"),
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
            Some("no writer"),
        ),
        ("slot held", slot, None, Some("no writer")),
        ("open record", root("idle", "open"), None, Some("mid-run")),
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
                assert!(!prepared, "{case}: a refused mount is never prepared");
            }
        }
    }
}
