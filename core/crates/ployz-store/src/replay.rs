//! What each keyed create's first answer must still fit for a replay to return it.
//! An omitted Project or Environment resolves to the Organization's only Project and
//! its Default Environment, which may have moved since the first call; a replay whose
//! scope now resolves to another place is refused rather than answered with the old
//! one. A scope that resolves nowhere, or to no single place, after a rename, removal
//! or a second Project, cannot name another place, so the first answer stands.

use crate::deployment::admit::Admit;
use crate::error::conflict;
use crate::id::ProjectId;
use crate::storage::Tx;
use crate::{
    Actor, Branched, CreateBranch, CreateConfig, CreateEnvironment, CreateGitService,
    CreateService, CreateVolume, DeploymentSummary, EnvironmentCreated, EnvironmentId,
    EnvironmentRef, Publish, Published, RpcError, scope,
};
use crate::{ConfigStaged, ServiceStaged, VolumeStaged};
use ployz_core::RpcErrorCode;

/// For a create with no Project or Environment to resolve.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every replay rule has one signature"
)]
pub(crate) const fn any<P, A>(_: &mut dyn Tx, _: &Actor, _: &P, _: &A) -> Result<(), RpcError> {
    Ok(())
}

/// The place a scope resolves to now, if it resolves to one.
fn now<T>(resolved: Result<T, RpcError>) -> Result<Option<T>, RpcError> {
    match resolved {
        Ok(place) => Ok(Some(place)),
        Err(error) if matches!(error.code, RpcErrorCode::NotFound | RpcErrorCode::Ambiguous) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
    first: Option<&EnvironmentId>,
    id: &str,
) -> Result<(), RpcError> {
    let Some(first) = first else {
        return Ok(());
    };
    match now(scope::resolve(tx, who, at))? {
        Some((_, now)) if now != *first => Err(conflict(
            "This ID was already used in another Environment; send a new ID to use this one.",
            serde_json::json!({ "id": id }),
        )),
        _ => Ok(()),
    }
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

/// A Retry names its Deployment rather than a scope.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    c: &Admit,
    first: &DeploymentSummary,
) -> Result<(), RpcError> {
    let at = match c {
        Admit::Deploy(deploy) => &deploy.environment,
        Admit::ShutDown(shut_down) => &shut_down.environment,
        Admit::Remove(removal) => &removal.environment,
        Admit::Retry(_) => return Ok(()),
    };
    environment(tx, who, at, Some(&first.environment_id), c.id().as_str())
}

/// The Branch must still hang off the Environment `from` names now, while it is open.
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
    let rows = tx.query(
        "SELECT project_id FROM config_environment WHERE id = ?1",
        &[first.environment.id.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(());
    };
    let first = row.parse::<ProjectId>(0, "Environment")?;
    match now(scope::project(tx, who, c.project.as_ref()))? {
        Some(now) if now.id != first => Err(conflict(
            "This ID was already used in another Project; send a new ID to use this one.",
            serde_json::json!({ "id": c.id }),
        )),
        _ => Ok(()),
    }
}
