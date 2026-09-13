//! Explicit, bounded Machine upgrade sequencing and inspection.

use std::{collections::BTreeSet, time::Duration};

use clap::ArgMatches;
use ployz_core::{
    InspectMachineUpgradeRequest, Machine, MachineRelease, MachineTarget, MachineUpgradeAttempt,
    MachineUpgradeAttemptId, MachineUpgradeOutcome, RequestMachineUpgradeRequest, op,
};
use tokio::time::Instant;

use crate::{cluster::Client, connect::ConnectError};

use super::super::{Error, leaf_matches, string_values, with_client};

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(16 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

trait UpgradeRequests {
    async fn request_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        target: &MachineTarget,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>;

    async fn inspect_upgrade(
        &mut self,
        request: InspectMachineUpgradeRequest,
        target: &MachineTarget,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>>;
}

impl UpgradeRequests for Client {
    async fn request_upgrade(
        &mut self,
        request: RequestMachineUpgradeRequest,
        target: &MachineTarget,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
        self.call_repeatable_for::<op::RequestMachineUpgrade>(request, Some(target), wait)
            .await
    }

    async fn inspect_upgrade(
        &mut self,
        request: InspectMachineUpgradeRequest,
        target: &MachineTarget,
        wait: Duration,
    ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
        self.call_repeatable_for::<op::InspectMachineUpgrade>(request, Some(target), wait)
            .await
    }
}

pub(in crate::handlers) fn upgrade(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let release = matches
        .get_one::<MachineRelease>("version")
        .cloned()
        .ok_or_else(|| Error::usage("upgrade version is required"))?;
    let selectors = string_values(matches, "machine");
    with_client(root, |client| {
        Box::pin(async move {
            let machines = selected_machines(client, &selectors).await?;
            for (index, machine) in machines.iter().enumerate() {
                let attempt_id = MachineUpgradeAttemptId::random();
                let attempt = match run_one(client, machine, release.clone(), attempt_id).await {
                    Ok(attempt) => attempt,
                    Err(error) => {
                        print_unattempted(machines.iter().skip(index.saturating_add(1)), machine);
                        return Err(error);
                    }
                };
                print_attempt(machine, &attempt);
                match attempt.outcome {
                    MachineUpgradeOutcome::Succeeded { .. } => {}
                    MachineUpgradeOutcome::Failed { error, .. } => {
                        print_unattempted(machines.iter().skip(index.saturating_add(1)), machine);
                        return Err(Error::usage(error));
                    }
                    MachineUpgradeOutcome::Interrupted { .. } => {
                        print_unattempted(machines.iter().skip(index.saturating_add(1)), machine);
                        return Err(Error::usage(format!(
                            "Machine {} upgrade was interrupted; {}",
                            machine.name,
                            journal_hint(attempt_id)
                        )));
                    }
                    MachineUpgradeOutcome::Accepted | MachineUpgradeOutcome::Running { .. } => {
                        unreachable!("run_one returns only terminal evidence")
                    }
                }
            }
            Ok(())
        })
    })
}

pub(in crate::handlers) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let target = MachineTarget::parse(
        matches
            .get_one::<String>("machine")
            .ok_or_else(|| Error::usage("machine is required"))?,
    )?;
    let attempt_id = matches
        .get_one::<MachineUpgradeAttemptId>("attempt")
        .copied();
    let json = matches.get_one::<String>("output").is_some();
    with_client(root, |client| {
        Box::pin(async move {
            let attempt = client
                .call_repeatable::<op::InspectMachineUpgrade>(
                    InspectMachineUpgradeRequest { attempt_id },
                    Some(&target),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&attempt)?);
            } else {
                print_attempt_target(target.as_str(), &attempt);
            }
            Ok(())
        })
    })
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
                "Machine {} was selected more than once",
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
) -> Result<MachineUpgradeAttempt, Error> {
    let target = MachineTarget::from(&machine.id);
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    let request = RequestMachineUpgradeRequest {
        attempt_id,
        release,
    };
    let accepted = match client
        .request_upgrade(
            request,
            &target,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await
    {
        Ok(accepted) => accepted,
        Err(crate::setup_retry::Error::Permanent(error)) => return Err(error.into()),
        Err(crate::setup_retry::Error::Exhausted(error)) => {
            return Err(uncertain(machine, attempt_id, error));
        }
    };
    print_attempt(machine, &accepted);
    if accepted.is_terminal() {
        return Ok(accepted);
    }

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
                &target,
                deadline.saturating_duration_since(Instant::now()),
            )
            .await
            .map_err(|error| match error {
                crate::setup_retry::Error::Permanent(error) => error.into(),
                crate::setup_retry::Error::Exhausted(error) => {
                    uncertain(machine, attempt_id, error)
                }
            })?;
        if observed.is_terminal() {
            return Ok(observed);
        }
    }
}

fn print_attempt(machine: &Machine, attempt: &MachineUpgradeAttempt) {
    print_attempt_target(&format!("{} ({})", machine.name, machine.id), attempt);
}

fn print_attempt_target(machine: &str, attempt: &MachineUpgradeAttempt) {
    let MachineUpgradeAttempt {
        attempt_id,
        target,
        outcome,
    } = attempt;
    match outcome {
        MachineUpgradeOutcome::Accepted => println!(
            "Machine {machine}: upgrade {attempt_id} accepted for {target}; {}",
            journal_hint(*attempt_id)
        ),
        MachineUpgradeOutcome::Running { stage } => println!(
            "Machine {machine}: upgrade {attempt_id} is {} for {target}; {}",
            stage.as_str(),
            journal_hint(*attempt_id)
        ),
        MachineUpgradeOutcome::Succeeded { version } => {
            println!("Machine {machine}: upgrade {attempt_id} succeeded; running version {version}")
        }
        MachineUpgradeOutcome::Failed { stage, error } => println!(
            "Machine {machine}: upgrade {attempt_id} failed at {} for {target}: {error}; {}",
            stage.as_str(),
            journal_hint(*attempt_id)
        ),
        MachineUpgradeOutcome::Interrupted { stage } => println!(
            "Machine {machine}: upgrade {attempt_id} was interrupted at {} for {target}; {}",
            stage.as_str(),
            journal_hint(*attempt_id)
        ),
    }
}

fn print_unattempted<'a>(machines: impl IntoIterator<Item = &'a Machine>, after: &Machine) {
    for line in unattempted_lines(machines, after) {
        println!("{line}");
    }
}

fn unattempted_lines<'a>(
    machines: impl IntoIterator<Item = &'a Machine>,
    after: &Machine,
) -> Vec<String> {
    machines
        .into_iter()
        .map(|machine| {
            format!(
                "Machine {} ({}): upgrade unattempted after {} ({})",
                machine.name, machine.id, after.name, after.id
            )
        })
        .collect()
}

fn journal_hint(attempt_id: MachineUpgradeAttemptId) -> String {
    format!("inspect locally with `journalctl -u ployz-upgrade-{attempt_id}.service`")
}

fn uncertain(
    machine: &Machine,
    attempt_id: MachineUpgradeAttemptId,
    error: impl std::fmt::Display,
) -> Error {
    Error::usage(format!(
        "Machine {} ({}) upgrade {attempt_id} outcome is uncertain: {error}; reconnect and run `ployz machine upgrade inspect {} --attempt {attempt_id}`; {}",
        machine.name,
        machine.id,
        machine.id,
        journal_hint(attempt_id)
    ))
}

fn uncertain_timeout(machine: &Machine, attempt_id: MachineUpgradeAttemptId) -> Error {
    Error::usage(format!(
        "Machine {} ({}) upgrade {attempt_id} outcome is uncertain after {} minutes; reconnect and run `ployz machine upgrade inspect {} --attempt {attempt_id}`; {}",
        machine.name,
        machine.id,
        OBSERVATION_TIMEOUT.as_secs() / 60,
        machine.id,
        journal_hint(attempt_id)
    ))
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
            target: &MachineTarget,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            self.seen
                .push((request.attempt_id, target.as_str().to_owned()));
            self.request.take().expect("one request result")
        }

        async fn inspect_upgrade(
            &mut self,
            _request: InspectMachineUpgradeRequest,
            _target: &MachineTarget,
            _wait: Duration,
        ) -> Result<MachineUpgradeAttempt, crate::setup_retry::Error<ConnectError>> {
            panic!("an unaccepted request is not inspected")
        }
    }

    #[tokio::test]
    async fn definitive_busy_rejection_is_not_reported_as_uncertain() {
        let machine = machine('a', 1);
        let attempt_id = MachineUpgradeAttemptId::parse("1".repeat(32)).unwrap();
        let mut client = FakeRequests {
            request: Some(Err(crate::setup_retry::Error::Permanent(
                ConnectError::Remote(RpcError {
                    code: RpcErrorCode::Conflict,
                    message: "a Machine upgrade or mutation is active".into(),
                    details: Value::Null,
                }),
            ))),
            seen: Vec::new(),
        };

        let error = run_one(
            &mut client,
            &machine,
            MachineRelease::parse("beta").unwrap(),
            attempt_id,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("upgrade or mutation is active"));
        assert!(!error.to_string().contains("outcome is uncertain"));
        assert_eq!(client.seen, [(attempt_id, machine.id.as_str().to_owned())]);
    }

    #[tokio::test]
    async fn lost_request_reply_is_uncertain_and_keeps_the_dispatched_attempt_id() {
        let machine = machine('b', 2);
        let attempt_id = MachineUpgradeAttemptId::parse("2".repeat(32)).unwrap();
        let mut client = FakeRequests {
            request: Some(Err(crate::setup_retry::Error::Exhausted(
                "request reply was lost".into(),
            ))),
            seen: Vec::new(),
        };

        let error = run_one(
            &mut client,
            &machine,
            MachineRelease::parse("1.2.3").unwrap(),
            attempt_id,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("outcome is uncertain"), "{error}");
        assert!(error.contains(attempt_id.as_str()), "{error}");
        assert!(error.contains("machine upgrade inspect"), "{error}");
        assert_eq!(client.seen, [(attempt_id, machine.id.as_str().to_owned())]);
    }

    #[test]
    fn first_and_middle_failures_report_the_complete_ordered_suffix() {
        let machines = [
            machine('a', 1),
            machine('b', 2),
            machine('c', 3),
            machine('d', 4),
        ];

        let first = unattempted_lines(&machines[1..], &machines[0]);
        assert_eq!(
            first,
            [
                "Machine b (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb): upgrade unattempted after a (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)",
                "Machine c (cccccccccccccccccccccccccccccccc): upgrade unattempted after a (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)",
                "Machine d (dddddddddddddddddddddddddddddddd): upgrade unattempted after a (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)",
            ]
        );

        let middle = unattempted_lines(&machines[2..], &machines[1]);
        assert_eq!(
            middle,
            [
                "Machine c (cccccccccccccccccccccccccccccccc): upgrade unattempted after b (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb)",
                "Machine d (dddddddddddddddddddddddddddddddd): upgrade unattempted after b (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb)",
            ]
        );
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
        }
    }
}
