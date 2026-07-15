use std::time::Duration;

use async_nats::connection::State;
use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, MintedNatsUser, NatsUserPublicKey,
};
use ployz_core::operation::{
    CredentialGrantFailure, CredentialGrantOperationState, OperationStatus,
};
use ployz_nats::connect::connect_authenticated;
use ployz_sdk_types::{CredentialAddRequest, CredentialListRequest, CredentialRemoveRequest};
use ployz_test_support::ids::operation_id;
use ployz_test_support::ops::{poll_until, wait_for_terminal_status};

use crate::tests::support::control::TestNats;

#[tokio::test]
async fn operator_can_add_list_rename_and_remove_a_live_credential() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let minted = MintedNatsUser::generate().expect("credential mints");
    let client_config = nats.server().config_with_seed(
        ployz_core::security::NatsPrincipal::Operator,
        minted.seed.clone(),
    );

    api.credential_add(&CredentialAddRequest {
        operation_id: operation_id("op_credential_add"),
        grant: credential(&minted.public, "Nick laptop"),
    })
    .await
    .expect("credential add accepts");
    assert_completed(&api, "op_credential_add").await;

    let added_client = connect_authenticated(&client_config, Duration::from_secs(2))
        .await
        .expect("added credential connects");
    api.credential_add(&CredentialAddRequest {
        operation_id: operation_id("op_credential_rename"),
        grant: credential(&minted.public, "Nick workstation"),
    })
    .await
    .expect("credential rename accepts");
    assert_completed(&api, "op_credential_rename").await;

    let listed = api
        .credential_list(&CredentialListRequest {})
        .await
        .expect("credentials list");
    assert_eq!(
        listed
            .credentials
            .iter()
            .filter(|grant| grant.public_key == minted.public)
            .collect::<Vec<_>>(),
        vec![&credential(&minted.public, "Nick workstation")]
    );

    api.credential_remove(&CredentialRemoveRequest {
        operation_id: operation_id("op_credential_remove"),
        public_key: minted.public.clone(),
    })
    .await
    .expect("credential remove accepts");
    assert_completed(&api, "op_credential_remove").await;

    poll_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        async || (added_client.connection_state() != State::Connected).then_some(()),
    )
    .await
    .expect("removed live credential disconnects");
    assert!(
        connect_authenticated(&client_config, Duration::from_secs(1))
            .await
            .is_err(),
        "removed credential cannot reconnect"
    );
    api.credential_list(&CredentialListRequest {})
        .await
        .expect("remaining Operator still works");

    runtime.shutdown().await.expect("runtime shuts down");
}

#[tokio::test]
async fn final_operator_removal_is_a_typed_terminal_failure() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let founder_public_key = public_key_from_seed(nats.server().user_seed());

    api.credential_remove(&CredentialRemoveRequest {
        operation_id: operation_id("op_remove_final_operator"),
        public_key: founder_public_key,
    })
    .await
    .expect("final removal accepts as operation work");
    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_remove_final_operator"),
        Duration::from_secs(5),
    )
    .await;
    assert!(matches!(
        status,
        OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Failed {
                failure: CredentialGrantFailure::LastOperator,
            },
            ..
        }
    ));

    runtime.shutdown().await.expect("runtime shuts down");
}

fn credential(public_key: &NatsUserPublicKey, name: &str) -> CredentialGrant {
    CredentialGrant {
        public_key: public_key.clone(),
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::Operator,
    }
}

fn public_key_from_seed(seed: &ployz_core::nats_config::NatsUserSeed) -> NatsUserPublicKey {
    let pair = nkeys::KeyPair::from_seed(seed.secret()).expect("fixture seed parses");
    NatsUserPublicKey::try_new(pair.public_key()).expect("fixture public key parses")
}

async fn assert_completed(api: &ployz_nats::operation_api_client::OperationApiClient, id: &str) {
    let status = wait_for_terminal_status(api, &operation_id(id), Duration::from_secs(5)).await;
    assert!(matches!(
        status,
        OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Completed,
            ..
        }
    ));
}
