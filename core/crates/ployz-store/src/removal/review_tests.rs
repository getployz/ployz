use ployz_core::config::{SavedVolumeIntent, VolumeKind};

use super::*;

fn volume(id: &str, name: &str) -> SavedVolumeIntent {
    SavedVolumeIntent {
        resource_id: id.to_owned(),
        resource_lineage_id: id.to_owned(),
        name: name.to_owned(),
        storage: VolumeKind::Docker {},
        shared_writes: false,
    }
}

fn holding(volumes: Vec<SavedVolumeIntent>) -> SavedEnvironmentIntent {
    SavedEnvironmentIntent {
        version: 1,
        environment_slug: "production".to_owned(),
        services: Vec::new(),
        volumes,
        configs: Vec::new(),
    }
}

#[test]
fn consent_for_a_restored_volume_does_not_carry_while_a_removed_one_keeps_it() {
    let data = volume("00000000-0000-4000-8000-000000000005", "data");
    let logs = volume("00000000-0000-4000-8000-000000000006", "logs");
    let namespace = Namespace::parse("shop-production").unwrap();
    let applied = holding(vec![data.clone(), logs]);
    let consent: Vec<VolumeLoss> = removed(&applied, &holding(Vec::new()), &namespace)
        .unwrap()
        .into_iter()
        .map(|volume| VolumeLoss {
            volume,
            deletes: Vec::new(),
        })
        .collect();

    let carried = still_removed(&consent, &holding(vec![data]));

    let names: Vec<&str> = carried
        .iter()
        .map(|loss| loss.volume.name.as_str())
        .collect();
    assert_eq!(names, ["logs"]);
}
