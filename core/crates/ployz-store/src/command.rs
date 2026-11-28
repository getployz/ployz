//! `write`'s commands, registered once in the [`commands!`] table: each row is a
//! [`Command`] variant, the [`Written`] variant that answers it, the IDs a create is
//! replayed by, and the call that runs it.

use ployz_core::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

pub use crate::config_item::{
    AttachConfig, ConfigFileSummary, ConfigMountAt, ConfigStaged, ConfigSummary, CreateConfig,
    DeleteConfig, DetachConfig, PutConfigFile, RemoveConfigFile, RenameConfig,
};
pub use crate::deployment::admit::{
    Admit, AdmitRemoval, Cancel, Deploy, Reapply, Retry, ShutDown, Start,
};
pub use crate::project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
    RenameProject,
};
pub use crate::review::history::{HistoryStaged, NotSetVariable, StageHistory};
pub use crate::review::publish::{
    Discard, DiscardTarget, Discarded, Publish, Published, Reversal, WayBack,
};
pub use crate::service::{
    CreateService, RemoveService, RenameService, ServiceStaged, ServiceSummary,
};
pub use crate::settings::edit::{
    Change, Edit, Edited, PlaintextSecret, SettingNote, TypedAddresses,
};
pub use crate::typed_address::Instead;
pub use crate::volume::{
    CreateVolume, Mount, RemoveVolume, RenameVolume, SetVolumeSharedWrites, SetVolumeStorage,
    VolumeStaged, VolumeSummary,
};

use crate::error;
use crate::followup::FollowUp;
use crate::scope::EnvironmentRef;
use crate::storage::Tx;
use crate::storage::json_text;
use crate::{Actor, Permission, Trusted};

/// What a command or query runs with: one open transaction, its caller, and the
/// evidence Cloud gathered. Only the Store makes one.
pub struct Call<'s> {
    pub(crate) tx: &'s mut dyn Tx,
    pub(crate) who: &'s Actor,
    pub(crate) trusted: &'s Trusted,
}

/// A [`Command`], or one of its payloads, and what it answers with.
pub trait Tell: Serialize {
    /// The result `write` returns for it: a [`Written`] variant's payload, or
    /// [`Written`] itself for a whole [`Command`].
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

/// The agent tools that send a command, or why none does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// Each tool by the name a model calls it by.
    Tools(&'static [&'static str]),
    None(&'static str),
}

macro_rules! claim {
    ([$($tool:ident),+]) => {
        Claim::Tools(&[$(stringify!($tool)),+])
    };
    (none $reason:literal) => {
        Claim::None($reason)
    };
}

/// The body of `decode` for a table whose enum is tagged by `$tag`: serde buffers an
/// internally tagged payload and loses the path inside it, so a refused payload is
/// decoded again on its own, without the tag.
macro_rules! tagged_decode {
    ($value:ident, $tag:literal, [$($variant:ident),*]) => {{
        use ployz_core::decode::decode;
        if let Ok(decoded) = Self::deserialize(&$value) {
            return Ok(decoded);
        }
        #[derive(Deserialize)]
        #[serde(tag = $tag, rename_all = "snake_case")]
        enum Tag {
            $($variant(serde::de::IgnoredAny),)*
        }
        let Ok(tag) = Tag::deserialize(&$value) else {
            return decode(&$value);
        };
        if let Value::Object(fields) = &mut $value {
            fields.remove($tag);
        }
        match tag {
            $(Tag::$variant(_) => decode(&$value).map(Self::$variant),)*
        }
    }};
}
pub(crate) use tagged_decode;

/// A payload that writes into the Environment its caller names.
pub(crate) trait InEnvironment: Clone {
    fn environment(&self) -> &EnvironmentRef;

    /// This payload written into `to` instead.
    #[must_use]
    fn rebound(&self, to: &EnvironmentRef) -> Self;
}

macro_rules! in_environment {
    ($($payload:ty),+ $(,)?) => {$(
        impl InEnvironment for $payload {
            fn environment(&self) -> &EnvironmentRef {
                &self.environment
            }

            fn rebound(&self, to: &EnvironmentRef) -> Self {
                Self {
                    environment: to.clone(),
                    ..self.clone()
                }
            }
        }
    )+};
}

in_environment!(
    CreateService,
    crate::CreateGitService,
    CreateVolume,
    CreateConfig,
    PutConfigFile,
    RemoveConfigFile,
    RenameConfig,
    DeleteConfig,
    AttachConfig,
    DetachConfig,
    Edit,
    Discard,
    crate::NeverSync,
    Publish,
);

/// Register every command. A row reads:
/// `/// doc` `Variant(Payload) -> WrittenVariant(Answer) [as *] [keyed [IDS] if SAME]
/// needs PERMISSION agent CLAIM => CALL;`
/// where `keyed` lists the caller-minted IDs a create replays by (an optional one when
/// the caller may send none), `SAME` (see [`crate::replay`]) refuses a replay its first
/// answer no longer fits, `PERMISSION` is the [`Permission`] the caller must hold,
/// checked before the row replays or runs, `CLAIM` is `[tool, ...]`, the agent tools
/// that send it by the names a model calls them, or `none "why"`, and `CALL` runs it
/// with the names bound in the table's header.
macro_rules! commands {
    (
        |$tx:ident, $who:ident, $trusted:ident, $c:ident|
        $(
            $(#[doc = $doc:literal])*
            $variant:ident($payload:ty) -> $written:ident($answer:ty) $(as $unbox:tt)?
            $(keyed [$($id:expr),*] if $($same:ident)::+)?
            needs $needs:ident
            agent $claim:tt $($reason:literal)? => $call:expr;
        )*
    ) => {
        /// One change to authored configuration, applied in one transaction.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "command", rename_all = "snake_case")]
        #[ts(rename = "ConfigCommand")]
        pub enum Command {
            $($(#[doc = $doc])* $variant($payload),)*
        }

        impl Command {
            /// Every command by the name it is sent as, and its [`Claim`].
            pub fn claims() -> impl Iterator<Item = (String, Claim)> {
                #[derive(Serialize)]
                #[serde(rename_all = "snake_case")]
                enum Sent {
                    $($variant,)*
                }
                [$((Sent::$variant, claim!($claim $($reason)?)),)*]
                    .into_iter()
                    .map(|(name, claim)| match serde_json::to_value(name) {
                        Ok(Value::String(name)) => (name, claim),
                        _ => unreachable!("a unit variant serializes to its name"),
                    })
            }

            /// Refuses this command unless each registry login it sets is sealed, kept
            /// or removed, as [`Edit::refuse_unsealed_logins`] reads an Edit, alone or
            /// in a Batch. A client asks before it sends anything.
            ///
            /// # Errors
            /// What [`Edit::refuse_unsealed_logins`] refuses.
            pub fn refuse_unsealed_logins(&self) -> Result<(), RpcError> {
                let edits: Vec<&Edit> = if let Self::Edit(edit) = self {
                    vec![edit]
                } else if let Self::Batch(batch) = self {
                    batch
                        .commands
                        .iter()
                        .filter_map(|command| {
                            if let BatchCommand::Edit(edit) = command {
                                Some(edit)
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                edits.into_iter().try_for_each(Edit::refuse_unsealed_logins)
            }

            /// Decode a command a caller sent.
            ///
            /// # Errors
            /// Where it does not fit, inside its payload too, and what fits there;
            /// never a value it holds.
            pub fn decode(mut value: Value) -> Result<Self, ployz_core::decode::DecodeError> {
                tagged_decode!(value, "command", [$($variant),*])
            }
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
                    at.trusted.admit(Permission::$needs)?;
                    let run = |at: &mut Call<'_>| -> Result<$answer, RpcError> {
                        let $c = self;
                        let $tx: &mut dyn Tx = &mut *at.tx;
                        let ($who, $trusted) = (at.who, at.trusted);
                        let _ = $trusted;
                        $call
                    };
                    $(
                        let $c = self;
                        let ids: Vec<&str> =
                            [$(Option::<&str>::from($id)),*].into_iter().flatten().collect();
                        if let Some(&id) = ids.first() {
                            let fits = |at: &mut Call<'_>, first: &$answer| {
                                $($same)::+(&mut *at.tx, at.who, self, first, id)
                            };
                            let answer = replayable(at, self, &ids, fits, run)?;
                            at.trusted.refuse_written_outside_environments(&mut *at.tx, &answer)?;
                            return Ok(answer);
                        }
                    )?
                    let answer = run(at)?;
                    at.trusted.refuse_written_outside_environments(&mut *at.tx, &answer)?;
                    Ok(answer)
                }

                fn to_command(&self) -> Value {
                    serde_json::to_value(Command::$variant(self.clone())).expect("commands are JSON")
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
    ($($row:tt)*) => {
        compile_error!(
            "a `commands!` row reads `Variant(Payload) -> Written(Answer) [as *] \
             [keyed [IDS] if SAME] needs PERMISSION agent CLAIM => CALL;`: a keyed row \
             states its replay rule with `if SAME`, a fn from `crate::replay`, every row \
             names the `Permission` the caller must hold, `needs ChangesStage`, and the \
             agent tools that send it, `agent [tool, ...]`, or why none does, \
             `agent none \"why\"`"
        );
    };
}

commands! {
    |tx, who, trusted, c|
    /// Create a Project with its Default Environment.
    CreateProject(CreateProject) -> Project(ProjectCreated)
        keyed [c.id.as_str(), c.default_environment.as_str()] if crate::replay::any
        needs ProjectCreate
        agent [project_create]
        => crate::project::create_project(tx, who, c, trusted);
    /// Rename a Project; its Namespaces stay.
    RenameProject(RenameProject) -> ProjectRenamed(ProjectSummary)
        needs ProjectEdit
        agent [project_rename]
        => crate::project::rename_project(tx, who, c);
    /// Create an empty Environment.
    CreateEnvironment(CreateEnvironment) -> Environment(EnvironmentCreated)
        keyed [c.id.as_str()] if crate::replay::project
        needs EnvironmentCreate
        agent none "No tool creates an Environment."
        => crate::project::create_environment(tx, who, c, trusted);
    /// Create an image Service, or an empty one.
    CreateService(CreateService) -> Service(ServiceStaged)
        keyed [c.id.as_str()] if crate::replay::same_environment
        needs ChangesStage
        agent [service_add]
        => crate::service::create_service(tx, who, c, trusted);
    /// Create a Service that builds a GitHub repository.
    CreateGitService(crate::CreateGitService) -> Service(ServiceStaged)
        keyed [c.id.as_str()] if crate::replay::same_environment
        needs ChangesStage
        agent [service_add]
        => crate::git::create_git_service(tx, who, c, trusted);
    /// Rename a Service, keeping its Private DNS name.
    RenameService(RenameService) -> ServiceRenamed(ServiceStaged)
        needs ChangesStage
        agent [service_rename]
        => crate::service::rename_service(tx, who, c, trusted);
    /// Remove a Service from Working State; a Deploy removes it.
    RemoveService(RemoveService) -> ServiceRemoved(ServiceStaged)
        needs ChangesStage
        agent [service_remove]
        => crate::service::remove_service(tx, who, c, trusted);
    /// Create a Volume, optionally mounted into Services.
    CreateVolume(CreateVolume) -> Volume(VolumeStaged)
        keyed [c.id.as_str()] if crate::replay::same_environment
        needs ChangesStage
        agent [volume_add]
        => crate::volume::create_volume(tx, who, c, trusted);
    /// Change a draft Volume's storage; a deployed Managed Volume resizes through
    /// `ployz volume set --size` instead.
    SetVolumeStorage(SetVolumeStorage) -> Volume(VolumeStaged)
        needs ChangesStage
        agent [volume_update]
        => crate::volume::set_storage(tx, who, c, trusted);
    /// Allow or refuse more than one writer of a Volume; applies at once.
    SetVolumeSharedWrites(SetVolumeSharedWrites) -> Volume(VolumeStaged)
        needs ChangesStage
        agent [volume_update]
        => crate::volume::set_shared_writes(tx, who, c, trusted);
    /// Remove a Volume from Working State; a Deploy deletes its data.
    RemoveVolume(RemoveVolume) -> VolumeRemoved(VolumeStaged)
        needs ChangesStage
        agent [volume_remove]
        => crate::volume::remove_volume(tx, who, c, trusted);
    /// Rename a Volume; mounts follow it.
    RenameVolume(RenameVolume) -> VolumeRenamed(VolumeStaged)
        needs ChangesStage
        agent [volume_update]
        => crate::volume::rename_volume(tx, who, c, trusted);
    /// Create a Config, optionally mounted into Services.
    CreateConfig(CreateConfig) -> Config(ConfigStaged)
        keyed [c.id.as_str()] if crate::replay::same_environment
        needs ChangesStage
        agent [config_write]
        => crate::config_item::create_config(tx, who, c, trusted);
    /// Put a file into a Config, replacing one of that name.
    PutConfigFile(PutConfigFile) -> Config(ConfigStaged)
        needs ChangesStage
        agent [config_write]
        => crate::config_item::put_file(tx, who, c, trusted);
    /// Remove a file from a Config.
    RemoveConfigFile(RemoveConfigFile) -> Config(ConfigStaged)
        needs ChangesStage
        agent [config_remove]
        => crate::config_item::remove_file(tx, who, c, trusted);
    /// Rename a Config; mounts follow it.
    RenameConfig(RenameConfig) -> ConfigRenamed(ConfigStaged)
        needs ChangesStage
        agent [config_rename]
        => crate::config_item::rename_config(tx, who, c, trusted);
    /// Delete a Config from Working State, unmounting it everywhere.
    DeleteConfig(DeleteConfig) -> ConfigRemoved(ConfigStaged)
        needs ChangesStage
        agent [config_remove]
        => crate::config_item::delete_config(tx, who, c, trusted);
    /// Mount a Config into a Service at a directory.
    AttachConfig(AttachConfig) -> Config(ConfigStaged)
        needs ChangesStage
        agent [config_write]
        => crate::config_item::attach_config(tx, who, c, trusted);
    /// Unmount a Config from a Service.
    DetachConfig(DetachConfig) -> Config(ConfigStaged)
        needs ChangesStage
        agent none "service_update unmounts a Config with an edit."
        => crate::config_item::detach_config(tx, who, c, trusted);
    /// Set and unset Settings in one Environment.
    Edit(Edit) -> Edited(Edited)
        needs ChangesStage
        agent [service_update, environment_region]
        => crate::settings::edit::edit(tx, who, c, trusted);
    /// Save Working State as the next Saved revision.
    Publish(Publish) -> Published(Published)
        keyed [c.id.as_deref()] if crate::replay::same_environment
        needs ChangesStage
        agent [deploy]
        => crate::review::publish::publish(tx, who, c, trusted);
    /// Stage a guarded Restore or Undo of an immutable Saved revision.
    StageHistory(StageHistory) -> HistoryStaged(HistoryStaged)
        needs ChangesStage
        agent [history]
        => crate::review::history::stage(tx, who, c, trusted);
    /// Return Working State, or part of it, to what is deployed.
    Discard(Discard) -> Discarded(Discarded)
        needs ChangesStage
        agent [discard]
        => crate::review::publish::discard(tx, who, c, trusted);
    /// Freeze Saved State into a queued Deployment, publishing Working State first,
    /// or queue again what an ended Deployment froze.
    Admit(Admit) -> Deployment(crate::DeploymentSummary)
        keyed [c.id().as_str()] if crate::replay::admit
        needs ChangesDeploy
        agent [deploy, deployment_retry]
        => crate::deployment::admit::admit(tx, who, c, trusted);
    /// Hand a queued Deployment to a runner now.
    Start(Start) -> Deployment(crate::DeploymentSummary)
        needs ChangesDeploy
        agent [deployment_start]
        => crate::deployment::start(tx, who, &c.deployment, trusted);
    /// Cancel a queued or running Deployment.
    Cancel(Cancel) -> Deployment(crate::DeploymentSummary)
        needs ChangesDeploy
        agent [deployment_cancel]
        => crate::removal::cancel(tx, who, &c.deployment, trusted);
    /// Give a Service a generated or custom public domain.
    AddDomain(crate::AddDomain) -> DomainAdded(crate::DomainAdded)
        needs ChangesStage
        agent [domain_add, domain_update]
        => crate::domain::add_domain(tx, who, c, trusted);
    /// Change the prefix of a Service's generated domain.
    SetGeneratedDomain(crate::SetGeneratedDomain) -> Domain(crate::DomainStaged)
        needs ChangesStage
        agent [domain_update]
        => crate::domain::set_generated_domain(tx, who, c, trusted);
    /// Take a public domain off its Service.
    RemoveDomain(crate::RemoveDomain) -> Domain(crate::DomainStaged)
        needs ChangesStage
        agent [domain_remove]
        => crate::domain::remove_domain(tx, who, c, trusted);
    /// Make a Branch of an Environment.
    CreateBranch(crate::CreateBranch) -> Branch(crate::Branched)
        keyed [c.id.as_str()] if crate::replay::branch
        needs EnvironmentCreate
        agent none "No tool works on branches."
        => crate::branch::create_branch(tx, who, c, trusted);
    /// Sync one Environment's changes into another of its Project: staged there,
    /// never deleting or deploying.
    Sync(crate::SyncChanges) -> Synced(crate::Synced)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::sync(tx, who, c, trusted);
    /// Undo a Sync while what it staged is undeployed and unchanged, or forget the
    /// offer it made.
    UndoSync(crate::UndoSync) -> Undone(crate::Undone)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::undo(tx, who, c, trusted);
    /// Remove a source included in a draft: what it still owns there goes back.
    RemoveProposal(crate::RemoveProposal) -> Removed(crate::Removed)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::remove_proposal(tx, who, c, trusted);
    /// Stage the hints a Parent left in an Environment.
    Take(crate::Take) -> Taken(crate::Taken)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::take(tx, who, c, trusted);
    /// Turn a Branch's Live Node into an Own Copy.
    CopyNode(crate::CopyNode) -> Branch(crate::Branched)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::copy_node(tx, who, c, trusted);
    /// Mark rows of an Environment Never sync, or sync them again.
    NeverSync(crate::NeverSync) -> NeverSynced(crate::NeverSynced)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::never_sync(tx, who, c);
    /// Keep a Branch, or stop keeping it.
    KeepBranch(crate::KeepBranch) -> Branch(crate::Branched)
        needs ChangesStage
        agent none "No tool works on branches."
        => crate::branch::keep_branch(tx, who, c);
    /// Set the Organization's Build Order, at once.
    SetBuildOrder(crate::SetBuildOrder) -> BuildOrder(crate::BuildOrderView)
        needs OrgSettings
        agent none "No tool sets the build order."
        => crate::builders::set_build_order(tx, who, c);
    /// Give a region a name. Nothing that places reads it, so nothing restarts.
    NameRegion(crate::NameRegion) -> RegionNamed(crate::Region)
        needs ServersOperate
        agent [region_rename]
        => crate::region::name(tx, who, c);
    /// Make an Environment its Project's Default Environment.
    SetDefaultEnvironment(crate::SetDefaultEnvironment)
        -> DefaultEnvironment(crate::EnvironmentsView)
        needs ProjectEdit
        agent none "No tool sets the default Environment."
        => crate::teardown::set_default(tx, who, c);
    /// Set the Setup Commands a Branch of an Environment runs once it is made.
    SetBranchSetup(crate::SetBranchSetup) -> BranchSetup(crate::EnvironmentsView)
        needs ProjectEdit
        agent none "No tool works on branches."
        => crate::branch::set_branch_setup(tx, who, c);
    /// Protect a Project's own Environment from removal, or unprotect it.
    SetEnvironmentProtected(crate::SetEnvironmentProtected) -> Protected(crate::ProtectedView)
        needs EnvironmentsProtect
        agent [environment_protect]
        => crate::teardown::set_protected(tx, who, c);
    /// Remove an Environment or a Project: off the Servers, then out of the Store.
    Remove(crate::Remove) -> Removal(crate::Removal)
        needs EnvironmentDelete
        agent [environment_remove, project_remove]
        => {
            if c.environment.is_none() {
                trusted.admit(Permission::ProjectDelete)?;
            }
            crate::removal::remove(tx, who, c, trusted)
        };
    /// Stop removing an Environment or a Project.
    CancelRemoval(crate::CancelRemoval) -> RemovalCancelled(crate::RemovalCancelled)
        needs ChangesDeploy
        agent none "Stopping a removal is cancelling its Deployment, which deployment_cancel does."
        => crate::removal::cancel_removal(tx, who, c);
    /// Change a Project's PR plan for one repository.
    SetPrPlan(crate::SetPrPlan) -> PrPlans(crate::PrPlansView)
        needs ProjectEdit
        agent [environment_pr]
        => crate::pull_request::set_plan(tx, who, c, trusted);
    /// Keep the automatic deploy rule a Device signed for an Environment.
    ApproveRule(crate::ApproveRule) -> Rule(crate::RuleView) as *
        needs ChangesDeploy
        agent none "A person's Device signs a rule that deploys every push of a branch."
        => crate::auto_rule::approve(tx, who, c);
    /// Deploy a pull request's head that needed a member's approval.
    ApprovePullRequest(crate::ApprovePullRequest) -> Automated(crate::Automated)
        needs ChangesDeploy
        agent none "A member approves a pull request's code to run with an Environment's secrets."
        => crate::pull_request::approve(tx, who, c, trusted);
    /// Deploy a member's pull request as a preview now.
    DeployPreview(crate::DeployPullRequestPreview) -> Automated(crate::Automated)
        needs ChangesDeploy
        agent none "No tool deploys previews yet."
        => crate::pull_request::deploy_preview(tx, who, c, trusted);
    /// Close one live preview, freeing its place under the cap.
    ClosePreview(crate::ClosePreview) -> Automated(crate::Automated)
        needs ChangesDeploy
        agent none "No tool closes previews yet."
        => crate::pull_request::close_preview(tx, who, c);
    /// Writes of one Environment applied together, all or none.
    Batch(Batch) -> Batch(Batched)
        needs ChangesStage
        agent [service_add, volume_add, config_write, config_rename, config_remove, discard]
        => {
            crate::scope::lock(tx, who, &c.environment)?.expect(c.expect)?;
            let mut at = Call { tx, who, trusted };
            c.commands
                .iter()
                .enumerate()
                .map(|(index, command)| {
                    command.apply(&c.environment, &mut at).map_err(|mut refused| {
                        // Its `next` reads what the commands before it wrote, and the
                        // refusal rolls them back.
                        if index > 0 {
                            FollowUp::remove_from(&mut refused.details);
                        }
                        refused
                    })
                })
                .collect::<Result<_, _>>()
                .map(|results| Batched { results })
        };
}

/// Writes of one Environment in one transaction, in order: all apply or none do.
/// Each create keeps its own caller-minted ID, so a retry replays it; the rest apply
/// again. Cloud gathers no trusted evidence
/// inside a Batch, so an edit that needs some (a repository or branch) is refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    /// The Environment every command writes. A command naming another is refused.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The commands, applied in order.
    pub commands: Vec<BatchCommand>,
    /// Refuse with `conflict` unless Working State is at this revision before the
    /// first command.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub expect: Option<crate::Revision>,
}

/// Register every command a [`Batch`] may hold: the enum, the Environment a command
/// names, and the [`Command`] it runs as once bound to the Batch's own.
macro_rules! batch_commands {
    ($($(#[doc = $doc:literal])* $variant:ident($payload:ty);)*) => {
        /// A command a [`Batch`] may hold.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "command", rename_all = "snake_case")]
        pub enum BatchCommand {
            $($(#[doc = $doc])* $variant($payload),)*
        }

        impl BatchCommand {
            fn named(&self) -> &EnvironmentRef {
                match self {
                    $(Self::$variant(payload) => payload.environment(),)*
                }
            }

            fn as_command(&self, at: &EnvironmentRef) -> Command {
                match self {
                    $(Self::$variant(payload) => Command::$variant(payload.rebound(at)),)*
                }
            }
        }
    };
}

batch_commands! {
    /// See [`Command::CreateService`].
    CreateService(CreateService);
    /// See [`Command::CreateVolume`].
    CreateVolume(CreateVolume);
    /// See [`Command::CreateConfig`].
    CreateConfig(CreateConfig);
    /// See [`Command::PutConfigFile`].
    PutConfigFile(PutConfigFile);
    /// See [`Command::RemoveConfigFile`].
    RemoveConfigFile(RemoveConfigFile);
    /// See [`Command::RenameConfig`].
    RenameConfig(RenameConfig);
    /// See [`Command::DeleteConfig`].
    DeleteConfig(DeleteConfig);
    /// See [`Command::AttachConfig`].
    AttachConfig(AttachConfig);
    /// See [`Command::DetachConfig`].
    DetachConfig(DetachConfig);
    /// See [`Command::Edit`].
    Edit(Edit);
    /// See [`Command::Discard`].
    Discard(Discard);
    /// See [`Command::NeverSync`].
    NeverSync(crate::NeverSync);
}

impl BatchCommand {
    fn apply(&self, environment: &EnvironmentRef, at: &mut Call<'_>) -> Result<Written, RpcError> {
        let named = self.named();
        if named != environment && *named != EnvironmentRef::default() {
            return Err(error::invalid(
                "Every command in a Batch writes the Batch's Environment",
                json!({ "environment": named }),
            ));
        }
        self.as_command(environment).apply(at)
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

/// What each command of a [`Batch`] did, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Batched {
    /// One result per command.
    pub results: Vec<Written>,
}

/// Register every answer: a [`Written`] variant and the answer it holds, which names
/// the Environments it touched through [`Answer`].
macro_rules! written {
    ($($(#[doc = $doc:literal])* $variant:ident($answer:ty),)*) => {
        /// What a command did.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "written", rename_all = "snake_case")]
        #[ts(rename = "ConfigWritten")]
        #[expect(
            clippy::large_enum_variant,
            reason = "one per command, serialized at once: boxing a Deployment saves nothing"
        )]
        pub enum Written {
            $($(#[doc = $doc])* $variant($answer),)*
        }

        impl Answer for Written {
            fn touched(&self) -> Vec<Touched<'_>> {
                match self {
                    $(Self::$variant(answer) => answer.touched(),)*
                }
            }
        }
    };
}

written! {
    /// A Project was created.
    Project(ProjectCreated),
    ProjectRenamed(ProjectSummary),
    /// An Environment was created.
    Environment(EnvironmentCreated),
    /// A Service was created.
    Service(ServiceStaged),
    ServiceRenamed(ServiceStaged),
    /// A Service was removed from Working State.
    ServiceRemoved(ServiceStaged),
    /// A Volume was created or its draft storage changed.
    Volume(VolumeStaged),
    /// A Volume was removed from Working State.
    VolumeRemoved(VolumeStaged),
    VolumeRenamed(VolumeStaged),
    /// A Config was created, a file put or removed, or a mount changed.
    Config(ConfigStaged),
    /// A Config was deleted from Working State.
    ConfigRemoved(ConfigStaged),
    ConfigRenamed(ConfigStaged),
    Edited(Edited),
    Published(Published),
    Discarded(Discarded),
    /// A Saved revision was restored or undone into the draft.
    HistoryStaged(HistoryStaged),
    /// A Deployment admitted, started or cancelled, or what its runner recorded.
    Deployment(crate::DeploymentSummary),
    /// A public domain was given to a Service, or it already had it.
    DomainAdded(crate::DomainAdded),
    /// A public domain was changed or removed.
    Domain(crate::DomainStaged),
    /// What a system event made the Store do.
    Automated(crate::Automated),
    /// An Environment's automatic deploy rule was kept.
    Rule(Box<crate::RuleView>),
    /// A Branch was made, given an Own Copy, or kept.
    Branch(crate::Branched),
    /// The Build Order was set; it applies to the next build.
    BuildOrder(crate::BuildOrderView),
    /// A region was given a name.
    RegionNamed(crate::Region),
    /// Changes synced into another Environment.
    Synced(crate::Synced),
    Undone(crate::Undone),
    /// A proposal was removed from its draft, or was gone already.
    Removed(crate::Removed),
    /// Hints were staged.
    Taken(crate::Taken),
    /// An Environment's settings marked Never sync changed.
    NeverSynced(crate::NeverSynced),
    /// The Default Environment changed: the Project's Environments after it.
    DefaultEnvironment(crate::EnvironmentsView),
    /// A Branch setup changed: the Project's Environments after it.
    BranchSetup(crate::EnvironmentsView),
    /// An Environment was protected or unprotected: which, and the Project's Environments after it.
    Protected(crate::ProtectedView),
    /// Where a removal stands.
    Removal(crate::Removal),
    RemovalCancelled(crate::RemovalCancelled),
    /// A PR plan changed: the Project's PR plans after it.
    PrPlans(crate::PrPlansView),
    /// What each command of a Batch did.
    Batch(Batched),
}

impl Written {
    /// The Environment whose config it wrote, if it wrote one that still exists.
    #[must_use]
    pub fn environment(&self) -> Option<&crate::EnvironmentId> {
        self.config()
    }
}

/// An Environment a write touched.
pub(crate) enum Touched<'a> {
    /// Its config: Working State, Saved State or a Deployment.
    Config(&'a crate::EnvironmentId),
    /// Whether it is protected, which is no config.
    Protection(&'a crate::EnvironmentId),
    /// A Branch a Sync closed, which may be gone already.
    Closed(&'a crate::EnvironmentName),
}

/// A command's answer, naming every Environment the command touched. A token
/// limited to some Environments writes only through an answer that names some, all
/// of them its own.
pub(crate) trait Answer {
    fn touched(&self) -> Vec<Touched<'_>>;

    /// The Environment whose config it wrote, if any.
    fn config(&self) -> Option<&crate::EnvironmentId> {
        self.touched()
            .into_iter()
            .find_map(|touched| match touched {
                Touched::Config(id) => Some(id),
                Touched::Protection(_) | Touched::Closed(_) => None,
            })
    }
}

/// Each answer and the Environments it touched. Under `none`: a new Project or
/// Branch, which has no pull request yet, and the writes outside any one
/// Environment's config.
macro_rules! touched {
    (none: $($none:ty),*; $($answer:ty => |$it:ident| [$($touched:expr),*];)*) => {
        $(impl Answer for $answer {
            fn touched(&self) -> Vec<Touched<'_>> {
                let $it = self;
                vec![$($touched),*]
            }
        })*
        $(impl Answer for $none {
            fn touched(&self) -> Vec<Touched<'_>> {
                Vec::new()
            }
        })*
    };
}

touched! {
    none:
        ProjectCreated,
        ProjectSummary,
        crate::Automated,
        crate::Branched,
        crate::BuildOrderView,
        crate::Region,
        crate::EnvironmentsView,
        crate::Removal,
        crate::RemovalCancelled,
        crate::PrPlansView;
    EnvironmentCreated => |it| [Touched::Config(&it.environment.id)];
    ServiceStaged => |it| [Touched::Config(&it.environment.id)];
    VolumeStaged => |it| [Touched::Config(&it.environment.id)];
    ConfigStaged => |it| [Touched::Config(&it.environment.id)];
    Edited => |it| [Touched::Config(&it.environment.id)];
    Published => |it| [Touched::Config(&it.environment.id)];
    Discarded => |it| [Touched::Config(&it.environment.id)];
    HistoryStaged => |it| [Touched::Config(&it.environment.id)];
    crate::DeploymentSummary => |it| [Touched::Config(&it.environment_id)];
    crate::RuleView => |it| [Touched::Config(&it.environment.id)];
    crate::DomainStaged => |it| [Touched::Config(&it.environment.id)];
    crate::DomainAdded => |it| [Touched::Config(&it.written.environment.id)];
    crate::Taken => |it| [Touched::Config(&it.into.id)];
    crate::Undone => |it| [Touched::Config(&it.into.id)];
    crate::Removed => |it| [Touched::Config(&it.environment.id)];
    crate::NeverSynced => |it| [Touched::Config(&it.environment.id)];
    crate::ProtectedView => |it| [Touched::Protection(&it.environment)];
}

impl Answer for crate::Synced {
    fn touched(&self) -> Vec<Touched<'_>> {
        let mut touched = vec![Touched::Config(&self.into.id)];
        if self.closing {
            touched.push(Touched::Closed(&self.from.name));
        }
        touched
    }
}

impl Answer for Batched {
    fn touched(&self) -> Vec<Touched<'_>> {
        self.results.iter().flat_map(Answer::touched).collect()
    }
}

/// Run `create` once for the caller-minted `ids`. Replaying the identical command
/// returns what the first run wrote; any of its IDs reused with another body, or
/// from another Organization, is `conflict`.
fn replayable<C: Tell + ?Sized>(
    at: &mut Call<'_>,
    command: &C,
    ids: &[&str],
    fits: impl FnOnce(&mut Call<'_>, &C::Written) -> Result<(), RpcError>,
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
            let first = row.json(2, "create result")?;
            fits(at, &first)?;
            return Ok(first);
        }
    }
    let written = create(at)?;
    let written_json = json_text(&written);
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
