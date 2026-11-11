//! Follow: what a Parent deploys arrives in each of its Branches as staged changes.
//! A row the Branch changed itself keeps the Branch's value, and the Parent's becomes
//! a hint. Each of the Parent's values is delivered once (its arrival): one the
//! Branch discards stays a hint until the Parent changes that row again.

use super::*;
use crate::Trusted;
use crate::removal::mark;
use ployz_core::RpcErrorCode;

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

/// Parent `parent` deployed the nodes of `lineages`, shipping its Saved revision
/// `shipped`: stage each change it made to them in each of its Branches, and take
/// back each node it removed that a Follow brought there untouched. Changes a Branch refuses (one of its rules, say) stay
/// hints there, and never fail the Parent's Deployment; nor does a Branch another
/// write holds past [`BRANCH_LOCK_WAIT`], which takes them at a later deploy or by a
/// Sync, its arrivals unrecorded (#3788). With `seal_below`, a seal a
/// Branch holds back also reaches the Branches below it that inherited the value:
/// once per Deployment, as it ends (#3780). The caller holds the Project's lock.
///
/// It runs as the Parent's runner records, with no Cloud call before it, so it holds no
/// evidence, of the Cluster Domain neither: a Door switch may have replaced the one the
/// Deployment froze, and a prefix it brings expands under the one the Branch deploys with.
pub(crate) fn follow(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
    shipped: &SavedEnvironmentIntent,
    lineages: &BTreeSet<String>,
    seal_below: bool,
) -> Result<(), RpcError> {
    let trusted = &Trusted::default();
    let branches = tx.query(
        &format!(
            "SELECT environment_id, organization_id FROM config_environment_branch b \
             WHERE parent_id = ?1 AND {} ORDER BY environment_id",
            mark::unmarked("b.environment_id")
        ),
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
        if stopped(tx, &branch)? {
            continue;
        }
        let who = Actor::system(row.parse(1, "Branch")?);
        let sealed = match crate::storage::attempt(tx, |tx| lock_below(tx, &who, &branch)) {
            Err(error) if error.code == RpcErrorCode::Unavailable => {
                tracing::warn!(
                    branch = branch.as_str(),
                    code = error.code.as_str(),
                    error = error.message.as_str(),
                    "couldn't follow into a Branch; it takes the changes at a later deploy"
                );
                continue;
            }
            locked => {
                match locked
                    .and_then(|mut locked| into(tx, &who, &parent, &mut locked, lineages, trusted))
                {
                    Err(error) if crate::automation::skippable(&error) => Vec::new(),
                    done => done?,
                }
            }
        };
        if seal_below && !sealed.is_empty() {
            let below = crate::storage::attempt(tx, |tx| {
                seal_inherited_below(tx, &who, &parent.summary.id, &branch, &sealed)
            });
            if let Err(error) = below {
                tracing::warn!(
                    branch = branch.as_str(),
                    code = error.code.as_str(),
                    error = error.message.as_str(),
                    "couldn't seal values below a Branch"
                );
            }
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
                    withdraw(tx, &mut branch, &parent.summary.id, &lineage, trusted)
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

/// How long a Parent's Deployment waits for each Branch's lock as it ends (#3780,
/// #3788). A Branch another write holds longer is left for a later deploy.
pub(super) const BRANCH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// Lock `branch` as [`scope::lock_id`] does, waiting at most [`BRANCH_LOCK_WAIT`],
/// and only the wait: what it writes then keeps the usual timeout. Call it in a
/// [`crate::storage::attempt`], whose savepoint rolled back drops the shorter wait
/// too. Another write holding it longer is [`RpcErrorCode::Unavailable`].
pub(super) fn lock_below(
    tx: &mut dyn Tx,
    who: &Actor,
    branch: &EnvironmentId,
) -> Result<Environment, RpcError> {
    tx.wait_for_locks(Some(BRANCH_LOCK_WAIT))?;
    let branch = scope::lock_id(tx, who, branch)?;
    tx.wait_for_locks(None)?;
    Ok(branch)
}

/// Whether `branch` was shut down, or is being removed: nothing follows into it.
pub(super) fn stopped(tx: &mut dyn Tx, branch: &EnvironmentId) -> Result<bool, RpcError> {
    let shut_down = crate::deployment::latest_removal(tx, branch)?.is_some_and(|removal| {
        !matches!(
            removal.status,
            crate::DeploymentStatus::Failed | crate::DeploymentStatus::Cancelled
        )
    });
    Ok(shut_down || crate::removal::mark::naming(tx, branch)?.is_some())
}

/// Stage in `branch`, locked, what `parent` deployed to `lineages` and hasn't
/// delivered yet, unless a hostname it brings could be a custom domain's under any
/// Cluster Domain. Answers each variable `parent` sealed that the Branch's own change
/// holds back.
fn into(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &Environment,
    branch: &mut Environment,
    lineages: &BTreeSet<String>,
    trusted: &Trusted,
) -> Result<Vec<RowId>, RpcError> {
    let before = Cells::of(&branch.working, "");
    let follow = Move::follow(tx, parent, branch)?;
    let checked = follow.check(tx, branch, Guard::Deploy)?;
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
            let was = branch.working.clone();
            checked.apply(tx, who, branch, &landing, &none, (Owner::None, trusted))?;
            crate::domain::refuse_new_shared_hostnames(
                tx,
                branch,
                crate::domain::Under::Any,
                (&was, &branch.working),
            )
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
    // A seal the Branch's own change, a plain value or a deletion, holds back, as a
    // hint or with none at all; never one it marks Never sync. Read against its
    // Working State before this Follow, so a seal that landed isn't one, nor one the
    // Branch holds a secret of.
    let (from, base, now) = (
        Cells::of(&follow.from, ""),
        Cells::of(&follow.base, ""),
        before,
    );
    Ok(from
        .rows()
        .filter(|row| {
            let (from, base, now) = (from.at(row), base.at(row), now.at(row));
            matches!(row.at(), At::Variable(_))
                && lineages.contains(row.lineage())
                && !follow.rules.into_marks.contains(row)
                && from.is_secret()
                && !now.is_secret()
                && from != base
                && now != base
                && now != from
        })
        .cloned()
        .collect())
}

/// The rows of `follow` into `branch` delivered at the value they carry now, yet not
/// staged: its hints. Never a plain value an Environment above sealed since: a
/// Branch never gets a secret from above, so neither a hint nor a take offers it
/// back (#3794).
fn hinted<'rows>(
    tx: &mut dyn Tx,
    branch: &EnvironmentId,
    parent: &EnvironmentId,
    rows: &'rows [PlannedRow],
) -> Result<Vec<&'rows PlannedRow>, RpcError> {
    let delivered = arrivals(tx, branch, parent)?;
    let mut sealed = review::sealed::AncestorSeals::default();
    let mut hinted = Vec::new();
    for row in rows {
        if !matches!(row.verdict, Verdict::Moves { .. })
            || delivered.get(&row.id) != Some(&row.from)
        {
            continue;
        }
        if let (At::Variable(key), Some(fingerprint)) =
            (row.id.at(), super::pair::plain_fingerprint(&row.from))
            && sealed.sealed(tx, branch.as_str(), row.id.lineage(), key, fingerprint)?
        {
            continue;
        }
        hinted.push(row);
    }
    Ok(hinted)
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
    let staged = checked.apply(tx, who, &mut branch, &picks, &none, (Owner::None, trusted))?;
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
