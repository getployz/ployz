use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_identity::VisibleNodes;
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore};
use mvp_projection::ProjectionFactPayload;

use crate::{
    AcmeClaimCommand, AcmeClearHttp01Command, AcmeCommandError, AcmeCommandExecutor,
    AcmeFactWriter, AcmePresentHttp01Command, AcmePresentResult, ClaimCommandResult,
};

pub struct PandaAcmeCommandAdapter {
    executor: AcmeCommandExecutor,
    author: PandaFactAuthor,
}

impl PandaAcmeCommandAdapter {
    #[must_use]
    pub fn new(session: BusSession, author: PandaFactAuthor, visible_nodes: VisibleNodes) -> Self {
        Self {
            executor: AcmeCommandExecutor::new(session, visible_nodes),
            author,
        }
    }

    #[must_use]
    pub fn session(&self) -> &BusSession {
        self.executor.session()
    }

    #[must_use]
    pub fn author(&self) -> &PandaFactAuthor {
        &self.author
    }

    pub async fn claim(
        &mut self,
        store: &mut PandaFactStore,
        command: AcmeClaimCommand,
    ) -> Result<ClaimCommandResult, AcmeCommandError> {
        let prepared = self.executor.prepare_claim(&*store, command)?;
        let mut writer = PandaFactWriter {
            store,
            author: &self.author,
        };
        writer.preflight(self.executor.session(), &prepared.key)?;
        writer
            .write(self.executor.session(), prepared.key, prepared.payload)
            .await?;
        Ok(prepared.result)
    }

    pub async fn present(
        &mut self,
        store: &mut PandaFactStore,
        command: AcmePresentHttp01Command,
    ) -> Result<AcmePresentResult, AcmeCommandError> {
        let prepared = self.executor.prepare_present(&*store, command)?;
        let mut writer = PandaFactWriter {
            store,
            author: &self.author,
        };
        writer.preflight(self.executor.session(), &prepared.key)?;
        writer
            .write(self.executor.session(), prepared.key, prepared.payload)
            .await?;
        Ok(prepared.result)
    }

    pub async fn clear(
        &mut self,
        store: &mut PandaFactStore,
        command: AcmeClearHttp01Command,
    ) -> Result<crate::AcmeClearResult, AcmeCommandError> {
        let prepared = self.executor.prepare_clear(&*store, command)?;
        let mut writer = PandaFactWriter {
            store,
            author: &self.author,
        };
        writer.preflight(self.executor.session(), &prepared.release.key)?;
        writer.preflight(self.executor.session(), &prepared.clear.key)?;
        writer
            .write(
                self.executor.session(),
                prepared.release.key,
                prepared.release.payload,
            )
            .await?;
        writer
            .write(
                self.executor.session(),
                prepared.clear.key,
                prepared.clear.payload,
            )
            .await?;
        Ok(prepared.result)
    }
}

struct PandaFactWriter<'a> {
    store: &'a mut PandaFactStore,
    author: &'a PandaFactAuthor,
}

impl AcmeFactWriter for PandaFactWriter<'_> {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> Result<(), AcmeCommandError> {
        if self.store.can_write_fact(session, key) {
            return Ok(());
        }
        Err(AcmeCommandError::UnauthorizedFactWrite {
            principal: session.principal().clone(),
            key: key.clone(),
        })
    }

    async fn write(
        &mut self,
        session: &BusSession,
        key: FactKey,
        payload: ProjectionFactPayload,
    ) -> Result<(), AcmeCommandError> {
        let payload = FactPayload::copy_from_slice(&payload.to_fact_bytes().map_err(|error| {
            AcmeCommandError::ProjectionPayload {
                operation: "encode",
                message: error.to_string(),
            }
        })?);
        self.store
            .write_fact_payload(session, self.author, key, payload)
            .await
            .map(|_| ())
            .map_err(AcmeCommandError::from)
    }
}
