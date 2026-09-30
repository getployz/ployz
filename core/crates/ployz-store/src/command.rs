//! `write`'s commands, registered once in the [`commands!`] table: each row is a
//! [`Command`] variant, the [`Written`] variant that answers it, the IDs a create is
//! replayed by, and the call that runs it.

use ployz_core::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

pub use crate::deployment::admit::{Admit, Cancel, Deploy, Removal, Retry, Start};
pub use crate::project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
    RenameProject,
};
pub use crate::review::publish::{Discard, Discarded, Publish, Published};
pub use crate::service::{
    CreateService, RemoveService, RenameService, ServiceStaged, ServiceSummary,
};
pub use crate::settings::edit::{Change, Edit, Edited};
pub use crate::volume::{
    CreateVolume, Mount, RemoveVolume, RenameVolume, SetVolumeStorage, VolumeStaged, VolumeSummary,
};

use crate::error;
use crate::scope::EnvironmentRef;
use crate::storage::Tx;
use crate::{Actor, SealingKey, Trusted};

/// What a command or query runs with: one open transaction, its caller, and the
/// evidence Cloud gathered. Only the Store makes one.
pub struct Call<'s> {
    pub(crate) tx: &'s mut dyn Tx,
    pub(crate) who: &'s Actor,
    pub(crate) sealing: &'s SealingKey,
    pub(crate) trusted: &'s Trusted,
}

/// A [`Command`], or one of its payloads, and what it answers with.
pub trait Tell: Serialize {
    /// What it answers with.
    type Written: Serialize + DeserializeOwned;

    /// Run it in `at`'s transaction, once for a create's IDs.
    ///
    /// # Errors
    /// What running it refuses.
    #[doc(hidden)]
    fn apply(&self, at: &mut Call<'_>) -> Result<Self::Written, RpcError>;

    /// The whole command, tagged, as `write` takes it over HTTPS.
    fn to_command(&self) -> Value;

    /// Its result out of a [`Written`], as this command's own.
    ///
    /// # Errors
    /// `internal` for the result of another command.
    fn written(written: Written) -> Result<Self::Written, RpcError>;
}

/// `CreateProject` as serde names it: `create_project`.
pub(crate) fn snake(name: &str) -> String {
    let mut snake = String::with_capacity(name.len() + 4);
    for (index, letter) in name.char_indices() {
        if letter.is_ascii_uppercase() && index > 0 {
            snake.push('_');
        }
        snake.push(letter.to_ascii_lowercase());
    }
    snake
}

/// Register every command. A row reads:
/// `/// doc` `Variant(Payload) -> WrittenVariant(Answer) [as *] [keyed [IDS]] => CALL;`
/// where `keyed` lists the caller-minted IDs a create replays by, and `CALL` runs it
/// with the names bound in the table's header.
macro_rules! commands {
    (
        |$tx:ident, $who:ident, $sealing:ident, $trusted:ident, $c:ident|
        $(
            $(#[doc = $doc:literal])*
            $variant:ident($payload:ty) -> $written:ident($answer:ty) $(as $unbox:tt)?
            $(keyed [$($id:expr),*])? => $call:expr;
        )*
    ) => {
        /// One change to authored configuration, applied in one transaction.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "command", rename_all = "snake_case")]
        #[ts(rename = "ConfigCommand")]
        pub enum Command {
            $($(#[doc = $doc])* $variant($payload),)*
        }

        impl Tell for Command {
            type Written = Written;

            fn apply(&self, at: &mut Call<'_>) -> Result<Written, RpcError> {
                match self {
                    $(Self::$variant(command) => {
                        command.apply(at).map(|answer| Written::$written(answer.into()))
                    })*
                }
            }

            fn to_command(&self) -> Value {
                serde_json::to_value(self).expect("commands are JSON")
            }

            fn written(written: Written) -> Result<Written, RpcError> {
                Ok(written)
            }
        }

        $(
            impl Tell for $payload {
                type Written = $answer;

                fn apply(&self, at: &mut Call<'_>) -> Result<$answer, RpcError> {
                    let ids: Vec<&str> = {
                        let $c = self;
                        let _ = $c;
                        vec![$($($id),*)?]
                    };
                    let run = |at: &mut Call<'_>| -> Result<$answer, RpcError> {
                        let $c = self;
                        let $tx: &mut dyn Tx = &mut *at.tx;
                        let ($who, $sealing, $trusted) = (at.who, at.sealing, at.trusted);
                        let _ = ($sealing, $trusted);
                        $call
                    };
                    match ids.is_empty() {
                        true => run(at),
                        false => replayable(at, self, &ids, run),
                    }
                }

                fn to_command(&self) -> Value {
                    let mut command = serde_json::to_value(self).expect("commands are JSON");
                    if let Some(fields) = command.as_object_mut() {
                        fields.insert("command".to_owned(), snake(stringify!($variant)).into());
                    }
                    command
                }

                fn written(written: Written) -> Result<$answer, RpcError> {
                    match written {
                        Written::$written(answer) => Ok($($unbox)? answer),
                        _ => Err(error::internal("The Store answered another command")),
                    }
                }
            }
        )*
    };
}

commands! {
    |tx, who, sealing, trusted, c|
    /// Create a Project with its Default Environment.
    CreateProject(CreateProject) -> Project(ProjectCreated)
        keyed [c.id.as_str(), c.default_environment.as_str()]
        => crate::project::create_project(tx, who, c);
    /// Rename a Project; its Namespaces stay.
    RenameProject(RenameProject) -> ProjectRenamed(ProjectSummary)
        => crate::project::rename_project(tx, who, c);
    /// Create an empty Environment.
    CreateEnvironment(CreateEnvironment) -> Environment(EnvironmentCreated)
        keyed [c.id.as_str()] => crate::project::create_environment(tx, who, c);
    /// Create an image Service, or an empty one.
    CreateService(CreateService) -> Service(ServiceStaged)
        keyed [c.id.as_str()] => crate::service::create_service(tx, who, c);
    /// Create a Service that builds a GitHub repository.
    CreateGitService(crate::CreateGitService) -> Service(ServiceStaged)
        keyed [c.id.as_str()] => crate::git::create_git_service(tx, who, c, trusted);
    /// Rename a Service, keeping its Private DNS name.
    RenameService(RenameService) -> ServiceRenamed(ServiceStaged)
        => crate::service::rename_service(tx, who, c);
    /// Remove a Service from Working State; a Deploy removes it.
    RemoveService(RemoveService) -> ServiceRemoved(ServiceStaged)
        => crate::service::remove_service(tx, who, c);
    /// Create a Volume, optionally mounted into Services.
    CreateVolume(CreateVolume) -> Volume(VolumeStaged)
        keyed [c.id.as_str()] => crate::volume::create_volume(tx, who, c);
    /// Change a Volume's storage before its first Deployment is requested.
    SetVolumeStorage(SetVolumeStorage) -> Volume(VolumeStaged)
        => crate::volume::set_storage(tx, who, c);
    /// Remove a Volume from Working State; a Deploy deletes its data.
    RemoveVolume(RemoveVolume) -> VolumeRemoved(VolumeStaged)
        => crate::volume::remove_volume(tx, who, c);
    /// Rename a Volume; mounts follow it.
    RenameVolume(RenameVolume) -> VolumeRenamed(VolumeStaged)
        => crate::volume::rename_volume(tx, who, c);
    /// Set and unset Settings in one Environment.
    Edit(Edit) -> Edited(Edited) => crate::settings::edit::edit(tx, who, sealing, c, trusted);
    /// Save Working State as the next Saved revision.
    Publish(Publish) -> Published(Published)
        => crate::review::publish::publish(tx, who, c, trusted);
    /// Return Working State, or part of it, to what is deployed.
    Discard(Discard) -> Discarded(Discarded) => crate::review::publish::discard(tx, who, c);
    /// Freeze Saved State into a queued Deployment, publishing Working State first,
    /// or queue again what an ended Deployment froze.
    Admit(Admit) -> Deployment(crate::DeploymentSummary)
        keyed [c.id().as_str()] => crate::deployment::admit::admit(tx, who, c, trusted);
    /// Hand a queued Deployment to a runner now.
    Start(Start) -> Deployment(crate::DeploymentSummary)
        => crate::deployment::start(tx, who, &c.deployment);
    /// Cancel a queued or running Deployment.
    Cancel(Cancel) -> Deployment(crate::DeploymentSummary)
        => crate::deployment::cancel(tx, who, &c.deployment);
    /// Give a Service a generated or custom public domain.
    AddDomain(crate::AddDomain) -> Domain(crate::DomainStaged)
        => crate::domain::add_domain(tx, who, c, trusted);
    /// Change the prefix of a Service's generated domain.
    SetGeneratedDomain(crate::SetGeneratedDomain) -> Domain(crate::DomainStaged)
        => crate::domain::set_generated_domain(tx, who, c, trusted);
    /// Take a public domain off its Service.
    RemoveDomain(crate::RemoveDomain) -> Domain(crate::DomainStaged)
        => crate::domain::remove_domain(tx, who, c, trusted);
    /// Make a Branch of an Environment.
    CreateBranch(crate::CreateBranch) -> Branch(crate::Branched)
        keyed [c.id.as_str()] => crate::branch::create_branch(tx, who, c);
    /// Move changes between a Branch and its Parent: Save or Update.
    Move(crate::Move) -> Moved(crate::Moved) as *
        => crate::branch::move_changes(tx, who, sealing, c);
    /// Turn a Branch's Live Node into an Own Copy.
    CopyNode(crate::CopyNode) -> Branch(crate::Branched) => crate::branch::copy_node(tx, who, c);
    /// Keep a Branch, or stop keeping it.
    KeepBranch(crate::KeepBranch) -> Branch(crate::Branched)
        => crate::branch::keep_branch(tx, who, c);
    /// Set the Organization's Build Order, at once.
    SetBuildOrder(crate::SetBuildOrder) -> BuildOrder(crate::BuildOrderView)
        => crate::builders::set_build_order(tx, who, c);
    /// Make an Environment its Project's Default Environment.
    SetDefaultEnvironment(crate::SetDefaultEnvironment)
        -> DefaultEnvironment(crate::EnvironmentsView)
        => crate::teardown::set_default(tx, who, c);
    /// Set the Setup Commands a Branch of an Environment runs once it is made.
    SetBranchSetup(crate::SetBranchSetup) -> BranchSetup(crate::EnvironmentsView)
        => crate::branch::set_branch_setup(tx, who, c);
    /// Delete an Environment nothing of which runs on the Servers.
    RemoveEnvironment(crate::RemoveEnvironment)
        -> EnvironmentRemoved(crate::Teardown<crate::EnvironmentRemoved>)
        => crate::teardown::remove(tx, who, c);
    /// Delete a Project nothing of which runs on the Servers.
    RemoveProject(crate::RemoveProject)
        -> ProjectRemoved(crate::Teardown<crate::ProjectRemoved>)
        => crate::teardown::remove_project(tx, who, c);
    /// Change a Project's PR plan for one repository.
    SetPrPlan(crate::SetPrPlan) -> PrPlans(crate::PrPlansView)
        => crate::pull_request::set_plan(tx, who, c);
    /// Creates and edits applied together, all or none.
    Batch(Batch) -> Batch(Batched) => {
        let mut at = Call { tx, who, sealing, trusted };
        c.commands
            .iter()
            .map(|command| command.apply(&c.environment, &mut at))
            .collect::<Result<_, _>>()
            .map(|results| Batched { results })
    };
}

/// Creates and edits of one Environment in one transaction, in order: all apply or
/// none do. Each create keeps its own caller-minted ID, so a retry replays it; an
/// edit applies again. Cloud gathers no trusted evidence inside a Batch, so an edit
/// that needs some (a repository or branch) is refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    /// The Environment every command writes. A command naming another is refused.
    #[serde(default)]
    pub environment: EnvironmentRef,
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

impl BatchCommand {
    /// Apply this command to the Batch's `environment`.
    fn apply(&self, environment: &EnvironmentRef, at: &mut Call<'_>) -> Result<Written, RpcError> {
        let named = match self {
            Self::CreateService(create) => &create.environment,
            Self::CreateVolume(create) => &create.environment,
            Self::Edit(edit) => &edit.environment,
        };
        if named != environment && *named != EnvironmentRef::default() {
            return Err(error::invalid(
                "Every command in a Batch writes the Batch's Environment",
                json!({ "environment": named }),
            ));
        }
        match self {
            Self::CreateService(create) => CreateService {
                environment: environment.clone(),
                ..create.clone()
            }
            .apply(at)
            .map(Written::Service),
            Self::CreateVolume(create) => CreateVolume {
                environment: environment.clone(),
                ..create.clone()
            }
            .apply(at)
            .map(Written::Volume),
            Self::Edit(edit) => Edit {
                environment: environment.clone(),
                ..edit.clone()
            }
            .apply(at)
            .map(Written::Edited),
        }
    }
}

/// A write's answer as Cloud gets it: what the command did, and the pull requests
/// whose GitHub check it may move, which Cloud publishes again.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[ts(rename = "ConfigCommitted")]
pub struct Committed {
    #[serde(flatten)]
    #[ts(flatten)]
    pub written: Written,
    /// The open PR Environments' pull requests in the Project of the Environment it
    /// wrote; none when it wrote none.
    pub checks: Vec<crate::PullRequestRef>,
}

impl Written {
    /// The Environment it wrote, if it wrote one that still exists.
    #[must_use]
    pub fn environment(&self) -> Option<&crate::EnvironmentId> {
        match self {
            Self::Environment(created) => Some(&created.environment.id),
            Self::Service(staged) | Self::ServiceRenamed(staged) | Self::ServiceRemoved(staged) => {
                Some(&staged.environment.id)
            }
            Self::Volume(staged) | Self::VolumeRemoved(staged) | Self::VolumeRenamed(staged) => {
                Some(&staged.environment.id)
            }
            Self::Edited(edited) => Some(&edited.environment.id),
            Self::Published(published) => Some(&published.environment.id),
            Self::Discarded(discarded) => Some(&discarded.environment.id),
            Self::Deployment(deployment) => Some(&deployment.environment_id),
            Self::Domain(staged) => Some(&staged.environment.id),
            Self::Moved(moved) => Some(&moved.into.id),
            Self::Batch(batched) => batched.results.last().and_then(Self::environment),
            // A new Project or Branch has no pull request yet; the rest write no
            // Environment's config, or delete it.
            Self::Project(_)
            | Self::ProjectRenamed(_)
            | Self::Automated(_)
            | Self::Branch(_)
            | Self::BuildOrder(_)
            | Self::DefaultEnvironment(_)
            | Self::BranchSetup(_)
            | Self::EnvironmentRemoved(_)
            | Self::ProjectRemoved(_)
            | Self::PrPlans(_) => None,
        }
    }
}

/// What each command of a [`Batch`] did, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Batched {
    /// One result per command.
    pub results: Vec<Written>,
}

/// What a command did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "written", rename_all = "snake_case")]
#[ts(rename = "ConfigWritten")]
pub enum Written {
    /// A Project was created.
    Project(ProjectCreated),
    /// A Project was renamed.
    ProjectRenamed(ProjectSummary),
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
    /// A Volume was renamed.
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
    /// A Branch setup changed: the Project's Environments after it.
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

/// Run `create` once for the caller-minted `ids`. Replaying the identical command
/// returns what the first run wrote; any of its IDs reused with another body, or
/// from another Organization, is `conflict`.
fn replayable<C: Tell + ?Sized>(
    at: &mut Call<'_>,
    command: &C,
    ids: &[&str],
    create: impl FnOnce(&mut Call<'_>) -> Result<C::Written, RpcError>,
) -> Result<C::Written, RpcError> {
    let body = command.to_command().to_string();
    let organization = at.who.organization.as_str();
    for id in ids {
        let rows = at.tx.query(
            "SELECT organization_id, command, written FROM config_create WHERE id = ?1",
            &[(*id).into()],
        )?;
        if let Some(row) = rows.first() {
            if row.text(0)? != organization || row.text(1)? != body {
                return Err(error::conflict(
                    "This ID was already used by a different create",
                    json!({ "id": id }),
                ));
            }
            return row.json(2, "create result");
        }
    }
    let written = create(at)?;
    let written_json = serde_json::to_string(&written).expect("results are JSON");
    for id in ids {
        at.tx.execute(
            "INSERT INTO config_create (id, organization_id, command, written) \
             VALUES (?1, ?2, ?3, ?4)",
            &[
                (*id).into(),
                organization.into(),
                body.as_str().into(),
                written_json.as_str().into(),
            ],
        )?;
    }
    Ok(written)
}
