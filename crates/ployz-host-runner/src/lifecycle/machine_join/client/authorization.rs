use std::time::Duration;

use ployz_core::operation::FailureMessage;
use ployz_nats::connect::{
    NatsConnectConfig, NatsConnectError, connect_authenticated_observing_authorization,
};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinToken,
};

use crate::runtime::{
    DEFAULT_NATS_CONNECT_TIMEOUT, REDEEM_MATERIAL_ATTEMPTS, REDEEM_MATERIAL_RETRY_DELAY,
    failure_message,
};

const AUTHORIZATION_ATTEMPTS: usize = 10;
const AUTHORIZATION_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct JoinAuthorizationRetry {
    pub attempt: usize,
    pub attempts: usize,
}

pub(super) async fn redeem_across_authorization_reload(
    connect: &NatsConnectConfig,
    join_token: MachineJoinToken,
    observe_retry: impl Fn(JoinAuthorizationRetry),
) -> Result<MachineJoinRedeemed, FailureMessage> {
    let mut authorization_attempt = 0;
    let mut material_attempt = 0;

    'authorization: loop {
        authorization_attempt += 1;
        let observed = match connect_authenticated_observing_authorization(
            connect,
            DEFAULT_NATS_CONNECT_TIMEOUT,
        )
        .await
        {
            Ok(observed) => observed,
            Err(error @ NatsConnectError::AuthorizationViolation { .. }) => {
                retry_authorization_rejection(authorization_attempt, &error, &observe_retry)
                    .await?;
                continue;
            }
            Err(error @ (NatsConnectError::Connect { .. } | NatsConnectError::Timeout { .. })) => {
                return Err(failure_message(&error.to_string()));
            }
        };
        let api = OperationApiClient::new(observed.client());

        loop {
            match api
                .machine_join_redeem(&MachineJoinRedeemRequest {
                    join_token: join_token.clone(),
                })
                .await
            {
                Ok(redeemed) => return Ok(redeemed),
                Err(OperationApiClientError::Domain {
                    error: MachineJoinRedeemError::MaterialNotReady { operation_id },
                    ..
                }) => {
                    material_attempt += 1;
                    let not_ready = format!(
                        "operation {} has not reached material-ready",
                        operation_id.as_str()
                    );
                    if material_attempt == REDEEM_MATERIAL_ATTEMPTS {
                        return Err(material_exhausted(&not_ready));
                    }
                    tokio::time::sleep(REDEEM_MATERIAL_RETRY_DELAY).await;
                }
                Err(error @ OperationApiClientError::Request { .. }) => {
                    if observed
                        .authorization_rejected_within(AUTHORIZATION_RETRY_DELAY)
                        .await
                    {
                        let rejection = NatsConnectError::AuthorizationViolation {
                            url: connect.url.as_str().to_owned(),
                        };
                        retry_authorization_rejection(
                            authorization_attempt,
                            &rejection,
                            &observe_retry,
                        )
                        .await?;
                        continue 'authorization;
                    }
                    return Err(redeem_failure(error));
                }
                Err(error @ OperationApiClientError::EncodeRequest { .. })
                | Err(error @ OperationApiClientError::Service { .. })
                | Err(error @ OperationApiClientError::ServiceProtocol { .. })
                | Err(error @ OperationApiClientError::DecodeResponse { .. })
                | Err(error @ OperationApiClientError::Domain { .. }) => {
                    return Err(redeem_failure(error));
                }
            }
        }
    }
}

async fn retry_authorization_rejection(
    attempt: usize,
    error: &NatsConnectError,
    observe_retry: &impl Fn(JoinAuthorizationRetry),
) -> Result<(), FailureMessage> {
    if attempt == AUTHORIZATION_ATTEMPTS {
        return Err(failure_message(&format!(
            "join authorization remained unavailable after {attempt} attempts: {error}"
        )));
    }
    observe_retry(JoinAuthorizationRetry {
        attempt,
        attempts: AUTHORIZATION_ATTEMPTS,
    });
    tokio::time::sleep(AUTHORIZATION_RETRY_DELAY).await;
    Ok(())
}

fn material_exhausted(last_not_ready: &str) -> FailureMessage {
    failure_message(&format!(
        "join material did not become ready within {REDEEM_MATERIAL_ATTEMPTS} attempts: {last_not_ready}"
    ))
}

fn redeem_failure(error: OperationApiClientError<MachineJoinRedeemError>) -> FailureMessage {
    failure_message(&format!("failed to redeem join token: {error}"))
}
