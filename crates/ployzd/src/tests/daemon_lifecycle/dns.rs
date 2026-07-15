//! Black-box DNS role process and machine-scoped service behavior.

use crate::roles::dns::process::start_dns_process_with_client;
use ployz_core::ids::MachineId;
use ployz_core::network::internal_dns::{
    InternalDnsIntentRefreshHealth, InternalDnsIntentWatchHealth, InternalDnsResolverStatus,
    InternalDnsStatus,
};
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
use ployz_test_support::ids::machine_id;
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
    let nats = ployz_test_support::nats::TestNats::start_with_machines(std::slice::from_ref(
        &dns_machine_id,
    ))
    .await;
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

    let response = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let response = request_json::<_, DnsStatusResponse>(
                &nats.controller,
                machine_service(&dns_machine_id, MachineServiceEndpoint::DnsStatus),
                &serde_json::json!({}),
                Duration::from_secs(1),
            )
            .await
            .expect("DNS status request succeeds");
            if matches!(
                &response.value.intent_health.refresh,
                InternalDnsIntentRefreshHealth::RequestFailed { .. }
            ) && response.value.intent_health.watch == InternalDnsIntentWatchHealth::Watching
            {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("DNS intent refresh failure becomes visible");

    assert_eq!(response.machine_id, dns_machine_id);
    assert_eq!(
        response.value.resolver,
        InternalDnsResolverStatus::NotConfigured
    );
    assert!(response.value.fact_watermarks.is_empty());
    assert!(matches!(
        &response.value.intent_health.refresh,
        InternalDnsIntentRefreshHealth::RequestFailed { .. }
    ));
    assert_eq!(
        response.value.intent_health.watch,
        InternalDnsIntentWatchHealth::Watching
    );

    process.shutdown().await.expect("DNS process shuts down");
}
