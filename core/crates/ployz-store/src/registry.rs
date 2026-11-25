//! Registry logins: what a Machine pulls a Service's private image with, addressed as
//! `SERVICE.registryCredential`. Each Service identity owns one, sealed on a person's
//! device to the Cluster for one registry host and kept outside Working State, so
//! setting a new one rotates it at once. The Store never reads one.
//! Turning a login on or off for the Service is a staged change to its source.
//! Unsetting keeps the stored login, and `{"secret": true}` turns it back on.
//! Admission freezes the logins each Deployment pulls with.

use std::collections::BTreeMap;

use ployz_core::config::{
    SavedEnvironmentIntent, SavedServiceIntent, ServiceImageCredentials, ServiceSource,
};
use ployz_core::{RegistryAuth, RpcError, SealedLogin, ServiceName};
use serde_json::Value;

use crate::client_sealed::{PLAINTEXT_REFUSED, Sealer};
use crate::error;
use crate::followup::FollowUp;
use crate::id::EnvironmentId;
use crate::scope::Environment;
use crate::settings::{SecretInput, ServiceSetting};
use crate::storage::Tx;
use crate::storage::json_text;
use crate::{Actor, deployment::Frozen};

const SETTING: ServiceSetting = ServiceSetting::RegistryCredential;

/// What `set` changed: the Service's source (staged) and the stored login (immediate).
pub(crate) struct Changed {
    pub(crate) staged: bool,
    pub(crate) rotated: bool,
}

/// Set `service`'s login from a [`SealedLogin`], which replaces the stored one, or
/// `{"secret": true}`, which turns the stored one back on. A login's plain text is
/// refused, and `sealer` names the update when it came from the CLI.
pub(crate) fn set(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &mut Environment,
    service: &ServiceName,
    value: &Value,
    sealer: Sealer<'_>,
) -> Result<Changed, RpcError> {
    let new = input(value, sealer)?;
    let environment_id = environment.summary.id.clone();
    let node = environment.service_mut(service)?;
    let ServiceSource::Image { credentials, .. } = &mut node.config.source else {
        return Err(SETTING.invalid("only an image Service pulls with registry credentials"));
    };
    let rotated = match new {
        None if stored(tx, &environment_id, &node.id)?.is_none() => {
            return Err(SETTING.invalid("it holds no credential to keep: set one with --secret"));
        }
        None => false,
        // Two seals of one login differ, so every new one counts as rotated.
        Some(login) => {
            store(tx, who, &environment_id, &node.id, &login)?;
            true
        }
    };
    // Each node's login is its own, as core binds it.
    let configured = ServiceImageCredentials::Configured {
        credential_id: node.id.clone(),
    };
    let staged = *credentials != configured;
    *credentials = configured;
    Ok(Changed { staged, rotated })
}

/// A new sealed login, or none to keep the stored one. The value is never echoed.
fn input(value: &Value, sealer: Sealer<'_>) -> Result<Option<SealedLogin>, RpcError> {
    let invalid = || {
        SETTING.invalid(
            "expected a login sealed on your device, or {\"secret\": true} to keep the stored one; set one with --secret",
        )
    };
    // The CLI sends `web.registryCredential={"secret":true}` as JSON text.
    if let Some(text) = value.as_str() {
        return if SecretInput::keeps_by_text(text) {
            Ok(None)
        } else {
            Err(invalid())
        };
    }
    if let Ok(login) = serde_json::from_value::<SealedLogin>(value.clone()) {
        return Ok(Some(login));
    }
    if plaintext_login(value).is_some() {
        return Err(sealer.refuse("registryCredential", SETTING.invalid(PLAINTEXT_REFUSED)));
    }
    match value.get("secret").and_then(SecretInput::of) {
        Some(SecretInput::Keep) if value.as_object().is_some_and(|fields| fields.len() == 1) => {
            Ok(None)
        }
        Some(_) | None => Err(invalid()),
    }
}

/// The login `value` holds as plain text: `{"secret": "…"}`, nonempty, with an optional
/// username, as a CLI from before sealed logins sent one.
pub(crate) fn plaintext_login(value: &Value) -> Option<RegistryAuth> {
    let fields = value.as_object()?;
    if fields
        .keys()
        .any(|key| key != "username" && key != "secret")
    {
        return None;
    }
    let text = |name| match fields.get(name) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(Some(text.trim().to_owned())),
        Some(_) => None,
        None => Some(None),
    };
    Some(RegistryAuth {
        username: text("username")?,
        password: text("secret")??,
    })
}

/// Service `service_id`'s login in `environment`, if it has one.
pub(crate) fn stored(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service_id: &str,
) -> Result<Option<SealedLogin>, RpcError> {
    let rows = tx.query(
        "SELECT login FROM config_registry_login \
         WHERE environment_id = ?1 AND service_id = ?2",
        &[environment.as_str().into(), service_id.into()],
    )?;
    rows.first()
        .map(|row| row.json(0, "registry login"))
        .transpose()
}

/// Whether `service` pulls its image with a configured registry login.
pub(crate) fn uses_login(service: &SavedServiceIntent) -> bool {
    matches!(
        &service.config.source,
        ServiceSource::Image {
            credentials: ServiceImageCredentials::Configured { .. },
            ..
        }
    )
}

/// The sealed logins a Deployment of `saved` pulls with, by runtime Service, for every
/// targeted image Service that uses one.
pub(crate) fn freeze(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    frozen: &Frozen,
) -> Result<BTreeMap<ServiceName, SealedLogin>, RpcError> {
    let mut logins = BTreeMap::new();
    for service in &saved.services {
        if !uses_login(service) || !frozen.targets(&service.id) {
            continue;
        }
        let login = stored(tx, environment, &service.id)?.ok_or_else(|| {
            error::conflict(
                format!("{} has no registry credential: set one", service.slug),
                FollowUp::SetRegistryCredential {
                    service: service.slug.clone(),
                }
                .details(),
            )
        })?;
        logins.insert(service.config.private_dns.clone(), login);
    }
    Ok(logins)
}

/// Store `login` as Service `service_id`'s in `environment`, replacing any it had.
pub(crate) fn store(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    service_id: &str,
    login: &SealedLogin,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_registry_login \
         (environment_id, service_id, organization_id, login) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (environment_id, service_id) \
         DO UPDATE SET login = excluded.login",
        &[
            environment.as_str().into(),
            service_id.into(),
            who.organization.as_str().into(),
            json_text(login).as_str().into(),
        ],
    )?;
    Ok(())
}
