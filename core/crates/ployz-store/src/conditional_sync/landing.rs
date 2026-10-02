//! Where a frozen Conditional Sync lands: decided from the documents alone, so the
//! caller only writes what this returns.

use super::*;

/// What landing a Conditional Sync does.
pub(super) struct Planned {
    /// The Destination's next Saved State.
    pub(super) saved: SavedEnvironmentIntent,
    /// Its next Working State.
    pub(super) next: SavedEnvironmentIntent,
    /// Every pick that landed, Saved's then Working's.
    pub(super) picks: Vec<String>,
    /// The rows left as hints or staged changes; none deletes the Conditional Sync.
    pub(super) left: Vec<Row>,
}

/// Land `stored` onto the Destination's `latest` Saved State and its `working`
/// State. A value lands only where the Destination still holds what was reviewed.
pub(super) fn plan(
    stored: &Stored,
    latest: &SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Result<Planned, RpcError> {
    let values_in = |into: &SavedEnvironmentIntent| -> Result<BTreeMap<String, Value>, RpcError> {
        Ok(against(stored, into, None)?
            .rows
            .into_iter()
            .filter(|row| matches!(row.role, BranchRole::Move { .. }))
            .map(|row| (row.key.to_string(), row.into))
            .collect())
    };
    let in_saved = values_in(latest)?;
    let in_working = values_in(working)?;
    let reviewed: BTreeMap<&str, &Value> = stored
        .rows
        .iter()
        .map(|row| (row.key.as_str(), &row.into))
        .collect();
    let same = |left: Option<&Value>, right: Option<&Value>| {
        left.unwrap_or(&Value::Null) == right.unwrap_or(&Value::Null)
    };
    let unchanged = |key: &str| {
        let reviewed = reviewed.get(key).copied();
        same(in_saved.get(key), reviewed) || same(in_working.get(key), reviewed)
    };
    let lineage = |key: &str| split_row_key(key).0.to_owned();
    let nodes = |picks: &[&String]| -> BTreeSet<String> {
        picks
            .iter()
            .filter(|pick| split_row_key(pick).1 == "node")
            .map(|pick| lineage(pick))
            .collect()
    };
    let all: Vec<&String> = stored.picks.iter().map(|pick| &pick.key).collect();
    let introduced = nodes(&all);

    // 1. Saved. A node the pull request introduced arrives with its variables, or not at all.
    let to_saved: Vec<&String> = all
        .iter()
        .copied()
        .filter(|pick| in_saved.contains_key(*pick) && unchanged(pick))
        .collect();
    let arriving = nodes(&to_saved);
    let saved_picks: Vec<String> = to_saved
        .into_iter()
        .filter(|pick| {
            let lineage = lineage(pick);
            !introduced.contains(&lineage) || arriving.contains(&lineage)
        })
        .cloned()
        .collect();
    let saved = match saved_picks.is_empty() {
        true => latest.clone(),
        false => against(stored, latest, Some(saved_picks.clone()))?.next,
    };

    // 2. Working: arriving nodes as Saved has them, so their ids match; then the rest,
    // where the Destination has no staged edit of its own, with the variables Saved
    // gained under Saved's ids.
    let mut into = working.clone();
    into.services.extend(
        saved
            .services
            .iter()
            .filter(|node| arriving.contains(&node.lineage_id))
            .cloned(),
    );
    into.volumes.extend(
        saved
            .volumes
            .iter()
            .filter(|node| arriving.contains(&node.resource_lineage_id))
            .cloned(),
    );
    let working_picks: Vec<String> = all
        .iter()
        .filter(|pick| {
            !introduced.contains(&lineage(pick))
                && in_working.contains_key(**pick)
                && same(in_saved.get(**pick), in_working.get(**pick))
        })
        .map(|pick| (*pick).clone())
        .collect();
    let next = match working_picks.is_empty() {
        true => into,
        false => {
            let next = against(stored, &into, Some(working_picks.clone()))?.next;
            with_variable_ids_of(&saved, &into, next)
        }
    };

    let staged: BTreeSet<&str> = working_picks.iter().map(String::as_str).collect();
    let left: Vec<Row> = stored
        .rows
        .iter()
        .filter(|row| {
            (in_saved.contains_key(&row.key) || in_working.contains_key(&row.key))
                && !unchanged(&row.key)
        })
        .map(|row| Row {
            landed: Some(match staged.contains(row.key.as_str()) {
                true => Landed::Staged,
                false => Landed::Hint,
            }),
            ..row.clone()
        })
        .collect();
    Ok(Planned {
        saved,
        next,
        picks: saved_picks.iter().chain(&working_picks).cloned().collect(),
        left,
    })
}
