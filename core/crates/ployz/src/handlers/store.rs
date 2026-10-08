//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::RpcErrorCode;
use std::path::Path;

use ployz_store::{
    Actor, Admit, Ask, ConfigStore, DeploymentId, DeploymentSummary, EnvironmentRef,
    OrganizationId, ProjectName, RemovalsQuery, SealingKey, Tell, Trusted, View, VolumeObservation,
    Written,
};

use super::{Error, config_path, leaf_matches, runtime};
use crate::approval::{self, Asked};
use crate::cli::{env, value};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::ui::Hint;

impl From<StoreCallError> for Error {
    fn from(error: StoreCallError) -> Self {
        match error {
            StoreCallError::Refused(error) => error.into(),
            StoreCallError::Cloud(error) => error.into(),
            StoreCallError::Stopped(failure) => failure,
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
                .read_trusted(who, query, &Trusted::default())
                .map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                let view: View = runtime.block_on(cloud_account::config_store(
                    credential,
                    "read",
                    &query.to_query(),
                    None,
                ))?;
                Q::view(view).map_err(StoreCallError::Refused)
            }
        }
    }

    fn write<C: Tell>(
        &self,
        command: &C,
        trusted: Trusted,
        approval: Option<&str>,
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
                    approval,
                ))?;
                C::written(written).map_err(StoreCallError::Refused)
            }
        }
    }
}

/// The Config Store as one command sees it: a refusal becomes this command's
/// failure, and an ambiguous Project names `ployz link --project PROJECT`.
pub(crate) struct Store<'m> {
    backend: Backend,
    matches: &'m ArgMatches,
    /// The command's words, then the arguments its retry repeats: never raw input.
    words: Vec<String>,
}

impl<'m> Store<'m> {
    #[cfg(test)]
    pub(super) fn for_test(backend: Backend, matches: &'m ArgMatches) -> Self {
        Self {
            backend,
            matches,
            words: vec!["deploy".into()],
        }
    }

    /// Name the arguments this command's retry repeats.
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
        self.approved(command, &Trusted::default())
    }

    fn approved<C: Tell>(
        &self,
        command: &C,
        trusted: &Trusted,
    ) -> Result<C::Written, StoreCallError> {
        let mut approval = self
            .matches
            .try_get_one::<String>("approval")
            .ok()
            .flatten()
            .cloned();
        loop {
            let refused = match self
                .backend
                .write(command, trusted.clone(), approval.as_deref())
            {
                Err(StoreCallError::Refused(refused)) => refused,
                written => return written,
            };
            let (Backend::Cloud(runtime, credential), Some(asked), false) =
                (&self.backend, Asked::of(&refused), crate::ui::json())
            else {
                return Err(StoreCallError::Refused(refused));
            };
            let verb = match command
                .to_command()
                .get("command")
                .and_then(serde_json::Value::as_str)
            {
                Some("publish") => "publish",
                _ => "deploy",
            };
            let retry = self.again(&["--approval", &asked.id]);
            approval = Some(approval::settle(runtime, credential, verb, &asked, retry)?);
        }
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
        self.approved(
            admit,
            &Trusted {
                volumes,
                ..Trusted::default()
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
    /// linking this directory to one, after which the same command runs as typed.
    pub(crate) fn fail(&self, error: impl Into<Refusal>) -> Error {
        let Refusal { error, mut hint } = with_next(
            error,
            |refusal| {
                refusal.code == RpcErrorCode::Ambiguous && refusal.details.get("projects").is_some()
            },
            || next(self.matches, &["link", "--project", "PROJECT"]),
        );
        if let (None, StoreCallError::Refused(refused)) = (&hint, &error)
            && let Some(asked) = Asked::of(refused)
        {
            hint = Some(Hint::Retry(self.again(&["--approval", &asked.id])));
        }
        Error::from(error).hint(hint)
    }

    /// This command again with its [`Self::args`] and `extra`, in the same Project
    /// and Environment.
    pub(crate) fn again(&self, extra: &[&str]) -> String {
        let mut words: Vec<&str> = self.words.iter().map(String::as_str).collect();
        words.extend(extra);
        next(self.matches, &words)
    }

    /// This command's failure for a Store error; a refusal to delete Volume data
    /// names this command again accepting each Volume it lists, at the version it
    /// reviewed.
    pub(crate) fn accepting(&self, error: impl Into<Refusal>) -> Error {
        let refusal = match error.into() {
            Refusal {
                error: StoreCallError::Refused(error),
                ..
            } if error.code == RpcErrorCode::ConfirmationRequired => {
                let text = |value: &serde_json::Value| value.as_str().map(str::to_owned);
                let mut extra = Vec::new();
                let accept = error
                    .details
                    .get("accept")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten();
                for name in accept.filter_map(text) {
                    extra.extend(["--accept-volume-loss".to_owned(), name]);
                }
                if let Some(version) = error.details.get("version").and_then(text) {
                    extra.extend(["--expect-version".to_owned(), version]);
                }
                let retry = self.again(&extra.iter().map(String::as_str).collect::<Vec<_>>());
                Refusal {
                    error: StoreCallError::Refused(error),
                    hint: Some(Hint::Retry(retry)),
                }
            }
            refusal => refusal,
        };
        self.fail(refusal)
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

/// A Store error and the hint this command adds to it.
pub(crate) struct Refusal {
    error: StoreCallError,
    hint: Option<Hint>,
}

impl From<StoreCallError> for Refusal {
    fn from(error: StoreCallError) -> Self {
        Self { error, hint: None }
    }
}

/// A refused stale write names the `read` command that shows the fresh state.
pub(crate) fn with_refresh_hint(
    error: impl Into<Refusal>,
    matches: &ArgMatches,
    read: &str,
) -> Refusal {
    with_next(
        error,
        // A conflict that already names its next step (no Server: `ployz server add`) keeps it.
        |refusal| refusal.code == RpcErrorCode::Conflict && refusal.details.get("next").is_none(),
        || next(matches, &[read]),
    )
}

/// A refusal `when` picks, with no hint yet, names `next` as the command to run next.
pub(crate) fn with_next(
    error: impl Into<Refusal>,
    when: impl FnOnce(&ployz_core::RpcError) -> bool,
    next: impl FnOnce() -> String,
) -> Refusal {
    let mut refusal = error.into();
    if let (None, StoreCallError::Refused(error)) = (&refusal.hint, &refusal.error)
        && when(error)
    {
        refusal.hint = Some(Hint::Next(next()));
    }
    refusal
}

/// A unit enum variant as a person reads it: its JSON word with spaces, such as `not applied` for `not_applied`.
pub(crate) fn word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(|word| word.replace('_', " ")))
        .unwrap_or_default()
}

/// A value as a change line shows it: a long one cut to its start and length, so one big variable doesn't fill the terminal.
pub(crate) fn shown(value: &serde_json::Value) -> String {
    const SHOWN: usize = 80;
    let length = value.as_str().map_or_else(
        || value.to_string().chars().count(),
        |text| text.chars().count(),
    );
    if length <= SHOWN {
        return value.to_string();
    }
    let start: String = value.to_string().chars().take(SHOWN).collect();
    format!("{start}… ({length} chars)")
}

/// Argument `arg`, a Service name. Core's name error quotes the value; a rejected
/// value is never echoed.
pub(crate) fn service_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_core::ServiceName, Error> {
    ployz_core::ServiceName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Service name: up to 63 lowercase letters, digits and -, like web")
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
        Error::usage("Expected a Volume name: up to 63 lowercase letters, digits and -, like data")
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_long_value_shows_its_start_and_length() {
        assert_eq!(shown(&json!("short")), "\"short\"");
        assert_eq!(shown(&json!(3)), "3");
        let long = shown(&json!("a".repeat(70_000)));
        assert!(long.starts_with("\"aaa"));
        assert!(long.ends_with("… (70000 chars)"));
        assert!(long.chars().count() < 100);
    }

    #[test]
    fn a_refused_volume_loss_names_the_exact_retry() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging"])
            .unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let local = ConfigStore::open("sqlite::memory:", key).unwrap();
        let store = Store {
            backend: Backend::Local(
                std::sync::Arc::new(local),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            matches: leaf_matches(&root),
            words: vec!["deploy".to_owned()],
        };
        let refused = ployz_core::RpcError {
            code: RpcErrorCode::ConfirmationRequired,
            message: "This Deploy permanently deletes the data of data".into(),
            details: json!({ "version": "3:1:0.1", "accept": ["data"] }),
            cause: Vec::new(),
        };
        let error = store.accepting(StoreCallError::Refused(refused));
        let retry = "ployz deploy --accept-volume-loss data --expect-version 3:1:0.1 --env staging";
        assert_eq!(error.hints(), [Hint::Retry(retry.into())]);
        let error = error.report();
        assert_eq!(error.details.get("retry"), Some(&json!(retry)));
        assert_eq!(
            error.message,
            "This Deploy permanently deletes the data of data"
        );
    }

    #[test]
    fn a_conflict_keeps_the_next_step_the_store_named() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging"])
            .unwrap();
        let refused = |details| {
            StoreCallError::Refused(ployz_core::RpcError {
                code: RpcErrorCode::Conflict,
                message: "refused".into(),
                details,
                cause: Vec::new(),
            })
        };
        let next_of = |error| {
            let refusal = with_refresh_hint(error, leaf_matches(&root), "diff");
            Error::from(refusal.error).hint(refusal.hint).hints()
        };
        assert_eq!(
            next_of(refused(json!({ "next": "ployz server add" }))),
            [Hint::Next("ployz server add".into())]
        );
        assert_eq!(
            next_of(refused(json!({}))),
            [Hint::Next("ployz diff --env staging".into())]
        );
    }
}
