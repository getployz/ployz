//! `write`'s commands. Each family lives in its own module and adds one
//! [`Command`] variant, one [`Written`] variant, one arm in [`run`], and a typed
//! method on [`ConfigStore`](crate::ConfigStore) that calls the same function.

mod edit;
mod project;
mod service;

use ployz_core::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

pub(crate) use edit::edit;
pub use edit::{Change, Edit, Edited};
pub use project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
};
pub(crate) use project::{create_environment, create_project};
pub(crate) use service::create_service;
pub use service::{CreateService, ServiceCreated, ServiceSummary};

use crate::Actor;
use crate::error;
use crate::storage::Tx;

/// One change to authored configuration, applied in one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "command", rename_all = "snake_case")]
#[ts(rename = "ConfigCommand")]
pub enum Command {
    /// Create a Project with its Default Environment.
    CreateProject(CreateProject),
    /// Create an empty Environment.
    CreateEnvironment(CreateEnvironment),
    /// Create an image Service.
    CreateService(CreateService),
    /// Set and unset Settings in one Environment.
    Edit(Edit),
}

impl Command {
    /// The caller-minted IDs a create is keyed by, so a retry replays it. Empty
    /// for commands that create nothing.
    fn create_ids(&self) -> Vec<&str> {
        match self {
            Self::CreateProject(create) => {
                vec![create.id.as_str(), create.default_environment.as_str()]
            }
            Self::CreateEnvironment(create) => vec![create.id.as_str()],
            Self::CreateService(create) => vec![create.id.as_str()],
            Self::Edit(_) => Vec::new(),
        }
    }
}

/// What a command did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "written", rename_all = "snake_case")]
#[ts(rename = "ConfigWritten")]
pub enum Written {
    /// A Project was created.
    Project(ProjectCreated),
    /// An Environment was created.
    Environment(EnvironmentCreated),
    /// A Service was created.
    Service(ServiceCreated),
    /// Settings were edited.
    Edited(Edited),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, command: &Command) -> Result<Written, RpcError> {
    match command {
        Command::CreateProject(create) => create_project(tx, who, create).map(Written::Project),
        Command::CreateEnvironment(create) => {
            create_environment(tx, who, create).map(Written::Environment)
        }
        Command::CreateService(create) => create_service(tx, who, create).map(Written::Service),
        Command::Edit(edit) => self::edit(tx, who, edit).map(Written::Edited),
    }
}

/// Run a create keyed by its caller-minted IDs once. Replaying the identical
/// command returns what the first run wrote; any of its IDs reused with another
/// body, or from another Organization, is `conflict`.
fn replayable<T: Serialize + DeserializeOwned>(
    tx: &mut dyn Tx,
    who: &Actor,
    command: &Command,
    create: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    let body = serde_json::to_string(command).expect("commands are JSON");
    let ids = command.create_ids();
    for id in &ids {
        let rows = tx.query(
            "SELECT organization_id, command, written FROM config_create WHERE id = ?1",
            &[(*id).into()],
        )?;
        if let Some(row) = rows.first() {
            if row.text(0)? != who.organization.as_str() || row.text(1)? != body {
                return Err(error::conflict(
                    "This ID was already used by a different create",
                    json!({ "id": id }),
                ));
            }
            return serde_json::from_str(row.text(2)?).map_err(|_| error::corrupt("create result"));
        }
    }
    let written = create(tx)?;
    let written_json = serde_json::to_string(&written).expect("results are JSON");
    for id in ids {
        tx.execute(
            "INSERT INTO config_create (id, organization_id, command, written) \
             VALUES (?1, ?2, ?3, ?4)",
            &[
                id.into(),
                who.organization.as_str().into(),
                body.as_str().into(),
                written_json.as_str().into(),
            ],
        )?;
    }
    Ok(written)
}
