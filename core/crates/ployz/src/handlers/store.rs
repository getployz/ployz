//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{
    Actor, Admit, Command, ConfigStore, CreateEnvironment, CreateProject, CreateService,
    DeploymentId, DeploymentQuery, DeploymentSummary, DeploymentView, DeploymentsQuery,
    DeploymentsView, DiffQuery, DiffView, Discard, Discarded, Edit, Edited, EnvironmentCreated,
    EnvironmentName, EnvironmentQuery, EnvironmentRef, EnvironmentView, OrganizationId, PlanQuery,
    PlanView, ProjectCreated, ProjectName, Publish, Published, Query, RemoveService, RenameService,
    ServiceQuery, ServiceStaged, ServiceView, ServicesQuery, ServicesView,
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
    ) -> Result<ServiceStaged, StoreCallError> {
        let request = Command::CreateService(create.clone());
        self.call("write", &request, |store, who| {
            store.create_service(who, create)
        })
    }

    pub(crate) fn rename_service(
        &self,
        rename: &RenameService,
    ) -> Result<ServiceStaged, StoreCallError> {
        let request = Command::RenameService(rename.clone());
        self.call("write", &request, |store, who| {
            store.rename_service(who, rename)
        })
    }

    pub(crate) fn remove_service(
        &self,
        remove: &RemoveService,
    ) -> Result<ServiceStaged, StoreCallError> {
        let request = Command::RemoveService(remove.clone());
        self.call("write", &request, |store, who| {
            store.remove_service(who, remove)
        })
    }

    pub(crate) fn services(&self, query: &ServicesQuery) -> Result<ServicesView, StoreCallError> {
        let request = Query::Services(query.clone());
        self.call("read", &request, |store, who| store.services(who, query))
    }

    pub(crate) fn service(&self, query: &ServiceQuery) -> Result<ServiceView, StoreCallError> {
        let request = Query::Service(query.clone());
        self.call("read", &request, |store, who| store.service(who, query))
    }

    pub(crate) fn diff(&self, query: &DiffQuery) -> Result<DiffView, StoreCallError> {
        let request = Query::Diff(query.clone());
        self.call("read", &request, |store, who| store.diff(who, query))
    }

    pub(crate) fn publish(&self, publish: &Publish) -> Result<Published, StoreCallError> {
        let request = Command::Publish(publish.clone());
        self.call("write", &request, |store, who| store.publish(who, publish))
    }

    pub(crate) fn discard(&self, discard: &Discard) -> Result<Discarded, StoreCallError> {
        let request = Command::Discard(discard.clone());
        self.call("write", &request, |store, who| store.discard(who, discard))
    }

    pub(crate) fn edit(&self, edit: &Edit) -> Result<Edited, StoreCallError> {
        let request = Command::Edit(edit.clone());
        self.call("write", &request, |store, who| store.edit(who, edit))
    }

    pub(crate) fn admit(&self, admit: &Admit) -> Result<DeploymentSummary, StoreCallError> {
        let request = Command::Admit(admit.clone());
        self.call("write", &request, |store, who| store.admit(who, admit))
    }

    pub(crate) fn plan(&self, query: &PlanQuery) -> Result<PlanView, StoreCallError> {
        let request = Query::Plan(query.clone());
        self.call("read", &request, |store, who| store.plan(who, query))
    }

    pub(crate) fn deployments(
        &self,
        query: &DeploymentsQuery,
    ) -> Result<DeploymentsView, StoreCallError> {
        let request = Query::Deployments(query.clone());
        self.call("read", &request, |store, who| store.deployments(who, query))
    }

    pub(crate) fn deployment(&self, id: &DeploymentId) -> Result<DeploymentView, StoreCallError> {
        let request = Query::Deployment(DeploymentQuery { id: id.clone() });
        self.call("read", &request, |store, who| store.deployment(who, id))
    }

    /// The in-process Store, which only the hidden test mode has: there this CLI
    /// runs Deployments itself. `claim` and `record` never cross HTTPS.
    pub(crate) fn local(&self) -> Option<&ConfigStore> {
        match self {
            Self::Local(store, _) => Some(store),
            Self::Cloud(..) => None,
        }
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
    store_at(&config_path(leaf_matches(root))?)
}

/// The Store as seen with the CLI config at `config` (where a device's sign-in lives).
pub(crate) fn store_at(config: &std::path::Path) -> Result<Store, Error> {
    if let Ok(url) = std::env::var(env::STORE) {
        let actor = Actor {
            organization: OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"),
        };
        return Ok(Store::Local(ConfigStore::open(&url)?, actor));
    }
    let credentials = CredentialStore::beside(config);
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

/// A command's result and, when there is one, the command to run next.
#[derive(serde::Serialize)]
pub(crate) struct Next<'a, T> {
    #[serde(flatten)]
    value: &'a T,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

impl<'a, T> Next<'a, T> {
    pub(crate) fn new(value: &'a T, next: Option<String>) -> Self {
        Self { value, next }
    }
}

/// `ployz WORDS…` in the same Project and Environment as this command.
pub(crate) fn next(matches: &ArgMatches, words: &[&str]) -> String {
    let mut next = vec!["ployz".to_owned()];
    next.extend(words.iter().map(|word| (*word).to_owned()));
    for flag in ["project", "env"] {
        // Not every command takes both flags (`env new` has no `--env`).
        if let Ok(Some(value)) = matches.try_get_one::<String>(flag) {
            next.extend([format!("--{flag}"), value.clone()]);
        }
    }
    shell_words::join(next)
}

/// This same command again with `--project`. It keeps every guard flag it was given
/// (`--expect`, `--version`, `--all`): dropping one would make a guarded write blind.
/// Only [`next`]'s scope flags travel to other commands, so this stays separate.
fn rerun(matches: &ArgMatches, words: &[&str]) -> String {
    let mut rerun = words
        .iter()
        .map(|word| (*word).to_owned())
        .collect::<Vec<_>>();
    for flag in ["expect", "version"] {
        if let Ok(Some(value)) = matches.try_get_one::<String>(flag) {
            rerun.extend([format!("--{flag}"), value.clone()]);
        }
    }
    if let Ok(Some(true)) = matches.try_get_one::<bool>("all") {
        rerun.push("--all".to_owned());
    }
    rerun.extend(["--project".to_owned(), "PROJECT".to_owned()]);
    next(
        matches,
        &rerun.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

/// A refused stale write names the `read` command that shows the fresh state.
pub(crate) fn with_refresh_hint(
    error: StoreCallError,
    matches: &ArgMatches,
    read: &str,
) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if error.code == RpcErrorCode::Conflict
        && let Some(details) = error.details.as_object_mut()
    {
        details.insert("next".into(), json!(next(matches, &[read])));
    }
    StoreCallError::Refused(error)
}

/// Turn a Store error into this command's failure, adding the next step only the
/// command line can name: an ambiguous Project is fixed by rerunning `words` with
/// `--project`. `words` are the command and its accepted arguments, never raw input.
pub(crate) fn failed<'matches>(
    matches: &'matches ArgMatches,
    words: &'matches [&'matches str],
) -> impl FnOnce(StoreCallError) -> Error + 'matches {
    move |error| {
        let StoreCallError::Refused(mut error) = error else {
            return error.into();
        };
        if error.code == RpcErrorCode::Ambiguous
            && let Some(details) = error.details.as_object_mut()
            && details.contains_key("projects")
        {
            details.insert("next".into(), json!(rerun(matches, words)));
        }
        error.into()
    }
}
