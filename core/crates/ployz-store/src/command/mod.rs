//! `write`'s commands. Each family lives in its own module and adds one
//! [`Command`] variant, one [`Written`] variant, and one arm in [`run`].

mod edit;
mod project;
mod service;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

pub use edit::{Change, Edit, Edited};
pub use project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
};
pub use service::{CreateService, ServiceCreated, ServiceSummary};

use crate::Actor;
use crate::error;
use crate::storage::Tx;

/// One change to authored configuration, applied in one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "command", rename_all = "snake_case")]
#[ts(rename = "ConfigCommand")]
pub enum Command {
    CreateProject(CreateProject),
    CreateEnvironment(CreateEnvironment),
    CreateService(CreateService),
    Edit(Edit),
}

/// What a command did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "written", rename_all = "snake_case")]
#[ts(rename = "ConfigWritten")]
pub enum Written {
    Project(ProjectCreated),
    Environment(EnvironmentCreated),
    Service(ServiceCreated),
    Edited(Edited),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, command: Command) -> Result<Written, RpcError> {
    match &command {
        Command::CreateProject(create) => replayable(tx, who, create.id.as_str(), &command, |tx| {
            project::create_project(tx, who, create).map(Written::Project)
        }),
        Command::CreateEnvironment(create) => {
            replayable(tx, who, create.id.as_str(), &command, |tx| {
                project::create_environment(tx, who, create).map(Written::Environment)
            })
        }
        Command::CreateService(create) => replayable(tx, who, create.id.as_str(), &command, |tx| {
            service::create(tx, who, create).map(Written::Service)
        }),
        Command::Edit(edit) => edit::run(tx, who, edit).map(Written::Edited),
    }
}

/// Run a create keyed by its caller-minted `id` once. Replaying the identical
/// command returns what the first run wrote; the same ID with any other body, or
/// from another Organization, is `conflict`.
fn replayable(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &str,
    command: &Command,
    create: impl FnOnce(&mut dyn Tx) -> Result<Written, RpcError>,
) -> Result<Written, RpcError> {
    let body = serde_json::to_string(command).expect("commands are JSON");
    let rows = tx.query(
        "SELECT organization_id, command, written FROM config_create WHERE id = ?1",
        &[id.into()],
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
    let written = create(tx)?;
    tx.execute(
        "INSERT INTO config_create (id, organization_id, command, written) VALUES (?1, ?2, ?3, ?4)",
        &[
            id.into(),
            who.organization.as_str().into(),
            body.as_str().into(),
            serde_json::to_string(&written)
                .expect("results are JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(written)
}
