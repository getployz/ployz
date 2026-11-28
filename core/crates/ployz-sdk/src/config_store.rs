//! The Config Store for Cloud: `read` and `write` as Promises. Every Store call,
//! the runner's and GitHub's included, goes through `ployz::sdk::store_call`: on a
//! worker thread, bounded, ending `unavailable` when it waits or runs too long.
//! The Store's in-process-only operations are never bound here without their own caller checks: Cloud's worker
//! runs a Deployment in one call, so its secrets and evidence never reach JavaScript.

use std::collections::BTreeMap;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz_core::{RpcError, ServiceName};
use ployz_store::{Actor, DeploymentId, GithubBuildId, OrganizationId, RunEvidence, RunnerId};
use serde::de::DeserializeOwned;

use crate::boundary::{
    from_js, invalid_argument, parse_connections, parse_expect, rpc_to_napi, whole_numbers,
};

/// One Config Store over one database.
#[napi]
pub struct ConfigStore {
    store: Arc<ployz_store::ConfigStore>,
}

/// Open the Config Store at `url` (`postgres://…`, or `sqlite:PATH` in tests),
/// migrating its tables as needed. It seals secrets with a key derived from
/// `sealing_secret`, Cloud's encryption secret.
///
/// # Errors
/// Returns `invalid_argument` for an unsupported URL, or a storage error.
#[napi]
pub async fn open_config_store(url: String, sealing_secret: String) -> Result<ConfigStore> {
    // Cloud's container log keeps stderr; its own logger never sees the Store's (#3786).
    ployz::sdk::stderr_log::install();
    let sealing = ployz_store::SealingKey::new(sealing_secret.as_bytes()).map_err(rpc_to_napi)?;
    let store = tokio::task::spawn_blocking(move || ployz_store::ConfigStore::open(&url, sealing))
        .await
        .map_err(|error| Error::from_reason(error.to_string()))?
        .map_err(rpc_to_napi)?;
    Ok(ConfigStore {
        store: Arc::new(store),
    })
}

/// Which of `connections`' Servers hold each Docker Volume named in `sought`: the
/// evidence (`VolumeObservation`) Cloud admits a Deploy that removes deployed
/// Volumes with. Servers that don't answer are named, never assumed empty, and when
/// the check fails its `failure` says whether no Server was reached or which one
/// answered with an error, and why. With `expect`, only a Server whose Cluster holds
/// that Sealing public key answers, as `connect`'s `expect` does.
///
/// # Errors
/// Returns `invalid_argument` for malformed input.
#[napi(ts_return_type = "Promise<VolumeObservation>")]
pub async fn observe_volumes(
    #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
    sought: Vec<String>,
    #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let connections = parse_connections(connections)?;
    let expect = parse_expect(expect)?;
    let sought = sought
        .into_iter()
        .map(ployz_core::DockerVolumeName::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid_argument("invalid Docker Volume name"))?;
    to_json(ployz::sdk::observe_volumes(connections, sought, expect).await)
}

/// The Servers of `connections`' Cluster that can hold a preview: each takes Services,
/// isn't leaving, and advertises a preview's home and its Close. Cloud tells the Store
/// these as `ConfigTrusted.preview_homes`; a preview whose home isn't among them opens
/// again on another. `expect` is as `observeVolumes`'.
///
/// # Errors
/// Returns `invalid_argument` for malformed input, or the connection's error when no
/// Server can be reached.
#[napi(ts_return_type = "Promise<PreviewHomes & { foreign: string[] }>")]
pub async fn observe_preview_homes(
    #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
    #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let connections = parse_connections(connections)?;
    let expect = parse_expect(expect)?;
    to_json(ployz::sdk::observe_preview_homes(connections, expect).await)
}

/// The keys the first Server of `connections` reaches trusts, as its own record lists
/// them. Cloud tells the Store these as `ConfigTrusted.signers`, which a preview rule's
/// signature must check against. `expect` is as `observeVolumes`'.
///
/// # Errors
/// Returns `invalid_argument` for malformed input, or the connection's error when no
/// Server can be reached.
#[napi(ts_return_type = "Promise<Signers & { foreign: string[] }>")]
pub async fn observe_signers(
    #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
    #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let connections = parse_connections(connections)?;
    let expect = parse_expect(expect)?;
    to_json(ployz::sdk::observe_signers(connections, expect).await)
}

/// Every copy of every Volume on `connections`' Servers, by role
/// (`CopyObservation`): what Cloud's sweep finds orphaned mirror slots with. Servers
/// that don't answer are named, never assumed empty. `expect` is as `observeVolumes`';
/// `foreign` names the Machines it left because they run in another Cluster.
///
/// # Errors
/// Returns `invalid_argument` for malformed input, or the connection's error when
/// no Server can be reached.
#[napi(ts_return_type = "Promise<CopyObservation & { foreign: string[] }>")]
pub async fn observe_copies(
    #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
    #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let connections = parse_connections(connections)?;
    let expect = parse_expect(expect)?;
    to_json(ployz::sdk::observe_copies(connections, expect).await)
}

/// Which Services run on `connections`' Servers, by Namespace
/// (`RunningObservation`): the evidence Cloud admits a full Deploy that prunes with,
/// naming each Service it removes. Servers that don't answer are named, and when the
/// check fails its `failure` says why. `expect` is as `observeVolumes`'.
///
/// # Errors
/// Returns `invalid_argument` for malformed input.
#[napi(ts_return_type = "Promise<RunningObservation>")]
pub async fn observe_running(
    #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
    #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let connections = parse_connections(connections)?;
    let expect = parse_expect(expect)?;
    to_json(ployz::sdk::observe_running(connections, expect).await)
}

#[napi]
impl ConfigStore {
    /// Answer a `ConfigQuery` as the given Organization. `trusted` is what Cloud
    /// observed itself (`ConfigTrusted`), never the caller's.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi(ts_return_type = "Promise<ConfigView>")]
    pub async fn read(
        &self,
        organization: String,
        #[napi(ts_arg_type = "ConfigQuery")] query: serde_json::Value,
        #[napi(ts_arg_type = "ConfigTrusted")] trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "whole numbers first, as `from_js`"
        )]
        let query = ployz_store::Query::decode(whole_numbers(query))
            .map_err(|error| refused("Expected a Config Store query", &error))?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.read_trusted(&who, &query, &trusted))
            .await
    }

    /// Apply a `ConfigCommand` as `principal` (who Cloud authenticated; none for
    /// Cloud itself) in the given Organization, in one transaction. `trusted` is
    /// evidence Cloud gathered itself (`ConfigTrusted`), never the caller's.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi(ts_return_type = "Promise<ConfigCommitted>")]
    pub async fn write(
        &self,
        organization: String,
        #[napi(ts_arg_type = "ConfigCommand")] command: serde_json::Value,
        #[napi(ts_arg_type = "ConfigTrusted")] trusted: Option<serde_json::Value>,
        principal: Option<String>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, principal)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "whole numbers first, as `from_js`"
        )]
        let command = ployz_store::Command::sent(whole_numbers(command)).map_err(rpc_to_napi)?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.commit(&who, &command, &trusted))
            .await
    }

    /// What retiring Conditional Syncs did (`Converted`) when opening this
    /// Store converted them, for Cloud to log; null after an earlier open did.
    ///
    /// # Errors
    /// Returns an error if the receipt can't be serialized.
    #[napi(ts_return_type = "Converted | null")]
    pub fn converted(&self) -> Result<serde_json::Value> {
        serde_json::to_value(self.store.converted())
            .map_err(|error| Error::from_reason(error.to_string()))
    }

    /// The pull requests (`PullRequestRef[]`) whose checks a change in Environment
    /// `environment` may move, such as a Deployment of it ending. Only Cloud calls this.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed ID, or a storage error.
    #[napi(ts_return_type = "Promise<PullRequestRef[]>")]
    pub async fn checks(&self, environment: String) -> Result<serde_json::Value> {
        let environment = ployz_store::EnvironmentId::parse(environment).map_err(rpc_to_napi)?;
        self.run(move |store| store.checks(&environment)).await
    }

    /// The Git Services Deployment `deployment` builds (`GitSource[]`), each with its
    /// pinned commit, if any. Only Cloud's worker calls this, to read their sources.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    #[napi(ts_return_type = "Promise<GitSource[]>")]
    pub async fn deployment_sources(&self, deployment: String) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        self.run(move |store| store.sources(&deployment)).await
    }

    /// Pin the commits Cloud resolved (`{runtime Service name: commit}`) for
    /// Deployment `deployment`'s Git Services; a pinned commit never changes. Resolves
    /// to every source with its pin. `trusted` carries the Organization's members as
    /// Cloud read them: without them, a Trusted pull request a person wrote pins
    /// nothing, while a bot's from the repository itself still does. An approved head,
    /// and a commit for any Environment but a PR Environment, pin without them. Only
    /// Cloud's worker calls this, as worker attempt `runner`, which must own the
    /// Deployment.
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment was replaced, cancelled or ended, or
    /// (`newer`) once a later attempt began, `invalid_argument` for a bad pin, or a
    /// storage error.
    #[napi(ts_return_type = "Promise<GitSource[]>")]
    pub async fn pin_sources(
        &self,
        deployment: String,
        #[napi(ts_arg_type = "Record<string, string>")] commits: serde_json::Value,
        runner: String,
        #[napi(ts_arg_type = "Pick<ConfigTrusted, 'members'>")] trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let commits = decode(commits, "Expected commits by Service name")?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.pin(&deployment, &commits, &trusted, &runner))
            .await
    }

    /// Run the Organization's queued Deployment `deployment` as `runner` on one of
    /// `connections` (`Connection[]`), and resolve to its summary once its outcome is
    /// recorded. Its Git Services build from `sources.checkouts` (`{runtime Service
    /// name: directory}`) at their pinned commits, and its uploaded Services from
    /// `sources.upload`, the directory Cloud extracted its upload to (without it, they
    /// reuse a usable image or need a new upload); `sources.failure` says why Cloud
    /// could not read them, and is recorded as the reason nothing ran. Only Cloud's
    /// worker calls this. It takes as long as its builds and Deploy do. With `expect`,
    /// it runs only through a Server whose Cluster holds that Sealing public key, as
    /// `connect`'s `expect` does.
    ///
    /// # Errors
    /// Returns `conflict` when this runner has nothing to run, or a storage error.
    #[napi(ts_return_type = "Promise<DeploymentSummary>")]
    pub async fn run_deployment(
        &self,
        organization: String,
        deployment: String,
        runner: String,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(
            ts_arg_type = "{ checkouts?: Record<string, string>; upload?: string; failure?: string; reader?: string }"
        )]
        sources: Option<serde_json::Value>,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        #[derive(Default, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Sources {
            #[serde(default)]
            checkouts: BTreeMap<ServiceName, std::path::PathBuf>,
            upload: Option<std::path::PathBuf>,
            failure: Option<String>,
            reader: Option<String>,
        }
        let who = actor(organization, None)?;
        let (deployment, runner) = ids(deployment, runner)?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        let sources: Sources = sources
            .map(|sources| decode(sources, "Expected checkouts, upload and failure"))
            .transpose()?
            .unwrap_or_default();
        let checkouts = match sources.failure {
            Some(_) if !sources.checkouts.is_empty() || sources.upload.is_some() => {
                return Err(invalid_argument(
                    "Expected checkouts and an upload, or why they couldn't be read: not both",
                ));
            }
            Some(reason) => Err(reason),
            None => Ok(ployz::sdk::Sources {
                checkouts: sources.checkouts,
                upload: sources.upload,
                reader: sources.reader.map(ployz_core::GithubToken::new),
            }),
        };
        // Only a Deployment of this Organization runs here.
        let owned = deployment.clone();
        self.run(move |store| store.read(&who, &ployz_store::DeploymentQuery { id: owned }))
            .await?;
        to_json(
            ployz::sdk::run_deployment_expecting(
                ployz_core::Operator::Cloud,
                Arc::clone(&self.store),
                deployment,
                runner,
                connections,
                expect,
                checkouts,
            )
            .await,
        )
    }

    /// Drain the Organization's Server `machine` within `scope` (`DrainScope`) on one of
    /// `connections`, and resolve to its report, partial or not. Each copy it moves
    /// carries the signed versions this Store keeps for that copy's own spec, so a
    /// Server that never ran the version admits it. Only Cloud's worker calls this. With
    /// `expect`, it runs only through a Server whose Cluster holds that Sealing public
    /// key, as `connect`'s `expect` does.
    ///
    /// # Errors
    /// Returns `invalid_argument` for malformed input, the connection's error when no
    /// Server can be reached, or the Drain's refusal before anything moved.
    #[napi(ts_return_type = "Promise<DrainReport>")]
    pub async fn drain_machine(
        &self,
        organization: String,
        machine: String,
        #[napi(ts_arg_type = "DrainScope")] scope: serde_json::Value,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let scope: ployz::drain::DrainScope = from_js(scope).map_err(invalid_argument)?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        let kept = (Arc::clone(&self.store), who.organization);
        to_json(ployz::sdk::drain_machine_kept(connections, expect, kept, (&machine, &scope)).await)
    }

    /// What admits the start of a moved Volume's Service on its new Server
    /// (`VolumeStartQuery`): the signed versions this Store keeps for the spec it starts
    /// there, or the verified build of its image when the rule lists that Server. Only
    /// Cloud's worker calls this.
    ///
    /// # Errors
    /// Returns `invalid_argument` for malformed input, or the Store's error, which is never
    /// read as a spec nothing signed.
    #[napi(ts_return_type = "Promise<VolumeStartProof>")]
    pub async fn volume_start_proof(
        &self,
        organization: String,
        #[napi(ts_arg_type = "VolumeStartQuery")] query: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let query = decode(query, "Expected a Volume start")?;
        to_json(ployz::sdk::volume_start_proof(&self.store, who.organization, query).await)
    }

    /// Claim the Organization's queued Deployment `deployment` as `runner`, and
    /// resolve to its summary. Its upload is the one it builds from now that the
    /// Deployment ahead of it settled: the one Cloud extracts before `runDeployment`
    /// runs it as the same runner. Claiming again as that runner changes nothing
    /// until it runs. Only Cloud's worker calls this.
    ///
    /// # Errors
    /// Returns `conflict` when this runner has nothing to run, or a storage error.
    #[napi(ts_return_type = "Promise<DeploymentSummary>")]
    pub async fn claim_deployment(
        &self,
        organization: String,
        deployment: String,
        runner: String,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let (deployment, runner) = ids(deployment, runner)?;
        // Only a Deployment of this Organization runs here.
        let owned = deployment.clone();
        self.run(move |store| store.read(&who, &ployz_store::DeploymentQuery { id: owned }))
            .await?;
        self.run(move |store| {
            store
                .claim(&deployment, &runner)
                .map(|claimed| claimed.deployment)
        })
        .await
    }

    /// Close, on each Server of the Cluster `connections` (`Connection[]`) reach, every
    /// preview it holds open that the Store doesn't report open for the Organization:
    /// one whose Close never reached its Server, or that a Server holds from before.
    /// Resolves to what it closed, what failed and which Servers didn't answer
    /// (`PreviewsSwept`). Only Cloud's worker calls this. `expect` is as
    /// `runDeployment`'s, and `foreign` as `observeCopies`'.
    ///
    /// # Errors
    /// Returns the connection's error when no Server can be reached, or a storage error.
    #[napi(ts_return_type = "Promise<PreviewsSwept & { foreign: string[] }>")]
    pub async fn close_unheld_previews(
        &self,
        organization: String,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        to_json(
            ployz::sdk::close_unheld_previews(Arc::clone(&self.store), who, connections, expect)
                .await,
        )
    }

    /// Record that `runner` stopped without finishing Deployment `deployment`: its
    /// outcome is unknown once it prepared it. Recording it after an outcome changes
    /// nothing.
    ///
    /// # Errors
    /// Returns `conflict` when another runner owns the Deployment, or a storage error.
    #[napi(ts_return_type = "Promise<DeploymentSummary>")]
    pub async fn abandon_deployment(
        &self,
        deployment: String,
        runner: String,
    ) -> Result<serde_json::Value> {
        let (deployment, runner) = ids(deployment, runner)?;
        self.run(move |store| store.record(&deployment, &runner, RunEvidence::Abandoned))
            .await
    }

    /// Record when the steps Cloud times itself of Deployment `deployment` ran. A step
    /// recorded again takes its new times.
    ///
    /// # Errors
    /// Returns `not_found` for a Deployment that doesn't exist, or a storage error.
    #[napi(ts_return_type = "Promise<null>")]
    pub async fn record_steps(
        &self,
        deployment: String,
        #[napi(ts_arg_type = "DeploymentStep[]")] steps: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let steps: Vec<ployz_store::DeploymentStep> = decode(steps, "Expected Deployment steps")?;
        self.run(move |store| store.record_steps(&deployment, &steps))
            .await
    }

    /// Apply a `SystemEvent` Cloud observed of GitHub for the given Organization;
    /// resolves to what it did (`ConfigWritten`, `automated`). `trusted` carries how
    /// many Servers the Organization has (`ConfigTrusted.servers`) and, for an event
    /// that can deploy, its members (`ConfigTrusted.members`): without them, a Trusted
    /// pull request a person wrote deploys nothing, while a bot's from the repository
    /// itself still does. An approved head, and a push to any Environment but a PR
    /// Environment, deploy without them. Only Cloud's GitHub workers call this.
    ///
    /// # Errors
    /// Returns `conflict` when a branch head's base is stale, or a storage error.
    #[napi(ts_return_type = "Promise<ConfigWritten>")]
    pub async fn system(
        &self,
        organization: String,
        #[napi(ts_arg_type = "SystemEvent")] event: serde_json::Value,
        #[napi(ts_arg_type = "Pick<ConfigTrusted, 'servers' | 'domains' | 'volumes' | 'members'>")]
        trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let event: ployz_store::SystemEvent = decode(event, "Expected a system event")?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.system(&who.organization, &event, &trusted))
            .await
    }

    /// Forget the Organization's configuration once it has no Project
    /// (`OrganizationRemoved`). Only Cloud's own Organization removal calls this.
    ///
    /// # Errors
    /// Returns `conflict` while it has a Project, or a storage error.
    #[napi(ts_return_type = "Promise<OrganizationRemoved>")]
    pub async fn remove_organization(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.remove_organization(&who)).await
    }

    /// The Volumes a Deploy put on the Organization's Servers (`AppliedVolume[]`):
    /// what Forget Servers lists before the user confirms.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<AppliedVolume[]>")]
    pub async fn applied_volumes(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.applied_volumes(&who)).await
    }

    /// Every queued Deployment no runner claimed, admitted before `before` (Unix
    /// seconds), across Organizations (`Unclaimed[]`): what Cloud's sweep dispatches
    /// again.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<Unclaimed[]>")]
    pub async fn unclaimed(&self, before: i64) -> Result<serde_json::Value> {
        self.run(move |store| store.unclaimed(before)).await
    }

    /// Organizations still holding a secret the Store sealed itself in some Working
    /// State or Saved head: those the reseal has work in. Only Cloud's reseal job
    /// calls this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<string[]>")]
    pub async fn legacy_organizations(&self) -> Result<serde_json::Value> {
        self.run(|store| store.legacy_organizations()).await
    }

    /// How many secrets the Store sealed itself the Organization holds
    /// (`LegacyCount`): `current` ones a reseal converts, `history` ones it leaves.
    /// Only Cloud's reseal job calls this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<LegacyCount>")]
    pub async fn legacy_count(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.legacy_count(&who.organization))
            .await
    }

    /// The Organization's Environments with a secret the Store sealed itself in
    /// Working State or the Saved head: one reseal step each. Only Cloud's reseal
    /// job calls this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<string[]>")]
    pub async fn reseal_environments(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.reseal_environments(&who.organization))
            .await
    }

    /// Seal the Environment's secrets the Store sealed itself for the Cluster's key
    /// instead (`Resealed`), in one transaction. Running it again changes nothing.
    /// Only Cloud's reseal job calls this.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Environment, or a storage error.
    #[napi(ts_return_type = "Promise<Resealed>")]
    pub async fn reseal(
        &self,
        environment: String,
        #[napi(ts_arg_type = "SealingRecipient")] recipient: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let environment = ployz_store::EnvironmentId::parse(environment).map_err(rpc_to_napi)?;
        let recipient: ployz_store::SealingRecipient =
            decode(recipient, "Expected a Sealing recipient")?;
        self.run(move |store| store.reseal(&environment, &recipient))
            .await
    }

    /// The head of a GitHub branch the Store last saw for the Organization, or null:
    /// what Cloud compares a new head from. Only Cloud's GitHub workers call this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<string | null>")]
    pub async fn branch_head(
        &self,
        organization: String,
        repository_id: i64,
        branch: String,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let (repository_id, branch) = github_branch(repository_id, &branch)?;
        self.run(move |store| store.branch_head(&who.organization, repository_id, &branch))
            .await
    }

    /// The open pull requests into a GitHub branch whose merge would land config:
    /// `[number]`. Only Cloud's GitHub workers call this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<number[]>")]
    pub async fn awaiting_merge(
        &self,
        organization: String,
        repository_id: i64,
        branch: String,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let (repository_id, branch) = github_branch(repository_id, &branch)?;
        self.run(move |store| store.awaiting_merge(&who.organization, repository_id, &branch))
            .await
    }
}

/// GitHub as a Builder: Cloud dispatches and follows the run; these do the parts
/// that touch the Store's secrets or a Machine.
#[napi]
impl ConfigStore {
    /// Start GitHub build `build` (`DEPLOYMENT.SERVICE`): `{kind: "reused"}`,
    /// `{kind: "dispatch", runner}`, `{kind: "skipped", message}`, or under an automatic
    /// deploy rule `{kind: "follow", workflow, workflow_ref, commit}` for a push, and
    /// `{kind: "approved", workflow, workflow_ref, branch, commit}` for a commit a person
    /// reviewed or approved, which Cloud dispatches. `expect` is as
    /// `runDeployment`'s, here and in `githubCheckIn` and `githubFinish`; `foreign` is
    /// as `observeCopies`', here and in `githubCheckIn`.
    ///
    /// # Errors
    /// Returns `conflict` once the build started or ended.
    #[napi(ts_return_type = "Promise<GithubStart & { foreign: string[] }>")]
    pub async fn github_start(
        &self,
        build: String,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
        runner: String,
    ) -> Result<serde_json::Value> {
        let (build, connections) = (github_id(&build)?, parse_connections(connections)?);
        let expect = parse_expect(expect)?;
        to_json(
            ployz::sdk::github_start(Arc::clone(&self.store), build, connections, expect, runner)
                .await,
        )
    }

    /// Hand a pinned build to GitHub run `run` (`GithubRun`): `refused` once the
    /// Deployment no longer takes it, with the run owed a cancel when Cloud dispatched it
    /// (`GithubRecorded`).
    ///
    /// # Errors
    /// Returns `not_found` for an unknown build.
    #[napi(ts_return_type = "Promise<GithubRecorded>")]
    pub async fn github_dispatched(
        &self,
        build: String,
        #[napi(ts_arg_type = "GithubRun")] run: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        let run = decode(run, "Expected a GitHub run")?;
        self.run(move |store| store.github_dispatched(&build, &run))
            .await
    }

    /// Reserve a pinned build for worker run `runner` before asking GitHub for its run,
    /// so GitHub is asked once (`GithubAsked`). A build pinned before reservations fails
    /// instead, with why, and GitHub is asked for nothing.
    ///
    /// # Errors
    /// Returns `conflict` once it was reserved, handed off or passed on, or the
    /// Deployment no longer takes GitHub runs: ask GitHub for nothing.
    #[napi(ts_return_type = "Promise<GithubAsked>")]
    pub async fn github_ask(&self, build: String, runner: String) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        self.run(move |store| store.github_ask(&build, &runner))
            .await
    }

    /// Where a build handed to GitHub is (`GithubHandOff`).
    ///
    /// # Errors
    /// Returns `not_found` unless GitHub holds it, held it, or passed it on.
    #[napi(ts_return_type = "Promise<GithubHandOff>")]
    pub async fn github_build(&self, build: String) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        self.run(move |store| store.github_build(&build)).await
    }

    /// Skip GitHub for a build it can't take, before any run, for worker run `runner`,
    /// which must own the Deployment's GitHub runs: the next Builder takes it. With `owe`,
    /// a run Cloud dispatched whose record never landed (`GithubRun`) is owed a cancel in
    /// the same write.
    ///
    /// # Errors
    /// Returns `conflict` once the build started or ended, or, with `newer`, when another
    /// attempt owns the Deployment, owing nothing.
    #[napi(ts_return_type = "Promise<GithubSettled>")]
    pub async fn github_skip(
        &self,
        build: String,
        message: String,
        runner: String,
        #[napi(ts_arg_type = "GithubRun | undefined | null")] owe: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        let owe: Option<ployz_store::GithubRun> = owe
            .filter(|owe| !owe.is_null())
            .map(|owe| decode(owe, "Expected a GitHub run"))
            .transpose()?;
        let end = ployz_store::GithubEnd::Skipped { message };
        self.run(move |store| store.github_pass_on(&build, &end, owe.as_ref(), &runner))
            .await
    }

    /// A runner's check-in with its verified OIDC `claims` (`GithubClaims`) and the
    /// public key it holds its Build Grant with, if any: how it pushes and its build
    /// inputs, secrets included. Only the runner may receive them.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a holder that is not 64 lowercase hex,
    /// `unauthenticated` for another repository, workflow or run, `conflict` once it
    /// checked in, is no longer wanted or GitHub skipped it, or the Machine's refusal.
    #[napi(ts_return_type = "Promise<GithubCheckIn & { foreign: string[] }>")]
    pub async fn github_check_in(
        &self,
        build: String,
        #[napi(ts_arg_type = "GithubClaims")] claims: serde_json::Value,
        #[napi(ts_arg_type = "string | null")] holder: Option<String>,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let (build, claims) = (github_id(&build)?, claims_of(claims)?);
        let holder = holder
            .map(|holder| {
                ployz_core::BuildGrantId::parse(holder)
                    .map_err(|_| invalid_argument("Expected a holder of 64 lowercase hex"))
            })
            .transpose()?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        to_json(
            ployz::sdk::github_check_in(
                Arc::clone(&self.store),
                build,
                claims,
                holder,
                connections,
                expect,
            )
            .await,
        )
    }

    /// The GitHub build run `runId` holds while it builds, or null.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<string | null>")]
    pub async fn github_by_run(&self, run_id: i64) -> Result<serde_json::Value> {
        let run_id = u64::try_from(run_id).map_err(|_| invalid_argument("Expected a run id"))?;
        self.run(move |store| {
            store
                .github_by_run(run_id)
                .map(|build| build.map(|build| build.to_string()))
        })
        .await
    }

    /// A rule build's runner checks in on its push's run, with the OIDC `claims` Cloud
    /// verified and the `commit` it checked out: the rule (`rule` for an automatic deploy
    /// rule, `preview` for a preview rule), the Cluster's keys and the one-Service
    /// deployment, Sealed values sealed. Changes nothing.
    ///
    /// # Errors
    /// Returns `not_found` while no build holds the run, `unauthenticated` for another
    /// repository, workflow or run, and `conflict` for another commit, a build that
    /// checked in, or one under no rule.
    #[napi(
        ts_return_type = "Promise<{ build: string; rule?: unknown; preview?: unknown; cluster: string; attest: string; service: string; deployment: unknown; foreign: string[] }>"
    )]
    pub async fn github_rule_check_in(
        &self,
        #[napi(ts_arg_type = "GithubClaims")] claims: serde_json::Value,
        commit: String,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let claims = claims_of(claims)?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        to_json(
            ployz::sdk::github_rule_check_in(
                Arc::clone(&self.store),
                claims,
                commit,
                connections,
                expect,
            )
            .await,
        )
    }

    /// Relay a rule build's grant ask to a Machine, with `reader` for its head read.
    /// Resolves to the Machine's Management Identity in hex.
    ///
    /// # Errors
    /// As `githubRuleCheckIn`, `invalid_argument` for an ask that is not one, and the
    /// Machine's refusal.
    #[napi(ts_return_type = "Promise<{ machine: string; foreign: string[] }>")]
    pub async fn github_rule_grant(
        &self,
        #[napi(ts_arg_type = "GithubClaims")] claims: serde_json::Value,
        #[napi(ts_arg_type = "unknown")] ask: serde_json::Value,
        #[napi(ts_arg_type = "string | null")] reader: Option<String>,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let claims = claims_of(claims)?;
        let ask: ployz::sdk::RuleAsk = decode(ask, "Expected a rule build's grant ask")?;
        let connections = parse_connections(connections)?;
        let expect = parse_expect(expect)?;
        to_json(
            ployz::sdk::github_rule_grant(
                Arc::clone(&self.store),
                claims,
                ask,
                reader.map(ployz_core::GithubToken::new),
                connections,
                expect,
            )
            .await,
        )
    }

    /// A runner's report of its `ployz build --events` lines: `{received, ended}`.
    ///
    /// # Errors
    /// As `githubCheckIn`, `invalid_argument` for a body that is not a report, and
    /// `conflict` before check-in, after the end, or for missing lines.
    #[napi(ts_return_type = "Promise<{ received: number; ended: boolean }>")]
    pub async fn github_report(
        &self,
        build: String,
        #[napi(ts_arg_type = "GithubClaims")] claims: serde_json::Value,
        #[napi(ts_arg_type = "unknown")] report: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (build, claims) = (github_id(&build)?, claims_of(claims)?);
        #[expect(
            clippy::disallowed_methods,
            reason = "whole numbers first, as `from_js`"
        )]
        let reported = ployz::sdk::github_report(
            Arc::clone(&self.store),
            build,
            claims,
            whole_numbers(report),
        )
        .await;
        to_json(reported)
    }

    /// End a build at Cloud's `look`: the start limit, the run past its budget, or the
    /// run ended. `"waiting"` while its Machine can't read the grant, or when the start
    /// limit finds the run checked in, else `{ended: GithubSettled}`, whose `owed` says
    /// the run is owed a cancel. A grant Machine of another Cluster than `expect`'s
    /// fails it at once. Only for run `runId`, the run the caller follows, by worker run
    /// `runner`, which must own the Deployment.
    ///
    /// # Errors
    /// Returns a storage error, `conflict` when it ended meanwhile or another run holds
    /// it, or, with `newer`, when another attempt owns the Deployment.
    #[napi(ts_return_type = "Promise<GithubFinish>")]
    pub async fn github_finish(
        &self,
        build: String,
        #[napi(ts_arg_type = "\"start_limit\" | \"budget\" | \"ended\"")] look: serde_json::Value,
        #[napi(ts_arg_type = "Connections")] connections: serde_json::Value,
        #[napi(ts_arg_type = "ExpectedCluster | null")] expect: Option<serde_json::Value>,
        runner: String,
        run_id: i64,
    ) -> Result<serde_json::Value> {
        let (build, connections) = (github_id(&build)?, parse_connections(connections)?);
        let look = decode(look, "Expected a GitHub look")?;
        let expect = parse_expect(expect)?;
        let run_id = u64::try_from(run_id)
            .map_err(|_| napi::Error::from_reason("Expected a GitHub run id"))?;
        to_json(
            ployz::sdk::github_finish(
                Arc::clone(&self.store),
                build,
                look,
                connections,
                expect,
                runner,
                run_id,
            )
            .await,
        )
    }

    /// Stop Deployment `deployment`'s GitHub builds for worker run `runner`
    /// (`GithubOwed`). Each build a run holds fails as
    /// cancelled in the same write that checks the run's attempt still holds the
    /// Deployment, and the Store records a cancel owed for each run Cloud dispatched.
    /// Resolves to every run still owed one, which Cloud cancels and then confirms.
    ///
    /// # Errors
    /// Returns `conflict` when a newer attempt runs it or it still takes GitHub runs:
    /// cancel nothing.
    #[napi(ts_return_type = "Promise<GithubOwed[]>")]
    pub async fn github_stop(
        &self,
        deployment: String,
        runner: String,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let runner = RunnerId::parse(runner).map_err(rpc_to_napi)?;
        self.run(move |store| store.github_stop(&deployment, &runner))
            .await
    }

    /// Worker run `runner` begins running Deployment `deployment`, before it prepares any
    /// build: the Store numbers it after every run that began before, and it owns the
    /// Deployment's GitHub runs, taking them over from those it outranks. Beginning
    /// again resolves to the same number.
    ///
    /// # Errors
    /// Returns `conflict`, with `newer`, when a later run began or another runner
    /// claimed the Deployment: this run runs nothing.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn github_begin(
        &self,
        deployment: String,
        runner: String,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        self.run(move |store| store.github_begin(&deployment, &runner))
            .await
    }

    /// Every run Deployment `deployment` still owes a cancel (`GithubOwed`): what Cloud
    /// cancels after a cancel of the Deployment owed them.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<GithubOwed[]>")]
    pub async fn github_owed_by(&self, deployment: String) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        self.run(move |store| store.github_owed_by(&deployment))
            .await
    }

    /// GitHub confirmed run `run_id` of `build` cancelled or ended: the Store owes it no
    /// cancel any more.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown build.
    #[napi(ts_return_type = "Promise<null>")]
    pub async fn github_cancel_confirmed(
        &self,
        build: String,
        run_id: i64,
    ) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        let run_id = u64::try_from(run_id)
            .map_err(|_| napi::Error::from_reason("Expected a GitHub run id"))?;
        self.run(move |store| store.github_cancel_confirmed(&build, run_id))
            .await
    }

    /// Every run Cloud owes a cancel, in any Organization, up to `limit`
    /// (`GithubOwed`): what the sweep cancels.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi(ts_return_type = "Promise<GithubOwed[]>")]
    pub async fn github_owed(&self, limit: u32) -> Result<serde_json::Value> {
        self.run(move |store| store.github_owed(limit)).await
    }
}

fn github_id(build: &str) -> Result<GithubBuildId> {
    GithubBuildId::parse(build).map_err(rpc_to_napi)
}

fn claims_of(claims: serde_json::Value) -> Result<ployz_store::GithubClaims> {
    decode(claims, "Expected OIDC claims")
}

fn repository(repository_id: i64) -> Result<u64> {
    u64::try_from(repository_id).map_err(|_| invalid_argument("Expected a GitHub repository ID"))
}

fn decode<T: DeserializeOwned>(value: serde_json::Value, refusal: &str) -> Result<T> {
    from_js(value).map_err(|error| refused(refusal, &error))
}

/// `refusal`, then where and why the input did not fit, never a value it holds.
fn refused(refusal: &str, error: &ployz_core::decode::DecodeError) -> Error {
    invalid_argument(format!("{refusal}: {error}."))
}

fn to_json<T: serde::Serialize>(
    value: std::result::Result<T, RpcError>,
) -> Result<serde_json::Value> {
    serde_json::to_value(value.map_err(rpc_to_napi)?)
        .map_err(|error| Error::from_reason(error.to_string()))
}

impl ConfigStore {
    async fn run<T: serde::Serialize + Send + 'static>(
        &self,
        work: impl FnOnce(&ployz_store::ConfigStore) -> std::result::Result<T, RpcError>
        + Send
        + 'static,
    ) -> Result<serde_json::Value> {
        to_json(ployz::sdk::store_call(&self.store, work).await)
    }
}

fn actor(organization: String, principal: Option<String>) -> Result<Actor> {
    Ok(Actor {
        organization: OrganizationId::parse(organization).map_err(rpc_to_napi)?,
        principal: principal
            .map(ployz_store::Principal::parse)
            .transpose()
            .map_err(rpc_to_napi)?,
    })
}

fn github_branch(
    repository_id: i64,
    branch: &str,
) -> Result<(ployz_store::RepositoryId, ployz_store::BranchName)> {
    Ok((
        ployz_store::RepositoryId::parse(repository(repository_id)?).map_err(rpc_to_napi)?,
        ployz_store::BranchName::parse(branch).map_err(rpc_to_napi)?,
    ))
}

fn ids(deployment: String, runner: String) -> Result<(DeploymentId, RunnerId)> {
    Ok((
        DeploymentId::parse(deployment).map_err(rpc_to_napi)?,
        RunnerId::parse(runner).map_err(rpc_to_napi)?,
    ))
}

/// Evidence Cloud gathered itself (`ConfigTrusted`), or none.
fn evidence(trusted: Option<serde_json::Value>) -> Result<ployz_store::Trusted> {
    Ok(trusted
        .map(|trusted| decode(trusted, "Expected Config Store evidence"))
        .transpose()?
        .unwrap_or_default())
}
