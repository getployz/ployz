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
/// that key, so a code names one kind of key. `Remove` removes the Device the code names, or
/// else the passkey, and leaves its key removed for good. `RemoveKey` removes a key by its
/// full public key, and marks it removed even on a Machine that never held it, so a grant
/// still in flight never lands there; only a Machine with key-step.v1 takes it. Strict for
/// the same reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetDeviceRequest {
    Approve { key: DeviceKey },
    Remove { code: DeviceCode },
    RemoveKey { key: crate::authority::KeyRef },
}

/// Trust one person's passkey or CLI Device on this Machine alone, as root on this Server
/// asks with `ployz key add`. No other kind of key can be named.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetKeyRequest {
    pub key: crate::first_key::LineKey,
}

/// What a [`SetDeviceRequest`] or [`KeyStepRequest`] changed on this Machine. Each outcome
/// is final, so a rerun reports `AlreadyApproved` or `NotPresent` rather than failing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeviceSet {
    Approved { code: DeviceCode },
    AlreadyApproved { code: DeviceCode },
    Removed { code: DeviceCode },
    NotPresent { code: DeviceCode },
}

/// A person's signed change to the keys this Machine trusts: a trust as a step of a bundle,
/// a removal alone or in one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyStepRequest {
    pub step: crate::authority::Signed<crate::authority::KeyStep>,
}

/// Signed versions marked to forget, each asking this Machine to refuse every version of
/// its Service signed before it. Each is checked alone, so one refused leaves the rest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForgetVersionsRequest {
    #[serde(deserialize_with = "crate::authority::version::at_most_presented")]
    pub versions: Vec<crate::authority::Signed<crate::authority::ServiceVersion>>,
}

/// What this Machine did with each version of a [`ForgetVersionsRequest`], in order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VersionsForgotten {
    pub outcomes: Vec<ForgetOutcome>,
}

/// What one forget did here. A rerun reports `Held`, never a failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ForgetOutcome {
    /// The Service's floor rose to the forget's time.
    Raised {
        service: crate::QualifiedService,
        floor: crate::authority::Timestamp,
    },
    /// The Service's floor was already at or past the forget's time.
    Held {
        service: crate::QualifiedService,
        floor: crate::authority::Timestamp,
    },
    Refused {
        reason: String,
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
