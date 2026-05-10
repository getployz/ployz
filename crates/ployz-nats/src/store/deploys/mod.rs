use async_nats::jetstream;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::kv;
use async_nats::jetstream::message::PublishMessage;
use async_nats::jetstream::stream::DirectGetErrorKind;
use async_trait::async_trait;
use ployz_store_api::{DeployCommit, DeployCommitFacts, DeployStore};
use ployz_types::error::{DeployError, Error, Result, StoreError, StoreRecordKind};
use ployz_types::model::{
    DeployId, DeployPhaseId, DeployPhaseRecord, DeployPhaseState, DeployRecord,
    PreparedDeployRecord, PreparedDeployState, RoutingEvent, ServiceBranchLineageRecord,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeBranchLineageRecord, VolumeMovementRecord,
    VolumeRecord,
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

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceBranchLineageRecord>> {
        Ok(self
            .deploy_commit_facts()
            .await?
            .service_branch_lineage(namespace))
    }

    async fn list_volume_movements(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<VolumeMovementRecord>> {
        Ok(self
            .deploy_commit_facts()
            .await?
            .volume_movements(namespace))
    }

    async fn list_volume_branches(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<VolumeBranchLineageRecord>> {
        Ok(self.deploy_commit_facts().await?.volume_branches(namespace))
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
        .map_err(|error| {
            Error::Store(StoreError::DeployCommitRoutingPublishFailed {
                deploy_id: command.deploy.deploy_id.0.clone(),
                message: error.to_string(),
            })
        })
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

    async fn write_prepared_deploy(&self, prepared: &PreparedDeployRecord) -> Result<()> {
        write_prepared_deploy_entry(
            self.jetstream(),
            self.assets().prepared_deploys_bucket.as_str(),
            prepared,
        )
        .await
    }

    async fn get_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
    ) -> Result<Option<PreparedDeployRecord>> {
        read_prepared_deploy(
            self.jetstream(),
            self.assets().prepared_deploys_bucket.as_str(),
            prepared_deploy_id,
        )
        .await
    }

    async fn mark_prepared_deploy_applied(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        transition_prepared_deploy_entry(
            self.jetstream(),
            self.assets().prepared_deploys_bucket.as_str(),
            prepared_deploy_id,
            PreparedDeployState::Applied,
            updated_at,
        )
        .await
    }

    async fn expire_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        transition_prepared_deploy_entry(
            self.jetstream(),
            self.assets().prepared_deploys_bucket.as_str(),
            prepared_deploy_id,
            PreparedDeployState::Expired,
            updated_at,
        )
        .await
    }

    async fn supersede_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        transition_prepared_deploy_entry(
            self.jetstream(),
            self.assets().prepared_deploys_bucket.as_str(),
            prepared_deploy_id,
            PreparedDeployState::Superseded,
            updated_at,
        )
        .await
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> Result<()> {
        write_deploy_phase_entry(
            self.jetstream(),
            self.assets().deploy_phases_bucket.as_str(),
            phase,
        )
        .await
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> Result<Option<DeployPhaseRecord>> {
        let mut phase = read_deploy_phase(
            self.jetstream(),
            self.assets().deploy_phases_bucket.as_str(),
            namespace,
            deploy_id,
            phase_id,
        )
        .await?;
        if let Some(phase) = phase.as_mut() {
            apply_phase_commit_fact(
                phase,
                self.deploy_commit_facts()
                    .await?
                    .phase_commit(namespace, deploy_id, phase_id),
            );
        }
        Ok(phase)
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<Vec<DeployPhaseRecord>> {
        let mut phases = list_deploy_phases(
            self.jetstream(),
            self.assets().deploy_phases_bucket.as_str(),
            namespace,
            deploy_id,
        )
        .await?;
        let facts = self.deploy_commit_facts().await?;
        for phase in &mut phases {
            apply_phase_commit_fact(
                phase,
                facts.phase_commit(namespace, deploy_id, &phase.phase_id),
            );
        }
        Ok(phases)
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

async fn write_prepared_deploy_entry(
    js: &jetstream::Context,
    bucket_name: &str,
    prepared: &PreparedDeployRecord,
) -> Result<()> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_prepared_deploys_bucket").await?;
    kv_json::put_json(
        &bucket,
        &prepared.prepared_deploy_id.0,
        prepared,
        "nats_prepared_deploy_encode",
        "nats_prepared_deploy_put",
    )
    .await
}

async fn read_prepared_deploy(
    js: &jetstream::Context,
    bucket_name: &str,
    prepared_deploy_id: &DeployId,
) -> Result<Option<PreparedDeployRecord>> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_prepared_deploys_bucket").await?;
    let Some(bytes) = bucket
        .get(prepared_deploy_id.0.as_str())
        .await
        .map_err(|error| Error::operation("nats_prepared_deploy_get", format!("{error:?}")))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_prepared_deploy(
        &prepared_deploy_id.0,
        bytes.as_ref(),
    )?))
}

async fn transition_prepared_deploy_entry(
    js: &jetstream::Context,
    bucket_name: &str,
    prepared_deploy_id: &DeployId,
    state: PreparedDeployState,
    updated_at: u64,
) -> Result<PreparedDeployRecord> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_prepared_deploys_bucket").await?;
    let Some(entry) = bucket
        .entry(prepared_deploy_id.0.as_str())
        .await
        .map_err(|error| Error::operation("nats_prepared_deploy_get", format!("{error:?}")))?
    else {
        return Err(Error::operation(
            "nats_prepared_deploy_state",
            format!("prepared deploy '{prepared_deploy_id}' not found"),
        ));
    };
    if entry.operation != kv::Operation::Put {
        return Err(Error::operation(
            "nats_prepared_deploy_state",
            format!("prepared deploy '{prepared_deploy_id}' not found"),
        ));
    }
    let mut record = decode_prepared_deploy(&prepared_deploy_id.0, entry.value.as_ref())?;
    if record.state != PreparedDeployState::Prepared {
        return Err(Error::Deploy(DeployError::PreparedDeployNotApplicable {
            prepared_deploy_id: prepared_deploy_id.0.clone(),
            state: record.state,
        }));
    }
    record.state = state;
    record.updated_at = updated_at;
    let payload = serde_json::to_vec(&record)
        .map_err(|error| Error::operation("nats_prepared_deploy_encode", error.to_string()))?;
    bucket
        .update(
            prepared_deploy_id.0.as_str(),
            payload.into(),
            entry.revision,
        )
        .await
        .map_err(|error| Error::operation("nats_prepared_deploy_update", format!("{error:?}")))?;
    Ok(record)
}

fn decode_prepared_deploy(key: &str, bytes: &[u8]) -> Result<PreparedDeployRecord> {
    let record: PreparedDeployRecord = kv_json::decode_json("nats_prepared_deploy_decode", bytes)?;
    if record.prepared_deploy_id.0 != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::PreparedDeploy,
            key,
            record.prepared_deploy_id.0,
        ));
    }
    Ok(record)
}

async fn write_deploy_phase_entry(
    js: &jetstream::Context,
    bucket_name: &str,
    phase: &DeployPhaseRecord,
) -> Result<()> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_deploy_phases_bucket").await?;
    kv_json::put_json(
        &bucket,
        &deploy_phase_key(&phase.namespace, &phase.deploy_id, &phase.phase_id),
        phase,
        "nats_deploy_phase_encode",
        "nats_deploy_phase_put",
    )
    .await
}

async fn read_deploy_phase(
    js: &jetstream::Context,
    bucket_name: &str,
    namespace: &Namespace,
    deploy_id: &DeployId,
    phase_id: &DeployPhaseId,
) -> Result<Option<DeployPhaseRecord>> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_deploy_phases_bucket").await?;
    let key = deploy_phase_key(namespace, deploy_id, phase_id);
    let Some(bytes) = bucket
        .get(key.as_str())
        .await
        .map_err(|error| Error::operation("nats_deploy_phase_get", format!("{error:?}")))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_deploy_phase(&key, bytes.as_ref())?))
}

async fn list_deploy_phases(
    js: &jetstream::Context,
    bucket_name: &str,
    namespace: &Namespace,
    deploy_id: &DeployId,
) -> Result<Vec<DeployPhaseRecord>> {
    let bucket = kv_json::get_bucket(js, bucket_name, "nats_deploy_phases_bucket").await?;
    let prefix = deploy_phase_prefix(namespace, deploy_id);
    let mut phases = Vec::new();
    for key in kv_json::list_keys_with_prefix(&bucket, &prefix, "nats_deploy_phase_keys").await? {
        let Some(entry) = bucket
            .entry(key)
            .await
            .map_err(|error| Error::operation("nats_deploy_phase_entry", format!("{error:?}")))?
        else {
            continue;
        };
        if entry.operation != async_nats::jetstream::kv::Operation::Put {
            continue;
        }
        phases.push(decode_deploy_phase(&entry.key, entry.value.as_ref())?);
    }
    sort_deploy_phases(&mut phases);
    Ok(phases)
}

fn decode_deploy_phase(key: &str, bytes: &[u8]) -> Result<DeployPhaseRecord> {
    let record: DeployPhaseRecord = kv_json::decode_json("nats_deploy_phase_decode", bytes)?;
    let payload_key = deploy_phase_key(&record.namespace, &record.deploy_id, &record.phase_id);
    if payload_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::DeployPhase,
            key,
            payload_key,
        ));
    }
    Ok(record)
}

fn deploy_phase_key(
    namespace: &Namespace,
    deploy_id: &DeployId,
    phase_id: &DeployPhaseId,
) -> String {
    format!(
        "{}.{}.{}",
        subjects::subject_token(namespace.0.as_str()),
        subjects::subject_token(deploy_id.0.as_str()),
        subjects::subject_token(phase_id.0.as_str())
    )
}

fn deploy_phase_prefix(namespace: &Namespace, deploy_id: &DeployId) -> String {
    format!(
        "{}.{}.",
        subjects::subject_token(namespace.0.as_str()),
        subjects::subject_token(deploy_id.0.as_str())
    )
}

fn sort_deploy_phases(phases: &mut [DeployPhaseRecord]) {
    phases.sort_by(|left, right| {
        (
            left.namespace.0.as_str(),
            left.deploy_id.0.as_str(),
            left.order,
            left.phase_id.0.as_str(),
        )
            .cmp(&(
                right.namespace.0.as_str(),
                right.deploy_id.0.as_str(),
                right.order,
                right.phase_id.0.as_str(),
            ))
    });
}

fn apply_phase_commit_fact(
    phase: &mut DeployPhaseRecord,
    commit: Option<&ployz_types::model::DeployPhaseCommitRecord>,
) {
    let Some(commit) = commit else {
        return;
    };
    phase.commit_deploy_id = Some(commit.commit_deploy_id.clone());
    phase.state = DeployPhaseState::Succeeded {
        completed_at: commit.committed_at,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::error::{Error, StoreRecordKind};
    use ployz_types::model::{
        DeployPhaseCommitPolicy, DeployPhaseId, DeployPhaseRecord, DeployPhaseRollbackPolicy,
        DeployPhaseState, DeployPreview, DeployPreviewBaseline, DeployPreviewBaselineComponents,
        DeployState, MachineId, PreparedDeployRecord, PreparedDeployState, ServiceRelease,
        ServiceRevisionRecord, ServiceRoutingPolicy,
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

    #[test]
    fn prepared_deploy_decode_rejects_key_payload_mismatch() {
        let namespace = Namespace(String::from("prod"));
        let record = prepared_deploy(&namespace, "payload-prepare");
        let bytes = serde_json::to_vec(&record).expect("encode prepared deploy");

        let error = decode_prepared_deploy("key-prepare", &bytes)
            .expect_err("prepared deploy key mismatch should fail");

        assert_eq!(
            error,
            Error::store_key_mismatch(
                StoreRecordKind::PreparedDeploy,
                "key-prepare",
                "payload-prepare"
            )
        );
    }

    #[test]
    fn prepared_deploy_decode_accepts_matching_key() {
        let namespace = Namespace(String::from("prod"));
        let record = prepared_deploy(&namespace, "prepare-a");
        let bytes = serde_json::to_vec(&record).expect("encode prepared deploy");

        let decoded = decode_prepared_deploy("prepare-a", &bytes).expect("matching prepared key");

        assert_eq!(decoded, record);
    }

    #[test]
    fn deploy_phase_decode_rejects_malformed_payload() {
        let error = decode_deploy_phase("prod.deploy-a.deploy", b"{")
            .expect_err("malformed deploy phase should fail");

        assert!(error.to_string().contains("nats_deploy_phase_decode"));
    }

    #[test]
    fn deploy_phase_decode_rejects_key_payload_mismatch() {
        let record = deploy_phase("prod", "deploy-a", "deploy", 0);
        let bytes = serde_json::to_vec(&record).expect("encode deploy phase");

        let error = decode_deploy_phase("prod.deploy-a.other", &bytes)
            .expect_err("deploy phase key mismatch should fail");

        assert!(matches!(
            error,
            Error::Store(ployz_types::error::StoreError::KeyMismatch {
                record: StoreRecordKind::DeployPhase,
                ..
            })
        ));
    }

    #[test]
    fn deploy_phase_decode_accepts_matching_key() {
        let record = deploy_phase("prod", "deploy-a", "deploy", 0);
        let key = deploy_phase_key(&record.namespace, &record.deploy_id, &record.phase_id);
        let bytes = serde_json::to_vec(&record).expect("encode deploy phase");

        let decoded = decode_deploy_phase(&key, &bytes).expect("matching phase key");

        assert_eq!(decoded, record);
    }

    #[test]
    fn sort_deploy_phases_orders_by_contract_identity() {
        let mut phases = vec![
            deploy_phase("prod", "deploy-a", "web", 1),
            deploy_phase("prod", "deploy-a", "db", 0),
            deploy_phase("prod", "deploy-b", "deploy", 0),
        ];

        sort_deploy_phases(&mut phases);

        assert_eq!(
            phases
                .iter()
                .map(|phase| (
                    phase.deploy_id.0.as_str(),
                    phase.order,
                    phase.phase_id.0.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("deploy-a", 0, "db"),
                ("deploy-a", 1, "web"),
                ("deploy-b", 0, "deploy")
            ]
        );
    }

    #[test]
    fn phase_commit_fact_overrides_failed_phase_state() {
        let namespace = Namespace(String::from("prod"));
        let deploy_id = DeployId(String::from("deploy-a"));
        let phase_id = DeployPhaseId(String::from("db"));
        let commit = ployz_types::model::DeployPhaseCommitRecord {
            namespace: namespace.clone(),
            deploy_id: deploy_id.clone(),
            phase_id: phase_id.clone(),
            commit_deploy_id: DeployId(String::from("deploy-a-db-commit")),
            committed_at: 42,
        };
        let mut phase = deploy_phase("prod", "deploy-a", "db", 0);
        phase.state = DeployPhaseState::Failed {
            completed_at: 40,
            failure: ployz_types::model::DeployPhaseFailure {
                code: String::from("COMMIT_PUBLISH_FAILED"),
                message: String::from("routing publish failed after durable commit"),
            },
        };

        apply_phase_commit_fact(&mut phase, Some(&commit));

        assert_eq!(
            phase.commit_deploy_id,
            Some(DeployId(String::from("deploy-a-db-commit")))
        );
        assert_eq!(
            phase.state,
            DeployPhaseState::Succeeded { completed_at: 42 }
        );
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

    fn deploy_phase(
        namespace: &str,
        deploy_id: &str,
        phase_id: &str,
        order: u32,
    ) -> DeployPhaseRecord {
        DeployPhaseRecord {
            namespace: Namespace(namespace.into()),
            deploy_id: DeployId(deploy_id.into()),
            phase_id: DeployPhaseId(phase_id.into()),
            commit_deploy_id: None,
            name: phase_id.into(),
            order,
            after: Vec::new(),
            participants: Vec::new(),
            work: Vec::new(),
            state: DeployPhaseState::Running,
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: ployz_types::model::DeployPhaseAdvancePolicy::Immediate,
            started_at: 1,
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
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
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

    fn prepared_deploy(namespace: &Namespace, prepared_deploy_id: &str) -> PreparedDeployRecord {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });
        PreparedDeployRecord {
            prepared_deploy_id: DeployId(prepared_deploy_id.into()),
            namespace: namespace.clone(),
            manifest_hash: "manifest".into(),
            manifest_json: "{}".into(),
            preview: DeployPreview {
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                baseline: Some(baseline.clone()),
                participants: Vec::new(),
                phases: Vec::new(),
                services: Vec::new(),
                service_sources: Vec::new(),
                service_source_fingerprint: String::new(),
                service_branch_sources: Vec::new(),
                volume_moves: Vec::new(),
                volume_clones: Vec::new(),
                volume_clone_preflights: Vec::new(),
                warnings: Vec::new(),
            },
            baseline,
            coordinator_machine_id: MachineId("founder".into()),
            state: PreparedDeployState::Prepared,
            created_at: 1,
            expires_at: 100,
            updated_at: 1,
        }
    }
}
