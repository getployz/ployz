//! Explicit, bounded Machine upgrade sequencing and inspection.

use std::{collections::BTreeSet, time::Duration};

use clap::ArgMatches;
use ployz_core::{
    InspectMachineUpgradeRequest, Machine, MachineRelease, MachineTarget, MachineUpgradeAttempt,
    MachineUpgradeAttemptId, MachineUpgradeOutcome, RequestMachineUpgradeRequest, RpcErrorCode, op,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

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

    async fn inspect_upgrade(
        &mut self,
        request: InspectMachineUpgradeRequest,
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

    async fn inspect_upgrade(
        &mut self,
        request: InspectMachineUpgradeRequest,
        machine: &Machine,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
        self.call_repeatable_for::<op::InspectMachineUpgrade>(
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
    let version = release.to_string();
    let mut args = vec!["server", "upgrade", version.as_str()];
    args.extend(selectors.iter().map(String::as_str));
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
            let (result, outcome) = run_all(client, &machines, release, ingress, rerun).await;
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
    let mut attempts = Vec::new();
    let mut skipped = Vec::new();
    for (index, (machine, selector)) in servers.iter().zip(selectors).enumerate() {
        let (id, name) = (machine.id, machine.name.as_str());
        let started = runtime.block_on(cloud_account::start_run(
            credential,
            Method::POST,
            &format!("servers/{id}/upgrade"),
            &json!({ "channel": channel }),
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
                return Err(match along {
                    Some(hint) => Error::from(refused).hint(hint),
                    None => refused.into(),
                });
            }
            Err(StoreCallError::Refused(refused)) if refused.code == RpcErrorCode::Unavailable => {
                skipped.push(skip(machine, selector, refused.message));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        crate::ui::stream(format_args!("Upgrading Server {name}."));
        let settled = runtime
            .block_on(cloud_account::follow_run::<Settled<CloudUpgrade>>(
                credential,
                &format!("server-upgrades/{run}"),
                &format!("Upgrading Server {name}"),
                None,
            ))?
            .expect("a run followed without a deadline settles or fails");
        let upgrade = match settled {
            Settled::Ended { code, message } if code == "busy" => {
                skipped.push(skip(machine, selector, message));
                continue;
            }
            settled @ (Settled::Finished(_) | Settled::Ended { .. }) => settled.finished()?,
        };
        let stopped = stopped(name, &upgrade);
        attempts.push(json!({
            "server": id,
            "id": run,
            "outcome": upgrade.outcome,
            "from_version": upgrade.from_version,
            "target_version": upgrade.target_version,
            "message": upgrade.message,
        }));
        let Some(stopped) = stopped else {
            continue;
        };
        let unattempted = servers.get(index + 1..).unwrap_or_default();
        if let Some(warning) = unattempted_warning(names(unattempted), name) {
            crate::ui::warn(warning);
        }
        let result = json!({
            "attempts": attempts,
            "unattempted": unattempted.iter().map(|machine| machine.id).collect::<Vec<_>>(),
            "skipped": skipped_json(&skipped),
        });
        return crate::ui::emit_committed(result, Err(stopped));
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
        "succeeded" => {
            crate::ui::stream(format_args!("Upgraded Server {server} to {target}."));
            return None;
        }
        "failed" => format!("Server {server} failed to upgrade to {target}{reason}"),
        "interrupted" => {
            format!("The upgrade of Server {server} to {target} was interrupted{reason}")
        }
        _ => format!("Cloud cannot tell whether Server {server} upgraded to {target}{reason}"),
    };
    Some(Error::coded(RpcErrorCode::Internal, message))
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
) -> (Option<serde_json::Value>, Result<(), Error>) {
    let mut attempts = Vec::new();
    for (index, machine) in machines.iter().enumerate() {
        let attempt_id = MachineUpgradeAttemptId::random();
        let mut seen = None;
        let stopped = match run_one(client, machine, release.clone(), attempt_id, &mut seen).await {
            Err(error) => {
                // An accepted attempt is committed evidence: keep it in the result.
                attempts.extend(seen);
                error
            }
            Ok(attempt) => {
                print_attempt(machine, &attempt);
                let stopped = match &attempt.outcome {
                    MachineUpgradeOutcome::Succeeded { .. } => None,
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
                attempts.push(attempt);
                match stopped {
                    Some(error) => error,
                    None => continue,
                }
            }
        };
        let unattempted = machines.get(index + 1..).unwrap_or_default();
        if let Some(warning) = unattempted_warning(names(unattempted), machine.name.as_str()) {
            crate::ui::warn(warning);
        }
        // A recorded attempt is a result: print it, then exit partial.
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
        client.move_ingress(ingress).await
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
) -> Result<MachineUpgradeAttempt, Error> {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    let request = RequestMachineUpgradeRequest {
        attempt_id,
        release,
    };
    let accepted = match client
        // A Server that cannot take the request within a minute is down, not busy.
        .request_upgrade(request, machine, crate::setup_retry::WAIT)
        .await
    {
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
        if tokio::time::timeout_at(deadline, tokio::time::sleep(POLL_INTERVAL))
            .await
            .is_err()
        {
            return Err(uncertain_timeout(machine, attempt_id));
        }
        let observed = client
            .inspect_upgrade(
                InspectMachineUpgradeRequest {
                    attempt_id: Some(attempt_id),
                },
                machine,
                deadline.saturating_duration_since(Instant::now()),
            )
            .await
            .map_err(|error| match error {
                crate::setup_retry::Error::Permanent(error) => error.into(),
                exhausted => uncertain(machine, attempt_id, exhausted),
            })?;
        if observed.is_terminal() {
            return Ok(observed);
        }
        *seen = Some(observed);
    }
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
            crate::ui::stream(format_args!("Upgraded Server {name} to {version}."));
        }
        MachineUpgradeOutcome::Failed { .. } | MachineUpgradeOutcome::Interrupted { .. } => {}
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
            "Cannot tell whether Server {} finished upgrade {attempt_id}. Reconnect and compare it with the upgrade attempt `ployz server inspect {}` shows.",
            machine.name, machine.name,
        ),
        error,
    )
    .hint(journal_hint(attempt_id))
}

fn uncertain_timeout(machine: &Machine, attempt_id: MachineUpgradeAttemptId) -> Error {
    Error::unavailable(format!(
        "Cannot tell whether Server {} finished upgrade {attempt_id} after {} minutes. Reconnect and compare it with the upgrade attempt `ployz server inspect {}` shows.",
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

        async fn inspect_upgrade(
            &mut self,
            _request: InspectMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            panic!("an unaccepted request is not inspected")
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
                target: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
                outcome: self.outcomes.pop_front().expect("one outcome per request"),
            })
        }

        async fn inspect_upgrade(
            &mut self,
            _request: InspectMachineUpgradeRequest,
            _machine: &Machine,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            panic!("terminal answers are not inspected")
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
        )
        .await;
        (result, outcome, client.moved)
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
            })),
            seen: Vec::new(),
        };

        let error = run_one(
            &mut client,
            &machine,
            MachineRelease::parse("1.2.3").unwrap(),
            attempt_id,
            &mut None,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("Cannot tell whether"), "{error}");
        assert!(error.contains(attempt_id.as_str()), "{error}");
        assert!(error.contains("ployz server inspect"), "{error}");
        assert_eq!(client.seen, [(attempt_id, machine.id.as_str().to_owned())]);
    }

    /// A ployzd generation that accepts upgrades. The first inspect with `hang`
    /// signals and never answers. An absent `inspect` outcome answers with an
    /// Unknown gRPC status.
    fn daemon(
        path: &std::path::Path,
        inspect: Option<MachineUpgradeOutcome>,
        hang: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> tokio::runtime::Runtime {
        use ployz_core::{RpcRequestBody, RpcResponse};
        let hang = std::sync::Arc::new(std::sync::Mutex::new(hang));
        let target = ployz_core::MachineVersion::parse("1.2.3").unwrap();
        crate::connect::test_support::unix_daemon(path, move |body| {
            let (hang, inspect, target) = (hang.clone(), inspect.clone(), target.clone());
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "this fixture serves only upgrade RPCs"
                )]
                let (attempt_id, outcome) = match body {
                    RpcRequestBody::RequestMachineUpgrade(request) => {
                        (request.attempt_id, MachineUpgradeOutcome::Accepted)
                    }
                    RpcRequestBody::InspectMachineUpgrade(request) => {
                        let hang = hang.lock().unwrap().take();
                        if let Some(hang) = hang {
                            hang.send(()).unwrap();
                            std::future::pending::<()>().await;
                        }
                        let Some(outcome) = inspect else {
                            return Err(tonic::Status::unknown("upgrade record is unreadable"));
                        };
                        (request.attempt_id.unwrap(), outcome)
                    }
                    body => panic!("unexpected request: {body:?}"),
                };
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

    async fn restart_during_upgrade() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ployz.sock");
        let (hung, restarting) = tokio::sync::oneshot::channel();
        let old = daemon(&path, Some(MachineUpgradeOutcome::Accepted), Some(hung));
        let mut client = crate::connect::test_support::unix_client(&path).await;
        let machine = machine('e', 5);
        let restart = async {
            restarting.await.unwrap();
            // The old process exits with the inspect read in flight.
            old.shutdown_background();
            std::fs::remove_file(&path).unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
            daemon(
                &path,
                Some(MachineUpgradeOutcome::Succeeded {
                    version: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
                }),
                None,
            )
        };
        let mut seen = None;
        let (attempt, new) = tokio::join!(
            run_one(
                &mut client,
                &machine,
                MachineRelease::parse("1.2.3").unwrap(),
                MachineUpgradeAttemptId::random(),
                &mut seen,
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
        let daemon = daemon(&path, None, None);
        let mut client = crate::connect::test_support::unix_client(&path).await;
        let started = Instant::now();
        let mut seen = None;
        let error = run_one(
            &mut client,
            &machine('f', 6),
            MachineRelease::parse("1.2.3").unwrap(),
            MachineUpgradeAttemptId::random(),
            &mut seen,
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
