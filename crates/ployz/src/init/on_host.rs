//! Production assembly of the transport-neutral Host Runner founding sequence.

use std::env;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use ployz_core::corrosion::{CorrosionTimestamp, MachineTransport};
use ployz_core::founding::{
    FoundingDriverEnrollment, FoundingRefusal, FoundingResult, ValidatedFoundingRequest,
};
use ployz_core::machine::MachineName;
use ployz_core::network::WireGuardPublicKey;
use ployz_core::operation::FailureMessage;
use ployz_host_runner::lifecycle::founding::{
    FoundingDriverInput, FoundingPreparationError, FoundingStateDirectory, LinuxFoundingInput,
    LinuxFoundingPreflight, inspect_linux_founding, prepare_linux_founding,
};
use ployz_host_runner::{
    HostRunnerCommandRunner as _, ReleaseManifest, ReleasePlatform, SystemHostRunnerCommandRunner,
    default_release_manifest_url, persisted_release_manifest_url, read_release_manifest_text,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::commands::{InitCommand, InitDriver};
use crate::init::cloud::{CloudEnvelope, CloudProgress};
use crate::init::http::{BootstrapSecret, HttpFoundingControlPlane};
use crate::init::orchestration::{
    FoundingFailure, FoundingProgressObserver, found_machine_one, found_machine_one_with_progress,
};

const RELEASE_ENV_PATH: &str = "/etc/ployz/release.env";
const FOUNDING_STATE_PATH: &str = "/var/lib/ployz";
const API_PORT: u16 = 2_020;
const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";

pub async fn execute(command: InitCommand) -> Result<OnHostSuccess, OnHostInitError> {
    let mut runner = SystemHostRunnerCommandRunner::default();
    if !runner.is_linux() {
        return Err(OnHostInitError::LinuxRequired);
    }
    let uid = runner
        .current_uid()
        .map_err(|error| OnHostInitError::Host(error.to_string()))?;
    if uid != 0 {
        return Err(OnHostInitError::RootRequired);
    }

    let cloud = match &command.driver {
        InitDriver::Cloud(token) => Some(CloudEnvelope::decode(token)?),
        InitDriver::OnHost | InitDriver::SshTarget(_) | InitDriver::SshPeer(_) => None,
    };
    let state = open_or_initialize_state()?;
    let canonical_resume = match inspect_linux_founding(&state, &mut runner)? {
        LinuxFoundingPreflight::NoOp { canonical_request } => {
            validate_resume_driver(&canonical_request, &command.driver, cloud.as_ref())?;
            let request = canonical_request.request();
            return Ok(OnHostSuccess {
                result: FoundingResult::NoOp,
                cluster_name: request.cluster.name.clone(),
                machine_name: request.machine.name.as_str().to_owned(),
                storage: request.machine.storage,
                cluster_id: request.cluster_id.clone(),
                provider: request.cluster.provider,
                machine_transport: request.machine.transport.clone(),
            });
        }
        LinuxFoundingPreflight::Refused {
            refusal,
            cluster_id,
        } => {
            return report_preflight_refusal(&command.driver, cluster_id.as_ref(), refusal).await;
        }
        LinuxFoundingPreflight::Clean => None,
        LinuxFoundingPreflight::Resume { canonical_request } => canonical_request,
    };
    if let Some(canonical) = canonical_resume.as_ref() {
        validate_resume_driver(canonical, &command.driver, cloud.as_ref())?;
    }
    let hostname = host_name()?;
    let machine_name = MachineName::try_new(
        command
            .machine_name
            .clone()
            .unwrap_or_else(|| hostname.clone()),
    )
    .map_err(|error| OnHostInitError::Input(error.to_string()))?;
    let cluster_name = command
        .cluster_name
        .clone()
        .unwrap_or_else(|| hostname.clone());
    if cluster_name.trim().is_empty() {
        return Err(OnHostInitError::Input(
            "cluster name must not be empty".to_owned(),
        ));
    }
    let driver = driver_input(&command.driver, cloud.as_ref())?;
    let written_at = CorrosionTimestamp::try_new(
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| OnHostInitError::Input(error.to_string()))?,
    )
    .map_err(|error| OnHostInitError::Input(error.to_string()))?;
    let input = LinuxFoundingInput {
        cluster_name: cluster_name.clone(),
        machine_name: machine_name.clone(),
        endpoint: command.wireguard_endpoint,
        prefix: command.container_network,
        hostname_mode: command.service_urls,
        storage: command.storage,
        driver,
        written_at,
        acme_directory_url: DEFAULT_ACME_DIRECTORY_URL.to_owned(),
        acme_contact: None,
    };
    let manifest_url = release_manifest_url();
    let manifest_text =
        read_release_manifest_text(&manifest_url).map_err(OnHostInitError::Release)?;
    let manifest = ReleaseManifest::parse(&manifest_text)
        .map_err(|error| OnHostInitError::Release(error.to_string()))?;
    let platform = ReleasePlatform::from_target(env::consts::OS, env::consts::ARCH)
        .map_err(OnHostInitError::Release)?;
    let artifacts = manifest
        .install_artifacts_for(platform)
        .map_err(OnHostInitError::Release)?;
    let corrosion_version = manifest.corrosion_embedded_version().to_owned();
    let mut prepared = prepare_linux_founding(&state, input, artifacts, corrosion_version, runner)
        .map_err(map_preparation)?;
    let request = prepared.request.request();
    let MachineTransport::Wireguard {
        addr_v6,
        pubkey: machine_public_key,
        ..
    } = &request.machine.transport
    else {
        return Err(OnHostInitError::Host(
            "prepared machine-one transport is not WireGuard".to_owned(),
        ));
    };
    let listen_addr = SocketAddr::new(IpAddr::V6(*addr_v6), API_PORT);
    let bootstrap_secret = BootstrapSecret::new(prepared.bootstrap_credential.as_str())?;
    let mut control_plane =
        HttpFoundingControlPlane::new(listen_addr, listen_addr.ip(), bootstrap_secret)?;
    let cluster_id = request.cluster_id.clone();
    let machine_id = request.machine_id.clone();
    let machine_public_key = machine_public_key.clone();
    let storage = request.machine.storage;
    let output_cluster_name = request.cluster.name.clone();
    let output_machine_name = request.machine.name.as_str().to_owned();
    let output_provider = request.cluster.provider;
    let output_machine_transport = request.machine.transport.clone();
    let founding = if let Some(cloud) = &cloud {
        let mut progress = CloudProgressReporter {
            envelope: cloud,
            cluster_id: cluster_id.clone(),
            machine_id: machine_id.clone(),
            machine_public_key: machine_public_key.clone(),
        };
        found_machine_one_with_progress(
            &state,
            &prepared.request,
            &mut prepared.effects,
            &mut control_plane,
            &mut progress,
        )
        .await
    } else {
        found_machine_one(
            &state,
            &prepared.request,
            &mut prepared.effects,
            &mut control_plane,
        )
        .await
    };
    let result = match founding {
        Ok(result) => result,
        Err(error) => {
            if let Some(cloud) = &cloud {
                let repair_command = match &error {
                    FoundingFailure::Refused(FoundingRefusal::ForeignState {
                        repair_command,
                        ..
                    })
                    | FoundingFailure::Refused(FoundingRefusal::IncompleteDoorMaterial {
                        repair_command,
                    }) => Some(repair_command.as_str().to_owned()),
                    FoundingFailure::Refused(FoundingRefusal::InvalidRequest { .. })
                    | FoundingFailure::State { .. }
                    | FoundingFailure::Host { .. }
                    | FoundingFailure::ControlPlane { .. }
                    | FoundingFailure::Progress { .. } => None,
                };
                let _ = cloud
                    .report(CloudProgress::Failed {
                        cluster_id: cluster_id.clone(),
                        machine_id: Some(machine_id.clone()),
                        reason: error.to_string(),
                        repair_command,
                    })
                    .await;
            }
            return Err(OnHostInitError::Founding(error));
        }
    };
    Ok(OnHostSuccess {
        result,
        cluster_name: output_cluster_name,
        machine_name: output_machine_name,
        storage,
        cluster_id,
        provider: output_provider,
        machine_transport: output_machine_transport,
    })
}

async fn report_preflight_refusal(
    driver: &InitDriver,
    cluster_id: Option<&ployz_core::ids::ClusterName>,
    refusal: FoundingRefusal,
) -> Result<OnHostSuccess, OnHostInitError> {
    if let InitDriver::Cloud(token) = driver {
        let failure = match (&refusal, cluster_id) {
            (
                FoundingRefusal::ForeignState {
                    requested_cluster_id,
                    found_cluster_id,
                    repair_command,
                },
                _,
            ) => Some((
                requested_cluster_id.clone(),
                format!("machine contains state from cluster {found_cluster_id}"),
                repair_command,
            )),
            (FoundingRefusal::IncompleteDoorMaterial { repair_command }, Some(cluster_id)) => {
                Some((
                    cluster_id.clone(),
                    "machine door material is incomplete".to_owned(),
                    repair_command,
                ))
            }
            (FoundingRefusal::InvalidRequest { .. }, _)
            | (FoundingRefusal::IncompleteDoorMaterial { .. }, None) => None,
        };
        let Some((cluster_id, reason, repair_command)) = failure else {
            return Err(OnHostInitError::Refused(refusal));
        };
        let cloud = CloudEnvelope::decode(token)?;
        cloud
            .report(CloudProgress::Failed {
                cluster_id,
                machine_id: None,
                reason,
                repair_command: Some(repair_command.as_str().to_owned()),
            })
            .await?;
    }
    Err(OnHostInitError::Refused(refusal))
}

fn open_or_initialize_state() -> Result<FoundingStateDirectory, OnHostInitError> {
    let path = Path::new(FOUNDING_STATE_PATH);
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => FoundingStateDirectory::open(path)
            .map_err(|error| OnHostInitError::Host(error.to_string())),
        Ok(_) => Err(OnHostInitError::Host(format!(
            "founding state path {FOUNDING_STATE_PATH} is not a directory"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            FoundingStateDirectory::initialize(path)
                .map_err(|error| OnHostInitError::Host(error.to_string()))
        }
        Err(error) => Err(OnHostInitError::Host(format!(
            "could not inspect founding state path {FOUNDING_STATE_PATH}: {error}"
        ))),
    }
}

fn map_preparation(error: FoundingPreparationError) -> OnHostInitError {
    match error {
        FoundingPreparationError::Refused(refusal) => OnHostInitError::Refused(refusal),
        FoundingPreparationError::Storage(reason) => OnHostInitError::Storage(reason),
        error @ (FoundingPreparationError::Failed { .. }
        | FoundingPreparationError::MultipleImportedZfsPools { .. }
        | FoundingPreparationError::RetainedZfsPoolNotImported { .. }) => {
            OnHostInitError::Preparation(error)
        }
    }
}

fn validate_resume_driver(
    canonical: &ValidatedFoundingRequest,
    driver: &InitDriver,
    cloud: Option<&CloudEnvelope>,
) -> Result<(), OnHostInitError> {
    validate_resume_driver_enrollment(&canonical.request().driver, driver, cloud)
}

fn validate_resume_driver_enrollment(
    canonical: &FoundingDriverEnrollment,
    driver: &InitDriver,
    cloud: Option<&CloudEnvelope>,
) -> Result<(), OnHostInitError> {
    // A partial Cloud founding always needs its original token. Otherwise the
    // no-op progress observer could durably advance callback milestones
    // without notifying Cloud.
    use ployz_core::corrosion::PeerTransport;

    match (canonical, driver, cloud) {
        (FoundingDriverEnrollment::Cloud { .. }, InitDriver::OnHost, None) => {
            Err(OnHostInitError::Input(
                "partial Cloud founding requires the original matching --cloud-token".to_owned(),
            ))
        }
        (
            FoundingDriverEnrollment::Cloud { peer_id, document },
            InitDriver::Cloud(_),
            Some(envelope),
        ) => {
            let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
                return Err(OnHostInitError::Input(
                    "canonical Cloud peer is not WireGuard".to_owned(),
                ));
            };
            if *peer_id == envelope.peer_id
                && document.name == envelope.peer_id
                && *pubkey == envelope.public_key
            {
                Ok(())
            } else {
                Err(OnHostInitError::Input(
                    "Cloud token enrollment disagrees with the canonical founding request"
                        .to_owned(),
                ))
            }
        }
        (FoundingDriverEnrollment::Cloud { .. }, InitDriver::SshPeer(_), None)
        | (FoundingDriverEnrollment::OnHost, InitDriver::Cloud(_), Some(_))
        | (FoundingDriverEnrollment::Ssh { .. }, InitDriver::Cloud(_), Some(_)) => {
            Err(OnHostInitError::Input(
                "Cloud token does not match the canonical founding driver".to_owned(),
            ))
        }
        (FoundingDriverEnrollment::OnHost, InitDriver::OnHost, None)
        | (FoundingDriverEnrollment::Ssh { .. }, InitDriver::OnHost, None) => Ok(()),
        (FoundingDriverEnrollment::Ssh { peer_id, document }, InitDriver::SshPeer(peer), None) => {
            let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
                return Err(OnHostInitError::Input(
                    "canonical SSH peer is not WireGuard".to_owned(),
                ));
            };
            if *peer_id == peer.id && document.name == peer.id && *pubkey == peer.public_key {
                Ok(())
            } else {
                Err(OnHostInitError::Input(
                    "SSH enrollment disagrees with the canonical founding request".to_owned(),
                ))
            }
        }
        (FoundingDriverEnrollment::OnHost, InitDriver::SshPeer(_), None) => {
            Err(OnHostInitError::Input(
                "SSH enrollment does not match the canonical on-host founding driver".to_owned(),
            ))
        }
        (_, InitDriver::SshTarget(_), _) | (_, InitDriver::Cloud(_), None) | (_, _, Some(_)) => {
            Err(OnHostInitError::Input(
                "init driver does not match the canonical founding driver".to_owned(),
            ))
        }
    }
}

struct CloudProgressReporter<'a> {
    envelope: &'a CloudEnvelope,
    cluster_id: ployz_core::ids::ClusterName,
    machine_id: ployz_core::ids::MachineName,
    machine_public_key: WireGuardPublicKey,
}

impl FoundingProgressObserver for CloudProgressReporter<'_> {
    async fn driver_enrolled(
        &mut self,
        _driver: &ployz_core::founding::FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage> {
        self.envelope
            .report(CloudProgress::Enrolled {
                cluster_id: self.cluster_id.clone(),
                machine_id: self.machine_id.clone(),
                machine_public_key: self.machine_public_key.clone(),
            })
            .await
            .map_err(|error| {
                FailureMessage::try_new(error.to_string())
                    .expect("Cloud progress failures are nonempty")
            })
    }

    async fn ready(
        &mut self,
        _driver: &ployz_core::founding::FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage> {
        self.envelope
            .report(CloudProgress::Ready {
                cluster_id: self.cluster_id.clone(),
                machine_id: self.machine_id.clone(),
                machine_public_key: self.machine_public_key.clone(),
            })
            .await
            .map_err(|error| {
                FailureMessage::try_new(error.to_string())
                    .expect("Cloud progress failures are nonempty")
            })
    }
}

fn driver_input(
    driver: &InitDriver,
    cloud: Option<&CloudEnvelope>,
) -> Result<FoundingDriverInput, OnHostInitError> {
    match (driver, cloud) {
        (InitDriver::OnHost, None) => Ok(FoundingDriverInput::OnHost),
        (InitDriver::SshPeer(peer), None) => Ok(FoundingDriverInput::Ssh {
            peer_id: peer.id.clone(),
            public_key: peer.public_key.clone(),
        }),
        (InitDriver::Cloud(_), Some(cloud)) => Ok(FoundingDriverInput::Cloud {
            peer_id: cloud.peer_id.clone(),
            public_key: cloud.public_key.clone(),
        }),
        (InitDriver::SshTarget(_), None)
        | (InitDriver::Cloud(_), None)
        | (InitDriver::OnHost | InitDriver::SshPeer(_) | InitDriver::SshTarget(_), Some(_)) => Err(
            OnHostInitError::Input("init driver is not valid for on-host founding".to_owned()),
        ),
    }
}

fn release_manifest_url() -> String {
    env::var("PLOYZ_RELEASE_MANIFEST_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| persisted_release_manifest_url(Path::new(RELEASE_ENV_PATH)).ok())
        .unwrap_or_else(default_release_manifest_url)
}

fn host_name() -> Result<String, OnHostInitError> {
    let raw = std::fs::read_to_string("/etc/hostname")
        .map_err(|error| OnHostInitError::Host(format!("could not read /etc/hostname: {error}")))?;
    let short = raw.trim().split('.').next().unwrap_or_default();
    if short.is_empty() {
        return Err(OnHostInitError::Host(
            "machine hostname is empty".to_owned(),
        ));
    }
    Ok(short.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnHostSuccess {
    pub result: FoundingResult,
    pub cluster_name: String,
    pub machine_name: String,
    pub storage: ployz_core::corrosion::MachineStorageSelection,
    pub cluster_id: ployz_core::ids::ClusterName,
    pub provider: ployz_core::corrosion::MeshProvider,
    pub machine_transport: ployz_core::corrosion::MachineTransport,
}

#[derive(Debug, thiserror::Error)]
pub enum OnHostInitError {
    #[error("ployz init requires a Linux machine")]
    LinuxRequired,
    #[error("ployz init must run as root; use: sudo ployz init")]
    RootRequired,
    #[error("invalid init input: {0}")]
    Input(String),
    #[error("release channel failed: {0}")]
    Release(String),
    #[error("machine-one host preparation failed: {0}")]
    Preparation(#[from] ployz_host_runner::lifecycle::founding::FoundingPreparationError),
    #[error(transparent)]
    Http(#[from] crate::init::http::HttpFoundingError),
    #[error(transparent)]
    Cloud(#[from] crate::init::cloud::CloudError),
    #[error(transparent)]
    Founding(#[from] FoundingFailure),
    #[error("Init refused: {0}")]
    Storage(ployz_core::founding::InitStorageSelectionError),
    #[error("founding refused: {0:?}")]
    Refused(FoundingRefusal),
    #[error("machine-one host failed: {0}")]
    Host(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine as _;
    use ployz_core::corrosion::{
        CorrosionDocumentVersion, OperatorWriteProvenance, PeerDocument, PeerTransport,
        derive_builtin_wireguard_member,
    };
    use ployz_core::ids::{ClusterName, MachineName, PeerName};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use crate::commands::CloudToken;

    const CLOUD_PUBLIC_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    fn cloud_driver() -> (FoundingDriverEnrollment, CloudToken, CloudEnvelope) {
        let cluster_id = ClusterName::try_new("main").expect("cluster id");
        let machine_id = MachineName::try_new("node-one").expect("machine id");
        let peer_id = PeerName::try_new("ployz-cloud").expect("peer id");
        let public_key = WireGuardPublicKey::try_new(CLOUD_PUBLIC_KEY).expect("Cloud public key");
        let provenance = OperatorWriteProvenance {
            written_by: ployz_core::corrosion::OperationInitiator::Machine { machine_id },
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp"),
        };
        let driver = FoundingDriverEnrollment::Cloud {
            peer_id: peer_id.clone(),
            document: PeerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance,
                name: PeerName::try_new("ployz-cloud").expect("peer name"),
                transport: PeerTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &public_key)
                        .bind_address()
                        .get(),
                    pubkey: public_key.clone(),
                    endpoint: None,
                },
            },
        };
        let token = cloud_token("https://cloud.example/bootstrap", &peer_id, &public_key);
        let envelope = CloudEnvelope::decode(&token).expect("Cloud envelope");
        (driver, token, envelope)
    }

    fn cloud_token(
        callback_url: &str,
        peer_id: &PeerName,
        public_key: &WireGuardPublicKey,
    ) -> CloudToken {
        let wire = serde_json::json!({
            "callback_url": callback_url,
            "callback_token": "callback-secret",
            "peer_id": peer_id.as_str(),
            "peer_name": "Ployz Cloud",
            "public_key": public_key.as_str(),
        });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&wire).expect("Cloud envelope JSON"));
        format!("pcbs2_{payload}").parse().expect("Cloud token")
    }

    #[test]
    fn partial_cloud_resume_requires_the_original_matching_token() {
        let (canonical, token, envelope) = cloud_driver();
        let error = validate_resume_driver_enrollment(&canonical, &InitDriver::OnHost, None)
            .expect_err("Cloud resume without callback authority must stop");
        assert!(
            error
                .to_string()
                .contains("original matching --cloud-token")
        );
        validate_resume_driver_enrollment(&canonical, &InitDriver::Cloud(token), Some(&envelope))
            .expect("matching Cloud token resumes");
    }

    #[test]
    fn incomplete_door_preparation_is_preserved_as_a_typed_refusal() {
        let refusal = FoundingRefusal::IncompleteDoorMaterial {
            repair_command: ployz_core::founding::FoundingRepairCommand::ResetMachine,
        };

        assert!(matches!(
            map_preparation(FoundingPreparationError::Refused(refusal.clone())),
            OnHostInitError::Refused(found) if found == refusal
        ));
    }

    #[tokio::test]
    async fn incomplete_door_preflight_routes_through_the_typed_on_host_refusal() {
        let refusal = FoundingRefusal::IncompleteDoorMaterial {
            repair_command: ployz_core::founding::FoundingRepairCommand::ResetMachine,
        };

        assert!(matches!(
            report_preflight_refusal(&InitDriver::OnHost, None, refusal.clone()).await,
            Err(OnHostInitError::Refused(found)) if found == refusal
        ));
    }

    #[tokio::test]
    async fn ready_observer_sends_the_ready_progress_update() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("callback listener");
        let address = listener.local_addr().expect("callback address");
        let callback = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("callback accept");
            let request = read_http_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .expect("callback response");
            request
        });
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id");
        let public_key = WireGuardPublicKey::try_new(CLOUD_PUBLIC_KEY).expect("Cloud public key");
        let token = cloud_token(
            &format!("http://{address}/bootstrap"),
            &peer_id,
            &public_key,
        );
        let envelope = CloudEnvelope::decode(&token).expect("Cloud envelope");
        let mut reporter = CloudProgressReporter {
            envelope: &envelope,
            cluster_id: ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id"),
            machine_id: MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("machine id"),
            machine_public_key: WireGuardPublicKey::try_new(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )
            .expect("machine public key"),
        };
        reporter
            .ready(&FoundingDriverEnrollment::OnHost)
            .await
            .expect("Ready callback succeeds");
        let request = callback.await.expect("callback task");
        let body = request.split_once("\r\n\r\n").expect("HTTP request body").1;
        assert!(body.contains("\"state\":\"ready\""));
        assert!(!body.contains("\"state\":\"enrolled\""));
    }

    #[tokio::test]
    async fn foreign_cloud_preflight_reports_failed_before_returning_refusal() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("callback listener");
        let address = listener.local_addr().expect("callback address");
        let callback = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("callback accept");
            let request = read_http_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .expect("callback response");
            request
        });
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id");
        let public_key = WireGuardPublicKey::try_new(CLOUD_PUBLIC_KEY).expect("public key");
        let token = cloud_token(
            &format!("http://{address}/bootstrap"),
            &peer_id,
            &public_key,
        );
        let requested_cluster_id =
            ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("requested cluster");
        let found_cluster_id =
            ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("found cluster");
        let refusal = FoundingRefusal::ForeignState {
            requested_cluster_id: requested_cluster_id.clone(),
            found_cluster_id: found_cluster_id.clone(),
            repair_command: ployz_core::founding::FoundingRepairCommand::ResetMachine,
        };
        let error = report_preflight_refusal(&InitDriver::Cloud(token), None, refusal.clone())
            .await
            .expect_err("foreign state remains a refusal");
        assert!(matches!(error, OnHostInitError::Refused(found) if found == refusal));

        let request = callback.await.expect("callback task");
        let (headers, body) = request.split_once("\r\n\r\n").expect("HTTP request body");
        assert!(headers.contains(&format!(
            "idempotency-key: ployz-init-{requested_cluster_id}-preflight-failed"
        )));
        assert!(body.contains("\"state\":\"failed\""));
        assert!(body.contains(&format!("\"cluster_id\":\"{requested_cluster_id}\"")));
        assert!(body.contains(&found_cluster_id.to_string()));
        assert!(body.contains("\"repair_command\":\"ployz machine reset\""));
        assert!(!body.contains("machine_id"));
        assert!(!body.contains("callback-secret"));
    }

    #[tokio::test]
    async fn incomplete_door_cloud_preflight_reports_failed_before_returning_refusal() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("callback listener");
        let address = listener.local_addr().expect("callback address");
        let callback = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("callback accept");
            let request = read_http_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .expect("callback response");
            request
        });
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id");
        let public_key = WireGuardPublicKey::try_new(CLOUD_PUBLIC_KEY).expect("public key");
        let token = cloud_token(
            &format!("http://{address}/bootstrap"),
            &peer_id,
            &public_key,
        );
        let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let refusal = FoundingRefusal::IncompleteDoorMaterial {
            repair_command: ployz_core::founding::FoundingRepairCommand::ResetMachine,
        };
        let error = report_preflight_refusal(
            &InitDriver::Cloud(token),
            Some(&cluster_id),
            refusal.clone(),
        )
        .await
        .expect_err("incomplete door material remains a refusal");
        assert!(matches!(error, OnHostInitError::Refused(found) if found == refusal));

        let request = callback.await.expect("callback task");
        let (headers, body) = request.split_once("\r\n\r\n").expect("HTTP request body");
        assert!(headers.contains(&format!(
            "idempotency-key: ployz-init-{cluster_id}-preflight-failed"
        )));
        assert!(body.contains("\"state\":\"failed\""));
        assert!(body.contains(&format!("\"cluster_id\":\"{cluster_id}\"")));
        assert!(body.contains("door material is incomplete"));
        assert!(body.contains("\"repair_command\":\"ployz machine reset\""));
        assert!(!body.contains("machine_id"));
        assert!(!body.contains("callback-secret"));
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4_096];
        loop {
            let count = stream.read(&mut chunk).await.expect("callback request");
            assert_ne!(count, 0, "callback request ended before its body");
            let bytes = chunk.get(..count).expect("read count fits its buffer");
            request.extend_from_slice(bytes);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let header_bytes = request
                .get(..header_end)
                .expect("header boundary fits the request");
            let headers = String::from_utf8_lossy(header_bytes);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("callback Content-Length");
            if request.len() >= header_end + 4 + content_length {
                return String::from_utf8(request).expect("UTF-8 callback request");
            }
        }
    }
}
