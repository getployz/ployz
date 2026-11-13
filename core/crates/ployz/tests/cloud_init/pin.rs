//! Whose approval a Server added with `ployz server add --standalone` accepts: the pin its
//! join carries comes only from this computer and an entry it reached itself.

use std::{collections::BTreeMap, os::unix::fs::OpenOptionsExt, path::Path, process::Output};

use ployz::context::{Config, Connection, Context};
use ployz_core::{DeviceKey, authority::Signer};

use super::harness::{JoinDaemon, founder_machine, registration, serve_machine};

/// The Device the entry trusts, named in what it answers.
fn entry_device() -> DeviceKey {
    Signer::from_seed(&[11; 32]).public().into()
}

/// Write `seed` as this computer's device key beside `config`, as `ployz device request` does.
fn own_device(config: &Path, seed: [u8; 32]) -> DeviceKey {
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(config.with_file_name("device.key"))
        .unwrap();
    std::io::Write::write_all(&mut file, &seed).unwrap();
    Signer::from_seed(&seed).public().into()
}

/// `ployz server add --standalone` of a fresh target through an entry reached over `route`,
/// with this computer's device key from `seed` when there is one.
async fn add(
    route: &str,
    seed: Option<[u8; 32]>,
) -> (Output, Option<DeviceKey>, JoinDaemon, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    registration.devices = vec![entry_device()];
    let entry = JoinDaemon::new(registration.clone()).with_id(founder.id);
    let target = JoinDaemon::new(registration);
    let entry_address = serve_machine(entry.clone()).await;
    let target_address = serve_machine(target.clone()).await;
    let root =
        std::env::temp_dir().join(format!("ployz-add-pin-{}", ployz_core::MachineId::random()));
    let config = root.join("config.yaml");
    let own = seed.map(|seed| own_device(&config, seed));
    let connection: Connection = route
        .replace("ENTRY", &entry_address.to_string())
        .parse()
        .unwrap();
    Config::new(
        &config,
        Some("prod".into()),
        BTreeMap::from([(
            "prod".into(),
            Context {
                connections: vec![connection],
            },
        )]),
    )
    .save()
    .unwrap();
    let output = super::harness::cli()
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "server",
            "add",
            "--standalone",
            &format!("ssh://root@{target_address}"),
            "--no-install",
            "--name",
            "joiner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
    (output, own, entry, target)
}

#[tokio::test]
async fn the_join_is_pinned_to_this_computers_device_and_the_entry_it_reached_itself() {
    let (output, own, _entry, target) = add("ssh://root@ENTRY", Some([12; 32])).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        target.join_request().pin.map(|pin| pin.devices),
        Some(vec![own.unwrap(), entry_device()])
    );
}

#[tokio::test]
async fn an_entry_reached_over_tcp_adds_none_of_its_devices_to_the_pin() {
    let (output, own, _entry, target) = add("tcp://ENTRY", Some([13; 32])).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        target.join_request().pin.map(|pin| pin.devices),
        Some(vec![own.unwrap()])
    );
}

/// With no device key, an entry that can't vouch for itself leaves nothing to pin the join
/// to, so the add stops before it asks the Cluster to admit anyone.
#[tokio::test]
async fn an_unpinnable_add_refuses_before_register() {
    let (output, _, entry, target) = add("tcp://ENTRY", None).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("this computer has no approved device key"),
        "{stderr}"
    );
    assert!(!entry.registered());
    assert_eq!(target.join_attempts(), 0);
}
