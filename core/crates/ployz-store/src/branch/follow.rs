//! Follow: what a Parent deploys arrives in each of its Branches as staged changes.
//! A row the Branch changed itself keeps the Branch's value, and the Parent's becomes
//! a hint. Each of the Parent's values is delivered once (its arrival): one the
//! Branch discards stays a hint until the Parent changes that row again.

use super::*;

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
    /// Where it came from.
    pub from: EnvironmentName,
}

/// Parent `parent` deployed the nodes of `lineages`: stage each change it made to them
/// in each of its Branches. Changes a Branch refuses (one of its rules, say) stay
/// hints there, and never fail the Parent's Deployment. The caller holds the
/// Project's lock.
pub(crate) fn follow(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
    lineages: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let branches = tx.query(
        "SELECT environment_id, organization_id FROM config_environment_branch \
         WHERE parent_id = ?1 AND closing = 0 ORDER BY environment_id",
        &[parent.as_str().into()],
    )?;
    if branches.is_empty() || lineages.is_empty() {
        return Ok(());
    }
    let parent = scope::load_by_id(tx, parent)?;
    for row in &branches {
        let branch = row.parse::<EnvironmentId>(0, "Branch")?;
        if crate::teardown::removing(tx, &branch)?.is_some() {
            continue;
        }
        let who = Actor::system(row.parse(1, "Branch")?);
        match into(tx, &who, &parent, &branch, lineages) {
            Err(error) if crate::automation::skippable(&error) => {}
            done => done?,
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
    let picks = whole(checked.rows(), picks);
    if !picks.is_empty() {
        let before = branch.summary.revision;
        let none = BTreeMap::new();
        follow.apply(tx, who, &mut branch, &checked, &picks, &none, None)?;
        crate::conditional_sync::followed(tx, &branch.summary, before)?;
    }
    Ok(())
}

/// The rows of `follow` into `branch` delivered at the value they carry now, yet not
/// staged: its hints.
fn hinted<'r>(
    tx: &mut dyn Tx,
    branch: &EnvironmentId,
    parent: &EnvironmentId,
    rows: &'r [PlannedRow],
) -> Result<Vec<&'r PlannedRow>, RpcError> {
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
    let hints: BTreeSet<RowId> = hinted(tx, &branch.summary.id, &row.parent, checked.rows())?
        .into_iter()
        .map(|row| row.id.clone())
        .collect();
    if let Some(unknown) = take.rows.iter().flatten().find(|row| !hints.contains(row)) {
        let rows: Vec<String> = hints.iter().map(ToString::to_string).collect();
        return Err(error::choices(
            format!("No hint {unknown} to take"),
            &unknown.to_string(),
            rows.iter().map(String::as_str),
        ));
    }
    let picks = match &take.rows {
        Some(rows) => rows.iter().cloned().collect(),
        None => hints,
    };
    if picks.is_empty() {
        return Err(error::conflict(
            format!("No value from {parent} to use: read the diff again"),
            json!({}),
        ));
    }
    let none = BTreeMap::new();
    let staged = follow.apply(tx, who, &mut branch, &checked, &picks, &none, None)?;
    Ok(Taken {
        from: from.summary,
        into: branch.summary,
        staged,
        conditional_sync: None,
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
    let suffix = suffix(tx, receiver)?;
    let mut incoming = Vec::new();
    for arrival in &pending {
        let row = row_id(arrival.text(1)?, arrival.text(2)?)?;
        let value: Cell = arrival.json(3, "Sync")?;
        if cell_at(&receiver.working, &row, &suffix) != value {
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
