//! The owner's rotation verdict for fake Machines: the edge table and `expect`
//! check, without the guards that read other copies (health before In, `LastIn`,
//! `OlderThanIn`).

use ployz_core::{
    ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation, RotationEdge,
    RotationRefusal, RotationTarget, RunRotation,
};

/// What `want` does to a copy at `rotation`. A want already met is `Keep`.
///
/// # Errors
/// The refusal the owner answers.
pub fn decide(
    rotation: &CopyRotation,
    want: &ContainerWant,
) -> Result<RotationEdge, ContainerRefusal> {
    let (target, expect) = match want {
        ContainerWant::Running {
            rotation: RunRotation::In { run, expect },
            ..
        } => (RotationTarget::In { run: run.clone() }, expect.as_ref()),
        ContainerWant::Running {
            rotation: RunRotation::Pending,
            ..
        } => return Ok(RotationEdge::Keep),
        ContainerWant::Stopped { .. } => (RotationTarget::Stopped, None),
        ContainerWant::Gone { .. } => (RotationTarget::Gone, None),
        ContainerWant::Out { expect, .. } => (RotationTarget::Out, expect.as_ref()),
        ContainerWant::Held { run, expect, .. } => {
            (RotationTarget::Held(run.clone()), expect.as_ref())
        }
    };
    let met = match (&target, rotation) {
        (RotationTarget::In { .. }, CopyRotation::In)
        | (RotationTarget::Out, CopyRotation::Out) => true,
        (RotationTarget::Held(run), CopyRotation::Held(held)) => run == held,
        _ => false,
    };
    if met {
        return Ok(RotationEdge::Keep);
    }
    if let Some(expect) = expect
        && expect != rotation
    {
        return Err(refuse(
            ContainerRefusalReason::RotationMismatch,
            format!("the copy is {rotation:?}, not {expect:?} as expected"),
        ));
    }
    match rotation.edge(&target) {
        RotationEdge::Refused(refusal) => Err(match refusal {
            RotationRefusal::HeldByOtherRun => refuse(
                ContainerRefusalReason::HeldByOtherRun,
                format!("the copy is {rotation:?}, and only that run may move it"),
            ),
            RotationRefusal::InRotation => refuse(
                ContainerRefusalReason::InRotation,
                format!("the copy is {rotation:?}; take it out of routing first"),
            ),
            RotationRefusal::OutIsFinal | RotationRefusal::NoEdge => refuse(
                ContainerRefusalReason::RotationMismatch,
                format!("no move leads from {rotation:?} to {target:?}"),
            ),
        }),
        edge => Ok(edge),
    }
}

fn refuse(reason: ContainerRefusalReason, message: String) -> ContainerRefusal {
    ContainerRefusal { reason, message }
}
