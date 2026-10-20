//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches};
use ployz_core::RpcErrorCode;
use std::path::Path;
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use ployz_store::{
    Actor, Admit, Ask, ConfigStore, DeploymentId, DeploymentSummary, EnvironmentRef,
    OrganizationId, ProjectName, Publish, Published, Removal, RemovalsQuery, Remove, SealingKey,
    Sweep, SystemEvent, Tell, Trusted, View, VolumeObservation, Written,
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
                    cloud_account::Via::Cli,
                    "read",
                    &query.to_query(),
                    None,
                ))?;
                Q::view(view).map_err(StoreCallError::Refused)
            }
        }
    }

    /// [`Self::read`] once, giving up on Cloud after `limit`: `None` when it doesn't answer.
    pub(crate) fn read_within<Q: Ask>(&self, query: &Q, limit: Duration) -> Option<Q::View> {
        match self {
            Self::Local(..) => self.read(query).ok(),
            Self::Cloud(runtime, credential) => {
                let view: View = runtime
                    .block_on(async {
                        tokio::time::timeout(
                            limit,
                            cloud_account::config_store(
                                credential,
                                cloud_account::Via::Cli,
                                "read",
                                &query.to_query(),
                                None,
                            ),
                        )
                        .await
                    })
                    .ok()?
                    .ok()?;
                Q::view(view).ok()
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
                    cloud_account::Via::Cli,
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
        self.try_write_checked(command, &["diff"])
    }

    /// [`Self::try_write`] for a command whose effect `check`, a read, shows.
    pub(crate) fn try_write_checked<C: Tell>(
        &self,
        command: &C,
        check: &[&str],
    ) -> Result<C::Written, StoreCallError> {
        self.approved(command, &Trusted::default(), Some(check))
    }

    /// Apply `command`, asking for approval when Cloud does. A write Cloud took but
    /// never answered names `check`, the read that shows whether it applied.
    fn approved<C: Tell>(
        &self,
        command: &C,
        trusted: &Trusted,
        check: Option<&[&str]>,
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
                Err(StoreCallError::Cloud(error @ LoginError::Unanswered { .. })) => {
                    let check = check.map(|check| Hint::Inspect(next(self.matches, check)));
                    return Err(StoreCallError::Stopped(Error::from(error).hint(check)));
                }
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
                Some("publish") => approval::Verb::Publish,
                Some("remove") => approval::Verb::Remove,
                _ => approval::Verb::Deploy,
            };
            let retry = self.approving(&asked.id);
            approval = Some(approval::settle(runtime, credential, verb, &asked, retry)?);
        }
    }

    /// Save Working State for the next deploy. Locally this CLI observes Volume loss;
    /// Cloud gathers its own evidence.
    pub(crate) fn publish(&self, publish: &Publish) -> Result<Published, StoreCallError> {
        let volumes = match &self.backend {
            Backend::Local(..) => self.observe([RemovalsQuery {
                environment: publish.environment.clone(),
                ..RemovalsQuery::default()
            }])?,
            Backend::Cloud(..) => None,
        };
        self.approved(
            publish,
            &Trusted {
                volumes,
                ..Trusted::default()
            },
            Some(&["diff"]),
        )
    }

    /// Admit a Deployment. The in-process Store reviews Volume loss against the
    /// Servers this CLI observes; over HTTPS, Cloud gathers its own evidence.
    pub(crate) fn admit(&self, admit: &Admit) -> Result<DeploymentSummary, StoreCallError> {
        let removing = |environment: &EnvironmentRef, remove| RemovalsQuery {
            environment: environment.clone(),
            remove,
            deployment: None,
        };
        let volumes = match (&self.backend, admit) {
            (Backend::Local(..), Admit::Deploy(deploy)) if deploy.services.is_empty() => {
                self.observe([removing(&deploy.environment, false)])?
            }
            (Backend::Local(..), Admit::Remove(removal)) => {
                self.observe([removing(&removal.environment, true)])?
            }
            (Backend::Local(..), Admit::ShutDown(shut_down)) => {
                self.observe([removing(&shut_down.environment, true)])?
            }
            (Backend::Local(..), Admit::Retry(retry)) => self.observe([RemovalsQuery {
                deployment: Some(retry.deployment.clone()),
                ..RemovalsQuery::default()
            }])?,
            _ => None,
        };
        self.approved(
            admit,
            &Trusted {
                volumes,
                ..Trusted::default()
            },
            Some(&["deployment", "ls"]),
        )
    }

    /// Remove an Environment or a Project. The in-process Store reviews Volume loss
    /// against what this CLI observes of `members`' Volumes.
    pub(crate) fn remove(
        &self,
        remove: &Remove,
        members: &[EnvironmentRef],
    ) -> Result<Removal, StoreCallError> {
        let volumes = match &self.backend {
            Backend::Local(..) => {
                self.observe(members.iter().map(|environment| RemovalsQuery {
                    environment: environment.clone(),
                    remove: true,
                    deployment: None,
                }))?
            }
            Backend::Cloud(..) => None,
        };
        self.approved(
            remove,
            &Trusted {
                volumes,
                ..Trusted::default()
            },
            None,
        )
    }

    /// Move every removal on in the in-process Store, as Cloud's sweep does there.
    pub(crate) fn sweep(&self) -> Result<(), StoreCallError> {
        match &self.backend {
            Backend::Local(store, who) => sweep(store, who).map_err(StoreCallError::Refused),
            Backend::Cloud(..) => Ok(()),
        }
    }

    /// Which Servers hold the data of the Volumes each of `queries` removes: a
    /// Deploy, removal or rollback. A Cluster this can't reach leaves the evidence
    /// out, so the Store refuses.
    fn observe(
        &self,
        queries: impl IntoIterator<Item = RemovalsQuery>,
    ) -> Result<Option<VolumeObservation>, StoreCallError> {
        let mut sought = Vec::new();
        for query in queries {
            let removals = self.backend.read(&query)?;
            sought.extend(
                removals
                    .volumes
                    .into_iter()
                    .map(|volume| volume.docker_volume),
            );
        }
        if sought.is_empty() {
            return Ok(None);
        }
        // Without `--context`, this reaches the Cluster `--connect` or the current context names.
        let context = self
            .matches
            .try_get_one::<String>("context")
            .ok()
            .flatten()
            .map(String::as_str);
        let Ok(runtime) = runtime() else {
            return Ok(None);
        };
        Ok(runtime.block_on(async {
            let mut client = super::server::connect(self.matches, context).await.ok()?;
            client.observe_volumes(sought).await.ok()
        }))
    }

    /// Hand Cloud the archive of `listing`, read from `dir`, the source Deployment
    /// `deployment` builds from, before admitting it. The in-process Store's runner
    /// reads `dir` itself.
    pub(crate) fn upload(
        &self,
        deployment: &DeploymentId,
        dir: &Path,
        listing: &crate::build::UploadListing,
    ) -> Result<(), Error> {
        let Backend::Cloud(runtime, credential) = &self.backend else {
            return Ok(());
        };
        let archive = listing.archive().map_err(super::deploy::unreadable(dir))?;
        runtime
            .block_on(cloud_account::upload(
                credential,
                deployment.as_str(),
                archive,
            ))
            .map_err(|error| self.fail(error))
    }

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
            hint = Some(Hint::Retry(self.approving(&asked.id)));
        }
        Error::from(error).hint(hint)
    }

    /// This command again with its [`Self::args`] and `extra`, in the same Project
    /// and Environment, with the same Ployz config.
    pub(crate) fn again(&self, extra: &[&str]) -> String {
        let mut words: Vec<&str> = super::config_flag(self.matches)
            .into_iter()
            .flatten()
            .collect();
        words.extend(self.words.iter().map(String::as_str));
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
                let carried = self.carried();
                let mut accept: Vec<String> = Vec::new();
                for name in accepted_names(error.details.get("accept")) {
                    if !accept.contains(&name) {
                        accept.push(name);
                    }
                }
                let version = error.details.get("version").and_then(text);
                let retry = self.retry(Carried {
                    accept,
                    version: version.or(carried.version),
                    ..carried
                });
                Refusal {
                    error: StoreCallError::Refused(error),
                    hint: Some(Hint::Retry(retry)),
                }
            }
            refusal => refusal,
        };
        self.fail(refusal)
    }

    /// A conflict handing back the fresh review (`details.diff`) names this command
    /// again at its version; any other names `read`, as [`with_refresh_hint`] does.
    pub(crate) fn at_review(&self, error: impl Into<Refusal>, read: &str) -> Refusal {
        let refusal = error.into();
        let version = match &refusal.error {
            StoreCallError::Refused(error) if error.code == RpcErrorCode::Conflict => error
                .details
                .pointer("/diff/version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            StoreCallError::Refused(_) | StoreCallError::Cloud(_) | StoreCallError::Stopped(_) => {
                None
            }
        };
        match version {
            Some(version) => with_next(
                refusal,
                |_| true,
                || {
                    self.retry(Carried {
                        version: Some(version),
                        ..self.carried()
                    })
                },
            ),
            None => with_refresh_hint(refusal, self.matches, read),
        }
    }

    fn approving(&self, id: &str) -> String {
        self.retry(Carried {
            approval: Some(id.to_owned()),
            ..self.carried()
        })
    }

    /// The flag this command passes a reviewed version with: `discard`'s and
    /// `history`'s `--version`, or `--expect-version` elsewhere. Read from the
    /// command tree: `ArgMatches` refuses unknown ids only in debug builds.
    fn version_flag(&self) -> (&'static str, &'static str) {
        let root = crate::cli::compiled();
        let mut command = &root;
        for word in &self.words {
            let Some(child) = command.find_subcommand(word) else {
                break;
            };
            command = child;
        }
        let takes_version = command
            .get_arguments()
            .any(|arg| arg.get_id() == "version" && arg.get_action().takes_values());
        if takes_version {
            ("version", "--version")
        } else {
            ("expect-version", "--expect-version")
        }
    }

    fn carried(&self) -> Carried {
        let one = |id| {
            self.matches
                .try_get_one::<String>(id)
                .ok()
                .flatten()
                .cloned()
        };
        Carried {
            accept: super::string_values(self.matches, "accept-volume-loss"),
            version: one(self.version_flag().0),
            approval: one("approval"),
        }
    }

    fn retry(&self, carried: Carried) -> String {
        let mut extra = Vec::new();
        for name in &carried.accept {
            extra.extend(["--accept-volume-loss", name.as_str()]);
        }
        if let Some(version) = &carried.version {
            extra.extend([self.version_flag().1, version.as_str()]);
        }
        if let Some(approval) = &carried.approval {
            extra.extend(["--approval", approval.as_str()]);
        }
        self.again(&extra)
    }
}

/// The Volume names a refusal's `accept` lists: flat, or by Environment.
fn accepted_names(accept: Option<&serde_json::Value>) -> Vec<String> {
    let names = |list: &serde_json::Value| {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    match accept {
        Some(serde_json::Value::Object(by_environment)) => {
            by_environment.values().flat_map(names).collect()
        }
        Some(list) => names(list),
        None => Vec::new(),
    }
}

pub(crate) fn sweep(store: &ConfigStore, who: &Actor) -> Result<(), ployz_core::RpcError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    store
        .system(
            &who.organization,
            &SystemEvent::Sweep(Sweep { now }),
            &Trusted::default(),
        )
        .map(drop)
}

struct Carried {
    accept: Vec<String>,
    version: Option<String>,
    approval: Option<String>,
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
    if let Some(url) = std::env::var_os(env::STORE) {
        let Some(url) = url.to_str() else {
            return Err(Error::usage("PLOYZ_STORE must be valid Unicode"));
        };
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
        let store = ConfigStore::open(url, key)?;
        // Cloud sweeps on a clock; the in-process Store sweeps as each command starts.
        sweep(&store, &actor).map_err(StoreCallError::Refused)?;
        return Ok(Some(Backend::Local(std::sync::Arc::new(store), actor)));
    }
    let credentials = CredentialStore::beside(config);
    let runtime = runtime()?;
    match runtime.block_on(cloud_account::from_env(&credentials)) {
        Ok(credential) => Ok(Some(Backend::Cloud(runtime, credential))),
        Err(LoginError::SignedOut) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Run `read`, several Store reads, again when `torn` finds they came back at
/// different revisions: an edit landed between them. Torn twice is a conflict.
pub(crate) fn at_one_revision<T>(
    command: &str,
    read: impl Fn() -> Result<T, Error>,
    torn: impl Fn(&T) -> bool,
) -> Result<T, Error> {
    let first = read()?;
    if !torn(&first) {
        return Ok(first);
    }
    let again = read()?;
    if torn(&again) {
        return Err(
            Error::conflict(format!("Working State changed while {command} read it."))
                .hint(Hint::Retry(super::typed_with(&[]))),
        );
    }
    Ok(again)
}

#[cfg(test)]
/// A local Store holding Project `shop`, and a Service `webN` created in it by
/// each `edit()`: a write between two reads.
pub(crate) fn store_with_edits(matches: &ArgMatches) -> (Store<'_>, impl Fn()) {
    let who = Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap());
    let local = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(&[7; 32]).unwrap()).unwrap(),
    );
    local
        .write(
            &who,
            &ployz_store::CreateProject {
                id: ployz_store::ProjectId::parse(mint()).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: ployz_store::EnvironmentId::parse(mint()).unwrap(),
            },
        )
        .unwrap();
    let edits = std::cell::Cell::new(0);
    let writer = (std::sync::Arc::clone(&local), who.clone());
    let edit = move || {
        edits.set(edits.get() + 1);
        writer
            .0
            .write(
                &writer.1,
                &ployz_store::CreateService {
                    id: ployz_store::ServiceLineageId::parse(mint()).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse(format!("web{}", edits.get())).unwrap(),
                    image: None,
                    template: None,
                },
            )
            .unwrap();
    };
    let store = Store {
        backend: Backend::Local(local, who),
        matches,
        words: vec!["get".to_owned()],
    };
    (store, edit)
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

/// The command that points `reference` at something there: a new value for its
/// variable, or a new copy of its Config file.
pub(crate) fn fix_broken(matches: &ArgMatches, reference: &ployz_store::BrokenReference) -> String {
    match &reference.at {
        ployz_store::ReferenceAt::Variable { service, variable } => next(
            matches,
            &["set", &format!("{service}.env.{variable}=VALUE")],
        ),
        ployz_store::ReferenceAt::File { config, file } => next(
            matches,
            &[
                "config",
                "put",
                config.as_str(),
                file.as_str(),
                "--from",
                "PATH",
            ],
        ),
    }
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

/// A value as a change line shows it: a long one cut to the start that fits 80 columns, and its length, so one big variable doesn't fill the terminal.
pub(crate) fn shown(value: &serde_json::Value) -> String {
    const SHOWN: usize = 80;
    let text = value.to_string();
    let (start, whole) = fitted(&text, SHOWN);
    if whole {
        return start;
    }
    let length = value
        .as_str()
        .map_or_else(|| text.chars().count(), |raw| raw.chars().count());
    format!("{start}… ({length} chars)")
}

/// JSON `text` as shown, kept piece by piece while it fits `columns`, and whether all of it did.
fn fitted(text: &str, columns: usize) -> (String, bool) {
    let mut start = String::new();
    let mut width = 0;
    for piece in pieces(text) {
        width += piece.width();
        if width > columns {
            return (start, false);
        }
        start.push_str(&piece);
    }
    (start, true)
}

/// JSON `text` as shown, in pieces a cut must not split: a JSON escape such as `\n` or
/// `\u0001`, each [`crate::ui::unprintable`] character as its `\u{…}` escape, or one grapheme
/// cluster, measured whole since its marks and selectors change its width.
fn pieces(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut rest = text;
    std::iter::from_fn(move || {
        let c = rest.chars().next()?;
        let (piece, length) = if c == '\\' {
            let length = if rest.get(1..2) == Some("u") { 6 } else { 2 };
            let escape = rest.get(..length).unwrap_or(rest);
            (escape.to_owned(), escape.len())
        } else if crate::ui::unprintable(c) {
            (crate::ui::escaped(c.encode_utf8(&mut [0; 4])), c.len_utf8())
        } else {
            let printable = rest
                .find(|c| c == '\\' || crate::ui::unprintable(c))
                .map_or(rest, |end| rest.get(..end).unwrap_or(rest));
            let grapheme = printable.graphemes(true).next().unwrap_or(printable);
            (grapheme.to_owned(), grapheme.len())
        };
        rest = rest.get(length..).unwrap_or_default();
        Some(piece)
    })
}

/// A staged Setting change as lines, `web.replicas: 1 → 3`. A domain change reads as
/// one line per domain, `web.domains: added shop.acme.ployz.app`, never as the field storing it.
pub(crate) fn changed(row: &ployz_core::config::ServiceSettingChange) -> Vec<String> {
    if let Some(lines) = config_changed(row).or_else(|| source_changed(row)) {
        return lines;
    }
    let Some(domains) = ployz_store::DomainChange::of(row) else {
        let value = |value: &serde_json::Value| match value.as_f64() {
            Some(gb) if row.path.ends_with(".memLimit") => ployz_store::memory_size(gb),
            Some(1.0) if row.path.ends_with(".cpuLimit") => "1 vCPU".to_owned(),
            Some(cpus) if row.path.ends_with(".cpuLimit") => format!("{cpus} vCPUs"),
            _ => shown(value),
        };
        return vec![format!(
            "{}: {} → {}",
            crate::ui::escaped(&row.path),
            value(&row.before),
            value(&row.after)
        )];
    };
    let service = row.path.split('.').next().unwrap_or_default();
    domains
        .iter()
        .map(|domain| format!("{service}.domains: {domain}"))
        .collect()
}

/// Each part of a source row and the Setting `set` and `get` name it by.
const SOURCE_PARTS: [(&str, &str); 4] = [
    ("image", "image"),
    ("credentials", "registryCredential"),
    ("repository", "repository"),
    ("rootDir", "rootDir"),
];

/// A source row as the Settings it holds, `web.image: "nginx:1.26" → "nginx:1.27"`,
/// or `None` when no part `set` names changed.
fn source_changed(row: &ployz_core::config::ServiceSettingChange) -> Option<Vec<String>> {
    let service = crate::ui::escaped(row.path.strip_suffix(".source")?);
    // No registry credential reads as unset, so a removed Service doesn't list one.
    let part = |value: &serde_json::Value, field: &str| {
        value
            .get(field)
            .filter(|part| **part != serde_json::Value::Bool(false))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let lines: Vec<String> = SOURCE_PARTS
        .iter()
        .filter_map(|&(field, setting)| {
            let (before, after) = (part(&row.before, field), part(&row.after, field));
            (before != after).then(|| {
                format!(
                    "{service}.{setting}: {} → {}",
                    shown(&before),
                    shown(&after)
                )
            })
        })
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// A Config row by name, `name: "app" → "app2"` or `files: removed empty.txt`, never
/// by the id its path keeps.
fn config_changed(row: &ployz_core::config::ServiceSettingChange) -> Option<Vec<String>> {
    let (_, field) = row.path.strip_prefix("configs.@")?.split_once('.')?;
    let line = match field.strip_prefix("files.") {
        Some(file) => {
            let edit = match (row.before.is_null(), row.after.is_null()) {
                (true, _) => "added",
                (_, true) => "removed",
                _ => "changed",
            };
            format!("files: {edit} {}", crate::ui::escaped(file))
        }
        None => format!(
            "{}: {} → {}",
            crate::ui::escaped(field),
            shown(&row.before),
            shown(&row.after)
        ),
    };
    Some(vec![line])
}

/// A staged domain change as JSON reports it, beside the raw row storing it as
/// `managedHostnames` or `routes`: `{"path": "web.domains", "change": "added shop"}`.
#[derive(serde::Serialize)]
pub(crate) struct DomainLine {
    path: String,
    change: String,
}

/// Every domain change among `rows`, as [`changed`] reads them to people.
pub(crate) fn domain_changes<'a>(
    rows: impl IntoIterator<Item = &'a ployz_core::config::ServiceSettingChange>,
) -> Vec<DomainLine> {
    rows.into_iter()
        .filter_map(|row| {
            let domains = ployz_store::DomainChange::of(row)?;
            let service = row.path.split('.').next().unwrap_or_default();
            Some(domains.into_iter().map(move |domain| DomainLine {
                path: format!("{service}.domains"),
                change: domain.to_string(),
            }))
        })
        .flatten()
        .collect()
}

/// A Setting's value as `get` and `inspect` show it, marked when it is the default
/// and followed by the default when it is not: `3 (default 1)`.
pub(crate) fn valued(row: &ployz_store::SettingRow) -> String {
    let value = plain(&row.value);
    // A variable or mount (`SERVICE.env.KEY`, `SERVICE.mounts.VOLUME`) has no
    // default: unset removes it.
    let removable = row.path.to_string().split('.').count() > 2;
    if row.value == row.default {
        format!("{value} (default)")
    } else if removable && row.default.is_null() {
        value
    } else {
        format!("{value} (default {})", plain(&row.default))
    }
}

/// A value on one line as JSON writes it, text quoted, so `""` and `"-"` never read as
/// unset, which alone shows as `-`.
pub(crate) fn plain(value: &serde_json::Value) -> String {
    if value.is_null() {
        "-".to_owned()
    } else {
        crate::ui::escaped(&value.to_string())
    }
}

/// A Service's domains on one line, `shop.acme.ployz.app, app.example.com (port 3000)`,
/// or `-` when it has none.
pub(crate) fn domain_list(domains: &[ployz_store::DomainRow], service: &str) -> String {
    let listed: Vec<String> = domains
        .iter()
        .filter(|row| row.domain.service.as_str() == service)
        .map(|row| match row.domain.port {
            Some(port) => format!("{} (port {port})", row.domain.shown()),
            None => row.domain.shown().to_owned(),
        })
        .collect();
    if listed.is_empty() {
        "-".to_owned()
    } else {
        listed.join(", ")
    }
}

/// Argument `arg`, a Service name. Core's name error quotes the value; a rejected
/// value is never echoed.
pub(crate) fn service_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_core::ServiceName, Error> {
    ployz_core::ServiceName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Service name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like web")
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
                    "Expected {what} names: each up to 63 lowercase letters, digits and -, starting and ending with a letter or digit"
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
        Error::usage("Expected a Volume name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like data")
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_staged_domain_change_reads_as_a_domain() {
        let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(serde_json::json!({
            "path": "web.managedHostnames", "kind": "update", "canRestore": false,
            "before": [{ "prefix": "web", "targetPort": null }],
            "after": [{ "prefix": "web", "targetPort": null }, { "prefix": "shop", "targetPort": null }],
        }))
        .unwrap();
        assert_eq!(changed(&row), ["web.domains: added shop.<cluster domain>"]);
        let named: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
            "path": "web.managedHostnames", "kind": "update", "canRestore": false,
            "before": [], "after": [{ "prefix": "shop", "targetPort": 8080 }],
            "hostnames": { "shop": "shop.acme.ployz.app" },
        }))
        .unwrap();
        assert_eq!(
            changed(&named),
            ["web.domains: added shop.acme.ployz.app on port 8080"]
        );
        let route: ployz_core::config::ServiceSettingChange =
            serde_json::from_value(serde_json::json!({
                "path": "web.routes.r1", "kind": "remove", "canRestore": false,
                "before": { "id": "r1", "hostname": "app.example.com", "targetPort": null },
                "after": null,
            }))
            .unwrap();
        assert_eq!(changed(&route), ["web.domains: removed app.example.com"]);
    }

    fn row(path: &str, before: serde_json::Value, after: serde_json::Value) -> Vec<String> {
        changed(
            &serde_json::from_value(json!({
                "path": path, "kind": "update", "canRestore": false,
                "before": before, "after": after,
            }))
            .unwrap(),
        )
    }

    #[test]
    fn a_staged_image_change_reads_as_the_setting_set_names() {
        let source = |image: &str| json!({ "type": "image", "credentials": false, "image": image });
        assert_eq!(
            row(
                "web.source",
                source("traefik/whoami:v1.10.0"),
                source("traefik/whoami:v1.10.1")
            ),
            [r#"web.image: "traefik/whoami:v1.10.0" → "traefik/whoami:v1.10.1""#]
        );
        assert_eq!(
            row(
                "web.source",
                json!({ "type": "git", "repository": "acme/web", "rootDir": "/" }),
                json!({ "type": "git", "repository": "acme/web", "rootDir": "/api" })
            ),
            [r#"web.rootDir: "/" → "/api""#]
        );
        assert_eq!(
            row("db.source", source("postgres:16"), json!(null)),
            [r#"db.image: "postgres:16" → null"#]
        );
    }

    #[test]
    fn a_staged_config_change_reads_by_name_not_id() {
        let at = "configs.@765c98f1-af88-4dc8-a63d-6f8127034983";
        assert_eq!(
            row(&format!("{at}.name"), json!("app"), json!("app2")),
            [r#"name: "app" → "app2""#]
        );
        let file = json!({ "content": "", "gid": 0, "mode": "0444", "uid": 0 });
        assert_eq!(
            row(&format!("{at}.files.empty.txt"), file.clone(), json!(null)),
            ["files: removed empty.txt"]
        );
        assert_eq!(
            row(&format!("{at}.files.run.sh"), json!(null), file),
            ["files: added run.sh"]
        );
        assert_eq!(
            row(
                &format!("{at}.files.a\u{202E}b.txt"),
                json!(null),
                json!({})
            ),
            [r"files: added a\u{202e}b.txt"]
        );
    }

    #[test]
    fn a_staged_memory_limit_reads_with_its_unit() {
        let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
            "path": "web.memLimit", "kind": "update", "canRestore": true,
            "before": null, "after": 0.201_326_592,
        }))
        .unwrap();
        assert_eq!(changed(&row), ["web.memLimit: null → 192 MiB"]);
    }

    #[test]
    fn a_staged_cpu_limit_reads_in_vcpus() {
        let row = |before: serde_json::Value, after: serde_json::Value| {
            let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
                "path": "web.cpuLimit", "kind": "update", "canRestore": true,
                "before": before, "after": after,
            }))
            .unwrap();
            changed(&row)
        };
        assert_eq!(
            row(json!(0.5), json!(2)),
            ["web.cpuLimit: 0.5 vCPUs → 2 vCPUs"]
        );
        assert_eq!(row(json!(null), json!(1)), ["web.cpuLimit: null → 1 vCPU"]);
    }

    /// Two reads of Working State's revision, with `between` run between them.
    fn two_reads(store: &Store<'_>, between: impl Fn()) -> Result<(u64, u64), Error> {
        let revision = || -> Result<u64, Error> {
            let view = store.read(&ployz_store::ServicesQuery {
                environment: EnvironmentRef::default(),
            })?;
            Ok(view.environment.revision.0)
        };
        let first = revision()?;
        between();
        Ok((first, revision()?))
    }

    #[test]
    fn an_edit_between_reads_reads_again_at_one_revision() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "get"])
            .unwrap();
        let (store, edit) = store_with_edits(leaf_matches(&root));
        let attempts = std::cell::Cell::new(0);
        let read = || {
            attempts.set(attempts.get() + 1);
            two_reads(&store, || {
                if attempts.get() == 1 {
                    edit();
                }
            })
        };
        let (first, second) = at_one_revision("get", read, |(a, b)| a != b).unwrap();
        assert_eq!(attempts.get(), 2);
        assert_eq!(first, second);
    }

    #[test]
    fn edits_between_every_read_refuse_as_a_conflict_to_retry() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "get"])
            .unwrap();
        let (store, edit) = store_with_edits(leaf_matches(&root));
        let error =
            at_one_revision("get", || two_reads(&store, &edit), |(a, b)| a != b).unwrap_err();
        assert!(
            error
                .hints()
                .iter()
                .any(|hint| matches!(hint, Hint::Retry(_)))
        );
        let error = error.report();
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(error.message, "Working State changed while get read it.");
    }

    #[test]
    fn a_set_setting_without_a_default_shows_its_default_as_nothing() {
        let row = |path: &str, value: serde_json::Value| ployz_store::SettingRow {
            path: ployz_store::SettingPath::parse(path).unwrap(),
            value,
            default: serde_json::Value::Null,
            apply: serde_json::from_value(json!("staged")).unwrap(),
            row: None,
            note: None,
        };
        assert_eq!(
            valued(&row("web.startCommand", json!("npm start"))),
            "\"npm start\" (default -)"
        );
        assert_eq!(valued(&row("web.startCommand", json!(null))), "- (default)");
        assert_eq!(valued(&row("web.env.PORT", json!("3000"))), "\"3000\"");
        assert_eq!(valued(&row("web.mounts.data", json!("/data"))), "\"/data\"");
    }

    #[test]
    fn empty_text_a_dash_and_unset_read_differently() {
        let shown = [plain(&json!("")), plain(&json!("-")), plain(&json!(null))];
        assert_eq!(shown, ["\"\"", "\"-\"", "-"]);
    }

    #[test]
    fn a_value_escapes_what_would_steer_the_terminal() {
        let value = json!("مرحبا\u{202E}evil\u{200B}\u{85}\t");
        let escaped = r#""مرحبا\u{202e}evil\u{200b}\u{85}\t""#;
        assert_eq!(plain(&value), escaped);
        assert_eq!(shown(&value), escaped);
        let long = shown(&json!(format!("\u{202E}{}", "a".repeat(100))));
        assert!(long.starts_with(r#""\u{202e}aaa"#), "{long}");
    }

    #[test]
    fn a_long_value_is_cut_within_80_columns_never_inside_an_escape() {
        let cut = |before: usize, odd: char| {
            shown(&json!(format!(
                "{}{odd}{}",
                "a".repeat(before),
                "z".repeat(90 - before)
            )))
        };
        let a = |count| "a".repeat(count);
        assert_eq!(cut(78, '\u{202E}'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(75, '\u{202E}'), format!("\"{}… (91 chars)", a(75)));
        assert_eq!(
            cut(71, '\u{202E}'),
            format!(r#""{}\u{{202e}}… (91 chars)"#, a(71))
        );
        assert_eq!(cut(78, '\n'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(76, '\u{1}'), format!("\"{}… (91 chars)", a(76)));
        assert_eq!(
            cut(70, '\u{1}'),
            format!(r#""{}\u0001zzz… (91 chars)"#, a(70))
        );
        assert_eq!(cut(78, '界'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(78, '😀'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(77, '界'), format!("\"{}界… (91 chars)", a(77)));
    }

    #[test]
    fn a_value_is_measured_in_columns_by_grapheme() {
        let wide = |c: &str| format!("\"{}… (40 chars)", c.repeat(39));
        assert_eq!(shown(&json!("界".repeat(40))), wide("界"));
        assert_eq!(shown(&json!("😀".repeat(40))), wide("😀"));
        let marked = format!("{}\u{301}\u{301}\u{301}", "a".repeat(78));
        assert_eq!(shown(&json!(marked)), format!("\"{marked}\""));
        let heart = format!("{}\u{2764}\u{FE0F}", "a".repeat(78));
        assert_eq!(
            shown(&json!(heart)),
            format!("\"{}… (80 chars)", "a".repeat(78))
        );
    }

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
    fn a_refused_volume_loss_names_the_exact_retry_keeping_the_approval() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging", "--approval", "apr_1"])
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
        let retry = "ployz deploy --accept-volume-loss data --expect-version 3:1:0.1 --approval apr_1 --env staging";
        assert_eq!(error.hints(), [Hint::Retry(retry.into())]);
        let error = error.report();
        assert_eq!(error.details.get("retry"), Some(&json!(retry)));
        assert_eq!(
            error.message,
            "This Deploy permanently deletes the data of data"
        );
    }

    #[test]
    fn the_same_command_again_keeps_a_typed_ployz_config() {
        let config = "/home/me/my ployz/config.yaml";
        let again = |typed: &[&str]| {
            let root = crate::cli::command().try_get_matches_from(typed).unwrap();
            let key = SealingKey::new(&[7; 32]).unwrap();
            Store {
                backend: Backend::Local(
                    std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                    Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
                ),
                matches: leaf_matches(&root),
                words: vec!["env".to_owned(), "rm".to_owned(), "staging".to_owned()],
            }
            .again(&["--confirm", "shop/staging"])
        };
        let typed = again(&[
            "ployz",
            "--ployz-config",
            config,
            "env",
            "rm",
            "staging",
            "--confirm",
            "shop/staging",
        ]);
        assert_eq!(
            typed,
            "ployz --ployz-config '/home/me/my ployz/config.yaml' env rm staging --confirm shop/staging"
        );
        let root = crate::cli::command()
            .try_get_matches_from(shell_words::split(&typed).unwrap())
            .unwrap();
        let leaf = leaf_matches(&root);
        assert_eq!(leaf.get_one::<String>("ployz-config").unwrap(), config);
        assert_eq!(leaf.get_one::<String>("confirm").unwrap(), "shop/staging");
        // A default or `PLOYZ_CONFIG` holds for the next command on its own.
        assert_eq!(
            again(&["ployz", "env", "rm", "staging", "--confirm", "shop/staging"]),
            "ployz env rm staging --confirm shop/staging"
        );
    }

    #[test]
    fn a_skip_deploy_retry_keeps_its_flag_scope_names_and_approval() {
        let typed = [
            "ployz",
            "deploy",
            "--skip-deploy",
            "--project",
            "shop",
            "--env",
            "production",
            "--expect-version",
            "3:1:0.1",
            "--approval",
            "apr_1",
        ];
        let root = crate::cli::command().try_get_matches_from(typed).unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let store = Store {
            backend: Backend::Local(
                std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            matches: leaf_matches(&root),
            words: vec!["deploy".to_owned(), "--skip-deploy".to_owned()],
        };
        let refused = ployz_core::RpcError {
            code: RpcErrorCode::ConfirmationRequired,
            message: "Next deploy deletes the data of pg-data, logs. Accept each by name".into(),
            details: json!({ "version": "3:1:0.1:9f2c", "accept": ["pg-data", "logs"] }),
            cause: Vec::new(),
        };
        let loss = "ployz deploy --skip-deploy --accept-volume-loss pg-data --accept-volume-loss logs \
                    --expect-version 3:1:0.1:9f2c --approval apr_1 --project shop --env production";
        assert_eq!(
            store.accepting(StoreCallError::Refused(refused)).hints(),
            [Hint::Retry(loss.into())]
        );
        let approval = store.approving("apr_2");
        assert_eq!(
            approval,
            "ployz deploy --skip-deploy --expect-version 3:1:0.1 --approval apr_2 --project shop --env production"
        );
        let reparsed = |retry: &str| {
            let root = crate::cli::command()
                .try_get_matches_from(shell_words::split(retry).unwrap())
                .unwrap();
            let leaf = leaf_matches(&root);
            let mut words: Vec<String> = root
                .subcommand_name()
                .into_iter()
                .map(str::to_owned)
                .collect();
            words.push(leaf.get_flag("skip-deploy").to_string());
            for id in ["project", "env", "expect-version", "approval"] {
                words.extend(leaf.get_one::<String>(id).cloned());
            }
            words.extend(crate::handlers::string_values(leaf, "accept-volume-loss"));
            words
        };
        assert_eq!(
            reparsed(loss),
            [
                "deploy",
                "true",
                "shop",
                "production",
                "3:1:0.1:9f2c",
                "apr_1",
                "pg-data",
                "logs"
            ]
        );
        assert_eq!(
            reparsed(&approval),
            ["deploy", "true", "shop", "production", "3:1:0.1", "apr_2"]
        );
    }

    #[test]
    fn a_volume_loss_retry_passes_the_version_with_the_flag_its_command_takes() {
        for words in [
            &["deploy"][..],
            &["project", "rm", "shop", "--confirm", "shop"],
            &["env", "rm", "br", "--confirm", "shop/br"],
        ] {
            let root = crate::cli::command()
                .try_get_matches_from(std::iter::once("ployz").chain(words.iter().copied()))
                .unwrap();
            let key = SealingKey::new(&[7; 32]).unwrap();
            let store = Store {
                backend: Backend::Local(
                    std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                    Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
                ),
                matches: leaf_matches(&root),
                words: words.iter().map(|word| (*word).to_owned()).collect(),
            };
            let refused = ployz_core::RpcError {
                code: RpcErrorCode::ConfirmationRequired,
                message: "This permanently deletes the data of data".into(),
                details: json!({ "version": "3:1:0.1", "accept": ["data"] }),
                cause: Vec::new(),
            };
            let hints = store.accepting(StoreCallError::Refused(refused)).hints();
            let [Hint::Retry(retry)] = &hints[..] else {
                panic!("{words:?} names no retry");
            };
            let reparsed = crate::cli::command()
                .try_get_matches_from(shell_words::split(retry).unwrap())
                .unwrap_or_else(|error| panic!("{retry}: {error}"));
            assert_eq!(
                leaf_matches(&reparsed)
                    .get_one::<String>("expect-version")
                    .map(String::as_str),
                Some("3:1:0.1"),
                "{retry}"
            );
        }
    }

    #[test]
    fn publish_is_not_a_command() {
        assert!(
            crate::cli::command()
                .try_get_matches_from(["ployz", "publish"])
                .is_err()
        );
    }

    #[test]
    fn a_refused_review_names_this_command_at_the_fresh_version() {
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
        let next_of = |details| {
            let refusal = store.at_review(
                StoreCallError::Refused(ployz_core::RpcError {
                    code: RpcErrorCode::Conflict,
                    message: "This draft includes a pull request: review it and pass its version"
                        .into(),
                    details,
                    cause: Vec::new(),
                }),
                "diff",
            );
            Error::from(refusal.error).hint(refusal.hint).hints()
        };
        assert_eq!(
            next_of(json!({ "diff": { "version": "3:1:0.1" } })),
            [Hint::Next(
                "ployz deploy --expect-version 3:1:0.1 --env staging".into()
            )]
        );
        assert_eq!(
            next_of(json!({})),
            [Hint::Next("ployz diff --env staging".into())]
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
