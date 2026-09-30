//! `write`'s commands: each feature module adds one [`Command`] variant, one
//! [`Written`] variant and one arm in [`run`].

use ployz_core::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

pub use crate::deployment::admit::{Admit, Cancel, Deploy, Removal, Retry, Start};
pub(crate) use crate::deployment::admit::{admit, cancel, start};
pub use crate::project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
};
pub(crate) use crate::project::{create_environment, create_project, insert_environment};
pub use crate::review::publish::{Discard, Discarded, Publish, Published};
pub(crate) use crate::review::publish::{discard, publish};
pub use crate::service::{
    CreateService, RemoveService, RenameService, ServiceStaged, ServiceSummary,
};
pub(crate) use crate::service::{
    create_service, insert_service, remove_service, rename_service, summary,
};
pub(crate) use crate::settings::edit::edit;
pub use crate::settings::edit::{Change, Edit, Edited};
pub use crate::volume::{
    CreateVolume, Mount, RemoveVolume, RenameVolume, SetVolumeStorage, VolumeStaged, VolumeSummary,
};
pub(crate) use crate::volume::{
    check_storage, create_volume, locked_storage, remove_volume, rename_volume, set_storage,
    summary as volume_summary,
};

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
    /// Change a Volume's storage before its first Deployment is requested.
    SetVolumeStorage(SetVolumeStorage),
    /// Remove a Volume from Working State; a Deploy deletes its data.
    RemoveVolume(RemoveVolume),
    RenameVolume(RenameVolume),
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
    SetBranchSetup(crate::SetBranchSetup),
    /// Delete an Environment nothing of which runs on the Servers.
    RemoveEnvironment(crate::RemoveEnvironment),
    /// Delete a Project nothing of which runs on the Servers.
    RemoveProject(crate::RemoveProject),
    /// Change a Project's PR plan for one repository.
    SetPrPlan(crate::SetPrPlan),
    /// Creates and edits applied together, all or none.
    Batch(Batch),
}

/// Creates and edits in one transaction, in order: all apply or none do. Each create
/// keeps its own caller-minted ID, so a retry replays it; an edit applies again. Cloud
/// gathers no trusted evidence inside a Batch, so an edit that needs some (a repository
/// or branch) is refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    /// The commands, applied in order.
    pub commands: Vec<BatchCommand>,
}

/// A command a [`Batch`] may hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum BatchCommand {
    /// See [`Command::CreateService`].
    CreateService(CreateService),
    /// See [`Command::CreateVolume`].
    CreateVolume(CreateVolume),
    /// See [`Command::Edit`].
    Edit(Edit),
}

impl From<BatchCommand> for Command {
    fn from(command: BatchCommand) -> Self {
        match command {
            BatchCommand::CreateService(create) => Self::CreateService(create),
            BatchCommand::CreateVolume(create) => Self::CreateVolume(create),
            BatchCommand::Edit(edit) => Self::Edit(edit),
        }
    }
}

/// What each command of a [`Batch`] did, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Batched {
    /// One result per command.
    pub results: Vec<Written>,
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
            Self::Admit(admit) => vec![admit.id().as_str()],
            Self::CreateGitService(create) => vec![create.id.as_str()],
            Self::CreateVolume(create) => vec![create.id.as_str()],
            Self::CreateBranch(create) => vec![create.id.as_str()],
            Self::Edit(_)
            | Self::RenameService(_)
            | Self::RemoveService(_)
            | Self::RemoveVolume(_)
            | Self::RenameVolume(_)
            | Self::SetBranchSetup(_)
            | Self::SetVolumeStorage(_)
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
            | Self::RemoveEnvironment(_)
            | Self::RemoveProject(_)
            | Self::SetPrPlan(_)
            | Self::Batch(_) => Vec::new(),
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
    /// A Volume was created or its draft storage changed.
    Volume(VolumeStaged),
    /// A Volume was removed from Working State.
    VolumeRemoved(VolumeStaged),
    VolumeRenamed(VolumeStaged),
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
    Moved(Box<crate::Moved>),
    /// The Default Environment changed: the Project's Environments after it.
    DefaultEnvironment(crate::EnvironmentsView),
    BranchSetup(crate::EnvironmentsView),
    /// An Environment was deleted.
    EnvironmentRemoved(crate::Teardown<crate::EnvironmentRemoved>),
    /// A Project was deleted.
    ProjectRemoved(crate::Teardown<crate::ProjectRemoved>),
    /// A PR plan changed: the Project's PR plans after it.
    PrPlans(crate::PrPlansView),
    /// What each command of a Batch did.
    Batch(Batched),
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
        Command::SetVolumeStorage(set) => set_storage(tx, who, set).map(Written::Volume),
        Command::RemoveVolume(remove) => remove_volume(tx, who, remove).map(Written::VolumeRemoved),
        Command::RenameVolume(rename) => rename_volume(tx, who, rename).map(Written::VolumeRenamed),
        Command::Edit(edit) => self::edit(tx, who, sealing, edit, trusted).map(Written::Edited),
        Command::Publish(publish) => {
            self::publish(tx, who, publish, trusted).map(Written::Published)
        }
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
        Command::Move(request) => crate::branch::move_changes(tx, who, sealing, request)
            .map(|moved| Written::Moved(Box::new(moved))),
        Command::CopyNode(copy) => crate::branch::copy_node(tx, who, copy).map(Written::Branch),
        Command::KeepBranch(keep) => crate::branch::keep_branch(tx, who, keep).map(Written::Branch),
        Command::SetBuildOrder(set) => {
            crate::builders::set_build_order(tx, who, set).map(Written::BuildOrder)
        }
        Command::SetBranchSetup(set) => {
            crate::teardown::set_branch_setup(tx, who, set).map(Written::BranchSetup)
        }
        Command::SetDefaultEnvironment(set) => {
            crate::teardown::set_default(tx, who, set).map(Written::DefaultEnvironment)
        }
        Command::RemoveEnvironment(remove) => {
            crate::teardown::remove(tx, who, remove).map(Written::EnvironmentRemoved)
        }
        Command::RemoveProject(remove) => {
            crate::teardown::remove_project(tx, who, remove).map(Written::ProjectRemoved)
        }
        Command::SetPrPlan(set) => {
            crate::pull_request::set_plan(tx, who, set).map(Written::PrPlans)
        }
        Command::Batch(batch) => batch
            .commands
            .iter()
            .map(|command| run(tx, who, sealing, &command.clone().into(), trusted))
            .collect::<Result<_, _>>()
            .map(|results| Written::Batch(Batched { results })),
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
            return row.json(2, "create result");
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

/// A [`Command`], or one of its payloads, and what it answers with.
pub trait Tell: Clone {
    type Written;
    fn command(self) -> Command;
    /// The result, as this command's own.
    ///
    /// # Errors
    /// `internal` for the result of another command.
    fn written(written: Written) -> Result<Self::Written, RpcError>;
}

impl Tell for Command {
    type Written = Written;
    fn command(self) -> Command {
        self
    }
    fn written(written: Written) -> Result<Written, RpcError> {
        Ok(written)
    }
}

macro_rules! tells {
    ($($command:ty => $variant:ident / $written:ident($answer:ty) $(as $unbox:tt)?),* $(,)?) => {$(
        impl Tell for $command {
            type Written = $answer;
            fn command(self) -> Command {
                Command::$variant(self)
            }
            fn written(written: Written) -> Result<$answer, RpcError> {
                match written {
                    Written::$written(answer) => Ok($($unbox)? answer),
                    _ => Err(crate::error::internal("The Store answered another command")),
                }
            }
        }
    )*};
}

tells!(
    CreateProject => CreateProject / Project(ProjectCreated),
    CreateEnvironment => CreateEnvironment / Environment(EnvironmentCreated),
    CreateService => CreateService / Service(ServiceStaged),
    crate::CreateGitService => CreateGitService / Service(ServiceStaged),
    RenameService => RenameService / ServiceRenamed(ServiceStaged),
    RemoveService => RemoveService / ServiceRemoved(ServiceStaged),
    CreateVolume => CreateVolume / Volume(VolumeStaged),
    RemoveVolume => RemoveVolume / VolumeRemoved(VolumeStaged),
    RenameVolume => RenameVolume / VolumeRenamed(VolumeStaged),
    SetVolumeStorage => SetVolumeStorage / Volume(VolumeStaged),
    Edit => Edit / Edited(Edited),
    Publish => Publish / Published(Published),
    Discard => Discard / Discarded(Discarded),
    Admit => Admit / Deployment(crate::DeploymentSummary),
    Start => Start / Deployment(crate::DeploymentSummary),
    Cancel => Cancel / Deployment(crate::DeploymentSummary),
    crate::AddDomain => AddDomain / Domain(crate::DomainStaged),
    crate::RemoveDomain => RemoveDomain / Domain(crate::DomainStaged),
    crate::CreateBranch => CreateBranch / Branch(crate::Branched),
    crate::Move => Move / Moved(crate::Moved) as *,
    crate::CopyNode => CopyNode / Branch(crate::Branched),
    crate::KeepBranch => KeepBranch / Branch(crate::Branched),
    crate::SetBuildOrder => SetBuildOrder / BuildOrder(crate::BuildOrderView),
    crate::SetDefaultEnvironment => SetDefaultEnvironment / DefaultEnvironment(crate::EnvironmentsView),
    crate::SetBranchSetup => SetBranchSetup / BranchSetup(crate::EnvironmentsView),
    crate::RemoveEnvironment => RemoveEnvironment / EnvironmentRemoved(crate::Teardown<crate::EnvironmentRemoved>),
    crate::RemoveProject => RemoveProject / ProjectRemoved(crate::Teardown<crate::ProjectRemoved>),
    crate::SetPrPlan => SetPrPlan / PrPlans(crate::PrPlansView),
    Batch => Batch / Batch(Batched),
);
