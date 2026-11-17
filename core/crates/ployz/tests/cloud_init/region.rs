//! `ployz region ls` and `ployz region rename`: a region's name comes from the Store, and
//! its Servers from their `region` labels.

use super::*;
use ployz_core::{Machine, MachineId, MachineName};

fn in_region(mut machine: Machine, id: char, name: &str, region: Option<&str>) -> Machine {
    machine.id = MachineId::parse(id.to_string().repeat(32)).unwrap();
    machine.name = MachineName::parse(name).unwrap();
    machine.labels = region
        .map(|region| [("region".parse().unwrap(), region.parse().unwrap())].into())
        .unwrap_or_default();
    machine
}

async fn region(store: &str, args: &[&str]) -> (String, String) {
    let output = harness::cli()
        .env("PLOYZ_STORE", store)
        .arg("region")
        .args(args)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{args:?}: {stderr}");
    eprintln!("$ ployz region {}\n{stdout}{stderr}", args.join(" "));
    (stdout, stderr)
}

#[tokio::test]
async fn region_ls_shows_each_region_by_name_with_its_servers() {
    let mut registration = registration();
    let machine = founder_machine();
    registration.visible_peers = vec![
        in_region(machine.clone(), 'c', "fsn-1", Some("default")),
        in_region(machine.clone(), 'd', "syd-1", Some("SYD")),
        in_region(machine.clone(), 'e', "syd-2", Some("syd")),
        in_region(machine, 'f', "old-1", None),
    ];
    let address = serve_machine(JoinDaemon::new(registration)).await;
    let directory = tempfile::tempdir().unwrap();
    let store = format!("sqlite:{}", directory.path().join("store.db").display());
    let connect = format!("ssh://root@{address}");

    let (renamed, _) = region(&store, &["rename", "default", "Falkenstein"]).await;
    assert_eq!(
        renamed,
        "Renamed default to Falkenstein. Nothing restarts.\n"
    );

    let (listed, said) = region(&store, &["ls", "--connect", &connect]).await;
    assert_eq!(
        listed,
        "REGION\tKEY\tSERVERS\nFalkenstein\tdefault\tjoiner, fsn-1\nsyd\tsyd\tsyd-1, syd-2\n"
    );
    assert_eq!(
        said,
        "Not in a region: old-1.\nnext: ployz server set old-1 --label-add region=default\n"
    );

    let (json, _) = region(&store, &["ls", "--connect", &connect, "--json"]).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        json!({ "regions": [
            { "name": "Falkenstein", "key": "default", "servers": ["joiner", "fsn-1"] },
            { "name": "syd", "key": "syd", "servers": ["syd-1", "syd-2"] },
        ], "no_region": ["old-1"], "next": "ployz server set old-1 --label-add region=default" })
    );
}
