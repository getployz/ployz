//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

mod args;
mod hints;
mod refusal;
mod render;
mod sealing;

pub(crate) use args::{
    environment, named_environment, project, project_arg, scoped, service_name, service_names,
    volume_name, volume_names,
};
pub(crate) use hints::{broken_line, fix_broken, next, next_in, scope_args};
pub(crate) use refusal::{Refusal, with_next, with_refresh_hint};
pub(crate) use render::{
    DomainLine, changed, config_field_changed, domain_changes, domain_list, plain, shown, valued,
    word,
};

use clap::ArgMatches;
use futures_util::FutureExt as _;
use ployz_core::Region;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use ployz_store::{
    Actor, Admit, AdmitRemoval, Ask, ConfigStore, DeploymentId, DeploymentSummary, DomainEvidence,
    DueQuery, Edit, Edited, EnvironmentName, EnvironmentRef, ObservationFailure, OrganizationId,
    ProjectName, Publish, Published, Removal, RemovalsQuery, Remove, SealingKey, Sweep,
    SystemEvent, Tell, Trusted, VolumeObservation,
};

use super::{Error, config_path, leaf_matches, required, runtime};
use crate::approval::{self, Asked};
use crate::cloud;
use crate::cloud::account::{Credential, StoreCallError, StoreClient};
pub(crate) use crate::cloud::account::{LOCAL_ORGANIZATION, local_mode};
use crate::cloud::login::{CredentialStore, LoginError};
use crate::env;
use ployz_core::Hint;

/// How long a read of the Servers waits on them before it reads them as unobserved.
const OBSERVE_SERVERS_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the Config Store is: Cloud's over HTTPS, as `PLOYZ_TOKEN` or this device's
/// sign-in, or the hidden in-process SQLite Store when `PLOYZ_STORE` is set.
pub(crate) enum Backend {
    Local(std::sync::Arc<ConfigStore>, Actor),
    Cloud(tokio::runtime::Runtime, Credential),
}

impl Backend {
    /// Where a drain reads the versions each moved copy carries: this Store, as this CLI.
    pub(crate) fn kept_versions(self) -> crate::deploy::KeptVersions {
        match self {
            Self::Local(store, who) => crate::deploy::KeptVersions::new(store, who.organization),
            Self::Cloud(_, credential) => {
                crate::deploy::KeptVersions::Cloud(std::sync::Arc::new(credential))
            }
        }
    }

    /// The client for this Store, as this CLI.
    fn client(&self) -> StoreClient<'_> {
        match self {
            Self::Local(store, who) => StoreClient::Local(store, who),
            Self::Cloud(_, credential) => StoreClient::Cloud(credential, cloud::http::Via::Cli),
        }
    }

    /// Run one of [`Self::client`]'s calls to its end.
    fn call<T>(&self, call: impl std::future::Future<Output = T>) -> T {
        match self {
            Self::Local(..) => call
                .now_or_never()
                .expect("the in-process Store answers without awaiting"),
            Self::Cloud(runtime, _) => runtime.block_on(call),
        }
    }

    /// Answer `query`: a Store query payload, read as its own view.
    pub(crate) fn read<Q: Ask>(&self, query: &Q) -> Result<Q::View, StoreCallError> {
        self.read_seen(query, &Trusted::default())
    }

    /// [`Self::read`] with what this CLI observed; Cloud gathers its own.
    fn read_seen<Q: Ask>(&self, query: &Q, seen: &Trusted) -> Result<Q::View, StoreCallError> {
        self.call(self.client().read(query, seen))
    }

    /// [`Self::read`] once, giving up on Cloud after `limit`: `None` when it doesn't answer.
    pub(crate) fn read_within<Q: Ask>(&self, query: &Q, limit: Duration) -> Option<Q::View> {
        match self {
            Self::Local(..) => self.read(query).ok(),
            Self::Cloud(runtime, _) => runtime
                .block_on(async {
                    tokio::time::timeout(limit, self.client().read(query, &Trusted::default()))
                        .await
                })
                .ok()?
                .ok(),
        }
    }

    fn write<C: Tell>(
        &self,
        command: &C,
        trusted: &Trusted,
        approval: Option<&str>,
    ) -> Result<C::Written, StoreCallError> {
        self.call(self.client().write(command, trusted, approval))
    }
}

/// The Config Store as one command sees it: a refusal becomes this command's
/// failure, and an ambiguous Project names `ployz link --project PROJECT`.
pub(crate) struct Store<'m> {
    backend: Backend,
    root: &'m ArgMatches,
    matches: &'m ArgMatches,
    /// What this command's retry gives beyond its command line, as `--confirm NAME`.
    retry_sets: Vec<(&'static str, String)>,
    /// This run reads secrets from stdin, so its approval retry asks for a new approval.
    secrets: bool,
}

impl<'m> Store<'m> {
    #[cfg(test)]
    pub(super) fn for_test(backend: Backend, root: &'m ArgMatches) -> Self {
        Self {
            backend,
            root,
            matches: leaf_matches(root),
            retry_sets: Vec::new(),
            secrets: false,
        }
    }

    /// The subcommand words of this command, `deploy`.
    pub(crate) fn command_path(&self) -> String {
        crate::cli::command_path(self.root)
    }

    /// Mark this run as reading secrets from stdin (`env sync --value`): a retry can't
    /// repeat them, so its approval retry names no approval and asks for a new one.
    pub(crate) const fn reads_secrets(mut self, secrets: bool) -> Self {
        self.secrets = secrets;
        self
    }

    /// Have this command's retry give `id` the value `value`, as a `--confirm` the
    /// command line may lack.
    pub(crate) fn retrying(mut self, id: &'static str, value: &str) -> Self {
        self.retry_sets.push((id, value.to_owned()));
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

    /// Answer `query` about domains. Cloud observes the Servers itself; for the
    /// in-process Store this CLI does, from one Runtime Watch frame.
    pub(crate) fn read_domains<Q: Ask>(&self, query: &Q) -> Result<Q::View, Error> {
        let seen = match &self.backend {
            Backend::Local(..) => Trusted {
                domains: self.observe_domains(),
                ..Trusted::default()
            },
            Backend::Cloud(..) => Trusted::default(),
        };
        self.backend
            .read_seen(query, &seen)
            .map_err(|error| self.fail(error))
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

    pub(crate) fn try_edit(&self, edit: &Edit) -> Result<Edited, StoreCallError> {
        let named = edit.regions();
        let regions = (!named.is_empty())
            .then(|| self.observe_regions())
            .flatten();
        if let (Backend::Cloud(..), Some(regions)) = (&self.backend, &regions) {
            for region in &named {
                ployz_store::refuse_unknown_region(region, regions)
                    .map_err(StoreCallError::Refused)?;
            }
        }
        let trusted = Trusted {
            regions,
            ..Trusted::default()
        };
        self.approved(edit, &trusted, Some(&["diff"]))
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
        let mut said_yes = approval.is_some();
        loop {
            let refused = match self.backend.write(command, trusted, approval.as_deref()) {
                Err(StoreCallError::Refused(refused)) => refused,
                Err(StoreCallError::Cloud(error @ LoginError::Unanswered { .. })) => {
                    let check = check.map(|check| Hint::Inspect(next(self.matches, check)));
                    return Err(StoreCallError::Stopped(Error::from(error).hint(check)));
                }
                written => return written,
            };
            let (Backend::Cloud(..), Some(asked), false) =
                (&self.backend, Asked::of(&refused), crate::ui::json())
            else {
                return Err(StoreCallError::Refused(refused));
            };
            let verb = asked.verb(
                ployz_store::Command::decode(command.to_command())
                    .ok()
                    .as_ref(),
            );
            if said_yes {
                return Err(StoreCallError::Stopped(approval::moved(verb, refused)));
            }
            approval = Some(approval::settle(
                verb,
                &asked,
                self.approving(asked.digest()),
            )?);
            said_yes = true;
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
            // Naming Services still saves Working State, which reviews what it drops.
            (Backend::Local(..), Admit::Deploy(deploy)) => {
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

    /// Do in the in-process Store what Cloud's scheduler does on its own: sweep every
    /// removal on, then admit the removal Deployments now due for `project`, or only
    /// its `environment`. Over HTTPS this admits nothing.
    pub(crate) fn admit_due_removals(
        &self,
        project: &ProjectName,
        environment: Option<&EnvironmentName>,
    ) -> Result<Vec<DeploymentSummary>, Error> {
        let Backend::Local(store, who) = &self.backend else {
            return Ok(Vec::new());
        };
        sweep(store, who).map_err(|error| self.fail(StoreCallError::Refused(error)))?;
        let mut admitted = Vec::new();
        for due in self.read(&DueQuery {})? {
            let summary = due.environment;
            if summary.project != *project || environment.is_some_and(|name| *name != summary.name)
            {
                continue;
            }
            admitted.push(
                self.admit(&Admit::Remove(AdmitRemoval {
                    id: DeploymentId::parse(mint())?,
                    environment: EnvironmentRef {
                        project: Some(project.clone()),
                        environment: Some(summary.name),
                    },
                }))
                .map_err(|error| self.fail(error))?,
            );
        }
        Ok(admitted)
    }

    /// `--context`; without it, this reaches the Cluster `--connect` or the current
    /// context names.
    fn context(&self) -> Option<&str> {
        self.matches
            .try_get_one::<String>("context")
            .ok()
            .flatten()
            .map(String::as_str)
    }

    /// Reach this Store's Cluster: the in-process Store's through `--connect` or the
    /// context, never Cloud's Server Access, whose Cluster isn't this Store's; Cloud's
    /// through this credential's Server Access.
    async fn cluster(&self) -> Result<crate::connect::Client, Error> {
        let context = self.context();
        match &self.backend {
            Backend::Local(..) => super::connect_context(self.matches, context).await,
            Backend::Cloud(..) => super::route::connect(self.matches, context).await,
        }
    }

    /// Which Servers hold the data of the Volumes each of `queries` removes: a
    /// Deploy, removal or rollback. A Cluster this can't reach is a failed
    /// observation that says why, so the Store refuses with it.
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
        let runtime = runtime().map_err(StoreCallError::Stopped)?;
        // The endpoint names the refusal only while no Server is known.
        let endpoint = crate::cli::connection(self.matches).map(str::to_owned);
        runtime.block_on(async {
            let mut client = match self.cluster().await {
                Ok(client) => client,
                Err(error)
                    if matches!(
                        error.context_error(),
                        Some(crate::context::ContextError::NoConnections { .. })
                    ) =>
                {
                    return Err(StoreCallError::Stopped(error));
                }
                Err(error) => {
                    let failure = if error.daemon_answered() {
                        ObservationFailure::answered(None, error.report())
                    } else {
                        ObservationFailure::unreachable(error.report())
                    };
                    let failure = failure.at_endpoint(endpoint);
                    return Ok(Some(VolumeObservation::failed(sought, Vec::new(), failure)));
                }
            };
            Ok(Some(match client.observe_volumes(sought.clone()).await {
                Ok(observed) => observed,
                Err(error) => {
                    let server = client.entry_machine();
                    let failure = ObservationFailure::answered(server, error)
                        .at_endpoint(endpoint.filter(|_| server.is_none()));
                    VolumeObservation::failed(sought, server.into_iter().collect(), failure)
                }
            }))
        })
    }

    /// One Runtime Watch frame of this Store's Cluster; none when it isn't reached in time.
    fn observe_frame(&self) -> Option<ployz_core::RuntimeWatchFrame> {
        let runtime = runtime().ok()?;
        let observe = async { self.cluster().await.ok()?.runtime_frame().await.ok() };
        runtime
            .block_on(async { tokio::time::timeout(OBSERVE_SERVERS_TIMEOUT, observe).await })
            .ok()?
    }

    /// The regions the Servers are in, by their `region` label; none when a Server went
    /// unobserved.
    fn observe_regions(&self) -> Option<BTreeSet<Region>> {
        let frame = self.observe_frame()?;
        frame.incomplete_ids.machines.is_empty().then(|| {
            frame
                .machines
                .iter()
                .filter_map(|observed| Region::of(&observed.machine))
                .collect()
        })
    }

    /// The region a new Environment starts in: the Cluster's first Server's. None when any
    /// Server has no region, or a Server went unobserved, which pins nothing.
    pub(crate) fn new_environment_region(&self) -> Option<Region> {
        let frame = self.observe_frame()?;
        Region::of_first_server(ployz_core::JoinedServer::all(&frame))
    }

    /// The in-process Store's Servers' certificates, ingress Servers and their addresses.
    /// Servers this can't reach leave the certificates unobserved. DNS failures reach a status
    /// through the certificate attempts that met them.
    fn observe_domains(&self) -> DomainEvidence {
        let mut evidence = DomainEvidence::default();
        if let Some(frame) = self.observe_frame() {
            let ingress = frame
                .machines
                .iter()
                .filter(|observed| observed.machine.accepts_ingress);
            evidence.ingress_servers = frame
                .incomplete_ids
                .machines
                .is_empty()
                .then(|| u32::try_from(ingress.clone().count()).unwrap_or(u32::MAX));
            evidence.ingress_addresses = ingress
                .filter_map(|observed| observed.machine.public_ip)
                .map(|address| address.to_string())
                .collect();
            evidence.certificates = Some(frame.certificates);
            evidence.incomplete_certificates = frame.incomplete_ids.certificates;
        }
        evidence
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
            .block_on(cloud::account::upload(
                credential,
                deployment.as_str(),
                archive,
            ))
            .map_err(|error| self.fail(error))
    }

    pub(crate) const fn backend(&self) -> &Backend {
        &self.backend
    }

    pub(crate) fn into_backend(self) -> Backend {
        self.backend
    }

    /// The in-process Store, which only the hidden test mode has: there this CLI
    /// runs Deployments itself. `claim` and `record` never cross HTTPS.
    pub(crate) fn local(&self) -> Option<&std::sync::Arc<ConfigStore>> {
        match &self.backend {
            Backend::Local(store, _) => Some(store),
            Backend::Cloud(..) => None,
        }
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

/// The Config Store `root`'s command reads and writes.
pub(crate) fn store(root: &ArgMatches) -> Result<Store<'_>, Error> {
    reachable(root)?.ok_or_else(|| LoginError::SignedOut.into())
}

/// The Store, or `None` when there's none to reach: signed out, no `PLOYZ_TOKEN`
/// and no `PLOYZ_STORE`.
pub(crate) fn reachable(root: &ArgMatches) -> Result<Option<Store<'_>>, Error> {
    let matches = leaf_matches(root);
    Ok(backend_at(&config_path(matches)?)?.map(|backend| Store {
        backend,
        root,
        matches,
        retry_sets: Vec::new(),
        secrets: false,
    }))
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
            None => crate::config_dir::ConfigDir::new(config).store_key(),
        };
        let key = SealingKey::from_file(&key)?;
        let store = ConfigStore::open(url, key)?;
        // Cloud sweeps on a clock; the in-process Store sweeps as each command starts.
        sweep(&store, &actor).map_err(StoreCallError::Refused)?;
        return Ok(Some(Backend::Local(std::sync::Arc::new(store), actor)));
    }
    let credentials = CredentialStore::beside(config);
    let runtime = runtime()?;
    match runtime.block_on(cloud::account::from_env(&credentials)) {
        Ok(credential) => Ok(Some(Backend::Cloud(runtime, credential))),
        Err(LoginError::SignedOut) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl Store<'_> {
    /// Run `read`, several Store reads, again when `torn` finds they came back at
    /// different revisions: an edit landed between them. Torn twice is a conflict.
    pub(crate) fn at_one_revision<T>(
        &self,
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
                    .hint(self.again()),
            );
        }
        Ok(again)
    }
}

#[cfg(test)]
/// A local Store holding Project `shop`, and a Service `webN` created in it by
/// each `edit()`: a write between two reads.
pub(crate) fn store_with_edits(root: &ArgMatches) -> (Store<'_>, impl Fn()) {
    let who = Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap());
    let local = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(&[7; 32]).unwrap()).unwrap(),
    );
    local
        .write(
            &who,
            &ployz_store::CreateProject {
                id: ployz_store::ProjectId::parse(mint()).unwrap(),
                name: ployz_store::ProjectName::parse("shop").unwrap(),
                default_environment: ployz_store::EnvironmentId::parse(mint()).unwrap(),
                region: None,
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
    let store = Store::for_test(Backend::Local(local, who), root);
    (store, edit)
}

/// Mint the ID a create is keyed by, so a retried request replays instead of repeating.
pub(crate) fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use ployz_core::RpcErrorCode;

    use super::*;

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
        let (store, edit) = store_with_edits(&root);
        let attempts = std::cell::Cell::new(0);
        let read = || {
            attempts.set(attempts.get() + 1);
            two_reads(&store, || {
                if attempts.get() == 1 {
                    edit();
                }
            })
        };
        let (first, second) = store.at_one_revision("get", read, |(a, b)| a != b).unwrap();
        assert_eq!(attempts.get(), 2);
        assert_eq!(first, second);
    }

    #[test]
    fn edits_between_every_read_refuse_as_a_conflict_to_retry() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "get"])
            .unwrap();
        let (store, edit) = store_with_edits(&root);
        let error = store
            .at_one_revision("get", || two_reads(&store, &edit), |(a, b)| a != b)
            .unwrap_err();
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
}
