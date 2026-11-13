//! The trusted snapshot: the Members and keys this Machine holds evidence for.
//!
//! A daemon that advertises `ployz.machine.enforced.v1` never gives a mesh caller Full
//! scope, and Cloud's own slot is Operator. So once the snapshot exists, only two callers
//! can make a Full call here: this Server's local socket, and a direct connection from a key
//! already in the snapshot. A key either of them installs has evidence, whether or not the
//! Machine enforced then, because enforcement never widened who could make that call.
//! The socket's evidence is only as good as who reaches it: a person with SSH or a console,
//! but also any container placed before activation that mounts `/run/ployz`, which keeps
//! that reach after activation. Nothing learned before the first such start has evidence,
//! so the snapshot starts empty. A Member has evidence when this Machine verified its
//! `JoinApproval` against a key in the snapshot, or a person activated a set naming it.
//! Each Machine reads only its own snapshot.

use std::collections::BTreeMap;

use ployz_core::{DeviceCode, OperationId, authority::Member};
use serde::{Deserialize, Serialize};

/// How a key came to have evidence on this Machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "by")]
pub enum Evidence {
    /// Installed over this Server's local socket.
    LocalSocket,
    /// Installed by a Device's own connection, with that Device's key in the snapshot.
    Key { code: DeviceCode },
    /// Kept by an activation: the person confirmed its fingerprint and signed the set.
    Activation { operation: OperationId },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustedSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<Member>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    keys: BTreeMap<DeviceCode, Evidence>,
}

impl TrustedSnapshot {
    #[must_use]
    pub fn holds_key(&self, code: DeviceCode) -> bool {
        self.keys.contains_key(&code)
    }

    #[must_use]
    pub fn holds_member(&self, member: &Member) -> bool {
        self.members.contains(member)
    }

    #[must_use]
    pub fn keys(&self) -> &BTreeMap<DeviceCode, Evidence> {
        &self.keys
    }

    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// Record `code` with `evidence`, keeping the first evidence a key had.
    pub(crate) fn add_key(&mut self, code: DeviceCode, evidence: Evidence) {
        self.keys.entry(code).or_insert(evidence);
    }

    pub(crate) fn remove_key(&mut self, code: DeviceCode) {
        self.keys.remove(&code);
    }

    pub(crate) fn add_member(&mut self, member: Member) {
        if !self.members.contains(&member) {
            self.members.push(member);
        }
    }
}
