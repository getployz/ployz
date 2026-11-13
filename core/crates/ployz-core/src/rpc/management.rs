//! Management client, Device and passkey trust, push target and Device enforcement DTOs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DeviceCode, DeviceKey, ManagementCapability, ManagementClientLabel};

/// Exactly one update to a labelled Management Client slot: set it or clear it.
///
/// Neither case carries a secret; `Set` returns the fresh client key in its capability.
/// Strict by the Stable promise's security exception: an unrecognized field may
/// be secret material the daemon must never accept.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetManagementClientRequest {
    Set { label: ManagementClientLabel },
    Clear { label: ManagementClientLabel },
}

/// Confirmation of a slot update. `capability` is present after `Set` and absent after `Clear`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetManagementClientResponse {
    #[ts(type = "string | null")]
    pub capability: Option<ManagementCapability>,
}

/// Approve a Device on this Machine, or remove one, at Full scope only.
///
/// `Approve` carries the Device's public key, never a secret; the Machine stores it in
/// the slot the key's [`DeviceCode`] names, and is refused while a trusted passkey holds
/// that key, so a code names one kind of key. `Remove` clears only the Device slot;
/// [`RemoveTrustedKeyRequest`] removes whichever kind holds a code. Strict for the same
/// reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetDeviceRequest {
    Approve { key: DeviceKey },
    Remove { code: DeviceCode },
}

/// What a [`SetDeviceRequest`] changed on this Machine. Each outcome is final, so a
/// rerun reports `AlreadyApproved` or `NotPresent` rather than failing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeviceSet {
    Approved { code: DeviceCode },
    AlreadyApproved { code: DeviceCode },
    Removed { code: DeviceCode },
    NotPresent { code: DeviceCode },
}

/// Approve a passkey on this Machine at Full scope only. The Machine records who
/// approved it from its own view of the caller, never from the request.
/// [`RemoveTrustedKeyRequest`] removes one.
///
/// A passkey only approves workflows: it gets no Management Client slot, so it never
/// connects. Strict for the same reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetPasskeyRequest {
    Approve {
        passkey: Box<crate::authority::Passkey>,
    },
}

/// Stop trusting the key `code` names on this Machine, at Full scope only, whether a
/// Device or a passkey holds it. The Machine picks the kind under the same admission as
/// the approvals, so no approval between a caller's read and this removal survives it.
/// Strict for the same reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveTrustedKeyRequest {
    pub code: DeviceCode,
}

/// What a [`RemoveTrustedKeyRequest`] removed. Each outcome is final, so a rerun reports
/// `NotPresent` rather than failing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TrustedKeyRemoved {
    RemovedDevice {
        code: DeviceCode,
        /// Passkeys the removed Device had approved here, read in the same mutation.
        /// They stay trusted, and a Device that removed itself can no longer look.
        #[serde(default)]
        approved_passkeys: Vec<DeviceCode>,
    },
    RemovedPasskey {
        code: DeviceCode,
    },
    NotPresent {
        code: DeviceCode,
    },
}

/// Where this Machine's observe process pushes crash events, or `None` to stop pushing.
///
/// Strict for the same reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPushTargetRequest {
    pub url: Option<url::Url>,
}

/// The push target the Machine now holds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PushTargetSet {
    #[ts(type = "string | null")]
    pub url: Option<url::Url>,
}

/// Whether this Machine requires a Device-signed approval before an Operator caller
/// changes a Service. Full scope only, so neither Cloud nor a CLI without an approved
/// Device can turn it off. Strict for the same reason as [`SetManagementClientRequest`].
/// Turning it on needs `starting_set`, the Machines a Device approved to trust from the start.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDeviceEnforcementRequest {
    pub enforced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starting_set: Option<crate::authority::Signed<crate::authority::StartingSet>>,
}

/// The Device enforcement setting the Machine now holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeviceEnforcementSet {
    pub enforced: bool,
}
