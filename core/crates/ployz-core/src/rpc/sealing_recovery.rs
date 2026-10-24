//! Typed requests and replies for backing up a Cluster's Sealing Key under a
//! recovery code, and for recovering it after every holder is gone.
//!
//! The code reaches a Machine only over a direct connection: a Machine refuses
//! these routes through the Ployz Cloud management tunnel, so Cloud never holds
//! the code beside the backup it stores.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::{
    EnrollRequest, RecoveryCode, RecoverySeed, RpcError, SealingKeyBackup, SealingPublicKey,
};

/// Found a Cluster whose Sealing Key is recovered from a backup instead of generated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollForRecoveryRequest {
    pub enroll: EnrollRequest,
    pub recovery: RecoverySeed,
}

/// Ask a Machine that holds the Sealing Key to wrap it under a recovery code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct WrapSealingKeyRequest {
    #[ts(type = "string")]
    pub code: RecoveryCode,
}

/// The Sealing Key wrapped under the caller's code. Holds no secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyWrapped {
    pub public_key: SealingPublicKey,
    pub backup: SealingKeyBackup,
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
}

/// The Sealing Key this Machine now holds, answered by Recover.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyRecovered {
    pub public_key: SealingPublicKey,
}

/// Why a Machine refused to wrap or recover the Sealing Key. Carried in
/// [`RpcError::details`] as `{"recovery": ...}`.
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
}

impl RecoveryRefusal {
    /// The [`RpcError::details`] value naming this refusal.
    #[must_use]
    pub fn details(self) -> Value {
        json!({ "recovery": self })
    }
}

impl RpcError {
    /// The recovery refusal this error names, if it is one.
    #[must_use]
    pub fn recovery_refusal(&self) -> Option<RecoveryRefusal> {
        serde_json::from_value(self.details.get("recovery")?.clone()).ok()
    }
}
