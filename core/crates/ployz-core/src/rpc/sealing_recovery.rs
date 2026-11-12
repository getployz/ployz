//! Typed requests and replies for backing up a Cluster's Sealing Key under a
//! recovery code, and for recovering it after every holder is gone.
//!
//! The code reaches a Machine only over a direct connection: a Machine refuses
//! these routes through the Ployz Cloud management tunnel, so Cloud never holds
//! the code beside the backup it stores.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::evidence::Evidence;
use crate::{
    EnrollRequest, RecoveryCode, RecoverySeed, SealingKeyBackup, SealingPublicKey,
    authority::{SealingRotation, Signed},
};

/// Found a Cluster whose Sealing Key is recovered from a backup instead of generated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollForRecoveryRequest {
    pub enroll: EnrollRequest,
    pub recovery: RecoverySeed,
}

/// Which of a Cluster's Sealing keys a recovery code backs up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SealingKeyRole {
    /// The Cluster's identity key, which opens values sealed before activation.
    #[default]
    Identity,
    /// The recipient key activation added, which opens values sealed after it.
    Recipient,
}

/// Ask a Machine that holds the Sealing Key to wrap it under a recovery code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct WrapSealingKeyRequest {
    #[ts(type = "string")]
    pub code: RecoveryCode,
    /// A daemon without [`crate::RECIPIENT_RECOVERY_CAPABILITY`] ignores this and wraps the
    /// identity, so a caller checks the answered key.
    #[serde(default)]
    pub key: SealingKeyRole,
}

/// The Sealing Key wrapped under the caller's code. Holds no secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyWrapped {
    pub public_key: SealingPublicKey,
    pub backup: SealingKeyBackup,
    /// For the recipient key, the signed rotation that named it, which a Machine recovering
    /// it verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Signed<SealingRotation> | null")]
    pub rotation: Option<Signed<SealingRotation>>,
}

/// Ask a Machine to open a backup with a recovery code and keep the Sealing Key
/// the Cluster names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RecoverSealingKeyRequest {
    #[ts(type = "string")]
    pub code: RecoveryCode,
    /// Absent on a founder that holds the backup it was founded with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<SealingKeyBackup>,
    /// Present to recover the recipient key this rotation named instead of the identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Signed<SealingRotation> | null")]
    pub rotation: Option<Signed<SealingRotation>>,
}

/// The Sealing Key this Machine now holds, answered by Recover.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyRecovered {
    pub public_key: SealingPublicKey,
}

/// Why a Machine refused to wrap or recover the Sealing Key. Carried in
/// [`RpcError::details`](crate::RpcError) as `{"recovery": ...}`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRefusal {
    /// The code came through the Ployz Cloud management tunnel.
    CloudRouted,
    /// The Machine holds no Sealing Key to wrap.
    NotHeld,
    /// The Cluster names no Sealing Key to recover.
    NothingAwaited,
    /// No backup was sent and the Machine was not founded with one.
    NoBackup,
    /// The backup is not one this version can read.
    Malformed,
    /// The backup holds a different Sealing Key than the Cluster names.
    OtherKey,
    /// The code does not open the backup.
    WrongCode,
    /// The Machine already holds a different Sealing Key.
    HeldAnother,
    /// The Machine holds no recipient key, or none a rotation named.
    NoRecipient,
    /// The rotation doesn't verify against this Machine's trusted keys, names another
    /// Cluster, or names an activation other than the one this Machine committed.
    RotationRefused,
}

impl Evidence for RecoveryRefusal {
    const KEY: Option<&'static str> = Some("recovery");
}
