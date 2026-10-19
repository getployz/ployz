//! A copy's place in its Service's routing, and the one table of moves between places.
//!
//! The owning Machine alone writes a copy's rotation and enforces [`CopyRotation::edge`]; callers follow these sequences. "Fresh run" is a new [`DeploymentLogId`]. "Wait unlisted" waits, bounded, until no live proxy lists the copy. A routed copy stops through Held, which keeps the Container running.
//!
//! | Caller | Copy now | Sequence |
//! |---|---|---|
//! | User stop | In or Pending | Held(fresh run), wait unlisted, `Stopped`. |
//! | User stop | Held(run) | Held(run) again, wait unlisted, `Stopped`. Never Held(A) to Held(B). |
//! | User stop | Out | `Stopped`. Rotation stays Out. |
//! | User start | Held(run) | In with that run, after healthy. |
//! | User start | Pending | Running with rotation Pending, wait healthy, then In. |
//! | User start | Out | Refused: deploy again. |
//! | User restart | In | Held(fresh run), wait unlisted, `Stopped`, then In with the same run. |
//! | User restart | Held(run) | In with its run, which starts a stopped Container. |
//! | User restart | Pending | `Stopped`, then Running. |
//! | User restart | Out | Refused: deploy again. |
//! | Removal (user remove, scale-to-zero, Deploy removal) | In | Out with `allow_empty`, wait unlisted, Gone. |
//! | Removal | Held(run) | Out, wait unlisted, Gone. |
//! | Removal | Pending (failed or never admitted) and hooks | Gone. |
//! | Removal | Out | Gone. |
//! | Deploy replacing an old copy, rolling | In | New copy In first, then Out, wait unlisted, Gone. |
//! | Deploy replacing an old copy, stop-first | In | Held(run), wait unlisted, `Stopped`, new copy In, then Out, Gone. |
//! | Deploy meeting an old copy | Held | Out, then Gone. |
//! | Deploy meeting an old copy | Out and still running | Stays Out, then Gone. |
//! | Drain `movable()` and Global catch-up source | Held or Out | Skipped: neither a source nor a mover. |
//! | Drain retire of this Machine's own copies | Out | Wait unlisted, Gone. Not moved. |
//! | Drain retire of this Machine's own copies | Held(run) | Stays; reported as stopped by the user. |

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::DeploymentLogId;

/// A copy's place in its Service's routing. A Container with no recorded
/// rotation is `Pending`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CopyRotation {
    /// Created but never admitted to routing.
    #[default]
    Pending,
    /// Routed.
    In,
    /// Out of routing and kept by one run, which alone may bring it back In.
    /// The Container keeps running until that run stops it.
    Held(DeploymentLogId),
    /// Out of routing for good. Only removal follows.
    Out,
}

/// Orders sibling copies of one Service; newer is higher. Not a fencing token. A copy with no gen reads [`Gen::FIRST`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "number")]
pub struct Gen(u64);

impl Gen {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Gen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// What a want asks of a copy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RotationTarget {
    /// Route the copy. `run` is the run bringing a Held copy back.
    In {
        run: Option<DeploymentLogId>,
    },
    Held(DeploymentLogId),
    Out,
    /// Stop the Container. Not a rotation: the copy keeps its rotation.
    Stopped,
    /// Remove the Container. Not a rotation: the copy and its rotation go.
    Gone,
}

/// The owner's answer to one move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RotationEdge {
    /// Write this rotation.
    Write(CopyRotation),
    /// Allowed with nothing to write: the rotation already holds, or the move is
    /// a stop that keeps it.
    Keep,
    /// Remove the Container and its rotation entry.
    Remove,
    Refused(RotationRefusal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RotationRefusal {
    /// Held by another run, which alone may move it.
    HeldByOtherRun,
    /// Routed, or held by a run that may route it again; it leaves through Out first.
    InRotation,
    OutIsFinal,
    /// No edge leads there, such as In to In or Pending straight to Out. A Pending
    /// copy that never serves is removed instead.
    NoEdge,
}

impl CopyRotation {
    /// The one edge table; every move not listed is refused. The owner adds the
    /// runtime guards: a copy reaches In only once healthy, and In reaches Out only
    /// while another In copy is acked, unless the caller allows an empty Service.
    #[must_use]
    pub fn edge(&self, target: &RotationTarget) -> RotationEdge {
        use CopyRotation::{Held, In, Out, Pending};
        use RotationEdge::{Keep, Refused, Remove, Write};
        use RotationRefusal::{HeldByOtherRun, InRotation, NoEdge, OutIsFinal};
        match (self, target) {
            (Pending, RotationTarget::In { .. }) => Write(In),
            (Pending | In, RotationTarget::Held(run)) => Write(Held(run.clone())),
            (Pending, RotationTarget::Out) => Refused(NoEdge),
            (Pending | Held(_) | Out, RotationTarget::Stopped) => Keep,
            (Pending | Out, RotationTarget::Gone) => Remove,
            (Held(held), RotationTarget::Held(run)) if held == run => Keep,
            (Held(held), RotationTarget::In { run: Some(run) }) if held == run => Write(In),
            (Held(_), RotationTarget::Held(_) | RotationTarget::In { .. }) => {
                Refused(HeldByOtherRun)
            }
            (In | Held(_), RotationTarget::Out) => Write(Out),
            (In, RotationTarget::In { .. }) => Refused(NoEdge),
            (In, RotationTarget::Stopped) | (In | Held(_), RotationTarget::Gone) => {
                Refused(InRotation)
            }
            (Out, RotationTarget::In { .. } | RotationTarget::Held(_) | RotationTarget::Out) => {
                Refused(OutIsFinal)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str) -> DeploymentLogId {
        DeploymentLogId::parse(name).unwrap()
    }

    fn targets() -> Vec<RotationTarget> {
        vec![
            RotationTarget::In { run: None },
            RotationTarget::In {
                run: Some(run("a")),
            },
            RotationTarget::In {
                run: Some(run("b")),
            },
            RotationTarget::Held(run("a")),
            RotationTarget::Held(run("b")),
            RotationTarget::Out,
            RotationTarget::Stopped,
            RotationTarget::Gone,
        ]
    }

    /// Held copies are held by run `a`. Every pair not listed is refused.
    fn allowed() -> Vec<(CopyRotation, RotationTarget, RotationEdge)> {
        use CopyRotation::{Held, In, Out, Pending};
        use RotationEdge::{Keep, Remove, Write};
        let a = || run("a");
        vec![
            (Pending, RotationTarget::Held(a()), Write(Held(a()))),
            (
                Pending,
                RotationTarget::Held(run("b")),
                Write(Held(run("b"))),
            ),
            (Pending, RotationTarget::In { run: None }, Write(In)),
            (Pending, RotationTarget::In { run: Some(a()) }, Write(In)),
            (
                Pending,
                RotationTarget::In {
                    run: Some(run("b")),
                },
                Write(In),
            ),
            (Held(a()), RotationTarget::Held(a()), Keep),
            (Held(a()), RotationTarget::In { run: Some(a()) }, Write(In)),
            (Held(a()), RotationTarget::Out, Write(Out)),
            (Held(a()), RotationTarget::Stopped, Keep),
            (In, RotationTarget::Held(a()), Write(Held(a()))),
            (In, RotationTarget::Held(run("b")), Write(Held(run("b")))),
            (In, RotationTarget::Out, Write(Out)),
            (Pending, RotationTarget::Stopped, Keep),
            (Out, RotationTarget::Stopped, Keep),
            (Pending, RotationTarget::Gone, Remove),
            (Out, RotationTarget::Gone, Remove),
        ]
    }

    #[test]
    fn rotation_edge_table() {
        let allowed = allowed();
        let froms = [
            CopyRotation::Pending,
            CopyRotation::In,
            CopyRotation::Held(run("a")),
            CopyRotation::Out,
        ];
        for from in &froms {
            for target in targets() {
                let edge = from.edge(&target);
                match allowed
                    .iter()
                    .find(|(row_from, row_target, _)| row_from == from && *row_target == target)
                {
                    Some((_, _, expected)) => assert_eq!(&edge, expected, "{from:?} -> {target:?}"),
                    None => assert!(
                        matches!(edge, RotationEdge::Refused(_)),
                        "{from:?} -> {target:?} must be refused, got {edge:?}"
                    ),
                }
            }
        }
        let held = CopyRotation::Held(run("a"));
        assert_eq!(
            held.edge(&RotationTarget::Held(run("b"))),
            RotationEdge::Refused(RotationRefusal::HeldByOtherRun)
        );
        assert_eq!(
            held.edge(&RotationTarget::In {
                run: Some(run("b"))
            }),
            RotationEdge::Refused(RotationRefusal::HeldByOtherRun)
        );
        for routed in [CopyRotation::In, held] {
            assert_eq!(
                routed.edge(&RotationTarget::Gone),
                RotationEdge::Refused(RotationRefusal::InRotation)
            );
        }
        assert_eq!(
            CopyRotation::In.edge(&RotationTarget::Stopped),
            RotationEdge::Refused(RotationRefusal::InRotation)
        );
    }

    #[test]
    fn out_is_final() {
        for target in targets() {
            let edge = CopyRotation::Out.edge(&target);
            match target {
                RotationTarget::Stopped => assert_eq!(edge, RotationEdge::Keep),
                RotationTarget::Gone => assert_eq!(edge, RotationEdge::Remove),
                RotationTarget::In { .. } | RotationTarget::Held(_) | RotationTarget::Out => {
                    assert_eq!(edge, RotationEdge::Refused(RotationRefusal::OutIsFinal))
                }
            }
        }
    }

    #[test]
    fn held_same_run_is_idempotent() {
        let held = CopyRotation::Held(run("a"));
        assert_eq!(
            held.edge(&RotationTarget::Held(run("a"))),
            RotationEdge::Keep
        );
        assert_eq!(held.edge(&RotationTarget::Stopped), RotationEdge::Keep);
    }

    #[test]
    fn rotation_reads_lowercase_in_json() {
        assert_eq!(
            serde_json::to_value(CopyRotation::Pending).unwrap(),
            serde_json::json!("pending")
        );
        assert_eq!(
            serde_json::to_value(CopyRotation::Held(run("a"))).unwrap(),
            serde_json::json!({"held": "a"})
        );
    }
}
