//! The plan: what each row's sides hold, its verdict under a policy, and landing
//! the picked rows.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::*;
use crate::config::service_changes::default_value;
use crate::config::{
    ConfigError, SavedEnvironmentIntent as Intent, SavedServiceIntent, SavedVolumeIntent,
    ServiceImageCredentials, ServiceSource, VolumeAttachment, parse_environment_intent,
    redact_environment_intent,
};

/// Each side's generated-address suffix, appended to managed hostname prefixes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hostnames {
    /// The sender's.
    pub from: String,
    /// The receiver's.
    pub into: String,
}

/// The three configurations of one move: changes flow from `from` into `into`, judged
/// against what the two last shared. No `base` means nothing is shared yet.
pub struct Sides<'intent> {
    /// What the two last shared.
    pub base: Option<&'intent Intent>,
    /// The sender.
    pub from: &'intent Intent,
    /// The receiver.
    pub into: &'intent Intent,
    /// Each side's generated-address suffix.
    pub hostnames: Hostnames,
}

/// How changes travel; see [`Policy::verdict`] for what each does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Way {
    /// A person's Sync: removals and the receiver's secrets stay, and a new secret
    /// arrives needing its value.
    Sync,
    /// A Parent's deploy reaching its Branch.
    Follow,
    /// Everything as it is: a new Branch, an Own Copy, an Environment's own changes.
    Copy,
}

/// The caller's context for a comparison.
pub struct Policy {
    /// How the changes travel.
    pub way: Way,
    /// Rows `from` marked Never sync.
    pub from_marks: BTreeSet<RowId>,
    /// Rows `into` marked Never sync.
    pub into_marks: BTreeSet<RowId>,
    /// Lineages `into` uses live.
    pub live: BTreeSet<String>,
    /// The sender's own changes when it is not syncing into its own Parent; the other
    /// rows it only inherited and they start unticked. None when every row is its own.
    pub own: Option<BTreeSet<RowId>>,
}

/// Why a row never moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Its node is in use live.
    Live,
    /// One side removed its node, and removals never move.
    LeftOut,
    /// Each Environment sizes its own Services.
    Sizing,
    /// Each Environment keeps its own custom domains.
    CustomDomain,
    /// A generated address names its own Environment.
    GeneratedAddress,
    /// Each Environment builds from its own git branch.
    GitBranch,
    /// A Volume's data stays where it is.
    Data,
    /// Either side marked it Never sync.
    NeverSynced,
}

/// Whether a row moves, and how.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// It moves when picked.
    Moves {
        /// `into` changed it too since the base.
        conflict: bool,
        /// Picked unless someone unticks it.
        ticked: bool,
        /// Whether it lands as it is.
        arrives: Arrives,
    },
    /// It differs, but never moves.
    Differs(Why),
}

/// How a moving row lands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arrives {
    /// With the sender's cell.
    AsIs,
    /// A secret that lands without its value unless [`Plan::apply`] gets one.
    NeedsValue,
}

/// One row that differs between the sides, and what a move does with it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlannedRow {
    /// Which row.
    pub id: RowId,
    /// What the sides last shared there; redacted.
    pub base: Cell,
    /// The sender's cell; redacted.
    pub from: Cell,
    /// The receiver's cell; redacted.
    pub into: Cell,
    /// The new node's row, which must be picked too.
    pub requires: Option<RowId>,
    /// Whether it moves.
    pub verdict: Verdict,
}

/// The rows between two configurations, ready to apply.
#[derive(Clone, Debug)]
pub struct Plan {
    rows: Vec<PlannedRow>,
    base: Option<Intent>,
    from: Intent,
    into: Intent,
    hostnames: Hostnames,
    live: BTreeSet<String>,
}

/// `into` after a Plan's picks landed, and the base advanced by exactly them.
#[derive(Clone, Debug)]
pub struct Applied {
    /// `into` with the picks landed.
    pub next: Intent,
    /// Redacted. Creating (no base), `from` minus what `into` uses live.
    pub base: Intent,
    /// Each picked row, as it landed.
    pub landed: Vec<Landed>,
    /// `NeedsValue` picks that landed without a value.
    pub waiting: Vec<RowId>,
}

/// One landed row: what to [`put`] back to rewind the base or undo the landing.
#[derive(Clone, Debug, PartialEq)]
pub struct Landed {
    /// Which row.
    pub row: RowId,
    /// The base's cell before; redacted.
    pub prior: Cell,
    /// `into`'s cell before.
    pub was: Was,
    /// What arrived; redacted.
    pub value: Cell,
}

/// What a landed row held in `into` before, for [`unapply`] to check and put back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Was {
    /// A Service's whole source, when landing the row switched what kind of source it
    /// is: one change, whichever source row made it.
    Source {
        /// Before the landing.
        was: Box<ServiceSource>,
        /// As the landing left it.
        landed: Box<ServiceSource>,
    },
    /// The row's own cell.
    #[serde(untagged)]
    Cell(SealedCell),
}

impl Policy {
    /// `way` with no marks, nothing live, and every row its own.
    #[must_use]
    pub fn new(way: Way) -> Self {
        Self {
            way,
            from_marks: BTreeSet::new(),
            into_marks: BTreeSet::new(),
            live: BTreeSet::new(),
            own: None,
        }
    }

    fn marked(&self, row: &RowId) -> bool {
        match self.way {
            Way::Sync => self.from_marks.contains(row) || self.into_marks.contains(row),
            Way::Follow => self.into_marks.contains(row),
            Way::Copy => false,
        }
    }

    /// The policy table, the only place that knows about secrets, removals and marks:
    ///
    /// | Way    | `from` Absent | secret, `into` has one                      | secret, `into` lacks it | Never sync marks |
    /// |--------|---------------|---------------------------------------------|-------------------------|------------------|
    /// | Sync   | no row        | no row                                      | `NeedsValue`            | both sides'      |
    /// | Follow | moves         | if `into` = base or `SecretWithoutValue`    | carries                 | receiver's only  |
    /// | Copy   | moves         | carries                                     | carries                 | ignored          |
    ///
    /// Settings each Environment owns ([`owned`]) differ and never move, even in a new
    /// node, but for a copy's new node, which starts with them as they are.
    fn verdict(&self, row: &RowId, base: &Cell, from: &Cell, into: &Cell) -> Option<Verdict> {
        match owned(&row.at) {
            Some(why) => (from != into).then_some(Verdict::Differs(why)),
            None => self.moved(row, base, from, into),
        }
    }

    /// [`Self::verdict`] for a setting no Environment owns.
    fn moved(&self, row: &RowId, base: &Cell, from: &Cell, into: &Cell) -> Option<Verdict> {
        if from == into || from == base {
            return None;
        }
        let mut conflict = into != base;
        let secret = from.is_secret() || into.is_secret();
        let held = secret && *into != Cell::Absent;
        let arrives = match self.way {
            Way::Sync if *from == Cell::Absent || held => return None,
            Way::Sync if secret => Arrives::NeedsValue,
            Way::Follow if held && *into == Cell::SecretWithoutValue => {
                conflict = false;
                Arrives::AsIs
            }
            Way::Follow if held && conflict => return None,
            Way::Sync | Way::Follow | Way::Copy => Arrives::AsIs,
        };
        Some(if self.marked(row) {
            Verdict::Differs(Why::NeverSynced)
        } else {
            Verdict::Moves {
                conflict,
                ticked: self.own.as_ref().is_none_or(|own| own.contains(row)),
                arrives,
            }
        })
    }
}

/// Settings each Environment owns: shown as meant to differ, never moved.
fn owned(at: &At) -> Option<Why> {
    let At::Setting(setting) = at else {
        return None;
    };
    match setting {
        Setting::Replicas | Setting::CpuLimit | Setting::MemLimit => Some(Why::Sizing),
        Setting::Routes => Some(Why::CustomDomain),
        Setting::ManagedHostnames => Some(Why::GeneratedAddress),
        Setting::Branch => Some(Why::GitBranch),
        Setting::Repository
        | Setting::Image
        | Setting::RootDir
        | Setting::Credentials
        | Setting::PrivateDns
        | Setting::PreDeployCommand
        | Setting::StartCommand
        | Setting::Healthcheck
        | Setting::RestartPolicy
        | Setting::MaxRetries
        | Setting::BuildMethod
        | Setting::DockerfilePath
        | Setting::BuildCommand => None,
    }
}

/// Compare `sides` under `policy`: every place either side holds, by lineage.
#[must_use]
pub fn plan(sides: Sides, policy: &Policy) -> Plan {
    let Sides {
        base,
        from,
        into,
        hostnames,
    } = sides;
    let (base_nodes, from_nodes, into_nodes) = (
        base.map(nodes).unwrap_or_default(),
        nodes(from),
        nodes(into),
    );
    let base_cells = base.map(|base| Cells::of(base, &hostnames.into));
    let from_cells = Cells::of(from, &hostnames.from);
    let into_cells = Cells::of(into, &hostnames.into);
    let mut rows = Vec::new();
    let mut push = |id: RowId, cells: [Cell; 3], requires: Option<RowId>, verdict| {
        let [base, from, into] = cells;
        rows.push(PlannedRow {
            id,
            base,
            from,
            into,
            requires,
            verdict,
        });
    };
    let cells_at = |row: &RowId| {
        [
            base_cells
                .as_ref()
                .map_or(&Cell::Absent, |cells| cells.at(row)),
            from_cells.at(row),
            into_cells.at(row),
        ]
        .map(Cell::clone)
    };
    let node_cells = |lineage: &str| cells_at(&RowId::node(lineage));
    for lineage in from_nodes
        .keys()
        .chain(into_nodes.keys())
        .collect::<BTreeSet<_>>()
    {
        let in_base = base_nodes.contains_key(lineage);
        match (from_nodes.get(lineage), into_nodes.get(lineage)) {
            (Some(&from_node), Some(&into_node)) => {
                for id in from_cells
                    .places(lineage)
                    .chain(into_cells.places(lineage))
                    .map(|(id, _)| id)
                    .collect::<BTreeSet<_>>()
                {
                    let id = id.clone();
                    let sides = cells_at(&id);
                    let [b, f, i] = &sides;
                    if let Some(verdict) = policy.verdict(&id, b, f, i) {
                        push(id, sides, None, verdict);
                    }
                }
                if let (NodeRef::Volume(_), NodeRef::Volume(_)) = (from_node, into_node) {
                    let id = RowId {
                        lineage: (*lineage).to_owned(),
                        at: At::Data,
                    };
                    push(id, node_cells(lineage), None, Verdict::Differs(Why::Data));
                }
            }
            (Some(_), None) if !policy.live.contains(*lineage) && !in_base => {
                let id = RowId::node(lineage);
                let node_cells = node_cells(lineage);
                let Some(mut verdict) =
                    policy.verdict(&id, &Cell::Absent, &node_cells[1], &Cell::Absent)
                else {
                    continue;
                };
                if from_cells
                    .places(lineage)
                    .any(|(child, _)| child.at.needed() && policy.marked(child))
                {
                    verdict = Verdict::Differs(Why::NeverSynced);
                }
                push(id.clone(), node_cells, None, verdict);
                for (child, cell) in from_cells
                    .places(lineage)
                    .filter(|(id, _)| !id.at.carried())
                {
                    let child = child.clone();
                    let cell = cell.clone();
                    let judge = match policy.way {
                        Way::Copy => Policy::moved,
                        Way::Sync | Way::Follow => Policy::verdict,
                    };
                    if let Some(verdict) =
                        judge(policy, &child, &Cell::Absent, &cell, &Cell::Absent)
                    {
                        push(
                            child,
                            [Cell::Absent, cell, Cell::Absent],
                            Some(id.clone()),
                            verdict,
                        );
                    }
                }
            }
            (Some(_), None) => {
                let why = if policy.live.contains(*lineage) {
                    Why::Live
                } else {
                    Why::LeftOut
                };
                push(
                    RowId::node(lineage),
                    node_cells(lineage),
                    None,
                    Verdict::Differs(why),
                );
            }
            // ponytail: a node `from` lacks never leaves `into`; a Copy or Follow of a
            // deleted node shows it as left out. Add node removal when someone asks.
            (None, Some(_)) if uses(from, lineage) || in_base => {
                let why = if uses(from, lineage) {
                    Why::Live
                } else {
                    Why::LeftOut
                };
                push(
                    RowId::node(lineage),
                    node_cells(lineage),
                    None,
                    Verdict::Differs(why),
                );
            }
            (None, Some(_) | None) => {}
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Plan {
        rows,
        base: base.cloned(),
        from: from.clone(),
        into: into.clone(),
        hostnames,
        live: policy.live.clone(),
    }
}

impl Plan {
    /// Every row that differs, in row order.
    #[must_use]
    pub fn rows(&self) -> &[PlannedRow] {
        &self.rows
    }

    /// The canonical, id-free rendering of the rows, their verdicts and what is ticked;
    /// the caller hashes it into the version a person reviewed.
    #[must_use]
    pub fn digest(&self) -> String {
        serde_json::to_string(&self.rows).expect("rows are JSON")
    }

    /// The most of `picks` that [`Self::apply`] takes with no `values`: each pick in
    /// landing order, left out when it would fail, and with it the rows of its node.
    #[must_use]
    pub fn landable(&self, picks: &BTreeSet<RowId>) -> BTreeSet<RowId> {
        let none = BTreeMap::new();
        if self.apply(picks, &none).is_ok() {
            return picks.clone();
        }
        let mut order: Vec<&RowId> = picks.iter().collect();
        order.sort_by_key(|row| landing_order(&self.from, row));
        // ponytail: one apply per pick, O(n²) only once something is refused.
        let mut landable = BTreeSet::new();
        for pick in order {
            landable.insert(pick.clone());
            if self.apply(&landable, &none).is_err() {
                landable.remove(pick);
            }
        }
        landable
    }

    /// Land `picks` in `into`: [`put`] once per pick, a new node first. A `NeedsValue`
    /// pick takes its secret from `values`, or lands without one and is `waiting`;
    /// `values` for any other row are ignored.
    ///
    /// # Errors
    /// Returns ConfigError for an unknown pick, a pick of a row that differs or whose new
    /// node isn't picked, a name clash, or a `next` that isn't valid.
    pub fn apply(
        &self,
        picks: &BTreeSet<RowId>,
        values: &BTreeMap<RowId, SealedSecret>,
    ) -> Result<Applied, ConfigError> {
        let refuse = |message| ConfigError::at("picks", message);
        let mut landing = Vec::new();
        for pick in picks {
            let row = self
                .rows
                .iter()
                .find(|row| row.id == *pick)
                .ok_or_else(|| refuse("Unknown change"))?;
            let Verdict::Moves { arrives, .. } = row.verdict else {
                return Err(refuse("Change is meant to differ"));
            };
            if row
                .requires
                .as_ref()
                .is_some_and(|node| !picks.contains(node))
            {
                return Err(refuse("Pick the new node this change belongs to"));
            }
            landing.push((row, arrives));
        }
        landing.sort_by_key(|(row, _)| landing_order(&self.from, &row.id));
        let mut next = self.into.clone();
        let mut base = self.base.clone();
        let mut landed = Vec::new();
        let mut waiting = Vec::new();
        let into_cells = Cells::of(&self.into, &self.hostnames.into);
        let from_cells = Cells::of(&self.from, &self.hostnames.from);
        for (row, arrives) in landing {
            let id = &row.id;
            let mut was = Was::Cell(sealed_cell(&self.into, &into_cells, id));
            let value = match arrives {
                Arrives::AsIs => sealed_cell(&self.from, &from_cells, id),
                Arrives::NeedsValue => match values.get(id) {
                    Some(secret) => SealedCell::Secret(secret.clone()),
                    None => {
                        waiting.push(id.clone());
                        SealedCell::Cell(Cell::SecretWithoutValue)
                    }
                },
            };
            if id.at == At::Node {
                self.introduce(&mut next, base.as_mut(), &id.lineage)?;
            } else {
                let kind = |env: &Intent| {
                    service(env, &id.lineage).map(|s| std::mem::discriminant(&s.config.source))
                };
                let before = kind(&next);
                put_sealed(&mut next, id, &value)?;
                if kind(&next) != before
                    && let Some(service) = service(&self.into, &id.lineage)
                {
                    let source = Box::new(service.config.source.clone());
                    // `landed` is the source as the whole landing leaves it; see below.
                    was = Was::Source {
                        landed: source.clone(),
                        was: source,
                    };
                }
                if let (At::Variable(key), Was::Cell(SealedCell::Cell(Cell::Absent))) =
                    (&id.at, &was)
                {
                    self.describe(&mut next, &id.lineage, key);
                }
                if let Some(base) = base.as_mut() {
                    adopt(base, &self.into, id);
                    put_into(base, id, &row.from)?;
                }
            }
            landed.push(Landed {
                row: id.clone(),
                prior: row.base.clone(),
                was,
                value: value.to_redacted(),
            });
        }
        for one in &mut landed {
            if let (Was::Source { landed, .. }, Some(service)) =
                (&mut one.was, service(&next, &one.row.lineage))
            {
                **landed = service.config.source.clone();
            }
        }
        let next = parse_environment_intent(json!(next))?;
        let base = base.unwrap_or_else(|| {
            let mut base = self.from.clone();
            base.services
                .retain(|service| !self.live.contains(&service.lineage_id));
            base
        });
        Ok(Applied {
            next,
            base: redact_environment_intent(base),
            landed,
            waiting,
        })
    }

    /// Add `from`'s node at `lineage` to `next` (and `base`) as it needs to exist: fresh
    /// ids, no custom domains, `into`'s generated-address naming, its own credential, and
    /// every setting that arrives as a row of its own at its default.
    fn introduce(
        &self,
        next: &mut Intent,
        base: Option<&mut Intent>,
        lineage: &str,
    ) -> Result<(), ConfigError> {
        let clash = || ConfigError::at("picks", "Name or private address is already used");
        if let Some(source) = volume(&self.from, lineage) {
            if next.volumes.iter().any(|v| v.name == source.name) {
                return Err(clash());
            }
            let copy = SavedVolumeIntent {
                resource_id: uuid::Uuid::new_v4().to_string(),
                ..source.clone()
            };
            if let Some(base) = base {
                base.volumes.push(copy.clone());
            }
            next.volumes.push(copy);
            return Ok(());
        }
        let source = service(&self.from, lineage).expect("node rows are nodes `from` has");
        if next
            .services
            .iter()
            .any(|s| s.slug == source.slug || s.config.private_dns == source.config.private_dns)
        {
            return Err(clash());
        }
        let mut copy = SavedServiceIntent {
            id: uuid::Uuid::new_v4().to_string(),
            lineage_id: source.lineage_id.clone(),
            slug: source.slug.clone(),
            config: source.config.clone(),
            variables: Vec::new(),
            volume_attachments: Vec::new(),
        };
        for setting in Setting::ALL.into_iter().filter(|s| !s.carried()) {
            put_setting(&mut copy, setting, default_value(setting.path()))
                .expect("a setting takes its default");
        }
        let config = &mut copy.config;
        config.routes.clear();
        for hostname in &mut config.managed_hostnames {
            let prefix = hostname
                .prefix
                .strip_suffix(self.hostnames.from.as_str())
                .unwrap_or(&hostname.prefix);
            hostname.prefix = format!("{prefix}{}", self.hostnames.into);
        }
        if let ServiceSource::Image {
            credentials: ServiceImageCredentials::Configured { credential_id },
            ..
        } = &mut config.source
        {
            credential_id.clone_from(&copy.id);
        }
        if let Some(base) = base {
            base.services.push(copy.clone());
        }
        next.services.push(copy);
        Ok(())
    }

    /// A variable new to `next` takes `from`'s description and export.
    // ponytail: rows compare a variable's value only; a changed description or export
    // never moves on its own. Add a row for them when someone misses it.
    fn describe(&self, next: &mut Intent, lineage: &str, key: &str) {
        let source =
            service(&self.from, lineage).and_then(|s| s.variables.iter().find(|v| v.key == key));
        let target =
            service_mut(next, lineage).and_then(|s| s.variables.iter_mut().find(|v| v.key == key));
        if let (Some(source), Some(target)) = (source, target) {
            target.description.clone_from(&source.description);
            target.exported = source.exported;
        }
    }
}

/// Why [`unapply`] refused.
#[derive(Debug, thiserror::Error)]
pub enum Unapplied {
    /// The row holds other than what landed (a source switch, any of the source), or
    /// it is a new node that holds a row that didn't land with it.
    #[error("{0} changed since it landed")]
    Changed(RowId),
    /// What it puts back doesn't hold together.
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// Put back what each of `landed` held before it landed in `intent`, whose generated
/// addresses end in `suffix`: [`Plan::apply`] in reverse. A new node goes whole.
///
/// # Errors
/// Returns [`Unapplied::Changed`] for a row changed since it landed, and
/// [`Unapplied::Invalid`] when what it held no longer fits.
pub fn unapply(intent: &Intent, suffix: &str, landed: &[Landed]) -> Result<Intent, Unapplied> {
    let cells = Cells::of(intent, suffix);
    for one in landed {
        let row = &one.row;
        let source = service(intent, &row.lineage).map(|s| &s.config.source);
        let changed = match &one.was {
            Was::Source { landed, .. } => source != Some(landed),
            Was::Cell(_) => *cells.at(row) != one.value,
        };
        if changed {
            return Err(Unapplied::Changed(row.clone()));
        }
        if row.at != At::Node {
            continue;
        }
        if let Some((child, _)) = cells
            .places(&row.lineage)
            .find(|(child, _)| !child.at.needed() && !landed.iter().any(|l| l.row == **child))
        {
            return Err(Unapplied::Changed(child.clone()));
        }
    }
    let mut order: Vec<&Landed> = landed.iter().collect();
    order.sort_by_key(|one| std::cmp::Reverse(landing_order(intent, &one.row)));
    let mut intent = intent.clone();
    for one in order {
        match &one.was {
            Was::Source { was, .. } => {
                if let Some(service) = service_mut(&mut intent, &one.row.lineage) {
                    service.config.source = (**was).clone();
                }
            }
            Was::Cell(was) => put_sealed(&mut intent, &one.row, was)?,
        }
    }
    Ok(intent)
}

/// Where `row` lands in a landing: new Volumes before the Services that mount them,
/// nodes before their rows. `env` holds the node.
fn landing_order(env: &Intent, row: &RowId) -> u8 {
    match row.at {
        At::Node if volume(env, &row.lineage).is_some() => 0,
        At::Node => 1,
        At::Data | At::Name | At::Storage | At::Setting(_) | At::Mount(_) | At::Variable(_) => 2,
    }
}

/// The marks among `marks` that keep `row` from syncing: its own and, for a node,
/// those on what it can't arrive without.
// ponytail: a removed node's row also counts its children's marks; split by the
// row's sides if unmarking those ever surprises.
pub fn marks_on<'marks>(
    row: &RowId,
    marks: &'marks BTreeSet<RowId>,
) -> impl Iterator<Item = &'marks RowId> {
    marks.iter().filter(move |mark| {
        *mark == row || (row.at == At::Node && mark.lineage == row.lineage && mark.at.needed())
    })
}

/// Whether `env` references a lineage it does not own, i.e. uses it live.
fn uses(env: &Intent, lineage: &str) -> bool {
    env.services
        .iter()
        .flat_map(|s| &s.variables)
        .any(|v| v.value.referenced_lineages().any(|used| used == lineage))
}

/// A base that predates a row's node (or the Volume it mounts) starts it from `into`'s.
fn adopt(base: &mut Intent, into: &Intent, row: &RowId) {
    let mounted = match &row.at {
        At::Mount(volume) => Some(volume.as_str()),
        At::Node | At::Data | At::Name | At::Storage | At::Setting(_) | At::Variable(_) => None,
    };
    for lineage in [mounted, Some(row.lineage.as_str())].into_iter().flatten() {
        if nodes(base).contains_key(lineage) {
            continue;
        }
        if let Some(volume) = volume(into, lineage) {
            base.volumes.push(volume.clone());
        } else if let Some(service) = service(into, lineage) {
            // Mounts on Volumes the base lacks stay out until a mount row lands them.
            let attachments = service
                .volume_attachments
                .iter()
                .filter_map(|attachment| {
                    let lineage = &into
                        .volumes
                        .iter()
                        .find(|v| v.resource_id == attachment.volume_resource_id)?
                        .resource_lineage_id;
                    Some(VolumeAttachment {
                        volume_resource_id: volume(base, lineage)?.resource_id.clone(),
                        mount_path: attachment.mount_path.clone(),
                    })
                })
                .collect();
            base.services.push(SavedServiceIntent {
                volume_attachments: attachments,
                ..service.clone()
            });
        }
    }
}
