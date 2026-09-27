//! Compare two authored configurations against the base they share, keyed by lineage.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::service_changes::{FIELDS, at};
use super::{
    ConfigError, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, SavedVolumeIntent, ServiceConfig, ValuePart, ValuePartOwner,
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

/// One chosen move row. A variable row also names a choice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchPick {
    pub key: String,
    #[serde(default)]
    #[ts(optional)]
    pub choice: Option<BranchOption>,
    /// The value for `new`, supplied by the caller (a secret's arrives sealed); never reviewed.
    #[serde(default)]
    #[ts(optional)]
    pub new_value: Option<BranchNewValue>,
}

/// A caller-supplied variable value; core never encrypts or fingerprints.
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
    /// Canonical, id-free rendering of the rows; callers hash it.
    pub review: String,
}

/// One setting of one lineage. Values are redacted: secrets carry only fingerprints.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchRow {
    /// `<lineageId>:<path>`.
    pub key: String,
    #[serde(flatten)]
    #[ts(flatten)]
    pub role: BranchRole,
    pub base: Value,
    pub from: Value,
    pub into: Value,
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
/// Returns ConfigError when any supplied configuration is invalid.
pub fn branch_changes(input: BranchChangesInput) -> Result<BranchChanges, ConfigError> {
    let base = input.base.map(parse_environment_intent).transpose()?;
    let from = parse_environment_intent(input.from)?;
    let into = parse_environment_intent(input.into)?;
    let parent = input.parent.map(parse_environment_intent).transpose()?;
    let sides = Sides {
        base: base.as_ref(),
        from: &from,
        into: &into,
        parent: parent.as_ref(),
        provided: input.provided.iter().map(String::as_str).collect(),
        hostnames: &input.hostnames,
        from_kept: input.from_kept,
    };
    let rows = sides.rows();
    let Some(picks) = input.picks else {
        let review = serde_json::to_string(&rows)
            .map_err(|_| ConfigError::at("review", "Rows must be JSON"))?;
        return Ok(BranchChanges {
            rows,
            next: into,
            base,
            review,
        });
    };
    let (next, advanced) = sides.apply(&rows, &picks)?;
    let mut reviewed: Vec<_> = picks
        .iter()
        .map(|pick| json!({"choice": pick.choice, "key": pick.key}))
        .collect();
    reviewed.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    let review = json!({"picks": reviewed, "rows": rows}).to_string();
    Ok(BranchChanges {
        rows,
        next,
        base: advanced,
        review,
    })
}

#[derive(Clone, Copy)]
enum Node<'a> {
    Service(&'a SavedServiceIntent),
    Volume(&'a SavedVolumeIntent),
}

struct Sides<'a> {
    base: Option<&'a SavedEnvironmentIntent>,
    from: &'a SavedEnvironmentIntent,
    into: &'a SavedEnvironmentIntent,
    parent: Option<&'a SavedEnvironmentIntent>,
    provided: BTreeSet<&'a str>,
    hostnames: &'a BranchHostnames,
    from_kept: bool,
}

impl Sides<'_> {
    fn rows(&self) -> Vec<BranchRow> {
        let base = self.base.map(nodes).unwrap_or_default();
        let from = nodes(self.from);
        let into = nodes(self.into);
        let mut rows = Vec::new();
        for lineage in from.keys().chain(into.keys()).collect::<BTreeSet<_>>() {
            let b = base.get(lineage).copied();
            match (from.get(lineage).copied(), into.get(lineage).copied()) {
                (Some(f), Some(i)) => self.settings_rows(lineage, b, f, i, &mut rows),
                // ponytail: a node only `from` has and `base` lacks is an introduction (#1146).
                (Some(f), None) => {
                    let why = if self.provided.contains(lineage) {
                        BranchReason::Live
                    } else if b.is_some() {
                        BranchReason::LeftOut
                    } else {
                        continue;
                    };
                    rows.push(node_row(lineage, why, b, Some(f), None));
                }
                (None, Some(i)) => {
                    let why = if uses(self.from, lineage) {
                        BranchReason::Live
                    } else if b.is_some() {
                        BranchReason::LeftOut
                    } else {
                        continue;
                    };
                    rows.push(node_row(lineage, why, b, None, Some(i)));
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
        let base_values = base.map(|n| settings(self.base, n, &self.hostnames.into));
        let base_values = base_values.unwrap_or_default();
        let from_values = settings(Some(self.from), from, &self.hostnames.from);
        let into_values = settings(Some(self.into), into, &self.hostnames.into);
        let paths: BTreeSet<_> = from_values.keys().chain(into_values.keys()).collect();
        for path in paths {
            let get = |values: &BTreeMap<String, Value>| values.get(path).cloned();
            let (b, f, i) = (
                get(&base_values).unwrap_or_default(),
                get(&from_values).unwrap_or_default(),
                get(&into_values).unwrap_or_default(),
            );
            let row = |role| BranchRow {
                key: format!("{lineage}:{path}"),
                role,
                base: b.clone(),
                from: f.clone(),
                into: i.clone(),
            };
            if let Some(why) = reason(path) {
                if f != i {
                    rows.push(row(BranchRole::Differ { why }));
                }
                continue;
            }
            // Nothing is deleted: a value `from` lacks never moves.
            if f.is_null() || f == b || f == i {
                continue;
            }
            let choice = match path.strip_prefix("variables.") {
                None => None,
                Some(key) => {
                    let secret = *at(&f, "kind") == "secret";
                    if secret && !i.is_null() {
                        continue;
                    }
                    Some(self.choice(lineage, key, secret))
                }
            };
            rows.push(row(BranchRole::Move {
                conflict: i != b,
                choice,
            }));
        }
        if let (Node::Volume(_), Node::Volume(_)) = (from, into) {
            let name = |node: Option<Node>| node.map_or(Value::Null, node_value);
            rows.push(BranchRow {
                key: format!("{lineage}:data"),
                role: BranchRole::Differ {
                    why: BranchReason::Data,
                },
                base: name(base),
                from: name(Some(from)),
                into: name(Some(into)),
            });
        }
    }

    /// Move the picked rows into `next` and advance `base` by exactly those rows.
    fn apply(
        &self,
        rows: &[BranchRow],
        picks: &[BranchPick],
    ) -> Result<(SavedEnvironmentIntent, Option<SavedEnvironmentIntent>), ConfigError> {
        let mut next = self.into.clone();
        let mut base = self.base.cloned();
        let mut seen = BTreeSet::new();
        for pick in picks {
            if !seen.insert(pick.key.as_str()) {
                return Err(ConfigError::at("picks.key", "Each change is picked once"));
            }
            let row = rows
                .iter()
                .find(|row| row.key == pick.key)
                .ok_or_else(|| ConfigError::at("picks.key", "Unknown change"))?;
            let BranchRole::Move { choice, .. } = &row.role else {
                return Err(ConfigError::at("picks.key", "Change is meant to differ"));
            };
            let (lineage, path) = pick.key.split_once(':').expect("row keys hold a lineage");
            match (choice, pick.choice, path.strip_prefix("variables.")) {
                (Some(offered), Some(chosen), Some(key)) if offered.options.contains(&chosen) => {
                    if let Some(variable) = self.chosen_variable(lineage, key, chosen, pick) {
                        put_variable(&mut next, lineage, variable);
                    }
                }
                (None, None, None) => move_setting(&mut next, self.from, lineage, path)?,
                _ => return Err(ConfigError::at("picks.choice", "Choice is not offered")),
            }
            // ponytail: a null base (create) advances in #1146.
            if let Some(base) = &mut base {
                adopt_node(base, self.into, lineage, path);
                match path.strip_prefix("variables.") {
                    Some(key) => put_variable(base, lineage, variable(self.from, lineage, key)),
                    None => move_setting(base, self.from, lineage, path)?,
                }
            }
        }
        let next = parse_environment_intent(json!(next))?;
        Ok((next, base.map(redact_environment_intent)))
    }

    fn chosen_variable(
        &self,
        lineage: &str,
        key: &str,
        chosen: BranchOption,
        pick: &BranchPick,
    ) -> Option<SavedVariableIntent> {
        match chosen {
            BranchOption::From => Some(variable(self.from, lineage, key)),
            BranchOption::Parent => self.parent.map(|parent| variable(parent, lineage, key)),
            BranchOption::New => pick.new_value.clone().map(|new| SavedVariableIntent {
                value: new.value,
                value_fingerprint: new.value_fingerprint,
                ..variable(self.from, lineage, key)
            }),
            BranchOption::LeaveOut => None,
        }
    }

    fn choice(&self, lineage: &str, key: &str, secret: bool) -> BranchChoice {
        let parent_has = self.parent.is_some_and(|parent| {
            parent
                .services
                .iter()
                .any(|s| s.lineage_id == lineage && s.variables.iter().any(|v| v.key == key))
        });
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

/// Settings each Environment owns; they are shown as meant to differ, never moved.
fn reason(path: &str) -> Option<BranchReason> {
    Some(match path {
        "replicas" | "cpuLimit" | "memLimit" => BranchReason::Sizing,
        "routes" => BranchReason::CustomDomain,
        "managedHostnames" => BranchReason::GeneratedAddress,
        "source.branch" => BranchReason::GitBranch,
        _ => return None,
    })
}

/// A node's comparable settings by path, normalized so copies compare with their originals.
fn settings(
    env: Option<&SavedEnvironmentIntent>,
    node: Node,
    suffix: &str,
) -> BTreeMap<String, Value> {
    let service = match node {
        Node::Volume(volume) => return BTreeMap::from([("name".into(), json!(volume.name))]),
        Node::Service(service) => service,
    };
    let config = json!(service.config);
    let mut values: BTreeMap<String, Value> = FIELDS
        .iter()
        .map(|path| ((*path).to_owned(), at(&config, path).clone()))
        .collect();
    // Registry credentials compare by presence; dropping one is a removal, so it never moves.
    let credentials = at(&config, "source.credentials.type");
    values.insert(
        "source.credentials".into(),
        if credentials == "configured" {
            json!(true)
        } else {
            Value::Null
        },
    );
    let mut domains: Vec<_> = service.config.routes.iter().map(|r| &r.hostname).collect();
    domains.sort();
    values.insert("routes".into(), json!(domains));
    let generated: Vec<_> = service
        .config
        .managed_hostnames
        .iter()
        .map(|h| {
            let prefix = h.prefix.strip_suffix(suffix).unwrap_or(&h.prefix);
            json!({"prefix": prefix, "targetPort": h.target_port})
        })
        .collect();
    values.insert("managedHostnames".into(), json!(generated));
    // Mounts compare by Volume lineage and mount path.
    for attachment in &service.volume_attachments {
        let lineage = env
            .and_then(|env| {
                env.volumes
                    .iter()
                    .find(|v| v.resource_id == attachment.volume_resource_id)
            })
            .map(|v| v.resource_lineage_id.as_str());
        if let Some(lineage) = lineage {
            values.insert(format!("mounts.{lineage}"), json!(attachment.mount_path));
        }
    }
    for variable in &service.variables {
        values.insert(
            format!("variables.{}", variable.key),
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

fn node_row(
    lineage: &str,
    why: BranchReason,
    base: Option<Node>,
    from: Option<Node>,
    into: Option<Node>,
) -> BranchRow {
    let value = |node: Option<Node>| node.map_or(Value::Null, node_value);
    BranchRow {
        key: format!("{lineage}:node"),
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
        .any(|v| match &v.value {
            SavedVariableValue::Template { parts } => parts.iter().any(|part| {
                matches!(part, ValuePart::Ref { owner: ValuePartOwner::Service { lineage_id }, .. } if lineage_id == lineage)
            }),
            SavedVariableValue::Literal { .. } | SavedVariableValue::Secret { .. } => false,
        })
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

fn service<'a>(env: &'a SavedEnvironmentIntent, lineage: &str) -> &'a SavedServiceIntent {
    env.services
        .iter()
        .find(|s| s.lineage_id == lineage)
        .expect("move rows are on nodes both sides have")
}

/// The variable a row was computed from; offered choices guarantee it exists.
fn variable(env: &SavedEnvironmentIntent, lineage: &str, key: &str) -> SavedVariableIntent {
    service(env, lineage)
        .variables
        .iter()
        .find(|v| v.key == key)
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

/// Give `env` the setting at `path` that `from` has.
fn move_setting(
    env: &mut SavedEnvironmentIntent,
    from: &SavedEnvironmentIntent,
    lineage: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if path == "name" {
        let name = from
            .volumes
            .iter()
            .find(|v| v.resource_lineage_id == lineage)
            .map(|v| v.name.clone());
        if let (Some(volume), Some(name)) = (
            env.volumes
                .iter_mut()
                .find(|v| v.resource_lineage_id == lineage),
            name,
        ) {
            volume.name = name;
        }
        return Ok(());
    }
    if let Some(volume_lineage) = path.strip_prefix("mounts.") {
        let volume_id = |env: &SavedEnvironmentIntent| {
            env.volumes
                .iter()
                .find(|v| v.resource_lineage_id == volume_lineage)
                .map(|v| v.resource_id.clone())
        };
        let from_id = volume_id(from).expect("mount rows name an authored Volume");
        let mount_path = service(from, lineage)
            .volume_attachments
            .iter()
            .find(|a| a.volume_resource_id == from_id)
            .expect("mount rows come from an attachment")
            .mount_path
            .clone();
        let volume_resource_id = volume_id(env).ok_or_else(|| {
            ConfigError::at(
                "picks.key",
                "Mounted Volume is not in the receiving configuration",
            )
        })?;
        let attachments = &mut service_mut(env, lineage).volume_attachments;
        attachments.retain(|a| a.volume_resource_id != volume_resource_id);
        attachments.push(VolumeAttachment {
            volume_resource_id,
            mount_path,
        });
        return Ok(());
    }
    let target = service_mut(env, lineage);
    target.config = restore_service_setting(
        ServiceConfig::from(target.config.clone()),
        &ServiceConfig::from(service(from, lineage).config.clone()),
        path,
    )?
    .settings;
    Ok(())
}

/// A base that predates a node starts that node, and any mounted Volume, from `into`'s copy.
fn adopt_node(
    base: &mut SavedEnvironmentIntent,
    into: &SavedEnvironmentIntent,
    lineage: &str,
    path: &str,
) {
    let volume = |lineage: &str| {
        into.volumes
            .iter()
            .find(|v| v.resource_lineage_id == lineage)
    };
    for lineage in [Some(lineage), path.strip_prefix("mounts.")]
        .into_iter()
        .flatten()
    {
        if nodes(base).contains_key(lineage) {
            continue;
        }
        if let Some(volume) = volume(lineage) {
            base.volumes.push(volume.clone());
        } else if let Some(service) = into.services.iter().find(|s| s.lineage_id == lineage) {
            // Its mounts name `into`'s Volume ids, which base lacks; mount picks re-add them.
            base.services.push(SavedServiceIntent {
                volume_attachments: Vec::new(),
                ..service.clone()
            });
        }
    }
}
