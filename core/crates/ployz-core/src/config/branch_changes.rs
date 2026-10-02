//! Compare two authored configurations against the base they share, keyed by lineage, and move
//! the picked rows from one into the other.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::branch_changes_types::RowPath;
use super::service_changes::{FIELDS, at};
use super::{
    BranchChanges, BranchChangesInput, BranchChoice, BranchHostnames, BranchNewValue, BranchOption,
    BranchPick, BranchPickChoice, BranchReason, BranchRole, BranchRow, BranchRowKey, ConfigError,
    SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent, SavedVariableValue,
    SavedVolumeIntent, ServiceConfig, ServiceImageCredentials, ServiceSource, VolumeAttachment,
    parse_environment_intent, redact_environment_intent, restore_service_setting,
};

/// Compare `from` and `into` against `base`. Without picks (`None`) this only compares: `next` is
/// `into` and `base` is unchanged. With picks, even none, `next` receives them and `base` advances;
/// creating (no `base`) returns the Parent minus the lineages `into` uses live.
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
        never_synced: &input.never_synced,
    };
    let rows = comparison.rows();
    let picks = input.picks.as_deref();
    let (next, advanced) = match picks {
        None => (into.clone(), base.clone()),
        Some(picks) => comparison.apply(&rows, picks)?,
    };
    let review = review(&rows, picks.unwrap_or_default());
    Ok(BranchChanges {
        rows,
        next,
        base: advanced,
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

/// Admitted picks in landing order, the lineages they introduce, and the rows they name.
struct Admitted<'a> {
    landings: Vec<(&'a str, Landing<'a>)>,
    introduced: BTreeSet<&'a str>,
    picked: BTreeSet<&'a BranchRowKey>,
}

/// What an admitted pick changes.
enum Landing<'a> {
    Node,
    Variable {
        key: &'a str,
        choice: &'a BranchPickChoice,
    },
    Setting(SettingPath<'a>),
}

/// A setting a move row carries.
#[derive(Clone, Copy)]
enum SettingPath<'a> {
    Name,
    Storage,
    Mount(&'a str),
    Field(&'static str),
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
    never_synced: &'a [String],
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
                    | RowPath::Storage
                    | RowPath::Mount(_)
                    | RowPath::Setting(_) => None,
                };
                self.unless_never_synced(
                    lineage,
                    path,
                    BranchRole::Move {
                        conflict: into_value != base_value,
                        choice,
                    },
                )
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
            let secret = matches!(
                variable.value,
                SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue
            );
            let mut choice = self.choice(lineage, &variable.key, secret);
            // A Branch starts with its Parent's values, secrets included.
            if self.base.is_none() {
                choice.default = BranchOption::From;
            }
            let path = RowPath::Variable(variable.key.clone());
            rows.push(BranchRow {
                role: self.unless_never_synced(
                    lineage,
                    &path,
                    BranchRole::Move {
                        conflict: false,
                        choice: Some(choice),
                    },
                ),
                key: key(lineage, path),
                base: Value::Null,
                from: variable_value(variable),
                into: Value::Null,
            });
        }
    }

    /// `role`, unless the row at `lineage`'s `path` is marked Never sync: then it differs.
    fn unless_never_synced(&self, lineage: &str, path: &RowPath, role: BranchRole) -> BranchRole {
        let row = key(lineage, path.clone()).to_string();
        let marked = self.never_synced.iter().any(|marked| {
            row.strip_prefix(marked.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        });
        match marked {
            true => BranchRole::Differ {
                why: BranchReason::NeverSynced,
            },
            false => role,
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

    /// Move the picked rows into `next` and advance `base` by what lands there.
    /// Creating (no `base`) returns `from` minus the lineages `into` uses live as the new base.
    fn apply(
        &self,
        rows: &[BranchRow],
        picks: &[BranchPick],
    ) -> Result<(SavedEnvironmentIntent, Option<SavedEnvironmentIntent>), ConfigError> {
        let admitted = self.admit_picks(rows, picks)?;
        let mut next = self.into.clone();
        let mut base = self.base.cloned();
        for (lineage, landing) in &admitted.landings {
            self.land(lineage, landing, &mut next, base.as_mut())?;
        }
        self.land_defaults(rows, &admitted, &mut next, base.as_mut());
        let next = parse_environment_intent(json!(next))?;
        let base = base.unwrap_or_else(|| {
            let mut base = self.from.clone();
            base.services
                .retain(|s| !self.provided.contains(s.lineage_id.as_str()));
            base
        });
        Ok((next, Some(redact_environment_intent(base))))
    }

    /// Match picks to rows, refuse any that can't land, and type each by what it changes;
    /// introductions come first (Volumes, then services) so settings and variables find their node.
    fn admit_picks<'r>(
        &self,
        rows: &'r [BranchRow],
        picks: &'r [BranchPick],
    ) -> Result<Admitted<'r>, ConfigError> {
        let by_key: BTreeMap<String, &BranchRow> =
            rows.iter().map(|row| (row.key.to_string(), row)).collect();
        let mut admitted = Admitted {
            landings: Vec::new(),
            introduced: BTreeSet::new(),
            picked: BTreeSet::new(),
        };
        for pick in picks {
            let row = by_key
                .get(pick.key.as_str())
                .copied()
                .ok_or_else(|| ConfigError::at("picks.key", "Unknown change"))?;
            if !admitted.picked.insert(&row.key) {
                return Err(ConfigError::at("picks.key", "Each change is picked once"));
            }
            let BranchRole::Move {
                choice: offered, ..
            } = &row.role
            else {
                return Err(ConfigError::at("picks.key", "Change is meant to differ"));
            };
            let landing = match (&row.key.path, offered, &pick.choice) {
                (RowPath::Variable(key), Some(offered), Some(choice))
                    if offered.options.contains(&choice.option()) =>
                {
                    admit_new_value(offered, choice)?;
                    Landing::Variable { key, choice }
                }
                (RowPath::Node, None, None) => Landing::Node,
                (RowPath::Name, None, None) => Landing::Setting(SettingPath::Name),
                (RowPath::Storage, None, None) => Landing::Setting(SettingPath::Storage),
                (RowPath::Mount(volume), None, None) => {
                    Landing::Setting(SettingPath::Mount(volume))
                }
                (RowPath::Setting(field), None, None) => {
                    Landing::Setting(SettingPath::Field(field))
                }
                _ => return Err(ConfigError::at("picks.choice", "Choice is not offered")),
            };
            let lineage = row.key.lineage.as_str();
            if matches!(landing, Landing::Node) {
                admitted.introduced.insert(lineage);
            }
            admitted.landings.push((lineage, landing));
        }
        let into = nodes(self.into);
        if admitted.landings.iter().any(|(lineage, _)| {
            !admitted.introduced.contains(lineage) && !into.contains_key(lineage)
        }) {
            return Err(ConfigError::at(
                "picks.key",
                "Pick the node to introduce its variables",
            ));
        }
        admitted
            .landings
            .sort_by_key(|(lineage, landing)| match landing {
                Landing::Node if volume(self.from, lineage).is_some() => 0,
                Landing::Node => 1,
                Landing::Variable { .. } | Landing::Setting(_) => 2,
            });
        Ok(admitted)
    }

    fn land(
        &self,
        lineage: &str,
        landing: &Landing,
        next: &mut SavedEnvironmentIntent,
        base: Option<&mut SavedEnvironmentIntent>,
    ) -> Result<(), ConfigError> {
        match *landing {
            Landing::Node => self.introduce(next, base, lineage)?,
            Landing::Variable { key, choice } => {
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
                if let Some(base) = base {
                    adopt_node(base, self.into, lineage, None);
                    put_variable(base, lineage, variable(self.from, lineage, key));
                }
            }
            Landing::Setting(path) => {
                move_setting(next, self.from, lineage, path)?;
                if let Some(base) = base {
                    let mounted = match path {
                        SettingPath::Mount(volume) => Some(volume),
                        SettingPath::Name | SettingPath::Storage | SettingPath::Field(_) => None,
                    };
                    adopt_node(base, self.into, lineage, mounted);
                    move_setting(base, self.from, lineage, path)?;
                }
            }
        }
        Ok(())
    }

    /// An introduced node's unpicked variables land in `next` by their default choice, and
    /// `base` records each one that lands; one left out stays proposed.
    fn land_defaults(
        &self,
        rows: &[BranchRow],
        admitted: &Admitted,
        next: &mut SavedEnvironmentIntent,
        mut base: Option<&mut SavedEnvironmentIntent>,
    ) {
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
            if !admitted.introduced.contains(lineage) || admitted.picked.contains(&row.key) {
                continue;
            }
            let Some(landed) = self.chosen_variable(lineage, key, choice.default, None) else {
                continue;
            };
            put_variable(next, lineage, landed);
            if let Some(base) = base.as_deref_mut() {
                put_variable(base, lineage, variable(self.from, lineage, key));
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
            // Only a secret takes `new` without a value: it lands without one.
            BranchOption::New => {
                let (value, value_fingerprint) = new_value.map_or_else(
                    || (SavedVariableValue::SecretWithoutValue, String::new()),
                    |new| (new.value.clone(), new.value_fingerprint.clone()),
                );
                Some(SavedVariableIntent {
                    value,
                    value_fingerprint,
                    ..variable(self.from, lineage, key)
                })
            }
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

fn volume_mut<'a>(
    env: &'a mut SavedEnvironmentIntent,
    lineage: &str,
) -> Option<&'a mut SavedVolumeIntent> {
    env.volumes
        .iter_mut()
        .find(|v| v.resource_lineage_id == lineage)
}

fn volume_by_id<'a>(env: &'a SavedEnvironmentIntent, id: &str) -> Option<&'a SavedVolumeIntent> {
    env.volumes.iter().find(|v| v.resource_id == id)
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
        Node::Volume(volume) => {
            return BTreeMap::from([
                (RowPath::Name, json!(volume.name)),
                (RowPath::Storage, json!(volume.storage)),
            ]);
        }
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
        if let Some(volume) = volume_by_id(env, &attachment.volume_resource_id) {
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
        SavedVariableValue::SecretWithoutValue => json!({"kind": "secret"}),
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
    path: SettingPath,
) -> Result<(), ConfigError> {
    match path {
        SettingPath::Storage => {
            let storage = volume(from, lineage)
                .expect("storage rows are Volumes")
                .storage;
            if let Some(target) = volume_mut(env, lineage) {
                target.storage = storage;
            }
        }
        SettingPath::Name => {
            let name = &volume(from, lineage).expect("name rows are Volumes").name;
            if let Some(target) = volume_mut(env, lineage) {
                target.name.clone_from(name);
            }
        }
        SettingPath::Mount(volume_lineage) => {
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
        SettingPath::Field(path) => {
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
    let lineage = &volume_by_id(from, &attachment.volume_resource_id)?.resource_lineage_id;
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
    mounted: Option<&str>,
) {
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
