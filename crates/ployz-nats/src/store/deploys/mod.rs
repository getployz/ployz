use async_nats::jetstream;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use async_nats::jetstream::stream::DirectGetErrorKind;
use async_trait::async_trait;
use ployz_store_api::{DeployCommit, DeployCommitFacts, DeployStore};
use ployz_types::error::{Error, Result, StoreRecordKind};
use ployz_types::model::{
    DeployId, DeployRecord, RoutingEvent, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;

use crate::NatsStore;
use crate::buckets::NatsAssetNames;
use crate::store::kv_json;
use crate::subjects::{self, NatsScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployCommitPublish {
    Created,
    Existing,
}

#[async_trait]
impl DeployStore for NatsStore {
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        Ok(self.deploy_commit_facts().await?.revisions(namespace))
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        Ok(self.deploy_commit_facts().await?.releases(namespace))
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        Ok(self.deploy_commit_facts().await?.volumes(namespace))
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        Ok(self
            .deploy_commit_facts()
            .await?
            .volume(namespace, volume_name)
            .cloned())
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        let publish = publish_commit_in(self.jetstream(), self.scope(), command).await?;
        let routing_events = match publish {
            DeployCommitPublish::Created => {
                let mut commit_facts = DeployCommitFacts::new();
                commit_facts.apply_commit_events(command)
            }
            DeployCommitPublish::Existing => {
                duplicate_commit_repair_events(self.jetstream(), self.scope(), command).await?
            }
        };
        self.publish_routing_events(
            format!("deploy:{}", command.deploy.deploy_id.0),
            "deploy.commit",
            &routing_events,
        )
        .await
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> Result<()> {
        write_deploy_status_entry(
            self.jetstream(),
            self.assets().deploy_status_bucket.as_str(),
            deploy,
        )
        .await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        read_deploy_status(
            self.jetstream(),
            self.assets().deploy_status_bucket.as_str(),
            deploy_id,
        )
        .await
    }
}

impl NatsStore {
    pub(crate) async fn deploy_commit_facts(&self) -> Result<DeployCommitFacts> {
        let mut stream = self
            .jetstream()
            .get_stream(self.assets().deploy_commits_stream.as_str())
            .await
            .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
        let info = stream
            .info()
            .await
            .map_err(|error| Error::operation("nats_deploy_stream_info", format!("{error:?}")))?
            .clone();
        replay_commit_facts_from_stream(&mut stream, info).await
    }
}

async fn publish_commit_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    commit: &DeployCommit,
) -> Result<DeployCommitPublish> {
    let subject = subjects::deploy_commit_in(scope, &commit.namespace, &commit.deploy.deploy_id);
    let stream = NatsAssetNames::new(scope).deploy_commits_stream;
    let payload = serde_json::to_vec(commit)
        .map_err(|error| Error::operation("nats_deploy_commit_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(stream.as_str())
        .expected_last_subject_sequence(0)
        .message_id(format!("deploy-commit:{}", commit.deploy.deploy_id.0));
    let ack = js
        .send_publish(subject.clone(), publish)
        .await
        .map_err(|error| Error::operation("nats_deploy_commit_publish", format!("{error:?}")))?;
    match ack.await {
        Ok(ack) if ack.duplicate => existing_commit_publish(js, &stream, subject, commit).await,
        Ok(_ack) => Ok(DeployCommitPublish::Created),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => {
            existing_commit_publish(js, &stream, subject, commit).await
        }
        Err(error) => Err(Error::operation(
            "nats_deploy_commit_ack",
            format!("{error:?}"),
        )),
    }
}

async fn existing_commit_publish(
    js: &jetstream::Context,
    stream_name: &str,
    subject: String,
    commit: &DeployCommit,
) -> Result<DeployCommitPublish> {
    let stream = js
        .get_stream(stream_name)
        .await
        .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
    let message = stream
        .direct_get_last_for_subject(subject)
        .await
        .map_err(|error| Error::operation("nats_deploy_commit_get", format!("{error:?}")))?;
    let stored: DeployCommit = serde_json::from_slice(message.payload.as_ref())
        .map_err(|error| Error::operation("nats_deploy_commit_decode", error.to_string()))?;
    if stored != *commit {
        return Err(Error::operation(
            "nats_deploy_commit_conflict",
            format!(
                "deploy commit '{}' already exists with different payload",
                commit.deploy.deploy_id
            ),
        ));
    }
    Ok(DeployCommitPublish::Existing)
}

async fn duplicate_commit_repair_events(
    js: &jetstream::Context,
    scope: &NatsScope,
    command: &DeployCommit,
) -> Result<Vec<RoutingEvent>> {
    let stream_name = NatsAssetNames::new(scope).deploy_commits_stream;
    let mut stream = js
        .get_stream(stream_name.as_str())
        .await
        .map_err(|error| Error::operation("nats_deploy_stream", format!("{error:?}")))?;
    let info = stream
        .info()
        .await
        .map_err(|error| Error::operation("nats_deploy_stream_info", format!("{error:?}")))?
        .clone();
    let current = replay_commit_facts_from_stream(&mut stream, info).await?;
    Ok(repair_events_for_duplicate_commit(&current, command))
}

fn repair_events_for_duplicate_commit(
    current: &DeployCommitFacts,
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
            events.push(RoutingEvent::RevisionUpsert(revision.clone()));
        }
    }
    for service in &command.removed_services {
        if current.release(&command.namespace, service).is_none() {
            events.push(RoutingEvent::ReleaseRemoved {
                namespace: command.namespace.clone(),
                service: service.clone(),
            });
        }
    }
    for release in &command.releases {
        if current.release(&release.namespace, &release.service) == Some(release) {
            events.push(RoutingEvent::ReleaseUpsert(release.clone()));
        }
    }
    events
}

async fn replay_commit_facts_from_stream(
    stream: &mut async_nats::jetstream::stream::Stream,
    info: async_nats::jetstream::stream::Info,
) -> Result<DeployCommitFacts> {
    let mut facts = DeployCommitFacts::default();
    if info.state.messages > 0 {
        apply_commit_fact_range(
            stream,
            &mut facts,
            info.state.first_sequence,
            info.state.last_sequence,
        )
        .await?;
    }
    Ok(facts)
}

async fn apply_commit_fact_range(
    stream: &mut async_nats::jetstream::stream::Stream,
    facts: &mut DeployCommitFacts,
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
        let _events = facts.apply_commit_events(&commit);
    }
    Ok(())
}

async fn write_deploy_status_entry(
    js: &jetstream::Context,
    bucket_name: &str,
    deploy: &DeployRecord,
) -> Result<()> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_deploy_status_bucket").await?;
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
    bucket_name: &str,
    deploy_id: &DeployId,
) -> Result<Option<DeployRecord>> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_deploy_status_bucket").await?;
    let Some(bytes) = bucket
        .get(deploy_id.0.as_str())
        .await
        .map_err(|error| Error::operation("nats_deploy_status_get", format!("{error:?}")))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_deploy_status(&deploy_id.0, bytes.as_ref())?))
}

fn decode_deploy_status(key: &str, bytes: &[u8]) -> Result<DeployRecord> {
    let record: DeployRecord = kv_json::decode_json("nats_deploy_status_decode", bytes)?;
    if record.deploy_id.0 != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::DeployStatus,
            key,
            record.deploy_id.0,
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::error::{Error, StoreRecordKind};
    use ployz_types::model::{
        DeployState, MachineId, ServiceRelease, ServiceRevisionRecord, ServiceRoutingPolicy,
    };
    use ployz_types::spec::Namespace;

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
        let mut before = DeployCommitFacts::default();
        before.apply_commit_events(&deploy_commit(
            &namespace,
            "deploy-old",
            Vec::new(),
            Vec::new(),
            vec![old_release.clone()],
        ));
        let mut current = before.clone();
        current.apply_commit_events(&command);

        let events = repair_events_for_duplicate_commit(&current, &command);

        assert!(events.contains(&RoutingEvent::RevisionUpsert(revision)));
        assert!(events.contains(&RoutingEvent::ReleaseRemoved {
            namespace: old_release.namespace,
            service: old_release.service,
        }));
        assert!(events.contains(&RoutingEvent::ReleaseUpsert(new_release)));
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
        let mut current = DeployCommitFacts::default();
        current.apply_commit_events(&command);
        current.apply_commit_events(&deploy_commit(
            &namespace,
            "deploy-b",
            Vec::new(),
            Vec::new(),
            vec![later_release],
        ));

        let events = repair_events_for_duplicate_commit(&current, &command);

        assert!(!events.contains(&RoutingEvent::ReleaseUpsert(command_release)));
    }

    #[test]
    fn deploy_status_decode_rejects_malformed_payload() {
        let error = decode_deploy_status("deploy-a", b"{")
            .expect_err("malformed deploy status should fail");

        assert!(error.to_string().contains("nats_deploy_status_decode"));
    }

    #[test]
    fn deploy_status_decode_rejects_key_payload_mismatch() {
        let namespace = Namespace(String::from("prod"));
        let record = deploy_commit(
            &namespace,
            "payload-deploy",
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .deploy;
        let bytes = serde_json::to_vec(&record).expect("encode deploy status");

        let error = decode_deploy_status("key-deploy", &bytes)
            .expect_err("deploy status key mismatch should fail");

        assert_eq!(
            error,
            Error::store_key_mismatch(
                StoreRecordKind::DeployStatus,
                "key-deploy",
                "payload-deploy"
            )
        );
    }

    #[test]
    fn deploy_status_decode_accepts_matching_key() {
        let namespace = Namespace(String::from("prod"));
        let record =
            deploy_commit(&namespace, "deploy-a", Vec::new(), Vec::new(), Vec::new()).deploy;
        let bytes = serde_json::to_vec(&record).expect("encode deploy status");

        let decoded = decode_deploy_status("deploy-a", &bytes).expect("matching status key");

        assert_eq!(decoded, record);
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
