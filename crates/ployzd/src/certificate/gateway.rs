use std::time::Duration;

use futures_util::future::join_all;
use ployz_core::cert::{
    AcmeHttp01Challenge, CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateChallengeApplyRequest, CertificateChallengeApplyResponse,
    CertificateChallengeRemoveRequest, CertificateChallengeRemoveResponse,
    CertificateProvisionFailure, CustomCertBundle,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_rpc::MachineRpcResponse;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;

const GATEWAY_RPC_TIMEOUT: Duration = Duration::from_secs(2);

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
            bundle: bundle.clone(),
            expected_digest: expected_digest.clone(),
            expected_size,
        };

        join_all(machine_ids.iter().map(|machine_id| async {
            self.push_bundle_to_gateway(machine_id, bundle, &expected_digest, &request)
                .await
        }))
        .await
        .into_iter()
        .collect::<Result<(), _>>()?;
        Ok(())
    }

    async fn push_bundle_to_gateway(
        &self,
        machine_id: &MachineId,
        bundle: &CustomCertBundle,
        expected_digest: &ployz_core::install::InstallSha256Digest,
        request: &CertificateArtifactPushRequest,
    ) -> Result<(), CertificateProvisionFailure> {
        let subject = machine_service(machine_id, MachineServiceEndpoint::CertificateArtifactPush);
        let response = request_json::<_, CertificateArtifactPushResponse>(
            &self.client,
            subject,
            request,
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
            || ok.digest != *expected_digest
        {
            return Err(CertificateProvisionFailure::GatewayArtifactPush {
                machine_id: machine_id.clone(),
                message: failure_message("gateway certificate artifact acknowledgment mismatch"),
            });
        }
        Ok(())
    }

    pub(super) async fn apply_challenge(
        &self,
        operation_id: &OperationId,
        challenge: &AcmeHttp01Challenge,
        machine_ids: &[MachineId],
    ) -> Result<(), GatewayChallengeReadinessError> {
        let request = CertificateChallengeApplyRequest {
            operation_id: operation_id.clone(),
            challenge: challenge.clone(),
        };
        let missing_machine_ids = join_all(machine_ids.iter().map(|machine_id| async {
            let subject = machine_service(
                machine_id,
                MachineServiceEndpoint::CertificateChallengeApply,
            );
            let applied = request_json::<_, CertificateChallengeApplyResponse>(
                &self.client,
                subject,
                &request,
                GATEWAY_RPC_TIMEOUT,
            )
            .await
            .is_ok_and(|response| {
                matches!(response, MachineRpcResponse::Ok(ok) if ok.machine_id == *machine_id)
            });
            (!applied).then(|| machine_id.clone())
        }))
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if missing_machine_ids.is_empty() {
            Ok(())
        } else {
            Err(GatewayChallengeReadinessError {
                missing_machine_ids,
            })
        }
    }

    pub(super) async fn remove_challenge(
        &self,
        operation_id: &OperationId,
        challenge: &AcmeHttp01Challenge,
        machine_ids: &[MachineId],
    ) -> Vec<MachineId> {
        let request = CertificateChallengeRemoveRequest {
            operation_id: operation_id.clone(),
            challenge: challenge.clone(),
        };
        join_all(machine_ids.iter().map(|machine_id| async {
            let subject = machine_service(
                machine_id,
                MachineServiceEndpoint::CertificateChallengeRemove,
            );
            let removed = request_json::<_, CertificateChallengeRemoveResponse>(
                &self.client,
                subject,
                &request,
                GATEWAY_RPC_TIMEOUT,
            )
            .await
            .is_ok_and(|response| {
                matches!(response, MachineRpcResponse::Ok(ok) if ok.machine_id == *machine_id)
            });
            (!removed).then(|| machine_id.clone())
        }))
        .await
        .into_iter()
        .flatten()
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HTTP-01 challenge is not applied on gateways {missing_machine_ids:?}")]
pub(super) struct GatewayChallengeReadinessError {
    pub(super) missing_machine_ids: Vec<MachineId>,
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
    use ployz_core::cert::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CertificateArtifactPushOk,
        CustomCertBundle, GatewayCertificateRpcError, custom_bundle_digest,
    };
    use ployz_core::ops::RouteHostname;
    use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::service_catalog::{gateway_role_service, machine_endpoint_spec};

    #[tokio::test]
    async fn artifact_push_attempts_every_known_gateway_before_returning_failure() {
        let rejected_id = MachineId::try_new("machine_rejected").expect("machine id");
        let accepted_id = MachineId::try_new("machine_accepted").expect("machine id");
        let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
            rejected_id.clone(),
            accepted_id.clone(),
        ])
        .await;
        let bundle = custom_bundle();
        let (digest, _) = bundle
            .active_cert()
            .bundle_ref
            .artifact_parts()
            .expect("bundle reference");
        let accepted_called = Arc::new(AtomicBool::new(false));
        let mut rejected = start_nats_service(
            nats.machine_client(&rejected_id).await,
            &gateway_role_service(&rejected_id),
        )
        .await
        .expect("rejected gateway service");
        rejected
            .bind_endpoint(
                &machine_endpoint_spec(
                    &rejected_id,
                    MachineServiceEndpoint::CertificateArtifactPush,
                ),
                {
                    let rejected_id = rejected_id.clone();
                    move |_| {
                        let rejected_id = rejected_id.clone();
                        async move {
                            NatsServiceResponse::json_domain_error(
                                &CertificateArtifactPushResponse::DomainError {
                                    machine_id: rejected_id,
                                    error: GatewayCertificateRpcError::InvalidRequest {
                                        message: FailureMessage::try_new("rejected artifact")
                                            .expect("failure message"),
                                    },
                                },
                            )
                        }
                    }
                },
            )
            .await
            .expect("rejected endpoint");
        let mut accepted = start_nats_service(
            nats.machine_client(&accepted_id).await,
            &gateway_role_service(&accepted_id),
        )
        .await
        .expect("accepted gateway service");
        accepted
            .bind_endpoint(
                &machine_endpoint_spec(
                    &accepted_id,
                    MachineServiceEndpoint::CertificateArtifactPush,
                ),
                {
                    let accepted_id = accepted_id.clone();
                    let accepted_called = Arc::clone(&accepted_called);
                    let cert_id = bundle.active_cert().cert_id.clone();
                    let digest = digest.clone();
                    move |_| {
                        accepted_called.store(true, Ordering::SeqCst);
                        let response =
                            CertificateArtifactPushResponse::Ok(CertificateArtifactPushOk {
                                machine_id: accepted_id.clone(),
                                cert_id: cert_id.clone(),
                                digest: digest.clone(),
                            });
                        async move { NatsServiceResponse::json_ok(&response) }
                    }
                },
            )
            .await
            .expect("accepted endpoint");

        let error = GatewayCertificateClient::new(nats.controller)
            .push_bundle(
                &OperationId::try_new("op_cert").expect("operation id"),
                &[rejected_id.clone(), accepted_id],
                &bundle,
            )
            .await
            .expect_err("one gateway rejects the artifact");

        assert!(matches!(
            (error, accepted_called.load(Ordering::SeqCst)),
            (
                CertificateProvisionFailure::GatewayArtifactPush { machine_id, .. },
                true,
            ) if machine_id == rejected_id
        ));
        rejected.shutdown().await.expect("service shutdown");
        accepted.shutdown().await.expect("service shutdown");
    }

    fn custom_bundle() -> CustomCertBundle {
        let certificate = "certificate";
        let private_key = "private key";
        let digest = custom_bundle_digest(certificate, private_key).expect("digest");
        CustomCertBundle::try_new(
            ActiveCertState {
                cert_id: ployz_core::ids::CertId::try_new("cert_example").expect("cert id"),
                hostname: RouteHostname::try_new("example.com").expect("hostname"),
                bundle_ref: CertBundleRef::try_new(format!(
                    "sha256:{}:/var/lib/ployz/certificates/example.bundle",
                    digest.as_str()
                ))
                .expect("bundle reference"),
                validity: CertValidityWindow::try_new(
                    CertValidAt::try_new(1).expect("not before"),
                    CertValidAt::try_new(2).expect("not after"),
                )
                .expect("validity"),
            },
            certificate.to_owned(),
            private_key.to_owned(),
        )
        .expect("custom bundle")
    }
}
