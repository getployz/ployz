//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{
    Actor, Command, ConfigStore, CreateEnvironment, CreateProject, CreateService, Edit, Edited,
    EnvironmentCreated, EnvironmentName, EnvironmentQuery, EnvironmentRef, EnvironmentView,
    OrganizationId, ProjectCreated, ProjectName, Query, ServiceCreated,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{env, value};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::CredentialStore;

impl From<StoreCallError> for Error {
    fn from(error: StoreCallError) -> Self {
        match error {
            StoreCallError::Refused(error) => error.into(),
            StoreCallError::Cloud(error) => error.into(),
        }
    }
}

/// The Organization of the hidden in-process Store.
const LOCAL_ORGANIZATION: &str = "local";

/// `--project`: which Project a command addresses.
pub(crate) fn project_arg() -> Arg {
    value("project", None)
        .env(env::PROJECT)
        .help("Project [default: the only Project]")
}

/// `--project` and `--env`, which every Environment-scoped command takes.
pub(crate) fn scoped(command: clap::Command) -> clap::Command {
    command.arg(project_arg()).arg(
        value("env", None)
            .env(env::ENVIRONMENT)
            .help("Environment [default: the Project's Default Environment]"),
    )
}

pub(crate) fn project(matches: &ArgMatches) -> Result<Option<ProjectName>, Error> {
    matches
        .get_one::<String>("project")
        .map(|name| ProjectName::parse(name.as_str()))
        .transpose()
        .map_err(Into::into)
}

pub(crate) fn environment(matches: &ArgMatches) -> Result<EnvironmentRef, Error> {
    Ok(EnvironmentRef {
        project: project(matches)?,
        environment: matches
            .get_one::<String>("env")
            .map(|name| EnvironmentName::parse(name.as_str()))
            .transpose()?,
    })
}

/// The Config Store a command reads and writes: Cloud's over HTTPS, as
/// `PLOYZ_TOKEN` or this device's sign-in, or the hidden in-process SQLite Store
/// when `PLOYZ_STORE` is set.
pub(crate) enum Store {
    Local(ConfigStore, Actor),
    Cloud(tokio::runtime::Runtime, Credential),
}

impl Store {
    pub(crate) fn environment(
        &self,
        query: &EnvironmentQuery,
    ) -> Result<EnvironmentView, StoreCallError> {
        let request = Query::Environment(query.clone());
        self.call("read", &request, |store, who| store.environment(who, query))
    }

    pub(crate) fn create_project(
        &self,
        create: &CreateProject,
    ) -> Result<ProjectCreated, StoreCallError> {
        let request = Command::CreateProject(create.clone());
        self.call("write", &request, |store, who| {
            store.create_project(who, create)
        })
    }

    pub(crate) fn create_environment(
        &self,
        create: &CreateEnvironment,
    ) -> Result<EnvironmentCreated, StoreCallError> {
        let request = Command::CreateEnvironment(create.clone());
        self.call("write", &request, |store, who| {
            store.create_environment(who, create)
        })
    }

    pub(crate) fn create_service(
        &self,
        create: &CreateService,
    ) -> Result<ServiceCreated, StoreCallError> {
        let request = Command::CreateService(create.clone());
        self.call("write", &request, |store, who| {
            store.create_service(who, create)
        })
    }

    pub(crate) fn edit(&self, edit: &Edit) -> Result<Edited, StoreCallError> {
        let request = Command::Edit(edit.clone());
        self.call("write", &request, |store, who| store.edit(who, edit))
    }

    /// Run in-process, or send `request` to Cloud's `read` or `write`. Cloud answers
    /// the tagged View or Written, which reads as the typed result.
    fn call<T: DeserializeOwned>(
        &self,
        operation: &str,
        request: &impl Serialize,
        local: impl FnOnce(&ConfigStore, &Actor) -> Result<T, RpcError>,
    ) -> Result<T, StoreCallError> {
        match self {
            Self::Local(store, who) => local(store, who).map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                runtime.block_on(cloud_account::config_store(credential, operation, request))
            }
        }
    }
}

pub(crate) fn store(root: &ArgMatches) -> Result<Store, Error> {
    if let Ok(url) = std::env::var(env::STORE) {
        let actor = Actor {
            organization: OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"),
        };
        return Ok(Store::Local(ConfigStore::open(&url)?, actor));
    }
    let credentials = CredentialStore::beside(&config_path(leaf_matches(root))?);
    let token = std::env::var(env::TOKEN).ok();
    let cloud = std::env::var(env::CLOUD_URL).ok();
    let runtime = runtime()?;
    let credential = runtime.block_on(cloud_account::credential(&credentials, token, cloud))?;
    Ok(Store::Cloud(runtime, credential))
}

/// Mint the ID a create is keyed by, so a retried request replays instead of repeating.
pub(crate) fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A Store error, with the next step only the command line can name: an ambiguous
/// Project is fixed by rerunning this command with `--project`.
pub(crate) fn failed(error: StoreCallError) -> Error {
    let StoreCallError::Refused(mut error) = error else {
        return error.into();
    };
    if error.code == RpcErrorCode::Ambiguous
        && let Some(details) = error.details.as_object_mut()
        && details.contains_key("projects")
    {
        let mut next = vec!["ployz".to_owned()];
        next.extend(std::env::args().skip(1));
        next.extend(["--project".to_owned(), "PROJECT".to_owned()]);
        details.insert("next".into(), json!(shell_words::join(next)));
    }
    error.into()
}
