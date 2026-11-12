//! Activation: turning Device enforcement on once per Cluster, with no off, and the
//! recipient key that follows it.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    Ciphertext, DeviceCode, MachineId, OperationId, PlatformPurpose, SealedIdentity, SealedValue,
    SealingPublicKey,
    authority::{SealingRotation, Signed, StartingSet},
};

/// A starting set an Operator delivers, as the dashboard does after a passkey's own prompt.
/// A Machine admits it only when the signer and every Member are in its trusted snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ActivateEnforcementRequest {
    pub starting_set: Signed<StartingSet>,
}

/// Where a Machine stands on activation, as it reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActivationState {
    /// Not activated. Enforcement may be on from before 0.4, on a set nobody verified.
    Awaiting,
    /// A set was admitted and the daemon is restarting to commit it.
    Activating,
    /// Committed. Enforcement can't be turned off.
    Activated,
}

/// What kind of key a legacy key is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TrustedKeyKind {
    Device,
    Passkey,
}

/// A trusted key this Machine holds no evidence for: it was added before this daemon's
/// first 0.4 start, or by a caller other than its local socket or a key with evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LegacyKey {
    pub code: DeviceCode,
    pub kind: TrustedKeyKind,
    /// A passkey's account name, so a person can tell which one it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub account: Option<String>,
}

/// One Machine's own account of its activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ActivationReport {
    pub state: ActivationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub operation: Option<OperationId>,
    /// Members of the replicated store this Machine holds no evidence for.
    #[serde(default)]
    pub legacy_members: Vec<MachineId>,
    /// Trusted keys this Machine holds no evidence for.
    #[serde(default)]
    pub legacy_keys: Vec<LegacyKey>,
    /// The Private Access router this Machine keeps from before activation, if any.
    /// Activation restores it as it was, on the tailnet it had joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub private_access: Option<PrivateAccessKept>,
}

/// A Private Access router placed before activation, which activation keeps exactly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PrivateAccessKept {
    /// The tailnet the router's node joined, as its own state names it. Absent when the
    /// node hasn't joined one or its state can't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub tailnet: Option<String>,
}

impl ActivationReport {
    /// Whether a starting set could reach this Machine without its local socket.
    #[must_use]
    pub fn nothing_legacy(&self) -> bool {
        self.legacy_members.is_empty() && self.legacy_keys.is_empty()
    }
}

/// Install the recipient key an activation adds, in three steps an operator drives.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum SetRecipientKeyRequest {
    /// Generate a recipient key on this activated Machine for `operation`, held unused
    /// until a person names it. A resend returns the same public key.
    Generate { operation: OperationId },
    /// Install the recipient a verified rotation names: from `sealed`, its private half
    /// sealed to this Machine's WireGuard key, or from the key this Machine generated.
    Install {
        rotation: Signed<SealingRotation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        sealed: Option<Ciphertext>,
    },
    /// Seal this Machine's installed recipient key to `to`, a Member the rotation names.
    Share {
        rotation: Signed<SealingRotation>,
        to: MachineId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum RecipientKeySet {
    Generated { recipient: SealingPublicKey },
    Installed { recipient: SealingPublicKey },
    Shared { to: MachineId, sealed: Ciphertext },
}

/// The Cluster's recipient key as an activated Machine hands it to a Server joining after
/// activation: its private half sealed to the joiner's WireGuard key, bound to the rotation
/// that named it. The joiner installs it only once that rotation verifies against the keys
/// its own join approval names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct JoinRecipient {
    pub rotation: Signed<SealingRotation>,
    pub sealed: Ciphertext,
}

/// One stored value to seal again to this Machine's recipient key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ResealValue {
    Env {
        identity: SealedIdentity,
        value: SealedValue,
    },
    Platform {
        purpose: PlatformPurpose,
        sealed: Ciphertext,
    },
}

/// Seal stored values again to this Machine's own recipient key, which only a verified
/// rotation installs. The request names no key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ResealValuesRequest {
    pub values: Vec<ResealValue>,
}

/// Each value sealed again, in request order, or `None` where it did not open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ValuesResealed {
    pub recipient: SealingPublicKey,
    pub values: Vec<Option<Ciphertext>>,
}
