//! The Move command and view: Save and Update between a Branch and its Parent,
//! compared and landed by [`pair`](super::pair).

use super::*;

pub(crate) fn move_changes(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &Move,
) -> Result<Moved, RpcError> {
    let (named, direction, picks, version) = match request {
        Move::Take(take) => {
            return match &take.from {
                HintSource::ConditionalSync(save) => {
                    crate::conditional_sync::take(tx, who, save, take)
                }
                HintSource::Parent(parent) => follow::take(tx, who, parent, take),
            };
        }
        Move::Save(save)
            if crate::conditional_sync::at_merge(
                tx,
                who,
                (&save.from, save.into.as_ref()),
                save.when,
            )? =>
        {
            return crate::conditional_sync::save(tx, who, sealing, save);
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
        conditional_sync: None,
    })
}

pub(crate) fn move_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &MoveQuery,
) -> Result<MoveView, RpcError> {
    let (named, direction) = match query {
        MoveQuery::Save { from, into, when } => {
            if crate::conditional_sync::at_merge(tx, who, (from, into.as_ref()), *when)? {
                return crate::conditional_sync::view(tx, who, from, into.as_ref());
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
pub(super) fn shown_row(
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
    pub(super) from: Environment,
    pub(super) into: Environment,
    direction: Direction,
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
    Ok(match direction {
        Direction::Update => Sides {
            from: parent,
            into: branch,
            direction,
        },
        Direction::Save => Sides {
            from: branch,
            into: parent,
            direction,
        },
    })
}

/// A Move's comparison, read under the locks: the base may have moved meanwhile.
pub(super) fn moving(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
    match sides.direction {
        Direction::Update => Moving::update(tx, &sides.from, &sides.into),
        Direction::Save => Moving::save(tx, &sides.from, &sides.into),
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
