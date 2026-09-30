//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::RpcErrorCode;
use std::path::Path;

use ployz_store::{
    Actor, Admit, Ask, ConfigStore, DeploymentId, DeploymentSummary, DomainEvidence,
    EnvironmentRef, OrganizationId, ProjectName, RemovalsQuery, SealingKey, Tell, Trusted, View,
    VolumeObservation, Written,
};
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

/// Where the Config Store is: Cloud's over HTTPS, as `PLOYZ_TOKEN` or this device's
/// sign-in, or the hidden in-process SQLite Store when `PLOYZ_STORE` is set.
pub(crate) enum Backend {
    Local(std::sync::Arc<ConfigStore>, Actor),
    Cloud(tokio::runtime::Runtime, Credential),
}

impl Backend {
    /// Answer `query`: a Store query payload, read as its own view.
    pub(crate) fn read<Q: Ask>(&self, query: &Q) -> Result<Q::View, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .read_trusted(who, query, &self_hosted())
                .map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                let view: View = runtime.block_on(cloud_account::config_store(
                    credential,
                    "read",
                    &query.clone().query(),
                ))?;
                Q::view(view).map_err(StoreCallError::Refused)
            }
        }
    }

    /// `trusted` is the in-process Store's evidence; Cloud gathers its own.
    fn write<C: Tell>(&self, command: &C, trusted: Trusted) -> Result<C::Written, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .write_trusted(who, command, &trusted)
                .map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                let written: Written = runtime.block_on(cloud_account::config_store(
                    credential,
                    "write",
                    &command.clone().command(),
                ))?;
                C::written(written).map_err(StoreCallError::Refused)
            }
        }
    }
}

/// The Config Store as one command sees it: a refusal becomes this command's
/// failure, and an ambiguous Project names this same command with `--project`.
pub(crate) struct Store<'m> {
    backend: Backend,
    matches: &'m ArgMatches,
    /// The command's words, then its accepted arguments: never raw input.
    words: Vec<String>,
}

impl<'m> Store<'m> {
    /// Name the command's accepted arguments, which its rerun repeats.
    pub(crate) fn args<'a>(mut self, args: impl IntoIterator<Item = &'a str>) -> Self {
        self.words.extend(args.into_iter().map(str::to_owned));
        self
    }

    /// Answer `query` as its own view.
    pub(crate) fn read<Q: Ask>(&self, query: &Q) -> Result<Q::View, Error> {
        self.try_read(query).map_err(|error| self.fail(error))
    }

    /// Apply `command`, answered as its own result.
    pub(crate) fn write<C: Tell>(&self, command: &C) -> Result<C::Written, Error> {
        self.try_write(command).map_err(|error| self.fail(error))
    }

    /// [`Self::read`], leaving the refusal for the caller to add a next step to.
    pub(crate) fn try_read<Q: Ask>(&self, query: &Q) -> Result<Q::View, StoreCallError> {
        self.backend.read(query)
    }

    /// [`Self::write`], leaving the refusal for the caller to add a next step to.
    pub(crate) fn try_write<C: Tell>(&self, command: &C) -> Result<C::Written, StoreCallError> {
        self.backend.write(command, self_hosted())
    }

    /// Admit a Deployment. The in-process Store reviews Volume loss against the
    /// Servers this CLI observes; over HTTPS, Cloud gathers its own evidence.
    pub(crate) fn admit(&self, admit: &Admit) -> Result<DeploymentSummary, StoreCallError> {
        let volumes = match (&self.backend, admit) {
            (Backend::Local(..), Admit::Deploy(deploy)) if deploy.services.is_empty() => {
                self.observe(&deploy.environment, false)?
            }
            (Backend::Local(..), Admit::Remove(removal)) => {
                self.observe(&removal.environment, true)?
            }
            _ => None,
        };
        self.backend.write(
            admit,
            Trusted {
                volumes,
                ..self_hosted()
            },
        )
    }

    /// Which Servers hold the data of the Volumes a Deploy (or a removal: `remove`)
    /// deletes. A Cluster this can't reach leaves the evidence out, so the Store refuses.
    fn observe(
        &self,
        environment: &EnvironmentRef,
        remove: bool,
    ) -> Result<Option<VolumeObservation>, StoreCallError> {
        let removals = self.backend.read(&RemovalsQuery {
            environment: environment.clone(),
            remove,
        })?;
        if removals.volumes.is_empty() {
            return Ok(None);
        }
        let sought = removals
            .volumes
            .into_iter()
            .map(|volume| volume.docker_volume)
            .collect();
        let context = self
            .matches
            .get_one::<String>("context")
            .map(String::as_str);
        let Ok(runtime) = runtime() else {
            return Ok(None);
        };
        Ok(runtime.block_on(async {
            let mut client = super::server::connect(self.matches, context).await.ok()?;
            client.observe_volumes(sought).await.ok()
        }))
    }

    /// Hand Cloud the archive of `dir`, the source Deployment `deployment` builds
    /// from, before admitting it. The in-process Store's runner reads `dir` itself.
    pub(crate) fn upload(&self, deployment: &DeploymentId, dir: &Path) -> Result<(), Error> {
        let Backend::Cloud(runtime, credential) = &self.backend else {
            return Ok(());
        };
        let archive = crate::build::upload_archive(dir).map_err(super::deploy::unreadable(dir))?;
        runtime
            .block_on(cloud_account::upload(
                credential,
                deployment.as_str(),
                archive,
            ))
            .map_err(|error| self.fail(error))
    }

    /// Where this Store is.
    pub(crate) const fn backend(&self) -> &Backend {
        &self.backend
    }

    /// The in-process Store, which only the hidden test mode has: there this CLI
    /// runs Deployments itself. `claim` and `record` never cross HTTPS.
    pub(crate) fn local(&self) -> Option<&std::sync::Arc<ConfigStore>> {
        match &self.backend {
            Backend::Local(store, _) => Some(store),
            Backend::Cloud(..) => None,
        }
    }

    /// This command's failure for a Store error: an ambiguous Project is fixed by
    /// rerunning it with `--project`.
    pub(crate) fn fail(&self, error: StoreCallError) -> Error {
        with_next(
            error,
            |refusal| {
                refusal.code == RpcErrorCode::Ambiguous && refusal.details.get("projects").is_some()
            },
            || rerun(self.matches, &self.words),
        )
        .into()
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

/// The Config Store `root`'s command reads and writes.
pub(crate) fn store(root: &ArgMatches) -> Result<Store<'_>, Error> {
    reachable(root)?.ok_or_else(|| LoginError::SignedOut.into())
}

/// The Store, or `None` when there's none to reach: signed out, no `PLOYZ_TOKEN`
/// and no `PLOYZ_STORE`.
pub(crate) fn reachable(root: &ArgMatches) -> Result<Option<Store<'_>>, Error> {
    let matches = leaf_matches(root);
    let mut words = Vec::new();
    let mut at = root;
    while let Some((word, child)) = at.subcommand() {
        words.push(word.to_owned());
        at = child;
    }
    Ok(backend_at(&config_path(matches)?)?.map(|backend| Store {
        backend,
        matches,
        words,
    }))
}

/// Whether commands use the hidden in-process Store (`PLOYZ_STORE`).
pub(crate) fn local_mode() -> bool {
    std::env::var_os(env::STORE).is_some()
}

/// The Store as seen with the CLI config at `config` (where a device's sign-in lives).
pub(crate) fn backend_at(config: &Path) -> Result<Option<Backend>, Error> {
    if let Ok(url) = std::env::var(env::STORE) {
        let actor = Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"));
        // The hidden test mode keeps its sealing key beside its database.
        let key = match url
            .strip_prefix("sqlite:")
            .filter(|path| *path != ":memory:")
        {
            Some(path) => format!("{path}.key").into(),
            None => config.with_file_name("store.key"),
        };
        let key = SealingKey::from_file(&key)?;
        return Ok(Some(Backend::Local(
            std::sync::Arc::new(ConfigStore::open(&url, key)?),
            actor,
        )));
    }
    let credentials = CredentialStore::beside(config);
    let runtime = runtime()?;
    match runtime.block_on(cloud_account::from_env(&credentials)) {
        Ok(credential) => Ok(Some(Backend::Cloud(runtime, credential))),
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
fn rerun(matches: &ArgMatches, words: &[String]) -> String {
    let mut rerun = words.to_vec();
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
    with_next(
        error,
        |refusal| refusal.code == RpcErrorCode::Conflict,
        || next(matches, &[read]),
    )
}

/// A refusal `when` picks names `next` as the command to run next.
pub(crate) fn with_next(
    error: StoreCallError,
    when: impl FnOnce(&ployz_core::RpcError) -> bool,
    next: impl FnOnce() -> String,
) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if when(&error)
        && let Some(details) = error.details.as_object_mut()
    {
        details.insert("next".into(), json!(next()));
    }
    StoreCallError::Refused(error)
}

/// A unit enum variant as the word its JSON uses, such as `not_applied`.
pub(crate) fn word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Argument `arg`, a Service name. Core's name error quotes the value; a rejected
/// value is never echoed.
pub(crate) fn service_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_core::ServiceName, Error> {
    ployz_core::ServiceName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Service name: lowercase letters, digits and -, like web")
            .with_exit(crate::failure::USAGE_EXIT)
    })
}

/// Every value of argument `arg`, Service names.
pub(crate) fn service_names(
    matches: &ArgMatches,
    arg: &str,
) -> Result<Vec<ployz_core::ServiceName>, Error> {
    names(
        matches,
        arg,
        |name| ployz_core::ServiceName::parse(name).ok(),
        "Service",
    )
}

/// Every value of argument `arg`, Volume names.
pub(crate) fn volume_names(
    matches: &ArgMatches,
    arg: &str,
) -> Result<Vec<ployz_store::VolumeName>, Error> {
    names(
        matches,
        arg,
        |name| ployz_store::VolumeName::parse(name).ok(),
        "Volume",
    )
}

/// A rejected value is never echoed.
fn names<T>(
    matches: &ArgMatches,
    arg: &str,
    parse: impl Fn(String) -> Option<T>,
    what: &str,
) -> Result<Vec<T>, Error> {
    super::string_values(matches, arg)
        .into_iter()
        .map(|name| {
            parse(name).ok_or_else(|| {
                Error::usage(format!(
                    "Expected {what} names: lowercase letters, digits and -"
                ))
                .with_exit(crate::failure::USAGE_EXIT)
            })
        })
        .collect()
}

/// Argument `arg`, a Volume name.
pub(crate) fn volume_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_store::VolumeName, Error> {
    ployz_store::VolumeName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Volume name: lowercase letters, digits and -, like data")
            .with_exit(crate::failure::USAGE_EXIT)
    })
}
