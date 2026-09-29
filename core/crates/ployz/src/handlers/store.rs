//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{
    Actor, AddDomain, Admit, Branched, BuildLogQuery, BuildLogView, Cancel, Command, ConfigStore,
    CopyNode, CreateBranch, CreateEnvironment, CreateGitService, CreateProject, CreateService,
    CreateVolume, DeploymentId, DeploymentQuery, DeploymentSummary, DeploymentView,
    DeploymentsQuery, DeploymentsView, DiffQuery, DiffView, Discard, Discarded, DomainEvidence,
    DomainQuery, DomainStaged, DomainView, DomainsQuery, DomainsView, Edit, Edited,
    EnvironmentCreated, EnvironmentQuery, EnvironmentRemoved, EnvironmentsQuery, EnvironmentsView,
    RemoveEnvironment, SetDefaultEnvironment, EnvironmentRef, EnvironmentView, KeepBranch,
    NamespaceQuery, NamespaceView, OrganizationId, PlanQuery, PlanView, ProjectCreated,
    ProjectName, Publish, Published, Query, RemovalsQuery, RemovalsView, RemoveDomain,
    RemoveService, RemoveVolume, RenameService, SealingKey, ServiceQuery, ServiceStaged,
    ServiceView, ServicesQuery, ServicesView, Start, Trusted, UpdateBranch, VolumeQuery,
    VolumeStaged, VolumeView, VolumesQuery, VolumesView,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{env, value};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::{CredentialStore, LoginError};

impl From<StoreCallError> for Error {
    fn from(error: StoreCallError) -> Self {
        match error {
            StoreCallError::Refused(error) => error.into(),
            StoreCallError::Cloud(error) => error.into(),
        }
    }
}

/// The Organization of the hidden in-process Store.
pub(super) const LOCAL_ORGANIZATION: &str = "local";

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

/// The Project a command addresses: `--project`, `PLOYZ_PROJECT`, else the directory link.
pub(crate) fn project(matches: &ArgMatches) -> Result<Option<ProjectName>, Error> {
    Ok(super::link::scope(matches)?.at().project)
}

/// The Environment a command addresses: flags, environment variables, else the directory link.
pub(crate) fn environment(matches: &ArgMatches) -> Result<EnvironmentRef, Error> {
    Ok(super::link::scope(matches)?.at())
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

    /// Cloud checks the repository and branch for the Organization; the hidden
    /// local Store has no way to, so it refuses every repository.
    pub(crate) fn create_git_service(
        &self,
        create: &CreateGitService,
    ) -> Result<ServiceStaged, StoreCallError> {
        let request = Command::CreateGitService(create.clone());
        self.call("write", &request, |store, who| {
            store.create_git_service(who, create, &Trusted::default())
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

    /// Admit a Deployment. Only the in-process Store takes `trusted`: over HTTPS,
    /// Cloud gathers its own evidence.
    /// Admit a Deployment. Only the in-process Store takes `volumes`, the Servers
    /// this CLI observed: over HTTPS, Cloud gathers its own evidence.
    pub(crate) fn admit(
        &self,
        admit: &Admit,
        volumes: Option<ployz_store::VolumeObservation>,
    ) -> Result<DeploymentSummary, StoreCallError> {
        let request = Command::Admit(admit.clone());
        self.call("write", &request, |store, who| {
            let trusted = Trusted {
                volumes,
                ..self_hosted()
            };
            store.admit(who, admit, &trusted)
        })
    }

    pub(crate) fn create_volume(
        &self,
        create: &CreateVolume,
    ) -> Result<VolumeStaged, StoreCallError> {
        let request = Command::CreateVolume(create.clone());
        self.call("write", &request, |store, who| {
            store.create_volume(who, create)
        })
    }

    pub(crate) fn remove_volume(
        &self,
        remove: &RemoveVolume,
    ) -> Result<VolumeStaged, StoreCallError> {
        let request = Command::RemoveVolume(remove.clone());
        self.call("write", &request, |store, who| {
            store.remove_volume(who, remove)
        })
    }

    pub(crate) fn volumes(&self, query: &VolumesQuery) -> Result<VolumesView, StoreCallError> {
        let request = Query::Volumes(query.clone());
        self.call("read", &request, |store, who| store.volumes(who, query))
    }

    pub(crate) fn volume(&self, query: &VolumeQuery) -> Result<VolumeView, StoreCallError> {
        let request = Query::Volume(query.clone());
        self.call("read", &request, |store, who| store.volume(who, query))
    }

    pub(crate) fn removals(&self, query: &RemovalsQuery) -> Result<RemovalsView, StoreCallError> {
        let request = Query::Removals(query.clone());
        self.call("read", &request, |store, who| store.removals(who, query))
    }

    /// Hand Cloud `archive`, the source Deployment `deployment` builds from, before
    /// admitting it. The hidden local Store's runner reads the directory itself.
    pub(crate) fn upload(
        &self,
        deployment: &DeploymentId,
        archive: Vec<u8>,
    ) -> Result<(), StoreCallError> {
        match self {
            Self::Local(..) => Ok(()),
            Self::Cloud(runtime, credential) => runtime.block_on(cloud_account::upload(
                credential,
                deployment.as_str(),
                archive,
            )),
        }
    }

    pub(crate) fn start(&self, start: &Start) -> Result<DeploymentSummary, StoreCallError> {
        let request = Command::Start(start.clone());
        self.call("write", &request, |store, who| store.start(who, start))
    }

    pub(crate) fn cancel(&self, cancel: &Cancel) -> Result<DeploymentSummary, StoreCallError> {
        let request = Command::Cancel(cancel.clone());
        self.call("write", &request, |store, who| store.cancel(who, cancel))
    }

    pub(crate) fn plan(&self, query: &PlanQuery) -> Result<PlanView, StoreCallError> {
        let request = Query::Plan(query.clone());
        self.call("read", &request, |store, who| store.plan(who, query))
    }

    pub(crate) fn namespace(
        &self,
        query: &NamespaceQuery,
    ) -> Result<NamespaceView, StoreCallError> {
        let request = Query::Namespace(query.clone());
        self.call("read", &request, |store, who| store.namespace(who, query))
    }

    pub(crate) fn build_log(&self, query: &BuildLogQuery) -> Result<BuildLogView, StoreCallError> {
        let request = Query::BuildLog(query.clone());
        self.call("read", &request, |store, who| store.build_log(who, query))
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

    /// Cloud checks the custom-domain capability; the hidden local Store stands for
    /// a self-hosted Cloud, which always has it.
    pub(crate) fn add_domain(&self, add: &AddDomain) -> Result<DomainStaged, StoreCallError> {
        let request = Command::AddDomain(add.clone());
        self.call("write", &request, |store, who| {
            store.add_domain(who, add, &self_hosted())
        })
    }

    pub(crate) fn remove_domain(
        &self,
        remove: &RemoveDomain,
    ) -> Result<DomainStaged, StoreCallError> {
        let request = Command::RemoveDomain(remove.clone());
        self.call("write", &request, |store, who| {
            store.remove_domain(who, remove, &self_hosted())
        })
    }

    /// Cloud observes the Cluster for each domain's status; the hidden local Store
    /// sees nothing of it.
    pub(crate) fn domains(&self, query: &DomainsQuery) -> Result<DomainsView, StoreCallError> {
        let request = Query::Domains(query.clone());
        self.call("read", &request, |store, who| {
            store.domains(who, query, &self_hosted())
        })
    }

    /// One domain, which Cloud re-checks first: its DNS, and the Cluster Domain.
    pub(crate) fn domain(&self, query: &DomainQuery) -> Result<DomainView, StoreCallError> {
        let request = Query::Domain(query.clone());
        self.call("read", &request, |store, who| {
            store.domain(who, query, &self_hosted())
        })
    }

    pub(crate) fn create_branch(&self, create: &CreateBranch) -> Result<Branched, StoreCallError> {
        let request = Command::CreateBranch(create.clone());
        self.call("write", &request, |store, who| {
            store.create_branch(who, create)
        })
    }

    pub(crate) fn update_branch(&self, update: &UpdateBranch) -> Result<Branched, StoreCallError> {
        let request = Command::UpdateBranch(update.clone());
        self.call("write", &request, |store, who| {
            store.update_branch(who, update)
        })
    }

    pub(crate) fn copy_node(&self, copy: &CopyNode) -> Result<Branched, StoreCallError> {
        let request = Command::CopyNode(copy.clone());
        self.call("write", &request, |store, who| store.copy_node(who, copy))
    }

    pub(crate) fn environments(
        &self,
        query: &EnvironmentsQuery,
    ) -> Result<EnvironmentsView, StoreCallError> {
        let request = Query::Environments(query.clone());
        self.call("read", &request, |store, who| store.environments(who, query))
    }

    pub(crate) fn set_default_environment(
        &self,
        set: &SetDefaultEnvironment,
    ) -> Result<EnvironmentsView, StoreCallError> {
        let request = Command::SetDefaultEnvironment(set.clone());
        self.call("write", &request, |store, who| {
            store.set_default_environment(who, set)
        })
    }

    pub(crate) fn remove_environment(
        &self,
        remove: &RemoveEnvironment,
    ) -> Result<EnvironmentRemoved, StoreCallError> {
        let request = Command::RemoveEnvironment(remove.clone());
        self.call("write", &request, |store, who| {
            store.remove_environment(who, remove)
        })
    }

    pub(crate) fn keep_branch(&self, keep: &KeepBranch) -> Result<Branched, StoreCallError> {
        let request = Command::KeepBranch(keep.clone());
        self.call("write", &request, |store, who| store.keep_branch(who, keep))
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

/// What the hidden local Store knows of domains: like a self-hosted Cloud, custom
/// domains are allowed; it has no Cluster Domain and observes no Servers.
fn self_hosted() -> Trusted {
    Trusted {
        domains: DomainEvidence {
            custom_domains: true,
            ..DomainEvidence::default()
        },
        ..Trusted::default()
    }
}

pub(crate) fn store(root: &ArgMatches) -> Result<Store, Error> {
    store_at(&config_path(leaf_matches(root))?)
}

/// The Store as seen with the CLI config at `config` (where a device's sign-in lives).
pub(crate) fn store_at(config: &std::path::Path) -> Result<Store, Error> {
    reachable_at(config)?.ok_or_else(|| LoginError::SignedOut.into())
}

/// The Store, or `None` when there's none to reach: signed out, no `PLOYZ_TOKEN`
/// and no `PLOYZ_STORE`.
pub(crate) fn reachable(root: &ArgMatches) -> Result<Option<Store>, Error> {
    reachable_at(&config_path(leaf_matches(root))?)
}

fn reachable_at(config: &std::path::Path) -> Result<Option<Store>, Error> {
    if let Ok(url) = std::env::var(env::STORE) {
        let actor = Actor {
            organization: OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"),
        };
        // The hidden test mode keeps its sealing key beside its database.
        let key = match url
            .strip_prefix("sqlite:")
            .filter(|path| *path != ":memory:")
        {
            Some(path) => format!("{path}.key").into(),
            None => config.with_file_name("store.key"),
        };
        let key = SealingKey::from_file(&key)?;
        return Ok(Some(Store::Local(ConfigStore::open(&url, key)?, actor)));
    }
    let credentials = CredentialStore::beside(config);
    let token = std::env::var(env::TOKEN).ok();
    let cloud = std::env::var(env::CLOUD_URL).ok();
    let runtime = runtime()?;
    match runtime.block_on(cloud_account::credential(&credentials, token, cloud)) {
        Ok(credential) => Ok(Some(Store::Cloud(runtime, credential))),
        Err(LoginError::SignedOut) => Ok(None),
        Err(error) => Err(error.into()),
    }
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
