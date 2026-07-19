//! NATS credential grants, authority intent, and validated key material.

use std::fmt;

use std::collections::BTreeMap;

use crate::ids::{BuildExecutorId, BuildPoolId, MachineId};
use crate::security::NatsPrincipal;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};
use serde::{Deserialize, Serialize};

/// Human-facing name attached to a client credential grant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct CredentialName(String);

impl CredentialName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CredentialNameError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CredentialNameError::Empty);
        }
        if trimmed.contains(['\r', '\n', '\0']) {
            return Err(CredentialNameError::InvalidControlCharacter);
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CredentialName {
    type Error = CredentialNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CredentialName> for String {
    fn from(value: CredentialName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialNameError {
    #[error("credential name must not be empty")]
    Empty,
    #[error("credential name must not contain CR, LF, or NUL")]
    InvalidControlCharacter,
}

positive_u64_wire_newtype! {
    /// The last Unix second before which an external Build Executor credential is active.
    pub struct BuildExecutorCredentialExpiresAt;
    ts_brand: "Brand<string, \"BuildExecutorCredentialExpiresAt\">";
    accessor: unix_seconds;
    error: BuildExecutorCredentialTimestampError;
}

positive_u64_wire_error! {
    pub enum BuildExecutorCredentialTimestampError;
    noun: "Build Executor credential timestamp";
}

/// Durable identity of one external Build Executor within one Build Pool.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorIdentity {
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
}

/// One point-of-use readiness answer from an external Build Executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildExecutorReadinessAnswer<T> {
    pub identity: BuildExecutorIdentity,
    pub readiness: T,
}

/// Readiness testimony for every executor in the enrolled known set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildExecutorReadinessTestimony<T> {
    Answered {
        identity: BuildExecutorIdentity,
        readiness: T,
    },
    Silent {
        identity: BuildExecutorIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutorReadinessReconcileError {
    #[error("Build Executor known set contains duplicate identity {identity:?}")]
    DuplicateKnownIdentity { identity: BuildExecutorIdentity },
    #[error("un-enrolled Build Executor answered as {identity:?}")]
    UnknownIdentity { identity: BuildExecutorIdentity },
    #[error(
        "Build Executor {executor_id:?} answered for pool {actual_pool_id:?}, not enrolled pool {expected_pool_id:?}"
    )]
    WrongPool {
        executor_id: BuildExecutorId,
        expected_pool_id: BuildPoolId,
        actual_pool_id: BuildPoolId,
    },
    #[error("Build Executor answered more than once as {identity:?}")]
    DuplicateResponse { identity: BuildExecutorIdentity },
}

fn reconcile_build_executor_readiness_for_known_set<T>(
    known: impl IntoIterator<Item = BuildExecutorIdentity>,
    answers: impl IntoIterator<Item = BuildExecutorReadinessAnswer<T>>,
) -> Result<Vec<BuildExecutorReadinessTestimony<T>>, BuildExecutorReadinessReconcileError> {
    let mut testimony = BTreeMap::new();
    for identity in known {
        if testimony.insert(identity.clone(), None).is_some() {
            return Err(BuildExecutorReadinessReconcileError::DuplicateKnownIdentity { identity });
        }
    }

    for BuildExecutorReadinessAnswer {
        identity,
        readiness,
    } in answers
    {
        let Some(existing) = testimony.get_mut(&identity) else {
            let mut enrolled = testimony
                .keys()
                .filter(|known| known.executor_id == identity.executor_id);
            let first = enrolled.next();
            if let (Some(enrolled), None) = (first, enrolled.next()) {
                return Err(BuildExecutorReadinessReconcileError::WrongPool {
                    executor_id: identity.executor_id,
                    expected_pool_id: enrolled.pool_id.clone(),
                    actual_pool_id: identity.pool_id,
                });
            }
            return Err(BuildExecutorReadinessReconcileError::UnknownIdentity { identity });
        };
        if existing.replace(readiness).is_some() {
            return Err(BuildExecutorReadinessReconcileError::DuplicateResponse { identity });
        }
    }

    Ok(testimony
        .into_iter()
        .map(|(identity, readiness)| match readiness {
            Some(readiness) => BuildExecutorReadinessTestimony::Answered {
                identity,
                readiness,
            },
            None => BuildExecutorReadinessTestimony::Silent { identity },
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
/// Authority granted to a human or automation client credential.
pub enum CredentialRole {
    Operator,
    BuildExecutor {
        pool_id: BuildPoolId,
        executor_id: BuildExecutorId,
        expires_at: BuildExecutorCredentialExpiresAt,
    },
}

impl CredentialRole {
    #[must_use]
    pub const fn is_active_at(&self, now_unix_seconds: u64) -> bool {
        match self {
            Self::Operator => true,
            Self::BuildExecutor { expires_at, .. } => now_unix_seconds < expires_at.unix_seconds(),
        }
    }

    #[must_use]
    pub fn has_same_authority_as(&self, requested: &Self) -> bool {
        match (self, requested) {
            (Self::Operator, Self::Operator) => true,
            (
                Self::BuildExecutor {
                    pool_id: current_pool,
                    executor_id: current_executor,
                    expires_at: _,
                },
                Self::BuildExecutor {
                    pool_id: requested_pool,
                    executor_id: requested_executor,
                    expires_at: _,
                },
            ) => current_pool == requested_pool && current_executor == requested_executor,
            (Self::Operator, Self::BuildExecutor { .. })
            | (Self::BuildExecutor { .. }, Self::Operator) => false,
        }
    }

    #[must_use]
    pub fn build_executor_identity(&self) -> Option<BuildExecutorIdentity> {
        match self {
            Self::BuildExecutor {
                pool_id,
                executor_id,
                expires_at: _,
            } => Some(BuildExecutorIdentity {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
            }),
            Self::Operator => None,
        }
    }
}

/// One named client credential. The public key is its stable identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CredentialGrant {
    pub public_key: NatsUserPublicKey,
    pub name: CredentialName,
    pub role: CredentialRole,
}

/// Reconciles readiness answers against active durable credential intent at an explicit time.
pub fn reconcile_build_executor_readiness_at<T>(
    credentials: &[CredentialGrant],
    answers: impl IntoIterator<Item = BuildExecutorReadinessAnswer<T>>,
    now_unix_seconds: u64,
) -> Result<Vec<BuildExecutorReadinessTestimony<T>>, BuildExecutorReadinessReconcileError> {
    reconcile_build_executor_readiness_for_known_set(
        credentials.iter().filter_map(|credential| {
            credential
                .role
                .is_active_at(now_unix_seconds)
                .then(|| credential.role.build_executor_identity())
                .flatten()
        }),
        answers,
    )
}

/// NATS authorities owned by the control plane rather than a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "authority", rename_all = "snake_case", deny_unknown_fields)]
pub enum NatsInternalAuthority {
    Machine { machine_id: MachineId },
    Controller,
    Join,
    System,
}

impl NatsInternalAuthority {
    #[must_use]
    pub fn principal(&self) -> NatsPrincipal {
        match self {
            Self::Machine { machine_id } => NatsPrincipal::Machine {
                machine_id: machine_id.clone(),
            },
            Self::Controller => NatsPrincipal::Controller,
            Self::Join => NatsPrincipal::Join,
            Self::System => NatsPrincipal::System,
        }
    }
}

/// One logical NATS authority grant.
///
/// Client credentials and internal authorities are separate variants, so an
/// internal principal cannot accidentally acquire client-facing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NatsAuthorizationGrant {
    Credential(CredentialGrant),
    Internal {
        authority: NatsInternalAuthority,
        public_key: NatsUserPublicKey,
    },
}

impl NatsAuthorizationGrant {
    #[must_use]
    pub fn principal(&self) -> NatsPrincipal {
        match self {
            Self::Credential(CredentialGrant { role, .. }) => match role {
                CredentialRole::Operator => NatsPrincipal::Operator,
                CredentialRole::BuildExecutor {
                    pool_id,
                    executor_id,
                    expires_at: _,
                } => NatsPrincipal::BuildExecutor {
                    pool_id: pool_id.clone(),
                    executor_id: executor_id.clone(),
                },
            },
            Self::Internal { authority, .. } => authority.principal(),
        }
    }

    #[must_use]
    pub const fn public_key(&self) -> &NatsUserPublicKey {
        match self {
            Self::Credential(CredentialGrant { public_key, .. })
            | Self::Internal { public_key, .. } => public_key,
        }
    }

    /// Stable identity for one durable authorization record.
    #[must_use]
    pub fn authority_record_key(&self) -> String {
        match self {
            Self::Credential(CredentialGrant { public_key, .. }) => {
                format!("user.{}", public_key.as_str())
            }
            Self::Internal { authority, .. } => authority.principal().authority_key(),
        }
    }
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

    /// Reconstruct a user from its seed, deriving the matching public key. Used to
    /// resurrect a promoted core's principals from their pre-positioned seeds
    /// (ADR 0031) rather than minting fresh ones.
    pub fn from_seed(seed: NatsUserSeed) -> Result<Self, NatsServerConfigError> {
        let pair = nkeys::KeyPair::from_seed(seed.secret())
            .map_err(|_| NatsServerConfigError::InvalidUserSeed)?;
        Ok(Self {
            public: NatsUserPublicKey::try_new(pair.public_key())?,
            seed,
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
