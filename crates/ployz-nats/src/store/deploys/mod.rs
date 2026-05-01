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
use crate::subjects::{self, DEPLOY_COMMITS_STREAM};

#[derive(Debug, Clone)]
pub(crate) struct CachedDeployProjection {
    projection: DeployProjection,
    last_sequence: u64,
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
        publish_revision(self.jetstream(), command).await?;
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
        publish_commit(self.jetstream(), command).await?;
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
        if let Some(cached) = self.deploy_projection.read().await.as_ref()
            && cached.last_sequence == info.state.last_sequence
        {
            return Ok(cached.projection.clone());
        }

        let cached = replay_projection_from_stream(&mut stream, info).await?;
        *self.deploy_projection.write().await = Some(cached.clone());
        Ok(cached.projection)
    }
}

pub async fn publish_commit(js: &jetstream::Context, commit: &DeployCommit) -> Result<()> {
    let subject = subjects::deploy_commit(&commit.namespace, &commit.deploy.deploy_id);
    let payload = serde_json::to_vec(commit)
        .map_err(|error| Error::operation("nats_deploy_commit_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(DEPLOY_COMMITS_STREAM)
        .expected_last_subject_sequence(0)
        .message_id(format!("deploy-commit:{}", commit.deploy.deploy_id.0));
    let ack = js
        .send_publish(subject, publish)
        .await
        .map_err(|error| Error::operation("nats_deploy_commit_publish", format!("{error:?}")))?;
    match ack.await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => Ok(()),
        Err(error) => Err(Error::operation(
            "nats_deploy_commit_ack",
            format!("{error:?}"),
        )),
    }
}

async fn replay_projection_from_stream(
    stream: &mut async_nats::jetstream::stream::Stream,
    info: async_nats::jetstream::stream::Info,
) -> Result<CachedDeployProjection> {
    let mut projection = DeployProjection::new();
    if info.state.messages == 0 {
        return Ok(CachedDeployProjection {
            projection,
            last_sequence: info.state.last_sequence,
        });
    }
    for sequence in info.state.first_sequence..=info.state.last_sequence {
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
    Ok(CachedDeployProjection {
        projection,
        last_sequence: info.state.last_sequence,
    })
}

async fn publish_revision(js: &jetstream::Context, command: &DeployRevisionUpsert) -> Result<()> {
    let revision = &command.revision;
    let subject = subjects::revision(
        &revision.namespace,
        &revision.service,
        &revision.revision_hash,
    );
    let payload = serde_json::to_vec(revision)
        .map_err(|error| Error::operation("nats_revision_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(crate::subjects::REVISIONS_STREAM)
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
    use ployz_types::model::MachineId;
    use ployz_types::spec::Namespace;

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
}
