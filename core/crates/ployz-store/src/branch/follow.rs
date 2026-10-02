//! Follow: what a Parent deploys arrives in each of its Branches as staged changes.
//! A setting the Branch changed itself keeps the Branch's value, and the Parent's
//! becomes a Use hint. Each of the Parent's changes is delivered once
//! (`config_followed`): one the Branch discards stays a hint until the Parent changes
//! that setting again.

use super::*;

/// A Parent's deployed value that followed into its Branch but isn't staged there:
/// the Branch changed the setting itself, or discarded it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct FollowHint {
    /// The Parent it comes from: pass to [`Take::from`] to use it.
    pub from: EnvironmentName,
    /// `NODE.path`, as a Sync names it.
    pub row: String,
    /// The setting as the Branch addresses it: what [`Take::rows`] names.
    pub path: SettingPath,
    /// It is a whole Service or Volume, not one of its settings.
    pub whole: bool,
    /// The Parent's value; secrets read `{"secret": true}`.
    pub value: Value,
}

/// A staged change that arrived from the Parent's deploy by Follow and still holds
/// the value it arrived with, until it deploys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct IncomingChange {
    /// `NODE`, or `NODE.path` for one of its settings or variables.
    pub row: String,
    /// The setting as the receiver addresses it.
    pub path: SettingPath,
    /// It is a whole Service or Volume, not one of its settings.
    pub whole: bool,
    /// Where it came from.
    pub from: EnvironmentName,
}

/// Parent `parent` deployed the nodes of `lineages`: stage each change it made to them
/// in each of its Branches. Changes a Branch refuses (one of its rules, say) stay
/// hints there, and never fail the Parent's Deployment.
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
    let ids = branches
        .iter()
        .map(|row| row.parse::<EnvironmentId>(0, "Branch"))
        .collect::<Result<Vec<_>, _>>()?;
    // The Parent and every Branch at once, in ID order, as a Sync locks its sides.
    scope::lock_all(tx, ids.iter().cloned().chain([parent.clone()]))?;
    let parent = scope::load_by_id(tx, parent)?;
    for (row, branch) in branches.iter().zip(&ids) {
        if crate::teardown::removing(tx, branch)?.is_some() {
            continue;
        }
        let who = Actor::system(row.parse(1, "Branch")?);
        match into(tx, &who, &parent, branch, lineages) {
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
    let follow = Comparison::follow(tx, parent, &branch)?;
    let changes = follow.compare(&branch.working, None)?;
    let delivered = delivered(tx, &branch.summary.id)?;
    let mut picks = Vec::new();
    for row in &changes.rows {
        let BranchRole::Move { conflict } = &row.role else {
            continue;
        };
        let key = row.key.to_string();
        let (lineage, path) = split_row_key(&key);
        let value = row.from.to_string();
        if !lineages.contains(lineage) || delivered.get(&key) == Some(&value) {
            continue;
        }
        tx.execute(
            "INSERT INTO config_followed (environment_id, lineage, path, organization_id, value) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (environment_id, lineage, path) DO UPDATE SET value = excluded.value",
            &[
                branch.summary.id.as_str().into(),
                lineage.into(),
                path.into(),
                who.organization.as_str().into(),
                value.as_str().into(),
            ],
        )?;
        // The Branch's own change wins: the Parent's value is a hint.
        if !conflict {
            picks.push(key);
        }
    }
    if !picks.is_empty() {
        let before = branch.summary.revision;
        follow.apply(tx, who, &mut branch, picks)?;
        crate::conditional_sync::followed(tx, &branch.summary, before)?;
    }
    Ok(())
}

/// What `branch`'s Parent deployed that followed into it but isn't staged there.
pub(crate) fn hints(tx: &mut dyn Tx, branch: &Environment) -> Result<Vec<FollowHint>, RpcError> {
    let delivered = delivered(tx, &branch.summary.id)?;
    let Some(row) = row(tx, &branch.summary.id)?.filter(|_| !delivered.is_empty()) else {
        return Ok(Vec::new());
    };
    let parent = scope::load_by_id(tx, &row.parent)?;
    let follow = Comparison::follow(tx, &parent, branch)?;
    let changes = follow.compare(&branch.working, None)?;
    hinted(&delivered, &changes.rows)
        .into_iter()
        .map(|row| {
            let (label, value, _) = shown_row(&follow, &parent, branch, row);
            let key = row.key.to_string();
            Ok(FollowHint {
                from: parent.summary.name.clone(),
                row: label,
                path: path_of(&follow.from, &branch.working, &key)?,
                whole: split_row_key(&key).1 == "node",
                value,
            })
        })
        .collect()
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
    review::check(&review::review(tx, &branch)?, Some(&take.version))?;
    let follow = Comparison::follow(tx, &from, &branch)?;
    let changes = follow.compare(&branch.working, None)?;
    let delivered = delivered(tx, &branch.summary.id)?;
    let mut hints = Vec::new();
    for row in hinted(&delivered, &changes.rows) {
        let key = row.key.to_string();
        hints.push((
            path_of(&follow.from, &branch.working, &key)?.to_string(),
            key,
        ));
    }
    if let Some(unknown) = take
        .rows
        .iter()
        .flatten()
        .find(|asked| !hints.iter().any(|(path, _)| covers(path, asked)))
    {
        return Err(error::choices(
            format!("No hint {unknown} to take"),
            unknown,
            hints.iter().map(|(path, _)| path.as_str()),
        ));
    }
    let picks: Vec<String> = hints
        .into_iter()
        .filter(|(path, _)| {
            take.rows
                .as_ref()
                .is_none_or(|asked| asked.iter().any(|asked| covers(path, asked)))
        })
        .map(|(_, key)| key)
        .collect();
    if picks.is_empty() {
        return Err(error::conflict(
            format!("No value from {parent} to use: read the diff again"),
            json!({}),
        ));
    }
    let staged = follow.apply(tx, who, &mut branch, picks)?;
    Ok(Taken {
        from: from.summary,
        into: branch.summary,
        staged,
        conditional_sync: None,
    })
}

/// What a Follow staged in `receiver` that still holds the Parent's deployed value
/// and isn't deployed yet. A later edit of it, or a Sync, makes it the receiver's own.
pub(crate) fn incoming(
    tx: &mut dyn Tx,
    receiver: &Environment,
) -> Result<Vec<IncomingChange>, RpcError> {
    let pending = tx.query(
        "SELECT e.name, p.lineage, p.path, p.other_id FROM config_sync_pending p \
         JOIN config_environment e ON e.id = p.other_id \
         WHERE p.environment_id = ?1 AND p.arrived = 'follow' ORDER BY p.lineage, p.path",
        &[receiver.summary.id.as_str().into()],
    )?;
    let Some(first) = pending.first() else {
        return Ok(Vec::new());
    };
    let parent = scope::load_by_id(tx, &first.parse(3, "Sync")?)?;
    let applied = deployment::head(tx, &parent)?.applied;
    let edited = changed_from(&applied, &receiver.working)?;
    let working = &receiver.working;
    let mut incoming = Vec::new();
    let name_in = |lineage: &str| name_of(working, lineage).unwrap_or_else(|| lineage.to_owned());
    for row in &pending {
        let (lineage, path) = (row.text(1)?, row.text(2)?);
        let key = format!("{lineage}:{path}");
        let Some(node) = name_of(working, lineage) else {
            continue;
        };
        if edited.contains(&key) {
            continue;
        }
        incoming.push(IncomingChange {
            row: match path {
                "node" => node,
                path => SettingPath::from_core(&node, path, name_in),
            },
            path: path_of(&applied, working, &key)?,
            whole: path == "node",
            from: row.parse(0, "Sync")?,
        });
    }
    Ok(incoming)
}

/// The Move rows of a Follow that were `delivered` at the value they carry now, yet
/// aren't staged: its hints.
fn hinted<'r>(delivered: &BTreeMap<String, String>, rows: &'r [BranchRow]) -> Vec<&'r BranchRow> {
    rows.iter()
        .filter(|row| {
            matches!(row.role, BranchRole::Move { .. })
                && delivered.get(&row.key.to_string()) == Some(&row.from.to_string())
        })
        .collect()
}

/// The Parent's value each setting of `branch` was last delivered at, by row key.
fn delivered(
    tx: &mut dyn Tx,
    branch: &EnvironmentId,
) -> Result<BTreeMap<String, String>, RpcError> {
    tx.query(
        "SELECT lineage, path, value FROM config_followed WHERE environment_id = ?1",
        &[branch.as_str().into()],
    )?
    .iter()
    .map(|row| {
        Ok((
            format!("{}:{}", row.text(0)?, row.text(1)?),
            row.text(2)?.to_owned(),
        ))
    })
    .collect()
}
