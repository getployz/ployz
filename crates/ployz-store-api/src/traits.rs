use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::error::Error;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord,
    InviteRecord, MachineEvent, MachineId, MachineMembership, RoutingEvent, RoutingState,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

pub type MachineSubscriptionUpdate = Result<MachineEvent>;
pub type MachineSubscription = (
    Vec<MachineMembership>,
    mpsc::Receiver<MachineSubscriptionUpdate>,
);
pub type CertificateSubscriptionUpdate = Result<CertificateEvent>;
pub type CertificateSubscription = (
    Vec<CertificateRecord>,
    mpsc::Receiver<CertificateSubscriptionUpdate>,
);
pub type AcmeChallengeSubscriptionUpdate = Result<AcmeChallengeEvent>;
pub type AcmeChallengeSubscription = (
    Vec<AcmeChallengeRecord>,
    mpsc::Receiver<AcmeChallengeSubscriptionUpdate>,
);
pub type RoutingEventSubscriptionUpdate = Result<RoutingEventEnvelope>;
pub type RoutingEventSubscription = (RoutingState, mpsc::Receiver<RoutingEventSubscriptionUpdate>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingSubscription {
    Durable { consumer_id: String },
    Temporary,
}

impl RoutingSubscription {
    #[must_use]
    pub fn durable(consumer_id: impl Into<String>) -> Self {
        Self::Durable {
            consumer_id: consumer_id.into(),
        }
    }

    #[must_use]
    pub fn temporary() -> Self {
        Self::Temporary
    }

    #[must_use]
    pub fn durable_consumer_id(&self) -> Option<&str> {
        match self {
            Self::Durable { consumer_id } => Some(consumer_id),
            Self::Temporary => None,
        }
    }

    #[must_use]
    pub fn is_temporary(&self) -> bool {
        matches!(self, Self::Temporary)
    }
}

#[derive(Debug)]
pub struct RoutingEventEnvelope {
    pub event_id: String,
    pub cause: Option<String>,
    pub event: RoutingEvent,
    ack: Option<oneshot::Sender<()>>,
}

impl RoutingEventEnvelope {
    #[must_use]
    pub fn unacked(
        event_id: impl Into<String>,
        cause: Option<String>,
        event: RoutingEvent,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            cause,
            event,
            ack: None,
        }
    }

    #[must_use]
    pub fn with_ack(
        event_id: impl Into<String>,
        cause: Option<String>,
        event: RoutingEvent,
        ack: oneshot::Sender<()>,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            cause,
            event,
            ack: Some(ack),
        }
    }

    pub async fn ack(mut self) -> Result<()> {
        let Some(ack) = self.ack.take() else {
            return Ok(());
        };
        ack.send(()).map_err(|()| {
            Error::operation(
                "routing_event_ack_failed",
                format!("routing event '{}' ack receiver closed", self.event_id),
            )
        })
    }
}

pub fn apply_routing_event(state: &mut RoutingState, event: RoutingEvent) {
    match event {
        RoutingEvent::MachineAdded(record) | RoutingEvent::MachineUpdated { new: record, .. } => {
            upsert_by(&mut state.machines, record, |record| record.id.clone());
        }
        RoutingEvent::MachineRemoved { id } => {
            state.machines.retain(|candidate| candidate.id != id);
        }
        RoutingEvent::RevisionAdded(record) | RoutingEvent::RevisionUpdated { new: record, .. } => {
            upsert_by(&mut state.revisions, record, |record| {
                (
                    record.namespace.clone(),
                    record.service.clone(),
                    record.revision_hash.clone(),
                )
            });
        }
        RoutingEvent::RevisionRemoved {
            namespace,
            service,
            revision_hash,
        } => {
            state.revisions.retain(|candidate| {
                candidate.namespace != namespace
                    || candidate.service != service
                    || candidate.revision_hash != revision_hash
            });
        }
        RoutingEvent::ReleaseAdded(record) | RoutingEvent::ReleaseUpdated { new: record, .. } => {
            upsert_by(&mut state.releases, record, |record| {
                (record.namespace.clone(), record.service.clone())
            });
        }
        RoutingEvent::ReleaseRemoved { namespace, service } => {
            state.releases.retain(|candidate| {
                candidate.namespace != namespace || candidate.service != service
            });
        }
        RoutingEvent::InstanceAdded(record) | RoutingEvent::InstanceUpdated { new: record, .. } => {
            upsert_by(&mut state.instances, record, |record| {
                record.instance_id.clone()
            });
        }
        RoutingEvent::InstanceRemoved { instance_id } => {
            state
                .instances
                .retain(|candidate| candidate.instance_id != instance_id);
        }
    }
}

pub fn apply_routing_events(
    state: &mut RoutingState,
    events: impl IntoIterator<Item = RoutingEvent>,
) {
    for event in events {
        apply_routing_event(state, event);
    }
}

fn upsert_by<T, K>(items: &mut Vec<T>, value: T, key: impl Fn(&T) -> K)
where
    K: PartialEq,
{
    let value_key = key(&value);
    if let Some(existing) = items
        .iter_mut()
        .find(|candidate| key(candidate) == value_key)
    {
        *existing = value;
    } else {
        items.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RoutingEventEnvelope, RoutingSubscription, apply_routing_event, apply_routing_events,
    };
    use ployz_types::model::{
        DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
        MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey, RoutingEvent,
        RoutingState, ServiceRelease, ServiceReleaseRecord, ServiceRevisionRecord,
        ServiceRoutingPolicy, SlotId, StorageParticipation,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    #[test]
    fn routing_subscription_kind_tracks_consumer_lifetime() {
        let durable = RoutingSubscription::durable("gateway.founder");
        let temporary = RoutingSubscription::temporary();

        assert_eq!(durable.durable_consumer_id(), Some("gateway.founder"));
        assert!(!durable.is_temporary());
        assert_eq!(temporary.durable_consumer_id(), None);
        assert!(temporary.is_temporary());
    }

    #[tokio::test]
    async fn unacked_routing_event_ack_is_noop() {
        RoutingEventEnvelope::unacked(
            "event-1",
            None,
            RoutingEvent::MachineAdded(machine("machine-1", MachineLifecycle::Active)),
        )
        .ack()
        .await
        .expect("unacked event ack should be a no-op");
    }

    #[tokio::test]
    async fn routing_event_ack_surfaces_closed_receiver() {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        drop(ack_rx);

        let error = RoutingEventEnvelope::with_ack(
            "event-1",
            None,
            RoutingEvent::MachineAdded(machine("machine-1", MachineLifecycle::Active)),
            ack_tx,
        )
        .ack()
        .await
        .expect_err("closed ack receiver should be visible to caller");

        assert!(error.to_string().contains("event-1"));
        assert!(error.to_string().contains("ack receiver closed"));
    }

    #[test]
    fn routing_events_replace_records_by_contract_identity() {
        let namespace = Namespace("prod".into());
        let old_api = release(&namespace, "api", "rev-old");
        let new_api = release(&namespace, "api", "rev-new");
        let worker = release(&namespace, "worker", "worker-rev");
        let mut state = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: vec![old_api.clone(), worker.clone()],
            instances: Vec::new(),
        };

        apply_routing_event(
            &mut state,
            RoutingEvent::ReleaseUpdated {
                old: old_api,
                new: new_api,
            },
        );

        assert_eq!(state.releases.len(), 2);
        assert_eq!(
            state
                .releases
                .iter()
                .find(|release| release.service == "api")
                .map(|release| release.release.primary_revision_hash.as_str()),
            Some("rev-new")
        );
        assert!(state.releases.iter().any(|release| release == &worker));
    }

    #[test]
    fn routing_events_remove_only_the_matching_contract_identity() {
        let namespace = Namespace("prod".into());
        let other_namespace = Namespace("staging".into());
        let prod_api = release(&namespace, "api", "prod-rev");
        let staging_api = release(&other_namespace, "api", "staging-rev");
        let mut state = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: vec![prod_api.clone(), staging_api.clone()],
            instances: Vec::new(),
        };

        apply_routing_event(
            &mut state,
            RoutingEvent::ReleaseRemoved {
                namespace: prod_api.namespace,
                service: prod_api.service,
            },
        );

        assert_eq!(state.releases, vec![staging_api]);
    }

    #[test]
    fn routing_events_project_revision_updates_and_removals_by_contract_identity() {
        let namespace = Namespace("prod".into());
        let old_api = revision(&namespace, "api", "rev-1", "{}");
        let new_api = revision(&namespace, "api", "rev-1", r#"{"image":"nginx:2"}"#);
        let worker = revision(&namespace, "worker", "rev-1", "{}");
        let mut state = RoutingState {
            machines: Vec::new(),
            revisions: vec![old_api.clone(), worker.clone()],
            releases: Vec::new(),
            instances: Vec::new(),
        };

        apply_routing_event(
            &mut state,
            RoutingEvent::RevisionUpdated {
                old: old_api,
                new: new_api,
            },
        );

        assert_eq!(state.revisions.len(), 2);
        assert_eq!(
            state
                .revisions
                .iter()
                .find(|revision| revision.service == "api")
                .map(|revision| revision.spec_json.as_str()),
            Some(r#"{"image":"nginx:2"}"#)
        );

        apply_routing_event(
            &mut state,
            RoutingEvent::RevisionRemoved {
                namespace: worker.namespace,
                service: worker.service,
                revision_hash: worker.revision_hash,
            },
        );

        assert_eq!(state.revisions.len(), 1);
        assert_eq!(state.revisions[0].service, "api");
    }

    #[test]
    fn routing_events_project_instance_updates_and_removals_by_instance_id() {
        let namespace = Namespace("prod".into());
        let old_api = instance(&namespace, "inst-api", false);
        let new_api = instance(&namespace, "inst-api", true);
        let worker = instance(&namespace, "inst-worker", true);
        let mut state = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: vec![old_api.clone(), worker.clone()],
        };

        apply_routing_event(
            &mut state,
            RoutingEvent::InstanceUpdated {
                old: old_api,
                new: new_api,
            },
        );

        assert_eq!(state.instances.len(), 2);
        assert_eq!(
            state
                .instances
                .iter()
                .find(|instance| instance.instance_id == InstanceId("inst-api".into()))
                .map(|instance| instance.ready),
            Some(true)
        );

        apply_routing_event(
            &mut state,
            RoutingEvent::InstanceRemoved {
                instance_id: worker.instance_id,
            },
        );

        assert_eq!(state.instances.len(), 1);
        assert_eq!(
            state.instances[0].instance_id,
            InstanceId("inst-api".into())
        );
    }

    #[test]
    fn routing_events_project_to_the_same_state_as_a_fresh_snapshot() {
        let machine_a = machine("machine-a", MachineLifecycle::Active);
        let mut machine_a_draining = machine_a.clone();
        machine_a_draining.lifecycle = MachineLifecycle::Draining;
        let machine_b = machine("machine-b", MachineLifecycle::Active);

        let mut state = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };
        apply_routing_events(
            &mut state,
            [
                RoutingEvent::MachineAdded(machine_a.clone()),
                RoutingEvent::MachineAdded(machine_b.clone()),
                RoutingEvent::MachineUpdated {
                    old: machine_a,
                    new: machine_a_draining.clone(),
                },
                RoutingEvent::MachineRemoved { id: machine_b.id },
            ],
        );

        assert_eq!(state.machines, vec![machine_a_draining]);
    }

    #[test]
    fn routing_event_removals_are_idempotent_by_contract_identity() {
        let namespace = Namespace("prod".into());
        let machine_a = machine("machine-a", MachineLifecycle::Active);
        let machine_b = machine("machine-b", MachineLifecycle::Active);
        let revision_a = revision(&namespace, "api", "rev-a", "{}");
        let revision_b = revision(&namespace, "worker", "rev-b", "{}");
        let release_a = release(&namespace, "api", "rev-a");
        let release_b = release(&namespace, "worker", "rev-b");
        let instance_a = instance(&namespace, "inst-a", true);
        let instance_b = instance(&namespace, "inst-b", true);
        let mut state = RoutingState {
            machines: vec![machine_a.clone(), machine_b.clone()],
            revisions: vec![revision_a.clone(), revision_b.clone()],
            releases: vec![release_a.clone(), release_b.clone()],
            instances: vec![instance_a.clone(), instance_b.clone()],
        };

        apply_routing_events(
            &mut state,
            [
                RoutingEvent::MachineRemoved {
                    id: machine_a.id.clone(),
                },
                RoutingEvent::MachineRemoved { id: machine_a.id },
                RoutingEvent::RevisionRemoved {
                    namespace: revision_a.namespace.clone(),
                    service: revision_a.service.clone(),
                    revision_hash: revision_a.revision_hash.clone(),
                },
                RoutingEvent::RevisionRemoved {
                    namespace: revision_a.namespace,
                    service: revision_a.service,
                    revision_hash: revision_a.revision_hash,
                },
                RoutingEvent::ReleaseRemoved {
                    namespace: release_a.namespace.clone(),
                    service: release_a.service.clone(),
                },
                RoutingEvent::ReleaseRemoved {
                    namespace: release_a.namespace,
                    service: release_a.service,
                },
                RoutingEvent::InstanceRemoved {
                    instance_id: instance_a.instance_id.clone(),
                },
                RoutingEvent::InstanceRemoved {
                    instance_id: instance_a.instance_id,
                },
            ],
        );

        assert_eq!(state.machines, vec![machine_b]);
        assert_eq!(state.revisions, vec![revision_b]);
        assert_eq!(state.releases, vec![release_b]);
        assert_eq!(state.instances, vec![instance_b]);
    }

    fn release(namespace: &Namespace, service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: namespace.clone(),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId("deploy-1".into()),
                updated_at: 1,
            },
        }
    }

    fn revision(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
        spec_json: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: namespace.clone(),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: spec_json.into(),
            created_by: MachineId("machine-a".into()),
            created_at: 1,
        }
    }

    fn instance(namespace: &Namespace, instance_id: &str, ready: bool) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId(instance_id.into()),
            namespace: namespace.clone(),
            service: "api".into(),
            slot_id: SlotId("slot-1".into()),
            machine_id: MachineId("machine-a".into()),
            revision_hash: "rev-1".into(),
            deploy_id: DeployId("deploy-1".into()),
            docker_container_id: format!("container-{instance_id}"),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready,
            drain_state: DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 1,
        }
    }

    fn machine(id: &str, lifecycle: MachineLifecycle) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployCommit {
    pub namespace: Namespace,
    pub revisions: Vec<ServiceRevisionRecord>,
    pub removed_services: Vec<String>,
    pub removed_volumes: Vec<String>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub volumes: Vec<VolumeRecord>,
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecordUpdate {
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploySnapshot {
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

pub trait MachineRegistry: Send + Sync {
    fn init(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async { Ok(()) }
    }

    fn list_machines(&self) -> impl Future<Output = Result<Vec<MachineMembership>>> + Send + '_;

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineMembership,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_machines(&self) -> impl Future<Output = Result<MachineSubscription>> + Send + '_;
}

pub trait InviteRepository: Send + Sync {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_invite<'a>(
        &'a self,
        invite_id: &'a str,
    ) -> impl Future<Output = Result<Option<InviteRecord>>> + Send + 'a;

    fn list_invites(&self) -> impl Future<Output = Result<Vec<InviteRecord>>> + Send + '_;

    fn redeem_invite<'a>(
        &'a self,
        invite_id: &'a str,
        machine_id: &'a MachineId,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;

    fn revoke_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;
}

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(&self) -> impl Future<Output = Result<RoutingState>> + Send + '_;

    fn subscribe_routing_events<'a>(
        &'a self,
        subscription: RoutingSubscription,
    ) -> impl Future<Output = Result<RoutingEventSubscription>> + Send + 'a;
}

pub trait DeployRepository: Send + Sync {
    fn list_deploy_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a;

    fn load_deploy_snapshot<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<DeploySnapshot>> + Send + 'a;

    fn list_volumes<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<VolumeRecord>>> + Send + 'a;

    fn get_volume<'a>(
        &'a self,
        namespace: &'a Namespace,
        volume_name: &'a str,
    ) -> impl Future<Output = Result<Option<VolumeRecord>>> + Send + 'a;

    fn commit_deploy<'a>(
        &'a self,
        command: &'a DeployCommit,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn update_deploy_record<'a>(
        &'a self,
        command: &'a DeployRecordUpdate,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl Future<Output = Result<Option<DeployRecord>>> + Send + 'a;
}

pub trait InstanceStatusRepository: Send + Sync {
    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a;

    fn record_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn remove_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}

pub trait CertificateStore: Send + Sync {
    fn get_acme_account<'a>(
        &'a self,
        issuer_url: &'a str,
    ) -> impl Future<Output = Result<Option<AcmeAccountRecord>>> + Send + 'a;

    fn upsert_acme_account<'a>(
        &'a self,
        record: &'a AcmeAccountRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_certificates(&self)
    -> impl Future<Output = Result<Vec<CertificateRecord>>> + Send + '_;

    fn get_certificate<'a>(
        &'a self,
        hostname: &'a str,
    ) -> impl Future<Output = Result<Option<CertificateRecord>>> + Send + 'a;

    fn upsert_certificate<'a>(
        &'a self,
        record: &'a CertificateRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeRecord>>> + Send + '_;

    fn upsert_acme_challenge<'a>(
        &'a self,
        record: &'a AcmeChallengeRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_acme_challenge<'a>(
        &'a self,
        hostname: &'a str,
        token: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_certificates(
        &self,
    ) -> impl Future<Output = Result<CertificateSubscription>> + Send + '_;

    fn subscribe_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<AcmeChallengeSubscription>> + Send + '_;

    fn upsert_acme_challenge_readiness<'a>(
        &'a self,
        record: &'a AcmeChallengeReadinessRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_acme_challenge_readiness<'a>(
        &'a self,
        hostname: &'a str,
        token: &'a str,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeReadinessRecord>>> + Send + 'a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Synced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRttObservation {
    pub addr: SocketAddr,
    pub rtts_ms: Vec<u64>,
}

pub trait PeerRttStore: Send + Sync {
    fn peer_rtt_observations(
        &self,
    ) -> impl Future<Output = Result<Vec<PeerRttObservation>>> + Send + '_ {
        async { Ok(Vec::new()) }
    }
}

pub trait SyncProbe: Send + Sync {
    fn sync_status(&self) -> impl Future<Output = Result<SyncStatus>> + Send + '_ {
        async { Ok(SyncStatus::Synced) }
    }
}

#[async_trait]
pub trait StoreRuntimeControl: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn wipe_data(&self) -> Result<()> {
        Ok(())
    }
    async fn healthy(&self) -> bool;
}
