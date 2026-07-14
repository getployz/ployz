use crate::intent::service::NatsIntentReader;
use ployz_core::state::IntentSnapshot;
use ployz_sdk_types::{VolumeListError, VolumeListResult, VolumeSnapshot, VolumeStatus};

#[derive(Clone)]
pub struct VolumeQueryService {
    intent_reader: NatsIntentReader,
}

impl VolumeQueryService {
    #[must_use]
    pub(crate) const fn new(intent_reader: NatsIntentReader) -> Self {
        Self { intent_reader }
    }

    pub(crate) async fn list(&self) -> Result<VolumeListResult, VolumeListError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| VolumeListError::Unavailable {
                    message: error.to_string(),
                })?;
        Ok(VolumeListResult {
            volumes: volume_snapshots(&intent),
        })
    }
}

fn volume_snapshots(intent: &IntentSnapshot) -> Vec<VolumeSnapshot> {
    let mut volumes = intent
        .volume_pins
        .iter()
        .map(|pin| VolumeSnapshot {
            namespace_id: pin.namespace_id.clone(),
            volume_name: pin.volume_name.clone(),
            machine_id: pin.machine_id.clone(),
            status: if intent
                .services_referencing_volume(&pin.namespace_id, &pin.volume_name)
                .is_empty()
            {
                VolumeStatus::Orphaned
            } else {
                VolumeStatus::InUse
            },
        })
        .collect::<Vec<_>>();
    volumes.sort_by(|left, right| {
        (&left.namespace_id, &left.volume_name).cmp(&(&right.namespace_id, &right.volume_name))
    });
    volumes
}

#[cfg(test)]
mod tests {
    use super::volume_snapshots;
    use ployz_core::deploy::VolumeName;
    use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot, VolumePinState};
    use ployz_sdk_types::VolumeStatus;
    use ployz_test_support::fixtures::serving_target_entry_in;
    use ployz_test_support::ids::{machine_id, namespace_id};

    #[test]
    fn volume_projection_tracks_serving_target_references() {
        let namespace_id = namespace_id("team-a");
        let used = VolumeName::try_new("used").expect("valid volume name");
        let orphaned = VolumeName::try_new("orphaned").expect("valid volume name");
        let mut target = serving_target_entry_in("team-a", "web", "entry_a");
        target.volume_names = vec![used.clone()];
        let mut intent = IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine_id("core"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: vec![target],
            volume_pins: vec![
                VolumePinState {
                    namespace_id: namespace_id.clone(),
                    volume_name: used.clone(),
                    machine_id: machine_id("machine_a"),
                },
                VolumePinState {
                    namespace_id,
                    volume_name: orphaned,
                    machine_id: machine_id("machine_a"),
                },
            ],
            nats_authorizations: Vec::new(),
            public_url: ployz_core::state::IntentPublicUrl::Auto(Box::new(
                ployz_core::state::ManagedLeaseProjection::Unacquired,
            )),
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        };

        let projected = volume_snapshots(&intent);
        let [orphaned_snapshot, used_snapshot] = projected.as_slice() else {
            panic!("expected two volume snapshots");
        };
        assert_eq!(orphaned_snapshot.status, VolumeStatus::Orphaned);
        assert_eq!(used_snapshot.status, VolumeStatus::InUse);

        let [target] = intent.serving_target_entries.as_mut_slice() else {
            panic!("expected one serving target");
        };
        target.volume_names.clear();
        let projected_after_drop = volume_snapshots(&intent);
        let [_, used_snapshot] = projected_after_drop.as_slice() else {
            panic!("expected two volume snapshots");
        };
        assert_eq!(used_snapshot.status, VolumeStatus::Orphaned);
    }
}
