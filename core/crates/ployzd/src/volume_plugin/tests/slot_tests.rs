//! The hidden slot layout: Docker never sees a slot, the Pool still pays for it.

use super::lease_tests::start;
use super::*;

#[tokio::test]
async fn docker_never_sees_a_slot() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "slot"]);

    let listed = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(listed.get("Err").unwrap(), "");
    let names: Vec<&str> = listed
        .pointer("/Volumes")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|volume| volume["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["data"]);

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name": "copy"})).await,
        json!({"Err": "Provisioned Volume copy does not exist"})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name": "data"}))
            .await
            .pointer("/Err")
            .unwrap(),
        ""
    );

    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/volumes").unwrap(),
        &json!({"data": 1_073_741_824_u64})
    );
    server.abort();
}

#[tokio::test]
async fn mirror_admission_uses_ensure_capacity() {
    for route in ["/VolumeDriver.Create", "/Storage.Prepare"] {
        let test = TestDir::new();
        let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);
        let request = match route {
            "/VolumeDriver.Create" => json!({"Name": "data", "Opts": {"size": "1g"}}),
            _ => json!({"data": 1_073_741_824_u64}),
        };
        let response = post(&socket, route, request).await;
        let message = match route {
            "/VolumeDriver.Create" => error(&response).to_owned(),
            _ => response
                .pointer("/Err/cause/0")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        assert!(message.contains("automatic growth"), "{route}: {response}");
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create")
        );
        server.abort();
    }

    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root"]);
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name": "data", "Opts": {"size": "1g"}}),
        )
        .await,
        json!({"Err": ""})
    );
    server.abort();
}

#[test]
fn places_follow_the_dataset_layout() {
    for (dataset, place) in [
        ("tank", Place::Outside),
        ("tank/ployz", Place::Outside),
        ("tank/ployz/data", Place::Root("data")),
        ("tank/ployz/data/child", Place::Root("data/child")),
        ("tank/ployz-mirror", Place::Outside),
        ("tank/ployz-mirror/data", Place::Slot("data")),
        ("tank/ployz-mirror/data/fs", Place::Slot("data")),
        ("tank/other/data", Place::Outside),
        ("tankard/ployz/data", Place::Outside),
    ] {
        assert_eq!(Place::of(dataset, "tank"), place, "{dataset}");
    }
}
