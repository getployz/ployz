//! Local helpers for typed Cloud bootstrap envelopes.

use std::fmt;
use std::path::{Path, PathBuf};

use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};
use ployz_sdk_types::{
    CloudBootstrapCallbackRequest, CloudBootstrapOutcome, CloudBootstrapRedemptionId,
    CloudJoinerBootstrap, CloudJoinerBootstrapResult, MachineJoinRedeemed,
};

pub fn write_cloud_joiner_trusted_ca(
    joiner: &CloudJoinerBootstrap,
    ca_file: &Path,
) -> Result<(), CloudJoinerEnvelopeError> {
    if let Some(parent) = ca_file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CloudJoinerEnvelopeError::CreateTrustDir {
                path: parent.to_path_buf(),
                message: error.to_string(),
            }
        })?;
    }
    std::fs::write(ca_file, joiner.trusted_nats.ca_pem.as_str()).map_err(|error| {
        CloudJoinerEnvelopeError::WriteTrustedCa {
            path: ca_file.to_path_buf(),
            message: error.to_string(),
        }
    })
}

pub fn cloud_joiner_connect_config(
    joiner: &CloudJoinerBootstrap,
    ca_file: PathBuf,
) -> Result<NatsConnectConfig, CloudJoinerEnvelopeError> {
    let url = NatsClientUrl::try_new(joiner.runtime_nats_url.as_str()).map_err(|error| {
        CloudJoinerEnvelopeError::InvalidRuntimeNatsUrl {
            message: format!("{error:?}"),
        }
    })?;
    Ok(NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(joiner.join_secret_delivery.nats_credentials.clone()),
        trust: NatsTlsTrust::ClusterCa(ca_file),
        principal: NatsPrincipal::Join,
    })
}

#[must_use]
pub fn cloud_joiner_success_callback(
    redemption_id: CloudBootstrapRedemptionId,
    redeemed: &MachineJoinRedeemed,
) -> CloudBootstrapCallbackRequest {
    CloudBootstrapCallbackRequest {
        redemption_id,
        outcome: CloudBootstrapOutcome::JoinerSucceeded {
            result: CloudJoinerBootstrapResult {
                operation_id: redeemed.operation_id.clone(),
                machine_id: redeemed.machine_id.clone(),
                name: redeemed.name.clone(),
                last_event_sequence: redeemed.last_event_sequence,
                result: redeemed.result,
            },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudJoinerEnvelopeError {
    InvalidRuntimeNatsUrl { message: String },
    CreateTrustDir { path: PathBuf, message: String },
    WriteTrustedCa { path: PathBuf, message: String },
}

impl fmt::Display for CloudJoinerEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRuntimeNatsUrl { message } => {
                write!(
                    formatter,
                    "cloud joiner runtime NATS URL is invalid: {message}"
                )
            }
            Self::CreateTrustDir { path, message } => write!(
                formatter,
                "failed to create cloud joiner trust directory {}: {message}",
                path.display()
            ),
            Self::WriteTrustedCa { path, message } => write!(
                formatter,
                "failed to write cloud joiner trusted CA {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CloudJoinerEnvelopeError {}

#[cfg(test)]
mod tests {
    use super::{
        cloud_joiner_connect_config, cloud_joiner_success_callback, write_cloud_joiner_trusted_ca,
    };
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::install::{
        AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
        InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::security::NatsPrincipal;
    use ployz_nats::connect::{NatsClientAuth, NatsTlsTrust};
    use ployz_sdk_types::{
        CloudBootstrapRedemptionId, CloudJoinerBootstrap, MachineJoinRedeemResult,
        MachineJoinRedeemed, MachineJoinToken,
    };

    #[test]
    fn cloud_joiner_envelope_writes_trust_and_connects_as_join_principal() {
        let root = std::env::temp_dir().join(format!(
            "ployz-cloud-joiner-envelope-{}",
            std::process::id()
        ));
        let ca_file = root.join("trust/ca.pem");
        let joiner = cloud_joiner_bootstrap();

        write_cloud_joiner_trusted_ca(&joiner, &ca_file).expect("trusted CA writes");
        let config = cloud_joiner_connect_config(&joiner, ca_file.clone()).expect("config builds");

        assert_eq!(
            std::fs::read_to_string(&ca_file).expect("ca reads"),
            joiner.trusted_nats.ca_pem.as_str()
        );
        assert_eq!(config.url.as_str(), "tls://203.0.113.10:4222");
        assert_eq!(config.principal, NatsPrincipal::Join);
        assert_eq!(config.trust, NatsTlsTrust::ClusterCa(ca_file));
        let NatsClientAuth::NkeySeed(seed) = config.auth;
        assert_eq!(
            seed.secret(),
            joiner.join_secret_delivery.nats_credentials.secret()
        );
    }

    #[test]
    fn cloud_joiner_success_callback_omits_join_token_and_nats_seed() {
        let redeemed = machine_join_redeemed();
        let callback = cloud_joiner_success_callback(
            CloudBootstrapRedemptionId::try_new("pcbr_123").expect("valid redemption id"),
            &redeemed,
        );
        let serialized = serde_json::to_string(&callback).expect("callback serializes");

        assert!(serialized.contains("joiner_succeeded"));
        assert!(serialized.contains("op_machine"));
        assert!(serialized.contains("edge_2"));
        assert!(!serialized.contains("join_once_123"));
        assert!(!serialized.contains("SUAAAAAAAA"));
    }

    fn cloud_joiner_bootstrap() -> CloudJoinerBootstrap {
        CloudJoinerBootstrap {
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
            join_secret_delivery: machine_join_secret_delivery(),
        }
    }

    fn machine_join_redeemed() -> MachineJoinRedeemed {
        MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        }
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(
                        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                    )
                    .expect("valid ca pem"),
                },
                ployzd: join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
                ebpf_bytecode: join_artifact(
                    "/tmp/ployz-ebpf-tc",
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                ),
                ebpf_ctl: join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
            },
        }
    }

    fn join_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
        InstallArtifactSpec {
            version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
            source: InstallArtifactSource::try_new(source).expect("valid source"),
            sha256: InstallSha256Digest::try_new(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("valid digest"),
            install_path: AbsoluteInstallPath::try_new(install_path).expect("valid install path"),
        }
    }

    fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
        MachineJoinSecretDelivery {
            nats_credentials: NatsUserSeed::try_new(
                "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .expect("valid nats credentials"),
        }
    }
}
