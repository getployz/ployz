//! Compare two authored configurations against the base they share, keyed by lineage, and move
//! the picked rows from one into the other.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use ts_rs::TS;

use super::service_changes::{FIELDS, at};
use super::{
    ConfigError, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, SavedVolumeIntent, ServiceConfig, ServiceImageCredentials, ServiceSource,
    VolumeAttachment, parse_environment_intent, redact_environment_intent, restore_service_setting,
};

/// The sides of one move: changes flow from `from` into `into`, judged against `base`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchChangesInput {
    /// Null when creating: nothing is shared yet.
    #[ts(as = "Option<SavedEnvironmentIntent>")]
    pub base: Option<Value>,
    #[ts(as = "SavedEnvironmentIntent")]
    pub from: Value,
    #[ts(as = "SavedEnvironmentIntent")]
    pub into: Value,
    /// The Parent, when the caller can offer its values as a variable choice.
    #[serde(default)]
    #[ts(optional, as = "Option<SavedEnvironmentIntent>")]
    pub parent: Option<Value>,
    /// Lineages `into` may use live.
    pub provided: Vec<String>,
    pub hostnames: BranchHostnames,
    pub from_kept: bool,
    /// Absent compares only; present moves the picked rows.
    #[serde(default)]
    #[ts(optional)]
    pub picks: Option<Vec<BranchPick>>,
}

/// One chosen move row. Only a variable row takes a choice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchPick {
    pub key: String,
    #[serde(default)]
    #[ts(optional)]
    pub choice: Option<BranchPickChoice>,
}

/// How a picked variable lands. Only `new` carries a value, and it is never reviewed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "option", rename_all = "snake_case", deny_unknown_fields)]
pub enum BranchPickChoice {
    From,
    Parent,
    /// Without a value the variable stays out of `next`, so the browser can still review.
    New {
        #[serde(default)]
        #[ts(optional)]
        value: Option<BranchNewValue>,
    },
    LeaveOut,
}

impl BranchPickChoice {
    const fn option(&self) -> BranchOption {
        match self {
            Self::From => BranchOption::From,
            Self::Parent => BranchOption::Parent,
            Self::New { .. } => BranchOption::New,
            Self::LeaveOut => BranchOption::LeaveOut,
        }
    }
}

/// A caller-supplied variable value; core never encrypts or fingerprints. A secret's is sealed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchNewValue {
    pub value: SavedVariableValue,
    pub value_fingerprint: String,
}

/// Each side's generated-address suffix, appended to managed hostname prefixes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchHostnames {
    pub from: String,
    pub into: String,
}

/// The rows between two configurations, the receiver after moving, and the advanced base.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChanges {
    pub rows: Vec<BranchRow>,
    pub next: SavedEnvironmentIntent,
    pub base: Option<SavedEnvironmentIntent>,
    /// Canonical, id-free rendering of the rows and picks; callers hash it.
    pub review: String,
}

/// One setting of one lineage. Values are redacted: secrets carry only fingerprints.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchRow {
    /// `<lineageId>:<path>`.
    #[ts(type = "string")]
    pub key: BranchRowKey,
    #[serde(flatten)]
    #[ts(flatten)]
    pub role: BranchRole,
    pub base: Value,
    pub from: Value,
    pub into: Value,
}

/// A row's lineage and setting path; it serializes as `<lineageId>:<path>`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BranchRowKey {
    lineage: String,
    path: RowPath,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RowPath {
    Node,
    Data,
    Name,
    Mount(String),
    Variable(String),
    Setting(&'static str),
}

impl fmt::Display for BranchRowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lineage = &self.lineage;
        match &self.path {
            RowPath::Node => write!(f, "{lineage}:node"),
            RowPath::Data => write!(f, "{lineage}:data"),
            RowPath::Name => write!(f, "{lineage}:name"),
            RowPath::Mount(volume) => write!(f, "{lineage}:mounts.{volume}"),
            RowPath::Variable(key) => write!(f, "{lineage}:variables.{key}"),
            RowPath::Setting(path) => write!(f, "{lineage}:{path}"),
        }
    }
}

impl Serialize for BranchRowKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A row either moves (possibly over a conflicting change) or is meant to differ.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(
    tag = "role",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BranchRole {
    Move {
        /// `into` also changed since `base`; shown into → from.
        conflict: bool,
        /// Present only on variable rows.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        choice: Option<BranchChoice>,
    },
    Differ {
        why: BranchReason,
    },
}

/// Why a row never moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchReason {
    Live,
    LeftOut,
    Sizing,
    CustomDomain,
    GeneratedAddress,
    GitBranch,
    Data,
}

/// The ways a moving variable can land.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChoice {
    pub default: BranchOption,
    pub options: Vec<BranchOption>,
    pub secret: bool,
}

/// A variable's source when it moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchOption {
    From,
    Parent,
    New,
    LeaveOut,
}

/// Compare `from` and `into` against `base`; without picks `next` is `into` and `base` is unchanged.
///
/// # Errors
/// Returns ConfigError when any supplied configuration is invalid, or a pick is unknown, meant to
/// differ, offers no such choice, lacks a required value, or cannot produce a valid `next`.
pub fn branch_changes(input: BranchChangesInput) -> Result<BranchChanges, ConfigError> {
    let base = input.base.map(parse_environment_intent).transpose()?;
    let from = parse_environment_intent(input.from)?;
    let into = parse_environment_intent(input.into)?;
    let parent = input.parent.map(parse_environment_intent).transpose()?;
    let comparison = Comparison {
        base: base.as_ref(),
        from: &from,
        into: &into,
        parent: parent.as_ref(),
        provided: input.provided.iter().map(String::as_str).collect(),
        hostnames: &input.hostnames,
        from_kept: input.from_kept,
    };
    let rows = comparison.rows();
    let picks = input.picks.unwrap_or_default();
    let (next, base) = if picks.is_empty() {
        (into.clone(), base.clone())
    } else {
        comparison.apply(&rows, &picks)?
    };
    let review = review(&rows, &picks);
    Ok(BranchChanges {
        rows,
        next,
        base,
        review,
    })
}

/// The one canonical review: rows plus each pick's key and option, never a supplied value.
fn review(rows: &[BranchRow], picks: &[BranchPick]) -> String {
    let mut picks: Vec<_> = picks
        .iter()
        .map(|pick| json!({"choice": pick.choice.as_ref().map(BranchPickChoice::option), "key": pick.key}))
        .collect();
    picks.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    serde_json::to_string(&json!({"picks": picks, "rows": rows})).expect("rows are JSON")
}

#[derive(Clone, Copy)]
enum Node<'a> {
    Service(&'a SavedServiceIntent),
    Volume(&'a SavedVolumeIntent),
}

/// The three sides of a move and the caller's context for it.
struct Comparison<'a> {
    base: Option<&'a SavedEnvironmentIntent>,
    from: &'a SavedEnvironmentIntent,
    into: &'a SavedEnvironmentIntent,
    parent: Option<&'a SavedEnvironmentIntent>,
    provided: BTreeSet<&'a str>,
    hostnames: &'a BranchHostnames,
    from_kept: bool,
}

impl Comparison<'_> {
    fn rows(&self) -> Vec<BranchRow> {
        let base = self.base.map(nodes).unwrap_or_default();
        let from = nodes(self.from);
        let into = nodes(self.into);
        let mut rows = Vec::new();
        for lineage in from.keys().chain(into.keys()).collect::<BTreeSet<_>>() {
            let in_base = base.get(lineage).copied();
            match (from.get(lineage).copied(), into.get(lineage).copied()) {
                (Some(from_node), Some(into_node)) => {
                    self.settings_rows(lineage, in_base, from_node, into_node, &mut rows);
                }
                (Some(from_node), None) => {
                    let why = if self.provided.contains(lineage) {
                        BranchReason::Live
                    } else if in_base.is_some() {
                        BranchReason::LeftOut
                    } else {
                        self.introduction_rows(lineage, from_node, &mut rows);
                        continue;
                    };
                    rows.push(differ_row(
                        lineage,
                        RowPath::Node,
                        why,
                        [in_base, Some(from_node), None],
                    ));
                }
                (None, Some(into_node)) => {
                    let why = if uses(self.from, lineage) {
                        BranchReason::Live
                    } else if in_base.is_some() {
                        BranchReason::LeftOut
                    } else {
                        continue;
                    };
                    rows.push(differ_row(
                        lineage,
                        RowPath::Node,
                        why,
                        [in_base, None, Some(into_node)],
                    ));
                }
                (None, None) => {}
            }
        }
        rows
    }

    fn settings_rows(
        &self,
        lineage: &str,
        base: Option<Node>,
        from: Node,
        into: Node,
        rows: &mut Vec<BranchRow>,
    ) {
        let base_values = base
            .zip(self.base)
            .map(|(node, env)| settings(env, node, &self.hostnames.into))
            .unwrap_or_default();
        let from_values = settings(self.from, from, &self.hostnames.from);
        let into_values = settings(self.into, into, &self.hostnames.into);
        let paths: BTreeSet<_> = from_values.keys().chain(into_values.keys()).collect();
        for path in paths {
            let value =
                |values: &BTreeMap<RowPath, Value>| values.get(path).cloned().unwrap_or_default();
            let (base_value, from_value, into_value) = (
                value(&base_values),
                value(&from_values),
                value(&into_values),
            );
            let role = if let Some(why) = reason(path) {
                if from_value == into_value {
                    continue;
                }
                BranchRole::Differ { why }
            } else {
                // Nothing is deleted: a value `from` lacks never moves.
                if from_value.is_null() || from_value == base_value || from_value == into_value {
                    continue;
                }
                let choice = match path {
                    RowPath::Variable(key) => {
                        let secret = *at(&from_value, "kind") == "secret";
                        // A secret moves only into a receiver that lacks it.
                        if secret && !into_value.is_null() {
                            continue;
                        }
                        Some(self.choice(lineage, key, secret))
                    }
                    RowPath::Node
                    | RowPath::Data
                    | RowPath::Name
                    | RowPath::Mount(_)
                    | RowPath::Setting(_) => None,
                };
                BranchRole::Move {
                    conflict: into_value != base_value,
                    choice,
                }
            };
            rows.push(BranchRow {
                key: key(lineage, path.clone()),
                role,
                base: base_value,
                from: from_value,
                into: into_value,
            });
        }
        if let (Node::Volume(_), Node::Volume(_)) = (from, into) {
            rows.push(differ_row(
                lineage,
                RowPath::Data,
                BranchReason::Data,
                [base, Some(from), Some(into)],
            ));
        }
    }

    /// A node `into` lacks and doesn't use live moves in whole, with a row per variable.
    fn introduction_rows(&self, lineage: &str, node: Node, rows: &mut Vec<BranchRow>) {
        rows.push(BranchRow {
            key: key(lineage, RowPath::Node),
            role: BranchRole::Move {
                conflict: false,
                choice: None,
            },
            base: Value::Null,
            from: node_value(node),
            into: Value::Null,
        });
        let Node::Service(service) = node else {
            return;
        };
        for variable in &service.variables {
            let secret = matches!(variable.value, SavedVariableValue::Secret { .. });
            let mut choice = self.choice(lineage, &variable.key, secret);
            // A Branch starts with its Parent's values, secrets included.
            if self.base.is_none() {
                choice.default = BranchOption::From;
            }
            rows.push(BranchRow {
                key: key(lineage, RowPath::Variable(variable.key.clone())),
                role: BranchRole::Move {
                    conflict: false,
                    choice: Some(choice),
                },
                base: Value::Null,
                from: variable_value(variable),
                into: Value::Null,
            });
        }
    }

    fn choice(&self, lineage: &str, key: &str, secret: bool) -> BranchChoice {
        let parent_has = self
            .parent
            .and_then(|parent| service(parent, lineage))
            .is_some_and(|service| service.variables.iter().any(|v| v.key == key));
        let mut options = vec![BranchOption::From];
        if parent_has {
            options.push(BranchOption::Parent);
        }
        options.extend([BranchOption::New, BranchOption::LeaveOut]);
        let default = match (secret, self.from_kept) {
            (false, _) => BranchOption::From,
            (true, false) => BranchOption::New,
            (true, true) => BranchOption::LeaveOut,
        };
        BranchChoice {
            default,
            options,
            secret,
        }
    }

    /// Move the picked rows into `next` and advance `base` by exactly those rows.
    /// Creating (no `base`) returns `from` minus the lineages `into` uses live as the new base.
    fn apply(
        &self,
        rows: &[BranchRow],
        picks: &[BranchPick],
    ) -> Result<(SavedEnvironmentIntent, Option<SavedEnvironmentIntent>), ConfigError> {
        let picked = self.admit_picks(rows, picks)?;
        let mut next = self.into.clone();
        let mut base = self.base.cloned();
        for (row, choice) in &picked {
            self.land(row, *choice, &mut next, base.as_mut())?;
        }
        self.land_defaults(rows, &picked, &mut next);
        let next = parse_environment_intent(json!(next))?;
        let base = base.unwrap_or_else(|| {
            let mut base = self.from.clone();
            base.services
                .retain(|s| !self.provided.contains(s.lineage_id.as_str()));
            base
        });
        Ok((next, Some(redact_environment_intent(base))))
    }

    /// Match picks to rows and refuse any that can't land; order introductions first
    /// (Volumes, then services) so settings and variables find their node.
    fn admit_picks<'r>(
        &self,
        rows: &'r [BranchRow],
        picks: &'r [BranchPick],
    ) -> Result<Vec<(&'r BranchRow, Option<&'r BranchPickChoice>)>, ConfigError> {
        let mut seen = BTreeSet::new();
        let mut picked = Vec::new();
        for pick in picks {
            if !seen.insert(pick.key.as_str()) {
                return Err(ConfigError::at("picks.key", "Each change is picked once"));
            }
            let row = rows
                .iter()
                .find(|row| row.key.to_string() == pick.key)
                .ok_or_else(|| ConfigError::at("picks.key", "Unknown change"))?;
            let BranchRole::Move {
                choice: offered, ..
            } = &row.role
            else {
                return Err(ConfigError::at("picks.key", "Change is meant to differ"));
            };
            match (offered, &pick.choice) {
                (None, None) => {}
                (Some(offered), Some(chosen)) if offered.options.contains(&chosen.option()) => {
                    admit_new_value(offered, chosen)?;
                }
                _ => return Err(ConfigError::at("picks.choice", "Choice is not offered")),
            }
            picked.push((row, pick.choice.as_ref()));
        }
        let introduced: BTreeSet<_> = picked
            .iter()
            .filter(|(row, _)| row.key.path == RowPath::Node)
            .map(|(row, _)| row.key.lineage.as_str())
            .collect();
        if picked.iter().any(|(row, _)| {
            !introduced.contains(row.key.lineage.as_str())
                && !nodes(self.into).contains_key(row.key.lineage.as_str())
        }) {
            return Err(ConfigError::at(
                "picks.key",
                "Pick the node to introduce its variables",
            ));
        }
        picked.sort_by_key(|(row, _)| match row.key.path {
            RowPath::Node if volume(self.from, &row.key.lineage).is_some() => 0,
            RowPath::Node => 1,
            RowPath::Data
            | RowPath::Name
            | RowPath::Mount(_)
            | RowPath::Variable(_)
            | RowPath::Setting(_) => 2,
        });
        Ok(picked)
    }

    fn land(
        &self,
        row: &BranchRow,
        choice: Option<&BranchPickChoice>,
        next: &mut SavedEnvironmentIntent,
        base: Option<&mut SavedEnvironmentIntent>,
    ) -> Result<(), ConfigError> {
        let lineage = row.key.lineage.as_str();
        match &row.key.path {
            RowPath::Node => return self.introduce(next, base, lineage),
            RowPath::Variable(key) => {
                let choice = choice.expect("variable picks are admitted with a choice");
                let new_value = match choice {
                    BranchPickChoice::New { value } => value.as_ref(),
                    BranchPickChoice::From
                    | BranchPickChoice::Parent
                    | BranchPickChoice::LeaveOut => None,
                };
                if let Some(variable) =
                    self.chosen_variable(lineage, key, choice.option(), new_value)
                {
                    put_variable(next, lineage, variable);
                }
                // A secret still waiting for its value stays proposed.
                let waiting = matches!(choice, BranchPickChoice::New { value: None });
                if let (Some(base), false) = (base, waiting) {
                    adopt_node(base, self.into, lineage, &row.key.path);
                    put_variable(base, lineage, variable(self.from, lineage, key));
                }
            }
            path @ (RowPath::Data | RowPath::Name | RowPath::Mount(_) | RowPath::Setting(_)) => {
                move_setting(next, self.from, lineage, path)?;
                if let Some(base) = base {
                    adopt_node(base, self.into, lineage, path);
                    move_setting(base, self.from, lineage, path)?;
                }
            }
        }
        Ok(())
    }

    /// An introduced node's unpicked variables land in `next` by their default choice; `base`
    /// doesn't record them, so they stay proposed until picked.
    fn land_defaults(
        &self,
        rows: &[BranchRow],
        picked: &[(&BranchRow, Option<&BranchPickChoice>)],
        next: &mut SavedEnvironmentIntent,
    ) {
        let introduced: BTreeSet<_> = picked
            .iter()
            .filter(|(row, _)| row.key.path == RowPath::Node)
            .map(|(row, _)| row.key.lineage.as_str())
            .collect();
        for row in rows {
            let (
                RowPath::Variable(key),
                BranchRole::Move {
                    choice: Some(choice),
                    ..
                },
            ) = (&row.key.path, &row.role)
            else {
                continue;
            };
            let lineage = row.key.lineage.as_str();
            if !introduced.contains(lineage) || picked.iter().any(|(p, _)| p.key == row.key) {
                continue;
            }
            if let Some(variable) = self.chosen_variable(lineage, key, choice.default, None) {
                put_variable(next, lineage, variable);
            }
        }
    }

    /// Add `from`'s node to `next` with fresh ids, no custom domains, `into`'s generated-address
    /// naming, a rebound credential, and mounts on `into`'s Volumes; its variables land separately.
    fn introduce(
        &self,
        next: &mut SavedEnvironmentIntent,
        base: Option<&mut SavedEnvironmentIntent>,
        lineage: &str,
    ) -> Result<(), ConfigError> {
        let clash = || ConfigError::at("picks.key", "Name or private address is already used");
        if let Some(source) = volume(self.from, lineage) {
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
        let source = service(self.from, lineage).expect("node picks name a node `from` has");
        if next
            .services
            .iter()
            .any(|s| s.slug == source.slug || s.config.private_dns == source.config.private_dns)
        {
            return Err(clash());
        }
        let mut copy = SavedServiceIntent {
            id: uuid::Uuid::new_v4().to_string(),
            variables: Vec::new(),
            volume_attachments: Vec::new(),
            ..source.clone()
        };
        copy.config.routes.clear();
        for hostname in &mut copy.config.managed_hostnames {
            let prefix = hostname
                .prefix
                .strip_suffix(self.hostnames.from.as_str())
                .unwrap_or(&hostname.prefix);
            hostname.prefix = format!("{prefix}{}", self.hostnames.into);
        }
        rebind_credential(&mut copy);
        copy.volume_attachments = source
            .volume_attachments
            .iter()
            .map(|a| attachment_in(self.from, a, next).ok_or_else(missing_volume))
            .collect::<Result<_, _>>()?;
        if let Some(base) = base {
            // TODO: a mount on a Volume that base predates is dropped from base, because base has
            // no id for it; the next compare offers that mount again until someone picks it.
            let attachments = source
                .volume_attachments
                .iter()
                .filter_map(|a| attachment_in(self.from, a, base))
                .collect();
            base.services.push(SavedServiceIntent {
                volume_attachments: attachments,
                ..copy.clone()
            });
        }
        next.services.push(copy);
        Ok(())
    }

    fn chosen_variable(
        &self,
        lineage: &str,
        key: &str,
        option: BranchOption,
        new_value: Option<&BranchNewValue>,
    ) -> Option<SavedVariableIntent> {
        match option {
            BranchOption::From => Some(variable(self.from, lineage, key)),
            BranchOption::Parent => self.parent.map(|parent| variable(parent, lineage, key)),
            BranchOption::New => new_value.map(|new| SavedVariableIntent {
                value: new.value.clone(),
                value_fingerprint: new.value_fingerprint.clone(),
                ..variable(self.from, lineage, key)
            }),
            BranchOption::LeaveOut => None,
        }
    }
}

/// A plain `new` needs its value; a secret's value must be sealed material, never plaintext.
fn admit_new_value(offered: &BranchChoice, chosen: &BranchPickChoice) -> Result<(), ConfigError> {
    let BranchPickChoice::New { value } = chosen else {
        return Ok(());
    };
    let sealed = |value: &BranchNewValue| {
        matches!(
            value.value,
            SavedVariableValue::Secret {
                encrypted_value: Some(_)
            }
        )
    };
    match (offered.secret, value) {
        (false, None) => Err(ConfigError::at("picks.value", "A new value is required")),
        (false, Some(value)) if matches!(value.value, SavedVariableValue::Secret { .. }) => Err(
            ConfigError::at("picks.value", "A plain variable takes a plain value"),
        ),
        (true, Some(value)) if !sealed(value) => Err(ConfigError::at(
            "picks.value",
            "A secret's new value must be sealed",
        )),
        _ => Ok(()),
    }
}

fn key(lineage: &str, path: RowPath) -> BranchRowKey {
    BranchRowKey {
        lineage: lineage.to_owned(),
        path,
    }
}

fn nodes(env: &SavedEnvironmentIntent) -> BTreeMap<&str, Node<'_>> {
    env.services
        .iter()
        .map(|s| (s.lineage_id.as_str(), Node::Service(s)))
        .chain(
            env.volumes
                .iter()
                .map(|v| (v.resource_lineage_id.as_str(), Node::Volume(v))),
        )
        .collect()
}

fn service<'a>(env: &'a SavedEnvironmentIntent, lineage: &str) -> Option<&'a SavedServiceIntent> {
    env.services.iter().find(|s| s.lineage_id == lineage)
}

fn volume<'a>(env: &'a SavedEnvironmentIntent, lineage: &str) -> Option<&'a SavedVolumeIntent> {
    env.volumes
        .iter()
        .find(|v| v.resource_lineage_id == lineage)
}

fn service_mut<'a>(
    env: &'a mut SavedEnvironmentIntent,
    lineage: &str,
) -> &'a mut SavedServiceIntent {
    env.services
        .iter_mut()
        .find(|s| s.lineage_id == lineage)
        .expect("move rows are on nodes both sides have")
}

/// Settings each Environment owns; they are shown as meant to differ, never moved.
fn reason(path: &RowPath) -> Option<BranchReason> {
    let RowPath::Setting(path) = path else {
        return None;
    };
    Some(match *path {
        "replicas" | "cpuLimit" | "memLimit" => BranchReason::Sizing,
        "routes" => BranchReason::CustomDomain,
        "managedHostnames" => BranchReason::GeneratedAddress,
        "source.branch" => BranchReason::GitBranch,
        _ => return None,
    })
}

/// A node's comparable settings by path, normalized so copies compare with their originals.
fn settings(env: &SavedEnvironmentIntent, node: Node, suffix: &str) -> BTreeMap<RowPath, Value> {
    let service = match node {
        Node::Volume(volume) => return BTreeMap::from([(RowPath::Name, json!(volume.name))]),
        Node::Service(service) => service,
    };
    let config = json!(service.config);
    let mut values: BTreeMap<RowPath, Value> = FIELDS
        .iter()
        .map(|path| (RowPath::Setting(path), at(&config, path).clone()))
        .collect();
    // Repository authority (id and access) moves with the repository it names.
    let source = at(&config, "source");
    if !at(source, "repository").is_null() {
        values.insert(
            RowPath::Setting("source.repository"),
            json!({"access": source["access"], "repository": source["repository"], "repositoryId": source["repositoryId"]}),
        );
    }
    // Registry credentials compare by presence; dropping one is a removal, so it never moves.
    let credentials = at(&config, "source.credentials.type");
    values.insert(
        RowPath::Setting("source.credentials"),
        if credentials == "configured" {
            json!(true)
        } else {
            Value::Null
        },
    );
    let mut domains: Vec<_> = service.config.routes.iter().map(|r| &r.hostname).collect();
    domains.sort();
    values.insert(RowPath::Setting("routes"), json!(domains));
    let generated: Vec<_> = service
        .config
        .managed_hostnames
        .iter()
        .map(|h| {
            let prefix = h.prefix.strip_suffix(suffix).unwrap_or(&h.prefix);
            json!({"prefix": prefix, "targetPort": h.target_port})
        })
        .collect();
    values.insert(RowPath::Setting("managedHostnames"), json!(generated));
    // Mounts compare by Volume lineage and mount path.
    for attachment in &service.volume_attachments {
        if let Some(volume) = env
            .volumes
            .iter()
            .find(|v| v.resource_id == attachment.volume_resource_id)
        {
            values.insert(
                RowPath::Mount(volume.resource_lineage_id.clone()),
                json!(attachment.mount_path),
            );
        }
    }
    for variable in &service.variables {
        values.insert(
            RowPath::Variable(variable.key.clone()),
            variable_value(variable),
        );
    }
    values
}

/// Secrets compare by value fingerprint, never by variable id or sealed material.
fn variable_value(variable: &SavedVariableIntent) -> Value {
    match &variable.value {
        SavedVariableValue::Literal { value } => json!({"kind": "literal", "value": value}),
        SavedVariableValue::Template { parts } => json!({"kind": "template", "parts": parts}),
        SavedVariableValue::Secret { .. } => {
            json!({"fingerprint": variable.value_fingerprint, "kind": "secret"})
        }
    }
}

fn node_value(node: Node) -> Value {
    match node {
        Node::Service(service) => json!(service.slug),
        Node::Volume(volume) => json!(volume.name),
    }
}

/// A meant-to-differ row whose sides are node names: `[base, from, into]`.
fn differ_row(
    lineage: &str,
    path: RowPath,
    why: BranchReason,
    [base, from, into]: [Option<Node>; 3],
) -> BranchRow {
    let value = |node: Option<Node>| node.map_or(Value::Null, node_value);
    BranchRow {
        key: key(lineage, path),
        role: BranchRole::Differ { why },
        base: value(base),
        from: value(from),
        into: value(into),
    }
}

/// Whether `env` references a lineage it does not own, i.e. uses it live.
fn uses(env: &SavedEnvironmentIntent, lineage: &str) -> bool {
    env.services
        .iter()
        .flat_map(|s| &s.variables)
        .any(|v| v.value.referenced_lineages().any(|used| used == lineage))
}

/// The variable a row was computed from; offered choices guarantee it exists.
fn variable(env: &SavedEnvironmentIntent, lineage: &str, key: &str) -> SavedVariableIntent {
    service(env, lineage)
        .and_then(|service| service.variables.iter().find(|v| v.key == key))
        .expect("offered variables exist")
        .clone()
}

/// Set a variable by key, keeping the receiver's id or minting a fresh one.
fn put_variable(
    env: &mut SavedEnvironmentIntent,
    lineage: &str,
    mut variable: SavedVariableIntent,
) {
    let variables = &mut service_mut(env, lineage).variables;
    if let Some(existing) = variables.iter_mut().find(|v| v.key == variable.key) {
        variable.id = std::mem::take(&mut existing.id);
        *existing = variable;
    } else {
        variable.id = uuid::Uuid::new_v4().to_string();
        variables.push(variable);
    }
}

fn missing_volume() -> ConfigError {
    ConfigError::at(
        "picks.key",
        "Mounted Volume is not in the receiving configuration",
    )
}

/// Give `env` the setting at `path` that `from` has.
fn move_setting(
    env: &mut SavedEnvironmentIntent,
    from: &SavedEnvironmentIntent,
    lineage: &str,
    path: &RowPath,
) -> Result<(), ConfigError> {
    match path {
        RowPath::Name => {
            let name = volume(from, lineage)
                .expect("name rows are Volumes")
                .name
                .clone();
            if let Some(target) = env
                .volumes
                .iter_mut()
                .find(|v| v.resource_lineage_id == lineage)
            {
                target.name = name;
            }
        }
        RowPath::Mount(volume_lineage) => {
            let from_id = &volume(from, volume_lineage)
                .expect("mount rows name an authored Volume")
                .resource_id;
            let attachment = service(from, lineage)
                .and_then(|s| {
                    s.volume_attachments
                        .iter()
                        .find(|a| &a.volume_resource_id == from_id)
                })
                .expect("mount rows come from an attachment");
            let moved = attachment_in(from, attachment, env).ok_or_else(missing_volume)?;
            let attachments = &mut service_mut(env, lineage).volume_attachments;
            attachments.retain(|a| a.volume_resource_id != moved.volume_resource_id);
            attachments.push(moved);
        }
        RowPath::Setting(path) => {
            let target = service_mut(env, lineage);
            let from_config = &service(from, lineage)
                .expect("setting rows are services")
                .config;
            target.config = restore_service_setting(
                ServiceConfig::from(target.config.clone()),
                &ServiceConfig::from(from_config.clone()),
                path,
            )?
            .settings;
            if path.starts_with("source") {
                rebind_credential(target);
            }
        }
        RowPath::Node | RowPath::Data | RowPath::Variable(_) => {
            unreachable!("nodes and variables land elsewhere; data rows are refused")
        }
    }
    Ok(())
}

/// Credentials compare by presence, and each node's credential is its own: bind it to `service`.
fn rebind_credential(service: &mut SavedServiceIntent) {
    if let ServiceSource::Image {
        credentials: ServiceImageCredentials::Configured { credential_id },
        ..
    } = &mut service.config.source
    {
        credential_id.clone_from(&service.id);
    }
}

/// `attachment` re-pointed at `to`'s Volume of the same lineage, if `to` has one.
fn attachment_in(
    from: &SavedEnvironmentIntent,
    attachment: &VolumeAttachment,
    to: &SavedEnvironmentIntent,
) -> Option<VolumeAttachment> {
    let lineage = &from
        .volumes
        .iter()
        .find(|v| v.resource_id == attachment.volume_resource_id)?
        .resource_lineage_id;
    Some(VolumeAttachment {
        volume_resource_id: volume(to, lineage)?.resource_id.clone(),
        mount_path: attachment.mount_path.clone(),
    })
}

/// A base that predates a node starts that node, and any mounted Volume, from `into`'s copy.
fn adopt_node(
    base: &mut SavedEnvironmentIntent,
    into: &SavedEnvironmentIntent,
    lineage: &str,
    path: &RowPath,
) {
    let mounted = match path {
        RowPath::Mount(volume_lineage) => Some(volume_lineage.as_str()),
        RowPath::Node
        | RowPath::Data
        | RowPath::Name
        | RowPath::Variable(_)
        | RowPath::Setting(_) => None,
    };
    for lineage in [Some(lineage), mounted].into_iter().flatten() {
        if nodes(base).contains_key(lineage) {
            continue;
        }
        if let Some(volume) = volume(into, lineage) {
            base.volumes.push(volume.clone());
        } else if let Some(service) = service(into, lineage) {
            // Its mounts name `into`'s Volume ids, which base lacks; mount picks re-add them.
            base.services.push(SavedServiceIntent {
                volume_attachments: Vec::new(),
                ..service.clone()
            });
        }
    }
}
