pub mod projection;

use async_nats::jetstream;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use async_nats::jetstream::stream::DirectGetErrorKind;
use ployz_store_api::{
    DeployCommit, DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    DeployId, DeployRecord, RoutingEvent, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::DEPLOY_STATUS_BUCKET;
use crate::store::deploys::projection::DeployProjection;
use crate::store::kv_json;
use crate::subjects::{self, DEPLOY_COMMITS_STREAM, NatsScope, REVISIONS_STREAM};

#[derive(Debug, Clone)]
pub(crate) struct CachedDeployProjection {
    projection: DeployProjection,
    deploy_last_sequence: u64,
    revision_last_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployCommitPublish {
    Created,
    Existing { sequence: u64 },
}

impl DeployRepository for NatsStore {
    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        Ok(self.deploy_projection_snapshot().await?.releases(namespace))
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        let projection = self.deploy_projection_snapshot().await?;
        Ok(DeploySnapshot {
            revisions: projection.revisions(namespace),
            releases: projection.releases(namespace),
            instances: self.list_instance_status(namespace).await?,
        })
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        Ok(self.deploy_projection_snapshot().await?.volumes(namespace))
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        Ok(self
            .deploy_projection_snapshot()
            .await?
            .volume(namespace, volume_name)
            .cloned())
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        let existing = self
            .deploy_projection_snapshot()
            .await?
            .revision(
                &command.revision.namespace,
                &command.revision.service,
                &command.revision.revision_hash,
            )
            .cloned();
        publish_revision_in(self.jetstream(), self.scope(), command).await?;
        let event = revision_routing_event(existing, &command.revision);
        self.publish_routing_batch(
            format!(
                "revision:{}:{}:{}",
                command.revision.namespace.0,
                command.revision.service,
                command.revision.revision_hash
            ),
            "deploy.revision",
            &[event],
        )
        .await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        let mut projection = self.deploy_projection_snapshot().await?;
        let routing_events = projection.apply_commit_events(command);
        let publish = publish_commit_in(self.jetstream(), self.scope(), command).await?;
        let routing_events = match publish {
            DeployCommitPublish::Created => routing_events,
            DeployCommitPublish::Existing { sequence } => {
                duplicate_commit_repair_events(self.jetstream(), command, sequence).await?
            }
        };
        *self.deploy_projection.write().await = None;
        self.publish_routing_batch(
            format!("deploy:{}", command.deploy.deploy_id.0),
            "deploy.commit",
            &routing_events,
        )
        .await?;
        if let Err(error) = write_deploy_status(self.jetstream(), &command.deploy).await {
            warn!(?error, deploy_id = %command.deploy.deploy_id, "initial deploy status write failed after commit");
        }
        Ok(())
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        write_deploy_status(self.jetstream(), &command.deploy).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        if let Some(status) = read_deploy_status(self.jetstream(), deploy_id).await? {
            return Ok(Some(status));
        }
        Ok(self
            .deploy_projection_snapshot()
            .await?
            .deploy(deploy_id)
            .cloned())
    }
}

fn revision_routing_event(
    existing: Option<ServiceRevisionRecord>,
    revision: &ServiceRevisionRecord,
) -> RoutingEvent {
    match existing {
        Some(old) if old != *revision => RoutingEvent::RevisionUpdated {
            old,
            new: revision.clone(),
        },
        Some(_) | None => RoutingEvent::RevisionAdded(revision.clone()),
    }
}

impl NatsStore {
    pub(crate) async fn deploy_projection_snapshot(&self) -> Result<DeployProjection> {
        let mut stream = self
            .jetstream()
            .get_stream(DEPLOY_COMMITS_STREAM)
            .await
            .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
        let info = stream
            .info()
            .await
            .map_err(|error| Error::operation("nats_deploy_stream_info", format!("{error:?}")))?
            .clone();
        let mut revision_stream = self
            .jetstream()
            .get_stream(REVISIONS_STREAM)
            .await
            .map_err(|error| Error::operation("nats_revision_stream", format!("{error:?}")))?;
        let revision_info = revision_stream
            .info()
            .await
            .map_err(|error| Error::operation("nats_revision_stream_info", format!("{error:?}")))?
            .clone();
        if let Some(cached) = self.deploy_projection.read().await.as_ref()
            && cached.deploy_last_sequence == info.state.last_sequence
            && cached.revision_last_sequence == revision_info.state.last_sequence
        {
            return Ok(cached.projection.clone());
        }

        let current = self.deploy_projection.read().await.clone();
        let cached = match current {
            Some(cached)
                if can_extend_cached_projection(cached.deploy_last_sequence, &info)
                    && can_extend_cached_projection(
                        cached.revision_last_sequence,
                        &revision_info,
                    ) =>
            {
                extend_projection_from_stream(
                    &mut stream,
                    &mut revision_stream,
                    cached,
                    info,
                    revision_info,
                )
                .await?
            }
            Some(_) | None => {
                replay_projection_from_stream(
                    &mut stream,
                    &mut revision_stream,
                    info,
                    revision_info,
                )
                .await?
            }
        };
        *self.deploy_projection.write().await = Some(cached.clone());
        Ok(cached.projection)
    }
}

pub async fn publish_commit(
    js: &jetstream::Context,
    commit: &DeployCommit,
) -> Result<DeployCommitPublish> {
    publish_commit_in(js, &NatsScope::default(), commit).await
}

pub async fn publish_commit_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    commit: &DeployCommit,
) -> Result<DeployCommitPublish> {
    let subject = subjects::deploy_commit_in(scope, &commit.namespace, &commit.deploy.deploy_id);
    let payload = serde_json::to_vec(commit)
        .map_err(|error| Error::operation("nats_deploy_commit_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(DEPLOY_COMMITS_STREAM)
        .expected_last_subject_sequence(0)
        .message_id(format!("deploy-commit:{}", commit.deploy.deploy_id.0));
    let ack = js
        .send_publish(subject.clone(), publish)
        .await
        .map_err(|error| Error::operation("nats_deploy_commit_publish", format!("{error:?}")))?;
    match ack.await {
        Ok(_) => Ok(DeployCommitPublish::Created),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => {
            let stream = js
                .get_stream(DEPLOY_COMMITS_STREAM)
                .await
                .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
            let message = stream
                .direct_get_last_for_subject(subject)
                .await
                .map_err(|error| {
                    Error::operation("nats_deploy_commit_get", format!("{error:?}"))
                })?;
            let stored: DeployCommit =
                serde_json::from_slice(message.payload.as_ref()).map_err(|error| {
                    Error::operation("nats_deploy_commit_decode", error.to_string())
                })?;
            if stored != *commit {
                return Err(Error::operation(
                    "nats_deploy_commit_conflict",
                    format!(
                        "deploy commit '{}' already exists with different payload",
                        commit.deploy.deploy_id
                    ),
                ));
            }
            Ok(DeployCommitPublish::Existing {
                sequence: message.sequence,
            })
        }
        Err(error) => Err(Error::operation(
            "nats_deploy_commit_ack",
            format!("{error:?}"),
        )),
    }
}

async fn duplicate_commit_repair_events(
    js: &jetstream::Context,
    command: &DeployCommit,
    sequence: u64,
) -> Result<Vec<RoutingEvent>> {
    let mut stream = js
        .get_stream(DEPLOY_COMMITS_STREAM)
        .await
        .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
    let info = stream
        .info()
        .await
        .map_err(|error| Error::operation("nats_deploy_stream_info", format!("{error:?}")))?
        .clone();
    let before = deploy_projection_before_sequence(&mut stream, info.clone(), sequence).await?;
    let mut revision_stream = js
        .get_stream(REVISIONS_STREAM)
        .await
        .map_err(|error| Error::operation("nats_revision_stream", format!("{error:?}")))?;
    let revision_info = revision_stream
        .info()
        .await
        .map_err(|error| Error::operation("nats_revision_stream_info", format!("{error:?}")))?
        .clone();
    let current =
        replay_projection_from_stream(&mut stream, &mut revision_stream, info, revision_info)
            .await?
            .projection;
    Ok(repair_events_for_duplicate_commit(
        &before, &current, command,
    ))
}

async fn deploy_projection_before_sequence(
    stream: &mut async_nats::jetstream::stream::Stream,
    info: async_nats::jetstream::stream::Info,
    sequence: u64,
) -> Result<DeployProjection> {
    let mut projection = DeployProjection::new();
    if info.state.messages == 0 || sequence <= info.state.first_sequence {
        return Ok(projection);
    }
    apply_projection_range(
        stream,
        &mut projection,
        info.state.first_sequence,
        sequence.saturating_sub(1),
    )
    .await?;
    Ok(projection)
}

fn repair_events_for_duplicate_commit(
    before: &DeployProjection,
    current: &DeployProjection,
    command: &DeployCommit,
) -> Vec<RoutingEvent> {
    let mut events = Vec::new();
    for revision in &command.revisions {
        if current.revision(
            &revision.namespace,
            &revision.service,
            &revision.revision_hash,
        ) == Some(revision)
        {
            events.push(RoutingEvent::RevisionAdded(revision.clone()));
        }
    }
    for service in &command.removed_services {
        if current.release(&command.namespace, service).is_none()
            && let Some(old) = before.release(&command.namespace, service)
        {
            events.push(RoutingEvent::ReleaseRemoved(old.clone()));
        }
    }
    for release in &command.releases {
        if current.release(&release.namespace, &release.service) == Some(release) {
            events.push(RoutingEvent::ReleaseAdded(release.clone()));
        }
    }
    events
}

async fn replay_projection_from_stream(
    stream: &mut async_nats::jetstream::stream::Stream,
    revision_stream: &mut async_nats::jetstream::stream::Stream,
    info: async_nats::jetstream::stream::Info,
    revision_info: async_nats::jetstream::stream::Info,
) -> Result<CachedDeployProjection> {
    let mut projection = DeployProjection::new();
    if info.state.messages > 0 {
        apply_projection_range(
            stream,
            &mut projection,
            info.state.first_sequence,
            info.state.last_sequence,
        )
        .await?;
    }
    if revision_info.state.messages > 0 {
        apply_revision_range(
            revision_stream,
            &mut projection,
            revision_info.state.first_sequence,
            revision_info.state.last_sequence,
        )
        .await?;
    }
    Ok(CachedDeployProjection {
        projection,
        deploy_last_sequence: info.state.last_sequence,
        revision_last_sequence: revision_info.state.last_sequence,
    })
}

async fn extend_projection_from_stream(
    stream: &mut async_nats::jetstream::stream::Stream,
    revision_stream: &mut async_nats::jetstream::stream::Stream,
    cached: CachedDeployProjection,
    info: async_nats::jetstream::stream::Info,
    revision_info: async_nats::jetstream::stream::Info,
) -> Result<CachedDeployProjection> {
    let mut projection = cached.projection;
    apply_projection_range(
        stream,
        &mut projection,
        cached.deploy_last_sequence.saturating_add(1),
        info.state.last_sequence,
    )
    .await?;
    apply_revision_range(
        revision_stream,
        &mut projection,
        cached.revision_last_sequence.saturating_add(1),
        revision_info.state.last_sequence,
    )
    .await?;
    Ok(CachedDeployProjection {
        projection,
        deploy_last_sequence: info.state.last_sequence,
        revision_last_sequence: revision_info.state.last_sequence,
    })
}

fn can_extend_cached_projection(
    cached_last_sequence: u64,
    info: &async_nats::jetstream::stream::Info,
) -> bool {
    cached_projection_extension_start(
        cached_last_sequence,
        info.state.first_sequence,
        info.state.last_sequence,
        info.state.messages,
    )
    .is_some()
}

fn cached_projection_extension_start(
    cached_last_sequence: u64,
    first_sequence: u64,
    last_sequence: u64,
    messages: u64,
) -> Option<u64> {
    if messages == 0 || cached_last_sequence >= last_sequence {
        return None;
    }
    let next_sequence = cached_last_sequence.saturating_add(1);
    if next_sequence < first_sequence {
        return None;
    }
    Some(next_sequence)
}

async fn apply_projection_range(
    stream: &mut async_nats::jetstream::stream::Stream,
    projection: &mut DeployProjection,
    first_sequence: u64,
    last_sequence: u64,
) -> Result<()> {
    for sequence in first_sequence..=last_sequence {
        let message = match stream.direct_get(sequence).await {
            Ok(message) => message,
            Err(error) if error.kind() == DirectGetErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::operation(
                    "nats_deploy_stream_replay",
                    format!("{error:?}"),
                ));
            }
        };
        let commit: DeployCommit = serde_json::from_slice(message.payload.as_ref())
            .map_err(|error| Error::operation("nats_deploy_commit_decode", error.to_string()))?;
        projection.apply_commit(&commit);
    }
    Ok(())
}

async fn apply_revision_range(
    stream: &mut async_nats::jetstream::stream::Stream,
    projection: &mut DeployProjection,
    first_sequence: u64,
    last_sequence: u64,
) -> Result<()> {
    if first_sequence > last_sequence {
        return Ok(());
    }
    for sequence in first_sequence..=last_sequence {
        let message = match stream.direct_get(sequence).await {
            Ok(message) => message,
            Err(error) if error.kind() == DirectGetErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::operation(
                    "nats_revision_stream_replay",
                    format!("{error:?}"),
                ));
            }
        };
        let revision: ServiceRevisionRecord = serde_json::from_slice(message.payload.as_ref())
            .map_err(|error| Error::operation("nats_revision_decode", error.to_string()))?;
        projection.apply_revision(&revision);
    }
    Ok(())
}

async fn publish_revision_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    command: &DeployRevisionUpsert,
) -> Result<()> {
    let revision = &command.revision;
    let subject = subjects::revision_in(
        scope,
        &revision.namespace,
        &revision.service,
        &revision.revision_hash,
    );
    let payload = serde_json::to_vec(revision)
        .map_err(|error| Error::operation("nats_revision_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(REVISIONS_STREAM)
        .message_id(format!(
            "revision:{}:{}:{}",
            revision.namespace.0, revision.service, revision.revision_hash
        ));
    let ack = js
        .send_publish(subject, publish)
        .await
        .map_err(|error| Error::operation("nats_revision_publish", format!("{error:?}")))?;
    ack.await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_revision_ack", format!("{error:?}")))
}

async fn write_deploy_status(js: &jetstream::Context, deploy: &DeployRecord) -> Result<()> {
    let bucket = kv_json::get_bucket(js, DEPLOY_STATUS_BUCKET, "nats_deploy_status_bucket").await?;
    kv_json::put_json(
        &bucket,
        &deploy.deploy_id.0,
        deploy,
        "nats_deploy_status_encode",
        "nats_deploy_status_put",
    )
    .await
}

async fn read_deploy_status(
    js: &jetstream::Context,
    deploy_id: &DeployId,
) -> Result<Option<DeployRecord>> {
    let bucket = kv_json::get_bucket(js, DEPLOY_STATUS_BUCKET, "nats_deploy_status_bucket").await?;
    let Some(bytes) = bucket
        .get(deploy_id.0.as_str())
        .await
        .map_err(|error| Error::operation("nats_deploy_status_get", format!("{error:?}")))?
    else {
        return Ok(None);
    };
    Ok(Some(kv_json::decode_json(
        "nats_deploy_status_decode",
        bytes.as_ref(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{DeployState, MachineId, ServiceRelease, ServiceRoutingPolicy};
    use ployz_types::spec::Namespace;

    #[test]
    fn cached_projection_extends_from_next_sequence() {
        assert_eq!(cached_projection_extension_start(10, 1, 12, 12), Some(11));
    }

    #[test]
    fn cached_projection_replays_when_cache_precedes_stream_start() {
        assert_eq!(cached_projection_extension_start(10, 12, 15, 4), None);
    }

    #[test]
    fn cached_projection_replays_when_stream_is_empty_or_not_ahead() {
        assert_eq!(cached_projection_extension_start(10, 0, 10, 0), None);
        assert_eq!(cached_projection_extension_start(10, 1, 10, 10), None);
    }

    #[test]
    fn unchanged_revision_still_emits_idempotent_routing_event() {
        let revision = revision("rev-a", "{}");

        let event = revision_routing_event(Some(revision.clone()), &revision);

        assert!(matches!(event, RoutingEvent::RevisionAdded(record) if record == revision));
    }

    #[test]
    fn changed_revision_emits_update_routing_event() {
        let old = revision("rev-a", "{\"old\":true}");
        let new = revision("rev-a", "{\"new\":true}");

        let event = revision_routing_event(Some(old.clone()), &new);

        assert!(matches!(
            event,
            RoutingEvent::RevisionUpdated { old: event_old, new: event_new }
                if event_old == old && event_new == new
        ));
    }

    #[test]
    fn duplicate_commit_repair_republishes_current_truth_for_touched_keys() {
        let namespace = Namespace(String::from("prod"));
        let old_release = release(&namespace, "web", "rev-old", "deploy-old");
        let revision = revision("rev-new", "{}");
        let new_release = release(&namespace, "api", "rev-new", "deploy-new");
        let command = deploy_commit(
            &namespace,
            "deploy-new",
            vec![revision.clone()],
            vec![String::from("web")],
            vec![new_release.clone()],
        );
        let mut before = DeployProjection::new();
        before.apply_commit(&deploy_commit(
            &namespace,
            "deploy-old",
            Vec::new(),
            Vec::new(),
            vec![old_release.clone()],
        ));
        let mut current = before.clone();
        current.apply_commit(&command);

        let events = repair_events_for_duplicate_commit(&before, &current, &command);

        assert!(events.contains(&RoutingEvent::RevisionAdded(revision)));
        assert!(events.contains(&RoutingEvent::ReleaseRemoved(old_release)));
        assert!(events.contains(&RoutingEvent::ReleaseAdded(new_release)));
    }

    #[test]
    fn duplicate_commit_repair_skips_release_superseded_by_later_commit() {
        let namespace = Namespace(String::from("prod"));
        let command_release = release(&namespace, "api", "rev-a", "deploy-a");
        let later_release = release(&namespace, "api", "rev-b", "deploy-b");
        let command = deploy_commit(
            &namespace,
            "deploy-a",
            Vec::new(),
            Vec::new(),
            vec![command_release.clone()],
        );
        let before = DeployProjection::new();
        let mut current = before.clone();
        current.apply_commit(&command);
        current.apply_commit(&deploy_commit(
            &namespace,
            "deploy-b",
            Vec::new(),
            Vec::new(),
            vec![later_release],
        ));

        let events = repair_events_for_duplicate_commit(&before, &current, &command);

        assert!(!events.contains(&RoutingEvent::ReleaseAdded(command_release)));
    }

    fn revision(hash: &str, spec_json: &str) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace(String::from("prod")),
            service: String::from("web"),
            revision_hash: hash.into(),
            spec_json: spec_json.into(),
            created_by: MachineId(String::from("founder")),
            created_at: 10,
        }
    }

    fn release(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
        deploy_id: &str,
    ) -> ServiceReleaseRecord {
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
                updated_by_deploy_id: DeployId(deploy_id.into()),
                updated_at: 1,
            },
        }
    }

    fn deploy_commit(
        namespace: &Namespace,
        deploy_id: &str,
        revisions: Vec<ServiceRevisionRecord>,
        removed_services: Vec<String>,
        releases: Vec<ServiceReleaseRecord>,
    ) -> DeployCommit {
        DeployCommit {
            namespace: namespace.clone(),
            revisions,
            removed_services,
            removed_volumes: Vec::new(),
            releases,
            volumes: Vec::new(),
            deploy: DeployRecord {
                deploy_id: DeployId(deploy_id.into()),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId(String::from("founder")),
                manifest_hash: String::from("manifest"),
                state: DeployState::Committed,
                started_at: 1,
                committed_at: Some(2),
                finished_at: Some(2),
                summary_json: String::from("{}"),
            },
        }
    }
}
