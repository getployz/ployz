//! Compare two authored configurations against the base they share, keyed by lineage.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::service_changes::{FIELDS, at};
use super::{
    ConfigError, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, SavedVolumeIntent, ValuePart, ValuePartOwner, parse_environment_intent,
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
    let review =
        serde_json::to_string(&rows).map_err(|_| ConfigError::at("review", "Rows must be JSON"))?;
    Ok(BranchChanges {
        rows,
        next: into,
        base,
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
