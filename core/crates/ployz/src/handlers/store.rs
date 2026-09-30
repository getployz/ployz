//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::RpcErrorCode;
use ployz_store::{
    Actor, Admit, Ask, ConfigStore, DeploymentId, DeploymentSummary, DomainEvidence,
    EnvironmentRef, OrganizationId, ProjectName, SealingKey, Tell, Trusted, View, Written,
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

/// The Config Store a command reads and writes: Cloud's over HTTPS, as
/// `PLOYZ_TOKEN` or this device's sign-in, or the hidden in-process SQLite Store
/// when `PLOYZ_STORE` is set.
pub(crate) enum Store {
    Local(std::sync::Arc<ConfigStore>, Actor),
    Cloud(tokio::runtime::Runtime, Credential),
}

impl Store {
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
                    &query.to_query(),
                ))?;
                Q::view(view).map_err(StoreCallError::Refused)
            }
        }
    }

    /// Apply `command`: a Store command payload, answered as its own result.
    pub(crate) fn write<C: Tell>(&self, command: &C) -> Result<C::Written, StoreCallError> {
        self.write_trusted(command, self_hosted())
    }

    /// Admit a Deployment. Only the in-process Store takes `volumes`, the Servers
    /// this CLI observed: over HTTPS, Cloud gathers its own evidence.
    pub(crate) fn admit(
        &self,
        admit: &Admit,
        volumes: Option<ployz_store::VolumeObservation>,
    ) -> Result<DeploymentSummary, StoreCallError> {
        self.write_trusted(
            admit,
            Trusted {
                volumes,
                ..self_hosted()
            },
        )
    }

    /// `trusted` is the in-process Store's evidence; Cloud gathers its own.
    fn write_trusted<C: Tell>(
        &self,
        command: &C,
        trusted: Trusted,
    ) -> Result<C::Written, StoreCallError> {
        match self {
            Self::Local(store, who) => store
                .write_trusted(who, command, &trusted)
                .map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                let written: Written = runtime.block_on(cloud_account::config_store(
                    credential,
                    "write",
                    &command.to_command(),
                ))?;
                C::written(written).map_err(StoreCallError::Refused)
            }
        }
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

    /// The in-process Store, which only the hidden test mode has: there this CLI
    /// runs Deployments itself. `claim` and `record` never cross HTTPS.
    pub(crate) fn local(&self) -> Option<&std::sync::Arc<ConfigStore>> {
        match self {
            Self::Local(store, _) => Some(store),
            Self::Cloud(..) => None,
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
        return Ok(Some(Store::Local(
            std::sync::Arc::new(ConfigStore::open(&url, key)?),
            actor,
        )));
    }
    let credentials = CredentialStore::beside(config);
    let runtime = runtime()?;
    match runtime.block_on(cloud_account::from_env(&credentials)) {
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

/// Turn a Store error into this command's failure, adding the next step only the
/// command line can name: an ambiguous Project is fixed by rerunning `words` with
/// `--project`. `words` are the command and its accepted arguments, never raw input.
pub(crate) fn failed<'matches>(
    matches: &'matches ArgMatches,
    words: &'matches [&'matches str],
) -> impl FnOnce(StoreCallError) -> Error + 'matches {
    move |error| {
        with_next(
            error,
            |refusal| {
                refusal.code == RpcErrorCode::Ambiguous && refusal.details.get("projects").is_some()
            },
            || rerun(matches, words),
        )
        .into()
    }
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
