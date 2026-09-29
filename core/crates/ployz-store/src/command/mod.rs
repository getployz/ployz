//! `write`'s commands. Each family lives in its own module and adds one
//! [`Command`] variant, one [`Written`] variant, and one arm in [`run`].

mod edit;
mod project;
mod review;
mod service;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use edit::{Change, Edit, Edited};
pub use project::{
    CreateEnvironment, CreateProject, EnvironmentCreated, ProjectCreated, ProjectSummary,
};
pub use review::{Discard, Discarded, Publish, Published};
pub use service::{CreateService, ServiceCreated, ServiceSummary};

use crate::Actor;
use crate::error;
use crate::storage::Tx;

/// One change to authored configuration, applied in one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    CreateProject(CreateProject),
    CreateEnvironment(CreateEnvironment),
    CreateService(CreateService),
    Edit(Edit),
    Publish(Publish),
    Discard(Discard),
}

/// What a command did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "written", rename_all = "snake_case")]
pub enum Written {
    Project(ProjectCreated),
    Environment(EnvironmentCreated),
    Service(ServiceCreated),
    Edited(Edited),
    Published(Published),
    Discarded(Discarded),
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
        Command::Publish(publish) => review::publish(tx, who, publish).map(Written::Published),
        Command::Discard(discard) => review::discard(tx, who, discard).map(Written::Discarded),
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
