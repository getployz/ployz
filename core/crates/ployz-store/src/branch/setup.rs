//! A Branch's Setup Commands: its own, run in its Services before they first deploy
//! there, and those an Environment hands each new Branch of it.

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

/// A Branch's own Setup Commands: each runs in its Service before it first deploys
/// in the Branch.
pub(crate) struct Own {
    setup: Vec<Setup>,
    deployed: BTreeSet<String>,
    branch: EnvironmentName,
}

impl Own {
    /// `environment`'s own Setup Commands; none unless it is a Branch.
    pub(crate) fn of(tx: &mut dyn Tx, environment: &Environment) -> Result<Option<Self>, RpcError> {
        row(tx, &environment.summary.id)?
            .map(|row| Self::from_row(tx, environment, row))
            .transpose()
    }

    /// `environment`'s own Setup Commands, or why only a Branch has them.
    pub(crate) fn of_branch(tx: &mut dyn Tx, environment: &Environment) -> Result<Self, RpcError> {
        let row = branch_row(tx, environment)?;
        Self::from_row(tx, environment, row)
    }

    fn from_row(tx: &mut dyn Tx, environment: &Environment, row: Row) -> Result<Self, RpcError> {
        Ok(Self {
            setup: row.setup,
            deployed: deployed(tx, &environment.summary.id)?,
            branch: environment.summary.name.clone(),
        })
    }

    fn commands(&self, service: &SavedServiceIntent) -> Vec<&str> {
        self.setup
            .iter()
            .filter(|setup| setup.lineage == service.lineage_id)
            .map(|setup| setup.command.as_str())
            .collect()
    }

    /// What `setup.SERVICE` reads: its command, each of them when it has several, or
    /// `null`.
    pub(crate) fn value(&self, service: &SavedServiceIntent) -> Value {
        match self.commands(service).as_slice() {
            [] => Value::Null,
            [command] => json!(command),
            commands => json!(commands),
        }
    }

    /// Why `service`'s Setup Command won't run at the next Deploy: it deployed already.
    pub(crate) fn note(&self, service: &SavedServiceIntent) -> Option<String> {
        (self.deployed.contains(&service.id) && !self.commands(service).is_empty()).then(|| {
            format!(
                "{} has deployed in {}, so setup.{} won't run",
                service.slug, self.branch, service.slug
            )
        })
    }

    /// The Setup Commands the next Deploy of `working` runs: those of its Services
    /// that haven't deployed in the Branch.
    pub(crate) fn pending(&self, working: &SavedEnvironmentIntent) -> Vec<SetupCommand> {
        let mut pending = Vec::new();
        for service in &working.services {
            let Ok(name) = ServiceName::parse(service.slug.as_str()) else {
                continue;
            };
            if self.deployed.contains(&service.id) {
                continue;
            }
            pending.extend(
                self.commands(service)
                    .into_iter()
                    .map(|command| SetupCommand {
                        service: name.clone(),
                        command: command.to_owned(),
                    }),
            );
        }
        pending
    }
}

/// Set or, with `None`, remove the Setup Command Branch `environment` runs in
/// `service` before it first deploys there. Whether it changed anything.
pub(crate) fn set_own(
    tx: &mut dyn Tx,
    environment: &Environment,
    service: &ServiceName,
    value: Option<Value>,
) -> Result<bool, RpcError> {
    refuse_service_named_setup(environment)?;
    let row = branch_row(tx, environment)?;
    let lineage = environment.service(service)?.lineage_id.clone();
    let command = value
        .map(|value| {
            let Value::String(command) = value else {
                return Err(error::invalid(
                    format!("setup.{service}: expected a shell command"),
                    json!({ "example": "pnpm db:seed" }),
                ));
            };
            setup_command(&SetupCommand {
                service: service.clone(),
                command,
            })
        })
        .transpose()?;
    let before: Vec<&str> = row
        .setup
        .iter()
        .filter(|setup| setup.lineage == lineage)
        .map(|setup| setup.command.as_str())
        .collect();
    if before == command.as_deref().into_iter().collect::<Vec<_>>() {
        return Ok(false);
    }
    let mut setup: Vec<Setup> = row
        .setup
        .into_iter()
        .filter(|setup| setup.lineage != lineage)
        .collect();
    setup.extend(command.map(|command| Setup { lineage, command }));
    tx.execute(
        "UPDATE config_environment_branch SET setup = ?1 WHERE environment_id = ?2",
        &[
            serde_json::to_string(&setup)
                .expect("Setup Commands are JSON")
                .as_str()
                .into(),
            environment.summary.id.as_str().into(),
        ],
    )?;
    Ok(true)
}

/// A Service named `setup`, from before the name was reserved: its paths read as a
/// Branch's Setup Commands, so it is renamed before any `setup.` path is used.
pub(crate) fn refuse_service_named_setup(environment: &Environment) -> Result<(), RpcError> {
    if !environment
        .working
        .services
        .iter()
        .any(|service| service.slug == "setup")
    {
        return Ok(());
    }
    let next = format!(
        "ployz service rename setup NAME --env {} --project {}",
        environment.summary.name, environment.summary.project
    );
    Err(error::invalid(
        format!(
            "A Service in {} is named setup, and setup.SERVICE now means a Branch's Setup Command; rename it first: {next}",
            environment.summary.name
        ),
        json!({ "service": "setup", "next": next }),
    ))
}

/// `setup.SERVICE` never waits for a Deploy, so there is nothing to discard.
pub(crate) fn setup_is_not_staged(path: &crate::SettingPath) -> RpcError {
    error::invalid(
        format!("{path} is not staged, so there is nothing to discard; unset it instead"),
        json!({ "path": path }),
    )
}

/// The IDs of the Services that have deployed in Environment `id`.
pub(super) fn deployed(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<BTreeSet<String>, RpcError> {
    tx.query(
        "SELECT node_id FROM config_applied WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?
    .iter()
    .map(|row| row.text(0).map(str::to_owned))
    .collect()
}
