//! `SetVolumeCopy`: one verb that moves a Machine's copy of a Volume between Writer,
//! Mirror and nothing, and the pure planner that picks the one transition it runs.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ContainerId, DockerVolumeName, ManagementAddress, Namespace, ResolvedServiceSpec, RpcError,
    RpcErrorCode, SlotId, SnapshotGuid, SnapshotName, VolumeCopy,
};

/// Which run of writes a Writer root belongs to. Cloud numbers it; the Machine stores it
/// on the root and only ever raises it. A root without one reads as 0.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS,
)]
#[serde(transparent)]
pub struct WriterEpoch(u32);

impl WriterEpoch {
    #[must_use]
    pub const fn new(epoch: u32) -> Self {
        Self(epoch)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for WriterEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The idempotency key a driver gives a new Container.
///
/// Invariant: non-empty, at most 512 bytes. No charset rule: the daemon hashes
/// the key into the Docker name, and today's Global replacement keys
/// (`global:{service}:{shape}:replace:{old_container_id}`) run past 128 bytes.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct CreationKey(String);

impl CreationKey {
    pub const MAX_BYTES: usize = 512;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CreationKey {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("a creation key must not be empty".to_owned());
        }
        if value.len() > Self::MAX_BYTES {
            return Err(format!(
                "a creation key must be at most {} bytes; this one is {}",
                Self::MAX_BYTES,
                value.len()
            ));
        }
        Ok(Self(value))
    }
}

impl From<CreationKey> for String {
    fn from(key: CreationKey) -> Self {
        key.0
    }
}

/// How long the Machine keeps a running Container out of ingress before it
/// stops it. Patience, not a safety wait (DESIGN.md bet 6 amendment). The
/// Machine does not check that any other Machine's proxy reloaded.
///
/// Invariant: `0 ..= MAX` milliseconds, checked at deserialize. Required on the
/// wire, so the Machine owns no default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "u32", into = "u32")]
#[ts(type = "number")]
pub struct Drain(u32);

impl Drain {
    pub const NONE: Drain = Drain(0);
    /// What Cloud sends. Mark to Caddy withdrawn on both Machines, worst of 92
    /// observations: 1.41 s unloaded, 2.0 s under CPU load
    /// (evidence/drain/REPORT.md). 3 s is 2.1x and 1.5x those.
    pub const DEFAULT: Drain = Drain(3_000);
    pub const MAX: Drain = Drain(30_000);

    #[must_use]
    pub fn as_duration(self) -> Duration {
        Duration::from_millis(u64::from(self.0))
    }
}

impl TryFrom<u32> for Drain {
    type Error = String;
    fn try_from(millis: u32) -> Result<Self, Self::Error> {
        if millis > Self::MAX.0 {
            return Err(format!(
                "a drain must be at most {} ms; this one is {millis}",
                Self::MAX.0
            ));
        }
        Ok(Self(millis))
    }
}

impl From<Drain> for u32 {
    fn from(drain: Drain) -> Self {
        drain.0
    }
}

/// Docker's own stop options, unchanged in meaning. `u16` on the wire; the
/// handler widens to the `i32` Docker takes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct StopOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period_seconds: Option<u16>,
}

/// Move this Machine's copy of `name` to `want`, provided it holds `expect` now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeCopyRequest {
    pub name: DockerVolumeName,
    pub expect: ExpectedCopy,
    pub want: WantedCopy,
}

/// What the sender believes this Machine holds. Compared by equality; a copy already at
/// the want is accepted whatever this says.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectedCopy {
    Writer { epoch: WriterEpoch },
    Mirror { newest: SnapshotGuid, slot: SlotId },
    None,
}

/// The copy the sender wants this Machine to hold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a Writer start carries the whole resolved spec; one request holds one"
)]
pub enum WantedCopy {
    /// A writable root whose newest snapshot is `at`, numbered `epoch`, with its
    /// Container running when `start` is given.
    Writer {
        at: SnapshotGuid,
        epoch: WriterEpoch,
        #[serde(default)]
        start: Option<Start>,
    },
    /// A read-only slot whose newest snapshot is `target`. On a Writer, the cut: stop the
    /// Container behind ingress for `drain`, take `target`, park the root in the slot.
    /// On a Mirror or nothing, pull `target` from `from`.
    Mirror {
        #[serde(default)]
        #[ts(type = "string | null")]
        from: Option<ManagementAddress>,
        target: SnapshotName,
        drain: Drain,
    },
    /// A Writer with a run snapshot named `name`, taken only if anything was written
    /// since its newest one.
    Snapshot { name: SnapshotName },
    /// No run snapshots on a Writer, no slot on a Mirror.
    Forget,
}

/// The Container a Writer runs: one Docker already holds, or one to create under a key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(rename = "CopyStart")]
#[expect(
    clippy::large_enum_variant,
    reason = "a Create carries the whole resolved spec; one request holds one"
)]
pub enum Start {
    Kept {
        container_id: ContainerId,
    },
    Create {
        namespace: Namespace,
        resolved_spec: ResolvedServiceSpec,
        creation_key: CreationKey,
    },
}

/// The copy as it is after the request, or as it was when the request refused.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeCopyReply {
    pub copy: Option<VolumeCopy>,
}

/// Why a Machine refused a `SetVolumeCopy`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CopyRefusal {
    /// The copy is neither at the want nor what `expect` says.
    Mismatch,
    /// The table has no cell from this copy to the want, or the want lowers the epoch.
    NoTransition,
    /// A Container runs on the root, or a pull into the slot is still receiving.
    Busy,
    NoCapacity,
    /// The copy's newest snapshot is not the one the want names.
    Behind,
    /// The cut's target exists but the root was written after it.
    Written,
    /// A pull names no `from`.
    NoSource,
}

/// `RpcError.details` of a refused `SetVolumeCopy`: the reason and the copy it left.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeCopyRefused {
    pub reason: CopyRefusal,
    pub copy: Option<VolumeCopy>,
}

impl SetVolumeCopyRefused {
    /// The refusal carried by an RPC error, if it is one.
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        Self::deserialize(details).ok()
    }

    /// An RPC error that carries this refusal in `details`.
    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        let code = match self.reason {
            CopyRefusal::NoCapacity => RpcErrorCode::Unavailable,
            CopyRefusal::Mismatch
            | CopyRefusal::NoTransition
            | CopyRefusal::Busy
            | CopyRefusal::Behind
            | CopyRefusal::Written
            | CopyRefusal::NoSource => RpcErrorCode::Conflict,
        };
        RpcError {
            code,
            message: message.into(),
            details: serde_json::to_value(self).expect("a copy refusal is JSON serializable"),
            cause: Vec::new(),
        }
    }
}

/// One cell of the transition table with an effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyTransition {
    /// Raise the root's epoch, make it writable again if a cut left it read-only, then
    /// resume the start.
    WriterToWriter,
    /// The cut.
    WriterToMirror,
    WriterToSnapshot,
    WriterToForget,
    /// Promote the slot to a writable root at the wanted epoch, then start.
    MirrorToWriter,
    /// Pull `target` from `from`.
    MirrorToMirror,
    MirrorToForget,
    /// Create the slot, then pull.
    NoneToMirror,
}

/// What `SetVolumeCopy` does for one request against the copy it found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyPlan {
    AtWant,
    Refuse(CopyRefusal),
    Transition(CopyTransition),
}

/// The one plan for `want` against `copy`, given what the sender expected.
///
/// `start_running` says whether the Container a Writer want's `start` names is running;
/// it is ignored for every other want. Checks run in order: at the want already, then
/// `expect` against the copy, then the table. An empty slot (no snapshot landed yet)
/// counts as nothing for `expect`, so a pull resent after a crash between the slot and
/// its first snapshot converges. A root already at a Writer want's snapshot and epoch
/// resumes its start whatever `expect` says, so a promote resent after its ZFS half
/// converges too. A slot whose pull for the want's target landed finishes that pull
/// whatever `expect` says, since the landing itself moved `newest` past the expect.
#[must_use]
pub fn plan(
    copy: Option<&VolumeCopy>,
    start_running: bool,
    expect: &ExpectedCopy,
    want: &WantedCopy,
) -> CopyPlan {
    use CopyPlan::{AtWant, Refuse, Transition};
    use CopyTransition::{
        MirrorToForget, MirrorToMirror, MirrorToWriter, NoneToMirror, WriterToForget,
        WriterToMirror, WriterToSnapshot, WriterToWriter,
    };
    let expected = |transition| {
        if expect_matches(copy, expect) {
            Transition(transition)
        } else {
            Refuse(CopyRefusal::Mismatch)
        }
    };
    let refuse = |reason| {
        if expect_matches(copy, expect) {
            Refuse(reason)
        } else {
            Refuse(CopyRefusal::Mismatch)
        }
    };
    match (copy, want) {
        (
            Some(VolumeCopy::Root {
                readonly,
                newest,
                epoch: held,
                ..
            }),
            WantedCopy::Writer { at, epoch, start },
        ) => {
            let at_snapshot = newest.as_ref().is_some_and(|newest| newest.guid == *at);
            if held == epoch && at_snapshot {
                if !readonly && (start.is_none() || start_running) {
                    return AtWant;
                }
                return Transition(WriterToWriter);
            }
            if !expect_matches(copy, expect) {
                return Refuse(CopyRefusal::Mismatch);
            }
            if held > epoch {
                return Refuse(CopyRefusal::NoTransition);
            }
            if !at_snapshot {
                return Refuse(CopyRefusal::Behind);
            }
            Transition(WriterToWriter)
        }
        (Some(VolumeCopy::Root { .. }), WantedCopy::Mirror { .. }) => expected(WriterToMirror),
        (Some(VolumeCopy::Root { newest, .. }), WantedCopy::Snapshot { name }) => {
            // A resend at the want still runs, so the route prunes past the newest two.
            if newest.as_ref().is_some_and(|newest| newest.name == *name) {
                return Transition(WriterToSnapshot);
            }
            expected(WriterToSnapshot)
        }
        (Some(VolumeCopy::Root { newest, .. }), WantedCopy::Forget) => {
            if newest.is_none() {
                return AtWant;
            }
            expected(WriterToForget)
        }
        (Some(VolumeCopy::Slot { newest, .. }), WantedCopy::Writer { at, .. }) => {
            if !expect_matches(copy, expect) {
                return Refuse(CopyRefusal::Mismatch);
            }
            if newest.as_ref().is_some_and(|newest| newest.guid == *at) {
                return Transition(MirrorToWriter);
            }
            Refuse(CopyRefusal::Behind)
        }
        (
            Some(VolumeCopy::Slot {
                slot, newest, pull, ..
            }),
            WantedCopy::Mirror { from, target, .. },
        ) => {
            let landed = newest.as_ref().is_some_and(|newest| newest.name == *target);
            if landed && pull.is_none() {
                return AtWant;
            }
            // This request's own pull moves `newest` past its expect: it lands, or a Full
            // fallback empties the slot first. A resend continues it on the same slot.
            let pulling = pull.as_ref().is_some_and(|pull| pull.target == *target);
            let same_slot =
                matches!(expect, ExpectedCopy::Mirror { slot: expected, .. } if expected == slot);
            if from.is_some() && pulling && (landed || same_slot) {
                return Transition(MirrorToMirror);
            }
            if from.is_none() {
                return refuse(CopyRefusal::NoSource);
            }
            expected(MirrorToMirror)
        }
        (Some(VolumeCopy::Slot { .. }), WantedCopy::Snapshot { .. }) => {
            refuse(CopyRefusal::NoTransition)
        }
        (Some(VolumeCopy::Slot { .. }), WantedCopy::Forget) => expected(MirrorToForget),
        (None, WantedCopy::Writer { .. } | WantedCopy::Snapshot { .. }) => {
            refuse(CopyRefusal::NoTransition)
        }
        (None, WantedCopy::Mirror { from, .. }) => {
            if from.is_none() {
                return refuse(CopyRefusal::NoSource);
            }
            expected(NoneToMirror)
        }
        (None, WantedCopy::Forget) => AtWant,
    }
}

fn expect_matches(copy: Option<&VolumeCopy>, expect: &ExpectedCopy) -> bool {
    match (copy, expect) {
        (Some(VolumeCopy::Root { epoch, .. }), ExpectedCopy::Writer { epoch: expected }) => {
            epoch == expected
        }
        (
            Some(VolumeCopy::Slot {
                slot,
                newest: Some(newest),
                ..
            }),
            ExpectedCopy::Mirror {
                newest: expected,
                slot: expected_slot,
            },
        ) => newest.guid == *expected && slot == expected_slot,
        (Some(VolumeCopy::Slot { newest: None, .. }) | None, ExpectedCopy::None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pull, Snapshot};

    fn snapshot(name: &str, guid: u64) -> Snapshot {
        Snapshot {
            name: name.parse().unwrap(),
            guid: SnapshotGuid::new(guid),
            created_unix_seconds: 1,
        }
    }

    fn root(epoch: u32, readonly: bool, newest: Option<Snapshot>) -> VolumeCopy {
        VolumeCopy::Root {
            readonly,
            newest,
            epoch: WriterEpoch::new(epoch),
        }
    }

    fn slot(newest: Option<Snapshot>, pull: Option<Pull>) -> VolumeCopy {
        VolumeCopy::Slot {
            slot: SLOT,
            readonly: true,
            newest,
            resume_token: None,
            pull,
        }
    }

    fn writer(at: u64, epoch: u32, start: bool) -> WantedCopy {
        WantedCopy::Writer {
            at: SnapshotGuid::new(at),
            epoch: WriterEpoch::new(epoch),
            start: start.then(|| Start::Kept {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            }),
        }
    }

    fn mirror(from: bool, target: &str) -> WantedCopy {
        WantedCopy::Mirror {
            from: from.then(|| ManagementAddress("fd00::1".parse().unwrap())),
            target: target.parse().unwrap(),
            drain: Drain::NONE,
        }
    }

    fn snapshot_want(name: &str) -> WantedCopy {
        WantedCopy::Snapshot {
            name: name.parse().unwrap(),
        }
    }

    const WRITER_1: ExpectedCopy = ExpectedCopy::Writer {
        epoch: WriterEpoch::new(1),
    };
    const SLOT: SlotId = SlotId::new(1);
    const MIRROR_AT_10: ExpectedCopy = ExpectedCopy::Mirror {
        newest: SnapshotGuid::new(10),
        slot: SLOT,
    };

    type Row = (
        &'static str,
        Option<VolumeCopy>,
        bool,
        ExpectedCopy,
        WantedCopy,
        CopyPlan,
    );

    #[test]
    fn plans_every_cell_of_the_table() {
        use CopyPlan::{AtWant, Refuse, Transition};
        use CopyRefusal::{Behind, Mismatch, NoSource, NoTransition};
        use CopyTransition::*;
        let g = snapshot("f-1", 10);
        let w = snapshot("w-1-1", 9);
        let pull = Pull {
            target: "f-1".parse().unwrap(),
            failure: None,
        };
        let table: &[Row] = &[
            // Writer wants.
            (
                "writer at want",
                Some(root(1, false, Some(g.clone()))),
                false,
                WRITER_1,
                writer(10, 1, false),
                AtWant,
            ),
            (
                "writer at want, start running",
                Some(root(1, false, Some(g.clone()))),
                true,
                ExpectedCopy::None,
                writer(10, 1, true),
                AtWant,
            ),
            (
                "writer start stopped resumes whatever expect",
                Some(root(1, false, Some(g.clone()))),
                false,
                MIRROR_AT_10,
                writer(10, 1, true),
                Transition(WriterToWriter),
            ),
            (
                "readonly root at want resumes",
                Some(root(1, true, Some(g.clone()))),
                false,
                WRITER_1,
                writer(10, 1, false),
                Transition(WriterToWriter),
            ),
            (
                "writer higher epoch at the snapshot raises it",
                Some(root(1, false, Some(g.clone()))),
                false,
                WRITER_1,
                writer(10, 2, true),
                Transition(WriterToWriter),
            ),
            (
                "writer lower epoch refuses",
                Some(root(2, false, Some(g.clone()))),
                false,
                ExpectedCopy::Writer {
                    epoch: WriterEpoch::new(2),
                },
                writer(10, 1, false),
                Refuse(NoTransition),
            ),
            (
                "writer higher epoch behind",
                Some(root(1, false, Some(w.clone()))),
                false,
                WRITER_1,
                writer(10, 2, false),
                Refuse(Behind),
            ),
            (
                "writer higher epoch, expect mismatch",
                Some(root(1, false, Some(g.clone()))),
                false,
                ExpectedCopy::None,
                writer(10, 2, false),
                Refuse(Mismatch),
            ),
            (
                "writer behind",
                Some(root(1, false, Some(w.clone()))),
                false,
                WRITER_1,
                writer(10, 1, false),
                Refuse(Behind),
            ),
            (
                "writer no snapshot",
                Some(root(1, false, None)),
                false,
                WRITER_1,
                writer(10, 1, false),
                Refuse(Behind),
            ),
            (
                "mirror to writer",
                Some(slot(Some(g.clone()), None)),
                false,
                MIRROR_AT_10,
                writer(10, 2, true),
                Transition(MirrorToWriter),
            ),
            (
                "mirror to writer behind",
                Some(slot(Some(w.clone()), None)),
                false,
                ExpectedCopy::Mirror {
                    newest: SnapshotGuid::new(9),
                    slot: SLOT,
                },
                writer(10, 2, true),
                Refuse(Behind),
            ),
            (
                "mirror to writer mismatch",
                Some(slot(Some(g.clone()), None)),
                false,
                ExpectedCopy::Mirror {
                    newest: SnapshotGuid::new(9),
                    slot: SLOT,
                },
                writer(10, 2, true),
                Refuse(Mismatch),
            ),
            (
                "none to writer",
                None,
                false,
                ExpectedCopy::None,
                writer(10, 1, true),
                Refuse(NoTransition),
            ),
            (
                "none to writer mismatch",
                None,
                false,
                WRITER_1,
                writer(10, 1, true),
                Refuse(Mismatch),
            ),
            // Mirror wants.
            (
                "the cut",
                Some(root(1, false, Some(w.clone()))),
                false,
                WRITER_1,
                mirror(false, "f-1"),
                Transition(WriterToMirror),
            ),
            (
                "the cut resumed on a readonly root",
                Some(root(1, true, Some(g.clone()))),
                false,
                WRITER_1,
                mirror(false, "f-1"),
                Transition(WriterToMirror),
            ),
            (
                "the cut mismatch",
                Some(root(2, false, Some(w.clone()))),
                false,
                WRITER_1,
                mirror(false, "f-1"),
                Refuse(Mismatch),
            ),
            (
                "mirror at want",
                Some(slot(Some(g.clone()), None)),
                false,
                ExpectedCopy::None,
                mirror(true, "f-1"),
                AtWant,
            ),
            (
                "mirror landed but not pruned",
                Some(slot(Some(g.clone()), Some(pull.clone()))),
                false,
                MIRROR_AT_10,
                mirror(true, "f-1"),
                Transition(MirrorToMirror),
            ),
            (
                "mirror landed under its own pull finishes whatever expect",
                Some(slot(Some(g.clone()), Some(pull.clone()))),
                false,
                ExpectedCopy::Mirror {
                    newest: SnapshotGuid::new(9),
                    slot: SLOT,
                },
                mirror(true, "f-1"),
                Transition(MirrorToMirror),
            ),
            (
                "mirror pull",
                Some(slot(Some(w.clone()), None)),
                false,
                ExpectedCopy::Mirror {
                    newest: SnapshotGuid::new(9),
                    slot: SLOT,
                },
                mirror(true, "f-1"),
                Transition(MirrorToMirror),
            ),
            (
                "mirror pull mismatch",
                Some(slot(Some(w.clone()), None)),
                false,
                MIRROR_AT_10,
                mirror(true, "f-1"),
                Refuse(Mismatch),
            ),
            (
                "mirror pull no source",
                Some(slot(Some(w.clone()), None)),
                false,
                ExpectedCopy::Mirror {
                    newest: SnapshotGuid::new(9),
                    slot: SLOT,
                },
                mirror(false, "f-1"),
                Refuse(NoSource),
            ),
            (
                "empty slot counts as none",
                Some(slot(None, None)),
                false,
                ExpectedCopy::None,
                mirror(true, "f-1"),
                Transition(MirrorToMirror),
            ),
            (
                "none to mirror",
                None,
                false,
                ExpectedCopy::None,
                mirror(true, "f-1"),
                Transition(NoneToMirror),
            ),
            (
                "none to mirror no source",
                None,
                false,
                ExpectedCopy::None,
                mirror(false, "f-1"),
                Refuse(NoSource),
            ),
            (
                "none to mirror mismatch",
                None,
                false,
                MIRROR_AT_10,
                mirror(true, "f-1"),
                Refuse(Mismatch),
            ),
            // Snapshot wants.
            (
                "snapshot at want prunes whatever expect",
                Some(root(1, false, Some(g.clone()))),
                false,
                ExpectedCopy::None,
                snapshot_want("f-1"),
                Transition(WriterToSnapshot),
            ),
            (
                "snapshot",
                Some(root(1, false, Some(w.clone()))),
                false,
                WRITER_1,
                snapshot_want("f-1"),
                Transition(WriterToSnapshot),
            ),
            (
                "first snapshot",
                Some(root(1, false, None)),
                false,
                WRITER_1,
                snapshot_want("w-1-1"),
                Transition(WriterToSnapshot),
            ),
            (
                "snapshot mismatch",
                Some(root(1, false, Some(w.clone()))),
                false,
                ExpectedCopy::None,
                snapshot_want("f-1"),
                Refuse(Mismatch),
            ),
            (
                "snapshot on a mirror",
                Some(slot(Some(g.clone()), None)),
                false,
                MIRROR_AT_10,
                snapshot_want("f-2"),
                Refuse(NoTransition),
            ),
            (
                "snapshot on a mirror mismatch",
                Some(slot(Some(g.clone()), None)),
                false,
                WRITER_1,
                snapshot_want("f-2"),
                Refuse(Mismatch),
            ),
            (
                "snapshot on nothing",
                None,
                false,
                ExpectedCopy::None,
                snapshot_want("f-2"),
                Refuse(NoTransition),
            ),
            // Forget wants.
            (
                "forget at want on a bare root",
                Some(root(1, false, None)),
                false,
                MIRROR_AT_10,
                WantedCopy::Forget,
                AtWant,
            ),
            (
                "forget",
                Some(root(1, false, Some(g.clone()))),
                false,
                WRITER_1,
                WantedCopy::Forget,
                Transition(WriterToForget),
            ),
            (
                "forget mismatch",
                Some(root(1, false, Some(g.clone()))),
                false,
                ExpectedCopy::None,
                WantedCopy::Forget,
                Refuse(Mismatch),
            ),
            (
                "destroy the slot",
                Some(slot(Some(g.clone()), None)),
                false,
                MIRROR_AT_10,
                WantedCopy::Forget,
                Transition(MirrorToForget),
            ),
            (
                "destroy an empty slot",
                Some(slot(None, None)),
                false,
                ExpectedCopy::None,
                WantedCopy::Forget,
                Transition(MirrorToForget),
            ),
            (
                "destroy the slot mismatch",
                Some(slot(Some(g.clone()), None)),
                false,
                ExpectedCopy::None,
                WantedCopy::Forget,
                Refuse(Mismatch),
            ),
            (
                "forget nothing",
                None,
                false,
                WRITER_1,
                WantedCopy::Forget,
                AtWant,
            ),
        ];
        for (case, copy, start_running, expect, want, expected) in table {
            assert_eq!(
                plan(copy.as_ref(), *start_running, expect, want),
                *expected,
                "{case}"
            );
        }
    }

    #[test]
    fn a_mirror_expect_refuses_a_slot_created_again_at_the_same_snapshot() {
        let mut again = slot(Some(snapshot("f-1", 10)), None);
        if let VolumeCopy::Slot { slot, .. } = &mut again {
            *slot = SlotId::new(2);
        }
        for want in [writer(10, 2, true), mirror(true, "f-2"), WantedCopy::Forget] {
            assert_eq!(
                plan(Some(&again), false, &MIRROR_AT_10, &want),
                CopyPlan::Refuse(CopyRefusal::Mismatch),
                "{want:?}"
            );
        }
    }

    #[test]
    fn a_mirror_want_continues_its_own_pull_after_the_slot_newest_moved() {
        let pull = |target: &str| Pull {
            target: target.parse().unwrap(),
            failure: None,
        };
        let restarted = slot(None, Some(pull("f-1")));
        let moved = slot(Some(snapshot("w-1-1", 9)), Some(pull("f-1")));
        for copy in [&restarted, &moved] {
            assert_eq!(
                plan(Some(copy), false, &MIRROR_AT_10, &mirror(true, "f-1")),
                CopyPlan::Transition(CopyTransition::MirrorToMirror),
                "{copy:?}"
            );
        }
        let mut again = restarted.clone();
        if let VolumeCopy::Slot { slot, .. } = &mut again {
            *slot = SlotId::new(2);
        }
        let other = slot(None, Some(pull("f-2")));
        for copy in [&again, &other] {
            assert_eq!(
                plan(Some(copy), false, &MIRROR_AT_10, &mirror(true, "f-1")),
                CopyPlan::Refuse(CopyRefusal::Mismatch),
                "{copy:?}"
            );
        }
    }

    #[test]
    fn wire_bounds_hold_at_deserialize() {
        assert!(serde_json::from_str::<Drain>("30000").is_ok());
        assert_eq!(
            serde_json::from_str::<Drain>("30001")
                .unwrap_err()
                .to_string(),
            "a drain must be at most 30000 ms; this one is 30001"
        );
        assert_eq!(
            serde_json::from_str::<CreationKey>("\"\"")
                .unwrap_err()
                .to_string(),
            "a creation key must not be empty"
        );
        let long = format!("\"{}\"", "k".repeat(513));
        assert_eq!(
            serde_json::from_str::<CreationKey>(&long)
                .unwrap_err()
                .to_string(),
            "a creation key must be at most 512 bytes; this one is 513"
        );
        assert_eq!(
            serde_json::to_value(StopOptions::default()).unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::to_value(WriterEpoch::default()).unwrap(),
            serde_json::json!(0)
        );
    }

    #[test]
    fn refusals_carry_the_reason_and_the_copy() {
        let refused = SetVolumeCopyRefused {
            reason: CopyRefusal::Busy,
            copy: Some(root(1, false, None)),
        };
        let error = refused.clone().rpc_error("the Container still runs");
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            SetVolumeCopyRefused::from_details(&error.details),
            Some(refused)
        );
        let capacity = SetVolumeCopyRefused {
            reason: CopyRefusal::NoCapacity,
            copy: None,
        }
        .rpc_error("no room");
        assert_eq!(capacity.code, RpcErrorCode::Unavailable);
    }
}
