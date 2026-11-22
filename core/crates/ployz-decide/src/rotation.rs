//! The owner's verdict on a rotation want before any guard that reads other copies:
//! met, refused by `expect` or the edge table, or allowed to write. The daemon and the
//! fake Machines answer with these refusals, so a fake says what a real Machine says.

use ployz_core::{
    ContainerId, ContainerRefusal, ContainerRefusalReason, CopyRotation, DeploymentLogId,
    RotationEdge, RotationRefusal, RotationTarget,
};

#[derive(Clone, Debug)]
pub enum RotationWant {
    In { run: Option<DeploymentLogId> },
    Held(DeploymentLogId),
    Out { allow_empty: bool },
}

impl RotationWant {
    #[must_use]
    pub fn target(&self) -> RotationTarget {
        match self {
            Self::In { run } => RotationTarget::In { run: run.clone() },
            Self::Held(run) => RotationTarget::Held(run.clone()),
            Self::Out { .. } => RotationTarget::Out,
        }
    }

    #[must_use]
    pub fn met_by(&self, rotation: &CopyRotation) -> bool {
        match (self, rotation) {
            (Self::In { .. }, CopyRotation::In) | (Self::Out { .. }, CopyRotation::Out) => true,
            (Self::Held(run), CopyRotation::Held(held)) => run == held,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Checked {
    Met,
    Allowed(CopyRotation),
    Refused(ContainerRefusal),
}

/// What `want` does to a copy at `rotation` when the caller expects `expect`.
#[must_use]
pub fn decide(
    rotation: &CopyRotation,
    want: &RotationWant,
    expect: Option<&CopyRotation>,
) -> Checked {
    if want.met_by(rotation) {
        return Checked::Met;
    }
    if let Some(expect) = expect
        && expect != rotation
    {
        return Checked::Refused(refuse(
            ContainerRefusalReason::RotationMismatch,
            format!(
                "the copy is {}, not {} as expected",
                describe(rotation),
                describe(expect)
            ),
        ));
    }
    match rotation.edge(&want.target()) {
        RotationEdge::Write(to) => Checked::Allowed(to),
        RotationEdge::Refused(refusal) => Checked::Refused(edge_refusal(refusal, rotation, want)),
        RotationEdge::Keep | RotationEdge::Remove => {
            unreachable!("a met want returned early, and no rotation want removes")
        }
    }
}

fn edge_refusal(
    refusal: RotationRefusal,
    rotation: &CopyRotation,
    want: &RotationWant,
) -> ContainerRefusal {
    let from = describe(rotation);
    match refusal {
        RotationRefusal::HeldByOtherRun => refuse(
            ContainerRefusalReason::HeldByOtherRun,
            format!("the copy is {from}, and only that run may move it"),
        ),
        RotationRefusal::InRotation => refuse(
            ContainerRefusalReason::InRotation,
            format!("the copy is {from}; take it out of routing first"),
        ),
        RotationRefusal::OutIsFinal => refuse(
            ContainerRefusalReason::RotationMismatch,
            "the copy is out, and Out is final; deploy again",
        ),
        RotationRefusal::NoEdge => refuse(
            ContainerRefusalReason::RotationMismatch,
            format!("no move leads from {from} to {}", target_name(want)),
        ),
    }
}

fn target_name(want: &RotationWant) -> String {
    match want {
        RotationWant::In { .. } => "in".to_owned(),
        RotationWant::Held(run) => format!("held by {run}"),
        RotationWant::Out { .. } => "out".to_owned(),
    }
}

/// True when the copy must leave routing through an Out want before it may reach
/// `target`.
#[must_use]
pub fn in_rotation(rotation: &CopyRotation, target: &RotationTarget) -> bool {
    matches!(
        rotation.edge(target),
        RotationEdge::Refused(RotationRefusal::InRotation)
    )
}

#[must_use]
pub fn in_rotation_refusal(container: &ContainerId, rotation: &CopyRotation) -> ContainerRefusal {
    refuse(
        ContainerRefusalReason::InRotation,
        format!(
            "Container {container} is {}; take it out of routing first",
            describe(rotation)
        ),
    )
}

#[must_use]
pub fn describe(rotation: &CopyRotation) -> String {
    match rotation {
        CopyRotation::Pending => "pending".to_owned(),
        CopyRotation::In => "in".to_owned(),
        CopyRotation::Held(run) => format!("held by {run}"),
        CopyRotation::Out => "out".to_owned(),
    }
}

fn refuse(reason: ContainerRefusalReason, message: impl Into<String>) -> ContainerRefusal {
    ContainerRefusal {
        reason,
        message: message.into(),
        capability: None,
    }
}
