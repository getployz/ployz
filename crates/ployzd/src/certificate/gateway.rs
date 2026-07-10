use std::time::Duration;

use ployz_core::cert::{
    AcmeHttp01Challenge, CertificateArtifactKind, CertificateArtifactPushRequest,
    CertificateArtifactPushResponse, CertificateChallengeApplicationStatus,
    CertificateChallengeStatusRequest, CertificateChallengeStatusResponse,
    CertificateProvisionFailure, CustomCertBundle,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_rpc::MachineRpcResponse;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;

const GATEWAY_RPC_TIMEOUT: Duration = Duration::from_secs(2);
const CHALLENGE_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub(super) struct GatewayCertificateClient {
    client: async_nats::Client,
}

impl GatewayCertificateClient {
    pub(super) fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    pub(super) async fn push_bundle(
        &self,
        operation_id: &OperationId,
        machine_ids: &[MachineId],
        bundle: &CustomCertBundle,
    ) -> Result<(), CertificateProvisionFailure> {
        let Some(first_machine_id) = machine_ids.first() else {
            return Err(CertificateProvisionFailure::DnsPreflight {
                message: failure_message("no intent-known gateway accepts certificate artifacts"),
            });
        };
        let (expected_digest, _) = bundle
            .active_cert()
            .bundle_ref
            .artifact_parts()
            .map_err(|error| push_failure(first_machine_id, error))?;
        let expected_size = u64::try_from(bundle.material_bytes().len())
            .map_err(|error| push_failure(first_machine_id, error))?;
        let request = CertificateArtifactPushRequest {
            operation_id: operation_id.clone(),
            artifact_kind: CertificateArtifactKind::CustomTlsBundle,
            bundle: bundle.clone(),
            expected_digest: expected_digest.clone(),
            expected_size,
        };

        for machine_id in machine_ids {
            let subject =
                machine_service(machine_id, MachineServiceEndpoint::CertificateArtifactPush);
            let response = request_json::<_, CertificateArtifactPushResponse>(
                &self.client,
                subject,
                &request,
                GATEWAY_RPC_TIMEOUT,
            )
            .await
            .map_err(|error| push_failure(machine_id, error))?;
            let ok = match response {
                MachineRpcResponse::Ok(ok) => ok,
                MachineRpcResponse::DomainError {
                    machine_id: response_machine_id,
                    error,
                } => {
                    return Err(CertificateProvisionFailure::GatewayArtifactPush {
                        machine_id: machine_id.clone(),
                        message: failure_message(format!(
                            "gateway {} rejected certificate artifact: {error:?}",
                            response_machine_id.as_str()
                        )),
                    });
                }
            };
            if ok.machine_id != *machine_id
                || ok.cert_id != bundle.active_cert().cert_id
                || ok.digest != expected_digest
            {
                return Err(CertificateProvisionFailure::GatewayArtifactPush {
                    machine_id: machine_id.clone(),
                    message: failure_message(
                        "gateway certificate artifact acknowledgment mismatch",
                    ),
                });
            }
        }
        Ok(())
    }

    pub(super) async fn wait_until_challenge_applied(
        &self,
        challenge: &AcmeHttp01Challenge,
        machine_ids: &[MachineId],
        timeout: Duration,
    ) -> Result<(), CertificateProvisionFailure> {
        let request = CertificateChallengeStatusRequest {
            challenge: challenge.clone(),
        };
        let mut missing = machine_ids.to_vec();
        let wait = async {
            loop {
                let mut round_missing = Vec::new();
                for machine_id in machine_ids {
                    if !self.challenge_is_applied(machine_id, &request).await {
                        round_missing.push(machine_id.clone());
                    }
                }
                if round_missing.is_empty() {
                    return;
                }
                missing = round_missing;
                tokio::time::sleep(CHALLENGE_POLL_INTERVAL).await;
            }
        };
        if tokio::time::timeout(timeout, wait).await.is_err() {
            return Err(CertificateProvisionFailure::ChallengeReadiness {
                missing_machine_ids: missing,
            });
        }
        Ok(())
    }

    async fn challenge_is_applied(
        &self,
        machine_id: &MachineId,
        request: &CertificateChallengeStatusRequest,
    ) -> bool {
        let subject = machine_service(
            machine_id,
            MachineServiceEndpoint::CertificateChallengeStatus,
        );
        let Ok(response) = request_json::<_, CertificateChallengeStatusResponse>(
            &self.client,
            subject,
            request,
            GATEWAY_RPC_TIMEOUT,
        )
        .await
        else {
            return false;
        };
        matches!(
            response,
            MachineRpcResponse::Ok(ok)
                if ok.machine_id == *machine_id
                    && ok.application == CertificateChallengeApplicationStatus::Applied
        )
    }
}

fn push_failure(
    machine_id: &MachineId,
    error: impl std::fmt::Display,
) -> CertificateProvisionFailure {
    CertificateProvisionFailure::GatewayArtifactPush {
        machine_id: machine_id.clone(),
        message: failure_message(error.to_string()),
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    let rendered = format!("certificate gateway RPC: {}", message.into());
    let Ok(message) = FailureMessage::try_new(rendered) else {
        unreachable!("prefixed gateway certificate failure is non-empty");
    };
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue};
    use ployz_core::ops::RouteHostname;

    #[tokio::test]
    async fn silence_keeps_the_known_gateway_in_the_readiness_failure() {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let client = GatewayCertificateClient::new(nats.controller);
        let machine_id = MachineId::try_new("machine_silent").expect("machine id");
        let challenge = AcmeHttp01Challenge::try_new(
            RouteHostname::try_new("example.com").expect("hostname"),
            AcmeChallengeToken::try_new("token").expect("token"),
            AcmeChallengeValue::try_new("token.thumbprint").expect("value"),
            AcmeChallengeTtlSeconds::try_new(300).expect("ttl"),
        )
        .expect("challenge");

        let error = client
            .wait_until_challenge_applied(
                &challenge,
                std::slice::from_ref(&machine_id),
                Duration::from_millis(10),
            )
            .await
            .expect_err("silent gateway is not ready");

        assert_eq!(
            error,
            CertificateProvisionFailure::ChallengeReadiness {
                missing_machine_ids: vec![machine_id],
            }
        );
    }
}
