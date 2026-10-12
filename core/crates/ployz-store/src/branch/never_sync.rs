//! Never sync: rows an Environment keeps as its own. A Sync never carries one from
//! that Environment nor changes it there, but a Branch of it still gets its value:
//! the mark doesn't carry into children.

use super::*;

/// Mark rows of an Environment Never sync, or sync them again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NeverSync {
    /// The Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its rows, by name in its own configuration (with `off`, among its marks), or
    /// by RowId. Each names a node the Environment has, but not necessarily a
    /// variable it has yet: marking a sender's new variable keeps it out of the
    /// receiver.
    pub rows: Vec<RowRef>,
    /// Sync them again.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub off: bool,
}

/// An Environment's rows marked Never sync, after a change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NeverSynced {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// Every row it marks Never sync.
    pub never_synced: Vec<NamedRow>,
}

pub(crate) fn never_sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &NeverSync,
) -> Result<NeverSynced, RpcError> {
    let environment = scope::lock(tx, who, &request.environment)?;
    let sides = sides(tx, &environment)?;
    let sides: Vec<_> = sides.iter().collect();
    let candidates = match request.off {
        true => never_synced(tx, &environment)?,
        false => {
            // A setting at its default has no cell, yet is a row a mark can name.
            let mut rows = BTreeSet::new();
            for side in &sides {
                rows.extend(Cells::of(side, "").rows().cloned());
                rows.extend(side.services.iter().flat_map(|service| {
                    Setting::ALL.map(|setting| RowId::of(&service.lineage_id, At::Setting(setting)))
                }));
            }
            rows.iter().filter_map(|row| named(&sides, row)).collect()
        }
    };
    for row in &resolve_all(&request.rows, &candidates)? {
        let at = row.at().to_string();
        let key = [
            environment.summary.id.as_str().into(),
            row.lineage().into(),
            at.as_str().into(),
        ];
        if request.off {
            tx.execute(
                "DELETE FROM config_never_sync \
                 WHERE environment_id = ?1 AND lineage = ?2 AND at = ?3",
                &key,
            )?;
            continue;
        }
        if named(&sides, row).is_none() {
            return Err(error::invalid(
                format!("{} has nothing at {row}", environment.summary.name),
                json!({ "row": row }),
            ));
        }
        let [id, lineage, at] = key;
        tx.execute(
            "INSERT INTO config_never_sync (environment_id, lineage, at, organization_id) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (environment_id, lineage, at) DO NOTHING",
            &[id, lineage, at, who.organization.as_str().into()],
        )?;
    }
    Ok(NeverSynced {
        never_synced: never_synced(tx, &environment)?,
        environment: environment.summary,
    })
}

/// The rows `environment` marks Never sync; a mark on a node neither it nor what it
/// last shared with its Parent has is left out.
pub(crate) fn never_synced(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<Vec<NamedRow>, RpcError> {
    let sides = sides(tx, environment)?;
    let sides: Vec<_> = sides.iter().collect();
    let mut marked: Vec<_> = marked(tx, &environment.summary.id)?
        .iter()
        .filter_map(|row| named(&sides, row))
        .collect();
    marked.sort_by_key(NamedRow::to_string);
    Ok(marked)
}

/// Where `environment`'s rows are: its Working State, and for a Branch what it last
/// shared with its Parent, so a row it removed can be marked too and stays put.
fn sides(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<Vec<SavedEnvironmentIntent>, RpcError> {
    let mut sides = vec![environment.working.clone()];
    if let Some(branch) = row(tx, &environment.summary.id)? {
        sides.push(base_of(tx, (&environment.summary.id, &branch.parent))?);
    }
    Ok(sides)
}

/// The rows `environment` marks Never sync.
pub(crate) fn marked(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeSet<RowId>, RpcError> {
    let rows = tx.query(
        "SELECT lineage, at FROM config_never_sync WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    rows.iter()
        .map(|row| row_id(row.text(0)?, row.text(1)?))
        .collect()
}

/// What a move from `from` into `into` never carries, as `from`'s marks and
/// `into`'s: `from`'s count unless `into` is one of its Branches.
pub(crate) fn marks(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<(BTreeSet<RowId>, BTreeSet<RowId>), RpcError> {
    let from_marks = match row(tx, into)? {
        Some(branch) if branch.parent == *from => BTreeSet::new(),
        _ => marked(tx, from)?,
    };
    Ok((from_marks, marked(tx, into)?))
}
