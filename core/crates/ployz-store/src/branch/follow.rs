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
    /// The Parent's value; secrets read `{"secret": true}`.
    pub value: Value,
}

/// A staged change that arrived from another Environment, by Sync or Follow, until it
/// deploys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct IncomingChange {
    /// `NODE`, or `NODE.path` for one of its settings or variables.
    pub row: String,
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
    let moving = Moving::update(tx, parent, &branch)?;
    let changes = moving.compare(&branch.working, None)?;
    let delivered = delivered(tx, &branch.summary.id)?;
    let mut picks = Vec::new();
    for row in &changes.rows {
        let BranchRole::Move { conflict, choice } = &row.role else {
            continue;
        };
        let key = row.key.to_string();
        let (lineage, path) = split(&key);
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
            picks.push(from_parent(key, choice.as_ref()));
        }
    }
    if !picks.is_empty() {
        moving.apply(tx, who, &mut branch, picks)?;
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
    let moving = Moving::update(tx, &parent, branch)?;
    let changes = moving.compare(&branch.working, None)?;
    Ok(hinted(&delivered, &changes.rows)
        .into_iter()
        .map(|row| {
            let (label, value, _) = shown_row(&moving, &parent, branch, row);
            FollowHint {
                from: parent.summary.name.clone(),
                row: label,
                value,
            }
        })
        .collect())
}

/// Stage the Follow hints `take` names (all of them when it names none) in the
/// Branch it names, whose Parent is `parent`.
pub(crate) fn take(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &EnvironmentName,
    take: &Take,
) -> Result<Moved, RpcError> {
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
    if take.version.is_some() {
        review::check(&review::review(tx, &branch)?, take.version.as_deref())?;
    }
    let moving = Moving::update(tx, &from, &branch)?;
    let changes = moving.compare(&branch.working, None)?;
    let delivered = delivered(tx, &branch.summary.id)?;
    let hints: Vec<(String, &BranchRow)> = hinted(&delivered, &changes.rows)
        .into_iter()
        .map(|row| (moving.name(&branch.working, &row.key.to_string()), row))
        .collect();
    let mut picks = Vec::new();
    for (label, row) in &hints {
        let asked = take
            .rows
            .as_ref()
            .is_none_or(|asked| asked.iter().any(|asked| under(label, asked)));
        if let (true, BranchRole::Move { choice, .. }) = (asked, &row.role) {
            picks.push(from_parent(row.key.to_string(), choice.as_ref()));
        }
    }
    if let Some(unknown) = take
        .rows
        .iter()
        .flatten()
        .find(|asked| !hints.iter().any(|(label, _)| under(label, asked)))
    {
        return Err(error::choices(
            format!("No hint named {unknown} to take"),
            unknown,
            hints.iter().map(|(label, _)| label.as_str()),
        ));
    }
    if picks.is_empty() {
        return Err(error::conflict(
            format!("No value from {parent} to use: read the diff again"),
            json!({}),
        ));
    }
    let staged = moving.apply(tx, who, &mut branch, picks)?;
    Ok(Moved {
        branch: Some(view(tx, &branch)?),
        from: from.summary,
        into: branch.summary,
        staged,
        conditional_save: None,
    })
}

/// What landed in `receiver` from another Environment and isn't deployed yet.
pub(crate) fn incoming(
    tx: &mut dyn Tx,
    receiver: &Environment,
) -> Result<Vec<IncomingChange>, RpcError> {
    let pending = tx.query(
        "SELECT e.name, p.lineage, p.path FROM config_sync_pending p \
         JOIN config_environment e ON e.id = p.other_id \
         WHERE p.environment_id = ?1 ORDER BY e.name, p.lineage, p.path",
        &[receiver.summary.id.as_str().into()],
    )?;
    let working = &receiver.working;
    let mut incoming = Vec::new();
    for row in &pending {
        let (lineage, path) = (row.text(1)?, row.text(2)?);
        let Some(node) = name_of(working, lineage) else {
            continue;
        };
        let name_in =
            |lineage: &str| name_of(working, lineage).unwrap_or_else(|| lineage.to_owned());
        incoming.push(IncomingChange {
            row: match path {
                "node" => node,
                path => SettingPath::from_core(&node, path, name_in),
            },
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

/// Pick row `key` with the Parent's value, a secret's sealed value included.
fn from_parent(key: String, choice: Option<&BranchChoice>) -> BranchPick {
    BranchPick {
        key,
        choice: choice.map(|_| BranchPickChoice::From),
    }
}
