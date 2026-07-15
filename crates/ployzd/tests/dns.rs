//! Black-box DNS role process and machine-scoped service behavior.

use ployz_core::ids::MachineId;
use ployz_core::internal_dns::{InternalDnsResolverStatus, InternalDnsStatus};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;
use ployz_test_support::ids::machine_id;
use ployzd::roles::dns::process::start_dns_process_with_client;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct DnsStatusResponse {
    machine_id: MachineId,
    value: InternalDnsStatus,
}

#[tokio::test]
async fn dns_process_serves_machine_scoped_status_and_shuts_down() {
    let dns_machine_id = machine_id("dns_a");
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[dns_machine_id.clone()]).await;
    let machine_client = nats.machine_client(&dns_machine_id).await;
    let process = start_dns_process_with_client(
        machine_client.clone(),
        dns_machine_id.clone(),
        Duration::from_millis(25),
        None,
        None,
    )
    .await
    .expect("DNS process starts");

    let response = request_json::<_, DnsStatusResponse>(
        &nats.controller,
        machine_service(&dns_machine_id, MachineServiceEndpoint::DnsStatus),
        &serde_json::json!({}),
        Duration::from_secs(1),
    )
    .await
    .expect("DNS status request succeeds");

    assert_eq!(response.machine_id, dns_machine_id);
    assert_eq!(
        response.value.resolver,
        InternalDnsResolverStatus::NotConfigured
    );
    assert!(response.value.fact_watermarks.is_empty());

    process.shutdown().await.expect("DNS process shuts down");
}
