//! Assertion and polling helpers the scenario bodies share.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::MachineCredentialProvisioningStep;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployRunningStage, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatus,
};
use ployz_e2e::dind::DindMachine;
use ployz_nats::connect::{NatsConnectConfig, authenticated_connect_options};
use ployz_sdk_types::{MachineInspectRequest, MachineSnapshot, OpsStatusRequest};
use ployzd::docker::labels::MANAGED_LABEL;

use super::formation::CoreContext;
use ployz_test_support::ids::{event_replay_limit, event_sequence, machine_id};

/// Per-request budget for HTTP probes against a published gateway port.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget for one server-side permission violation to arrive on the client
/// event channel.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn assert_unit_active(core: &CoreContext, machine: &DindMachine, unit: &str) {
    let outcome = core
        .exec_on(machine, &["systemctl", "is-active", unit])
        .await;
    assert!(
        outcome.stdout.trim() == "active",
        "unit {unit} on {} is not active: {outcome:?}",
        machine.name
    );
}

pub async fn unit_main_pid(core: &CoreContext, machine: &DindMachine, unit: &str) -> String {
    let outcome = core
        .exec_on(
            machine,
            &["systemctl", "show", "-p", "MainPID", "--value", unit],
        )
        .await;
    let pid = outcome.stdout.trim().to_owned();
    assert!(
        outcome.success() && !pid.is_empty() && pid != "0",
        "unit {unit} on {} has no main pid: {outcome:?}",
        machine.name
    );
    pid
}

/// Polls the units' journal on the core machine until the marker shows up
/// (journald may lag the unit's stderr by a moment).
pub async fn assert_journal_contains(core: &CoreContext, units: &[&str], marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let mut command = vec!["journalctl", "--no-pager"];
        for unit in units {
            command.push("-u");
            command.push(unit);
        }
        let outcome = core.exec_on(core.cluster.core(), &command).await;
        if outcome.stdout.contains(marker) {
            return;
        }
        last = outcome.stdout;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("journal of {units:?} never contained {marker:?}: {last}")
}

pub async fn operation_status(core: &CoreContext, operation_id: &OperationId) -> OperationStatus {
    core.api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .unwrap_or_else(|error| panic!("ops status {operation_id:?} failed: {error}"))
        .status
}

/// Replays the operation's event page from the first sequence.
pub async fn operation_event_page(
    core: &CoreContext,
    operation_id: &OperationId,
) -> OperationEventReplayPage {
    core.api
        .ops_watch(&OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(64),
        })
        .await
        .unwrap_or_else(|error| panic!("ops watch {operation_id:?} failed: {error}"))
}

pub async fn operation_events(
    core: &CoreContext,
    operation_id: &OperationId,
) -> Vec<OperationEvent> {
    operation_event_page(core, operation_id)
        .await
        .events
        .into_iter()
        .map(|event| event.event)
        .collect()
}

/// Replays the full event history of a terminal operation.
pub async fn terminal_operation_events(
    core: &CoreContext,
    operation_id: &OperationId,
) -> Vec<OperationEvent> {
    let page = operation_event_page(core, operation_id).await;
    assert!(
        page.cursor == OperationEventReplayCursor::Terminal,
        "operation {operation_id:?} replay is not terminal: {page:?}"
    );
    page.events.into_iter().map(|event| event.event).collect()
}

/// One named step of an expected event sequence.
pub type LabeledEventPredicate<'a> = (&'static str, Box<dyn Fn(&OperationEvent) -> bool + 'a>);

/// Resolves each labeled step to its event index and asserts the steps
/// appear in order; the panic message names the missing or misordered step.
/// Returns the resolved indices for window checks on top of the order.
pub fn assert_events_in_order(
    what: &str,
    events: &[OperationEvent],
    steps: Vec<LabeledEventPredicate<'_>>,
) -> Vec<usize> {
    let mut resolved: Vec<(&'static str, usize)> = Vec::with_capacity(steps.len());
    for (label, predicate) in &steps {
        let Some(index) = events.iter().position(predicate) else {
            panic!("{what}: missing event `{label}`: {events:?}");
        };
        resolved.push((label, index));
    }
    for ((earlier_label, earlier), (later_label, later)) in
        resolved.iter().zip(resolved.iter().skip(1))
    {
        assert!(
            earlier <= later,
            "{what}: event `{later_label}` (index {later}) arrived before \
             `{earlier_label}` (index {earlier}): {events:?}"
        );
    }
    resolved.into_iter().map(|(_, index)| index).collect()
}

/// The committed machine-add event vocabulary: submitted, then the five
/// mint steps in order, then joined, then completed — with acceptance
/// strictly before the reload.
pub fn assert_machine_add_event_sequence(events: &[OperationEvent], expected_machine: &MachineId) {
    let mut steps: Vec<LabeledEventPredicate<'_>> = vec![(
        "submitted",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddSubmitted { machine_id, .. } if machine_id == expected_machine
            )
        }),
    )];
    for (label, expected_step) in [
        (
            "credential-minted",
            MachineCredentialProvisioningStep::Minted,
        ),
        (
            "credential-rendered",
            MachineCredentialProvisioningStep::Rendered,
        ),
        (
            "credential-reloaded",
            MachineCredentialProvisioningStep::Reloaded,
        ),
        (
            "credential-verified",
            MachineCredentialProvisioningStep::Verified,
        ),
        (
            "credential-material-ready",
            MachineCredentialProvisioningStep::MaterialReady,
        ),
    ] {
        steps.push((
            label,
            Box::new(move |event| {
                matches!(
                    event,
                    OperationEvent::MachineAddCredentialProvisioned { step, machine_id, .. }
                        if *step == expected_step && machine_id == expected_machine
                )
            }),
        ));
    }
    steps.push((
        "joined",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddJoined { machine_id, .. } if machine_id == expected_machine
            )
        }),
    ));
    steps.push((
        "completed",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddCompleted { machine_id, .. } if machine_id == expected_machine
            )
        }),
    ));
    assert_events_in_order(
        &format!("machine add for {expected_machine:?}"),
        events,
        steps,
    );
}

/// Polls the operation status through the host-side API until the deploy is
/// terminal, within the budget.
pub async fn wait_for_terminal_deploy_status(
    core: &CoreContext,
    operation_id: &OperationId,
    budget: Duration,
) -> OperationStatus {
    let deadline = Instant::now() + budget;
    loop {
        let status = operation_status(core, operation_id).await;
        let OperationStatus::Deploy { state, .. } = &status else {
            panic!("operation {operation_id:?} is not a deploy: {status:?}");
        };
        if state.is_terminal() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "deploy {operation_id:?} not terminal in budget: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The committed deploy event vocabulary (the `operations.rs` sequence) for
/// the two-machine routed deploy: submitted → planning → plan →
/// WireGuard/eBPF preparation over both machines → container starts on both
/// machines → health → commit → completed, in order.
pub fn assert_deploy_event_sequence(events: &[OperationEvent], deploy_operation: &OperationId) {
    let steps: Vec<LabeledEventPredicate<'_>> = vec![
        (
            "submitted",
            Box::new(move |event| {
                matches!(
                    event,
                    OperationEvent::DeploySubmitted { operation_id, .. }
                        if operation_id == deploy_operation
                )
            }),
        ),
        (
            "planning-started",
            Box::new(|event| matches!(event, OperationEvent::DeployPlanningStarted { .. })),
        ),
        (
            "plan-created",
            Box::new(|event| matches!(event, OperationEvent::DeployPlanCreated { .. })),
        ),
        (
            "running:preparing-dataplane",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::PreparingDataplane,
                        ..
                    }
                )
            }),
        ),
        (
            "dataplane-prepared-on-both-machines",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployDataplanePrepared {
                        report,
                        ..
                    }
                        if report
                            .machines
                            .iter()
                            .map(|machine| machine.machine_id.clone())
                            .collect::<Vec<_>>()
                            == vec![machine_id("core_1"), machine_id("edge_2")]
                )
            }),
        ),
        (
            "running:starting-containers",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::StartingContainers,
                        ..
                    }
                )
            }),
        ),
        (
            "running:waiting-for-health",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::WaitingForHealth,
                        ..
                    }
                )
            }),
        ),
        (
            "health-check-started",
            Box::new(|event| matches!(event, OperationEvent::DeployHealthCheckStarted { .. })),
        ),
        (
            "running:active-service-commit",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::ServingTargetCommit,
                        ..
                    }
                )
            }),
        ),
        (
            "completed",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployCompleted {
                        outcome: DeployCompletionOutcome::Completed,
                        ..
                    }
                )
            }),
        ),
    ];
    let resolved = assert_events_in_order(&format!("deploy {deploy_operation:?}"), events, steps);

    // One container start per machine, both inside the StartingContainers →
    // WaitingForHealth window (their relative order is placement-dependent).
    let [.., starting_index, waiting_index, _, _, _] = resolved.as_slice() else {
        unreachable!("resolved has ten entries");
    };
    for expected_machine in [machine_id("core_1"), machine_id("edge_2")] {
        let started = events.iter().position(|event| {
            matches!(
                event,
                OperationEvent::DeployContainerStarted { machine_id, .. }
                    if *machine_id == expected_machine
            )
        });
        let Some(started) = started else {
            panic!("no container start on {expected_machine:?}: {events:?}");
        };
        assert!(
            started >= *starting_index && started <= *waiting_index,
            "container start on {expected_machine:?} outside the starting window: {events:?}"
        );
    }
}

/// One plain HTTP GET against a published gateway port with the route's
/// host header; the error carries enough context for evidence.
pub async fn gateway_http_get(addr: SocketAddr, host: &str) -> Result<String, String> {
    let request = async {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|error| format!("connect {addr}: {error}"))?;
        stream
            .write_all(
                format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .map_err(|error| format!("write {addr}: {error}"))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .map_err(|error| format!("read {addr}: {error}"))?;
        Ok(response)
    };
    match tokio::time::timeout(HTTP_TIMEOUT, request).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!("http get {addr} timed out")),
    }
}

/// Connects with the exact product option set plus an event capture channel
/// so the test can observe server-side permission violations.
pub async fn connect_with_event_capture(
    config: &NatsConnectConfig,
) -> (
    async_nats::Client,
    tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = authenticated_connect_options(config)
        .event_callback(move |event| {
            let events_tx = events_tx.clone();
            async move {
                events_tx.send(event).ok();
            }
        })
        .connect(config.url.as_str())
        .await
        .expect("authenticated connect with event capture");
    (client, events_rx)
}

/// Waits for the next server-side permission violation on the event
/// channel; `None` when none arrives in budget.
pub async fn next_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) -> Option<String> {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let event = events.recv().await?;
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return Some(message);
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Polls `machine inspect` until the predicate holds, returning the matching
/// snapshot; panics with `what` and the last observation on budget overrun.
pub async fn wait_for_inspect(
    core: &CoreContext,
    machine: &MachineId,
    budget: Duration,
    what: &str,
    predicate: impl Fn(&MachineSnapshot) -> bool,
) -> MachineSnapshot {
    let deadline = Instant::now() + budget;
    let mut last = String::from("<no inspect yet>");
    while Instant::now() < deadline {
        match core
            .api
            .machine_inspect(&MachineInspectRequest {
                machine_id: machine.clone(),
            })
            .await
        {
            Ok(snapshot) => {
                if predicate(&snapshot) {
                    return snapshot;
                }
                last = format!("{snapshot:?}");
            }
            Err(error) => last = format!("{error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("machine {machine:?} {what} within {budget:?}: {last}")
}

/// Waits until the machine snapshot carries machine observations (public ip
/// from the machine process and gateway status from the gateway process) —
/// proof both processes connected with the machine's Machine credential.
pub async fn wait_for_machine_observations(core: &CoreContext, machine: &MachineId) {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(120),
        "never published observations",
        |snapshot| snapshot.public_ip.is_some() && snapshot.gateway.is_some(),
    )
    .await;
}

/// One managed workload container as the inner Docker daemon reports it.
#[derive(Debug)]
pub struct ManagedWorkloadContainer {
    pub id: String,
    pub labels: HashMap<String, String>,
}

/// Lists the running managed workload containers inside one machine's inner
/// Docker daemon (the product's `plz.managed` label schema), with their
/// exact label maps from `docker inspect`.
pub async fn managed_workload_containers(
    core: &CoreContext,
    machine: &DindMachine,
) -> Vec<ManagedWorkloadContainer> {
    let filter = format!("label={MANAGED_LABEL}=true");
    let listed = core
        .exec_on(
            machine,
            &["docker", "ps", "--no-trunc", "--quiet", "--filter", &filter],
        )
        .await;
    assert!(
        listed.success(),
        "inner docker ps on {} failed: {listed:?}",
        machine.name
    );
    let ids: Vec<&str> = listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }

    let mut command = vec![
        "docker",
        "inspect",
        "--format",
        "{{.Id}}\t{{json .Config.Labels}}",
    ];
    command.extend(ids);
    let inspected = core.exec_on(machine, &command).await;
    assert!(
        inspected.success(),
        "inner docker inspect on {} failed: {inspected:?}",
        machine.name
    );
    inspected
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let Some((id, labels_json)) = line.split_once('\t') else {
                panic!(
                    "docker inspect line on {} has no id/labels separator: {line}",
                    machine.name
                );
            };
            let labels: HashMap<String, String> =
                serde_json::from_str(labels_json).unwrap_or_else(|error| {
                    panic!(
                        "docker inspect labels on {} are not a JSON object ({error}): {line}",
                        machine.name
                    )
                });
            ManagedWorkloadContainer {
                id: id.to_owned(),
                labels,
            }
        })
        .collect()
}

#[must_use]
pub fn decode_base64(value: &str) -> String {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .expect("install line CA is valid base64");
    String::from_utf8(bytes).expect("install line CA is UTF-8")
}
