use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mvp_bus::BusSession;
use mvp_environment::{
    EnvironmentBranchFact, EnvironmentError, EnvironmentFactWriter, EnvironmentHeadFact,
    EnvironmentPromoteDecisionFact, EnvironmentResult, EnvironmentRollbackDecisionFact,
    WrittenEnvironmentFact, environment_branch_fact_key, environment_branch_fact_payload,
    environment_head_fact_key, environment_head_fact_payload,
    environment_promote_decision_fact_key, environment_promote_decision_fact_payload,
    environment_rollback_decision_fact_key, environment_rollback_decision_fact_payload,
};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactWriteOutcome, SharedPandaFactStore};

#[derive(Clone)]
pub struct PandaEnvironmentFactWriter {
    facts: SharedPandaFactStore,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl PandaEnvironmentFactWriter {
    #[must_use]
    pub fn new(
        facts: SharedPandaFactStore,
        session: BusSession,
        author: Arc<PandaFactAuthor>,
    ) -> Self {
        Self {
            facts,
            session,
            author,
        }
    }
}

impl EnvironmentFactWriter for PandaEnvironmentFactWriter {
    fn write_head<'a>(
        &'a self,
        fact: EnvironmentHeadFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_head_fact_key(&fact.environment, fact.epoch)?;
            let payload = environment_head_fact_payload(&fact)?;
            self.write_payload(key, payload).await
        })
    }

    fn write_branch<'a>(
        &'a self,
        fact: EnvironmentBranchFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_branch_fact_key(&fact.source_environment, &fact.branch_id)?;
            let payload = environment_branch_fact_payload(&fact)?;
            self.write_payload(key, payload).await
        })
    }

    fn write_promote_decision<'a>(
        &'a self,
        fact: EnvironmentPromoteDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_promote_decision_fact_key(&fact.environment, &fact.command_id)?;
            let payload = environment_promote_decision_fact_payload(&fact)?;
            self.write_payload(key, payload).await
        })
    }

    fn write_rollback_decision<'a>(
        &'a self,
        fact: EnvironmentRollbackDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_rollback_decision_fact_key(&fact.environment, &fact.command_id)?;
            let payload = environment_rollback_decision_fact_payload(&fact)?;
            self.write_payload(key, payload).await
        })
    }
}

impl PandaEnvironmentFactWriter {
    async fn write_payload(
        &self,
        key: mvp_bus::FactKey,
        payload: mvp_bus::FactPayload,
    ) -> EnvironmentResult<WrittenEnvironmentFact> {
        let outcome = self
            .facts
            .write_fact_payload(&self.session, self.author.as_ref(), key, payload)
            .await
            .map_err(|error| EnvironmentError::FactWrite {
                message: error.to_string(),
            })?;
        environment_fact_outcome(outcome)
    }
}

fn environment_fact_outcome(
    outcome: PandaFactWriteOutcome,
) -> EnvironmentResult<WrittenEnvironmentFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata)
        | PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenEnvironmentFact {
            key: metadata.key().clone(),
        }),
        PandaFactWriteOutcome::Conflict(metadata) => Err(EnvironmentError::FactConflict {
            key: metadata.key().clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mvp_bus::{FactKeyPattern, Grant, IslandId, PrincipalId};
    use mvp_environment::{
        EnvironmentBranchFact, EnvironmentBranchId, EnvironmentCommandId, EnvironmentEpoch,
        EnvironmentFactWriter, EnvironmentHeadFact, EnvironmentHeadId, EnvironmentId,
        EnvironmentPromoteDecisionFact, EnvironmentRollbackDecisionFact, EnvironmentRouteRef,
        EnvironmentVolumeRef, environment_branch_fact_key, environment_head_fact_key,
        environment_promote_decision_fact_key, environment_rollback_decision_fact_key,
    };
    use mvp_identity::{NodeId, VisibleNodes};
    use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, SharedPandaFactStore};
    use mvp_routing::ServingCommitId;

    use crate::PandaEnvironmentFactWriter;

    #[tokio::test]
    async fn writer_records_environment_facts_and_repeats_as_already_present() {
        let Fixture {
            store,
            writer_a,
            author_a,
            ..
        } = fixture();
        let writer = PandaEnvironmentFactWriter::new(store.clone(), writer_a, author_a);
        let head = head("prod", 1, "head-1", "serving-1", vec!["db-prod"]);
        let branch = branch("prod", "pr-1");
        let promote = promote_decision(&head);
        let rollback = rollback_decision(&head);

        let inserted = writer.write_head(head.clone()).await.expect("write head");
        let repeated = writer.write_head(head.clone()).await.expect("repeat head");
        let branch_write = writer
            .write_branch(branch.clone())
            .await
            .expect("write branch");
        let promote_write = writer
            .write_promote_decision(promote.clone())
            .await
            .expect("write promote decision");
        let rollback_write = writer
            .write_rollback_decision(rollback.clone())
            .await
            .expect("write rollback decision");

        assert_eq!(
            inserted.key,
            environment_head_fact_key(&head.environment, head.epoch).expect("head key")
        );
        assert_eq!(
            branch_write.key,
            environment_branch_fact_key(&branch.source_environment, &branch.branch_id)
                .expect("branch key")
        );
        assert_eq!(
            promote_write.key,
            environment_promote_decision_fact_key(&promote.environment, &promote.command_id)
                .expect("promote key")
        );
        assert_eq!(
            rollback_write.key,
            environment_rollback_decision_fact_key(&rollback.environment, &rollback.command_id)
                .expect("rollback key")
        );
        assert_eq!(repeated.key, inserted.key);
        assert_eq!(store.export_operations().await.len(), 4);
    }

    #[tokio::test]
    async fn writer_maps_conflict_to_environment_fact_conflict() {
        let Fixture {
            store,
            writer_a,
            writer_b,
            author_a,
            author_b,
            ..
        } = fixture();
        let writer_a = PandaEnvironmentFactWriter::new(store.clone(), writer_a, author_a);
        let writer_b = PandaEnvironmentFactWriter::new(store, writer_b, author_b);
        let original = head("prod", 1, "head-1", "serving-1", vec!["db-prod"]);
        let conflict = head("prod", 1, "head-conflict", "serving-2", vec!["db-other"]);
        writer_a
            .write_head(original.clone())
            .await
            .expect("write original");

        let error = writer_b
            .write_head(conflict)
            .await
            .expect_err("conflicting head");

        assert!(matches!(
            error,
            mvp_environment::EnvironmentError::FactConflict { key }
                if key == environment_head_fact_key(&original.environment, original.epoch)
                    .expect("head key")
        ));
    }

    #[tokio::test]
    async fn read_principal_cannot_write_environment_facts() {
        let Fixture {
            store,
            reader,
            reader_author,
            ..
        } = fixture();
        let writer = PandaEnvironmentFactWriter::new(store, reader, reader_author);
        let error = writer
            .write_head(head("prod", 1, "head-1", "serving-1", vec!["db-prod"]))
            .await
            .expect_err("read principal cannot write");

        assert!(matches!(
            error,
            mvp_environment::EnvironmentError::FactWrite { .. }
        ));
    }

    struct Fixture {
        store: SharedPandaFactStore,
        writer_a: mvp_bus::BusSession,
        writer_b: mvp_bus::BusSession,
        reader: mvp_bus::BusSession,
        author_a: Arc<PandaFactAuthor>,
        author_b: Arc<PandaFactAuthor>,
        reader_author: Arc<PandaFactAuthor>,
    }

    fn fixture() -> Fixture {
        let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let write_grant = Grant::empty().with_fact_write(
            FactKeyPattern::parse("/facts/environment/>").expect("environment pattern"),
        );
        let read_grant = Grant::empty();
        let session_a = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("environment-a"),
            write_grant.clone(),
        );
        let session_b = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("environment-b"),
            write_grant,
        );
        let read_session = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("environment-reader"),
            read_grant,
        );
        Fixture {
            store: SharedPandaFactStore::new(PandaFactStore::new(Arc::new(raw_bus))),
            writer_a: session_a.clone(),
            writer_b: session_b.clone(),
            reader: read_session.clone(),
            author_a: Arc::new(PandaFactAuthor::new(session_a.principal().clone())),
            author_b: Arc::new(PandaFactAuthor::new(session_b.principal().clone())),
            reader_author: Arc::new(PandaFactAuthor::new(read_session.principal().clone())),
        }
    }

    fn head(
        environment: &str,
        epoch: u64,
        head_id: &str,
        serving: &str,
        volumes: Vec<&str>,
    ) -> EnvironmentHeadFact {
        EnvironmentHeadFact {
            environment: EnvironmentId::parse(environment).expect("environment id"),
            epoch: EnvironmentEpoch::new(epoch).expect("epoch"),
            head_id: EnvironmentHeadId::parse(head_id).expect("head id"),
            source_command_id: EnvironmentCommandId::parse(format!("cmd-{head_id}"))
                .expect("command id"),
            serving_commit_id: ServingCommitId::new(serving),
            previous_head: None,
            volume_refs: volumes
                .into_iter()
                .map(|volume| EnvironmentVolumeRef::parse(volume).expect("volume ref"))
                .collect(),
            source_branch_id: None,
        }
    }

    fn branch(environment: &str, branch_id: &str) -> EnvironmentBranchFact {
        EnvironmentBranchFact {
            source_environment: EnvironmentId::parse(environment).expect("environment id"),
            source_epoch: EnvironmentEpoch::new(1).expect("epoch"),
            source_head_id: EnvironmentHeadId::parse("head-1").expect("head id"),
            branch_id: EnvironmentBranchId::parse(branch_id).expect("branch id"),
            branch_environment: EnvironmentId::parse(branch_id).expect("branch environment"),
            route_refs: vec![EnvironmentRouteRef::parse("route-1").expect("route ref")],
            forked_volume_refs: vec![EnvironmentVolumeRef::parse("db-branch").expect("volume ref")],
            visible_nodes: VisibleNodes::new([NodeId::new("node-1")]),
        }
    }

    fn promote_decision(head: &EnvironmentHeadFact) -> EnvironmentPromoteDecisionFact {
        EnvironmentPromoteDecisionFact {
            environment: head.environment.clone(),
            command_id: EnvironmentCommandId::parse("promote-1").expect("command id"),
            previous_head: head.reference(),
            branch_head: head.reference(),
            target_serving_commit_id: ServingCommitId::new("serving-promote-1"),
            target_volume_refs: head.volume_refs.clone(),
            visible_nodes: VisibleNodes::new([NodeId::new("node-1")]),
        }
    }

    fn rollback_decision(head: &EnvironmentHeadFact) -> EnvironmentRollbackDecisionFact {
        EnvironmentRollbackDecisionFact {
            environment: head.environment.clone(),
            command_id: EnvironmentCommandId::parse("rollback-1").expect("command id"),
            current_head: head.reference(),
            rollback_target: head.reference(),
            target_serving_commit_id: ServingCommitId::new("serving-rollback-1"),
            target_volume_refs: head.volume_refs.clone(),
            visible_nodes: VisibleNodes::new([NodeId::new("node-1")]),
        }
    }
}
