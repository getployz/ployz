//! NATS server config policy: TLS + NKey-user authorization rendering.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::ids::MachineId;
use crate::permissions::{NatsPermissionProfile, ResponsePermission};
use crate::security::NatsPrincipal;
use serde::{Deserialize, Serialize};

/// Where the NATS listener binds.
///
/// The listener becomes externally reachable only together with TLS +
/// authorization: both are required fields of [`NatsServerConfig`], so a
/// plaintext external listener is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsListener {
    Loopback,
    External { advertise_host: NatsAdvertisedHost },
}

/// TLS certificate/key file paths rendered into the server config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerTlsFiles {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfig {
    listener: NatsListener,
    port: u16,
    server_name: MachineId,
    tls: NatsServerTlsFiles,
    authorized_users_include: PathBuf,
}

impl NatsServerConfig {
    pub fn single_machine(
        machine_id: MachineId,
        listener: NatsListener,
        tls: NatsServerTlsFiles,
        authorized_users_include: PathBuf,
    ) -> Result<Self, NatsServerConfigError> {
        let config = Self {
            listener,
            port: 4222,
            server_name: machine_id,
            tls,
            authorized_users_include,
        };
        config.validate()?;
        Ok(config)
    }

    /// The host clients on this machine should dial.
    #[must_use]
    pub fn client_host(&self) -> &str {
        match &self.listener {
            NatsListener::Loopback => "127.0.0.1",
            NatsListener::External { advertise_host } => advertise_host.as_str(),
        }
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn render(&self) -> String {
        let cert_file = quote_nats_string(
            self.tls
                .cert_file
                .to_str()
                .expect("validated nats tls cert path is UTF-8"),
        );
        let key_file = quote_nats_string(
            self.tls
                .key_file
                .to_str()
                .expect("validated nats tls key path is UTF-8"),
        );
        let include_path = quote_nats_string(
            self.authorized_users_include
                .to_str()
                .expect("validated authorized-users include path is UTF-8"),
        );
        let host = match &self.listener {
            NatsListener::Loopback => "127.0.0.1",
            NatsListener::External { .. } => "0.0.0.0",
        };

        let mut rendered = format!(
            "server_name: {}\nhost: {}\nport: {}\n",
            self.server_name.as_str(),
            host,
            self.port,
        );
        if let NatsListener::External { advertise_host } = &self.listener {
            let client_advertise =
                quote_nats_string(&format!("{}:{}", advertise_host.as_str(), self.port));
            rendered.push_str(&format!("client_advertise: {client_advertise}\n",));
        }
        rendered.push_str(&format!(
            "tls {{\n  cert_file: {cert_file}\n  key_file: {key_file}\n}}\njetstream: disabled\ninclude {include_path}\n"
        ));
        rendered
    }

    fn validate(&self) -> Result<(), NatsServerConfigError> {
        validate_config_path("tls.cert_file", &self.tls.cert_file)?;
        validate_config_path("tls.key_file", &self.tls.key_file)?;
        validate_include_path(&self.authorized_users_include)?;
        Ok(())
    }
}

/// One rendered entry of the ployzd-owned `authorized-users.conf`.
///
/// Public keys plus permissions are non-secret recovery evidence; seeds
/// never appear in the authorization file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsAuthorizedUser {
    pub principal: NatsPrincipal,
    pub nkey_public: NatsUserPublicKey,
}

impl NatsAuthorizedUser {
    /// Stable identity for one authorization file entry.
    ///
    /// Most principals are unique by role or machine id. User credentials are
    /// intentionally plural: the local operator and Cloud can both hold User
    /// authority, so the public NKey is part of their durable record key.
    #[must_use]
    pub fn authority_record_key(&self) -> String {
        match self.principal {
            NatsPrincipal::User => {
                format!("user.{}", self.nkey_public.as_str())
            }
            NatsPrincipal::Machine { .. }
            | NatsPrincipal::Controller
            | NatsPrincipal::Join
            | NatsPrincipal::System => self.principal.authority_key(),
        }
    }
}

/// The marker comment that precedes each rendered user entry. It names the
/// entry's principal so the on-disk file is durable authorization evidence.
const PRINCIPAL_MARKER_PREFIX: &str = "# ployz-principal: ";

/// Renders the `authorization { users [...] }` include file from the
/// principals' permission profiles.
#[must_use]
pub fn render_authorized_users(users: &[NatsAuthorizedUser]) -> String {
    let mut rendered = String::from("authorization {\n  users [\n");
    for user in users {
        let NatsAuthorizedUser {
            principal,
            nkey_public,
        } = user;
        let profile = NatsPermissionProfile::render(principal.clone());
        rendered.push_str("    {\n");
        rendered.push_str(&format!(
            "      {PRINCIPAL_MARKER_PREFIX}{}\n",
            principal.authority_key()
        ));
        rendered.push_str(&format!("      nkey: {}\n", nkey_public.as_str()));
        rendered.push_str("      permissions {\n");
        rendered.push_str("        publish {\n");
        rendered.push_str(&render_subject_list(
            "allow",
            profile.publish.allowed_subjects(),
        ));
        if !profile.publish.denied_subjects().is_empty() {
            rendered.push_str(&render_subject_list(
                "deny",
                profile.publish.denied_subjects(),
            ));
        }
        rendered.push_str("        }\n");
        rendered.push_str("        subscribe {\n");
        rendered.push_str(&render_subject_list(
            "allow",
            profile.subscribe.allowed_subjects(),
        ));
        if !profile.subscribe.denied_subjects().is_empty() {
            rendered.push_str(&render_subject_list(
                "deny",
                profile.subscribe.denied_subjects(),
            ));
        }
        rendered.push_str("        }\n");
        match profile.allow_responses {
            ResponsePermission::Allowed => {
                rendered.push_str("        allow_responses: true\n");
            }
            ResponsePermission::Denied => {}
        }
        rendered.push_str("      }\n");
        rendered.push_str("    }\n");
    }
    rendered.push_str("  ]\n}\n");
    rendered
}

/// Parses the principal/public-key pairs back out of a rendered
/// `authorized-users.conf`. This is the core-local authority read path:
/// renders preserve existing principals and refresh their current permission
/// profile.
pub fn parse_authorized_users(
    rendered: &str,
) -> Result<Vec<NatsAuthorizedUser>, AuthorizedUsersParseError> {
    let mut users = Vec::new();
    let mut pending_principal: Option<NatsPrincipal> = None;
    for (index, line) in rendered.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if let Some(key) = trimmed.strip_prefix(PRINCIPAL_MARKER_PREFIX) {
            if pending_principal.is_some() {
                return Err(AuthorizedUsersParseError::MarkerWithoutNkey { line_number });
            }
            pending_principal = Some(NatsPrincipal::try_from_authority_key(key.trim()).map_err(
                |_| AuthorizedUsersParseError::InvalidPrincipal {
                    line_number,
                    value: key.trim().to_owned(),
                },
            )?);
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("nkey:") {
            let Some(principal) = pending_principal.take() else {
                return Err(AuthorizedUsersParseError::NkeyWithoutMarker { line_number });
            };
            let nkey_public = NatsUserPublicKey::try_new(value.trim()).map_err(|_| {
                AuthorizedUsersParseError::InvalidPublicKey {
                    line_number,
                    value: value.trim().to_owned(),
                }
            })?;
            users.push(NatsAuthorizedUser {
                principal,
                nkey_public,
            });
        }
    }
    if pending_principal.is_some() {
        return Err(AuthorizedUsersParseError::TrailingMarker);
    }

    Ok(users)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizedUsersParseError {
    #[error("authorized-users line {line_number}: principal marker is not followed by an nkey")]
    MarkerWithoutNkey { line_number: usize },
    #[error("authorized-users line {line_number}: nkey entry has no preceding principal marker")]
    NkeyWithoutMarker { line_number: usize },
    #[error("authorized-users file ends with a principal marker and no nkey")]
    TrailingMarker,
    #[error("authorized-users line {line_number}: {value:?} is not a principal authority key")]
    InvalidPrincipal { line_number: usize, value: String },
    #[error("authorized-users line {line_number}: {value:?} is not an NKey user public key")]
    InvalidPublicKey { line_number: usize, value: String },
}

fn render_subject_list(label: &str, subjects: &[String]) -> String {
    let quoted: Vec<String> = subjects
        .iter()
        .map(|subject| quote_nats_string(subject))
        .collect();
    format!("          {label}: [{}]\n", quoted.join(", "))
}

/// The host an externally reachable listener advertises to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsAdvertisedHost(String);

impl NatsAdvertisedHost {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        if !is_valid_host_syntax(&value) {
            return Err(NatsServerConfigError::InvalidAdvertisedHost { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Syntactic host validation: hostname, IPv4, or bracketed IPv6.
#[must_use]
pub fn is_valid_host_syntax(value: &str) -> bool {
    if let Some(bracketed) = value.strip_prefix('[') {
        let Some(address) = bracketed.strip_suffix(']') else {
            return false;
        };
        return address.parse::<std::net::Ipv6Addr>().is_ok();
    }
    if value.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    is_hostname_syntax(value)
}

fn is_hostname_syntax(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 {
        return false;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

/// An NKey user public key (`U`-prefixed base32). Non-secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct NatsUserPublicKey(String);

impl NatsUserPublicKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        let Ok(pair) = nkeys::KeyPair::from_public_key(&value) else {
            return Err(NatsServerConfigError::InvalidUserPublicKey { value });
        };
        if pair.key_pair_type() != nkeys::KeyPairType::User {
            return Err(NatsServerConfigError::InvalidUserPublicKey { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NatsUserPublicKey {
    type Error = NatsServerConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NatsUserPublicKey> for String {
    fn from(value: NatsUserPublicKey) -> Self {
        value.0
    }
}

/// An NKey user seed (`SU`-prefixed base32). Secret material: `Debug`
/// output is redacted and the value never appears in error variants.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct NatsUserSeed(String);

impl NatsUserSeed {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        let Ok(pair) = nkeys::KeyPair::from_seed(&value) else {
            return Err(NatsServerConfigError::InvalidUserSeed);
        };
        if pair.key_pair_type() != nkeys::KeyPairType::User {
            return Err(NatsServerConfigError::InvalidUserSeed);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NatsUserSeed {
    type Error = NatsServerConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NatsUserSeed> for String {
    fn from(value: NatsUserSeed) -> Self {
        value.0
    }
}

impl fmt::Debug for NatsUserSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NatsUserSeed([redacted])")
    }
}

/// One freshly minted NKey user: public key for the authorization file,
/// seed for the owning machine's `0600` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedNatsUser {
    pub public: NatsUserPublicKey,
    pub seed: NatsUserSeed,
}

impl MintedNatsUser {
    pub fn generate() -> Result<Self, NatsServerConfigError> {
        let pair = nkeys::KeyPair::new_user();
        let seed = pair
            .seed()
            .map_err(|error| NatsServerConfigError::NkeySeedGeneration {
                message: error.to_string(),
            })?;
        Ok(Self {
            public: NatsUserPublicKey::try_new(pair.public_key())?,
            seed: NatsUserSeed::try_new(seed)?,
        })
    }
}

/// The cluster CA certificate in PEM form. Public material distributed in
/// the join bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct NatsCaCertificatePem(String);

impl NatsCaCertificatePem {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        let trimmed = value.trim();
        if !trimmed.starts_with("-----BEGIN CERTIFICATE-----")
            || !trimmed.ends_with("-----END CERTIFICATE-----")
            || !value.is_ascii()
        {
            return Err(NatsServerConfigError::InvalidCaCertificatePem);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NatsCaCertificatePem {
    type Error = NatsServerConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NatsCaCertificatePem> for String {
    fn from(value: NatsCaCertificatePem) -> Self {
        value.0
    }
}

/// A NATS server TLS certificate in PEM form. Public material written next
/// to the server key on the owning machine; never serialized in contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerCertificatePem(String);

impl NatsServerCertificatePem {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        let trimmed = value.trim();
        if !trimmed.starts_with("-----BEGIN CERTIFICATE-----")
            || !trimmed.ends_with("-----END CERTIFICATE-----")
            || !value.is_ascii()
        {
            return Err(NatsServerConfigError::InvalidServerCertificatePem);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServerConfigError {
    #[error("NATS config path {field} {} is not a valid path", value.display())]
    InvalidPath { field: &'static str, value: PathBuf },
    #[error("NATS advertised host {value:?} must be a hostname, IPv4, or bracketed IPv6 address")]
    InvalidAdvertisedHost { value: String },
    #[error("NATS user public key {value:?} must be a 56-character U-prefixed base32 NKey")]
    InvalidUserPublicKey { value: String },
    #[error("NATS user seed must be a 58-character SU-prefixed base32 NKey seed")]
    InvalidUserSeed,
    #[error("failed to generate NKey user seed: {message}")]
    NkeySeedGeneration { message: String },
    #[error("NATS cluster CA must be a PEM CERTIFICATE block")]
    InvalidCaCertificatePem,
    #[error("NATS server certificate must be a PEM CERTIFICATE block")]
    InvalidServerCertificatePem,
}

fn validate_config_path(field: &'static str, value: &Path) -> Result<(), NatsServerConfigError> {
    let rendered = value.to_string_lossy();
    if rendered.is_empty()
        || !value.is_absolute()
        || value.to_str().is_none()
        || rendered
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(NatsServerConfigError::InvalidPath {
            field,
            value: value.to_path_buf(),
        });
    }

    Ok(())
}

/// The authorized-users include may be relative: NATS resolves it against
/// the directory of the including config file.
fn validate_include_path(value: &Path) -> Result<(), NatsServerConfigError> {
    let rendered = value.to_string_lossy();
    if rendered.is_empty()
        || value.to_str().is_none()
        || rendered
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(NatsServerConfigError::InvalidPath {
            field: "authorized_users_include",
            value: value.to_path_buf(),
        });
    }

    Ok(())
}

fn quote_nats_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}
