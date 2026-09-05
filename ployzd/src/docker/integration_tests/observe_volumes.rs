use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    AdvertisedEndpoint, CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    MachineId, MachineName,
};
use tokio_util::sync::CancellationToken;

use super::*;

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn docker_volume_events_and_rescans_publish_named_local_observations() {
    let root = TestRoot::new();
    let mut corrosion = crate::corrosion::CorrosionConfig::new(
        root.0.join("corrosion"),
        root.0.join("run"),
        unused_address(),
        unused_address(),
        format!("ployz-corrosion-volumes-{}", MachineId::random()),
    )
    .start()
    .await
    .unwrap();
    let replicated = corrosion.store().clone();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let mut local = crate::machine::LocalMachineStore::open(root.0.join("machine")).unwrap();
    let machine_id = local
        .initialize(
            MachineName::parse("observer").unwrap(),
            crate::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
            },
            None,
            vec![AdvertisedEndpoint("127.0.0.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap()
        .id;
    let local = Arc::new(Mutex::new(local));
    let runtime = ContainerRuntime::new(docker, specs);
    let foreign_machine_id = MachineId::random();

    let external = runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: unique_volume("external"),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::from([("source".into(), "operator".into())]),
            },
        )
        .await
        .unwrap()
        .into_observation()
        .unwrap();
    let managed = runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: unique_volume("managed"),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::from([(LABEL_MANAGED.to_owned(), String::new())]),
            },
        )
        .await
        .unwrap()
        .into_observation()
        .unwrap();
    let stale_local = fixture_volume(&machine_id, "stale-local");
    let stale_foreign = fixture_volume(&foreign_machine_id, "stale-foreign");
    replicated.publish_volume(&stale_local).await.unwrap();
    replicated.publish_volume(&stale_foreign).await.unwrap();

    let runtime = runtime
        .replicating(replicated.clone(), Arc::clone(&local))
        .with_rescan_interval(Duration::from_secs(3));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        let runtime = runtime.clone();
        async move { runtime.publish_observations(shutdown).await }
    });

    wait_for(Duration::from_secs(5), || async {
        replicated.volume(&external.id).await.unwrap() == Some(external.clone())
            && replicated.volume(&managed.id).await.unwrap() == Some(managed.clone())
    })
    .await;
    assert!(replicated.volume(&stale_local.id).await.unwrap().is_none());
    assert_eq!(
        replicated.volume(&stale_foreign.id).await.unwrap().as_ref(),
        Some(&stale_foreign)
    );
    let listed = runtime.list_volumes(&machine_id).await.unwrap();
    assert!(listed.volumes.contains(&external));
    assert!(listed.volumes.contains(&managed));

    let created = runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: unique_volume("event"),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
        )
        .await
        .unwrap()
        .into_observation()
        .unwrap();
    wait_for(Duration::from_secs(2), || async {
        replicated.volume(&created.id).await.unwrap() == Some(created.clone())
    })
    .await;

    replicated
        .machine_publication()
        .await
        .apply_volume_rows(
            &machine_id,
            std::slice::from_ref(&managed.id.name),
            &[],
            &[],
        )
        .await
        .unwrap();
    assert!(replicated.volume(&managed.id).await.unwrap().is_none());
    wait_for(Duration::from_secs(5), || async {
        replicated.volume(&managed.id).await.unwrap() == Some(managed.clone())
    })
    .await;

    runtime
        .remove_volume(&created.id.name, false)
        .await
        .unwrap();
    wait_for(Duration::from_secs(2), || async {
        replicated.volume(&created.id).await.unwrap().is_none()
    })
    .await;
    assert_eq!(
        replicated.volume(&stale_foreign.id).await.unwrap().as_ref(),
        Some(&stale_foreign)
    );
    assert_eq!(
        replicated.volume(&external.id).await.unwrap().as_ref(),
        Some(&external)
    );

    shutdown.cancel();
    task.await.unwrap().unwrap();
    runtime
        .remove_volume(&external.id.name, true)
        .await
        .unwrap();
    runtime.remove_volume(&managed.id.name, true).await.unwrap();
    corrosion.cleanup().await.unwrap();
}

fn unique_volume(prefix: &str) -> DockerVolumeName {
    DockerVolumeName::parse(format!("ployz-{prefix}-{}", MachineId::random())).unwrap()
}

fn fixture_volume(machine_id: &MachineId, prefix: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: *machine_id,
            name: unique_volume(prefix),
        },
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}
