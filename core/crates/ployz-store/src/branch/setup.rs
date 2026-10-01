//! A Branch's Setup Commands: those an Environment hands each new Branch of it.

use super::*;

/// Set the Setup Commands a new Branch of an Environment runs when it names none:
/// each runs if the Branch copies its Service. Empty clears them. Takes effect at once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetBranchSetup {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub setup: Vec<crate::SetupCommand>,
}

pub(crate) fn set_branch_setup(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetBranchSetup,
) -> Result<crate::EnvironmentsView, RpcError> {
    let environment = scope::lock(tx, who, &set.environment)?;
    for setup in &set.setup {
        setup_command(setup)?;
    }
    // Setup Commands live beside Working State, so they pass the same rules here.
    let facts = |setup| crate::rules::Facts {
        working: &environment.working,
        live: &environment.live,
        setup,
    };
    let before = branch_setup(tx, &environment.summary.id)?;
    crate::rules::check_write(
        &environment.summary,
        facts(&before),
        None,
        facts(&set.setup),
    )?;
    let setup = (!set.setup.is_empty())
        .then(|| serde_json::to_string(&set.setup).expect("Setup Commands are JSON"));
    tx.execute(
        "UPDATE config_environment SET branch_setup = ?1 WHERE id = ?2",
        &[
            setup.as_deref().into(),
            environment.summary.id.as_str().into(),
        ],
    )?;
    crate::teardown::environments(
        tx,
        who,
        &crate::EnvironmentsQuery {
            project: Some(environment.summary.project),
        },
    )
}

/// What a new Branch of `parent` runs when it names no Setup Commands.
pub(crate) fn branch_setup(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
) -> Result<Vec<crate::SetupCommand>, RpcError> {
    let rows = tx.query(
        "SELECT branch_setup FROM config_environment WHERE id = ?1 AND branch_setup IS NOT NULL",
        &[parent.as_str().into()],
    )?;
    rows.first()
        .map_or(Ok(Vec::new()), |row| row.json(0, "Branch setup"))
}
