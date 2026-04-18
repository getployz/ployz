use super::*;
use crate::mesh::probe::ProbeListenerFamily;
use crate::mesh::tasks::{SelfRecordMutation, apply_self_record_mutation};

impl Mesh {
    pub async fn authoritative_self_record(&self) -> Option<MachineRecord> {
        let authoritative_self = self.authoritative_self.as_ref()?.clone();
        Some(authoritative_self.read().await.clone())
    }

    pub async fn update_authoritative_self_record(
        &self,
        update: impl FnOnce(&mut MachineRecord),
    ) -> Option<MachineRecord> {
        if let Some(self_record_tx) = &self.self_record_tx {
            let mut next = self.authoritative_self_record().await?;
            update(&mut next);
            self.sync_required_probe_family(next.overlay_ip);
            return apply_self_record_mutation(self_record_tx, SelfRecordMutation::Replace(next))
                .await;
        }

        let authoritative_self = self.authoritative_self.as_ref()?.clone();
        let mut record = authoritative_self.write().await;
        update(&mut record);
        self.sync_required_probe_family(record.overlay_ip);
        Some(record.clone())
    }

    pub async fn apply_self_drain(&self, drain: bool) -> Option<MachineRecord> {
        let self_record_tx = self.self_record_tx.as_ref()?;
        apply_self_record_mutation(self_record_tx, SelfRecordMutation::SetDrain { drain }).await
    }

    pub(crate) fn sync_required_probe_family(&self, overlay_ip: crate::model::OverlayIp) {
        let required_family = if self.network.runs_probe_listener() {
            Some(ProbeListenerFamily::for_overlay_ip(overlay_ip))
        } else {
            None
        };
        self.probe_readiness.set_required_family(required_family);
    }
}
