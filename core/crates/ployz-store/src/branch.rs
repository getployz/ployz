//! Branches: an Environment made from a Parent in the same Project. It holds Own
//! Copies of the Parent nodes it picked (fresh ids, the same lineage) and uses the
//! rest live from the nearest Environment it comes from that runs them. Its base is
//! what it and its Parent last shared, so Update stages exactly the Parent's
//! deployed changes since. Core plans the picks and moves the rows
//! (`plan_branch`, `branch_changes`, `live_values`); this module stores and lands them.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    BranchChanges, BranchChangesInput, BranchChoice, BranchHostnames, BranchNodeRole, BranchOption,
    BranchPick, BranchPickChoice, BranchPicks, BranchRole, BranchRow, ConfigError, LiveLineageUse,
    LiveValuesInput, LiveValuesOwner, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVariableProducer, ServiceImageCredentials, ServiceSource, ValuePart, ValuePartOwner,
    branch_changes, canonicalize_environment_intent, compile_environment_intent, live_values,
    parse_environment_intent, plan_branch,
};
use ployz_core::{Namespace, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::command::{Command, insert_environment, replayable};
use crate::deployment::{self, DeploymentStatus, NodeStatus};
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, EnvironmentName, Revision};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::ServiceSetting;
use crate::storage::Tx;
use crate::{Actor, registry, review};

/// Core lowers each Setup Command as one `/bin/sh -c` argument and refuses a longer one.
const SETUP_LIMIT: usize = 2000;

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
    pub copy: Vec<String>,
    /// Nodes the Branch must use live, by name: refused unless the plan agrees.
    #[serde(default)]
    pub live: Vec<String>,
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
/// Working State; nothing is published or deployed. Save moves the Branch's
/// Working State into its Parent's and deletes nothing there. Update moves what
/// the Parent deployed since the two last shared into the Branch, and is refused
/// unless the Branch runs its Working State.
///
/// From a PR Environment, a Save is a Conditional Save into one of its
/// Destinations (the Environments that deploy its target branch): it stages
/// nothing now and goes live with the pull request's merge. `take` instead moves a
/// pull request's value its Conditional Save left only as a hint in the Destination.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Move {
    /// Where the changes come from; omitted, the Parent of `into` (Update).
    #[serde(default)]
    #[ts(optional = nullable)]
    pub from: Option<EnvironmentRef>,
    /// Where they land; omitted, the Parent of `from` (Save).
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
    /// Take the picked rows (omitted: every hint) from retained Conditional Save
    /// `ID`, sealed secrets included, even once its PR Environment is gone: the
    /// pull request's value replaces the Destination's own edit, staged. `from`,
    /// `when` and `version` are then omitted.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub take: Option<String>,
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
    /// How the variables picked land: `from` (the moving value, sealed secrets
    /// included), `parent` (the Parent's deployed value) or `leave_out`; omitted,
    /// each its default.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub choice: Option<BranchOption>,
}

/// Read what a [`Move`] between a Branch and its Parent would stage.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MoveQuery {
    /// As [`Move::from`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub from: Option<EnvironmentRef>,
    /// As [`Move::into`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// As [`Move::when`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub when: Option<When>,
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
    /// Nodes staged in `into`'s Working State: Services by name, Volumes as `volumes.NAME`.
    pub staged: Vec<String>,
    /// The Branch now; none for a take.
    pub branch: Option<BranchView>,
    /// The Conditional Save now: standing after a Save at merge, the one taken
    /// from after a take; none once withdrawn and for a Move now.
    pub conditional_save: Option<crate::ConditionalSave>,
    /// Pull requests whose GitHub check Cloud publishes again.
    pub checks: Vec<crate::PullRequestRef>,
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
}

/// A node a Branch uses live.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LiveNode {
    /// Its name where it runs.
    pub name: String,
    /// The nearest Environment the Branch comes from that runs it; none when
    /// nothing does, so what reads it deploys empty.
    pub owner: Option<EnvironmentName>,
}

/// A Branch after a change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Branched {
    /// The Branch now.
    pub branch: BranchView,
    /// Nodes staged in its Working State: Services by name, Volumes as `volumes.NAME`.
    pub staged: Vec<String>,
    /// What changed at once.
    pub immediate: Vec<String>,
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

pub(crate) fn create_branch(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateBranch,
) -> Result<Branched, RpcError> {
    replayable(tx, who, &Command::CreateBranch(create.clone()), |tx| {
        insert_branch(tx, who, create)
    })
}

fn insert_branch(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateBranch,
) -> Result<Branched, RpcError> {
    let parent = scope::lock(tx, who, &create.from)?;
    if let Some(removal) = crate::teardown::removing(tx, &parent.summary.id)? {
        return Err(crate::teardown::being_removed(&parent, &removal));
    }
    let project = scope::project(tx, who, Some(&parent.summary.project))?;
    let taken = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND name = ?2",
        &[project.id.as_str().into(), create.name.as_str().into()],
    )?;
    if !taken.is_empty() {
        return Err(error::conflict(
            format!("{} is taken in Project {}", create.name, project.name),
            json!({ "environment": create.name }),
        ));
    }
    let applied = deployment::head(tx, &parent)?.applied;
    let deployed = lineages(&applied);
    let failed = match &create.fix {
        Some(id) => failed_services(tx, who, &parent, &applied, id)?,
        None => Vec::new(),
    };
    let working = &parent.working;
    let mut copy = create
        .copy
        .iter()
        .map(|name| lineage_named(working, name))
        .collect::<Result<Vec<_>, _>>()?;
    if copy.is_empty() {
        copy = failed
            .iter()
            .map(|service| service.lineage_id.clone())
            .filter(|lineage| holds(working, lineage))
            .collect();
    }
    let picks = BranchPicks::Own { own: copy.clone() };
    let plan = plan_branch(working, &deployed, &copy, &picks).map_err(config)?;
    let (mut own, mut live) = (BTreeSet::new(), Vec::new());
    for node in &plan.nodes {
        match node.role {
            BranchNodeRole::Own { .. } => {
                own.insert(node.lineage_id.clone());
            }
            BranchNodeRole::Live => live.push(node.lineage_id.clone()),
            BranchNodeRole::LeftOut => {}
        }
    }
    if own.is_empty() {
        return Err(error::invalid(
            "Pick something to copy",
            json!({ "valid_children": names(working) }),
        ));
    }
    for name in &create.live {
        let lineage = lineage_named(working, name)?;
        if !live.contains(&lineage) {
            let why = if own.contains(&lineage) {
                "the Branch copies it: nothing running can lend it"
            } else {
                "nothing the Branch copies uses it"
            };
            return Err(error::invalid(
                format!("{name} can't be used live: {why}"),
                json!({ "node": name }),
            ));
        }
    }
    let setup = create
        .setup
        .iter()
        .map(|setup| {
            let service = working
                .services
                .iter()
                .find(|service| service.slug == setup.service.as_str())
                .filter(|service| own.contains(&service.lineage_id))
                .ok_or_else(|| {
                    error::invalid(
                        format!(
                            "{}: a Setup Command runs in one of the Branch's own Services",
                            setup.service
                        ),
                        json!({ "service": setup.service }),
                    )
                })?;
            Ok(Setup {
                lineage: service.lineage_id.clone(),
                command: setup_command(setup)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // A fix: each failed Service it copies stands in for the Parent's; the Parent stays as it is.
    let fixed: Vec<SavedServiceIntent> = failed
        .into_iter()
        .filter(|service| own.contains(&service.lineage_id) && holds(working, &service.lineage_id))
        .collect();
    if create.fix.is_some() && fixed.is_empty() {
        return Err(error::invalid(
            "Copy a Service this Deployment failed to apply",
            json!({ "fix": create.fix }),
        ));
    }
    let mut from = working.clone();
    for failed in fixed {
        for service in &mut from.services {
            if service.lineage_id == failed.lineage_id {
                *service = failed.clone();
            }
        }
    }

    let into = empty(create.name.as_str());
    let hostnames = BranchHostnames {
        from: suffix(tx, &parent)?,
        into: format!("-{}", create.name),
    };
    let node_picks = own
        .iter()
        .map(|lineage| BranchPick {
            key: format!("{lineage}:node"),
            choice: None,
        })
        .collect();
    let creating = |from, picks| {
        compare(Comparing {
            base: None,
            from,
            into: &into,
            parent: None,
            provided: &live,
            hostnames: &hostnames,
            from_kept: false,
            picks: Some(picks),
        })
    };
    let changes = creating(&from, node_picks)?;
    // A fix's base is what the Parent runs, so the failed change shows as staged.
    let base = match create.fix {
        Some(_) => creating(&applied, Vec::new())?.base,
        None => changes.base,
    }
    .ok_or_else(|| error::internal("Core returned no base for a new Branch"))?;

    let summary = insert_environment(tx, who, &project, &create.id, create.name.clone())?;
    let mut branch = Environment {
        summary,
        working: into,
        live: BTreeMap::new(),
    };
    let staged = land(
        tx,
        who,
        &mut branch,
        &parent.summary.id,
        &from,
        changes.next,
        &[],
    )?;
    tx.execute(
        "INSERT INTO config_environment_branch (environment_id, organization_id, parent_id, kept, base, setup) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            create.id.as_str().into(),
            who.organization.as_str().into(),
            parent.summary.id.as_str().into(),
            i64::from(create.keep).into(),
            document(&base).as_str().into(),
            serde_json::to_string(&setup)
                .expect("Setup Commands are JSON")
                .as_str()
                .into(),
        ],
    )?;
    branch.live = live_names(tx, &create.id, &branch.working)?;
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged,
        immediate: Vec::new(),
    })
}

pub(crate) fn move_changes(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &Move,
) -> Result<Moved, RpcError> {
    if request.take.is_some() {
        return crate::conditional_save::take(tx, who, request);
    }
    if crate::conditional_save::at_merge(tx, who, request.from.as_ref(), request.when)? {
        return crate::conditional_save::save(tx, who, request);
    }
    let mut sides = sides(tx, who, request.from.as_ref(), request.into.as_ref(), true)?;
    if let Some(removal) = crate::teardown::removing(tx, &sides.branch().summary.id)? {
        return Err(crate::teardown::being_removed(sides.branch(), &removal));
    }
    if sides.update {
        settled(tx, &sides.into)?;
    }
    let moving = moving(tx, &sides)?;
    let changes = moving.compare(&sides.into.working, None)?;
    let version = version(&sides.into, &changes.review);
    if request
        .version
        .as_ref()
        .is_some_and(|asked| *asked != version)
    {
        return Err(error::conflict(
            "Changed since you reviewed: review the move again",
            json!({ "version": version }),
        ));
    }
    let picks = picks(
        &moving,
        &sides.into.working,
        &changes.rows,
        request.picks.as_deref(),
    )?;
    let staged = moving.apply(tx, who, &mut sides.into, picks)?;
    Ok(Moved {
        branch: Some(view(tx, sides.branch())?),
        checks: crate::pull_request::project_checks(tx, &sides.into.summary.id)?,
        from: sides.from.summary,
        into: sides.into.summary,
        staged,
        conditional_save: None,
    })
}

pub(crate) fn move_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &MoveQuery,
) -> Result<MoveView, RpcError> {
    if crate::conditional_save::at_merge(tx, who, query.from.as_ref(), query.when)? {
        return crate::conditional_save::view(tx, who, query);
    }
    let sides = sides(tx, who, query.from.as_ref(), query.into.as_ref(), false)?;
    let moving = moving(tx, &sides)?;
    let changes = moving.compare(&sides.into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter_map(|row| move_row(&moving, &sides.from, &sides.into, row))
        .collect();
    Ok(MoveView {
        version: version(&sides.into, &changes.review),
        from: sides.from.summary,
        into: sides.into.summary,
        rows,
    })
}

/// A moving row as the Move view shows it, from `from` into `into`; none for a row
/// that never moves.
pub(crate) fn move_row(
    moving: &Moving,
    from: &Environment,
    into: &Environment,
    row: &BranchRow,
) -> Option<MoveRow> {
    let BranchRole::Move { conflict, choice } = &row.role else {
        return None;
    };
    let key = row.key.to_string();
    let (lineage, path) = split(&key);
    let variable = |intent: &SavedEnvironmentIntent, names| {
        let key = path.strip_prefix("variables.")?;
        let service = intent.services.iter().find(|s| s.lineage_id == lineage)?;
        let found = service.variables.iter().find(|v| v.key == key)?;
        Some(crate::variables::shown(found, names))
    };
    let (from_names, into_names) = (from.names(), into.names());
    let from_value = variable(&moving.from, &from_names).unwrap_or_else(|| shown(path, &row.from));
    let into_value = variable(&into.working, &into_names).unwrap_or_else(|| shown(path, &row.into));
    Some(MoveRow {
        row: moving.name(&into.working, &key),
        conflict: *conflict,
        choice: choice.as_ref().map(|choice| MoveChoice {
            default: moving.default(choice),
            options: choice.options.clone(),
            secret: choice.secret,
        }),
        from: from_value,
        into: into_value,
    })
}

/// A Branch and its Parent, as one Move addresses them.
struct Sides {
    from: Environment,
    into: Environment,
    /// The Parent's changes move into the Branch (Update), rather than the
    /// Branch's into the Parent (Save).
    update: bool,
    row: Row,
}

impl Sides {
    fn branch(&self) -> &Environment {
        if self.update { &self.into } else { &self.from }
    }
}

/// Resolve a Move's sides; `lock` locks both, in ID order.
fn sides(
    tx: &mut dyn Tx,
    who: &Actor,
    from: Option<&EnvironmentRef>,
    into: Option<&EnvironmentRef>,
    lock: bool,
) -> Result<Sides, RpcError> {
    let mut id = |at: Option<&EnvironmentRef>| -> Result<Option<Environment>, RpcError> {
        at.map(|at| scope::environment(tx, who, at)).transpose()
    };
    let (from, into) = (id(from)?, id(into)?);
    let (branch, parent_named, update) = match (from, into) {
        (Some(from), None) => (from, None, false),
        (None, Some(into)) => (into, None, true),
        (Some(from), Some(into)) => match row(tx, &into.summary.id)? {
            Some(row) if row.parent == from.summary.id => (into, Some(from), true),
            _ => (from, Some(into), false),
        },
        (None, None) => {
            return Err(error::invalid(
                "Name the Branch changes move from or into",
                json!({}),
            ));
        }
    };
    let parent = branch_row(tx, &branch)?.parent;
    if let Some(named) = parent_named
        && named.summary.id != parent
    {
        return Err(error::invalid(
            format!(
                "Changes move only between a Branch and its Parent: {} is not the Parent of {}",
                named.summary.name, branch.summary.name
            ),
            json!({}),
        ));
    }
    let mut ids = [branch.summary.id.clone(), parent];
    ids.sort();
    let mut loaded = Vec::new();
    for id in &ids {
        loaded.push(match lock {
            true => scope::lock_id(tx, who, id)?,
            false => scope::load_by_id(tx, id)?,
        });
    }
    let first = loaded.remove(0);
    let second = loaded.remove(0);
    let (branch, parent) = match first.summary.id == branch.summary.id {
        true => (first, second),
        false => (second, first),
    };
    // Read again under the locks: the base may have moved meanwhile.
    let row = branch_row(tx, &branch)?;
    Ok(match update {
        true => Sides {
            from: parent,
            into: branch,
            update,
            row,
        },
        false => Sides {
            from: branch,
            into: parent,
            update,
            row,
        },
    })
}

/// A Move's comparison: Update from the Parent's Applied State, Save from the
/// Branch's Working State with the Parent's deployed values on offer.
fn moving(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
    let parent = if sides.update {
        &sides.from
    } else {
        &sides.into
    };
    let applied = deployment::head(tx, parent)?.applied;
    let branch = sides.branch().summary.id.clone();
    let hostnames = BranchHostnames {
        from: suffix(tx, &sides.from)?,
        into: suffix(tx, &sides.into)?,
    };
    let deployed = !(applied.services.is_empty() && applied.volumes.is_empty());
    Ok(match sides.update {
        true => Moving {
            source: sides.from.summary.id.clone(),
            branch,
            nothing: format!("Nothing new in {}", sides.from.summary.name),
            from: applied,
            base: sides.row.base.clone(),
            parent: None,
            provided: used_live(&sides.into.working).into_keys().collect(),
            from_kept: false,
            hostnames,
            update: true,
        },
        false => Moving {
            source: sides.from.summary.id.clone(),
            branch,
            nothing: format!("Nothing to save into {}", sides.into.summary.name),
            from: sides.from.working.clone(),
            base: sides.row.base.clone(),
            parent: deployed.then_some(applied),
            provided: used_live(&sides.into.working).into_keys().collect(),
            from_kept: sides.row.kept,
            hostnames,
            update: false,
        },
    })
}

/// A Move's guard: the receiver's revision and core's review of the changes.
pub(crate) fn version(into: &Environment, review: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, review.as_bytes());
    format!(
        "{}:{}",
        into.summary.revision,
        hex::encode(digest.as_ref().get(..8).unwrap_or_default())
    )
}

/// Core's picks for `asked`: every change named or under a name, each variable
/// the way asked or its default. A secret wanting a fresh value is refused: the
/// Store never makes one up, and a Branch's own secret moves only when asked.
pub(crate) fn picks(
    moving: &Moving,
    into: &SavedEnvironmentIntent,
    rows: &[BranchRow],
    asked: Option<&[MovePick]>,
) -> Result<Vec<BranchPick>, RpcError> {
    let named: Vec<(String, &BranchRow, Option<&BranchChoice>)> = rows
        .iter()
        .filter_map(|row| match &row.role {
            BranchRole::Move { choice, .. } => Some((
                moving.name(into, &row.key.to_string()),
                row,
                choice.as_ref(),
            )),
            BranchRole::Differ { .. } => None,
        })
        .collect();
    let mut chosen: BTreeMap<String, Option<BranchOption>> = BTreeMap::new();
    match asked {
        None => {
            for (_, row, _) in &named {
                chosen.insert(row.key.to_string(), None);
            }
        }
        Some(asked) => {
            for pick in asked {
                let under = |name: &str| {
                    name == pick.row
                        || name
                            .strip_prefix(pick.row.as_str())
                            .is_some_and(|rest| rest.starts_with('.'))
                };
                let found: Vec<_> = named.iter().filter(|(name, ..)| under(name)).collect();
                if found.is_empty() {
                    let names = named.iter().map(|(name, ..)| name.as_str());
                    return Err(error::not_found(
                        format!("No change named {} moves", pick.row),
                        json!({
                            "did_you_mean": error::did_you_mean(&pick.row, names.clone()),
                            "valid_children": names.collect::<Vec<_>>(),
                        }),
                    ));
                }
                if pick.choice.is_some() && found.iter().all(|(.., choice)| choice.is_none()) {
                    return Err(error::invalid(
                        format!("{}: only a variable takes a choice", pick.row),
                        json!({ "row": pick.row }),
                    ));
                }
                for (_, row, _) in found {
                    chosen.insert(row.key.to_string(), pick.choice);
                }
            }
        }
    }
    let mut fresh = Vec::new();
    let mut picks = Vec::new();
    for (name, row, offered) in &named {
        let key = row.key.to_string();
        let Some(asked) = chosen.get(&key) else {
            continue;
        };
        let choice = match offered {
            None => None,
            Some(offered) => {
                let option = asked.unwrap_or_else(|| moving.default(offered));
                if !offered.options.contains(&option) {
                    return Err(error::invalid(
                        format!("{name}: a variable lands as one of {:?}", offered.options),
                        json!({ "row": name }),
                    ));
                }
                Some(match option {
                    BranchOption::From => BranchPickChoice::From,
                    BranchOption::Parent => BranchPickChoice::Parent,
                    BranchOption::LeaveOut => BranchPickChoice::LeaveOut,
                    BranchOption::New => {
                        fresh.push(name.clone());
                        continue;
                    }
                })
            }
        };
        picks.push(BranchPick { key, choice });
    }
    if !fresh.is_empty() {
        return Err(error::invalid(
            format!(
                "{}: a secret set in the Branch moves only when picked `from`; or pick `leave_out` and set a new one after",
                fresh.join(", ")
            ),
            json!({ "rows": fresh }),
        ));
    }
    if picks.is_empty() {
        return Err(error::conflict(moving.nothing.clone(), json!({})));
    }
    Ok(picks)
}

pub(crate) fn copy_node(
    tx: &mut dyn Tx,
    who: &Actor,
    copy: &CopyNode,
) -> Result<Branched, RpcError> {
    let mut branch = scope::lock(tx, who, &copy.environment)?;
    branch.expect(copy.expect)?;
    let row = branch_row(tx, &branch)?;
    if branch.service(&copy.node).is_ok() {
        return Err(error::conflict(
            format!("This Branch already has its own copy of {}", copy.node),
            json!({ "node": copy.node }),
        ));
    }
    let uses = used_live(&branch.working);
    let lineage = uses
        .keys()
        .find(|lineage| branch.live.get(*lineage).map(String::as_str) == Some(copy.node.as_str()))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<&str> = branch.live.values().map(String::as_str).collect();
            error::not_found(
                format!("This Branch uses no node named {} live", copy.node),
                json!({
                    "did_you_mean": error::did_you_mean(copy.node.as_str(), names.iter().copied()),
                    "valid_children": names,
                }),
            )
        })?;
    settled(tx, &branch)?;
    let owner = chain(tx, &row.parent)?
        .into_iter()
        .find(|ancestor| holds(&ancestor.applied, &lineage))
        .ok_or_else(|| {
            error::conflict(
                format!("Nothing runs {}, so there is nothing to copy", copy.node),
                json!({ "node": copy.node }),
            )
        })?;
    // An Own Copy is an introduction: neither it nor the Volumes it mounts that the
    // Branch lacks are in the base or provided live.
    let mut copied = BTreeSet::from([lineage]);
    if let Some(service) = owner
        .applied
        .services
        .iter()
        .find(|service| copied.contains(&service.lineage_id))
    {
        for mount in &service.volume_attachments {
            let volume = owner
                .applied
                .volumes
                .iter()
                .find(|volume| volume.resource_id == mount.volume_resource_id);
            if let Some(volume) = volume
                && !holds(&branch.working, &volume.resource_lineage_id)
            {
                copied.insert(volume.resource_lineage_id.clone());
            }
        }
    }
    let mut base = row.base;
    base.services
        .retain(|service| !copied.contains(&service.lineage_id));
    base.volumes
        .retain(|volume| !copied.contains(&volume.resource_lineage_id));
    let provided = uses
        .into_keys()
        .filter(|lineage| !copied.contains(lineage))
        .collect();
    let moving = Moving {
        source: owner.environment.summary.id.clone(),
        branch: branch.summary.id.clone(),
        nothing: format!("Nothing to copy from {}", owner.environment.summary.name),
        from: owner.applied,
        base,
        parent: None,
        provided,
        from_kept: false,
        hostnames: BranchHostnames {
            from: suffix(tx, &owner.environment)?,
            into: suffix(tx, &branch)?,
        },
        update: true,
    };
    // Every change of the copy, variables with the owner's values.
    let picks: Vec<BranchPick> = moving
        .compare(&branch.working, None)?
        .rows
        .iter()
        .filter_map(|row| {
            let BranchRole::Move { choice, .. } = &row.role else {
                return None;
            };
            let key = row.key.to_string();
            copied.contains(split(&key).0).then(|| BranchPick {
                choice: choice.as_ref().map(|_| BranchPickChoice::From),
                key,
            })
        })
        .collect();
    if picks.is_empty() {
        return Err(error::conflict(moving.nothing, json!({})));
    }
    let staged = moving.apply(tx, who, &mut branch, picks)?;
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged,
        immediate: Vec::new(),
    })
}

pub(crate) fn keep_branch(
    tx: &mut dyn Tx,
    who: &Actor,
    keep: &KeepBranch,
) -> Result<Branched, RpcError> {
    let branch = scope::lock(tx, who, &keep.environment)?;
    let row = branch_row(tx, &branch)?;
    let mut immediate = Vec::new();
    if row.kept != keep.kept {
        tx.execute(
            "UPDATE config_environment_branch SET kept = ?1 WHERE environment_id = ?2",
            &[
                i64::from(keep.kept).into(),
                branch.summary.id.as_str().into(),
            ],
        )?;
        immediate.push("kept".to_owned());
    }
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged: Vec::new(),
        immediate,
    })
}

pub(crate) fn branch(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &BranchQuery,
) -> Result<BranchView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    view(tx, &environment)
}

/// What deploying a Branch adds to lowering: the values of the Live Nodes it
/// reads, from where they run, and the Setup Commands of Own Copies never deployed.
#[derive(Default)]
pub(crate) struct Lowering {
    /// Producers of the Live Nodes' values, rescoped to their owners' Namespaces.
    pub(crate) live: Vec<SavedVariableProducer>,
    /// Setup Commands by Service ID.
    pub(crate) setup: BTreeMap<String, Vec<String>>,
}

/// [`Lowering`] for a Deployment of `saved` in Environment `id`, as things stand now.
pub(crate) fn lowering(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
) -> Result<Lowering, RpcError> {
    let Some(row) = row(tx, id)? else {
        return Ok(Lowering::default());
    };
    let deployed = tx
        .query(
            "SELECT node_id FROM config_applied WHERE environment_id = ?1",
            &[id.as_str().into()],
        )?
        .iter()
        .map(|row| row.text(0).map(str::to_owned))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let setup = saved
        .services
        .iter()
        .filter(|service| !deployed.contains(&service.id))
        .filter_map(|service| {
            let commands: Vec<String> = row
                .setup
                .iter()
                .filter(|setup| setup.lineage == service.lineage_id)
                .map(|setup| setup.command.clone())
                .collect();
            (!commands.is_empty()).then(|| (service.id.clone(), commands))
        })
        .collect();
    Ok(Lowering {
        live: live_producers(tx, &row.parent, &used_live(saved))?,
        setup,
    })
}

/// The name of each node `working` uses live, by lineage, from the nearest
/// Environment it comes from whose Working State names it. Empty for a root.
pub(crate) fn live_names(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    working: &SavedEnvironmentIntent,
) -> Result<BTreeMap<String, String>, RpcError> {
    let mut wanted: BTreeSet<String> = used_live(working).into_keys().collect();
    let mut names = BTreeMap::new();
    for ancestor in ancestors(tx, id)? {
        if wanted.is_empty() {
            break;
        }
        let rows = tx.query(
            "SELECT working FROM config_environment WHERE id = ?1",
            &[ancestor.as_str().into()],
        )?;
        let intent = parse(
            rows.first()
                .ok_or_else(|| error::corrupt("Environment"))?
                .text(0)?,
        )?;
        wanted.retain(|lineage| match name_of(&intent, lineage) {
            Some(name) => {
                names.insert(lineage.clone(), name);
                false
            }
            None => true,
        });
    }
    Ok(names)
}

/// A Branch's view: its Parent, its Live Nodes and their owners, what Update would stage.
pub(crate) fn view(tx: &mut dyn Tx, branch: &Environment) -> Result<BranchView, RpcError> {
    let row = branch_row(tx, branch)?;
    let chain = chain(tx, &row.parent)?;
    let parent = chain.first().ok_or_else(|| error::corrupt("Branch"))?;
    let uses = used_live(&branch.working);
    let live = uses
        .keys()
        .map(|lineage| LiveNode {
            name: branch
                .live
                .get(lineage)
                .cloned()
                .unwrap_or_else(|| lineage.clone()),
            owner: chain
                .iter()
                .find(|ancestor| holds(&ancestor.applied, lineage))
                .map(|ancestor| ancestor.environment.summary.name.clone()),
        })
        .collect();
    let update = if parent.applied.services.is_empty() && parent.applied.volumes.is_empty() {
        Vec::new()
    } else {
        let moving = Moving::update(
            tx,
            &parent.environment,
            parent.applied.clone(),
            branch,
            row.base.clone(),
        )?;
        moving
            .compare(&branch.working, None)?
            .rows
            .iter()
            .filter(|row| matches!(row.role, BranchRole::Move { .. }))
            .map(|row| moving.name(&branch.working, &row.key.to_string()))
            .collect()
    };
    let setup = row
        .setup
        .iter()
        .filter_map(|setup| {
            let service = branch
                .working
                .services
                .iter()
                .find(|service| service.lineage_id == setup.lineage)?;
            Some(SetupCommand {
                service: ServiceName::parse(service.slug.as_str()).ok()?,
                command: setup.command.clone(),
            })
        })
        .collect();
    Ok(BranchView {
        environment: branch.summary.clone(),
        parent: parent.environment.summary.name.clone(),
        kept: row.kept,
        setup,
        live,
        update,
    })
}

/// One move between a Branch and an Environment it comes from: `from`'s changes
/// over `base`, into the other side. A Conditional Save keeps it to land later.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Moving {
    /// The Environment `from` belongs to: where arriving nodes' credentials are.
    pub(crate) source: EnvironmentId,
    /// The Branch whose base advances by what lands.
    pub(crate) branch: EnvironmentId,
    /// Why nothing moves.
    pub(crate) nothing: String,
    pub(crate) from: SavedEnvironmentIntent,
    pub(crate) base: SavedEnvironmentIntent,
    /// The Parent's deployed values, offered for a variable.
    pub(crate) parent: Option<SavedEnvironmentIntent>,
    /// Lineages the receiver uses live.
    pub(crate) provided: Vec<String>,
    /// Whether `from` is a kept Branch: its secrets then stay by default.
    pub(crate) from_kept: bool,
    pub(crate) hostnames: BranchHostnames,
    /// Into a Branch, where every variable lands with `from`'s value by default.
    pub(crate) update: bool,
}

impl Moving {
    /// What `parent` deployed, into `branch`.
    fn update(
        tx: &mut dyn Tx,
        parent: &Environment,
        applied: SavedEnvironmentIntent,
        branch: &Environment,
        base: SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        Ok(Self {
            source: parent.summary.id.clone(),
            branch: branch.summary.id.clone(),
            nothing: format!("Nothing new in {}", parent.summary.name),
            from: applied,
            base,
            parent: None,
            provided: used_live(&branch.working).into_keys().collect(),
            from_kept: false,
            hostnames: BranchHostnames {
                from: suffix(tx, parent)?,
                into: suffix(tx, branch)?,
            },
            update: true,
        })
    }

    pub(crate) fn compare(
        &self,
        into: &SavedEnvironmentIntent,
        picks: Option<Vec<BranchPick>>,
    ) -> Result<BranchChanges, RpcError> {
        compare(Comparing {
            base: Some(&self.base),
            from: &self.from,
            into,
            parent: self.parent.as_ref(),
            provided: &self.provided,
            hostnames: &self.hostnames,
            from_kept: self.from_kept,
            picks,
        })
    }

    /// How a variable lands unless picked otherwise.
    fn default(&self, offered: &BranchChoice) -> BranchOption {
        match self.update {
            true => BranchOption::From,
            false => offered.default,
        }
    }

    /// Row `key` as `NODE[.path]` in the catalog's words (`web.image`,
    /// `web.env.KEY`), by the node's name where it comes from, else in `into`.
    fn name(&self, into: &SavedEnvironmentIntent, key: &str) -> String {
        let (lineage, path) = split(key);
        let name_in = |lineage: &str| {
            name_of(&self.from, lineage)
                .or_else(|| name_of(into, lineage))
                .unwrap_or_else(|| lineage.to_owned())
        };
        let node = name_in(lineage);
        if path == "node" {
            return node;
        }
        let path = match (
            path.strip_prefix("variables."),
            path.strip_prefix("mounts."),
        ) {
            (Some(key), _) => format!("env.{key}"),
            (_, Some(volume)) => format!("mounts.{}", name_in(volume)),
            _ => setting(path).map_or_else(|| path.to_owned(), |setting| setting.name().to_owned()),
        };
        format!("{node}.{path}")
    }

    /// Land `picks` in `into` and advance the Branch's base by exactly what landed.
    /// Into a Parent, nothing there is deleted.
    fn apply(
        &self,
        tx: &mut dyn Tx,
        who: &Actor,
        into: &mut Environment,
        picks: Vec<BranchPick>,
    ) -> Result<Vec<String>, RpcError> {
        let moved = self.compare(&into.working, Some(picks.clone()))?;
        let base = moved
            .base
            .ok_or_else(|| error::internal("Core returned no base for a move"))?;
        let ids = |intent: &SavedEnvironmentIntent| -> BTreeSet<String> {
            let services = intent.services.iter().map(|service| service.id.clone());
            let volumes = intent
                .volumes
                .iter()
                .map(|volume| volume.resource_id.clone());
            services.chain(volumes).collect()
        };
        if !self.update && !ids(&into.working).is_subset(&ids(&moved.next)) {
            return Err(error::internal("A Save would delete from the Parent"));
        }
        let staged = land(tx, who, into, &self.source, &self.from, moved.next, &picks)?;
        tx.execute(
            "UPDATE config_environment_branch SET base = ?1 WHERE environment_id = ?2",
            &[document(&base).as_str().into(), self.branch.as_str().into()],
        )?;
        Ok(staged)
    }
}

/// The Setting a row's core path writes.
fn setting(path: &str) -> Option<ServiceSetting> {
    ServiceSetting::ALL
        .into_iter()
        .find(|setting| setting.field() == path)
}

/// A row's value as `get` shows it: never a credential or a repository ID.
fn shown(path: &str, value: &Value) -> Value {
    match path {
        "source.credentials" if *value == Value::Bool(true) => json!({ "secret": true }),
        "source.credentials" => Value::Null,
        "source.repository" => value.get("repository").cloned().unwrap_or(Value::Null),
        path => setting(path).map_or_else(|| value.clone(), |setting| setting.shown(value.clone())),
    }
}

/// A row key's lineage and path.
fn split(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or((key, ""))
}

/// Land core's `next` as `branch`'s Working State: the nodes arriving from
/// `source` (whose configuration was `from`) bring their registry credentials and
/// get their Node Introductions, and a picked credential row brings its credential
/// to the Service already there. Returns the nodes staged.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    branch: &mut Environment,
    source: &EnvironmentId,
    from: &SavedEnvironmentIntent,
    next: SavedEnvironmentIntent,
    picks: &[BranchPick],
) -> Result<Vec<String>, RpcError> {
    let before = std::mem::replace(&mut branch.working, next);
    scope::save_working(tx, branch)?;
    let id = branch.summary.id.clone();
    let source_of = |lineage: &str| {
        from.services
            .iter()
            .find(|service| service.lineage_id == lineage)
            .map(|service| service.id.clone())
    };
    let mut staged = Vec::new();
    for service in &branch.working.services {
        let old = before.services.iter().find(|old| old.id == service.id);
        if old == Some(service) {
            continue;
        }
        staged.push(service.slug.clone());
        if old.is_some() {
            continue;
        }
        let credentials = matches!(
            service.config.source,
            ServiceSource::Image {
                credentials: ServiceImageCredentials::Configured { .. },
                ..
            }
        );
        if credentials && let Some(source_id) = source_of(&service.lineage_id) {
            registry::copy(tx, who, (source, &source_id), (&id, &service.id))?;
        }
        introduce(tx, who, &id, "service", &service.id, service)?;
    }
    for pick in picks {
        let Some(lineage) = pick.key.strip_suffix(":source.credentials") else {
            continue;
        };
        let receiver = before.services.iter().find(|old| old.lineage_id == lineage);
        if let (Some(receiver), Some(source_id)) = (receiver, source_of(lineage)) {
            registry::copy(tx, who, (source, &source_id), (&id, &receiver.id))?;
        }
    }
    for volume in &branch.working.volumes {
        if !before
            .volumes
            .iter()
            .any(|old| old.resource_id == volume.resource_id)
        {
            staged.push(format!("volumes.{}", volume.name));
            introduce(tx, who, &id, "volume", &volume.resource_id, volume)?;
        }
    }
    branch.live = live_names(tx, &id, &branch.working)?;
    staged.sort();
    Ok(staged)
}

fn introduce(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    node_type: &str,
    id: &str,
    node: &impl Serialize,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_node_introduction \
         (environment_id, node_id, organization_id, node_type, node) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        &[
            environment.as_str().into(),
            id.into(),
            who.organization.as_str().into(),
            node_type.into(),
            serde_json::to_string(node)
                .expect("a node is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

/// Update and Own Copy rewrite what a Branch runs, so they wait until it runs its
/// Working State: no Deployment in flight and nothing staged.
fn settled(tx: &mut dyn Tx, branch: &Environment) -> Result<(), RpcError> {
    let scope = format!(
        "--project {} --env {}",
        branch.summary.project, branch.summary.name
    );
    let busy = tx.query(
        "SELECT number FROM config_deployment \
         WHERE environment_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
        &[branch.summary.id.as_str().into()],
    )?;
    if !busy.is_empty() {
        return Err(error::conflict(
            "A Deployment of this Branch is still running: wait for it to finish",
            json!({ "next": format!("ployz deployment ls {scope}") }),
        ));
    }
    if !review::review(tx, branch)?.view.changes.is_empty() {
        return Err(error::conflict(
            "This Branch has changes that aren't deployed: deploy or discard them first",
            json!({ "next": format!("ployz diff {scope}") }),
        ));
    }
    Ok(())
}

/// The changed Services `parent`'s failed Deployment `id` didn't apply, as it
/// configured them: those it would have changed from what `parent` runs.
fn failed_services(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &Environment,
    applied: &SavedEnvironmentIntent,
    id: &DeploymentId,
) -> Result<Vec<SavedServiceIntent>, RpcError> {
    let view = deployment::view(tx, who, id)?;
    if view.environment.id != parent.summary.id {
        return Err(error::invalid(
            format!("Deployment {id} is not one of {}", parent.summary.name),
            json!({ "fix": id }),
        ));
    }
    if view.deployment.status != DeploymentStatus::Failed {
        return Err(error::invalid(
            "Only a failed Deployment is fixed on a Branch",
            json!({ "fix": id }),
        ));
    }
    let saved = deployment::saved_at(tx, &parent.summary.id, view.deployment.saved)?;
    let applied = canonicalize_environment_intent(applied.clone());
    Ok(saved
        .services
        .into_iter()
        .filter(|service| {
            view.nodes
                .iter()
                .any(|node| node.id == service.id && node.outcome != NodeStatus::Applied)
                && !applied.services.contains(service)
        })
        .collect())
}

/// What a Branch's Live Nodes provide to the Services reading them, from the
/// nearest Environment `parent` or above that runs each: its nodes' values and,
/// when it is itself a Branch, the values it reads live in turn.
fn live_producers(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
    uses: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<SavedVariableProducer>, RpcError> {
    if uses.is_empty() {
        return Ok(Vec::new());
    }
    let chain = chain(tx, parent)?;
    let mut producers = Vec::new();
    for owner in &chain {
        let lineages: Vec<LiveLineageUse> = uses
            .iter()
            .filter(|(lineage, _)| {
                chain
                    .iter()
                    .find(|ancestor| holds(&ancestor.applied, lineage))
                    .is_some_and(|found| {
                        found.environment.summary.id == owner.environment.summary.id
                    })
            })
            .map(|(lineage, keys)| LiveLineageUse {
                lineage_id: lineage.clone(),
                keys: keys.iter().cloned().collect(),
            })
            .collect();
        if lineages.is_empty() {
            continue;
        }
        let id = &owner.environment.summary.id;
        let rows = tx.query(
            "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
            &[id.as_str().into()],
        )?;
        let Some(namespace) = rows.first() else {
            continue;
        };
        let namespace =
            Namespace::parse(namespace.text(0)?).map_err(|_| error::corrupt("Namespace"))?;
        let mut provided =
            compile_environment_intent(id.as_str(), owner.applied.clone()).variable_producers;
        if let Some(row) = row(tx, id)? {
            provided.extend(live_producers(tx, &row.parent, &used_live(&owner.applied))?);
        }
        let values = live_values(LiveValuesInput {
            owner: LiveValuesOwner {
                namespace,
                producers: provided,
            },
            lineages,
        });
        producers.extend(values.producers);
    }
    Ok(producers)
}

/// Each Service lineage `intent`'s variables reference but it doesn't own, with the
/// keys read from it: the nodes it uses live.
pub(crate) fn used_live(intent: &SavedEnvironmentIntent) -> BTreeMap<String, BTreeSet<String>> {
    let owned: BTreeSet<&str> = intent
        .services
        .iter()
        .map(|service| service.lineage_id.as_str())
        .collect();
    let mut uses: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for variable in intent
        .services
        .iter()
        .flat_map(|service| &service.variables)
    {
        let ployz_core::config::SavedVariableValue::Template { parts } = &variable.value else {
            continue;
        };
        for part in parts {
            if let ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                key,
            } = part
                && !owned.contains(lineage_id.as_str())
            {
                uses.entry(lineage_id.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
    }
    uses
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
fn ancestors(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Vec<EnvironmentId>, RpcError> {
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
        parent: EnvironmentId::parse(row.text(0)?).map_err(|_| error::corrupt("Branch"))?,
        kept: row.int(1)? != 0,
        base: parse(row.text(2)?)?,
        setup: serde_json::from_str(row.text(3)?).map_err(|_| error::corrupt("Branch"))?,
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

/// The sides of core's comparison: `from` and `into` over `base`, moving `picks` when given.
struct Comparing<'a> {
    base: Option<&'a SavedEnvironmentIntent>,
    from: &'a SavedEnvironmentIntent,
    into: &'a SavedEnvironmentIntent,
    parent: Option<&'a SavedEnvironmentIntent>,
    provided: &'a [String],
    hostnames: &'a BranchHostnames,
    from_kept: bool,
    picks: Option<Vec<BranchPick>>,
}

fn compare(sides: Comparing<'_>) -> Result<BranchChanges, RpcError> {
    let value = |intent: &SavedEnvironmentIntent| {
        serde_json::to_value(intent).expect("Working State is JSON")
    };
    branch_changes(BranchChangesInput {
        base: sides.base.map(value),
        from: value(sides.from),
        into: value(sides.into),
        parent: sides.parent.map(value),
        provided: sides.provided.to_vec(),
        hostnames: sides.hostnames.clone(),
        from_kept: sides.from_kept,
        picks: sides.picks,
    })
    .map_err(config)
}

fn config(error: ConfigError) -> RpcError {
    error::invalid(
        format!("{}: {}", error.path, error.message),
        json!({ "path": error.path }),
    )
}

/// A Setup Command's command, trimmed, or why it is refused.
pub(crate) fn setup_command(setup: &SetupCommand) -> Result<String, RpcError> {
    let command = setup.command.trim();
    if command.is_empty() || command.chars().count() > SETUP_LIMIT {
        return Err(error::invalid(
            format!(
                "{}: a Setup Command has 1-{SETUP_LIMIT} characters",
                setup.service
            ),
            json!({ "service": setup.service }),
        ));
    }
    Ok(command.to_owned())
}

/// How many of Branch `branch`'s changes a Save would move into `into`.
pub(crate) fn changes_into(
    tx: &mut dyn Tx,
    branch: &Environment,
    into: &Environment,
) -> Result<usize, RpcError> {
    let Some(row) = row(tx, &branch.summary.id)? else {
        return Ok(0);
    };
    let hostnames = BranchHostnames {
        from: suffix(tx, branch)?,
        into: suffix(tx, into)?,
    };
    let provided: Vec<String> = used_live(&branch.working).into_keys().collect();
    Ok(compare(Comparing {
        base: Some(&row.base),
        from: &branch.working,
        into: &into.working,
        parent: None,
        provided: &provided,
        hostnames: &hostnames,
        from_kept: false,
        picks: None,
    })?
    .rows
    .iter()
    .filter(|row| matches!(row.role, BranchRole::Move { .. }))
    .count())
}

/// The lineage of the Service or Volume named `name` in `intent`.
pub(crate) fn lineage_named(
    intent: &SavedEnvironmentIntent,
    name: &str,
) -> Result<String, RpcError> {
    let service = intent.services.iter().find(|service| service.slug == name);
    let volume = intent.volumes.iter().find(|volume| volume.name == name);
    match (service, volume) {
        (Some(service), None) => Ok(service.lineage_id.clone()),
        (None, Some(volume)) => Ok(volume.resource_lineage_id.clone()),
        (Some(_), Some(_)) => Err(error::ambiguous(
            format!("{name} names a Service and a Volume: rename one"),
            json!({ "node": name }),
        )),
        (None, None) => {
            let names = names(intent);
            Err(error::not_found(
                format!("No Service or Volume named {name}"),
                json!({
                    "did_you_mean": error::did_you_mean(name, names.iter().map(String::as_str)),
                    "valid_children": names,
                }),
            ))
        }
    }
}

fn names(intent: &SavedEnvironmentIntent) -> Vec<String> {
    intent
        .services
        .iter()
        .map(|service| service.slug.clone())
        .chain(intent.volumes.iter().map(|volume| volume.name.clone()))
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

fn empty(name: &str) -> SavedEnvironmentIntent {
    SavedEnvironmentIntent {
        version: 1,
        environment_slug: name.to_owned(),
        services: Vec::new(),
        volumes: Vec::new(),
    }
}

fn document(intent: &SavedEnvironmentIntent) -> String {
    serde_json::to_string(intent).expect("Working State is JSON")
}

fn parse(text: &str) -> Result<SavedEnvironmentIntent, RpcError> {
    serde_json::from_str(text)
        .ok()
        .and_then(|value| parse_environment_intent(value).ok())
        .ok_or_else(|| error::corrupt("Branch"))
}
