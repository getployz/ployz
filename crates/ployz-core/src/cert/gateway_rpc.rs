//! Machine-scoped gateway RPC contracts for custom certificates.

use serde::{Deserialize, Serialize};

use crate::ids::{CertId, MachineId, OperationId};
use crate::install::InstallSha256Digest;
use crate::machine_rpc::{MachineRpcResponder, MachineRpcResponse};
use crate::ops::FailureMessage;

use super::{AcmeHttp01Challenge, CustomCertBundle};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateArtifactPushRequest {
    pub operation_id: OperationId,
    pub bundle: CustomCertBundle,
    pub expected_digest: InstallSha256Digest,
    pub expected_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateArtifactPushOk {
    pub machine_id: MachineId,
    pub cert_id: CertId,
    pub digest: InstallSha256Digest,
}

impl MachineRpcResponder for CertificateArtifactPushOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type CertificateArtifactPushResponse =
    MachineRpcResponse<CertificateArtifactPushOk, GatewayCertificateRpcError>;

/// Removes one exact certificate artifact from a gateway.
///
/// Both identity fields are required so a delayed cleanup request cannot
/// remove newer material for the same certificate id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateArtifactRemoveRequest {
    pub operation_id: OperationId,
    pub cert_id: CertId,
    pub digest: InstallSha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateArtifactRemoveOk {
    pub machine_id: MachineId,
    pub cert_id: CertId,
    pub digest: InstallSha256Digest,
}

impl MachineRpcResponder for CertificateArtifactRemoveOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type CertificateArtifactRemoveResponse =
    MachineRpcResponse<CertificateArtifactRemoveOk, GatewayCertificateRpcError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateChallengeApplyRequest {
    pub operation_id: OperationId,
    pub challenge: AcmeHttp01Challenge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateChallengeRemoveRequest {
    pub operation_id: OperationId,
    pub challenge: AcmeHttp01Challenge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateChallengeMutationOk {
    pub machine_id: MachineId,
}

impl MachineRpcResponder for CertificateChallengeMutationOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id } = self;
        machine_id
    }
}

pub type CertificateChallengeApplyResponse =
    MachineRpcResponse<CertificateChallengeMutationOk, GatewayCertificateRpcError>;
pub type CertificateChallengeRemoveResponse =
    MachineRpcResponse<CertificateChallengeMutationOk, GatewayCertificateRpcError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateChallengeStatusRequest {
    pub challenge: AcmeHttp01Challenge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateChallengeApplicationStatus {
    Applied,
    NotApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateChallengeStatusOk {
    pub machine_id: MachineId,
    pub application: CertificateChallengeApplicationStatus,
}

impl MachineRpcResponder for CertificateChallengeStatusOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type CertificateChallengeStatusResponse =
    MachineRpcResponse<CertificateChallengeStatusOk, GatewayCertificateRpcError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayCertificateRpcError {
    InvalidRequest { message: FailureMessage },
    ArtifactStoreFailed { message: FailureMessage },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{
        AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, ActiveCertState,
        CertBundleRef, CertValidAt, CertValidityWindow, custom_bundle_digest,
    };
    use crate::install::AbsoluteInstallPath;
    use crate::ops::RouteHostname;

    #[test]
    fn artifact_push_request_roundtrips_validated_bundle() {
        let bundle = bundle();
        let (expected_digest, _) = bundle
            .active_cert()
            .bundle_ref
            .artifact_parts()
            .expect("bundle reference");
        let request = CertificateArtifactPushRequest {
            operation_id: OperationId::try_new("op_cert").expect("operation id"),
            expected_digest,
            expected_size: u64::try_from(bundle.material_bytes().len()).expect("bundle size"),
            bundle,
        };
        let encoded = serde_json::to_value(&request).expect("serialize request");
        let decoded: CertificateArtifactPushRequest =
            serde_json::from_value(encoded).expect("deserialize request");

        assert_eq!(decoded, request);
        assert_eq!(
            decoded.bundle.active_cert().cert_id.as_str(),
            "cert_example"
        );
    }

    #[test]
    fn challenge_status_request_preserves_exact_challenge() {
        let request = CertificateChallengeStatusRequest {
            challenge: super::super::AcmeHttp01Challenge::try_new(
                RouteHostname::try_new("example.com").expect("hostname"),
                AcmeChallengeToken::try_new("token_1").expect("token"),
                AcmeChallengeValue::try_new("token_1.thumbprint").expect("value"),
                AcmeChallengeTtlSeconds::try_new(300).expect("ttl"),
            )
            .expect("challenge"),
        };

        assert_eq!(
            serde_json::from_value::<CertificateChallengeStatusRequest>(
                serde_json::to_value(&request).expect("serialize request")
            )
            .expect("deserialize request"),
            request
        );
    }

    #[test]
    fn artifact_remove_request_requires_cert_id_and_digest() {
        let request = CertificateArtifactRemoveRequest {
            operation_id: OperationId::try_new("op_remove_cert").expect("operation id"),
            cert_id: CertId::try_new("cert_example").expect("cert id"),
            digest: InstallSha256Digest::try_new("a".repeat(64)).expect("digest"),
        };

        assert_eq!(
            serde_json::from_value::<CertificateArtifactRemoveRequest>(
                serde_json::to_value(&request).expect("serialize request")
            )
            .expect("deserialize request"),
            request
        );
    }

    #[test]
    fn challenge_status_response_does_not_collide_with_rpc_status_tag() {
        let response = CertificateChallengeStatusResponse::Ok(CertificateChallengeStatusOk {
            machine_id: MachineId::try_new("machine_gateway").expect("machine id"),
            application: CertificateChallengeApplicationStatus::Applied,
        });
        let encoded = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            serde_json::from_value::<CertificateChallengeStatusResponse>(encoded)
                .expect("deserialize response"),
            response
        );
    }

    fn bundle() -> CustomCertBundle {
        let certificate_chain_pem = "certificate";
        let private_key_pem = "private-key";
        let digest = custom_bundle_digest(certificate_chain_pem, private_key_pem).expect("digest");
        let path = AbsoluteInstallPath::try_new("/var/lib/ployz/certs/example.bundle")
            .expect("absolute path");
        CustomCertBundle::try_new(
            ActiveCertState {
                cert_id: CertId::try_new("cert_example").expect("cert id"),
                hostname: RouteHostname::try_new("example.com").expect("hostname"),
                bundle_ref: CertBundleRef::for_bundle(&digest, &path).expect("bundle ref"),
                validity: CertValidityWindow::try_new(
                    CertValidAt::try_new(1_000).expect("not before"),
                    CertValidAt::try_new(2_000).expect("not after"),
                )
                .expect("validity"),
            },
            certificate_chain_pem.to_owned(),
            private_key_pem.to_owned(),
        )
        .expect("bundle")
    }
}
