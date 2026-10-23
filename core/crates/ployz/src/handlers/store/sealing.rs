//! Seals a write's new secrets in this CLI once its Organization seals on the client.
//! Cloud says whether it does and to which key, asked once per command and only for
//! a write that carries plain text; the Store says where each secret lands. Off, the
//! write goes exactly as it always has. The in-process Store seals every secret itself.

use ployz_core::{RpcError, RpcErrorCode, SealedIdentity, SealingPublicKey};
use ployz_store::{
    Ask as _, Edit, EnvironmentSummary, SealingTargetsQuery, SealingTargetsView, SecretValue,
    SyncChanges, View,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{Backend, Error, Store};
use crate::cloud_account::{self, StoreCallError};
use crate::sealing::seal_value;
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

impl Store<'_> {
    /// `edit` with each new secret's plain text sealed for the Service it sets, when
    /// the Organization seals on the client.
    ///
    /// # Errors
    /// `conflict` when the Organization seals on the client but has no Server; what
    /// Cloud or the Store refused, the text included.
    pub(crate) fn seal_edit(&self, mut edit: Edit) -> Result<Edit, Error> {
        let fail = |error| self.fail(error);
        let mut plain = false;
        edit.map_plaintext_secrets(|_| {
            plain = true;
            Ok::<_, RpcError>(None)
        })?;
        if !plain {
            return Ok(edit);
        }
        let query = SealingTargetsQuery::Edit {
            environment: edit.environment.clone(),
        };
        let Some((recipient, targets)) = self.ready(&query, fail)? else {
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
        edit.map_plaintext_secrets(|secret| {
            let service = secret.service();
            let Some(target) = services.iter().find(|target| &target.name == service) else {
                return Err(Error::coded(
                    RpcErrorCode::NotFound,
                    format!(
                        "No Service named {service} in {}/{}",
                        environment.project, environment.name
                    ),
                ));
            };
            let identity = SealedIdentity {
                organization: organization.clone(),
                environment: environment_id,
                service: target.service,
            };
            let plaintext = secret
                .admit()
                .map_err(|error| fail(StoreCallError::Refused(error)))?;
            seal(&recipient, &identity, secret.key(), plaintext).map(Some)
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
            *value =
                SecretValue::ClientSealed(seal(&recipient, &identity, &target.key, plaintext)?);
        }
        Ok(request)
    }

    /// The key to seal to and where `query`'s secrets land; `None` when the Store seals.
    fn ready(
        &self,
        query: &SealingTargetsQuery,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<Option<(SealingPublicKey, SealingTargetsView)>, Error> {
        match self.backend.sealing(query).map_err(&fail)? {
            Sealing::Off => Ok(None),
            Sealing::NoServer => Err(Error::conflict("Sealed values need a server.")
                .hint(Hint::Next("ployz server add".into()))),
            Sealing::Ready {
                public_key,
                targets,
            } => {
                let targets = SealingTargetsQuery::view(*targets)
                    .map_err(|error| fail(StoreCallError::Refused(error)))?;
                Ok(Some((public_key, targets)))
            }
        }
    }
}

fn seal(
    recipient: &SealingPublicKey,
    identity: &SealedIdentity,
    key: &str,
    plaintext: &str,
) -> Result<ployz_core::config::ClientSealedValue, Error> {
    seal_value(recipient, identity, key, plaintext)
        .map_err(|error| Error::coded(RpcErrorCode::Internal, format!("{key}: {error}")))
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
