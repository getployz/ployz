//! Production assembly of the transport-neutral Host Runner founding sequence.

use std::env;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use ployz_core::corrosion::{CorrosionTimestamp, MachineTransport};
use ployz_core::founding::{
    FoundingDriverEnrollment, FoundingRefusal, FoundingResult, ValidatedFoundingRequest,
};
use ployz_core::ids::PeerId;
use ployz_core::machine::MachineName;
use ployz_core::network::WireGuardPublicKey;
use ployz_core::operation::FailureMessage;
use ployz_host_runner::lifecycle::founding::{
    FoundingDriverInput, FoundingFailure, FoundingPreparationError, FoundingProgressObserver,
    FoundingStateDirectory, LinuxFoundingInput, LinuxFoundingPreflight, found_machine_one,
    found_machine_one_with_progress, inspect_linux_founding, prepare_linux_founding,
};
use ployz_host_runner::{
    HostRunnerCommandRunner as _, ReleaseManifest, ReleasePlatform, SystemHostRunnerCommandRunner,
    default_release_manifest_url, persisted_release_manifest_url, read_release_manifest_text,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::commands::{DriverPeerArgs, InitCommand};
use crate::init::cloud::{CloudEnvelope, CloudProgress};
use crate::init::http::{BootstrapSecret, HttpFoundingControlPlane};

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

    let state = open_or_initialize_state()?;
    let canonical_resume = match inspect_linux_founding(&state, &mut runner)? {
        LinuxFoundingPreflight::NoOp { canonical_request } => {
            let request = canonical_request.request();
            return Ok(OnHostSuccess {
                result: FoundingResult::NoOp,
                cluster_name: request.cluster.name.clone(),
                machine_name: request.machine.name.as_str().to_owned(),
                storage: request.machine.storage,
            });
        }
        LinuxFoundingPreflight::Refused(refusal) => {
            return Err(OnHostInitError::Refused(refusal));
        }
        LinuxFoundingPreflight::Clean => None,
        LinuxFoundingPreflight::Resume { canonical_request } => canonical_request,
    };
    let cloud = command
        .cloud_token
        .as_ref()
        .map(CloudEnvelope::decode)
        .transpose()?;
    if let Some(canonical) = canonical_resume.as_ref() {
        validate_resume_driver(canonical, command.driver_peer.as_ref(), cloud.as_ref())?;
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
    let driver = driver_input(command.driver_peer.as_ref(), cloud.as_ref())?;
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
                        machine_id: machine_id.clone(),
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
    })
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
        error @ FoundingPreparationError::Failed { .. } => OnHostInitError::Preparation(error),
    }
}

fn validate_resume_driver(
    canonical: &ValidatedFoundingRequest,
    ssh: Option<&DriverPeerArgs>,
    cloud: Option<&CloudEnvelope>,
) -> Result<(), OnHostInitError> {
    validate_resume_driver_enrollment(&canonical.request().driver, ssh, cloud)
}

fn validate_resume_driver_enrollment(
    canonical: &FoundingDriverEnrollment,
    ssh: Option<&DriverPeerArgs>,
    cloud: Option<&CloudEnvelope>,
) -> Result<(), OnHostInitError> {
    // A partial Cloud founding always needs its original token. Otherwise the
    // no-op progress observer could durably advance callback milestones
    // without notifying Cloud.
    use ployz_core::corrosion::PeerTransport;

    match (canonical, ssh, cloud) {
        (FoundingDriverEnrollment::Cloud { .. }, None, None) => Err(OnHostInitError::Input(
            "partial Cloud founding requires the original matching --cloud-token".to_owned(),
        )),
        (FoundingDriverEnrollment::Cloud { peer_id, document }, None, Some(envelope)) => {
            let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
                return Err(OnHostInitError::Input(
                    "canonical Cloud peer is not WireGuard".to_owned(),
                ));
            };
            if *peer_id == envelope.peer_id
                && document.name == envelope.peer_name
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
        (FoundingDriverEnrollment::Cloud { .. }, Some(_), None | Some(_))
        | (FoundingDriverEnrollment::OnHost, _, Some(_))
        | (FoundingDriverEnrollment::Ssh { .. }, _, Some(_)) => Err(OnHostInitError::Input(
            "Cloud token does not match the canonical founding driver".to_owned(),
        )),
        (FoundingDriverEnrollment::OnHost, None, None)
        | (FoundingDriverEnrollment::Ssh { .. }, None, None) => Ok(()),
        (FoundingDriverEnrollment::Ssh { peer_id, document }, Some(peer), None) => {
            let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
                return Err(OnHostInitError::Input(
                    "canonical SSH peer is not WireGuard".to_owned(),
                ));
            };
            if peer_id.as_str() == peer.id
                && document.name == peer.name
                && pubkey.as_str() == peer.public_key
            {
                Ok(())
            } else {
                Err(OnHostInitError::Input(
                    "SSH enrollment disagrees with the canonical founding request".to_owned(),
                ))
            }
        }
        (FoundingDriverEnrollment::OnHost, Some(_), None) => Err(OnHostInitError::Input(
            "SSH enrollment does not match the canonical on-host founding driver".to_owned(),
        )),
    }
}

struct CloudProgressReporter<'a> {
    envelope: &'a CloudEnvelope,
    cluster_id: ployz_core::ids::ClusterId,
    machine_id: ployz_core::ids::MachineRowId,
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
    ssh: Option<&DriverPeerArgs>,
    cloud: Option<&CloudEnvelope>,
) -> Result<FoundingDriverInput, OnHostInitError> {
    match (ssh, cloud) {
        (None, None) => Ok(FoundingDriverInput::OnHost),
        (Some(peer), None) => Ok(FoundingDriverInput::Ssh {
            peer_id: PeerId::try_new(peer.id.clone())
                .map_err(|error| OnHostInitError::Input(error.to_string()))?,
            name: peer.name.clone(),
            public_key: WireGuardPublicKey::try_new(peer.public_key.clone())
                .map_err(|error| OnHostInitError::Input(error.to_string()))?,
            endpoint: None,
        }),
        (None, Some(cloud)) => Ok(FoundingDriverInput::Cloud {
            peer_id: cloud.peer_id.clone(),
            name: cloud.peer_name.clone(),
            public_key: cloud.public_key.clone(),
            endpoint: None,
        }),
        (Some(_), Some(_)) => Err(OnHostInitError::Input(
            "Cloud and SSH driver enrollment cannot be combined".to_owned(),
        )),
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
    use ployz_core::ids::{ClusterId, MachineRowId};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use crate::commands::CloudToken;

    const CLOUD_PUBLIC_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    fn cloud_driver() -> (FoundingDriverEnrollment, CloudEnvelope) {
        let cluster_id = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("machine id");
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id");
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
                name: "Ployz Cloud".to_owned(),
                transport: PeerTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &public_key)
                        .bind_address()
                        .get(),
                    pubkey: public_key.clone(),
                    endpoint: None,
                },
            },
        };
        let envelope = cloud_envelope("https://cloud.example/bootstrap", &peer_id, &public_key);
        (driver, envelope)
    }

    fn cloud_envelope(
        callback_url: &str,
        peer_id: &PeerId,
        public_key: &WireGuardPublicKey,
    ) -> CloudEnvelope {
        let wire = serde_json::json!({
            "callback_url": callback_url,
            "callback_token": "callback-secret",
            "peer_id": peer_id.as_str(),
            "peer_name": "Ployz Cloud",
            "public_key": public_key.as_str(),
        });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&wire).expect("Cloud envelope JSON"));
        let token: CloudToken = format!("pcbs2_{payload}").parse().expect("Cloud token");
        CloudEnvelope::decode(&token).expect("Cloud envelope")
    }

    #[test]
    fn partial_cloud_resume_requires_the_original_matching_token() {
        let (driver, envelope) = cloud_driver();
        let error = validate_resume_driver_enrollment(&driver, None, None)
            .expect_err("Cloud resume without callback authority must stop");
        assert!(
            error
                .to_string()
                .contains("original matching --cloud-token")
        );
        validate_resume_driver_enrollment(&driver, None, Some(&envelope))
            .expect("matching Cloud token resumes");
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
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id");
        let public_key = WireGuardPublicKey::try_new(CLOUD_PUBLIC_KEY).expect("Cloud public key");
        let envelope = cloud_envelope(
            &format!("http://{address}/bootstrap"),
            &peer_id,
            &public_key,
        );
        let mut reporter = CloudProgressReporter {
            envelope: &envelope,
            cluster_id: ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id"),
            machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("machine id"),
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
