use super::*;
use crate::control::operations::deploy::PreStartHookRuntimeError;

#[tokio::test]
async fn machine_role_service_reports_hook_create_failure() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_create_failure("disk full"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect_err("hook create failure is returned");

    assert_eq!(
        error,
        PreStartHookRuntimeError::CreateFailed {
            machine_id: machine_id("machine_a"),
            message: failure_message("hook container create failed: disk full"),
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_hook_start_failure_with_container_evidence() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_start_failure("ctr_hook", "exec format error"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect_err("hook start failure is returned");

    assert_eq!(
        error,
        PreStartHookRuntimeError::StartFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            message: failure_message("hook container start failed: exec format error"),
            inspect_hint: inspect_hint("ctr_hook"),
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_hook_wait_failure_with_log_evidence() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_next_container("ctr_hook")
            .with_wait_failure("runtime disconnected"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect_err("hook wait failure is returned");

    assert_eq!(
        error,
        PreStartHookRuntimeError::WaitFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            message: failure_message("hook container wait failed: runtime disconnected"),
            log_hint: log_hint("ctr_hook"),
        }
    );
}

#[tokio::test]
async fn machine_role_service_stops_hook_after_timeout() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_next_container("ctr_hook")
            .with_pending_wait(),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);
    let mut request = hook_request();
    request.timeout_millis = 0;
    let target_machine_id = machine_id("machine_a");
    let run =
        tokio::spawn(async move { client.run_pre_start_hook(&target_machine_id, request).await });
    state.wait_started().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::time::resume();
    let error = run
        .await
        .expect("hook request task completes")
        .expect_err("hook timeout is returned");

    assert_eq!(
        error,
        PreStartHookRuntimeError::TimedOut {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            timeout_millis: 0,
            message: failure_message("hook timed out after 1ms and was stopped"),
            inspect_hint: inspect_hint("ctr_hook"),
        }
    );
    assert_eq!(state.stops(), vec![container_id("ctr_hook")]);
}

#[tokio::test]
async fn machine_role_service_reports_failed_stop_after_hook_timeout() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_next_container("ctr_hook")
            .with_pending_wait()
            .with_stop_failure("ctr_hook", "runtime busy"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);
    let mut request = hook_request();
    request.timeout_millis = 10;
    let target_machine_id = machine_id("machine_a");
    let run =
        tokio::spawn(async move { client.run_pre_start_hook(&target_machine_id, request).await });
    state.wait_started().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::time::resume();
    let error = run
        .await
        .expect("hook request task completes")
        .expect_err("hook timeout is returned");

    assert_eq!(
        error,
        PreStartHookRuntimeError::TimedOut {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            timeout_millis: 10,
            message: failure_message(
                "hook timed out after 10ms and could not be stopped: runtime busy"
            ),
            inspect_hint: inspect_hint("ctr_hook"),
        }
    );
    assert!(state.stops().is_empty());
}

fn log_hint(container_id: &str) -> ployz_core::operation::OperatorHint {
    ployz_core::operation::OperatorHint::try_new(format!("ployzctl logs {container_id}"))
        .expect("valid log hint")
}
