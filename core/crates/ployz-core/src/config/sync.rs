//! Branch Sync's engine. It compares two authored configurations over the base they
//! share, one row per lineage and place, and lands the picked rows. There is one exact
//! comparison, one policy table ([`Policy::verdict`]) and one writer ([`put`]), the
//! inverse of the projection a row's cells come from.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use super::service_changes::{at, default_value};
use super::{
    AuthoredServiceConfig, ConfigError, EncryptedSecretValue, SavedEnvironmentIntent as Intent,
    SavedServiceIntent, SavedVariableIntent, SavedVariableValue, SavedVolumeIntent,
    ServiceGitAccess, ServiceGitBranch, ServiceImageCredentials, ServiceSource, VolumeAttachment,
    parse_environment_intent, redact_environment_intent,
};

/// The Service settings a row can address, in landing order: what a source is comes
/// before what it holds.
const SETTINGS: &[&str] = &[
    "source.repository",
    "source.image",
    "source.rootDir",
    "source.branch",
    "source.credentials",
    "privateDns",
    "managedHostnames",
    "routes",
    "preDeployCommand",
    "startCommand",
    "healthcheck",
    "restartPolicy",
    "maxRetries",
    "replicas",
    "cpuLimit",
    "memLimit",
    "build.buildMethod",
    "build.dockerfilePath",
    "build.command",
];

/// One row's address: a lineage and a place in its node. The only row address anywhere;
/// it reads and writes as `<lineage>:<at>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId {
    lineage: String,
    at: At,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum At {
    Node,
    Data,
    Name,
    Storage,
    Setting(&'static str),
    /// A mount on the Volume of this lineage.
    Mount(String),
    Variable(String),
}

impl At {
    /// Row order, which is also landing order within a node.
    fn rank(&self) -> (u8, usize, &str) {
        match self {
            Self::Node => (0, 0, ""),
            Self::Data => (1, 0, ""),
            Self::Name => (2, 0, ""),
            Self::Storage => (3, 0, ""),
            Self::Setting(path) => (4, SETTINGS.iter().position(|s| s == path).unwrap_or(0), ""),
            Self::Mount(volume) => (5, 0, volume),
            Self::Variable(key) => (6, 0, key),
        }
    }

    /// Settings a new node arrives with on its node row rather than as rows of their own.
    fn carried(&self) -> bool {
        match self {
            Self::Name | Self::Storage => true,
            Self::Setting(path) => path.starts_with("source.") || on_node(path),
            Self::Node | Self::Data | Self::Mount(_) | Self::Variable(_) => false,
        }
    }

    /// Whether a new node can't arrive without the row here: it needs what it is
    /// carried with, but arrives without custom domains anyway.
    fn needed(&self) -> bool {
        self.carried() && *self != Self::Setting("routes")
    }
}

fn on_node(path: &str) -> bool {
    matches!(path, "privateDns" | "managedHostnames" | "routes")
}

impl Ord for At {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for At {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node => f.write_str("node"),
            Self::Data => f.write_str("data"),
            Self::Name => f.write_str("name"),
            Self::Storage => f.write_str("storage"),
            Self::Setting(path) => f.write_str(path),
            Self::Mount(volume) => write!(f, "mounts.{volume}"),
            Self::Variable(key) => write!(f, "variables.{key}"),
        }
    }
}

impl RowId {
    /// The lineage of the node the row is in.
    #[must_use]
    pub fn lineage(&self) -> &str {
        &self.lineage
    }

    /// Where in the node: `node`, `variables.KEY`, `source.image`, …
    #[must_use]
    pub fn at(&self) -> String {
        self.at.to_string()
    }

    /// The row of the node itself.
    #[must_use]
    pub fn node(lineage: &str) -> Self {
        Self {
            lineage: lineage.to_owned(),
            at: At::Node,
        }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.lineage, self.at)
    }
}

impl FromStr for RowId {
    type Err = ConfigError;

    fn from_str(row: &str) -> Result<Self, ConfigError> {
        let unknown = || ConfigError::at("row", "Unknown change");
        let (lineage, at) = row.split_once(':').ok_or_else(unknown)?;
        let at = match at {
            "node" => At::Node,
            "data" => At::Data,
            "name" => At::Name,
            "storage" => At::Storage,
            at => match (at.strip_prefix("mounts."), at.strip_prefix("variables.")) {
                (Some(volume), _) if !volume.is_empty() => At::Mount(volume.to_owned()),
                (_, Some(key)) if !key.is_empty() => At::Variable(key.to_owned()),
                _ => At::Setting(SETTINGS.iter().find(|s| **s == at).ok_or_else(unknown)?),
            },
        };
        if lineage.is_empty() {
            return Err(unknown());
        }
        Ok(Self {
            lineage: lineage.to_owned(),
            at,
        })
    }
}

impl Serialize for RowId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Branded on the TypeScript side: a row is addressed only by the id a read gave.
impl ts_rs::TS for RowId {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(_: &ts_rs::Config) -> String {
        "RowId".to_owned()
    }

    fn inline(_: &ts_rs::Config) -> String {
        "string & { readonly __brand: \"RowId\" }".to_owned()
    }

    fn decl(cfg: &ts_rs::Config) -> String {
        format!("type RowId = {};", Self::inline(cfg))
    }

    fn output_path() -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from("RowId.ts"))
    }
}

impl<'de> Deserialize<'de> for RowId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What one side holds at a row. A setting at its default is `Absent`, so unsetting is
/// a removal. A secret is there by fingerprint only: its value travels as a
/// [`SealedCell`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cell {
    /// Nothing there, or a setting at its default.
    Absent,
    /// A plain value.
    Value(Value),
    /// A secret with a value.
    Secret {
        /// Tells two values apart without showing either.
        fingerprint: String,
    },
    /// A secret still waiting for its value.
    SecretWithoutValue,
}

impl Cell {
    /// Whether it is a secret, with a value or without one.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Secret { .. } | Self::SecretWithoutValue)
    }
}

/// A secret's value, sealed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SealedSecret {
    /// Tells it apart from another value without showing either.
    pub fingerprint: String,
    /// The value, encrypted.
    pub value: EncryptedSecretValue,
}

/// What a row holds with a secret's value: what lands, and what [`unapply`] puts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealedCell {
    /// Anything but a secret with its value; a secret there by fingerprint only
    /// where the configuration holds no value for it.
    Cell(Cell),
    /// A secret with its value.
    Secret(SealedSecret),
}

impl SealedCell {
    /// What a plan compares it as.
    #[must_use]
    pub fn redacted(&self) -> Cell {
        match self {
            Self::Cell(cell) => cell.clone(),
            Self::Secret(secret) => Cell::Secret {
                fingerprint: secret.fingerprint.clone(),
            },
        }
    }
}

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
    pub was: SealedCell,
    /// What arrived; redacted.
    pub value: Cell,
}

/// A node, as [`name_of`] finds it.
#[derive(Clone, Copy, Debug)]
pub enum NodeRef<'intent> {
    /// A Service.
    Service(&'intent SavedServiceIntent),
    /// A Volume.
    Volume(&'intent SavedVolumeIntent),
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
    let At::Setting(path) = at else {
        return None;
    };
    Some(match *path {
        "replicas" | "cpuLimit" | "memLimit" => Why::Sizing,
        "routes" => Why::CustomDomain,
        "managedHostnames" => Why::GeneratedAddress,
        "source.branch" => Why::GitBranch,
        _ => return None,
    })
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
    let node_cells = |lineage: &str| {
        [
            (base, &hostnames.into),
            (Some(from), &hostnames.from),
            (Some(into), &hostnames.into),
        ]
        .map(|(env, suffix)| env.map_or(Cell::Absent, |env| node_cell(env, lineage, suffix)))
    };
    for lineage in from_nodes
        .keys()
        .chain(into_nodes.keys())
        .collect::<BTreeSet<_>>()
    {
        let in_base = base_nodes.contains_key(lineage);
        match (from_nodes.get(lineage), into_nodes.get(lineage)) {
            (Some(&from_node), Some(&into_node)) => {
                let base_cells = base
                    .zip(base_nodes.get(lineage))
                    .map(|(env, node)| cells(env, *node, &hostnames.into))
                    .unwrap_or_default();
                let from_cells = cells(from, from_node, &hostnames.from);
                let into_cells = cells(into, into_node, &hostnames.into);
                for at in from_cells
                    .keys()
                    .chain(into_cells.keys())
                    .collect::<BTreeSet<_>>()
                {
                    let id = RowId {
                        lineage: (*lineage).to_owned(),
                        at: at.clone(),
                    };
                    let cell =
                        |cells: &BTreeMap<At, Cell>| cells.get(at).cloned().unwrap_or(Cell::Absent);
                    let sides = [cell(&base_cells), cell(&from_cells), cell(&into_cells)];
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
            (Some(&node), None) if !policy.live.contains(*lineage) && !in_base => {
                let id = RowId::node(lineage);
                let from_cells = cells(from, node, &hostnames.from);
                let node_cells = node_cells(lineage);
                let Some(mut verdict) =
                    policy.verdict(&id, &Cell::Absent, &node_cells[1], &Cell::Absent)
                else {
                    continue;
                };
                if from_cells.keys().any(|at| {
                    at.needed()
                        && policy.marked(&RowId {
                            lineage: (*lineage).to_owned(),
                            at: at.clone(),
                        })
                }) {
                    verdict = Verdict::Differs(Why::NeverSynced);
                }
                push(id.clone(), node_cells, None, verdict);
                for (at, cell) in from_cells.into_iter().filter(|(at, _)| !at.carried()) {
                    let child = RowId {
                        lineage: (*lineage).to_owned(),
                        at,
                    };
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
        for (row, arrives) in landing {
            let id = &row.id;
            let was = sealed_cell(&self.into, id, &self.hostnames.into);
            let value = match arrives {
                Arrives::AsIs => sealed_cell(&self.from, id, &self.hostnames.from),
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
                put_sealed(&mut next, id, &value)?;
                if let (At::Variable(key), SealedCell::Cell(Cell::Absent)) = (&id.at, &was) {
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
                value: value.redacted(),
            });
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
        let mut config = json!(source.config);
        for path in SETTINGS.iter().filter(|path| !At::Setting(path).carried()) {
            write(&mut config, path, default_value(path));
        }
        let mut config: AuthoredServiceConfig =
            serde_json::from_value(config).expect("defaults fit a serialized config");
        config.routes.clear();
        for hostname in &mut config.managed_hostnames {
            let prefix = hostname
                .prefix
                .strip_suffix(self.hostnames.from.as_str())
                .unwrap_or(&hostname.prefix);
            hostname.prefix = format!("{prefix}{}", self.hostnames.into);
        }
        let id = uuid::Uuid::new_v4().to_string();
        if let ServiceSource::Image {
            credentials: ServiceImageCredentials::Configured { credential_id },
            ..
        } = &mut config.source
        {
            credential_id.clone_from(&id);
        }
        let copy = SavedServiceIntent {
            id,
            lineage_id: source.lineage_id.clone(),
            slug: source.slug.clone(),
            config,
            variables: Vec::new(),
            volume_attachments: Vec::new(),
        };
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

/// Give `intent` `cell` at `row`, the inverse of the projection a plan's cells come
/// from. A node arrives only whole, through [`Plan::apply`]; `Absent` removes it.
///
/// # Errors
/// Returns ConfigError when `cell` can't be held at `row`: a value of the wrong shape, a
/// mount on a Volume `intent` lacks, a setting each Environment owns, or a new node.
pub fn put(intent: &Intent, row: &RowId, cell: &Cell) -> Result<Intent, ConfigError> {
    let mut intent = intent.clone();
    put_into(&mut intent, row, cell)?;
    Ok(intent)
}

/// What `intent` holds at `row`, redacted, as a plan's cells read it: generated
/// addresses without `suffix`, the one `intent`'s Environment gives them.
#[must_use]
pub fn cell_at(intent: &Intent, row: &RowId, suffix: &str) -> Cell {
    cell(intent, row, suffix)
}

/// Why [`unapply`] refused.
#[derive(Debug)]
pub enum Unapplied {
    /// The row holds other than what landed, or it is a new node that holds a row
    /// that didn't land with it.
    Changed(RowId),
    /// What it puts back doesn't hold together.
    Invalid(ConfigError),
}

/// Put back what each of `landed` held before it landed in `intent`, whose generated
/// addresses end in `suffix`: [`Plan::apply`] in reverse. A new node goes whole.
///
/// # Errors
/// Returns [`Unapplied::Changed`] for a row changed since it landed, and
/// [`Unapplied::Invalid`] when what it held no longer fits.
pub fn unapply(intent: &Intent, suffix: &str, landed: &[Landed]) -> Result<Intent, Unapplied> {
    for one in landed {
        let row = &one.row;
        let changed = || Err(Unapplied::Changed(row.clone()));
        if cell(intent, row, suffix) != one.value {
            return changed();
        }
        let Some(&node) = nodes(intent)
            .get(row.lineage.as_str())
            .filter(|_| row.at == At::Node)
        else {
            continue;
        };
        let with = |at: &At| {
            landed
                .iter()
                .any(|l| l.row.lineage == row.lineage && l.row.at == *at)
        };
        if let Some(at) = cells(intent, node, suffix)
            .into_keys()
            .find(|at| !at.carried() && !with(at))
        {
            return Err(Unapplied::Changed(RowId {
                lineage: row.lineage.clone(),
                at,
            }));
        }
    }
    let mut order: Vec<&Landed> = landed.iter().collect();
    order.sort_by_key(|one| std::cmp::Reverse(landing_order(intent, &one.row)));
    let mut intent = intent.clone();
    for one in order {
        put_sealed(&mut intent, &one.row, &one.was).map_err(Unapplied::Invalid)?;
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

/// The node at `row` and where in it, with a mount named by its Volume; display only.
#[must_use]
pub fn name_of<'intent>(
    intent: &'intent Intent,
    row: &RowId,
) -> Option<(NodeRef<'intent>, String)> {
    let node = *nodes(intent).get(row.lineage.as_str())?;
    let at = match &row.at {
        At::Mount(lineage) => format!("mounts.{}", volume(intent, lineage)?.name),
        at @ (At::Node | At::Data | At::Name | At::Storage | At::Setting(_) | At::Variable(_)) => {
            at.to_string()
        }
    };
    Some((node, at))
}

/// The row a reviewed change falls in: `path` as the review compares node `lineage`
/// (`env.KEY`, `healthcheck.path`, a Volume's `storage`), from `before` to `after`. A
/// healthcheck's parts fall in `healthcheck`, a route in `routes`, a source switch in
/// the source it switches to, or drops. None for what no row carries, like a
/// Service's name.
#[must_use]
pub fn row_of_change(
    intents: &[&Intent],
    lineage: &str,
    path: &str,
    (before, after): (&Value, &Value),
) -> Option<RowId> {
    let at = if let Some(key) = path.strip_prefix("env.") {
        At::Variable(key.to_owned())
    } else if let Some(id) = path.strip_prefix("mounts.") {
        let volume = intents
            .iter()
            .flat_map(|intent| &intent.volumes)
            .find(|volume| volume.resource_id == id)?;
        At::Mount(volume.resource_lineage_id.clone())
    } else if path.starts_with("routes.") {
        At::Setting("routes")
    } else if path.split('.').next() == Some("healthcheck") {
        At::Setting("healthcheck")
    } else if path == "source" {
        let source = match at(after, "type").as_str() {
            Some("git" | "image") => after,
            _ => before,
        };
        match at(source, "type").as_str()? {
            "git" => At::Setting("source.repository"),
            "image" => At::Setting("source.image"),
            _ => return None,
        }
    } else {
        match path {
            "name" => At::Name,
            "storage" => At::Storage,
            path => At::Setting(SETTINGS.iter().find(|setting| **setting == path)?),
        }
    };
    Some(RowId {
        lineage: lineage.to_owned(),
        at,
    })
}

/// The marks among `marks` that keep `row` from syncing: its own and, for a node,
/// those on what it can't arrive without.
// ponytail: a removed node's row also counts its children's marks; split by the
// row's sides if unmarking those ever surprises.
pub fn marks_on<'m>(row: &RowId, marks: &'m BTreeSet<RowId>) -> impl Iterator<Item = &'m RowId> {
    marks.iter().filter(move |mark| {
        *mark == row || (row.at == At::Node && mark.lineage == row.lineage && mark.at.needed())
    })
}

/// Every row `intent` holds something at: each node, and each place in it.
#[must_use]
pub fn rows_of(intent: &Intent) -> Vec<RowId> {
    let mut rows = Vec::new();
    for (lineage, node) in nodes(intent) {
        rows.push(RowId::node(lineage));
        rows.extend(cells(intent, node, "").into_keys().map(|at| RowId {
            lineage: lineage.to_owned(),
            at,
        }));
    }
    rows
}

/// [`put_into`], a secret with its value.
fn put_sealed(env: &mut Intent, row: &RowId, cell: &SealedCell) -> Result<(), ConfigError> {
    put_into(env, row, &cell.redacted())?;
    if let (SealedCell::Secret(secret), At::Variable(key)) = (cell, &row.at)
        && let Some(variable) = service_mut(env, &row.lineage)
            .and_then(|service| service.variables.iter_mut().find(|v| v.key == *key))
    {
        variable.value = SavedVariableValue::Secret {
            encrypted_value: Some(secret.value.clone()),
        };
    }
    Ok(())
}

fn put_into(env: &mut Intent, row: &RowId, cell: &Cell) -> Result<(), ConfigError> {
    let invalid = |message: &str| ConfigError::at(&row.to_string(), message);
    let lineage = row.lineage.as_str();
    // Clearing anything on a node that isn't there is already done.
    let missing = || match cell {
        Cell::Absent => Ok(()),
        Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue => {
            Err(invalid("The node isn't in this configuration"))
        }
    };
    match &row.at {
        At::Node => match cell {
            Cell::Absent => {
                env.services.retain(|s| s.lineage_id != lineage);
                env.volumes.retain(|v| v.resource_lineage_id != lineage);
            }
            // ponytail: a node row only adds or removes its node; a rename doesn't move.
            Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue
                if nodes(env).contains_key(lineage) => {}
            Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue => {
                return Err(invalid("A new node arrives whole, through a Sync"));
            }
        },
        At::Data => return Err(invalid("A Volume's data never moves")),
        At::Name | At::Storage => {
            let Some(target) = env
                .volumes
                .iter_mut()
                .find(|v| v.resource_lineage_id == lineage)
            else {
                return missing();
            };
            let Cell::Value(value) = cell else {
                return Err(invalid("A Volume always has a name and storage"));
            };
            let wrong = |_| invalid("Not a value this holds");
            if row.at == At::Name {
                target.name = serde_json::from_value(value.clone()).map_err(wrong)?;
            } else {
                target.storage = serde_json::from_value(value.clone()).map_err(wrong)?;
            }
        }
        At::Mount(volume_lineage) => {
            let mounted = volume(env, volume_lineage).map(|v| v.resource_id.clone());
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let attachments = &mut service.volume_attachments;
            match (cell, mounted) {
                (Cell::Absent, Some(id)) => attachments.retain(|a| a.volume_resource_id != id),
                (Cell::Absent, None) => {}
                (Cell::Value(Value::String(path)), Some(id)) => {
                    attachments.retain(|a| a.volume_resource_id != id);
                    attachments.push(VolumeAttachment {
                        volume_resource_id: id,
                        mount_path: path.clone(),
                    });
                }
                (Cell::Value(_), None) => {
                    return Err(ConfigError::at(
                        "picks",
                        "Mounted Volume is not in the receiving configuration",
                    ));
                }
                (Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue, _) => {
                    return Err(invalid("Not a mount path"));
                }
            }
        }
        At::Variable(key) => {
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let (value, fingerprint) = match cell {
                Cell::Absent => {
                    service.variables.retain(|v| v.key != *key);
                    return Ok(());
                }
                Cell::Value(value) => {
                    plain(value).ok_or_else(|| invalid("Not a variable's value"))?
                }
                Cell::Secret { fingerprint } => (
                    SavedVariableValue::Secret {
                        encrypted_value: None,
                    },
                    fingerprint.clone(),
                ),
                Cell::SecretWithoutValue => (SavedVariableValue::SecretWithoutValue, String::new()),
            };
            match service.variables.iter_mut().find(|v| v.key == *key) {
                Some(variable) => {
                    variable.value = value;
                    variable.value_fingerprint = fingerprint;
                }
                None => service.variables.push(SavedVariableIntent {
                    id: uuid::Uuid::new_v4().to_string(),
                    key: key.clone(),
                    description: None,
                    exported: false,
                    value_fingerprint: fingerprint,
                    value,
                }),
            }
        }
        At::Setting(path) => {
            let Some(service) = service_mut(env, lineage) else {
                return missing();
            };
            let value = match cell {
                Cell::Absent => default_value(path),
                Cell::Value(value) => value.clone(),
                Cell::Secret { .. } | Cell::SecretWithoutValue => {
                    return Err(invalid("A setting is never secret"));
                }
            };
            put_setting(service, path, value)
                .map_err(|()| invalid("Not a value this setting takes"))?;
        }
    }
    Ok(())
}

/// The repository a Git source names, and the authority it is read with.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Repository {
    access: ServiceGitAccess,
    repository: String,
    repository_id: u64,
}

fn put_setting(service: &mut SavedServiceIntent, path: &str, value: Value) -> Result<(), ()> {
    fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ()> {
        serde_json::from_value(value).map_err(|_| ())
    }
    let id = service.id.clone();
    let source = &mut service.config.source;
    // A source row of another kind of source switches it, as setting it would.
    // ponytail: a root directory or branch alone never switches one; the repository or
    // image that comes with it does.
    match (path, value) {
        ("routes" | "managedHostnames", _) => return Err(()),
        ("source.image", Value::Null) => {
            if let ServiceSource::Image { .. } = source {
                *source = ServiceSource::Empty {
                    version: 1,
                    root_dir: "/".to_owned(),
                };
            }
        }
        ("source.image", value) => {
            let value: String = parse(value)?;
            match source {
                ServiceSource::Image { image, .. } => *image = value,
                ServiceSource::Empty { .. } | ServiceSource::Git { .. } => {
                    *source = ServiceSource::Image {
                        version: 1,
                        image: value,
                        credentials: ServiceImageCredentials::None,
                    };
                }
            }
        }
        ("source.credentials", value) => {
            if let ServiceSource::Image { credentials, .. } = source {
                // Each Service's credential is its own: it is bound to the Service's id.
                match (value == json!(true), &credentials) {
                    (false, _) => *credentials = ServiceImageCredentials::None,
                    (true, ServiceImageCredentials::None) => {
                        *credentials = ServiceImageCredentials::Configured { credential_id: id };
                    }
                    (true, ServiceImageCredentials::Configured { .. }) => {}
                }
            }
        }
        ("source.repository", Value::Null) => {
            if let ServiceSource::Git { root_dir, .. } = source {
                *source = ServiceSource::Empty {
                    version: 1,
                    root_dir: std::mem::take(root_dir),
                };
            }
        }
        ("source.repository", value) => {
            let named: Repository = parse(value)?;
            match source {
                ServiceSource::Git {
                    repository,
                    repository_id,
                    access,
                    ..
                } => {
                    *repository = named.repository;
                    *repository_id = named.repository_id;
                    *access = named.access;
                }
                ServiceSource::Empty { .. } | ServiceSource::Image { .. } => {
                    let root_dir = match source {
                        ServiceSource::Empty { root_dir, .. } => std::mem::take(root_dir),
                        ServiceSource::Git { .. } | ServiceSource::Image { .. } => "/".to_owned(),
                    };
                    *source = ServiceSource::Git {
                        version: 2,
                        repository: named.repository,
                        repository_id: named.repository_id,
                        access: named.access,
                        root_dir,
                        branch: ServiceGitBranch::Disconnected {
                            previous_name: None,
                        },
                    };
                }
            }
        }
        ("source.rootDir", value) => {
            if let ServiceSource::Git { root_dir, .. } | ServiceSource::Empty { root_dir, .. } =
                source
                && !value.is_null()
            {
                *root_dir = parse(value)?;
            }
        }
        ("source.branch", value) => {
            if let ServiceSource::Git { branch, .. } = source
                && !value.is_null()
            {
                *branch = parse(value)?;
            }
        }
        (path, value) => {
            let mut config = json!(service.config);
            write(&mut config, path, value);
            service.config = parse(config)?;
        }
    }
    Ok(())
}

/// Set `config`'s field at `path`, one level deep at most.
fn write(config: &mut Value, path: &str, value: Value) {
    let (object, key) = match path.split_once('.') {
        Some((parent, key)) => (config.get_mut(parent), key),
        None => (Some(config), path),
    };
    if let Some(object) = object.and_then(Value::as_object_mut) {
        object.insert(key.to_owned(), value);
    }
}

/// A plain variable's cell back as its value and fingerprint.
fn plain(cell: &Value) -> Option<(SavedVariableValue, String)> {
    let mut value = cell.clone();
    let fingerprint = value.as_object_mut()?.remove("fingerprint")?;
    let value: SavedVariableValue = serde_json::from_value(value).ok()?;
    match value {
        SavedVariableValue::Literal { .. } | SavedVariableValue::Template { .. } => {
            Some((value, fingerprint.as_str()?.to_owned()))
        }
        SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue => None,
    }
}

fn nodes(env: &Intent) -> BTreeMap<&str, NodeRef<'_>> {
    env.services
        .iter()
        .map(|s| (s.lineage_id.as_str(), NodeRef::Service(s)))
        .chain(
            env.volumes
                .iter()
                .map(|v| (v.resource_lineage_id.as_str(), NodeRef::Volume(v))),
        )
        .collect()
}

fn service<'intent>(env: &'intent Intent, lineage: &str) -> Option<&'intent SavedServiceIntent> {
    env.services.iter().find(|s| s.lineage_id == lineage)
}

fn service_mut<'intent>(
    env: &'intent mut Intent,
    lineage: &str,
) -> Option<&'intent mut SavedServiceIntent> {
    env.services.iter_mut().find(|s| s.lineage_id == lineage)
}

fn volume<'intent>(env: &'intent Intent, lineage: &str) -> Option<&'intent SavedVolumeIntent> {
    env.volumes
        .iter()
        .find(|v| v.resource_lineage_id == lineage)
}

/// A node's cell is what it arrives with as a new node: its name and the settings it
/// carries (no custom domains), each by where it is.
fn node_cell(env: &Intent, lineage: &str, suffix: &str) -> Cell {
    let Some(&node) = nodes(env).get(lineage) else {
        return Cell::Absent;
    };
    let mut cell = serde_json::Map::new();
    if let NodeRef::Service(service) = node {
        cell.insert("name".to_owned(), json!(service.slug));
    }
    for (at, value) in cells(env, node, suffix) {
        if let (true, Cell::Value(value)) = (at.carried() && at != At::Setting("routes"), value) {
            cell.insert(at.to_string(), value);
        }
    }
    Cell::Value(Value::Object(cell))
}

/// `env`'s cell at `row`, sealed or redacted.
fn cell(env: &Intent, row: &RowId, suffix: &str) -> Cell {
    match (&row.at, nodes(env).get(row.lineage.as_str())) {
        (At::Node | At::Data, _) => node_cell(env, &row.lineage, suffix),
        (_, Some(node)) => cells(env, *node, suffix)
            .remove(&row.at)
            .unwrap_or(Cell::Absent),
        (_, None) => Cell::Absent,
    }
}

/// [`cell`], a secret with its value where `env` holds one.
fn sealed_cell(env: &Intent, row: &RowId, suffix: &str) -> SealedCell {
    let cell = cell(env, row, suffix);
    let (Cell::Secret { fingerprint }, At::Variable(key)) = (&cell, &row.at) else {
        return SealedCell::Cell(cell);
    };
    let value = service(env, &row.lineage)
        .and_then(|service| service.variables.iter().find(|v| v.key == *key))
        .and_then(|variable| match &variable.value {
            SavedVariableValue::Secret { encrypted_value } => encrypted_value.clone(),
            SavedVariableValue::Literal { .. }
            | SavedVariableValue::Template { .. }
            | SavedVariableValue::SecretWithoutValue => None,
        });
    match value {
        Some(value) => SealedCell::Secret(SealedSecret {
            fingerprint: fingerprint.clone(),
            value,
        }),
        None => SealedCell::Cell(cell),
    }
}

/// Every place in `node` that holds something, normalized so copies compare with their
/// originals: generated addresses without `suffix`, credentials and custom domains by
/// presence and hostname, mounts by Volume lineage, secrets by fingerprint.
fn cells(env: &Intent, node: NodeRef, suffix: &str) -> BTreeMap<At, Cell> {
    let service = match node {
        NodeRef::Volume(volume) => {
            return BTreeMap::from([
                (At::Name, Cell::Value(json!(volume.name))),
                (At::Storage, Cell::Value(json!(volume.storage))),
            ]);
        }
        NodeRef::Service(service) => service,
    };
    let config = json!(service.config);
    let mut cells = BTreeMap::new();
    for path in SETTINGS {
        let value = match *path {
            // Repository authority (id and access) moves with the repository it names.
            "source.repository" => {
                let source = at(&config, "source");
                match source.get("repository") {
                    Some(_) => json!({"access": source["access"], "repository": source["repository"], "repositoryId": source["repositoryId"]}),
                    None => Value::Null,
                }
            }
            "source.credentials" => match at(&config, "source.credentials.type").as_str() {
                Some("configured") => json!(true),
                _ => Value::Null,
            },
            "routes" => {
                let mut domains: Vec<_> = service.config.routes.iter().map(|r| &r.hostname).collect();
                domains.sort();
                json!(domains)
            }
            "managedHostnames" => json!(
                service
                    .config
                    .managed_hostnames
                    .iter()
                    .map(|h| json!({"prefix": h.prefix.strip_suffix(suffix).unwrap_or(&h.prefix), "targetPort": h.target_port}))
                    .collect::<Vec<_>>()
            ),
            path => at(&config, path).clone(),
        };
        if !(value.is_null() || value == default_value(path) || value == json!([])) {
            cells.insert(At::Setting(path), Cell::Value(value));
        }
    }
    for attachment in &service.volume_attachments {
        if let Some(volume) = env
            .volumes
            .iter()
            .find(|v| v.resource_id == attachment.volume_resource_id)
        {
            cells.insert(
                At::Mount(volume.resource_lineage_id.clone()),
                Cell::Value(json!(attachment.mount_path)),
            );
        }
    }
    for variable in &service.variables {
        cells.insert(At::Variable(variable.key.clone()), variable_cell(variable));
    }
    cells
}

fn variable_cell(variable: &SavedVariableIntent) -> Cell {
    match &variable.value {
        SavedVariableValue::Literal { .. } | SavedVariableValue::Template { .. } => {
            let mut value = json!(variable.value);
            if let Some(object) = value.as_object_mut() {
                object.insert("fingerprint".to_owned(), json!(variable.value_fingerprint));
            }
            Cell::Value(value)
        }
        SavedVariableValue::Secret { .. } => Cell::Secret {
            fingerprint: variable.value_fingerprint.clone(),
        },
        SavedVariableValue::SecretWithoutValue => Cell::SecretWithoutValue,
    }
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
