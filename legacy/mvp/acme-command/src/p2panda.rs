use async_trait::async_trait;
use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ChallengePublisher,
    AcmeHttp01OrderChallenge, AcmeIssuanceError,
};
use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_identity::VisibleNodes;
use mvp_lease::{LeaseState, LeaseTimestamp};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore};
use mvp_projection::ProjectionFactPayload;

use crate::{
    AcmeClaimCommand, AcmeClearHttp01Command, AcmeCommandError, AcmeCommandExecutor,
    AcmeFactWriter, AcmeLeaseHandle, AcmePresentHttp01Command, AcmePresentResult,
    ClaimCommandResult, lease_state_from_source,
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

pub struct PandaAcmeHttp01Publisher<'a> {
    adapter: &'a mut PandaAcmeCommandAdapter,
    store: &'a mut PandaFactStore,
}

impl<'a> PandaAcmeHttp01Publisher<'a> {
    #[must_use]
    pub fn new(adapter: &'a mut PandaAcmeCommandAdapter, store: &'a mut PandaFactStore) -> Self {
        Self { adapter, store }
    }
}

#[async_trait]
impl AcmeHttp01ChallengePublisher for PandaAcmeHttp01Publisher<'_> {
    async fn publish_http01(
        &mut self,
        challenge: AcmeHttp01OrderChallenge,
    ) -> Result<(), AcmeIssuanceError> {
        let id = AcmeChallengeId::new(challenge.hostname, challenge.token);
        let published_at = LeaseTimestamp::from_secs(
            challenge
                .expires_at_secs
                .saturating_sub(mvp_acme::DEFAULT_HTTP01_CHALLENGE_TTL_SECS),
        );
        let expires_at = LeaseTimestamp::from_secs(challenge.expires_at_secs);
        let claim = self
            .adapter
            .claim(
                self.store,
                AcmeClaimCommand::new(id.clone(), published_at, expires_at),
            )
            .await
            .map_err(command_error("acme_http01_claim"))?;
        self.adapter
            .present(
                self.store,
                AcmePresentHttp01Command::new(
                    id,
                    claim.into_lease(),
                    key_authorization_thumbprint(&challenge.key_authorization)?,
                    published_at,
                ),
            )
            .await
            .map_err(command_error("acme_http01_present"))?;
        Ok(())
    }

    async fn clear_http01(
        &mut self,
        hostname: &AcmeHostname,
        token: &AcmeChallengeToken,
        cleared_at_secs: u64,
    ) -> Result<(), AcmeIssuanceError> {
        let id = AcmeChallengeId::new(hostname.clone(), token.clone());
        let cleared_at = LeaseTimestamp::from_secs(cleared_at_secs);
        let lease = active_lease_handle(self.adapter.session(), &*self.store, &id, cleared_at)?;
        self.adapter
            .clear(
                self.store,
                AcmeClearHttp01Command::new(id, lease, cleared_at),
            )
            .await
            .map_err(command_error("acme_http01_clear"))?;
        Ok(())
    }
}

fn active_lease_handle(
    session: &BusSession,
    store: &PandaFactStore,
    id: &AcmeChallengeId,
    now: LeaseTimestamp,
) -> Result<AcmeLeaseHandle, AcmeIssuanceError> {
    match lease_state_from_source(store, session, id, now)
        .map_err(command_error("acme_lease_read"))?
    {
        LeaseState::Active { current, .. } => Ok(AcmeLeaseHandle::new(
            id.clone(),
            current.holder,
            current.epoch,
            current.content_hash,
        )),
        LeaseState::Vacant { .. } | LeaseState::Expired { .. } | LeaseState::Released { .. } => {
            Err(AcmeIssuanceError::Operation {
                operation: "acme_lease_read",
                message: format!("ACME HTTP-01 challenge lease is not active for {id:?}"),
            })
        }
    }
}

fn key_authorization_thumbprint(
    key_authorization: &mvp_acme::AcmeKeyAuthorization,
) -> Result<&str, AcmeIssuanceError> {
    let Some((_token, thumbprint)) = key_authorization.as_str().split_once('.') else {
        return Err(AcmeIssuanceError::Operation {
            operation: "acme_key_authorization",
            message: "key authorization did not contain an account thumbprint".to_string(),
        });
    };
    Ok(thumbprint)
}

fn command_error(
    operation: &'static str,
) -> impl FnOnce(AcmeCommandError) -> AcmeIssuanceError + Send + Sync + 'static {
    move |source| AcmeIssuanceError::Operation {
        operation,
        message: source.to_string(),
    }
}
