//! Seals a write's new secrets in this CLI once its Organization seals on the client.
//! Cloud says whether it does and to which key, asked once per command and only for
//! a write that carries plain text; the Store says where each secret lands. Off, the
//! write goes exactly as it always has. With the in-process Store, a Standalone
//! Cluster's, the CLI seals to the key the Cluster's Servers hold.

use std::cell::Cell;
use std::time::Duration;

use ployz_core::{RpcErrorCode, SealedIdentity, SealingPublicKey, seal_value};
use ployz_store::{
    Ask as _, Edit, EnvironmentSummary, SealingRecipient, SealingService, SealingTargetsQuery,
    SealingTargetsView, SecretValue, SyncChanges, View,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{Backend, Error, Store};
use crate::cloud_account::{self, StoreCallError};
use crate::context::ContextError;
use crate::sealing_backfill::BackfillSealingKeyReport;
use crate::ui::Hint;

/// What Cloud answers about sealing one write's secrets.
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Sealing {
    /// The Organization seals in the Store: the write goes as it always has.
    Off,
    /// It seals on the client, but no Server holds a Sealing Key yet.
    NoServer,
    /// It seals on the client, to this key, and the write's secrets land here.
    #[serde(rename_all = "camelCase")]
    Ready {
        public_key: SealingPublicKey,
        targets: Box<View>,
    },
}

impl Backend {
    fn sealing(&self, query: &SealingTargetsQuery) -> Result<Sealing, StoreCallError> {
        let Self::Cloud(runtime, credential) = self else {
            return Ok(Sealing::Off);
        };
        let answer = runtime.block_on(cloud_account::config_store_if_routed(
            credential,
            cloud_account::Via::Cli,
            "sealing",
            query,
        ))?;
        // Mixed-version: a Cloud from before this CLI sealed has no such route, and
        // its Store seals. Delete with `ployz_store::old_cli`.
        Ok(answer.unwrap_or(Sealing::Off))
    }
}

/// The key a write's secrets seal to: Cloud's answer, or for the in-process Store
/// the one its Cluster's Servers hold, read once a secret is ready to seal.
type Recipient = Cell<Option<SealingPublicKey>>;

/// How long the CLI waits for the Cluster's Servers to name their Sealing Key.
const SEALING_KEY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the CLI waits to settle a key no Server holds yet, as Cloud's backfill does.
const SEALING_KEY_SETTLE_TIMEOUT: Duration = Duration::from_secs(60);

impl Store<'_> {
    /// The Sealing public key this Standalone Cluster's Servers hold, read from one
    /// Runtime Watch frame as Cloud reads it. When no Server holds one, as in a Cluster
    /// founded before Sealing Keys, the CLI settles it as Cloud does: carried from a
    /// holder, or generated on the candidate the backfill names. None while no Server
    /// is known.
    fn cluster_sealing_key(&self) -> Result<Option<SealingPublicKey>, StoreCallError> {
        let runtime = super::runtime().map_err(StoreCallError::Stopped)?;
        let observe = async {
            match super::super::connect_context(self.matches, self.context()).await {
                Ok(client) => {
                    let frame = client.runtime_frame().await.map_err(Error::from)?;
                    Ok(Some((client, frame)))
                }
                Err(error)
                    if matches!(
                        error.context_error(),
                        Some(
                            ContextError::NoConfig
                                | ContextError::NoContexts(_)
                                | ContextError::NoConnections { .. }
                        )
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        };
        let observed = runtime
            .block_on(async { tokio::time::timeout(SEALING_KEY_TIMEOUT, observe).await })
            .unwrap_or_else(|_| Err(unanswered()));
        let Some((mut client, frame)) = observed.map_err(StoreCallError::Stopped)? else {
            return Ok(None);
        };
        if frame.machines.is_empty() {
            return Ok(None);
        }
        let mut held =
            frame
                .machines
                .iter()
                .filter_map(|observed| match observed.machine.runtime.sealing {
                    Some(ployz_core::Sealing::Held { public_key }) => Some(public_key),
                    _ => None,
                });
        let first = held.next();
        if let Some(key) = first.filter(|key| held.all(|other| other == *key)) {
            return Ok(Some(key));
        }
        let settled = runtime
            .block_on(async {
                tokio::time::timeout(
                    SEALING_KEY_SETTLE_TIMEOUT,
                    crate::sealing_backfill::settle(&mut client),
                )
                .await
            })
            .map_err(|_| StoreCallError::Stopped(unanswered()))?
            .map_err(|error| StoreCallError::Stopped(Error::from(error)))?;
        settled
            .map(Some)
            .map_err(|stopped| StoreCallError::Stopped(no_key(&stopped)))
    }

    /// The key `recipient` names, read from the Cluster on first use.
    fn key(&self, recipient: &Recipient) -> Result<SealingPublicKey, StoreCallError> {
        if let Some(key) = recipient.get() {
            return Ok(key);
        }
        let key = self
            .cluster_sealing_key()?
            .ok_or_else(|| StoreCallError::Stopped(no_server()))?;
        recipient.set(Some(key));
        Ok(key)
    }

    /// Seal for the Cluster each secret a CLI before 0.3.5 sealed with the in-process
    /// Store's key, as Cloud's worker does for its Store: no Machine opens one. Whether
    /// it resealed any; none without a key to seal to, and the Deploy says what waits.
    pub(crate) fn reseal(&self) -> Result<bool, StoreCallError> {
        let Backend::Local(store, who) = &self.backend else {
            return Ok(false);
        };
        let environments = store
            .reseal_environments(&who.organization)
            .map_err(StoreCallError::Refused)?;
        if environments.is_empty() {
            return Ok(false);
        }
        let Some(public_key) = self.cluster_sealing_key()? else {
            return Ok(false);
        };
        let recipient = SealingRecipient { public_key };
        let mut resealed = 0;
        for environment in environments {
            resealed += store
                .reseal(&environment, &recipient)
                .map_err(StoreCallError::Refused)?
                .resealed;
        }
        Ok(resealed > 0)
    }

    /// `edit` with each new secret's plain text sealed for the Service it sets, when
    /// the Organization seals on the client. `fail` maps what Cloud or the Store
    /// refuses, as it maps the write's own refusal.
    ///
    /// # Errors
    /// `conflict` when the Organization seals on the client but has no Server; what
    /// Cloud or the Store refused, the text included.
    pub(crate) fn seal_edit(
        &self,
        mut edit: Edit,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<Edit, Error> {
        if !edit.carries_plaintext_secret() {
            return Ok(edit);
        }
        let query = SealingTargetsQuery::Edit {
            environment: edit.environment.clone(),
        };
        let Some((recipient, targets)) = self.ready(&query, &fail)? else {
            return Ok(edit);
        };
        let SealingTargetsView::Edit {
            organization,
            environment,
            services,
        } = targets
        else {
            return Err(unexpected("an Edit's targets"));
        };
        let environment_id = bound(&environment)?;
        let refused = |error| fail(StoreCallError::Refused(error));
        edit.map_plaintext_secrets(refused, |secret| {
            let service =
                SealingService::bind(&services, &environment, secret.service(), secret.key())
                    .map_err(refused)?;
            let identity = SealedIdentity {
                organization: organization.clone(),
                environment: environment_id,
                service,
            };
            let plaintext = secret.admit().map_err(refused)?;
            let key = self.key(&recipient).map_err(&fail)?;
            seal(&key, &identity, secret.key(), plaintext).map(Some)
        })?;
        Ok(edit)
    }

    /// `request` with each plain-text value sealed for the receiver's Service its row
    /// lands in, when the Organization seals on the client. `fail` maps what Cloud or
    /// the Store refuses, as it maps the write's own refusal.
    ///
    /// # Errors
    /// As [`Self::seal_edit`].
    pub(crate) fn seal_sync_values(
        &self,
        mut request: SyncChanges,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<SyncChanges, Error> {
        let rows: Vec<_> = request
            .values
            .iter()
            .filter(|(_, value)| matches!(value, SecretValue::Text(_)))
            .map(|(asked, _)| asked.clone())
            .collect();
        if rows.is_empty() {
            return Ok(request);
        }
        let query = SealingTargetsQuery::Sync {
            from: request.from.clone(),
            into: request.into.clone(),
            version: request.version.clone(),
            rows,
        };
        let Some((recipient, targets)) = self.ready(&query, &fail)? else {
            return Ok(request);
        };
        let SealingTargetsView::Sync {
            organization,
            into,
            values: targets,
            ..
        } = targets
        else {
            return Err(unexpected("a Sync's targets"));
        };
        let environment = bound(&into)?;
        for (asked, value) in &mut request.values {
            let SecretValue::Text(plaintext) = value else {
                continue;
            };
            let Some(target) = targets.iter().find(|target| &target.asked == asked) else {
                return Err(unexpected(&format!("where {asked} lands")));
            };
            let identity = SealedIdentity {
                organization: organization.clone(),
                environment,
                service: target.service,
            };
            let plaintext = target
                .admit(plaintext)
                .map_err(|error| fail(StoreCallError::Refused(error)))?;
            let key = self.key(&recipient).map_err(&fail)?;
            *value = SecretValue::ClientSealed(seal(&key, &identity, &target.key, plaintext)?);
        }
        Ok(request)
    }

    /// The key to seal to and where `query`'s secrets land; `None` when the Store seals.
    fn ready(
        &self,
        query: &SealingTargetsQuery,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<Option<(Recipient, SealingTargetsView)>, Error> {
        if let Backend::Local(..) = &self.backend {
            let targets = self.backend.read(query).map_err(&fail)?;
            return Ok(Some((Cell::new(None), targets)));
        }
        match self.backend.sealing(query).map_err(&fail)? {
            Sealing::Off => Ok(None),
            Sealing::NoServer => Err(no_server()),
            Sealing::Ready {
                public_key,
                targets,
            } => {
                let targets = SealingTargetsQuery::view(*targets)
                    .map_err(|error| fail(StoreCallError::Refused(error)))?;
                Ok(Some((Cell::new(Some(public_key)), targets)))
            }
        }
    }
}

fn unanswered() -> Error {
    Error::unavailable(
        "No Server answered with the Cluster's Sealing Key, so nothing was sealed. \
         Start a Server and try again.",
    )
}

/// Why the Cluster's Servers have no Sealing Key to seal to, as the backfill stopped.
fn no_key(stopped: &BackfillSealingKeyReport) -> Error {
    let why = serde_json::to_value(stopped)
        .ok()
        .and_then(|report| report.get("result")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    let error = Error::conflict(format!(
        "This Cluster's Servers hold no Sealing Key to seal to ({why}), so nothing was sealed."
    ));
    if matches!(stopped, BackfillSealingKeyReport::AwaitsRecovery { .. }) {
        error.hint(Hint::Next("ployz recover".into()))
    } else {
        error
    }
}

fn no_server() -> Error {
    Error::conflict("Sealed values need a server.").hint(Hint::Next("ployz server add".into()))
}

fn seal(
    recipient: &SealingPublicKey,
    identity: &SealedIdentity,
    key: &str,
    plaintext: &str,
) -> Result<ployz_core::config::ClientSealedValue, Error> {
    seal_value(recipient, identity, key, plaintext).map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            format!("{key}: could not seal the value"),
            error,
        )
    })
}

/// The Environment's ID as a sealed value binds to it.
fn bound(environment: &EnvironmentSummary) -> Result<Uuid, Error> {
    Uuid::parse_str(environment.id.as_str()).map_err(|_| {
        Error::conflict(format!(
            "{}/{} has no ID a sealed value binds to",
            environment.project, environment.name
        ))
    })
}

fn unexpected(what: &str) -> Error {
    Error::coded(
        RpcErrorCode::Internal,
        format!("Cloud answered with something other than {what}"),
    )
}
