use super::*;
use futures_util::StreamExt;
use ployz_core::dataplane::WireGuardPublicKey;
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{MachineEndpointObservation, StagedMachineDataplaneState};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::connect::connect_authenticated;
use ployz_sdk_types::MachineJoinToken;
use ployzd::roles::machine::protocol::{
    MachineDataplanePublicKeyRpcOk, MachineDataplanePublicKeyRpcResponse, MachineFactsGetRpcOk,
    MachineFactsGetRpcResponse,
};

pub(super) async fn report_join_completed_with_retry(
    nats: &TestNats,
    machine_id: ployz_core::ids::MachineId,
    machine_seed: NatsUserSeed,
    join_token: MachineJoinToken,
) {
    let config = nats.server().config_with_seed(
        NatsPrincipal::Machine {
            machine_id: machine_id.clone(),
        },
        machine_seed,
    );
    let client = connect_authenticated(&config, Duration::from_secs(5))
        .await
        .expect("joined machine connects");
    start_join_dataplane_responder(client, machine_id).await;
    for _ in 0..10 {
        let join_report_api = OperationApiClient::new(nats.connected.join.clone())
            .with_request_timeout(Duration::from_secs(2));
        match join_report_api
            .machine_join_report(&MachineJoinReportRequest {
                join_token: join_token.clone(),
                outcome: MachineJoinReportOutcome::Completed,
            })
            .await
        {
            Ok(reported) => match reported.outcome {
                ployz_sdk_types::MachineJoinReportedOutcome::Completed => return,
                ployz_sdk_types::MachineJoinReportedOutcome::Failed { failure } => {
                    panic!("join admission failed: {failure:?}")
                }
            },
            Err(OperationApiClientError::Request { .. }) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("join report failed: {error:?}"),
        }
    }
    panic!("join report did not complete");
}

#[tokio::test]
async fn completed_report_can_repair_activation_before_secret_scrub() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_repair_scrub", "idem_repair_scrub", "machine_rs").await;
    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;

    let raw_token = RawJoinToken::try_new(accepted.join_token.as_str()).expect("join token valid");
    let repository = operation_repository_for_test(&nats).await;
    repository
        .stage_machine_dataplane(StagedMachineDataplaneState {
            operation_id: operation_id("op_repair_scrub"),
            machine_id: machine_id("machine_rs"),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                ployz_core::dataplane::default_endpoint_subnet(&machine_id("machine_rs")),
            )
            .expect("endpoint subnet"),
            mesh_endpoints: vec!["192.0.2.10:51820".parse().expect("mesh endpoint")],
            wireguard_public_key: test_wireguard_public_key(&machine_id("machine_rs")),
        })
        .await
        .expect("staged identity survives completed evidence");
    repository
        .record_machine_join_completed(&raw_token)
        .await
        .expect("completion records before activation");
    assert!(
        secret_row_count(&nats, "machine_add_submissions", "op_repair_scrub") > 0,
        "completed evidence keeps token lookup until activation can repair"
    );
    let config = nats.server().config_with_seed(
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_rs"),
        },
        redeemed.secret_delivery.nats_credentials,
    );
    let client = connect_authenticated(&config, Duration::from_secs(5))
        .await
        .expect("joined machine connects");
    start_join_dataplane_responder(client, machine_id("machine_rs")).await;

    join_api
        .machine_join_report(&MachineJoinReportRequest {
            join_token: accepted.join_token.clone(),
            outcome: MachineJoinReportOutcome::Completed,
        })
        .await
        .expect("completed report repairs activation and scrubs");
    assert_eq!(
        secret_row_count(&nats, "machine_add_submissions", "op_repair_scrub"),
        0,
        "successful activation scrubs completed machine-add secrets"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

async fn start_join_dataplane_responder(
    client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) {
    support::dataplane::start_applied_status_responder(
        client.clone(),
        client.clone(),
        machine_id.clone(),
    )
    .await;
    let mut prepare = client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::DataplanePublicKey,
        ))
        .await
        .expect("prepare responder subscribes");
    let mut facts = client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::FactsGet,
        ))
        .await
        .expect("facts responder subscribes");
    client.flush().await.expect("join responders flush");
    let facts_client = client.clone();
    let facts_machine_id = machine_id.clone();
    tokio::spawn(async move {
        while let Some(message) = facts.next().await {
            let Some(reply) = message.reply else { continue };
            let facts = MachineFactsSnapshot::try_new(
                facts_machine_id.clone(),
                MachineContainerObservationSnapshot::try_new(facts_machine_id.clone(), [])
                    .expect("empty observations"),
                Some(MachineEndpointObservation {
                    machine_id: facts_machine_id.clone(),
                    control_endpoints: vec!["192.0.2.10".parse().expect("control endpoint")],
                    mesh_endpoints: vec!["192.0.2.10:51820".parse().expect("mesh endpoint")],
                }),
                ployz_test_support::fixtures::test_disk_space(),
                ployz_core::image::OciPlatform::current(),
                1,
            )
            .expect("machine facts");
            let response = MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk { facts });
            let payload = serde_json::to_vec(&response).expect("facts response serializes");
            let _ = facts_client.publish(reply, payload.into()).await;
        }
    });
    let prepare_client = client.clone();
    let prepare_machine_id = machine_id.clone();
    tokio::spawn(async move {
        while let Some(message) = prepare.next().await {
            let Some(reply) = message.reply else { continue };
            let response =
                MachineDataplanePublicKeyRpcResponse::Ok(MachineDataplanePublicKeyRpcOk {
                    machine_id: prepare_machine_id.clone(),
                    public_key: test_wireguard_public_key(&prepare_machine_id),
                });
            let payload = serde_json::to_vec(&response).expect("prepare response serializes");
            let _ = prepare_client.publish(reply, payload.into()).await;
        }
    });
}

pub(super) fn test_wireguard_public_key(
    machine_id: &ployz_core::ids::MachineId,
) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(format!("test-public-key-{}", machine_id.as_str()))
        .expect("test public key")
}
