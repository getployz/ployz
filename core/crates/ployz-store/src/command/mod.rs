//! `write`'s commands. Each family lives in its own module and adds one
//! [`Command`] variant, one [`Written`] variant, one arm in [`run`], and a typed
//! method on [`ConfigStore`](crate::ConfigStore) that calls the same function.

mod admit;
mod edit;
mod project;
mod review;
mod service;
mod volume;

use ployz_core::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

pub use admit::{Admit, Cancel, Start};
pub(crate) use admit::{admit, cancel, start};
pub(crate) use edit::edit;
pub use edit::{Change, Edit, Edited};
pub use project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
};
pub(crate) use project::{create_environment, create_project, insert_environment};
pub use review::{Discard, Discarded, Publish, Published};
pub(crate) use review::{discard, publish};
pub use service::{CreateService, RemoveService, RenameService, ServiceStaged, ServiceSummary};
pub(crate) use service::{create_service, insert_service, remove_service, rename_service, summary};
pub use volume::{CreateVolume, Mount, RemoveVolume, VolumeStaged, VolumeSummary};
pub(crate) use volume::{create_volume, remove_volume, summary as volume_summary};

use crate::error;
use crate::storage::Tx;
use crate::{Actor, CreateGitService, Trusted};

/// One change to authored configuration, applied in one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "command", rename_all = "snake_case")]
#[ts(rename = "ConfigCommand")]
pub enum Command {
    /// Create a Project with its Default Environment.
    CreateProject(CreateProject),
    /// Create an empty Environment.
    CreateEnvironment(CreateEnvironment),
    /// Create an image Service, or an empty one.
    CreateService(CreateService),
    /// Create a Service that builds a GitHub repository.
    CreateGitService(CreateGitService),
    /// Rename a Service, keeping its Private DNS name.
    RenameService(RenameService),
    /// Remove a Service from Working State; a Deploy removes it.
    RemoveService(RemoveService),
    /// Create a Volume, optionally mounted into Services.
    CreateVolume(CreateVolume),
    /// Remove a Volume from Working State; a Deploy deletes its data.
    RemoveVolume(RemoveVolume),
    /// Set and unset Settings in one Environment.
    Edit(Edit),
    /// Save Working State as the next Saved revision.
    Publish(Publish),
    /// Return Working State, or part of it, to what is deployed.
    Discard(Discard),
    /// Freeze Saved State into a queued Deployment, publishing Working State first,
    /// or queue again what an ended Deployment froze.
    Admit(Admit),
    /// Hand a queued Deployment to a runner now.
    Start(Start),
    /// Cancel a queued or running Deployment.
    Cancel(Cancel),
    /// Give a Service a generated or custom public domain.
    AddDomain(crate::AddDomain),
    /// Take a public domain off its Service.
    RemoveDomain(crate::RemoveDomain),
    /// Make a Branch of an Environment.
    CreateBranch(crate::CreateBranch),
    /// Move changes between a Branch and its Parent: Save or Update.
    Move(crate::Move),
    /// Turn a Branch's Live Node into an Own Copy.
    CopyNode(crate::CopyNode),
    /// Keep a Branch, or stop keeping it.
    KeepBranch(crate::KeepBranch),
    /// Set the Organization's Build Order, at once.
    SetBuildOrder(crate::SetBuildOrder),
    /// Make an Environment its Project's Default Environment.
    SetDefaultEnvironment(crate::SetDefaultEnvironment),
    /// Delete an Environment nothing of which runs on the Servers.
    RemoveEnvironment(crate::RemoveEnvironment),
}

impl Command {
    /// The caller-minted IDs a create is keyed by, so a retry replays it. Empty
    /// for commands that create nothing.
    fn create_ids(&self) -> Vec<&str> {
        match self {
            Self::CreateProject(create) => {
                vec![create.id.as_str(), create.default_environment.as_str()]
            }
            Self::CreateEnvironment(create) => vec![create.id.as_str()],
            Self::CreateService(create) => vec![create.id.as_str()],
            Self::Admit(admit) => vec![admit.id.as_str()],
            Self::CreateGitService(create) => vec![create.id.as_str()],
            Self::CreateVolume(create) => vec![create.id.as_str()],
            Self::CreateBranch(create) => vec![create.id.as_str()],
            Self::Edit(_)
            | Self::RenameService(_)
            | Self::RemoveService(_)
            | Self::RemoveVolume(_)
            | Self::Publish(_)
            | Self::Discard(_)
            | Self::Start(_)
            | Self::Cancel(_)
            | Self::AddDomain(_)
            | Self::RemoveDomain(_)
            | Self::Move(_)
            | Self::CopyNode(_)
            | Self::KeepBranch(_)
            | Self::SetBuildOrder(_)
            | Self::SetDefaultEnvironment(_)
            | Self::RemoveEnvironment(_) => Vec::new(),
        }
    }
}

/// What a command did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "written", rename_all = "snake_case")]
#[ts(rename = "ConfigWritten")]
pub enum Written {
    /// A Project was created.
    Project(ProjectCreated),
    /// An Environment was created.
    Environment(EnvironmentCreated),
    /// A Service was created.
    Service(ServiceStaged),
    /// A Service was renamed.
    ServiceRenamed(ServiceStaged),
    /// A Service was removed from Working State.
    ServiceRemoved(ServiceStaged),
    /// A Volume was created.
    Volume(VolumeStaged),
    /// A Volume was removed from Working State.
    VolumeRemoved(VolumeStaged),
    /// Settings were edited.
    Edited(Edited),
    /// Working State was published.
    Published(Published),
    /// Changes were discarded.
    Discarded(Discarded),
    /// A Deployment admitted, started or cancelled, or what its runner recorded.
    Deployment(crate::DeploymentSummary),
    /// A public domain was added or removed.
    Domain(crate::DomainStaged),
    /// What a system event made the Store do.
    Automated(crate::Automated),
    /// A Branch was made, given an Own Copy, or kept.
    Branch(crate::Branched),
    /// The Build Order was set; it applies to the next build.
    BuildOrder(crate::BuildOrderView),
    /// Changes moved between a Branch and its Parent.
    Moved(crate::Moved),
    /// The Default Environment changed: the Project's Environments after it.
    DefaultEnvironment(crate::EnvironmentsView),
    /// An Environment was deleted.
    EnvironmentRemoved(crate::EnvironmentRemoved),
}

pub(crate) fn run(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &crate::SealingKey,
    command: &Command,
    trusted: &Trusted,
) -> Result<Written, RpcError> {
    match command {
        Command::CreateProject(create) => create_project(tx, who, create).map(Written::Project),
        Command::CreateEnvironment(create) => {
            create_environment(tx, who, create).map(Written::Environment)
        }
        Command::CreateService(create) => create_service(tx, who, create).map(Written::Service),
        Command::CreateGitService(create) => {
            crate::git::create_git_service(tx, who, create, trusted).map(Written::Service)
        }
        Command::RenameService(rename) => {
            rename_service(tx, who, rename).map(Written::ServiceRenamed)
        }
        Command::RemoveService(remove) => {
            remove_service(tx, who, remove).map(Written::ServiceRemoved)
        }
        Command::CreateVolume(create) => create_volume(tx, who, create).map(Written::Volume),
        Command::RemoveVolume(remove) => remove_volume(tx, who, remove).map(Written::VolumeRemoved),
        Command::Edit(edit) => self::edit(tx, who, sealing, edit, trusted).map(Written::Edited),
        Command::Publish(publish) => self::publish(tx, who, publish).map(Written::Published),
        Command::Discard(discard) => self::discard(tx, who, discard).map(Written::Discarded),
        Command::Admit(request) => admit(tx, who, request, trusted).map(Written::Deployment),
        Command::Start(request) => start(tx, who, request).map(Written::Deployment),
        Command::Cancel(request) => cancel(tx, who, request).map(Written::Deployment),
        Command::AddDomain(add) => {
            crate::domain::add_domain(tx, who, add, trusted).map(Written::Domain)
        }
        Command::RemoveDomain(remove) => {
            crate::domain::remove_domain(tx, who, remove, trusted).map(Written::Domain)
        }
        Command::CreateBranch(create) => {
            crate::branch::create_branch(tx, who, create).map(Written::Branch)
        }
        Command::Move(request) => crate::branch::move_changes(tx, who, request).map(Written::Moved),
        Command::CopyNode(copy) => crate::branch::copy_node(tx, who, copy).map(Written::Branch),
        Command::KeepBranch(keep) => crate::branch::keep_branch(tx, who, keep).map(Written::Branch),
        Command::SetBuildOrder(set) => {
            crate::builders::set_build_order(tx, who, set).map(Written::BuildOrder)
        }
        Command::SetDefaultEnvironment(set) => {
            crate::teardown::set_default(tx, who, set).map(Written::DefaultEnvironment)
        }
        Command::RemoveEnvironment(remove) => {
            crate::teardown::remove(tx, who, remove).map(Written::EnvironmentRemoved)
        }
    }
}

/// Run a create keyed by its caller-minted IDs once. Replaying the identical
/// command returns what the first run wrote; any of its IDs reused with another
/// body, or from another Organization, is `conflict`.
pub(crate) fn replayable<T: Serialize + DeserializeOwned>(
    tx: &mut dyn Tx,
    who: &Actor,
    command: &Command,
    create: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    let body = serde_json::to_string(command).expect("commands are JSON");
    let ids = command.create_ids();
    for id in &ids {
        let rows = tx.query(
            "SELECT organization_id, command, written FROM config_create WHERE id = ?1",
            &[(*id).into()],
        )?;
        if let Some(row) = rows.first() {
            if row.text(0)? != who.organization.as_str() || row.text(1)? != body {
                return Err(error::conflict(
                    "This ID was already used by a different create",
                    json!({ "id": id }),
                ));
            }
            return serde_json::from_str(row.text(2)?).map_err(|_| error::corrupt("create result"));
        }
    }
    let written = create(tx)?;
    let written_json = serde_json::to_string(&written).expect("results are JSON");
    for id in ids {
        tx.execute(
            "INSERT INTO config_create (id, organization_id, command, written) \
             VALUES (?1, ?2, ?3, ?4)",
            &[
                id.into(),
                who.organization.as_str().into(),
                body.as_str().into(),
                written_json.as_str().into(),
            ],
        )?;
    }
    Ok(written)
}
