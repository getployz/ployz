use std::future::Future;
use std::pin::Pin;

use mvp_bus::{BusActorHandle, BusSession, FactContentHash, FactKey, FactWriteOutcome};
use mvp_routing::{ServingCommitFacts, ServingCommitPlan, serving_commit_fact_key};

use crate::{DeployError, DeployResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenServingFact {
    key: FactKey,
    content_hash: FactContentHash,
    status: ServingFactWriteStatus,
}

impl WrittenServingFact {
    pub(crate) fn inserted(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: ServingFactWriteStatus::Inserted,
        }
    }

    pub(crate) fn already_present(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: ServingFactWriteStatus::AlreadyPresent,
        }
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn content_hash(&self) -> &FactContentHash {
        &self.content_hash
    }

    #[must_use]
    pub fn status(&self) -> ServingFactWriteStatus {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingFactWriteStatus {
    Inserted,
    AlreadyPresent,
}

pub trait ServingFactWriter: Send + Sync {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenServingFact>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct BusServingFactWriter {
    bus: BusActorHandle,
    session: BusSession,
}

impl BusServingFactWriter {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession) -> Self {
        Self { bus, session }
    }
}

impl ServingFactWriter for BusServingFactWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = serving_commit_fact_key(&commit.serving_commit_id)?;
            let facts = write_serving_commit(&self.bus, &self.session, commit).await?;
            match facts.serving {
                FactWriteOutcome::Inserted(fact) => Ok(WrittenServingFact::inserted(
                    key,
                    fact.content_hash().clone(),
                )),
                FactWriteOutcome::AlreadyPresent(fact) => Ok(WrittenServingFact::already_present(
                    key,
                    fact.content_hash().clone(),
                )),
                FactWriteOutcome::Conflict(_) => Err(DeployError::ServingFactConflict { key }),
            }
        })
    }
}

pub async fn write_serving_commit(
    bus: &BusActorHandle,
    session: &BusSession,
    commit: &ServingCommitPlan,
) -> DeployResult<ServingCommitFacts> {
    mvp_routing::write_serving_commit(bus, session, commit)
        .await
        .map_err(DeployError::from)
}
