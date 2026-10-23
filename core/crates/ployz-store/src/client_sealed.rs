//! Values the browser sealed to one Environment's Sealing Key, which the Store
//! carries as ciphertext and never opens. Off for every Organization until its
//! `config_sealed_variables` row exists, and nothing here inserts one.

use ployz_core::config::{
    ClientSealedValue, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, SealedSecret, SecretCiphertext,
};
use ployz_core::{RpcError, SealedScope, SealedValue};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;
use uuid::Uuid;

use crate::storage::Tx;
use crate::variables::{TOO_LONG, VariableKey, too_long};
use crate::{EnvironmentId, OrganizationId, SealingKey};

/// A secret's new value: plain text the Store seals, or a value the browser sealed
/// for the Environment it lands in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum SecretValue {
    Text(String),
    ClientSealed(ClientSealedValue),
}

impl From<String> for SecretValue {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for SecretValue {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl SecretValue {
    /// What Review compares the value by, without showing it.
    pub(crate) fn fingerprint(&self, sealing: &SealingKey) -> String {
        match self {
            Self::Text(text) => sealing.fingerprint(text),
            Self::ClientSealed(value) => fingerprint(value),
        }
    }

    pub(crate) const fn is_client_sealed(&self) -> bool {
        matches!(self, Self::ClientSealed(_))
    }
}

/// Whether `organization` takes client-sealed values, read in the caller's transaction.
///
/// # Errors
/// Returns a storage error.
pub(crate) fn enabled(tx: &mut dyn Tx, organization: &OrganizationId) -> Result<bool, RpcError> {
    let rows = tx.query(
        "SELECT 1 FROM config_sealed_variables WHERE organization_id = ?1",
        &[organization.as_str().into()],
    )?;
    Ok(!rows.is_empty())
}

/// Prefix of a client-sealed value's fingerprint. Two seals of one plaintext differ,
/// so a value entered again shows as changed: the Store can't tell them equal.
const FINGERPRINT_VERSION: &str = "c1";

/// The fingerprint a client-sealed value is compared by: SHA-256 of its ciphertext.
pub(crate) fn fingerprint(value: &ClientSealedValue) -> String {
    let hash = digest(&SHA256, value.value.sealed.as_bytes());
    format!("{FINGERPRINT_VERSION}:{}", hex::encode(hash))
}

/// What HPKE adds to a plaintext: the encapsulated key and the tag.
const HPKE_OVERHEAD: usize = 48;

/// The length of the plaintext `value` opens to.
pub(crate) fn plaintext_len(value: &SealedValue) -> usize {
    value.sealed.as_bytes().len().saturating_sub(HPKE_OVERHEAD)
}

/// Why PORT never holds a client-sealed value: Ployz reads it to route traffic.
pub(crate) const PORT_REFUSED: &str = "can't be sealed: Ployz reads it to route traffic";

/// Refuse a PORT holding a value the browser sealed. Every write of Working State
/// comes through here, so neither a rename nor History brings one in.
///
/// # Errors
/// Returns `invalid_argument` naming the Service's PORT.
pub(crate) fn refuse_port(working: &SavedEnvironmentIntent) -> Result<(), RpcError> {
    match working.services.iter().find(|service| sealed_port(service)) {
        Some(service) => Err(crate::error::invalid(
            format!("PORT: {PORT_REFUSED}"),
            json!({ "service": service.slug, "variable": "PORT" }),
        )),
        None => Ok(()),
    }
}

/// Whether `service`'s PORT holds a value the browser sealed.
fn sealed_port(service: &SavedServiceIntent) -> bool {
    service
        .variables
        .iter()
        .any(|variable| variable.key == "PORT" && browser_sealed(variable))
}

/// Whether `variable` holds a value the browser sealed.
pub(crate) fn browser_sealed(variable: &SavedVariableIntent) -> bool {
    matches!(
        variable.value,
        SavedVariableValue::Secret {
            encrypted_value: Some(SecretCiphertext::ClientSealed(_))
        }
    )
}

/// Where a client-sealed value lands, and whether its Organization takes one.
pub(crate) struct Receiver<'a> {
    pub(crate) environment: &'a EnvironmentId,
    pub(crate) service: &'a SavedServiceIntent,
    pub(crate) enabled: bool,
}

/// `value` as variable `key` of the receiver holds it, once it is sealed for exactly
/// that variable there. `refuse` names the variable; no refusal shows the value.
/// Plaintext checks (`validate_text`, typed addresses) can't run on ciphertext.
///
/// # Errors
/// Returns `invalid_argument` when the Organization doesn't take client-sealed
/// values, when the value is sealed for another Environment, Service or key, when
/// the receiver has no UUID to bind to, or when the ciphertext is too long.
pub(crate) fn accept(
    value: ClientSealedValue,
    key: &VariableKey,
    receiver: &Receiver<'_>,
    refuse: impl Fn(&str) -> RpcError,
) -> Result<SealedSecret, RpcError> {
    if !receiver.enabled {
        return Err(refuse(
            "Sealed values from the browser are off for this Organization",
        ));
    }
    let (Ok(environment), Ok(service)) = (
        Uuid::parse_str(receiver.environment.as_str()),
        Uuid::parse_str(&receiver.service.id),
    ) else {
        return Err(refuse("this Service has no ID a sealed value binds to"));
    };
    if value.sealed_for != environment {
        return Err(refuse("sealed for another Environment"));
    }
    if value.value.key != key.as_str() {
        return Err(refuse("sealed for another variable"));
    }
    if key.as_str() == "PORT" {
        return Err(refuse(PORT_REFUSED));
    }
    match value.value.scope {
        SealedScope::Service(scope) if scope == service => {}
        SealedScope::Service(_) => return Err(refuse("sealed for another Service")),
        SealedScope::Shared => {
            return Err(refuse("sealed for every Service; seal it for this one"));
        }
    }
    if too_long(key.as_str(), plaintext_len(&value.value)) {
        return Err(refuse(TOO_LONG));
    }
    Ok(SealedSecret {
        fingerprint: fingerprint(&value),
        value: SecretCiphertext::ClientSealed(value),
    })
}

#[cfg(test)]
mod tests {
    use ployz_core::Ciphertext;

    use super::*;

    fn value(sealed_for: u128, service: u128, key: &str, bytes: Vec<u8>) -> ClientSealedValue {
        ClientSealedValue {
            sealed_for: Uuid::from_u128(sealed_for),
            value: SealedValue {
                sealed: Ciphertext::from_bytes(bytes),
                scope: SealedScope::Service(Uuid::from_u128(service)),
                key: key.into(),
            },
        }
    }

    fn web() -> SavedServiceIntent {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(3).to_string(),
            "lineageId": Uuid::from_u128(30).to_string(),
            "slug": "web",
            "config": {
                "version": 2, "privateDns": "web",
                "source": {"version": 1, "type": "image", "image": "nginx:1", "credentials": {"type": "none"}},
                "preDeployCommand": null, "startCommand": null,
                "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped"
            },
            "variables": [],
            "volumeAttachments": [],
        }))
        .unwrap()
    }

    #[test]
    fn a_client_sealed_fingerprint_is_sha256_of_its_ciphertext() {
        let bytes: Vec<u8> = (0..48).collect();
        let expected = format!("c1:{}", hex::encode(digest(&SHA256, &bytes)));
        assert_eq!(fingerprint(&value(2, 3, "K", bytes)), expected);
        assert!(!expected.starts_with(crate::sealing::FINGERPRINT_VERSION));
    }

    #[test]
    fn a_value_is_accepted_only_sealed_for_the_receiving_variable_with_the_flag_on() {
        let environment = EnvironmentId::parse(Uuid::from_u128(2).to_string()).unwrap();
        let web = web();
        let key = VariableKey::parse("K").unwrap();
        let bytes: Vec<u8> = (0..48).collect();
        let refuse = |message: &str| crate::error::invalid(message, serde_json::Value::Null);
        let receiver = |enabled| Receiver {
            environment: &environment,
            service: &web,
            enabled,
        };
        let accepted = accept(
            value(2, 3, "K", bytes.clone()),
            &key,
            &receiver(true),
            refuse,
        )
        .unwrap();
        assert!(accepted.fingerprint.starts_with("c1:"));
        assert!(matches!(accepted.value, SecretCiphertext::ClientSealed(_)));
        let refused = [
            (
                value(2, 3, "K", bytes.clone()),
                false,
                "off for this Organization",
            ),
            (value(9, 3, "K", bytes.clone()), true, "another Environment"),
            (value(2, 9, "K", bytes.clone()), true, "another Service"),
            (
                value(2, 3, "OTHER", bytes.clone()),
                true,
                "another variable",
            ),
            (
                value(2, 3, "K", vec![0; crate::variables::MAX_ENV_STRING + 48]),
                true,
                "too long",
            ),
        ];
        for (value, enabled, why) in refused {
            let error = accept(value, &key, &receiver(enabled), refuse).unwrap_err();
            assert!(error.message.contains(why), "{}", error.message);
            assert!(!error.message.contains("AAAA"), "{}", error.message);
        }
        let port = VariableKey::parse("PORT").unwrap();
        let error = accept(
            value(2, 3, "PORT", bytes.clone()),
            &port,
            &receiver(true),
            refuse,
        )
        .unwrap_err();
        assert!(error.message.contains("route traffic"), "{}", error.message);
        let mut shared = value(2, 3, "K", bytes);
        shared.value.scope = SealedScope::Shared;
        let error = accept(shared, &key, &receiver(true), refuse).unwrap_err();
        assert!(error.message.contains("every Service"), "{}", error.message);
    }
}
