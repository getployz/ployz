//! Follow: what a Parent deploys arrives in each of its Branches as staged changes.
//! A row the Branch changed itself keeps the Branch's value, and the Parent's becomes
//! a hint. Each of the Parent's values is delivered once (its arrival): one the
//! Branch discards stays a hint until the Parent changes that row again.

use super::*;
use crate::Trusted;

/// A Parent's deployed value that followed into its Branch but isn't staged there:
/// the Branch changed the row itself, or discarded it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct FollowHint {
    /// The Parent it comes from: pass to [`Take::from`] to use it.
    pub from: EnvironmentName,
    /// What [`Take::rows`] names.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// The Parent's value; secrets read `{"secret": true}`.
    pub value: Value,
}

/// A staged change that arrived from the Parent's deploy by Follow and still holds
/// the value it arrived with, until it deploys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct IncomingChange {
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    pub from: EnvironmentName,
}

/// Parent `parent` deployed the nodes of `lineages` of its Saved revision `shipped`:
/// stage each change it made to them
/// in each of its Branches, and take back each node it removed that a Follow brought
/// there untouched. Changes a Branch refuses (one of its rules, say) stay
/// hints there, and never fail the Parent's Deployment. The caller holds the
/// Project's lock.
pub(crate) fn follow(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
    shipped: &SavedEnvironmentIntent,
    lineages: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let branches = tx.query(
        "SELECT environment_id, organization_id FROM config_environment_branch b \
         WHERE parent_id = ?1 AND NOT EXISTS (SELECT 1 FROM config_removal_mark m \
         WHERE m.environment_id = b.environment_id AND m.state = 'removing') ORDER BY environment_id",
        &[parent.as_str().into()],
    )?;
    if branches.is_empty() || lineages.is_empty() {
        return Ok(());
    }
    let parent = scope::load_by_id(tx, parent)?;
    // What both the Parent's Applied State and the revision it ships lack, it removed:
    // a forgotten Cluster empties Applied State alone, and a rollback ships a revision
    // older than the latest Saved State.
    let suffix_of_parent = suffix(tx, &parent)?;
    let applied = deployment::applied_state(tx, &parent.summary.id, &parent.working)?;
    let applied = Cells::of(&applied, &suffix_of_parent);
    let shipped = Cells::of(shipped, &suffix_of_parent);
    let removed = |lineage: &str| {
        let node = RowId::node(lineage);
        *applied.at(&node) == Cell::Absent && *shipped.at(&node) == Cell::Absent
    };
    for row in &branches {
        let branch = row.parse::<EnvironmentId>(0, "Branch")?;
        let shut_down = crate::removal::latest(tx, &branch)?.is_some_and(|removal| {
            !matches!(
                removal.status,
                crate::DeploymentStatus::Failed | crate::DeploymentStatus::Cancelled
            )
        });
        if shut_down || crate::removal::mark::naming(tx, &branch)?.is_some() {
            continue;
        }
        let who = Actor::system(row.parse(1, "Branch")?);
        match into(tx, &who, &parent, &branch, lineages) {
            Err(error) if crate::automation::skippable(&error) => {}
            done => done?,
        }
        // Take back what this Deployment removed, and each new node an earlier Follow
        // brought that the Parent has removed since: the runner confirms removals in
        // batches, each its own Follow, so a node kept for a referrer that a later
        // batch removed is tried again then (#3733). After the rest lands, so a node
        // the Parent unmounted in the same deploy is no longer mounted in the Branch
        // either. One the Branch's rules refuse, or another node still references, as
        // a Volume a Service still being withdrawn mounts, is tried again while others
        // go; what stays refused, the Branch keeps.
        let mut left: BTreeSet<String> = lineages
            .iter()
            .filter(|lineage| removed(lineage))
            .cloned()
            .collect();
        for arrived in arrived(tx, &branch, Which::From(&parent.summary.id))? {
            let lineage = arrived.row.lineage();
            if arrived.row == RowId::node(lineage)
                && matches!(arrived.how, How::Follow)
                && matches!(arrived.arrival, Arrival::Pending { .. })
                && removed(lineage)
            {
                left.insert(lineage.to_owned());
            }
        }
        let mut left: Vec<String> = left.into_iter().collect();
        loop {
            let tried = left.len();
            let mut refused = Vec::new();
            for lineage in left {
                let withdrawn = crate::storage::attempt(tx, |tx| {
                    let mut branch = scope::lock_id(tx, &who, &branch)?;
                    withdraw(tx, &mut branch, &parent.summary.id, &lineage)
                });
                match withdrawn {
                    Ok(false) => refused.push(lineage),
                    Err(error) if crate::automation::skippable(&error) => refused.push(lineage),
                    done => {
                        done?;
                    }
                }
            }
            if refused.is_empty() || refused.len() == tried {
                break;
            }
            left = refused;
        }
    }
    Ok(())
}

/// Stage in `branch` what `parent` deployed to `lineages` and hasn't delivered yet.
fn into(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &Environment,
    branch: &EnvironmentId,
    lineages: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let mut branch = scope::lock_id(tx, who, branch)?;
    let follow = Move::follow(tx, parent, &branch)?;
    let checked = follow.check(tx, &branch, Guard::Deploy)?;
    let delivered = arrivals(tx, &branch.summary.id, &parent.summary.id)?;
    let mut picks = BTreeSet::new();
    for row in checked.rows() {
        let Verdict::Moves { conflict, .. } = row.verdict else {
            continue;
        };
        if !lineages.contains(row.id.lineage()) || delivered.get(&row.id) == Some(&row.from) {
            continue;
        }
        // The Branch's own change wins: the Parent's value is a hint.
        if conflict {
            hint(
                tx,
                who,
                (&branch.summary.id, &parent.summary.id),
                &row.id,
                &row.from,
            )?;
        } else {
            picks.insert(row.id.clone());
        }
    }
    let mut picks = whole(checked.rows(), picks);
    let mut landing = checked.plan().landable(&picks);
    let none = BTreeMap::new();
    // A secret that would land Not set, as one sealed for the Parent does, never
    // replaces one the Branch holds with a value (#3389).
    if let Ok(applied) = checked.plan().apply(&landing, &none) {
        for landed in applied.landed {
            let holds = checked
                .rows()
                .iter()
                .any(|row| row.id == landed.row && matches!(row.into, Cell::Secret { .. }));
            if holds && landed.value == Cell::SecretWithoutValue {
                picks.remove(&landed.row);
                landing.remove(&landed.row);
            }
        }
    }
    if !landing.is_empty() {
        let landed = crate::storage::attempt(tx, |tx| {
            checked.apply(tx, who, &mut branch, &landing, &none, (Owner::None, None))
        });
        match landed {
            // One of the Branch's own rules refuses it: all of it is a hint.
            Err(error) if crate::automation::skippable(&error) => landing.clear(),
            landed => {
                landed?;
            }
        }
    }
    // What the Branch can't take stays a hint, as its own change does.
    for row in checked.rows() {
        if picks.contains(&row.id) && !landing.contains(&row.id) {
            hint(
                tx,
                who,
                (&branch.summary.id, &parent.summary.id),
                &row.id,
                &row.from,
            )?;
        }
    }
    Ok(())
}

/// The rows of `follow` into `branch` delivered at the value they carry now, yet not
/// staged: its hints.
fn hinted<'rows>(
    tx: &mut dyn Tx,
    branch: &EnvironmentId,
    parent: &EnvironmentId,
    rows: &'rows [PlannedRow],
) -> Result<Vec<&'rows PlannedRow>, RpcError> {
    let delivered = arrivals(tx, branch, parent)?;
    Ok(rows
        .iter()
        .filter(|row| {
            matches!(row.verdict, Verdict::Moves { .. })
                && delivered.get(&row.id) == Some(&row.from)
        })
        .collect())
}

/// What `branch`'s Parent deployed that followed into it but isn't staged there.
pub(crate) fn hints(tx: &mut dyn Tx, branch: &Environment) -> Result<Vec<FollowHint>, RpcError> {
    let Some(row) = row(tx, &branch.summary.id)? else {
        return Ok(Vec::new());
    };
    let parent = scope::load_by_id(tx, &row.parent)?;
    let follow = Move::follow(tx, &parent, branch)?;
    let plan = follow.plan(&branch.working);
    let names = parent.names();
    let mut hints = Vec::new();
    for row in hinted(tx, &branch.summary.id, &row.parent, plan.rows())? {
        let Some(at) = named(&[&follow.from, &branch.working], &row.id) else {
            continue;
        };
        hints.push(FollowHint {
            from: parent.summary.name.clone(),
            value: shown(&follow.from, &names, &row.id, &row.from),
            at,
        });
    }
    Ok(hints)
}

/// Stage the Follow hints `take` names (all of them when it names none) in the
/// Branch it names, whose Parent is `parent`.
pub(crate) fn take(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &EnvironmentName,
    take: &Take,
    trusted: &Trusted,
) -> Result<Taken, RpcError> {
    let Some(at) = &take.into else {
        return Err(error::invalid(
            format!("Name the Branch that follows {parent}"),
            json!({}),
        ));
    };
    let named = scope::environment(tx, who, at)?;
    let row = branch_row(tx, &named)?;
    scope::lock_project(tx, &named.summary.id)?;
    let (mut branch, from) = scope::load_pair(tx, who, (&named.summary.id, &row.parent), true)?;
    if from.summary.name != *parent {
        return Err(error::invalid(
            format!(
                "{} follows {}, not {parent}",
                branch.summary.name, from.summary.name
            ),
            json!({}),
        ));
    }
    let follow = Move::follow(tx, &from, &branch)?;
    let checked = follow.check(tx, &branch, Guard::Diff(&take.version))?;
    let asks = review::move_asks(tx, &branch, trusted)?;
    let hints: BTreeSet<RowId> = hinted(tx, &branch.summary.id, &row.parent, checked.rows())?
        .into_iter()
        .map(|row| row.id.clone())
        .collect();
    let named = named_in(&[&follow.from, &branch.working], &hints);
    let picks = chosen(take.rows.as_deref(), hints, &named, "hint")?;
    if picks.is_empty() {
        return Err(error::conflict(
            format!("No value from {parent} to use: read the diff again"),
            json!({}),
        ));
    }
    let none = BTreeMap::new();
    let staged = checked.apply(
        tx,
        who,
        &mut branch,
        &picks,
        &none,
        (Owner::None, Some(trusted)),
    )?;
    let asked = json!({ "version": take.version, "rows": take.rows });
    review::approve_move(
        tx,
        who,
        &branch,
        asks,
        review::Moving::Take(&from.summary, &asked),
    )?;
    Ok(Taken {
        from: from.summary,
        into: branch.summary,
        staged,
    })
}

/// What a Follow staged in `receiver` that still holds the Parent's deployed value
/// and isn't deployed yet. A later edit of it makes it the receiver's own.
pub(crate) fn incoming(
    tx: &mut dyn Tx,
    receiver: &Environment,
) -> Result<Vec<IncomingChange>, RpcError> {
    let pending = tx.query(
        "SELECT e.name, a.lineage, a.at, a.value FROM config_sync_arrival a \
         JOIN config_environment e ON e.id = a.other_id \
         WHERE a.environment_id = ?1 AND a.how = 'follow' AND a.state = 'pending' \
         ORDER BY a.lineage, a.at",
        &[receiver.summary.id.as_str().into()],
    )?;
    let working = Cells::of(&receiver.working, &suffix(tx, receiver)?);
    let mut incoming = Vec::new();
    for arrival in &pending {
        let row = row_id(arrival.text(1)?, arrival.text(2)?)?;
        let value: Cell = arrival.json(3, "Sync")?;
        if *working.at(&row) != value {
            continue;
        }
        if let Some(at) = named(&[&receiver.working], &row) {
            incoming.push(IncomingChange {
                at,
                from: arrival.parse(0, "Sync")?,
            });
        }
    }
    Ok(incoming)
}
