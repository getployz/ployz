//! Volume runs: a Mirror, Sync or Mirror removal of one Volume, which Ployz Cloud
//! runs across its Servers. The CLI starts one, reads them back, and with `--wait`
//! follows one until it ends. Without Cloud there are none: Volumes stay put.

use std::time::Duration;

use clap::{ArgMatches, Command};
use ployz_core::{MachineName, RpcErrorCode};
use ployz_store::{EnvironmentId, VolumeId, VolumeName, VolumeQuery, VolumesQuery};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::super::store::{self, Backend, Next};
use super::super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::ui::{Cell, Fields, Hint, Table, Tone};

const POLL: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(2)
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct VolumeRunId(String);

impl VolumeRunId {
    pub(crate) fn parse(id: &str) -> Result<Self, Error> {
        uuid::Uuid::parse_str(id)
            .map(|id| Self(id.hyphenated().to_string()))
            .map_err(|_| {
                Error::usage("Expected a Volume run ID, as `ployz volume runs VOLUME` lists them")
            })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for VolumeRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VolumeRunKind {
    Mirror,
    Sync,
    DeleteMirror,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VolumeRunState {
    Requested,
    Running,
    Done,
    Failed,
    Cancelled,
    Lost,
    NotStarted,
}

impl VolumeRunState {
    const fn ended(self) -> bool {
        !matches!(self, Self::Requested | Self::Running)
    }

    const fn tone(self) -> Tone {
        match self {
            Self::Requested | Self::Running => Tone::Change,
            Self::Done => Tone::Good,
            Self::Failed | Self::Cancelled | Self::Lost | Self::NotStarted => Tone::Bad,
        }
    }
}

/// What a run was asked for; each kind sets only its own fields.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct VolumeRunArgs {
    /// Mirror: the Server that gets the Mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<MachineName>,
    /// Sync: whether it copies everything again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) full: Option<bool>,
    /// Mirror removal: the Server whose Mirror goes; none for every Mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slot: Option<MachineName>,
    /// Mirror removal: the Volume name typed to confirm losing the last copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) confirmed_name: Option<String>,
}

/// One Volume run as Cloud keeps it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct VolumeRun {
    pub(crate) id: VolumeRunId,
    pub(crate) volume_id: VolumeId,
    pub(crate) volume_name: String,
    pub(crate) kind: VolumeRunKind,
    pub(crate) args: VolumeRunArgs,
    /// Cloud started it for a Volume no config holds any more.
    pub(crate) orphan: bool,
    pub(crate) state: VolumeRunState,
    pub(crate) lease: Option<u64>,
    /// Why it failed or was refused.
    pub(crate) message: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) finished_at: Option<String>,
}

/// What `POST volumes/{id}/runs` asks for.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct RunRequest {
    pub(crate) environment: EnvironmentId,
    pub(crate) kind: VolumeRunKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<MachineName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) full: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slot: Option<MachineName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) confirm: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OneRun {
    pub(crate) run: VolumeRun,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Runs {
    pub(crate) runs: Vec<VolumeRun>,
}

/// Have Cloud start `request` on Volume `volume`.
pub(crate) async fn start(
    credential: &Credential,
    volume: &VolumeId,
    request: &RunRequest,
) -> Result<VolumeRun, StoreCallError> {
    let body = serde_json::to_value(request)
        .map_err(|error| crate::cloud_login::LoginError::Reply(error.to_string()))?;
    let started: OneRun = cloud_account::refusable(
        credential,
        Method::POST,
        &format!("volumes/{volume}/runs"),
        Some(&body),
    )
    .await?;
    Ok(started.run)
}

/// Volume `volume`'s runs in Environment `environment`, newest first.
pub(crate) async fn list(
    credential: &Credential,
    volume: &VolumeId,
    environment: &EnvironmentId,
) -> Result<Vec<VolumeRun>, StoreCallError> {
    let runs: Runs = cloud_account::refusable(
        credential,
        Method::GET,
        &format!("volumes/{volume}/runs?environment={environment}"),
        None,
    )
    .await?;
    Ok(runs.runs)
}

/// Run `id` as Cloud has it now.
pub(crate) async fn get(
    credential: &Credential,
    id: &VolumeRunId,
) -> Result<VolumeRun, StoreCallError> {
    let one: OneRun =
        cloud_account::refusable(credential, Method::GET, &format!("volume-runs/{id}"), None)
            .await?;
    Ok(one.run)
}

/// Follow `run` until it ends, saying each state it moves to. It has no deadline:
/// Cloud ends every run, marking one nobody finished as lost.
pub(crate) async fn settle(
    credential: &Credential,
    mut run: VolumeRun,
) -> Result<VolumeRun, StoreCallError> {
    let mut said = run.state;
    while !run.state.ended() {
        tokio::time::sleep(POLL).await;
        run = get(credential, &run.id).await?;
        if run.state != said && !run.state.ended() {
            crate::ui::note(format_args!(
                "Run {} is {}.",
                run.id,
                store::word(&run.state)
            ));
        }
        said = run.state;
    }
    Ok(run)
}

pub(super) fn wait_flag() -> clap::Arg {
    switch("wait", None).help("Wait for the run to end; it fails unless the run is done")
}

pub(super) fn mirror_command() -> Command {
    store::scoped(
        Command::new("mirror")
            .about("Copy a Volume read-only onto another Server (Ployz Cloud); `mirror rm` deletes one")
            .args_conflicts_with_subcommands(true)
            .subcommand_negates_reqs(true),
    )
    .arg(positional("volume", true))
    .arg(
        value("to", None)
            .value_name("SERVER")
            .required(true)
            .help("The Server that gets the Mirror; it shows as VOLUME-SERVER"),
    )
    .arg(wait_flag())
    .subcommand(
        store::scoped(Command::new("rm").about("Delete a Volume's Mirror (Ployz Cloud)"))
            .arg(
                positional("mirror", true)
                    .value_name("VOLUME-SERVER")
                    .help("The Mirror, named by its Volume and Server, like data-web-2"),
            )
            .arg(
                value("confirm", None)
                    .value_name("VOLUME")
                    .help("The Volume's name, to delete a Mirror that is its only copy left"),
            )
            .arg(wait_flag()),
    )
}

pub(super) fn sync_command() -> Command {
    store::scoped(
        Command::new("sync")
            .about("Send what changed since the last sync to a Volume's Mirror (Ployz Cloud)"),
    )
    .arg(positional("volume", true))
    .arg(switch("full", None).help("Copy everything again, for a Mirror that fell behind for good"))
    .arg(wait_flag())
}

pub(super) fn runs_command() -> Command {
    store::scoped(
        Command::new("runs")
            .about("List a Volume's Mirror and Sync runs, or show one (Ployz Cloud)"),
    )
    .arg(positional("volume", true))
    .arg(positional("run", false).help("A run ID, to show that run"))
}

fn cloud(backend: &Backend) -> Result<(&tokio::runtime::Runtime, &Credential), Error> {
    match backend {
        Backend::Cloud(runtime, credential) => Ok((runtime, credential)),
        Backend::Local(..) => Err(Error::coded(
            RpcErrorCode::Unsupported,
            "Volume runs need Ployz Cloud; without it, Volumes stay on the Server that has them.",
        )),
    }
}

fn located(
    store: &store::Store<'_>,
    matches: &ArgMatches,
    name: &VolumeName,
) -> Result<(VolumeId, EnvironmentId), Error> {
    let view = store.read(&VolumeQuery {
        environment: store::environment(matches)?,
        volume: name.clone(),
    })?;
    Ok((view.volume.volume.id, view.environment.id))
}

pub(super) fn mirror(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = store::volume_name(matches, "volume")?;
    let to = server_name(matches, "to")?;
    let started = format!("Mirror of Volume {volume} onto {to}");
    request(root, &volume, started, |environment| RunRequest {
        environment,
        kind: VolumeRunKind::Mirror,
        to: Some(to.clone()),
        full: None,
        slot: None,
        confirm: None,
    })
}

pub(super) fn sync(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = store::volume_name(matches, "volume")?;
    let full = matches.get_flag("full");
    let started = if full {
        format!("full Sync of Volume {volume}'s Mirror")
    } else {
        format!("Sync of Volume {volume}'s Mirror")
    };
    request(root, &volume, started, |environment| RunRequest {
        environment,
        kind: VolumeRunKind::Sync,
        to: None,
        full: Some(full),
        slot: None,
        confirm: None,
    })
}

pub(super) fn remove_mirror(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let mirror = super::super::required(matches, "mirror")?;
    let confirm = matches.get_one::<String>("confirm").cloned();
    let store = store::store(root)?;
    cloud(store.backend())?;
    let listed = store.read(&VolumesQuery {
        environment: store::environment(matches)?,
    })?;
    let names: Vec<VolumeName> = listed
        .volumes
        .iter()
        .map(|listing| listing.volume.name.clone())
        .collect();
    let (volume, slot) = mirror_of(&names, &mirror)?;
    let started = format!("deletion of Mirror {mirror}");
    request(root, &volume, started, |environment| RunRequest {
        environment,
        kind: VolumeRunKind::DeleteMirror,
        to: None,
        full: None,
        slot: Some(slot.clone()),
        confirm: confirm.clone(),
    })
}

/// Split `mirror`, `VOLUME-SERVER`, at the one Volume of `volumes` it starts with.
pub(crate) fn mirror_of(
    volumes: &[VolumeName],
    mirror: &str,
) -> Result<(VolumeName, MachineName), Error> {
    let mut found = volumes.iter().filter_map(|volume| {
        let server = mirror.strip_prefix(volume.as_str())?.strip_prefix('-')?;
        Some((volume.clone(), MachineName::parse(server).ok()?))
    });
    match (found.next(), found.next()) {
        (Some(only), None) => Ok(only),
        (Some(_), Some(_)) => Err(Error::ambiguous(format!(
            "Mirror {mirror} could belong to more than one Volume here; delete it from the dashboard's Volume page."
        ))),
        (None, _) => Err(Error::not_found(format!(
            "No Volume here has Mirror {mirror}; a Mirror is named VOLUME-SERVER, like data-web-2."
        ))
        .hint(Hint::valid(volumes.iter().map(ToString::to_string)))),
    }
}

fn request(
    root: &ArgMatches,
    volume: &VolumeName,
    started: String,
    ask: impl FnOnce(EnvironmentId) -> RunRequest,
) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store::store(root)?;
    let (runtime, credential) = cloud(store.backend())?;
    let (volume_id, environment) = located(&store, matches, volume)?;
    let asked = ask(environment);
    let confirming = asked.kind == VolumeRunKind::DeleteMirror && asked.confirm.is_none();
    let run = runtime
        .block_on(start(credential, &volume_id, &asked))
        .map_err(|error| refused(&store, matches, error, confirming, volume))?;
    let follow = store::next(
        matches,
        &["volume", "runs", volume.as_str(), run.id.as_str()],
    );
    if !matches.get_flag("wait") {
        return crate::ui::finish(
            &Next::new(&OneRun { run: run.clone() }, Some(follow.clone())),
            || {
                crate::ui::stream(format_args!("Started the {started}: run {}.", run.id));
                crate::ui::hint(&Hint::Next(follow.clone()));
            },
        );
    }
    crate::ui::note(format_args!(
        "Started the {started}: run {}. Waiting for it to end.",
        run.id
    ));
    let run = runtime
        .block_on(settle(credential, run))
        .map_err(|error| store.fail(error))?;
    ended(&run, &started, follow)
}

fn ended(run: &VolumeRun, started: &str, follow: String) -> Result<(), Error> {
    if run.state == VolumeRunState::Done {
        return crate::ui::finish(&OneRun { run: run.clone() }, || {
            crate::ui::stream(format_args!("The {started} is done: run {}.", run.id));
        });
    }
    let why = run.message.as_deref().unwrap_or("Cloud gave no reason");
    let code = match run.state {
        VolumeRunState::Lost | VolumeRunState::NotStarted => RpcErrorCode::Unavailable,
        VolumeRunState::Requested
        | VolumeRunState::Running
        | VolumeRunState::Done
        | VolumeRunState::Failed
        | VolumeRunState::Cancelled => RpcErrorCode::Conflict,
    };
    Err(Error::detailed(
        code,
        format!("The {started} {}: {why}", ended_word(run.state)),
        serde_json::json!({ "run": run }),
    )
    .hint(Hint::Inspect(follow)))
}

const fn ended_word(state: VolumeRunState) -> &'static str {
    match state {
        VolumeRunState::Cancelled => "was cancelled",
        VolumeRunState::Lost => "was lost",
        VolumeRunState::NotStarted => "never started",
        VolumeRunState::Requested
        | VolumeRunState::Running
        | VolumeRunState::Done
        | VolumeRunState::Failed => "failed",
    }
}

fn refused(
    store: &store::Store<'_>,
    matches: &ArgMatches,
    error: StoreCallError,
    confirming: bool,
    volume: &VolumeName,
) -> Error {
    let confirm = matches!(&error, StoreCallError::Refused(refusal) if refusal.code == RpcErrorCode::ConfirmationRequired);
    let failure = store.fail(error);
    if confirming && confirm {
        let mirror = super::super::required(matches, "mirror").unwrap_or_default();
        let retry = store::next(
            matches,
            &[
                "volume",
                "mirror",
                "rm",
                &mirror,
                "--confirm",
                volume.as_str(),
            ],
        );
        return failure.hint(Hint::Retry(retry));
    }
    failure
}

pub(super) fn runs(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = store::volume_name(matches, "volume")?;
    let wanted = matches
        .get_one::<String>("run")
        .map(|id| VolumeRunId::parse(id))
        .transpose()?;
    let store = store::store(root)?;
    let (runtime, credential) = cloud(store.backend())?;
    let (volume_id, environment) = located(&store, matches, &volume)?;
    if let Some(id) = wanted {
        let run = runtime
            .block_on(get(credential, &id))
            .map_err(|error| store.fail(error))?;
        if run.volume_id != volume_id {
            return Err(
                Error::not_found(format!("Volume {volume} has no run {id}.")).hint(Hint::Inspect(
                    store::next(matches, &["volume", "runs", volume.as_str()]),
                )),
            );
        }
        return crate::ui::fields(&OneRun { run: run.clone() }, &record(&run));
    }
    let runs = runtime
        .block_on(list(credential, &volume_id, &environment))
        .map_err(|error| store.fail(error))?;
    let mut table = Table::new(
        ["RUN", "KIND", "STATE", "ASKED", "STARTED", "MESSAGE"],
        format!("Volume {volume} has no runs yet."),
    );
    for run in &runs {
        table.row([
            Cell::from(run.id.to_string()),
            Cell::from(store::word(&run.kind)),
            Cell::status(store::word(&run.state), run.state.tone()),
            Cell::from(asked(&run.args)),
            Cell::from(run.created_at.clone()),
            Cell::from(run.message.clone().unwrap_or_default()),
        ]);
    }
    crate::ui::list(&Runs { runs }, &table)
}

fn record(run: &VolumeRun) -> Fields {
    let mut record = Fields::new()
        .field("run", &run.id)
        .field("volume", &run.volume_name)
        .field("kind", store::word(&run.kind))
        .field("state", store::word(&run.state));
    let asked = asked(&run.args);
    if !asked.is_empty() {
        record.push("asked", asked);
    }
    if run.orphan {
        record.push("started by", "Ployz Cloud, for a Volume no config holds");
    }
    if let Some(message) = &run.message {
        record.push("message", message);
    }
    record.push("started", &run.created_at);
    if let Some(finished) = &run.finished_at {
        record.push("ended", finished);
    }
    record
}

fn asked(args: &VolumeRunArgs) -> String {
    let mut words = Vec::new();
    if let Some(to) = &args.to {
        words.push(format!("to {to}"));
    }
    if args.full == Some(true) {
        words.push("full".to_owned());
    }
    if let Some(slot) = &args.slot {
        words.push(format!("on {slot}"));
    }
    if args.confirmed_name.is_some() {
        words.push("confirmed".to_owned());
    }
    words.join(", ")
}

fn server_name(matches: &ArgMatches, arg: &str) -> Result<MachineName, Error> {
    MachineName::parse(super::super::required(matches, arg)?)
        .map_err(|_| Error::usage("Expected a Server name, as `ployz server ls` shows it"))
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
