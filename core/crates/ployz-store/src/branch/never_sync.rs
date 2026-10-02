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
    let candidates = match request.off {
        true => never_synced(tx, &environment)?,
        false => named_in(
            &[&environment.working],
            Cells::of(&environment.working, "").rows(),
        ),
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
        if named(&[&environment.working], row).is_none() {
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

/// The rows `environment` marks Never sync; a mark on a node it no longer has is
/// left out.
pub(crate) fn never_synced(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<Vec<NamedRow>, RpcError> {
    let mut marked = named_in(
        &[&environment.working],
        &marked(tx, &environment.summary.id)?,
    );
    marked.sort_by_key(NamedRow::label);
    Ok(marked)
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
