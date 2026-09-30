//! Registry credentials: what a Machine pulls a Service's private image with,
//! addressed as `SERVICE.registryCredential`. Each Service identity owns one,
//! sealed and kept outside Working State, so setting a new one rotates it at once.
//! Turning credentials on or off for the Service is a staged change to its source.
//! Unsetting keeps the stored credential, and `{"secret": true}` turns it back on.
//! Admission freezes the credentials each Deployment pulls with.

use std::collections::BTreeMap;

use ployz_core::config::{
    EncryptedSecretValue, SavedEnvironmentIntent, ServiceImageCredentials, ServiceSource,
};
use ployz_core::{RegistryAuth, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error;
use crate::id::EnvironmentId;
use crate::scope::Environment;
use crate::sealing::SealingKey;
use crate::settings::ServiceSetting;
use crate::storage::Tx;
use crate::{Actor, deployment::Frozen};

const SETTING: ServiceSetting = ServiceSetting::RegistryCredential;

/// A credential as sealed: one JSON document.
#[derive(Serialize, Deserialize, PartialEq)]
struct Credential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    secret: String,
}

/// What `set` changed: the Service's source (staged) and the stored credential (immediate).
pub(crate) struct Changed {
    pub(crate) staged: bool,
    pub(crate) rotated: bool,
}

/// Set `service`'s credential from `{"username"?: …, "secret": "…"}`, which seals
/// and stores a new one, or `{"secret": true}`, which turns the stored one back on.
pub(crate) fn set(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &mut Environment,
    service: &ServiceName,
    value: &Value,
    sealing: &SealingKey,
) -> Result<Changed, RpcError> {
    let new = input(value)?;
    let environment_id = environment.summary.id.clone();
    let node = environment.service_mut(service)?;
    let ServiceSource::Image { credentials, .. } = &mut node.config.source else {
        return Err(SETTING.invalid("only an image Service pulls with registry credentials"));
    };
    let stored = sealed(tx, &environment_id, &node.id)?;
    let rotated = match (new, &stored) {
        (None, None) => {
            return Err(SETTING.invalid("it holds no credential to keep: set one with --secret"));
        }
        (None, Some(_)) => false,
        (Some(new), stored) => {
            let unchanged = match stored {
                Some(stored) => {
                    serde_json::from_str::<Credential>(&sealing.open(stored)?)
                        .map_err(|_| error::corrupt("registry credential"))?
                        == new
                }
                None => false,
            };
            if !unchanged {
                let sealed = sealing
                    .seal(&serde_json::to_string(&new).expect("a registry credential is JSON"));
                store(tx, who, &environment_id, &node.id, &sealed)?;
            }
            !unchanged
        }
    };
    // Each node's credential is its own, as core binds it.
    let configured = ServiceImageCredentials::Configured {
        credential_id: node.id.clone(),
    };
    let staged = *credentials != configured;
    *credentials = configured;
    Ok(Changed { staged, rotated })
}

/// A new credential, or none to keep the stored one. The value is never echoed.
fn input(value: &Value) -> Result<Option<Credential>, RpcError> {
    let invalid = || {
        SETTING.invalid(
            "expected {\"secret\": \"…\"} with an optional username, or {\"secret\": true} to keep the stored one; a secret comes from --secret or --patch -",
        )
    };
    let fields = value.as_object().ok_or_else(invalid)?;
    if fields
        .keys()
        .any(|key| key != "username" && key != "secret")
    {
        return Err(invalid());
    }
    let username = match fields.get("username") {
        None => None,
        Some(Value::String(username))
            if !username.trim().is_empty() && username.chars().count() <= 255 =>
        {
            Some(username.trim().to_owned())
        }
        Some(_) => return Err(SETTING.invalid("username is text of at most 255 characters")),
    };
    match fields.get("secret") {
        Some(Value::Bool(true)) if username.is_none() => Ok(None),
        Some(Value::Bool(true)) => {
            Err(SETTING.invalid("a new username needs its secret: send both, with --patch -"))
        }
        Some(Value::String(secret)) if !secret.trim().is_empty() => Ok(Some(Credential {
            username,
            secret: secret.trim().to_owned(),
        })),
        Some(_) | None => Err(invalid()),
    }
}

/// Service `service_id`'s credential in `environment`, sealed, if it has one.
pub(crate) fn sealed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service_id: &str,
) -> Result<Option<EncryptedSecretValue>, RpcError> {
    let rows = tx.query(
        "SELECT credential FROM config_registry_credential \
         WHERE environment_id = ?1 AND service_id = ?2",
        &[environment.as_str().into(), service_id.into()],
    )?;
    rows.first()
        .map(|row| row.json(0, "registry credential"))
        .transpose()
}

/// The sealed credentials a Deployment of `saved` pulls with, by runtime Service,
/// for every targeted image Service that uses one.
pub(crate) fn freeze(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    frozen: &Frozen,
) -> Result<BTreeMap<ServiceName, EncryptedSecretValue>, RpcError> {
    let mut credentials = BTreeMap::new();
    for service in &saved.services {
        let ServiceSource::Image {
            credentials: ServiceImageCredentials::Configured { .. },
            ..
        } = &service.config.source
        else {
            continue;
        };
        if !frozen.targets(&service.id) {
            continue;
        }
        let sealed = sealed(tx, environment, &service.id)?.ok_or_else(|| {
            error::conflict(
                format!("{} has no registry credential: set one", service.slug),
                json!({ "next": format!("ployz set {}.registryCredential --secret", service.slug) }),
            )
        })?;
        credentials.insert(service.config.private_dns.clone(), sealed);
    }
    Ok(credentials)
}

/// Store `sealed` as Service `service_id`'s credential in `environment`, replacing
/// any it had.
pub(crate) fn store(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    service_id: &str,
    sealed: &EncryptedSecretValue,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_registry_credential \
         (environment_id, service_id, organization_id, credential) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (environment_id, service_id) \
         DO UPDATE SET credential = excluded.credential",
        &[
            environment.as_str().into(),
            service_id.into(),
            who.organization.as_str().into(),
            serde_json::to_string(sealed)
                .expect("a sealed credential is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

/// Open frozen credentials for the runner that claimed their Deployment.
pub(crate) fn unseal(
    frozen: BTreeMap<ServiceName, EncryptedSecretValue>,
    sealing: &SealingKey,
) -> Result<BTreeMap<ServiceName, RegistryAuth>, RpcError> {
    frozen
        .into_iter()
        .map(|(service, sealed)| {
            let credential: Credential = serde_json::from_str(&sealing.open(&sealed)?)
                .map_err(|_| error::corrupt("registry credential"))?;
            Ok((
                service,
                RegistryAuth {
                    username: credential.username,
                    password: credential.secret,
                },
            ))
        })
        .collect()
}
