//! Branches: an Environment made from a Parent in the same Project. It holds Own
//! Copies of the Parent nodes it picked (fresh ids, the same lineage) and uses the
//! rest live from the nearest Environment it comes from that runs them. Its base is
//! what it and its Parent last shared, so Update stages exactly the Parent's
//! deployed changes since. Core plans the picks and moves the rows
//! (`plan_branch`, `branch_changes`, `live_values`); this module stores and lands them.

mod create;
mod live;
mod moving;
mod setup;
pub(crate) use create::*;
pub(crate) use live::*;
pub(crate) use moving::*;
pub use setup::SetBranchSetup;
pub(crate) use setup::{branch_setup, set_branch_setup};

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    BranchChanges, BranchChangesInput, BranchChoice, BranchHostnames, BranchNewValue,
    BranchNodeReason, BranchNodeRole, BranchOption, BranchPick, BranchPickChoice, BranchPicks,
    BranchPlan, BranchPreset, BranchReason, BranchRole, BranchRow, COMMAND_MAX, ConfigError,
    EnvironmentNodeType, LiveLineageUse, LiveValuesInput, LiveValuesOwner, SavedEnvironmentIntent,
    SavedServiceIntent, SavedVariableProducer, SavedVariableValue, ServiceImageCredentials,
    ServiceSource, ValuePart, ValuePartOwner, branch_changes, canonicalize_environment_intent,
    compile_environment_intent, live_values, plan_branch,
};
use ployz_core::{Namespace, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::deployment::{self, DeploymentStatus};
use crate::error;
use crate::id::{
    ConditionalSaveId, DeploymentId, EnvironmentId, EnvironmentName, Revision, VolumeName,
};
use crate::policy::{self, Policy};
use crate::project::insert_environment;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::sealing::SealingKey;
use crate::settings::{NodeName, SettingPath, shown};
use crate::storage::Tx;
use crate::{Actor, registry, review};

/// Make a Branch of an Environment: Own Copies of the nodes picked, and of what
/// they use that its Parent doesn't run; everything else they use stays live.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateBranch {
    /// The new Branch's ID.
    pub id: EnvironmentId,
    /// Its Parent; the Branch joins the Parent's Project.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Its name, unique in the Project.
    pub name: EnvironmentName,
    /// The Parent's Services and Volumes to copy, by name. With `fix` and none
    /// named, the Services the failed Deployment didn't apply.
    #[serde(default)]
    pub copy: Vec<NodeName>,
    /// Nodes the Branch must use live, by name: refused unless the plan agrees.
    #[serde(default)]
    pub live: Vec<NodeName>,
    /// Commands to run in an Own Copy before it first deploys, such as seeding the
    /// copy of a database.
    #[serde(default)]
    pub setup: Vec<SetupCommand>,
    /// Keep it after a Save, and never close it for being idle.
    #[serde(default)]
    pub keep: bool,
    /// Fix this failed Deployment of the Parent on the Branch: each copied Service
    /// it didn't apply starts from that Deployment's configuration, staged over
    /// what the Parent runs.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub fix: Option<DeploymentId>,
}

/// A command to run in one of a Branch's own Services before it first deploys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetupCommand {
    /// The Service it runs in.
    pub service: ServiceName,
    /// A shell command.
    pub command: String,
}

/// Move changes between a Branch and its Parent, staging them in the other's
/// Working State; nothing is published or deployed. Or take a pull request's
/// value a Conditional Save left.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    /// The Branch's Working State into its Parent's; nothing there is deleted. From
    /// a PR Environment, a Conditional Save into one of its Destinations (the
    /// Environments that deploy its target branch): it stages nothing now and goes
    /// live with the pull request's merge.
    Save(Save),
    /// What the Branch's Parent deployed since the two last shared, into the
    /// Branch; refused unless the Branch runs its Working State.
    Update(Update),
    /// Stage the pull request's values a landed Conditional Save left as hints,
    /// sealed secrets included, even once its PR Environment is gone: each replaces
    /// the Destination's own edit.
    Take(Take),
}

/// A Save: see [`Move::Save`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Save {
    /// The Branch whose changes move.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Its Parent; from a PR Environment, the Destination. Omitted: the Parent, or
    /// the only Destination.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The changes to move; omitted, every change, each variable its default way.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub picks: Option<Vec<MovePick>>,
    /// Refuse with `conflict` unless the Move view is still at this version.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// `now` stages the changes; `at_merge` saves them as a Conditional Save that
    /// goes live with the pull request's merge, and `picks: []` withdraws it.
    /// Omitted: `at_merge` from a PR Environment, else `now`.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub when: Option<When>,
}

/// An Update: see [`Move::Update`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// The Branch the changes move into.
    #[serde(default)]
    pub into: EnvironmentRef,
    /// The changes to move; omitted, every change.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub picks: Option<Vec<MovePick>>,
    /// Refuse with `conflict` unless the Move view is still at this version.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
}

/// A take: see [`Move::Take`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Take {
    /// The retained Conditional Save whose hints to take.
    pub from: ConditionalSaveId,
    /// Its Destination; refused unless it is.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The hints to take, by row or a prefix of rows; omitted, every one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub rows: Option<Vec<String>>,
    /// Refuse with `conflict` unless the Destination's `diff` is still at this
    /// version: its Working State and the Saved revision the hints landed on.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
}

/// When a Move's changes land.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Staged in the receiver now.
    Now,
    /// With the pull request's merge.
    AtMerge,
}

/// Changes to move, by the name the Move view gives them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MovePick {
    /// A change (`web.source.image`), or a prefix of changes: `web` is every change
    /// of web, `web.variables` every variable of it.
    pub row: String,
    /// How the variables picked land; omitted, each its default.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub choice: Option<PickChoice>,
}

/// How a picked variable lands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PickChoice {
    /// The moving value, sealed secrets included.
    From,
    /// The Parent's deployed value.
    Parent,
    /// Not at all.
    LeaveOut,
    /// Its own value in the receiver: text that may reference Services there by
    /// name, or a secret's plaintext, sealed before it is stored.
    New(String),
}

impl PickChoice {
    const fn option(&self) -> BranchOption {
        match self {
            Self::From => BranchOption::From,
            Self::Parent => BranchOption::Parent,
            Self::LeaveOut => BranchOption::LeaveOut,
            Self::New(_) => BranchOption::New,
        }
    }
}

/// Read what a Save or Update would stage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "move", rename_all = "snake_case", deny_unknown_fields)]
pub enum MoveQuery {
    /// As [`Move::Save`].
    Save {
        #[serde(default)]
        from: EnvironmentRef,
        #[serde(default)]
        #[ts(optional = nullable)]
        into: Option<EnvironmentRef>,
        #[serde(default)]
        #[ts(optional = nullable)]
        when: Option<When>,
    },
    /// As [`Move::Update`].
    Update {
        #[serde(default)]
        into: EnvironmentRef,
    },
}

/// The changes a Move would stage, and the version that guards it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MoveView {
    /// Where the changes come from.
    pub from: EnvironmentSummary,
    /// Where they land.
    pub into: EnvironmentSummary,
    /// Pass to [`Move::version`] to move exactly these changes.
    pub version: String,
    /// Each change that moves.
    pub rows: Vec<MoveRow>,
    /// Each setting that differs and stays: sizing, domains and the Git branch
    /// belong to each Environment, so a Move never carries them.
    pub differ: Vec<DifferRow>,
}

/// A setting that differs between the two and stays as it is.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DifferRow {
    /// `NODE.path`.
    pub row: String,
    /// Why it stays.
    pub why: BranchReason,
    /// The value on the side changes come from.
    pub from: Value,
    /// The receiver's value.
    pub into: Value,
}

/// One change a Move carries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MoveRow {
    /// `NODE`, or `NODE.path` for one of its settings or variables.
    pub row: String,
    /// The receiver changed it too since the two last shared: moving it overwrites that.
    pub conflict: bool,
    /// How a variable can land.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub choice: Option<MoveChoice>,
    /// The value that moves; secrets read `{"secret": true}`.
    pub from: Value,
    /// The receiver's value now.
    pub into: Value,
}

/// The ways a moving variable can land.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MoveChoice {
    /// How it lands when not picked otherwise. `new` means a secret needs a fresh
    /// value: pick `leave_out` and set one, or `from` to move the Branch's own.
    pub default: BranchOption,
    /// Each way offered.
    pub options: Vec<BranchOption>,
    /// Whether it is a secret.
    pub secret: bool,
}

/// What a Move staged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Moved {
    /// Where the changes came from.
    pub from: EnvironmentSummary,
    /// Where they landed.
    pub into: EnvironmentSummary,
    /// Nodes staged in `into`'s Working State.
    pub staged: Vec<NodeName>,
    /// The Branch now; none for a take.
    pub branch: Option<BranchView>,
    /// The Conditional Save now: standing after a Save at merge, the one taken
    /// from after a take; none once withdrawn and for a Move now.
    pub conditional_save: Option<crate::ConditionalSave>,
}

/// Turn a Live Node into an Own Copy, from the Environment that runs it; a Volume
/// it mounts comes along, empty. Refused unless the Branch runs its Working State.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CopyNode {
    /// The Branch.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Live Node, by name.
    pub node: ServiceName,
    /// Refuse with `conflict` unless Working State is still at this revision.
    #[serde(default)]
    pub expect: Option<Revision>,
}

/// Keep a Branch after a Save and from closing when idle, or stop keeping it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeepBranch {
    /// The Branch.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Whether it is kept.
    pub kept: bool,
}

/// Plan a Branch before creating it: what each node of the Environment it comes
/// from becomes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BranchPlanQuery {
    /// The Environment to branch.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// The nodes the Branch is for, by name: what a preset plans around.
    #[serde(default)]
    pub focus: Vec<NodeName>,
    /// The nodes to copy, by name, as [`CreateBranch::copy`]; ignored with a preset.
    #[serde(default)]
    pub copy: Vec<NodeName>,
    /// Plan what a preset copies around `focus` instead.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub preset: Option<BranchPreset>,
}

/// A planned Branch: pass the nodes it owns to [`CreateBranch::copy`] and those
/// it uses live to [`CreateBranch::live`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BranchPlanView {
    /// The Environment it comes from.
    pub from: EnvironmentSummary,
    /// The preset this plan is, if any.
    pub preset: Option<BranchPreset>,
    /// The presets worth offering: "uses" only when it copies more than "only".
    pub presets: Vec<BranchPreset>,
    /// Each node, as the Branch would have it.
    pub nodes: Vec<PlannedNode>,
}

/// One node of a planned Branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PlannedNode {
    /// Its name where the Branch comes from.
    pub name: NodeName,
    pub kind: EnvironmentNodeType,
    /// `own`: the Branch gets its own copy; `live`: it uses the running one;
    /// `left_out`: it has none.
    pub role: PlannedRole,
    /// Why it is copied.
    pub because: Option<BranchNodeReason>,
    /// What it would become if the user toggled it.
    pub toggled: PlannedRole,
    /// Used live, the nearest Environment that runs it; none when nothing does.
    pub owner: Option<EnvironmentName>,
    /// It holds data: a Volume, or a Service mounting one.
    pub data: bool,
}

/// What a node becomes in a planned Branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PlannedRole {
    Own,
    Live,
    LeftOut,
}

/// Read a Branch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BranchQuery {
    /// The Branch.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// A Branch: its Parent, what it uses live and from where, and what Update would stage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BranchView {
    /// The Branch.
    pub environment: EnvironmentSummary,
    /// The Environment it was made from, in the same Project.
    pub parent: EnvironmentName,
    /// Whether it outlives a Save and never closes for being idle.
    pub kept: bool,
    /// What runs in each Own Copy before it first deploys.
    pub setup: Vec<SetupCommand>,
    /// The nodes it uses live.
    pub live: Vec<LiveNode>,
    /// The Parent's deployed changes Update would stage, as `NODE[.path]`.
    pub update: Vec<String>,
    /// The pull request it is the PR Environment of; its Save waits for the merge.
    pub pull_request: Option<crate::PullRequestRef>,
}

/// A node a Branch uses live.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LiveNode {
    /// Its name where it runs.
    pub name: String,
    /// The nearest Environment the Branch comes from that runs it; none when
    /// nothing does, so what reads it deploys empty.
    pub owner: Option<EnvironmentName>,
    /// It holds its owner's real data: a Volume, or a Service mounting one.
    pub data: bool,
    /// The Branch's own Services whose variables reference it, by name.
    pub used_by: Vec<ServiceName>,
}

/// A Branch after a change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Branched {
    /// The Branch now.
    pub branch: BranchView,
    /// Nodes staged in its Working State.
    pub staged: Vec<NodeName>,
}

/// A Branch's row.
pub(crate) struct Row {
    pub(crate) parent: EnvironmentId,
    pub(crate) kept: bool,
    pub(crate) base: SavedEnvironmentIntent,
    setup: Vec<Setup>,
}

/// A Setup Command, by the Service lineage it runs in, so renames keep it.
#[derive(Serialize, Deserialize)]
struct Setup {
    lineage: String,
    command: String,
}

/// An Environment a Branch comes from, with what it runs.
struct Ancestor {
    environment: Environment,
    applied: SavedEnvironmentIntent,
}

/// `parent` and every Environment it comes from, nearest first, each with what it runs.
fn chain(tx: &mut dyn Tx, parent: &EnvironmentId) -> Result<Vec<Ancestor>, RpcError> {
    let mut chain = Vec::new();
    for id in std::iter::once(parent.clone()).chain(ancestors(tx, parent)?) {
        let environment = scope::load_by_id(tx, &id)?;
        let applied = deployment::head(tx, &environment)?.applied;
        chain.push(Ancestor {
            environment,
            applied,
        });
    }
    Ok(chain)
}

/// The Environments `id` comes from, nearest first: none for a root.
pub(crate) fn ancestors(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let mut found: Vec<EnvironmentId> = Vec::new();
    let mut at = id.clone();
    while let Some(row) = row(tx, &at)? {
        if found.contains(&row.parent) || row.parent == *id {
            return Err(error::corrupt("Branch"));
        }
        found.push(row.parent.clone());
        at = row.parent;
    }
    Ok(found)
}

pub(crate) fn row(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<Row>, RpcError> {
    let rows = tx.query(
        "SELECT parent_id, kept, base, setup FROM config_environment_branch WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Row {
        parent: row.parse::<EnvironmentId>(0, "Branch")?,
        kept: row.int(1)? != 0,
        base: row.intent(2, "Branch")?,
        setup: row.json(3, "Branch")?,
    }))
}

/// `environment`'s Branch row, or why only a Branch does this.
fn branch_row(tx: &mut dyn Tx, environment: &Environment) -> Result<Row, RpcError> {
    row(tx, &environment.summary.id)?.ok_or_else(|| {
        error::invalid(
            format!("{} is not a Branch", environment.summary.name),
            json!({ "next": format!("ployz env branch NAME --from {} --project {}", environment.summary.name, environment.summary.project) }),
        )
    })
}

/// The managed-hostname suffix an Environment's generated domains carry: `-NAME`
/// for a Branch, none for a root.
pub(crate) fn suffix(tx: &mut dyn Tx, environment: &Environment) -> Result<String, RpcError> {
    Ok(match row(tx, &environment.summary.id)? {
        Some(_) => format!("-{}", environment.summary.name),
        None => String::new(),
    })
}

fn config(error: ConfigError) -> RpcError {
    error::invalid(
        format!("{}: {}", error.path, error.message),
        json!({ "path": error.path }),
    )
}

/// The lineage of the node named `name` in `intent`.
pub(crate) fn lineage_named(
    intent: &SavedEnvironmentIntent,
    name: &NodeName,
) -> Result<String, RpcError> {
    let found = match name {
        NodeName::Service(name) => intent
            .services
            .iter()
            .find(|service| service.slug == name.as_str())
            .map(|service| service.lineage_id.clone()),
        NodeName::Volume(name) => intent
            .volumes
            .iter()
            .find(|volume| volume.name == name.as_str())
            .map(|volume| volume.resource_lineage_id.clone()),
    };
    found.ok_or_else(|| {
        let names = names(intent);
        error::choices(
            format!("No node named {name}"),
            &name.to_string(),
            names.iter().map(String::as_str),
        )
    })
}

/// Every node of `intent` by name: Services as `SERVICE`, Volumes as `volumes.NAME`.
fn names(intent: &SavedEnvironmentIntent) -> Vec<String> {
    intent
        .services
        .iter()
        .map(|service| service.slug.clone())
        .chain(
            intent
                .volumes
                .iter()
                .map(|volume| format!("volumes.{}", volume.name)),
        )
        .collect()
}

/// The name of the node of lineage `lineage` in `intent`.
pub(crate) fn name_of(intent: &SavedEnvironmentIntent, lineage: &str) -> Option<String> {
    intent
        .services
        .iter()
        .find(|service| service.lineage_id == lineage)
        .map(|service| service.slug.clone())
        .or_else(|| {
            intent
                .volumes
                .iter()
                .find(|volume| volume.resource_lineage_id == lineage)
                .map(|volume| volume.name.clone())
        })
}

/// The node of lineage `lineage` in `intent`, by name.
pub(crate) fn node_of(intent: &SavedEnvironmentIntent, lineage: &str) -> Option<NodeName> {
    let service = intent
        .services
        .iter()
        .find(|service| service.lineage_id == lineage)
        .and_then(|service| ServiceName::parse(service.slug.as_str()).ok())
        .map(NodeName::Service);
    service.or_else(|| {
        intent
            .volumes
            .iter()
            .find(|volume| volume.resource_lineage_id == lineage)
            .and_then(|volume| VolumeName::parse(volume.name.as_str()).ok())
            .map(NodeName::Volume)
    })
}

/// Whether `lineage` in `intent` holds data: a Volume, or a Service mounting one.
fn holds_data(intent: &SavedEnvironmentIntent, lineage: &str) -> bool {
    intent
        .volumes
        .iter()
        .any(|volume| volume.resource_lineage_id == lineage)
        || intent
            .services
            .iter()
            .any(|service| service.lineage_id == lineage && !service.volume_attachments.is_empty())
}

fn holds(intent: &SavedEnvironmentIntent, lineage: &str) -> bool {
    name_of(intent, lineage).is_some()
}

fn lineages(intent: &SavedEnvironmentIntent) -> Vec<String> {
    intent
        .services
        .iter()
        .map(|service| service.lineage_id.clone())
        .chain(
            intent
                .volumes
                .iter()
                .map(|volume| volume.resource_lineage_id.clone()),
        )
        .collect()
}

fn document(intent: &SavedEnvironmentIntent) -> String {
    serde_json::to_string(intent).expect("Working State is JSON")
}
