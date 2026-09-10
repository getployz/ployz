//! The shared client publishes saved identity before Join and retries network work.
use super::*;
use ployz::enrollment::{join_enrollment, observe_enrollment};
use ployz_core::{AdvertisedEndpoint, RegisterRequest, WireGuardPublicKey};

#[tokio::test]
async fn saved_assignment_is_published_before_join_and_retried_after_lost_response() {
    let trace = Arc::new(Mutex::new(EnrollmentTrace::default()));
    let mut entry_service = DiscoveryService::new(test_description());
    entry_service.enrollment = Some(trace.clone());
    let entry_errors = entry_service.clone();
    let mut joining_description = test_description();
    joining_description.machine_id = MachineId::random();
    let joining_id = joining_description.machine_id;
    let mut joining_service = DiscoveryService::new(joining_description);
    joining_service.enrollment = Some(trace.clone());
    let (mut entry, entry_server, _) = connected_client(entry_service).await;
    let (mut joining, joining_server, _) = connected_client(joining_service).await;
    let snapshot = observe_enrollment(&mut entry).await.unwrap();
    let request = RegisterRequest {
        machine_id: joining_id,
        assigned_subnet: None,
        initial_policy: Default::default(),
        name: "joiner".parse().unwrap(),
        storage: ployz_core::StorageChoice::None,
        public_key: WireGuardPublicKey([0; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: ployz_core::MachineRuntime {
            daemon_version: "1.2.3".into(),
            hostname: "joining-host".into(),
            ..Default::default()
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let assignment =
        ployz::enrollment::local::save_assignment(dir.path(), &request, &snapshot).unwrap();
    // A process can acquire the stable lock before any network operation starts.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(lock);
    assert!(
        join_enrollment(&mut entry, &mut joining, &assignment, None, None)
            .await
            .is_err()
    );
    let fresh = observe_enrollment(&mut entry).await.unwrap();
    let reopened = ployz::enrollment::local::save_assignment(dir.path(), &request, &fresh).unwrap();
    assert_eq!(reopened, assignment);
    assert!(
        join_enrollment(&mut entry, &mut joining, &reopened, None, None)
            .await
            .unwrap()
            .already_accepted
    );
    {
        let trace = trace.lock().unwrap();
        assert_eq!(trace.events, ["publish", "join", "publish", "join"]);
        assert_eq!(
            trace.published.as_ref().unwrap().assigned_machine,
            assignment.machine
        );
        assert_eq!(
            trace.joined.as_ref().unwrap().registration.assigned_machine,
            assignment.machine
        );
    }
    entry_errors.set_register_error(RpcError {
        code: RpcErrorCode::Conflict,
        message: "assignment conflicts".into(),
        details: Value::Null,
    });
    assert!(
        join_enrollment(&mut entry, &mut joining, &assignment, None, None)
            .await
            .is_err()
    );
    assert_eq!(
        trace.lock().unwrap().events.len(),
        4,
        "failed publication must not call Join"
    );
    entry_server.abort();
    joining_server.abort();
}
