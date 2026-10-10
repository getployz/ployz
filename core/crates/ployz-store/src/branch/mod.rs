//! Branches: an Environment made from a Parent in the same Project. It holds Own
//! Copies of the Parent nodes it picked (fresh ids, the same lineage) and uses the
//! rest live from the nearest Environment it comes from that runs them. Its base is
//! what it and its Parent last shared, so Follow stages exactly the Parent's
//! deployed changes since. Core plans the picks and compares the rows
//! (`plan_branch`, `plan`, `live_values`); this module stores and lands them.

mod create;
mod follow;
mod live;
mod never_sync;
mod pair;
mod proposal;
mod setup;
mod sync;
pub(crate) use create::*;
pub use follow::{FollowHint, IncomingChange};
pub(crate) use follow::{follow, hints, incoming};
pub(crate) use live::*;
pub use never_sync::{NeverSync, NeverSynced};
pub(crate) use never_sync::{marked, marks, never_sync};
pub(crate) use pair::*;
pub(crate) use proposal::remove as remove_proposal;
pub use proposal::{Included, ProposalSource, RemoveProposal, Removed};
pub(crate) use proposal::{Owners, consume, included, release_changed};
pub use setup::SetBranchSetup;
pub(crate) use setup::{branch_setup, set_branch_setup};
pub use sync::{
    Mark, NeverSyncedRow, SecretRow, SyncChange, SyncChanges, SyncQuery, SyncRow, SyncView, Synced,
    SyncedWhen, UndoSync, Undone,
};
pub(crate) use sync::{picks, seal_secret, sealed, sync, sync_view, take, undo};

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    Arrives, At, BranchNodeReason, BranchNodeRole, BranchPicks, BranchPlan, BranchPreset, Cell,
    Cells, ConfigError, EnvironmentNodeType, Hostnames, Landed, LiveLineageUse, LiveValuesInput,
    LiveValuesOwner, NodeRef, Plan, PlannedRow, Policy as Rules, RowId, SavedEnvironmentIntent,
    SavedServiceIntent, SavedVariableProducer, SealedCell, SealedSecret, ServiceImageCredentials,
    ServiceSource, Setting, Sides, Unapplied, ValuePart, ValuePartOwner, Verdict, Way, Why,
    canonicalize_environment_intent, compile_environment_intent, dependents, live_values, marks_on,
    parse_service_setting, plan, plan_branch, put_back, unapply,
};
use ployz_core::{Namespace, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::deployment::{self, DeploymentStatus};
use crate::error;
use crate::id::{
    ConditionalSyncId, DeploymentId, EnvironmentId, EnvironmentName, ProposalId, PullRequestNumber,
    Revision, SyncId, VolumeName,
};
use crate::policy::{self, Policy};
use crate::project::insert_environment;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::NodeName;
use crate::storage::{self, Tx};
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
    /// Keep it after it syncs into its Parent, and never close it for being idle.
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

/// Stage the hints left in an Environment: a merged pull request's values its
/// landed Conditional Sync left, even once its PR Environment is gone; or a
/// Parent's deployed values that followed into its Branch but aren't staged there.
/// Each replaces the receiver's own edit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Take {
    /// The retained Conditional Sync whose hints to take, or the Parent whose Follow
    /// hints to take.
    pub from: HintSource,
    /// Its Destination, or the Branch following the Parent; refused unless it is.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The hints to take; omitted, every one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub rows: Option<Vec<RowRef>>,
    /// Refused with `conflict` unless the receiver's `diff` is still at this
    /// version: the one the hints were read at.
    pub version: String,
}

/// Where the hints a [`Take`] takes come from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum HintSource {
    /// A merged pull request's Conditional Sync: [`crate::PullRequestHint::conditional_sync`].
    ConditionalSync(ConditionalSyncId),
    /// The Branch's Parent: [`FollowHint::from`].
    Parent(EnvironmentName),
}

/// When a Sync's changes land. Omitted: at the merge from a PR Environment into one
/// of its Destinations, else now.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum When {
    /// Staged in the receiver now.
    Now {
        /// Close the Branch once its changes landed in its Parent: refused for a
        /// kept Branch, and for a Sync into anything but its Parent.
        #[serde(default)]
        #[ts(as = "Option<bool>", optional)]
        close_after: bool,
    },
    /// With the pull request's merge: a Conditional Sync, replacing the one
    /// standing there.
    AtMerge,
}

/// A row, and its name where it is shown. Only [`pair::named`] makes one, so `kind`
/// is always `node`'s.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NamedRow {
    /// What commands name it by; stable across renames.
    pub(crate) row: RowId,
    /// Its Service, Volume or Config.
    pub(crate) node: NodeName,
    /// Whether `node` is a Service, a Volume or a Config.
    pub(crate) kind: EnvironmentNodeType,
    /// Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
    /// node itself.
    pub(crate) name: Option<String>,
}

impl NamedRow {
    /// What commands name it by.
    #[must_use]
    pub const fn row(&self) -> &RowId {
        &self.row
    }

    /// Its Service or Volume.
    #[must_use]
    pub const fn node(&self) -> &NodeName {
        &self.node
    }
}

/// `NODE`, or `NODE.name`: as reads show it.
impl std::fmt::Display for NamedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{}.{name}", self.node),
            None => write!(f, "{}", self.node),
        }
    }
}

/// A row as a command names it: its [`RowId`], its name as reads show it
/// (`web.image`, `web.env.KEY`), or a prefix of names (`web`, `web.env`) for every
/// row under it. The Store resolves it against the rows the command acts on.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(from = "String", into = "String")]
#[ts(as = "String")]
pub enum RowRef {
    /// A row by its RowId.
    Row(RowId),
    /// A row's name, or a prefix of names.
    Name(String),
}

impl From<String> for RowRef {
    fn from(text: String) -> Self {
        match text.parse() {
            Ok(row) => Self::Row(row),
            Err(_) => Self::Name(text),
        }
    }
}

impl From<&str> for RowRef {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl From<RowId> for RowRef {
    fn from(row: RowId) -> Self {
        Self::Row(row)
    }
}

impl From<RowRef> for String {
    fn from(asked: RowRef) -> Self {
        asked.to_string()
    }
}

impl std::fmt::Display for RowRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Row(row) => write!(f, "{row}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

/// The rows `asked` names among `rows`, each named as one Environment shows it. A
/// RowId names itself, there or not: the command decides whether it acts on it.
///
/// # Errors
/// Returns a `not_found` error with the names there are for a name that names
/// none, and an `ambiguous` one with the rows it names for a name (not a prefix)
/// that names more than one row: a different row in each Environment, say.
pub(crate) fn resolve(asked: &RowRef, rows: &[NamedRow]) -> Result<BTreeSet<RowId>, RpcError> {
    let name = match asked {
        RowRef::Row(row) => return Ok(BTreeSet::from([row.clone()])),
        RowRef::Name(name) => name,
    };
    let exact: BTreeSet<RowId> = rows
        .iter()
        .filter(|row| row.to_string() == *name)
        .map(|row| row.row.clone())
        .collect();
    if exact.len() > 1 {
        return Err(several(asked, &exact, rows));
    }
    if exact.iter().any(|row| matches!(row.at(), At::File(_))) {
        return Ok(exact);
    }
    let under = format!("{name}.");
    let found: BTreeSet<RowId> = rows
        .iter()
        .filter(|row| row.to_string().starts_with(&under))
        .map(|row| row.row.clone())
        .chain(exact)
        .collect();
    if found.is_empty() {
        let labels: BTreeSet<String> = rows.iter().map(NamedRow::to_string).collect();
        return Err(error::choices(
            format!("No row named {name} here"),
            name,
            labels.iter().map(String::as_str),
        ));
    }
    Ok(found)
}

/// The one row `asked` names among `rows`, as [`resolve`] finds it; a prefix naming
/// several is refused with their names.
pub(crate) fn resolve_one(asked: &RowRef, rows: &[NamedRow]) -> Result<RowId, RpcError> {
    let found = resolve(asked, rows)?;
    let mut only = found.iter();
    match (only.next(), only.next()) {
        (Some(row), None) => Ok(row.clone()),
        _ => Err(several(asked, &found, rows)),
    }
}

/// `asked` names each of `found`, more than one row: refused with their names, or
/// their RowIds where names repeat.
fn several(asked: &RowRef, found: &BTreeSet<RowId>, rows: &[NamedRow]) -> RpcError {
    let labels: BTreeSet<String> = rows
        .iter()
        .filter(|row| found.contains(&row.row))
        .map(NamedRow::to_string)
        .collect();
    let choices: Vec<String> = match labels.len() == found.len() {
        true => labels.into_iter().collect(),
        false => found.iter().map(ToString::to_string).collect(),
    };
    error::ambiguous(
        format!("{asked} names more than one row: name one"),
        json!({ "valid_children": choices }),
    )
}

/// The rows `asked` names among `named`, each one of `offered` (each a `what`, such
/// as a hint); omitted, every one.
pub(crate) fn chosen(
    asked: Option<&[RowRef]>,
    offered: BTreeSet<RowId>,
    named: &[NamedRow],
    what: &str,
) -> Result<BTreeSet<RowId>, RpcError> {
    let Some(asked) = asked else {
        return Ok(offered);
    };
    let chosen = resolve_all(asked, named)?;
    if let Some(unknown) = chosen.iter().find(|row| !offered.contains(row)) {
        let rows: Vec<String> = offered.iter().map(ToString::to_string).collect();
        return Err(error::choices(
            format!("No {what} at {unknown}"),
            &unknown.to_string(),
            rows.iter().map(String::as_str),
        ));
    }
    Ok(chosen)
}

/// `asked` resolved among `rows`, every one together.
pub(crate) fn resolve_all<'asked>(
    asked: impl IntoIterator<Item = &'asked RowRef>,
    rows: &[NamedRow],
) -> Result<BTreeSet<RowId>, RpcError> {
    let mut found = BTreeSet::new();
    for asked in asked {
        found.extend(resolve(asked, rows)?);
    }
    Ok(found)
}

/// What a take staged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Taken {
    /// Where the values came from: the Parent, or the pull request's PR Environment.
    pub from: EnvironmentSummary,
    /// Where they landed.
    pub into: EnvironmentSummary,
    /// Nodes staged in `into`'s Working State.
    pub staged: Vec<NodeName>,
    /// The Conditional Sync taken from; none for a Parent's values.
    pub conditional_sync: Option<crate::ConditionalSync>,
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

/// Keep a Branch after it syncs into its Parent and from closing when idle, or stop
/// keeping it.
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

/// A Branch: its Parent, what it uses live and from where, and what it would sync
/// into its Parent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BranchView {
    /// The Branch.
    pub environment: EnvironmentSummary,
    /// The Environment it was made from, in the same Project.
    pub parent: EnvironmentName,
    /// Whether it stays after syncing into its Parent and never closes for being idle.
    pub kept: bool,
    /// What runs in each Own Copy before it first deploys.
    pub setup: Vec<SetupCommand>,
    /// The nodes it uses live.
    pub live: Vec<LiveNode>,
    /// How many changes a Sync into its Parent carries: the Sync view's rows
    /// ticked by default.
    pub to_parent: usize,
    /// When it closes for sitting idle, in seconds since the Unix epoch: a week
    /// after its latest Deployment. None while kept, never deployed, a Parent or
    /// closing already.
    #[ts(type = "number | null")]
    pub closes_at: Option<i64>,
    /// The pull request it is the PR Environment of; its Sync into a Destination
    /// waits for the merge.
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
        "SELECT parent_id, kept, setup FROM config_environment_branch WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Row {
        parent: row.parse::<EnvironmentId>(0, "Branch")?,
        kept: row.int(1)? != 0,
        setup: row.json(2, "Branch")?,
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

pub(crate) fn config(error: ConfigError) -> RpcError {
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
        NodeName::Config(name) => intent
            .configs
            .iter()
            .find(|config| name.matches(config))
            .map(|config| config.resource_lineage_id.clone()),
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
        .chain(
            intent
                .configs
                .iter()
                .map(|config| format!("configs.{}", config.name)),
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
        .or_else(|| {
            intent
                .configs
                .iter()
                .find(|config| config.resource_lineage_id == lineage)
                .map(|config| config.name.to_string())
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
            .or_else(|| {
                intent
                    .configs
                    .iter()
                    .find(|config| config.resource_lineage_id == lineage)
                    .map(|config| NodeName::Config(config.name.clone().into()))
            })
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
