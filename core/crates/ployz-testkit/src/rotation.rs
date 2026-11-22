//! The owner's rotation verdict for fake Machines: [`ployz_decide::rotation`], without
//! the guards that read other copies (health before In, `LastIn`, `OlderThanIn`).

use ployz_core::{
    ContainerId, ContainerRefusal, ContainerWant, CopyRotation, RotationEdge, RotationTarget,
    RunRotation,
};
use ployz_decide::rotation::{Checked, RotationWant, in_rotation, in_rotation_refusal};

/// What `want` does to `container`, a copy at `rotation`. A want already met is `Keep`.
///
/// # Errors
/// The refusal the owner answers.
pub fn decide(
    container: &ContainerId,
    rotation: &CopyRotation,
    want: &ContainerWant,
) -> Result<RotationEdge, ContainerRefusal> {
    let (want, expect) = match want {
        ContainerWant::Running {
            rotation: RunRotation::In { run, expect },
            ..
        } => (RotationWant::In { run: run.clone() }, expect.as_ref()),
        ContainerWant::Running {
            rotation: RunRotation::Pending,
            ..
        } => return Ok(RotationEdge::Keep),
        ContainerWant::Stopped { .. } => {
            return leave(container, rotation, &RotationTarget::Stopped);
        }
        ContainerWant::Gone { .. } => return leave(container, rotation, &RotationTarget::Gone),
        ContainerWant::Out {
            allow_empty,
            expect,
            ..
        } => (
            RotationWant::Out {
                allow_empty: *allow_empty,
            },
            expect.as_ref(),
        ),
        ContainerWant::Held { run, expect, .. } => {
            (RotationWant::Held(run.clone()), expect.as_ref())
        }
    };
    match ployz_decide::rotation::decide(rotation, &want, expect) {
        Checked::Met => Ok(RotationEdge::Keep),
        Checked::Allowed(to) => Ok(RotationEdge::Write(to)),
        Checked::Refused(refusal) => Err(refusal),
    }
}

fn leave(
    container: &ContainerId,
    rotation: &CopyRotation,
    target: &RotationTarget,
) -> Result<RotationEdge, ContainerRefusal> {
    if in_rotation(rotation, target) {
        return Err(in_rotation_refusal(container, rotation));
    }
    Ok(rotation.edge(target))
}
