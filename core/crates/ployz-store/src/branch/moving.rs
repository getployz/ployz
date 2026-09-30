//! Moving changes between a Branch and its Parent, and landing them.

use super::*;

pub(crate) fn move_changes(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &Move,
) -> Result<Moved, RpcError> {
    let (named, direction, picks, version) = match request {
        Move::Take(take) => return crate::conditional_save::take(tx, who, take),
        Move::Save(save) if crate::conditional_save::at_merge(tx, who, &save.from, save.when)? => {
            return crate::conditional_save::save(tx, who, sealing, save);
        }
        Move::Save(save) => (
            (&save.from, save.into.as_ref()),
            Direction::Save,
            save.picks.as_deref(),
            save.version.as_deref(),
        ),
        Move::Update(update) => (
            (&update.into, None),
            Direction::Update,
            update.picks.as_deref(),
            update.version.as_deref(),
        ),
    };
    let mut sides = sides(tx, who, named, direction, true)?;
    if let Some(removal) = crate::teardown::removing(tx, &sides.branch().summary.id)? {
        return Err(crate::teardown::being_removed(sides.branch(), &removal));
    }
    if sides.direction == Direction::Update {
        settled(tx, &sides.into)?;
    }
    let moving = moving(tx, &sides)?;
    let changes = reviewed(&moving, &sides.into, version)?;
    let picks = self::picks(&moving, &sides.into, sealing, &changes.rows, picks)?;
    let staged = moving.apply(tx, who, &mut sides.into, picks)?;
    Ok(Moved {
        branch: Some(view(tx, sides.branch())?),
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
    let (named, direction) = match query {
        MoveQuery::Save { from, into, when } => {
            if crate::conditional_save::at_merge(tx, who, from, *when)? {
                return crate::conditional_save::view(tx, who, from, into.as_ref());
            }
            ((from, into.as_ref()), Direction::Save)
        }
        MoveQuery::Update { into } => ((into, None), Direction::Update),
    };
    let sides = sides(tx, who, named, direction, false)?;
    let moving = moving(tx, &sides)?;
    view_of(&moving, sides.from, sides.into)
}

/// Compare `moving` into `into`, refusing unless `asked`, if any, is still the
/// version of that comparison.
pub(crate) fn reviewed(
    moving: &Moving,
    into: &Environment,
    asked: Option<&str>,
) -> Result<BranchChanges, RpcError> {
    let changes = moving.compare(&into.working, None)?;
    let current = version(into, &changes.review);
    if asked.is_some_and(|asked| asked != current) {
        return Err(error::conflict(
            "Changed since you reviewed: review the move again",
            json!({ "version": current }),
        ));
    }
    Ok(changes)
}

/// What moving `from` into `into` would stage, as the Move view shows it.
pub(crate) fn view_of(
    moving: &Moving,
    from: Environment,
    into: Environment,
) -> Result<MoveView, RpcError> {
    let changes = moving.compare(&into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter_map(|row| move_row(moving, &from, &into, row))
        .collect();
    let differ = changes
        .rows
        .iter()
        .filter_map(|row| match row.role {
            BranchRole::Differ {
                why:
                    why @ (BranchReason::Sizing
                    | BranchReason::CustomDomain
                    | BranchReason::GeneratedAddress
                    | BranchReason::GitBranch),
            } => {
                let (row, from, into) = shown_row(moving, &from, &into, row);
                Some(DifferRow {
                    row,
                    why,
                    from,
                    into,
                })
            }
            BranchRole::Differ { .. } | BranchRole::Move { .. } => None,
        })
        .collect();
    Ok(MoveView {
        version: version(&into, &changes.review),
        from: from.summary,
        into: into.summary,
        rows,
        differ,
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
    let (name, from_value, into_value) = shown_row(moving, from, into, row);
    Some(MoveRow {
        row: name,
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

/// A row's name and its two values as reads show them.
fn shown_row(
    moving: &Moving,
    from: &Environment,
    into: &Environment,
    row: &BranchRow,
) -> (String, Value, Value) {
    let key = row.key.to_string();
    let (lineage, path) = split(&key);
    let variable = |intent: &SavedEnvironmentIntent, names| {
        let key = path.strip_prefix("variables.")?;
        let service = intent.services.iter().find(|s| s.lineage_id == lineage)?;
        let found = service.variables.iter().find(|v| v.key == key)?;
        Some(crate::variables::shown(found, names))
    };
    let (from_names, into_names) = (from.names(), into.names());
    let from_value =
        variable(&moving.from, &from_names).unwrap_or_else(|| shown(path, row.from.clone()));
    let into_value =
        variable(&into.working, &into_names).unwrap_or_else(|| shown(path, row.into.clone()));
    (moving.name(&into.working, &key), from_value, into_value)
}

/// Which way changes move between a Branch and its Parent. [`Way`] is the same
/// choice once the move is compared, with what only a Save needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    /// The Branch's changes into its Parent.
    Save,
    /// The Parent's deployed changes into the Branch.
    Update,
}

/// A Branch and its Parent, as one Move addresses them.
pub(super) struct Sides {
    from: Environment,
    into: Environment,
    direction: Direction,
    row: Row,
}

impl Sides {
    pub(super) fn branch(&self) -> &Environment {
        match self.direction {
            Direction::Update => &self.into,
            Direction::Save => &self.from,
        }
    }
}

/// Resolve a Move's sides from the Branch it names and, if named, its Parent;
/// `lock` locks both, in ID order.
pub(super) fn sides(
    tx: &mut dyn Tx,
    who: &Actor,
    (branch, parent): (&EnvironmentRef, Option<&EnvironmentRef>),
    direction: Direction,
    lock: bool,
) -> Result<Sides, RpcError> {
    let branch = scope::environment(tx, who, branch)?;
    let parent_named = parent
        .map(|parent| scope::environment(tx, who, parent))
        .transpose()?;
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
    let (branch, parent) = scope::load_pair(tx, who, (&branch.summary.id, &parent), lock)?;
    // Read again under the locks: the base may have moved meanwhile.
    let row = branch_row(tx, &branch)?;
    Ok(match direction {
        Direction::Update => Sides {
            from: parent,
            into: branch,
            direction,
            row,
        },
        Direction::Save => Sides {
            from: branch,
            into: parent,
            direction,
            row,
        },
    })
}

/// A Move's comparison: Update from the Parent's Applied State, Save from the
/// Branch's Working State with the Parent's deployed values on offer.
pub(super) fn moving(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
    match sides.direction {
        Direction::Update => {
            let applied = deployment::head(tx, &sides.from)?.applied;
            Moving::update(
                tx,
                &sides.from,
                applied,
                &sides.into,
                sides.row.base.clone(),
            )
        }
        Direction::Save => {
            let parent = deployment::head(tx, &sides.into)?.applied;
            Moving::save(tx, &sides.from, &sides.into, &sides.row, parent)
        }
    }
}

/// A Move's guard: the receiver's revision and core's review of the changes.
pub(crate) fn version(into: &Environment, review: &str) -> String {
    format!(
        "{}:{}",
        into.summary.revision,
        crate::removal::short_digest(review)
    )
}

/// Core's picks for `asked`: every change named or under a name, each variable
/// the way asked or its default. A secret wanting a fresh value without one is
/// refused: the Store never makes one up, and a Branch's own secret moves only
/// when asked. A fresh value given lands as text, or sealed for a secret.
pub(crate) fn picks(
    moving: &Moving,
    into: &Environment,
    sealing: &SealingKey,
    rows: &[BranchRow],
    asked: Option<&[MovePick]>,
) -> Result<Vec<BranchPick>, RpcError> {
    let named: Vec<(String, &BranchRow, Option<&BranchChoice>)> = rows
        .iter()
        .filter_map(|row| match &row.role {
            BranchRole::Move { choice, .. } => Some((
                moving.name(&into.working, &row.key.to_string()),
                row,
                choice.as_ref(),
            )),
            BranchRole::Differ { .. } => None,
        })
        .collect();
    let mut chosen: BTreeMap<String, Option<&PickChoice>> = BTreeMap::new();
    match asked {
        None => {
            for (_, row, _) in &named {
                chosen.insert(row.key.to_string(), None);
            }
        }
        Some(asked) => {
            for pick in asked {
                let found: Vec<_> = named
                    .iter()
                    .filter(|(name, ..)| under(name, &pick.row))
                    .collect();
                if found.is_empty() {
                    let names = named.iter().map(|(name, ..)| name.as_str());
                    return Err(error::choices(
                        format!("No change named {} moves", pick.row),
                        &pick.row,
                        names,
                    ));
                }
                if pick.choice.is_some() && found.iter().all(|(.., choice)| choice.is_none()) {
                    return Err(error::invalid(
                        format!("{}: only a variable takes a choice", pick.row),
                        json!({ "row": pick.row }),
                    ));
                }
                if matches!(pick.choice, Some(PickChoice::New(_))) && found.len() != 1 {
                    return Err(error::invalid(
                        format!("{}: a value goes with one variable", pick.row),
                        json!({ "row": pick.row }),
                    ));
                }
                for (_, row, _) in found {
                    chosen.insert(row.key.to_string(), pick.choice.as_ref());
                }
            }
        }
    }
    let names = into.names();
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
                let option = asked.map_or_else(|| moving.default(offered), PickChoice::option);
                if !offered.options.contains(&option) {
                    let options = json!(offered.options);
                    let names: Vec<&str> = options
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    return Err(error::invalid(
                        format!("{name}: a variable lands as one of {}", names.join(", ")),
                        json!({ "row": name, "options": options }),
                    ));
                }
                Some(match (option, asked) {
                    (_, Some(PickChoice::New(text))) => BranchPickChoice::New {
                        value: Some(fresh_value(name, offered.secret, text, &names, sealing)?),
                    },
                    (BranchOption::From, _) => BranchPickChoice::From,
                    (BranchOption::Parent, _) => BranchPickChoice::Parent,
                    (BranchOption::LeaveOut, _) => BranchPickChoice::LeaveOut,
                    (BranchOption::New, _) => {
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
                "{}: a secret set in the Branch moves only when picked `from`; or pick `new` with a value, or `leave_out` and set one after",
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

/// Whether row `name` is `asked`, or under it: `web` covers `web.image`.
pub(crate) fn under(name: &str, asked: &str) -> bool {
    name == asked
        || name
            .strip_prefix(asked)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// One move between a Branch and an Environment it comes from: `from`'s changes
/// over `base`, into the other side.
pub(crate) struct Moving {
    /// The Environment `from` belongs to: what arriving nodes carry comes from it.
    pub(crate) source: EnvironmentId,
    /// The Branch whose base advances by what lands.
    pub(crate) branch: EnvironmentId,
    /// Why nothing moves.
    pub(crate) nothing: String,
    pub(crate) from: SavedEnvironmentIntent,
    pub(crate) base: SavedEnvironmentIntent,
    /// Lineages the receiver uses live.
    pub(crate) provided: Vec<String>,
    pub(crate) hostnames: BranchHostnames,
    pub(crate) way: Way,
}

/// Which way a move goes, with what only that way needs.
#[derive(Clone, Debug)]
pub(crate) enum Way {
    /// Into a Branch: every variable lands with `from`'s value by default.
    Update,
    /// Into a Parent, or a PR Environment's Destination: nothing there is deleted.
    Save {
        /// The Parent's deployed values, offered for a variable; none when it
        /// deployed nothing.
        parent: Option<SavedEnvironmentIntent>,
        /// Whether `from` is a kept Branch: its secrets then stay by default.
        from_kept: bool,
    },
}

impl Moving {
    /// What `parent` deployed, into `branch`.
    pub(super) fn update(
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
            provided: used_live(&branch.working).into_keys().collect(),
            hostnames: BranchHostnames {
                from: suffix(tx, parent)?,
                into: suffix(tx, branch)?,
            },
            way: Way::Update,
        })
    }

    /// Branch `from`'s Working State (its Branch row `row`) into `into`, with what
    /// its Parent deployed, `parent`, on offer.
    pub(crate) fn save(
        tx: &mut dyn Tx,
        from: &Environment,
        into: &Environment,
        row: &Row,
        parent: SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        let deployed = !(parent.services.is_empty() && parent.volumes.is_empty());
        Ok(Self {
            source: from.summary.id.clone(),
            branch: from.summary.id.clone(),
            nothing: format!("Nothing to save into {}", into.summary.name),
            from: from.working.clone(),
            base: row.base.clone(),
            provided: used_live(&into.working).into_keys().collect(),
            hostnames: BranchHostnames {
                from: suffix(tx, from)?,
                into: suffix(tx, into)?,
            },
            way: Way::Save {
                parent: deployed.then_some(parent),
                from_kept: row.kept,
            },
        })
    }

    /// A saved move landing later: `from`'s changes over `base` from Branch
    /// `branch`, with `parent`'s deployed values on offer, into `into`.
    pub(crate) fn landed(
        branch: &EnvironmentId,
        (from, base): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
        hostnames: &BranchHostnames,
        parent: Option<&SavedEnvironmentIntent>,
        into: &SavedEnvironmentIntent,
    ) -> Self {
        Self {
            source: branch.clone(),
            branch: branch.clone(),
            nothing: "Nothing left to land".to_owned(),
            from: from.clone(),
            base: base.clone(),
            provided: used_live(into).into_keys().collect(),
            hostnames: hostnames.clone(),
            way: Way::Save {
                parent: parent.cloned(),
                from_kept: false,
            },
        }
    }

    /// A Save's deployed Parent values on offer; none for an Update.
    pub(crate) fn parent(&self) -> Option<&SavedEnvironmentIntent> {
        match &self.way {
            Way::Save { parent, .. } => parent.as_ref(),
            Way::Update => None,
        }
    }

    pub(crate) fn compare(
        &self,
        into: &SavedEnvironmentIntent,
        picks: Option<Vec<BranchPick>>,
    ) -> Result<BranchChanges, RpcError> {
        let (parent, from_kept) = match &self.way {
            Way::Update => (None, false),
            Way::Save { parent, from_kept } => (parent.as_ref(), *from_kept),
        };
        compare(Comparing {
            base: Some(&self.base),
            from: &self.from,
            into,
            parent,
            provided: &self.provided,
            hostnames: &self.hostnames,
            from_kept,
            picks,
        })
    }

    /// How a variable lands unless picked otherwise.
    pub(super) fn default(&self, offered: &BranchChoice) -> BranchOption {
        match self.way {
            Way::Update => BranchOption::From,
            Way::Save { .. } => offered.default,
        }
    }

    /// Row `key` as `NODE[.path]` in the catalog's words (`web.image`,
    /// `web.env.KEY`), by the node's name where it comes from, else in `into`.
    pub(crate) fn name(&self, into: &SavedEnvironmentIntent, key: &str) -> String {
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
        SettingPath::from_core(&node, path, name_in)
    }

    /// Land `picks` in `into` and advance the Branch's base by exactly what landed.
    /// Into a Parent, nothing there is deleted.
    pub(super) fn apply(
        &self,
        tx: &mut dyn Tx,
        who: &Actor,
        into: &mut Environment,
        picks: Vec<BranchPick>,
    ) -> Result<Vec<NodeName>, RpcError> {
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
        let save = matches!(self.way, Way::Save { .. });
        if save && !ids(&into.working).is_subset(&ids(&moved.next)) {
            return Err(error::internal("A Save would delete from the Parent"));
        }
        let carried = Carried::of(tx, &self.source, &self.from)?;
        let staged = land(tx, who, into, (&self.from, &carried), moved.next, &picks)?;
        tx.execute(
            "UPDATE config_environment_branch SET base = ?1 WHERE environment_id = ?2",
            &[document(&base).as_str().into(), self.branch.as_str().into()],
        )?;
        Ok(staged)
    }
}

/// A row key's lineage and path.
pub(super) fn split(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or((key, ""))
}

/// What the Services of one side of a move carry besides their configuration, by
/// Service ID: their sealed registry credentials and their Deployment Policies.
/// Captured when the move is made, so landing never reads a side that may be gone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Carried {
    credentials: BTreeMap<String, ployz_core::config::EncryptedSecretValue>,
    policies: BTreeMap<String, Policy>,
}

impl Carried {
    /// What the Services of `from`, as Environment `source` holds them, carry.
    pub(crate) fn of(
        tx: &mut dyn Tx,
        source: &EnvironmentId,
        from: &SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        let mut carried = Self::default();
        for service in &from.services {
            if let Some(sealed) = registry::sealed(tx, source, &service.id)? {
                carried.credentials.insert(service.id.clone(), sealed);
            }
            if let Some(policy) = policy::stored(tx, source, &service.id)? {
                carried.policies.insert(service.id.clone(), policy);
            }
        }
        Ok(carried)
    }
}

/// Land core's `next` as `branch`'s Working State: the nodes arriving from `from`
/// bring their registry credentials and Deployment Policies (`carried`) and get
/// their Node Introductions, and a picked credential row brings its credential to
/// the Service already there. Returns the nodes staged.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    branch: &mut Environment,
    (from, carried): (&SavedEnvironmentIntent, &Carried),
    next: SavedEnvironmentIntent,
    picks: &[BranchPick],
) -> Result<Vec<NodeName>, RpcError> {
    let before = std::mem::replace(&mut branch.working, next);
    crate::volume::check_storage(tx, &branch.summary.id, &branch.working)?;
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
        staged.push(NodeName::Service(
            ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))?,
        ));
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
        if let Some(source) = source_of(&service.lineage_id) {
            if credentials && let Some(sealed) = carried.credentials.get(&source) {
                registry::store(tx, who, &id, &service.id, sealed)?;
            }
            if let Some(policy) = carried.policies.get(&source) {
                policy::store(tx, who, &id, &service.id, policy)?;
            }
        }
        scope::introduce(tx, who, &id, scope::Node::Service(service))?;
    }
    for pick in picks {
        let Some(lineage) = pick.key.strip_suffix(":source.credentials") else {
            continue;
        };
        let receiver = before.services.iter().find(|old| old.lineage_id == lineage);
        let sealed = source_of(lineage).and_then(|source| carried.credentials.get(&source));
        if let (Some(receiver), Some(sealed)) = (receiver, sealed) {
            registry::store(tx, who, &id, &receiver.id, sealed)?;
        }
    }
    for volume in &branch.working.volumes {
        if !before
            .volumes
            .iter()
            .any(|old| old.resource_id == volume.resource_id)
        {
            staged.push(NodeName::Volume(
                VolumeName::parse(volume.name.as_str())
                    .map_err(|_| error::corrupt("Volume name"))?,
            ));
            scope::introduce(tx, who, &id, scope::Node::Volume(volume))?;
        }
    }
    branch.live = live_names(tx, &id, &branch.working)?;
    staged.sort();
    Ok(staged)
}

/// Update and Own Copy rewrite what a Branch runs, so they wait until it runs its
/// Working State: no Deployment in flight and nothing staged.
pub(super) fn settled(tx: &mut dyn Tx, branch: &Environment) -> Result<(), RpcError> {
    let scope = format!(
        "--project {} --env {}",
        branch.summary.project, branch.summary.name
    );
    if deployment::in_flight(tx, &branch.summary.id)?.is_some() {
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

/// The sides of core's comparison: `from` and `into` over `base`, moving `picks` when given.
pub(super) struct Comparing<'a> {
    pub(super) base: Option<&'a SavedEnvironmentIntent>,
    pub(super) from: &'a SavedEnvironmentIntent,
    pub(super) into: &'a SavedEnvironmentIntent,
    pub(super) parent: Option<&'a SavedEnvironmentIntent>,
    pub(super) provided: &'a [String],
    pub(super) hostnames: &'a BranchHostnames,
    pub(super) from_kept: bool,
    pub(super) picks: Option<Vec<BranchPick>>,
}

pub(super) fn compare(sides: Comparing<'_>) -> Result<BranchChanges, RpcError> {
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
