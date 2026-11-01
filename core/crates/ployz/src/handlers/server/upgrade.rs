//! Explicit, bounded Machine upgrade sequencing and inspection.

use std::{collections::BTreeSet, time::Duration};

use clap::ArgMatches;
use ployz_core::{
    Machine, MachineId, MachineRelease, MachineTarget, MachineUpgradeAttempt,
    MachineUpgradeAttemptId, MachineUpgradeOutcome, MachineUpgradeStage, MachineVersion,
    RequestMachineUpgradeRequest, RpcErrorCode, op,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    cloud_account::{self, Credential, Settled, StoreCallError},
    cluster::Client,
    connect::ConnectError,
    deploy::Outcome,
    ingress::IngressImage,
    ui::Hint,
};

use serde_json::json;

use super::super::{Error, leaf_matches, runtime, string_values, with_client};
use super::ConnectionOptions;

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(16 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
// The first release whose relay protocol a 0.3.0 CLI and Ployz Cloud speak.
const RELAY_SINCE: &str = "0.3.0-beta.7";
// The daemon restarts itself mid-upgrade; its socket vanishing is expected.
const RESTART: crate::setup_retry::Expected =
    crate::setup_retry::Expected("Waiting for ployzd to restart…");

trait UpgradeRequests {
    async fn request_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        machine: &Machine,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>;

    /// Resend an accepted request to read its attempt, waiting out the daemon restart.
    async fn poll_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        machine: &Machine,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>;

    /// Deploy the Ingress Proxy with `image` onto the Servers holding the ingress role.
    async fn move_ingress(&mut self, image: IngressImage) -> Result<Option<Outcome>, Error>;
}

impl UpgradeRequests for Client {
    async fn request_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        machine: &Machine,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
        self.call_repeatable_for::<op::RequestMachineUpgrade>(
            request,
            Some(&MachineTarget::from(&machine.id)),
            Some(machine.name.as_str()),
            None,
            wait,
        )
        .await
    }

    async fn poll_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        machine: &Machine,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
        self.call_repeatable_for::<op::RequestMachineUpgrade>(
            request,
            Some(&MachineTarget::from(&machine.id)),
            Some(machine.name.as_str()),
            Some(RESTART),
            wait,
        )
        .await
    }

    async fn move_ingress(&mut self, image: IngressImage) -> Result<Option<Outcome>, Error> {
        crate::ingress::follow_roles(self, image).await
    }
}

pub(in crate::handlers) fn upgrade(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let release = matches
        .get_one::<MachineRelease>("version")
        .cloned()
        .ok_or_else(|| Error::usage("upgrade version is required"))?;
    let selectors = string_values(matches, "server");
    let image = matches.get_one::<String>("ingress-image");
    let allow_downgrade = matches.get_flag("allow-downgrade");
    let version = release.to_string();
    let mut args = vec!["server", "upgrade", version.as_str()];
    args.extend(selectors.iter().map(String::as_str));
    args.extend(allow_downgrade.then_some("--allow-downgrade"));
    args.extend(
        image
            .iter()
            .flat_map(|image| ["--ingress-image", image.as_str()]),
    );
    let rerun = super::super::rerun(matches, &args);
    let runtime = runtime()?;
    if let Some(credential) = super::cloud_runs(&runtime, matches)? {
        let direct = direct(root, &args)?;
        return through_cloud(
            &runtime,
            matches,
            &credential,
            &release,
            image.is_some(),
            &selectors,
            direct,
        );
    }
    drop(runtime);
    let ingress = IngressImage::given_or(image.cloned(), IngressImage::Latest);
    let recovery_matches = matches.clone();
    with_client(root, |client| {
        Box::pin(async move {
            let machines = selected_machines(client, &selectors).await?;
            confirm_downgrades(&machines, &release, allow_downgrade)?;
            let cancellation = crate::cancellation::on_ctrl_c();
            let (result, outcome) =
                run_all(client, &machines, release, ingress, rerun, &cancellation).await;
            let outcome = outcome.map_err(|error| {
                super::super::ingress_hints(error, |args| {
                    super::super::rerun(&recovery_matches, args)
                })
            });
            match result {
                Some(result) => crate::ui::emit_committed(result, outcome),
                None => outcome,
            }
        })
    })
}

#[derive(Deserialize, Serialize)]
struct CloudUpgrade {
    outcome: String,
    from_version: Option<String>,
    target_version: Option<String>,
    message: Option<String>,
}

fn direct(root: &ArgMatches, args: &[&str]) -> Result<Option<Hint>, Error> {
    let config = ConnectionOptions::from_matches(root)?.load_or_empty_config()?;
    Ok(config.current_context().map(|context| {
        let mut args = args.to_vec();
        args.extend(["--context", context]);
        Hint::Retry(super::super::rerun(leaf_matches(root), &args))
    }))
}

fn through_cloud(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    release: &MachineRelease,
    pinned_ingress: bool,
    selectors: &[String],
    direct: Option<Hint>,
) -> Result<(), Error> {
    let only_direct = |message: String| {
        let error = Error::usage(message);
        match direct.clone() {
            Some(hint) => error.hint(hint),
            None => error,
        }
    };
    let channel = match release {
        MachineRelease::Stable | MachineRelease::Beta => release.to_string(),
        MachineRelease::Exact(version) => {
            return Err(only_direct(format!(
                "Cloud upgrades along your Organization's Release Channel, not to {version}; \
                 pass --context to upgrade to an exact version. No changes made."
            )));
        }
    };
    if pinned_ingress {
        return Err(only_direct(
            "Cloud does not pin the Ingress Proxy image; pass --context for --ingress-image. \
             No changes made."
                .into(),
        ));
    }
    // Ids route the runs, and only the Engine knows the names prose says.
    let servers = runtime.block_on(async {
        let mut client = super::connect(matches, None).await?;
        selected_machines(&mut client, selectors).await
    })?;
    let unattempted = |from: usize| servers.get(from..).unwrap_or_default();
    let mut attempts = Vec::new();
    let mut skipped = Vec::new();
    for (index, (machine, selector)) in servers.iter().zip(selectors).enumerate() {
        let (id, name) = (machine.id, machine.name.as_str());
        let started = runtime.block_on(cloud_account::start_run(
            credential,
            Method::POST,
            &format!("servers/{id}/upgrade"),
            &json!({ "channel": channel, "server_name": name }),
            None,
        ));
        let run = match started {
            Ok(run) => run,
            Err(StoreCallError::Refused(refused))
                if refused.code.as_str() == "channel_mismatch" =>
            {
                let along = refused
                    .details
                    .get("channel")
                    .and_then(serde_json::Value::as_str)
                    .map(|channel| {
                        let mut args = vec!["server", "upgrade", channel];
                        args.extend(selectors.iter().map(String::as_str));
                        Hint::Retry(super::super::rerun(matches, &args))
                    });
                let error = match along {
                    Some(hint) => Error::from(refused).hint(hint),
                    None => refused.into(),
                };
                return cut_short(attempts, unattempted(index), &skipped, name, error);
            }
            Err(StoreCallError::Refused(refused)) if refused.code == RpcErrorCode::Unavailable => {
                skipped.push(skip(machine, selector, refused.message));
                continue;
            }
            Err(error) => {
                return cut_short(attempts, unattempted(index), &skipped, name, error.into());
            }
        };
        crate::ui::stream(format_args!("Upgrading Server {name}."));
        let followed = runtime.block_on(cloud_account::follow_run::<Settled<CloudUpgrade>>(
            credential,
            &format!("server-upgrades/{run}"),
            &format!("Upgrading Server {name}"),
            None,
        ));
        let settled = match followed {
            Ok(settled) => settled.expect("a run followed without a deadline settles or fails"),
            Err(error) => {
                return cut_short(
                    attempts,
                    unattempted(index + 1),
                    &skipped,
                    name,
                    error.into(),
                );
            }
        };
        let finished = match settled {
            Settled::Ended { code, message } if code == "busy" => {
                skipped.push(skip(machine, selector, message));
                continue;
            }
            settled @ (Settled::Finished(_) | Settled::Ended { .. }) => settled.finished(),
        };
        let upgrade = match finished {
            Ok(upgrade) => upgrade,
            Err(error) => {
                return cut_short(attempts, unattempted(index + 1), &skipped, name, error);
            }
        };
        let stopped = stopped(name, &upgrade).map(|stopped| {
            // A Server that failed outright, with none moved before it, fails the run;
            // it is partial only after an upgrade, and uncertain while Cloud can't tell.
            let upgraded = attempts.iter().any(|attempt| {
                attempt["outcome"] == "succeeded"
                    && attempt["from_version"] != attempt["target_version"]
            });
            if matches!(upgrade.outcome.as_str(), "failed" | "interrupted") && !upgraded {
                stopped.outcome()
            } else {
                stopped
            }
        });
        attempts.push(json!({
            "server": id,
            "id": run,
            "outcome": upgrade.outcome,
            "from_version": upgrade.from_version,
            "target_version": upgrade.target_version,
            "message": upgrade.message,
        }));
        if let Some(stopped) = stopped {
            return cut_short(attempts, unattempted(index + 1), &skipped, name, stopped);
        }
    }
    let result =
        json!({ "attempts": attempts, "unattempted": [], "skipped": skipped_json(&skipped) });
    let names = skipped
        .iter()
        .map(|(machine, _, _)| machine.name.as_str())
        .collect::<Vec<_>>();
    let message = match names.as_slice() {
        [] => return crate::ui::emit(&result),
        [one] => format!("Skipped Server {one}; it took no Upgrade."),
        many => format!("Skipped Servers {}; they took no Upgrade.", many.join(", ")),
    };
    let mut args = vec!["server", "upgrade", channel.as_str()];
    args.extend(skipped.iter().map(|(_, selector, _)| *selector));
    let skipped = Error::coded(RpcErrorCode::Unavailable, message)
        .hint(Hint::Retry(super::super::rerun(matches, &args)));
    crate::ui::emit_committed(result, Err(skipped))
}

/// End the run on `error`, which Server `after` raised. Upgrades already attempted are
/// committed, so they print with the error rather than vanish behind it.
fn cut_short(
    attempts: Vec<serde_json::Value>,
    unattempted: &[Machine],
    skipped: &[Skipped<'_>],
    after: &str,
    error: Error,
) -> Result<(), Error> {
    if attempts.is_empty() {
        return Err(error);
    }
    if let Some(warning) = unattempted_warning(names(unattempted), after) {
        crate::ui::warn(warning);
    }
    let result = json!({
        "attempts": attempts,
        "unattempted": unattempted.iter().map(|machine| machine.id).collect::<Vec<_>>(),
        "skipped": skipped_json(skipped),
    });
    crate::ui::emit_committed(result, Err(error))
}

/// A Server that took no Upgrade, the selector that chose it, and why.
type Skipped<'a> = (&'a Machine, &'a str, String);

fn skip<'a>(machine: &'a Machine, selector: &'a str, reason: String) -> Skipped<'a> {
    crate::ui::warn(format!("Skipped Server {}: {reason}", machine.name));
    (machine, selector, reason)
}

fn skipped_json(skipped: &[Skipped<'_>]) -> serde_json::Value {
    skipped
        .iter()
        .map(|(machine, _, reason)| json!({ "server": machine.id, "reason": reason }))
        .collect()
}

fn stopped(server: &str, upgrade: &CloudUpgrade) -> Option<Error> {
    let target = upgrade
        .target_version
        .as_deref()
        .unwrap_or("its target version");
    let reason = upgrade
        .message
        .as_deref()
        .map(|message| format!(": {message}"))
        .unwrap_or_default();
    let message = match upgrade.outcome.as_str() {
        "succeeded" if upgrade.from_version == upgrade.target_version => {
            crate::ui::stream(format_args!("Server {server} already runs {target}."));
            return None;
        }
        "succeeded" => {
            crate::ui::stream(format_args!("Upgraded Server {server} to {target}."));
            return None;
        }
        "failed" => {
            crate::ui::stream(format_args!(
                "Server {server} failed to upgrade to {target}."
            ));
            format!("Server {server} failed to upgrade to {target}{reason}")
        }
        "interrupted" => {
            crate::ui::stream(format_args!(
                "Server {server} stopped upgrading to {target}."
            ));
            format!("The upgrade of Server {server} to {target} was interrupted{reason}")
        }
        _ => {
            crate::ui::stream(format_args!(
                "Cloud cannot tell whether Server {server} upgraded to {target}."
            ));
            format!("Cloud cannot tell whether Server {server} upgraded to {target}{reason}")
        }
    };
    Some(Error::coded(RpcErrorCode::Internal, message))
}

/// An upgrade attempt and the Server it ran on, as `--json` prints it.
#[derive(Serialize)]
struct Attempted {
    server: MachineId,
    from_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<UpgradeChange>,
    #[serde(flatten)]
    attempt: MachineUpgradeAttempt,
}

impl Attempted {
    fn new(machine: &Machine, attempt: MachineUpgradeAttempt) -> Self {
        let change = if let MachineUpgradeOutcome::Succeeded { version } = &attempt.outcome {
            UpgradeChange::between(&machine.runtime.daemon_version, version)
        } else {
            None
        };
        Self {
            server: machine.id,
            from_version: machine.runtime.daemon_version.clone(),
            change,
            attempt,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum UpgradeChange {
    Unchanged,
    Downgraded,
    Upgraded,
}

impl UpgradeChange {
    fn between(from: &str, to: &MachineVersion) -> Option<Self> {
        let from = MachineVersion::parse(from).ok()?;
        Some(match from.cmp(to) {
            std::cmp::Ordering::Equal => Self::Unchanged,
            std::cmp::Ordering::Greater => Self::Downgraded,
            std::cmp::Ordering::Less => Self::Upgraded,
        })
    }
}

/// Upgrade each Server's daemon in order, stopping at the first failure; once all succeed,
/// move the Ingress Proxy when any of them holds the ingress role. The result is `None`
/// when nothing was committed.
async fn run_all(
    client: &mut impl UpgradeRequests,
    machines: &[Machine],
    release: MachineRelease,
    ingress: IngressImage,
    rerun: String,
    cancellation: &CancellationToken,
) -> (Option<serde_json::Value>, Result<(), Error>) {
    let mut attempts = Vec::new();
    // Whether a Server moved to another version: only that makes a later failure partial.
    let mut upgraded = false;
    for (index, machine) in machines.iter().enumerate() {
        let attempt_id = MachineUpgradeAttemptId::random();
        let mut seen = None;
        let ran = run_one(
            client,
            machine,
            release.clone(),
            attempt_id,
            &mut seen,
            cancellation,
        )
        .await;
        // Whether the Server answered that this upgrade failed, rather than going unheard.
        let (stopped, definitive) = match ran {
            Err(error) => {
                // An accepted attempt is committed evidence: keep it in the result.
                attempts.extend(seen.map(|attempt| Attempted::new(machine, attempt)));
                (error, false)
            }
            Ok(attempt) => {
                if let MachineUpgradeOutcome::Succeeded { version } = &attempt.outcome {
                    upgraded |= version.to_string() != machine.runtime.daemon_version;
                }
                print_attempt(machine, &attempt);
                let stopped = match &attempt.outcome {
                    MachineUpgradeOutcome::Succeeded { .. } => None,
                    MachineUpgradeOutcome::Failed {
                        stage: MachineUpgradeStage::Acquiring,
                        error: reason,
                    } if reason
                        == &format!("release selection: no such release {}", attempt.target)
                        || reason
                            == &format!(
                                "release selection: download release checksums: release {} has no checksums.txt",
                                attempt.target
                            ) =>
                    {
                        Some(Error::not_found(format!(
                            "No Ployz release {}.",
                            attempt.target
                        )))
                    }
                    MachineUpgradeOutcome::Failed {
                        stage,
                        error: reason,
                    } => Some(
                        Error::coded(
                            RpcErrorCode::Internal,
                            format!(
                                "Server {} failed to upgrade to {} while {}: {reason}",
                                machine.name,
                                attempt.target,
                                stage.as_str()
                            ),
                        )
                        .hint(journal_hint(attempt_id)),
                    ),
                    MachineUpgradeOutcome::Interrupted { stage } => Some(
                        Error::coded(
                            RpcErrorCode::Internal,
                            format!(
                                "The upgrade of Server {} to {} was interrupted while {}.",
                                machine.name,
                                attempt.target,
                                stage.as_str()
                            ),
                        )
                        .hint(journal_hint(attempt_id)),
                    ),
                    MachineUpgradeOutcome::Accepted | MachineUpgradeOutcome::Running { .. } => {
                        unreachable!("run_one returns only terminal evidence")
                    }
                };
                attempts.push(Attempted::new(machine, attempt));
                match stopped {
                    Some(error) => (error, true),
                    None => continue,
                }
            }
        };
        let unattempted = machines.get(index + 1..).unwrap_or_default();
        if let Some(warning) = unattempted_warning(names(unattempted), machine.name.as_str()) {
            crate::ui::warn(warning);
        }
        // A run where no Server upgraded and one failed outright fails; it is partial after
        // an upgrade, and while a Server's result is unknown.
        let stopped = if definitive && !upgraded {
            stopped.outcome()
        } else {
            stopped
        };
        // A recorded attempt is a result: print it, then exit by `stopped`.
        let result = (!attempts.is_empty()).then(|| {
            json!({
                "attempts": attempts,
                "unattempted": unattempted.iter().map(|machine| machine.id).collect::<Vec<_>>(),
            })
        });
        return (result, Err(stopped));
    }
    // ponytail: the Ingress Proxy is one Cluster-wide Global, so it moves once, after every
    // selected daemon, and also on ingress Servers that were not selected.
    let moved = if machines.iter().any(|machine| machine.accepts_ingress) {
        tokio::select! {
            moved = client.move_ingress(ingress) => moved,
            // Its rerun hint finishes the move.
            () = cancellation.cancelled() => Err(Error::cancelled()),
        }
    } else {
        Ok(None)
    };
    let result = json!({
        "attempts": attempts,
        "unattempted": [],
        "ingress": moved.as_ref().ok().and_then(Option::as_ref),
    });
    let outcome = moved
        .map(drop)
        .map_err(|error| super::ingress_incomplete("Servers upgraded", error, rerun));
    (Some(result), outcome)
}

/// Ask before an exact `release` moves any of `machines` to an older version than it runs.
/// Channels never move a Server back, and a version Ployz never published can't be compared.
fn confirm_downgrades(
    machines: &[Machine],
    release: &MachineRelease,
    allowed: bool,
) -> Result<(), Error> {
    let MachineRelease::Exact(target) = release else {
        return Ok(());
    };
    let newer = machines
        .iter()
        .filter_map(|machine| {
            let running = MachineVersion::parse(&machine.runtime.daemon_version).ok()?;
            (running > *target).then_some((machine, running))
        })
        .collect::<Vec<_>>();
    if newer.is_empty() || allowed {
        return Ok(());
    }
    let servers = newer
        .iter()
        .map(|(machine, running)| format!("Server {} from {running}", machine.name))
        .collect::<Vec<_>>()
        .join(", ");
    let relay = MachineVersion::parse(RELAY_SINCE).expect("RELAY_SINCE is a release version");
    let breaks = if *target < relay {
        "A newer CLI and Ployz Cloud can't reach a Server on it through the relay, so \
         upgrading it again may take SSH."
    } else {
        "A newer CLI or Ployz Cloud may need what the older release lacks."
    };
    if crate::ui::can_prompt() {
        crate::ui::note(breaks);
    }
    crate::ui::confirm(
        &format!("Downgrade {servers} to {target}?"),
        || {
            Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "Upgrading to {target} would downgrade {servers}. {breaks} Pass \
                     --allow-downgrade to downgrade anyway. Nothing upgraded."
                ),
                json!({
                    "target": target,
                    "downgrade": newer
                        .iter()
                        .map(|(machine, running)| json!({ "server": machine.id, "version": running }))
                        .collect::<Vec<_>>(),
                }),
            )
            .hint(Hint::Retry(super::super::typed_with(&["--allow-downgrade"])))
        },
        "Cancelled. Nothing upgraded.",
    )
}

async fn selected_machines(
    client: &mut Client,
    selectors: &[String],
) -> Result<Vec<Machine>, Error> {
    let visible = client.machines().await?;
    let mut selected = Vec::with_capacity(selectors.len());
    let mut ids = BTreeSet::new();
    for selector in selectors {
        let machine = super::remove::select_machine(&visible, selector)?;
        if !ids.insert(machine.id) {
            return Err(Error::usage(format!(
                "Server {} was selected more than once",
                machine.name
            )));
        }
        selected.push(machine);
    }
    Ok(selected)
}

async fn run_one(
    client: &mut impl UpgradeRequests,
    machine: &Machine,
    release: MachineRelease,
    attempt_id: MachineUpgradeAttemptId,
    // The latest observed evidence, kept when a later poll fails.
    seen: &mut Option<MachineUpgradeAttempt>,
    cancellation: &CancellationToken,
) -> Result<MachineUpgradeAttempt, Error> {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    let request = RequestMachineUpgradeRequest {
        attempt_id,
        release,
    };
    // A Server that cannot take the request within a minute is down, not busy.
    let requested = tokio::select! {
        requested = client.request_upgrade(request.clone(), machine, crate::setup_retry::WAIT) => requested,
        () = cancellation.cancelled() => return Err(stopped_following(machine, attempt_id, false)),
    };
    let accepted = match requested {
        Ok(accepted) => accepted,
        Err(crate::setup_retry::Error::Permanent(error)) => return Err(error.into()),
        Err(exhausted) => return Err(uncertain(machine, attempt_id, exhausted)),
    };
    *seen = Some(accepted.clone());
    if accepted.is_terminal() {
        return Ok(accepted);
    }
    print_attempt(machine, &accepted);

    loop {
        let waited = tokio::select! {
            waited = tokio::time::timeout_at(deadline, tokio::time::sleep(POLL_INTERVAL)) => waited,
            () = cancellation.cancelled() => return Err(stopped_following(machine, attempt_id, true)),
        };
        if waited.is_err() {
            return Err(uncertain_timeout(machine, attempt_id));
        }
        // A restarting daemon answers within its start window; a Server silent longer has gone away.
        let left = deadline.saturating_duration_since(Instant::now());
        let wait = ployz_core::MACHINE_START_WAIT.min(left);
        let polled = tokio::select! {
            polled = client.poll_upgrade(request.clone(), machine, wait) => polled,
            () = cancellation.cancelled() => return Err(stopped_following(machine, attempt_id, true)),
        };
        let observed = polled.map_err(|error| match error {
            crate::setup_retry::Error::Permanent(error) => error.into(),
            // The observation deadline cut this poll short, not the Server's silence.
            _ if wait < ployz_core::MACHINE_START_WAIT => uncertain_timeout(machine, attempt_id),
            exhausted => went_away(machine, attempt_id, exhausted),
        })?;
        if observed.is_terminal() {
            return Ok(observed);
        }
        *seen = Some(observed);
    }
}

/// Ctrl-C stops following an upgrade, never the upgrade: the Server runs it on its own.
fn stopped_following(
    machine: &Machine,
    attempt_id: MachineUpgradeAttemptId,
    accepted: bool,
) -> Error {
    let name = &machine.name;
    let message = if accepted {
        format!(
            "Stopped following upgrade {attempt_id}; Server {name} goes on with it. See how it ended with `ployz server show {name}`."
        )
    } else {
        format!(
            "Cancelled before Server {name} answered; it may still have started upgrade {attempt_id}. Check with `ployz server show {name}`."
        )
    };
    Error::coded(RpcErrorCode::Internal, message)
        .hint(journal_hint(attempt_id))
        .interrupted()
}

fn print_attempt(machine: &Machine, attempt: &MachineUpgradeAttempt) {
    let name = &machine.name;
    let target = &attempt.target;
    match &attempt.outcome {
        MachineUpgradeOutcome::Accepted => {
            crate::ui::stream(format_args!("Upgrading Server {name} to {target}."));
        }
        MachineUpgradeOutcome::Running { stage } => crate::ui::stream(format_args!(
            "Upgrading Server {name} to {target}: {}.",
            stage.as_str()
        )),
        MachineUpgradeOutcome::Succeeded { version } => {
            match UpgradeChange::between(&machine.runtime.daemon_version, version) {
                Some(UpgradeChange::Unchanged) => {
                    crate::ui::stream(format_args!("Server {name} already runs {version}."))
                }
                Some(UpgradeChange::Downgraded) => crate::ui::stream(format_args!(
                    "Downgraded Server {name} from {} to {version}.",
                    machine.runtime.daemon_version
                )),
                Some(UpgradeChange::Upgraded) => {
                    crate::ui::stream(format_args!("Upgraded Server {name} to {version}."))
                }
                None => crate::ui::stream(format_args!("Server {name} now runs {version}.")),
            }
        }
        MachineUpgradeOutcome::Failed { stage, .. } => crate::ui::stream(format_args!(
            "Server {name} failed to upgrade to {target} while {}.",
            stage.as_str()
        )),
        MachineUpgradeOutcome::Interrupted { stage } => crate::ui::stream(format_args!(
            "Server {name} stopped upgrading to {target} while {}.",
            stage.as_str()
        )),
    }
}

fn unattempted_warning(names: impl IntoIterator<Item = String>, after: &str) -> Option<String> {
    match names.into_iter().collect::<Vec<_>>().as_slice() {
        [] => None,
        [one] => Some(format!(
            "Did not upgrade Server {one}: Server {after} stopped the run."
        )),
        many => Some(format!(
            "Did not upgrade Servers {}: Server {after} stopped the run.",
            many.join(", ")
        )),
    }
}

fn names(machines: &[Machine]) -> impl Iterator<Item = String> {
    machines.iter().map(|machine| machine.name.to_string())
}

/// The unit that ran the upgrade, read on the Server itself.
fn journal_hint(attempt_id: MachineUpgradeAttemptId) -> Hint {
    Hint::Inspect(format!("journalctl -u ployz-upgrade-{attempt_id}.service"))
}

fn uncertain(
    machine: &Machine,
    attempt_id: MachineUpgradeAttemptId,
    error: crate::setup_retry::Error<ConnectError>,
) -> Error {
    Error::caused(
        ployz_core::RpcErrorCode::Unavailable,
        format!(
            "Cannot tell whether Server {} finished upgrade {attempt_id}. Reconnect and compare it with the upgrade attempt `ployz server show {}` shows.",
            machine.name, machine.name,
        ),
        error,
    )
    .hint(journal_hint(attempt_id))
}

fn went_away(
    machine: &Machine,
    attempt_id: MachineUpgradeAttemptId,
    error: crate::setup_retry::Error<ConnectError>,
) -> Error {
    let name = &machine.name;
    Error::caused(
        ployz_core::RpcErrorCode::Unavailable,
        format!(
            "Server {name} stopped answering during upgrade {attempt_id} and didn't answer again within {} minutes. It may have powered off, lost its network or still be restarting. Once it answers, `ployz server show {name}` shows how the upgrade ended.",
            ployz_core::MACHINE_START_WAIT.as_secs() / 60,
        ),
        error,
    )
    .hint(journal_hint(attempt_id))
}

fn uncertain_timeout(machine: &Machine, attempt_id: MachineUpgradeAttemptId) -> Error {
    Error::unavailable(format!(
        "Cannot tell whether Server {} finished upgrade {attempt_id} after {} minutes. Reconnect and compare it with the upgrade attempt `ployz server show {}` shows.",
        machine.name,
        OBSERVATION_TIMEOUT.as_secs() / 60,
        machine.name,
    ))
    .hint(journal_hint(attempt_id))
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        AdvertisedEndpoint, MachineId, MachineName, MachineRuntime, RpcError, RpcErrorCode,
        WireGuardPublicKey,
    };
    use serde_json::Value;

    use super::*;

    struct FakeRequests {
        request: Option<Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>>,
        seen: Vec<(MachineUpgradeAttemptId, String)>,
    }

    impl UpgradeRequests for FakeRequests {
        async fn request_upgrade(
            &mut self,
            request: RequestMachineUpgradeRequest,
            machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            self.seen.push((
                request.attempt_id,
                MachineTarget::from(&machine.id).as_str().to_owned(),
            ));
            self.request.take().expect("one request result")
        }

        async fn poll_upgrade(
            &mut self,
            _request: RequestMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            panic!("an unaccepted request is not polled")
        }

        async fn move_ingress(&mut self, _image: IngressImage) -> Result<Option<Outcome>, Error> {
            panic!("a single-Server run does not move the Ingress Proxy")
        }
    }

    /// Answers each upgrade request with a terminal outcome, in order, and records Ingress moves.
    struct Sequence {
        outcomes: std::collections::VecDeque<MachineUpgradeOutcome>,
        ingress: Option<Error>,
        moved: Vec<IngressImage>,
    }

    impl UpgradeRequests for Sequence {
        async fn request_upgrade(
            &mut self,
            request: RequestMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            Ok(MachineUpgradeAttempt {
                attempt_id: request.attempt_id,
                target: match request.release {
                    MachineRelease::Exact(version) => version,
                    MachineRelease::Stable | MachineRelease::Beta => {
                        MachineVersion::parse("1.2.3").unwrap()
                    }
                },
                outcome: self.outcomes.pop_front().expect("one outcome per request"),
            })
        }

        async fn poll_upgrade(
            &mut self,
            _request: RequestMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            Err(crate::setup_retry::Error::Permanent(ConnectError::Remote(
                RpcError {
                    code: RpcErrorCode::Internal,
                    message: "upgrade record is unreadable".into(),
                    details: Value::Null,
                    cause: Vec::new(),
                },
            )))
        }

        async fn move_ingress(&mut self, image: IngressImage) -> Result<Option<Outcome>, Error> {
            self.moved.push(image);
            self.ingress.take().map_or(Ok(None), Err)
        }
    }

    fn succeeded() -> MachineUpgradeOutcome {
        MachineUpgradeOutcome::Succeeded {
            version: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
        }
    }

    async fn run_sequence(
        machines: &[Machine],
        outcomes: Vec<MachineUpgradeOutcome>,
        ingress: Option<Error>,
    ) -> (Option<Value>, Result<(), Error>, Vec<IngressImage>) {
        let mut client = Sequence {
            outcomes: outcomes.into(),
            ingress,
            moved: Vec::new(),
        };
        let (result, outcome) = run_all(
            &mut client,
            machines,
            MachineRelease::parse("1.2.3").unwrap(),
            IngressImage::Latest,
            "ployz server upgrade 1.2.3 a b".into(),
            &CancellationToken::new(),
        )
        .await;
        (result, outcome, client.moved)
    }

    #[tokio::test]
    async fn upgrade_reports_the_observed_version_and_direction() {
        for (running, change) in [
            ("1.2.3", "unchanged"),
            ("1.2.4", "downgraded"),
            ("1.2.2", "upgraded"),
        ] {
            let mut server = machine('a', 1);
            server.name = MachineName::parse("machine-1").unwrap();
            server.runtime.daemon_version = running.into();
            let (result, outcome, _) = run_sequence(&[server], vec![succeeded()], None).await;
            outcome.unwrap();
            let result = result.unwrap();
            assert_eq!(
                result.pointer("/attempts/0/from_version"),
                Some(&json!(running)),
                "{result}"
            );
            assert_eq!(
                result.pointer("/attempts/0/change"),
                Some(&json!(change)),
                "{result}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_release_is_not_found_before_any_server_changes() {
        for reason in [
            "release selection: no such release 9.9.9",
            "release selection: download release checksums: release 9.9.9 has no checksums.txt",
        ] {
            let mut server = machine('a', 1);
            server.name = MachineName::parse("machine-1").unwrap();
            let mut client = Sequence {
                outcomes: vec![MachineUpgradeOutcome::Failed {
                    stage: ployz_core::MachineUpgradeStage::Acquiring,
                    error: reason.into(),
                }]
                .into(),
                ingress: None,
                moved: Vec::new(),
            };
            let (_, outcome) = run_all(
                &mut client,
                &[server],
                MachineRelease::parse("9.9.9").unwrap(),
                IngressImage::Latest,
                "ployz server upgrade 9.9.9 machine-1".into(),
                &CancellationToken::new(),
            )
            .await;
            let error = outcome.unwrap_err();
            assert!(error.is_outcome());
            assert_eq!(error.report().code, RpcErrorCode::NotFound);
            assert_eq!(error.report().message, "No Ployz release 9.9.9.");
        }
    }

    #[tokio::test]
    async fn a_lone_failed_upgrade_exits_failed_not_partial() {
        // #2888: one Server that failed to upgrade exited 3, as if part of the run worked.
        let failed = || MachineUpgradeOutcome::Failed {
            stage: ployz_core::MachineUpgradeStage::Acquiring,
            error: "release selection: no such release 0.3.0-beta.99".into(),
        };
        let (_, outcome, _) = run_sequence(&[machine('a', 1)], vec![failed()], None).await;
        assert!(outcome.unwrap_err().is_outcome());
        let (_, outcome, _) = run_sequence(
            &[machine('a', 1), machine('b', 2)],
            vec![succeeded(), failed()],
            None,
        )
        .await;
        assert!(!outcome.unwrap_err().is_outcome(), "one upgraded: partial");
        // A Server already on the target moved nowhere, so a later failure is not partial.
        let mut current = machine('a', 1);
        current.runtime.daemon_version = "1.2.3".into();
        let (_, outcome, _) = run_sequence(
            &[current, machine('b', 2)],
            vec![succeeded(), failed()],
            None,
        )
        .await;
        assert!(outcome.unwrap_err().is_outcome(), "none upgraded: failed");
    }

    #[tokio::test]
    async fn an_unanswered_upgrade_stays_partial() {
        // The Server may still finish it, so the run didn't fail outright.
        let mut client = FakeRequests {
            request: Some(Err(crate::setup_retry::Error::Exhausted {
                operation: "RequestUpgrade".into(),
                wait: crate::setup_retry::WAIT,
                last: Some(ConnectError::EntryNotReady),
                in_flight: true,
            })),
            seen: Vec::new(),
        };
        let (_, outcome) = run_all(
            &mut client,
            &[machine('a', 1)],
            MachineRelease::Stable,
            IngressImage::Latest,
            "ployz server upgrade stable a".into(),
            &CancellationToken::new(),
        )
        .await;
        assert!(!outcome.unwrap_err().is_outcome());
    }

    /// Accepts the upgrade, then never answers again, as a Server that lost power.
    struct GoneAfterAccepting {
        /// Polls answered with a restart in progress before the Server goes silent.
        answers: usize,
        waits: Vec<Duration>,
    }

    impl UpgradeRequests for GoneAfterAccepting {
        async fn request_upgrade(
            &mut self,
            request: RequestMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            Ok(MachineUpgradeAttempt {
                attempt_id: request.attempt_id,
                target: MachineVersion::parse("1.2.3").unwrap(),
                outcome: MachineUpgradeOutcome::Accepted,
            })
        }

        async fn poll_upgrade(
            &mut self,
            request: RequestMachineUpgradeRequest,
            _machine: &Machine,
            wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            self.waits.push(wait);
            if let Some(left) = self.answers.checked_sub(1) {
                self.answers = left;
                return Ok(MachineUpgradeAttempt {
                    attempt_id: request.attempt_id,
                    target: MachineVersion::parse("1.2.3").unwrap(),
                    outcome: MachineUpgradeOutcome::Running {
                        stage: ployz_core::MachineUpgradeStage::Restarting,
                    },
                });
            }
            Err(crate::setup_retry::Error::Exhausted {
                operation: "RequestUpgrade".into(),
                wait,
                last: Some(ConnectError::EntryNotReady),
                in_flight: true,
            })
        }

        async fn move_ingress(&mut self, _image: IngressImage) -> Result<Option<Outcome>, Error> {
            panic!("an unfinished upgrade does not move the Ingress Proxy")
        }
    }

    #[tokio::test]
    async fn a_server_gone_mid_upgrade_is_given_up_on_after_its_start_window() {
        let mut client = GoneAfterAccepting {
            answers: 0,
            waits: Vec::new(),
        };
        let (result, outcome) = run_all(
            &mut client,
            &[machine('a', 1)],
            MachineRelease::Stable,
            IngressImage::Latest,
            "ployz server upgrade stable a".into(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(client.waits, [ployz_core::MACHINE_START_WAIT]);
        let error = outcome.unwrap_err();
        assert!(!error.is_outcome());
        let text = crate::ui::plain(&error);
        assert!(
            text.contains("Server a stopped answering during upgrade")
                && text.contains("didn't answer again within 5 minutes.")
                && text
                    .contains("It may have powered off, lost its network or still be restarting.")
                && text.contains("`ployz server show a`"),
            "{text}"
        );
        assert!(result.is_some(), "the accepted attempt is kept");
    }

    #[tokio::test]
    async fn each_answer_gives_a_restarting_server_its_start_window_again() {
        let mut client = GoneAfterAccepting {
            answers: 1,
            waits: Vec::new(),
        };
        let (_, outcome) = run_all(
            &mut client,
            &[machine('a', 1)],
            MachineRelease::Stable,
            IngressImage::Latest,
            "ployz server upgrade stable a".into(),
            &CancellationToken::new(),
        )
        .await;
        assert!(outcome.is_err());
        assert_eq!(
            client.waits.get(1),
            Some(&ployz_core::MACHINE_START_WAIT),
            "{:?}",
            client.waits
        );
    }

    #[tokio::test]
    async fn ctrl_c_during_the_ingress_move_names_the_rerun() {
        struct Hangs;
        impl UpgradeRequests for Hangs {
            async fn request_upgrade(
                &mut self,
                request: RequestMachineUpgradeRequest,
                _machine: &Machine,
                _wait: Duration,
            ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>
            {
                Ok(MachineUpgradeAttempt {
                    attempt_id: request.attempt_id,
                    target: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
                    outcome: succeeded(),
                })
            }
            async fn poll_upgrade(
                &mut self,
                _request: RequestMachineUpgradeRequest,
                _machine: &Machine,
                _wait: Duration,
            ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>
            {
                unreachable!("a settled request is not polled")
            }
            async fn move_ingress(
                &mut self,
                _image: IngressImage,
            ) -> Result<Option<Outcome>, Error> {
                std::future::pending().await
            }
        }
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        tokio::spawn(async move { cancel.cancel() });
        let (_, outcome) = run_all(
            &mut Hangs,
            &[machine('a', 1)],
            MachineRelease::Stable,
            IngressImage::Latest,
            "ployz server upgrade stable a".into(),
            &cancellation,
        )
        .await;
        let error = outcome.unwrap_err();
        assert!(error.is_interrupted());
        assert_eq!(
            error.report().details.get("next").cloned(),
            Some(json!("ployz server upgrade stable a"))
        );
    }

    #[tokio::test]
    async fn ctrl_c_stops_following_and_leaves_the_upgrade_running() {
        // #2888: Ctrl-C was ignored until the upgrade finished.
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut client = Sequence {
            outcomes: vec![MachineUpgradeOutcome::Accepted].into(),
            ingress: None,
            moved: Vec::new(),
        };
        let mut seen = None;
        let error = run_one(
            &mut client,
            &machine('a', 1),
            MachineRelease::Stable,
            MachineUpgradeAttemptId::random(),
            &mut seen,
            &cancellation,
        )
        .await
        .err()
        .unwrap();
        assert!(error.is_interrupted());
        assert!(error.to_string().contains("ployz server show a"), "{error}");
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_failed_daemon_upgrade_stops_before_the_ingress_proxy_moves() {
        let machines = [machine('a', 1), machine('b', 2)];
        let failed = MachineUpgradeOutcome::Failed {
            stage: ployz_core::MachineUpgradeStage::Acquiring,
            error: "install failed".into(),
        };
        let (result, outcome, moved) = run_sequence(&machines, vec![failed], None).await;

        assert!(outcome.unwrap_err().to_string().contains("install failed"));
        let result = result.unwrap();
        assert_eq!(result["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(result["unattempted"], json!([machines[1].id]));
        assert!(moved.is_empty());
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_failed_ingress_move_after_upgraded_daemons_is_partial_with_the_rerun() {
        let machines = [machine('a', 1), machine('b', 2)];
        let (result, outcome, moved) = run_sequence(
            &machines,
            vec![succeeded(), succeeded()],
            Some(Error::unavailable("caddy image pull failed")),
        )
        .await;

        let failure = outcome.unwrap_err();
        assert_eq!(failure.causes(), ["caddy image pull failed"]);
        let error = failure.report();
        assert_eq!(
            error.message,
            "Servers upgraded; the Ingress Proxy did not follow."
        );
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.details["next"], "ployz server upgrade 1.2.3 a b");
        let result = result.unwrap();
        assert_eq!(result["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(result["ingress"], Value::Null);
        assert_eq!(moved, [IngressImage::Latest]);
    }

    #[tokio::test]
    async fn every_json_attempt_names_its_server_by_id() {
        let failed = MachineUpgradeOutcome::Failed {
            stage: ployz_core::MachineUpgradeStage::Launching,
            error: "no systemd".into(),
        };
        let interrupted = MachineUpgradeOutcome::Interrupted {
            stage: ployz_core::MachineUpgradeStage::Launching,
        };
        let (a, b, c) = (machine('a', 1), machine('b', 2), machine('c', 3));
        let cases = [
            (vec![a.clone()], vec![succeeded()], vec![("a", "succeeded")]),
            (vec![a.clone()], vec![failed.clone()], vec![("a", "failed")]),
            (
                vec![a.clone()],
                vec![interrupted.clone()],
                vec![("a", "interrupted")],
            ),
            (
                vec![a.clone()],
                vec![MachineUpgradeOutcome::Accepted],
                vec![("a", "accepted")],
            ),
            (
                vec![a.clone(), b.clone(), c.clone()],
                vec![succeeded(), succeeded(), failed],
                vec![("a", "succeeded"), ("b", "succeeded"), ("c", "failed")],
            ),
            (
                vec![a, b, c],
                vec![succeeded(), interrupted],
                vec![("a", "succeeded"), ("b", "interrupted")],
            ),
        ];
        for (machines, outcomes, expected) in cases {
            let (result, _, _) = run_sequence(&machines, outcomes, None).await;
            let result = result.unwrap();
            let attempts = result.pointer("/attempts").and_then(Value::as_array);
            let named = attempts
                .unwrap()
                .iter()
                .map(|attempt| {
                    assert!(attempt.get("attempt_id").is_some(), "{attempt}");
                    assert!(attempt.get("target").is_some(), "{attempt}");
                    (
                        attempt.get("server").cloned().unwrap_or_default(),
                        attempt.get("outcome").cloned().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let expected = expected
                .into_iter()
                .map(|(name, outcome)| {
                    let id = MachineId::parse(name.repeat(32)).unwrap();
                    (json!(id), json!(outcome))
                })
                .collect::<Vec<_>>();
            assert_eq!(named, expected, "{result}");
        }
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn servers_without_the_ingress_role_leave_the_proxy_alone() {
        let mut builder = machine('a', 1);
        builder.accepts_ingress = false;
        let (result, outcome, moved) = run_sequence(&[builder], vec![succeeded()], None).await;

        outcome.unwrap();
        assert_eq!(result.unwrap()["attempts"].as_array().unwrap().len(), 1);
        assert!(moved.is_empty());
    }

    #[tokio::test]
    async fn definitive_busy_rejection_is_not_reported_as_uncertain() {
        let machine = machine('a', 1);
        let attempt_id = MachineUpgradeAttemptId::parse("1".repeat(32)).unwrap();
        let mut client = FakeRequests {
            request: Some(Err(crate::setup_retry::Error::Permanent(
                ConnectError::Remote(RpcError {
                    code: RpcErrorCode::Conflict,
                    message: "a Server upgrade or mutation is active".into(),
                    details: Value::Null,
                    cause: Vec::new(),
                }),
            ))),
            seen: Vec::new(),
        };

        let error = run_one(
            &mut client,
            &machine,
            MachineRelease::parse("beta").unwrap(),
            attempt_id,
            &mut None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("upgrade or mutation is active"));
        assert!(!error.to_string().contains("Cannot tell whether"));
        assert_eq!(client.seen, [(attempt_id, machine.id.as_str().to_owned())]);
    }

    #[tokio::test]
    async fn lost_request_reply_is_uncertain_and_keeps_the_dispatched_attempt_id() {
        let machine = machine('b', 2);
        let attempt_id = MachineUpgradeAttemptId::parse("2".repeat(32)).unwrap();
        let mut client = FakeRequests {
            request: Some(Err(crate::setup_retry::Error::Exhausted {
                operation: "RequestUpgrade".into(),
                wait: crate::setup_retry::WAIT,
                last: Some(ConnectError::EntryNotReady),
                in_flight: false,
            })),
            seen: Vec::new(),
        };

        let error = run_one(
            &mut client,
            &machine,
            MachineRelease::parse("1.2.3").unwrap(),
            attempt_id,
            &mut None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("Cannot tell whether"), "{error}");
        assert!(error.contains(attempt_id.as_str()), "{error}");
        assert!(error.contains("ployz server show"), "{error}");
        assert_eq!(client.seen, [(attempt_id, machine.id.as_str().to_owned())]);
    }

    /// A ployzd generation serving upgrade requests. With `accept`, the first request
    /// answers Accepted. Each later resend answers `poll`; the first with `hang` signals and
    /// never answers, and an absent `poll` outcome answers with an Unknown gRPC status.
    fn daemon(
        path: &std::path::Path,
        accept: bool,
        poll: Option<MachineUpgradeOutcome>,
        hang: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> tokio::runtime::Runtime {
        use ployz_core::{RpcRequestBody, RpcResponse};
        let accept = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(accept));
        let hang = std::sync::Arc::new(std::sync::Mutex::new(hang));
        let target = ployz_core::MachineVersion::parse("1.2.3").unwrap();
        crate::connect::test_support::unix_daemon(path, move |body| {
            let (accept, hang, poll, target) =
                (accept.clone(), hang.clone(), poll.clone(), target.clone());
            async move {
                let RpcRequestBody::RequestMachineUpgrade(request) = body else {
                    panic!("unexpected request: {body:?}");
                };
                let outcome = if accept.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    MachineUpgradeOutcome::Accepted
                } else {
                    let hang = hang.lock().unwrap().take();
                    if let Some(hang) = hang {
                        hang.send(()).unwrap();
                        std::future::pending::<()>().await;
                    }
                    let Some(outcome) = poll else {
                        return Err(tonic::Status::unknown("upgrade record is unreadable"));
                    };
                    outcome
                };
                let attempt_id = request.attempt_id;
                Ok(RpcResponse::from(MachineUpgradeAttempt {
                    attempt_id,
                    target,
                    outcome,
                }))
            }
        })
    }

    #[test]
    fn daemon_restart_during_the_upgrade_is_waited_out() {
        // libtest captures eprintln!, so observe the notice from a child run.
        const CHILD: &str = "PLOYZ_UPGRADE_RESTART_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(restart_during_upgrade());
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handlers::server::upgrade::tests::daemon_restart_during_the_upgrade_is_waited_out",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(
            stderr.contains("Waiting for ployzd to restart…"),
            "{stderr}"
        );
        assert!(!stderr.contains("firewall"), "{stderr}");
    }

    #[test]
    fn a_lone_failed_server_is_named_on_stdout() {
        // The result lines go to the process's real stdout, so read them from a child run.
        const CHILD: &str = "PLOYZ_UPGRADE_FAILED_CHILD";
        if std::env::var_os(CHILD).is_some() {
            crate::ui::init(false, crate::ui::Color::Never);
            let failed = MachineUpgradeOutcome::Failed {
                stage: ployz_core::MachineUpgradeStage::Launching,
                error: "no systemd".into(),
            };
            let (_, outcome, _) = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(run_sequence(&[machine('a', 1)], vec![failed], None));
            assert!(outcome.is_err());
            let upgrade = CloudUpgrade {
                outcome: "failed".into(),
                from_version: Some("1.2.2".into()),
                target_version: Some("1.2.3".into()),
                message: Some("install failed".into()),
            };
            assert!(stopped("b", &upgrade).is_some());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handlers::server::upgrade::tests::a_lone_failed_server_is_named_on_stdout",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("Server a failed to upgrade to 1.2.3 while launching.\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains("Server b failed to upgrade to 1.2.3.\n"),
            "{stdout}"
        );
    }

    async fn restart_during_upgrade() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ployz.sock");
        let (hung, restarting) = tokio::sync::oneshot::channel();
        let old = daemon(
            &path,
            true,
            Some(MachineUpgradeOutcome::Accepted),
            Some(hung),
        );
        let mut client = crate::connect::test_support::unix_client(&path).await;
        let machine = machine('e', 5);
        let restart = async {
            restarting.await.unwrap();
            // The old process exits with the resend in flight.
            old.shutdown_background();
            std::fs::remove_file(&path).unwrap();
            // real-wait: keeps the daemon socket missing across a client retry; a connect to a missing socket emits no event
            tokio::time::sleep(Duration::from_secs(2)).await;
            daemon(
                &path,
                false,
                Some(MachineUpgradeOutcome::Succeeded {
                    version: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
                }),
                None,
            )
        };
        let mut seen = None;
        let cancellation = CancellationToken::new();
        let (attempt, new) = tokio::join!(
            run_one(
                &mut client,
                &machine,
                MachineRelease::parse("1.2.3").unwrap(),
                MachineUpgradeAttemptId::random(),
                &mut seen,
                &cancellation,
            ),
            restart
        );
        new.shutdown_background();
        let attempt = attempt.unwrap();
        assert!(
            matches!(attempt.outcome, MachineUpgradeOutcome::Succeeded { .. }),
            "{attempt:?}"
        );
    }

    #[tokio::test]
    async fn daemon_error_answer_fails_the_upgrade_without_retrying() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ployz.sock");
        let daemon = daemon(&path, true, None, None);
        let mut client = crate::connect::test_support::unix_client(&path).await;
        let started = Instant::now();
        let mut seen = None;
        let error = run_one(
            &mut client,
            &machine('f', 6),
            MachineRelease::parse("1.2.3").unwrap(),
            MachineUpgradeAttemptId::random(),
            &mut seen,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let error = crate::ui::chain_text(&error);
        daemon.shutdown_background();
        assert!(error.contains("upgrade record is unreadable"), "{error}");
        // The accepted attempt survives the failed poll as committed evidence.
        assert!(matches!(
            seen.map(|attempt| attempt.outcome),
            Some(MachineUpgradeOutcome::Accepted)
        ));
        // One poll interval, no retry budget spent.
        assert!(started.elapsed() < Duration::from_secs(3), "{error}");
    }

    #[test]
    fn first_and_middle_failures_report_the_complete_ordered_suffix() {
        let machines = [
            machine('a', 1),
            machine('b', 2),
            machine('c', 3),
            machine('d', 4),
        ];

        assert_eq!(
            unattempted_warning(names(&machines[1..]), "a").as_deref(),
            Some("Did not upgrade Servers b, c, d: Server a stopped the run.")
        );
        assert_eq!(
            unattempted_warning(names(&machines[3..]), "c").as_deref(),
            Some("Did not upgrade Server d: Server c stopped the run.")
        );
        assert_eq!(unattempted_warning(names(&[]), "d"), None);
    }

    #[test]
    fn a_failed_cloud_run_still_reports_the_upgrades_before_it() {
        let machines = [machine('a', 1), machine('b', 2), machine('c', 3)];
        let attempts =
            vec![serde_json::json!({ "server": machines[0].id, "outcome": "succeeded" })];
        let error = Error::coded(RpcErrorCode::Unavailable, "Could not reach Cloud.");
        let mode = crate::ui::test_mode(true);
        let (result, captured) =
            crate::ui::captured(|| cut_short(attempts, &machines[2..], &[], "b", error));
        drop(mode);
        assert_eq!(
            result.unwrap_err().json().pointer("/message").unwrap(),
            "Could not reach Cloud."
        );
        let captured = captured.unwrap();
        assert_eq!(
            captured.pointer("/attempts/0/server").unwrap(),
            machines[0].id.as_str()
        );
        assert_eq!(
            captured.pointer("/unattempted").unwrap(),
            &serde_json::json!([machines[2].id])
        );
        assert_eq!(
            captured.pointer("/follow_up_error/message").unwrap(),
            "Could not reach Cloud."
        );
    }

    fn running(id: char, version: &str) -> Machine {
        let mut machine = machine(id, 1);
        machine.runtime.daemon_version = version.into();
        machine
    }

    #[test]
    fn an_older_target_needs_allow_downgrade_without_a_terminal() {
        // #2991: `server upgrade 0.3.0-beta.6` moved a beta.7 Server back with no prompt.
        let older = MachineRelease::parse("0.3.0").unwrap();
        let servers = [running('a', "0.3.0"), running('b', "0.3.1")];
        let refused = confirm_downgrades(&servers, &older, false)
            .unwrap_err()
            .report();
        assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        assert!(
            refused.message.contains("downgrade")
                && refused.message.contains("Server b")
                && !refused.message.contains("Server a")
                && refused.message.contains("--allow-downgrade"),
            "{}",
            refused.message
        );
        assert!(confirm_downgrades(&servers, &older, true).is_ok());
        assert!(!refused.message.contains("relay"), "{}", refused.message);

        // Below the relay cut-off the refusal says what the downgrade cuts off.
        let past_relay = MachineRelease::parse("0.3.0-beta.6").unwrap();
        let refused = confirm_downgrades(&[running('b', "0.3.0-beta.7")], &past_relay, false)
            .unwrap_err()
            .report();
        assert!(
            refused.message.contains("through the relay"),
            "{}",
            refused.message
        );
    }

    #[test]
    fn a_same_or_newer_target_never_asks() {
        for target in ["0.3.1", "0.3.2", "stable", "beta"] {
            let release = MachineRelease::parse(target).unwrap();
            assert!(
                confirm_downgrades(&[running('a', "0.3.1")], &release, false).is_ok(),
                "{target}"
            );
        }
        // A version Ployz never published can't be compared, so the daemon decides.
        let older = MachineRelease::parse("0.3.0").unwrap();
        assert!(confirm_downgrades(&[running('a', "dev")], &older, false).is_ok());
    }

    fn machine(id: char, subnet: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(id.to_string()).unwrap(),
            subnet: format!("10.210.{subnet}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([subnet; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: MachineRuntime::default(),
            build_concurrency: None,
        }
    }
}
