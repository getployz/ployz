//! What each keyed create's first answer must still fit for a replay to return it.
//! An omitted Project or Environment resolves to the Organization's only Project and
//! its Default Environment, which may have moved since the first call; a replay that
//! resolves elsewhere now is refused rather than answered with the old place.

use crate::deployment::admit::Admit;
use crate::error::conflict;
use crate::storage::Tx;
use crate::{
    Actor, Branched, CreateBranch, CreateConfig, CreateEnvironment, CreateGitService,
    CreateService, CreateVolume, DeploymentSummary, EnvironmentCreated, EnvironmentId,
    EnvironmentRef, Publish, Published, RpcError, scope,
};
use crate::{ConfigStaged, ServiceStaged, VolumeStaged};

/// For a create with no Project or Environment to resolve.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every replay rule has one signature"
)]
pub(crate) const fn any<P, A>(_: &mut dyn Tx, _: &Actor, _: &P, _: &A) -> Result<(), RpcError> {
    Ok(())
}

fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
    first: Option<&EnvironmentId>,
    id: &str,
) -> Result<(), RpcError> {
    let (_, now) = scope::resolve(tx, who, at)?;
    if first == Some(&now) {
        return Ok(());
    }
    Err(conflict(
        "This ID was already used in another Environment; send a new ID to use this one",
        serde_json::json!({ "id": id }),
    ))
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &Publish,
    first: &Published,
) -> Result<(), RpcError> {
    let id = c.id.as_deref().unwrap_or_default();
    environment(tx, who, &c.environment, Some(&first.environment.id), id)
}

pub(crate) fn service(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateService,
    first: &ServiceStaged,
) -> Result<(), RpcError> {
    environment(
        tx,
        who,
        &c.environment,
        Some(&first.environment.id),
        c.id.as_str(),
    )
}

pub(crate) fn git_service(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateGitService,
    first: &ServiceStaged,
) -> Result<(), RpcError> {
    environment(
        tx,
        who,
        &c.environment,
        Some(&first.environment.id),
        c.id.as_str(),
    )
}

pub(crate) fn volume(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateVolume,
    first: &VolumeStaged,
) -> Result<(), RpcError> {
    environment(
        tx,
        who,
        &c.environment,
        Some(&first.environment.id),
        c.id.as_str(),
    )
}

pub(crate) fn config(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateConfig,
    first: &ConfigStaged,
) -> Result<(), RpcError> {
    environment(
        tx,
        who,
        &c.environment,
        Some(&first.environment.id),
        c.id.as_str(),
    )
}

/// A Retry names its Deployment, and a removal's Environment may be gone by the
/// replay: the removal that marked it is the consent either way.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &Admit,
    first: &DeploymentSummary,
) -> Result<(), RpcError> {
    let at = match c {
        Admit::Deploy(deploy) => &deploy.environment,
        Admit::ShutDown(shut_down) => &shut_down.environment,
        Admit::Retry(_) | Admit::Remove(_) => return Ok(()),
    };
    environment(tx, who, at, Some(&first.environment_id), c.id().as_str())
}

/// The Branch must still hang off the Environment `from` names now.
pub(crate) fn branch(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateBranch,
    first: &Branched,
) -> Result<(), RpcError> {
    let parent = crate::branch::row(tx, &first.branch.environment.id)?.map(|row| row.parent);
    environment(tx, who, &c.from, parent.as_ref(), c.id.as_str())
}

pub(crate) fn project(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &CreateEnvironment,
    first: &EnvironmentCreated,
) -> Result<(), RpcError> {
    let now = scope::project(tx, who, c.project.as_ref())?;
    if scope::project_of(tx, &first.environment.id)?.id == now.id {
        return Ok(());
    }
    Err(conflict(
        "This ID was already used in another Project; send a new ID to use this one",
        serde_json::json!({ "id": c.id }),
    ))
}
