//! Typed requests and replies for backfilling one Sealing Key across a Cluster.
//!
//! A Cluster founded before Sealing Keys existed has Machines that report
//! `needs_access`. An operator asks one of them to generate the key, then carries it
//! to each other Machine: the holder seals it to the recipient's WireGuard key, and the
//! recipient installs what it opens. No Machine elects itself; the operator chooses.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::{MachineId, RpcError, SealedSealingKey, SealingPublicKey, WireGuardPublicKey};

/// Ask a Participating Machine without a Sealing Key to generate one and publish its
/// public half. A Machine that already holds the key answers with that key.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GenerateSealingKeyRequest {}

/// Ask a Machine that holds the Sealing Key to seal it to another Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ShareSealingKeyRequest {
    /// The Machine that will install the key.
    pub machine: MachineId,
    /// The recipient's WireGuard key as the caller observed it; the holder refuses
    /// when its own view of that Machine disagrees.
    pub public_key: WireGuardPublicKey,
}

/// Ask a Participating Machine to install the Sealing Key another Machine sealed to it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InstallSealingKeyRequest {
    /// The Machine that sealed the key; its WireGuard key authenticates the blob.
    pub from: MachineId,
    pub sealed: SealedSealingKey,
    /// The public half the caller expects the blob to open to.
    pub public_key: SealingPublicKey,
}

/// The Sealing Key this Machine holds, answered by Generate and Install.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyHeld {
    pub public_key: SealingPublicKey,
}

/// The Sealing Key sealed to one recipient Machine, answered by Share.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingKeyShared {
    pub sealed: SealedSealingKey,
}

/// Why a Machine refused a Sealing Key backfill route. Carried in [`RpcError::details`]
/// as `{"sealing": ...}` so an operator can tell the refusals apart without reading
/// messages; the code alone says only Conflict.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SealingRefusal {
    /// The key file exists but cannot be read; the Machine never overwrites it.
    Unreadable,
    /// The Cluster already published a Sealing public key, and it is not this one.
    Published,
    /// The Cluster lost its key and awaits recovery of a specific one.
    AwaitsRecovery,
    /// The Machine holds no Sealing Key to share.
    NotHeld,
    /// The Machine already holds a different Sealing Key.
    HeldAnother,
    /// The named Machine is not in the Cluster.
    UnknownMachine,
    /// The named Machine is not published with the WireGuard key the caller gave.
    WireGuardKeyMismatch,
    /// The sealed key did not open as sent by the named Machine to this one.
    Unopenable,
    /// The sealed key opened to a different Sealing Key than the caller named.
    SealedAnother,
}

impl SealingRefusal {
    /// The [`RpcError::details`] value naming this refusal.
    #[must_use]
    pub fn details(self) -> Value {
        json!({ "sealing": self })
    }
}

impl RpcError {
    /// The Sealing Key backfill refusal this error names, if it is one.
    #[must_use]
    pub fn sealing_refusal(&self) -> Option<SealingRefusal> {
        serde_json::from_value(self.details.get("sealing")?.clone()).ok()
    }
}
