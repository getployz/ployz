//! Machine command execution, mesh HTTP, and human presentation.

use std::cmp::Ordering;
use std::future::Future;
use std::time::{Duration, Instant};

use ployz_core::corrosion::MachineTransport;
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::ClusterName;
use ployz_core::install::{ExactPloyzVersion, InstallArtifactVersion};
use ployz_core::join::{
    JoinStorageChoice, MachineEndpointSetRefusal, MachineEndpointSetReply,
    MachineEndpointSetRequest,
};
use ployz_core::machine::MachineName;
use ployz_core::operation::FailureMessage;
use ployz_core::{
    LensCollection, LensSnapshot, MachineLensRow, MachineRemoveRefusal, MachineRemoveReply,
    MachineRemoveRequest, MachineStatusLensRow, MachineUpgradeRefusal, MachineUpgradeReply,
    MachineUpgradeRequest,
};
use ployz_core::{MACHINE_ENDPOINT_ROUTE_PREFIX, MACHINE_REMOVE_ROUTE, MACHINE_UPGRADE_ROUTE};
use ployz_host_runner::lifecycle::machine_join::{
    MachineJoinFailure, MachineJoinOutcomeKind, run_linux_machine_join,
};
use ployz_host_runner::lifecycle::machine_reset::run_linux_machine_reset;
use ployz_host_runner::{
    ReleaseManifest, ReleasePlatform, read_release_manifest_text, release_manifest_url_for_platform,
};

use crate::JoinDoorClient;
use crate::commands::{
    MachineCommand, MachineEndpointSetCommand, MachineJoinCommand, MachineListCommand,
    MachineRemoveCommand, MachineUpgradeCommand, MachineUpgradeSelector, MachineUpgradeSource,
};
use crate::mesh::http::JsonReply;
use crate::remote::{MachineRemoteTargetError, OperatorRemote, OperatorRemoteError};

const DEFAULT_RELEASE_CHANNEL_URL: &str = "https://ployz.sh/channels/alpha.env";
const UPGRADE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(8);
const UPGRADE_CONFIRM_RETRY: Duration = Duration::from_millis(500);

pub async fn execute(command: MachineCommand) -> Result<String, MachineExecutionError> {
    match command {
        MachineCommand::List(command) => list(command).await,
        MachineCommand::Remove(command) => remove(command).await,
        MachineCommand::EndpointSet(command) => endpoint_set(command).await,
        MachineCommand::Upgrade(command) => upgrade(command).await,
        MachineCommand::Join(command) => join(command).await,
        MachineCommand::Reset => reset(),
    }
}

async fn upgrade(command: MachineUpgradeCommand) -> Result<String, MachineExecutionError> {
    let source = resolve_upgrade_source(&command.source).await?;
    if matches!(command.selector, MachineUpgradeSelector::Outdated)
        && matches!(source, UpgradeSource::Manual(_))
    {
        return Err(MachineExecutionError::ManualOutdatedSelector);
    }

    let remote = OperatorRemote::load(command.target.as_ref())?;
    let machines_snapshot = remote.lens(LensCollection::Machines).await?;
    validate_snapshot_cluster(&machines_snapshot, remote.cluster_id())?;
    let statuses_snapshot = remote.lens(LensCollection::MachineStatus).await?;
    let (machines, statuses) = upgrade_lenses(&machines_snapshot, &statuses_snapshot)?;
    let target_version = source.version();
    let selected = select_upgrade_machines(&command.selector, machines, statuses, target_version)?;
    if selected.is_empty() {
        return Ok(format!(
            "No machines are behind {}.\n",
            target_version.unwrap_or("the manually supplied artifact")
        ));
    }

    let mut cached_artifacts = Vec::new();
    let mut progress = UpgradeProgress::new(std::io::stdout());
    for (confirmed, machine) in selected.into_iter().enumerate() {
        let request =
            resolve_machine_upgrade_request(&source, machine.status, &mut cached_artifacts).await?;
        let name = machine.row.document.name.as_str();
        let short_sha256 = &request.sha256.as_str()[..12];
        progress.before_mutation(&format!(
            "{name}  target {} ({short_sha256}...)\n",
            request.version.as_str()
        ))?;

        let direct = remote
            .for_machine(machine.row)
            .map_err(MachineExecutionError::Target)?;
        let reply = direct
            .request_json_with_refusal::<_, MachineUpgradeReply, MachineUpgradeRefusal>(
                hyper::Method::POST,
                MACHINE_UPGRADE_ROUTE,
                Some(&request),
            )
            .await;
        let reply = match reply {
            Ok(JsonReply::Success(reply)) => reply,
            Ok(JsonReply::Refused(refusal)) => {
                return Err(upgrade_halted(
                    name,
                    confirmed,
                    format_upgrade_refusal(&refusal),
                ));
            }
            Err(error) => {
                return Err(upgrade_halted(name, confirmed, error.to_string()));
            }
        };
        if reply.version != request.version || reply.sha256 != request.sha256 {
            return Err(upgrade_halted(
                name,
                confirmed,
                "the API acknowledgement did not match the resolved artifact".to_owned(),
            ));
        }
        match source.confirmation() {
            UpgradeConfirmation::ReleaseVersion => {
                complete_release_upgrade(&mut progress, name, &request.version, confirmed, || {
                    confirm_upgrade(&direct, &machine.row.id, &request.version)
                })
                .await?;
            }
            UpgradeConfirmation::UnavailableForManualArtifact => {
                progress.after_mutation(&render_upgrade_staged(name));
                return Err(upgrade_halted(
                    name,
                    confirmed,
                    "swap armed; a manually supplied artifact has no release version to confirm. Check `ployz machine ls` before targeting another machine"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(String::new())
}

struct UpgradeProgress<W> {
    writer: W,
    deferred_output: Option<std::io::Error>,
}

impl<W: std::io::Write> UpgradeProgress<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            deferred_output: None,
        }
    }

    fn before_mutation(&mut self, line: &str) -> Result<(), MachineExecutionError> {
        self.write_line(line).map_err(MachineExecutionError::Output)
    }

    fn after_mutation(&mut self, line: &str) {
        if let Err(error) = self.write_line(line)
            && self.deferred_output.is_none()
        {
            self.deferred_output = Some(error);
        }
    }

    fn after_confirmation(
        &mut self,
        machine: &str,
        confirmed: usize,
        line: &str,
    ) -> Result<(), MachineExecutionError> {
        self.after_mutation(line);
        let Some(source) = self.deferred_output.take() else {
            return Ok(());
        };
        Err(MachineExecutionError::UpgradeOutput {
            machine: machine.to_owned(),
            confirmed,
            source,
        })
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()
    }
}

async fn complete_release_upgrade<W, Confirm, Confirmation>(
    progress: &mut UpgradeProgress<W>,
    machine: &str,
    version: &InstallArtifactVersion,
    previously_confirmed: usize,
    confirm: Confirm,
) -> Result<(), MachineExecutionError>
where
    W: std::io::Write,
    Confirm: FnOnce() -> Confirmation,
    Confirmation: Future<Output = bool>,
{
    progress.after_mutation(&render_upgrade_staged(machine));
    if !confirm().await {
        return Err(upgrade_halted(
            machine,
            previously_confirmed,
            format!(
                "swap armed; not yet confirming {}. Check `ployz machine ls` and resume with `ployz machine upgrade --outdated`",
                version.as_str()
            ),
        ));
    }
    progress.after_confirmation(
        machine,
        previously_confirmed + 1,
        &render_upgrade_confirmed(machine, version),
    )
}

#[must_use]
fn render_upgrade_staged(machine: &str) -> String {
    format!("{machine}  staged, swap armed\n")
}

#[must_use]
fn render_upgrade_confirmed(machine: &str, version: &InstallArtifactVersion) -> String {
    format!("{machine}  now {} ✓\n", version.as_str())
}

fn upgrade_halted(machine: &str, confirmed: usize, reason: String) -> MachineExecutionError {
    MachineExecutionError::UpgradeHalted {
        machine: machine.to_owned(),
        confirmed,
        reason,
    }
}

async fn confirm_upgrade(
    remote: &OperatorRemote,
    machine_id: &MachineName,
    expected_version: &InstallArtifactVersion,
) -> bool {
    let deadline = Instant::now() + UPGRADE_CONFIRM_TIMEOUT;
    loop {
        if let Ok(snapshot) = remote.lens(LensCollection::MachineStatus).await
            && matches!(
                machine_status_for(snapshot_rows(&snapshot), machine_id),
                Some(status) if same_release_version(&status.document.ployz_version, expected_version.as_str())
            )
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(UPGRADE_CONFIRM_RETRY).await;
    }
}

fn snapshot_rows(snapshot: &LensSnapshot) -> &[MachineStatusLensRow] {
    match snapshot {
        LensSnapshot::MachineStatus { rows } => rows,
        LensSnapshot::Machines { .. }
        | LensSnapshot::Services { .. }
        | LensSnapshot::Endpoints { .. }
        | LensSnapshot::Operations { .. } => &[],
    }
}

#[derive(Debug, Clone)]
enum UpgradeSource {
    Release {
        version: ExactPloyzVersion,
        manifest_base_url: String,
    },
    Manual(MachineUpgradeRequest),
}

impl UpgradeSource {
    fn version(&self) -> Option<&str> {
        match self {
            Self::Release { version, .. } => Some(version.as_str()),
            Self::Manual(_) => None,
        }
    }

    const fn confirmation(&self) -> UpgradeConfirmation {
        match self {
            Self::Release { .. } => UpgradeConfirmation::ReleaseVersion,
            Self::Manual(_) => UpgradeConfirmation::UnavailableForManualArtifact,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeConfirmation {
    ReleaseVersion,
    UnavailableForManualArtifact,
}

async fn resolve_upgrade_source(
    source: &MachineUpgradeSource,
) -> Result<UpgradeSource, MachineExecutionError> {
    match source {
        MachineUpgradeSource::Channel => {
            let contents = read_manifest_text(DEFAULT_RELEASE_CHANNEL_URL.to_owned()).await?;
            let channel = parse_release_channel(&contents)?;
            Ok(UpgradeSource::Release {
                version: channel.version,
                manifest_base_url: channel.release_base_url,
            })
        }
        MachineUpgradeSource::Version(version) => Ok(UpgradeSource::Release {
            version: version.clone(),
            manifest_base_url: format!(
                "https://github.com/getployz/ployz/releases/download/{}",
                version.tag()
            ),
        }),
        MachineUpgradeSource::Manual { url, sha256 } => {
            Ok(UpgradeSource::Manual(MachineUpgradeRequest {
                version: InstallArtifactVersion::try_new("manual")
                    .expect("the fixed manual artifact label is nonempty"),
                sha256: sha256.clone(),
                url: url.clone(),
            }))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseChannel {
    version: ExactPloyzVersion,
    release_base_url: String,
}

fn parse_release_channel(contents: &str) -> Result<ReleaseChannel, MachineExecutionError> {
    let version = channel_value(contents, "PLOYZ_VERSION")?
        .parse::<ExactPloyzVersion>()
        .map_err(|error| MachineExecutionError::Release(error.to_string()))?;
    let release_base_url = channel_value(contents, "PLOYZ_RELEASE_BASE_URL")?;
    let release_base_url = release_base_url.trim_end_matches('/').to_owned();
    ployz_core::MachineUpgradeUrl::try_new(release_base_url.clone())
        .map_err(|error| MachineExecutionError::Release(error.to_string()))?;
    Ok(ReleaseChannel {
        version,
        release_base_url,
    })
}

fn channel_value(contents: &str, key: &str) -> Result<String, MachineExecutionError> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MachineExecutionError::Release(format!("release channel is missing {key}")))
}

async fn read_manifest_text(url: String) -> Result<String, MachineExecutionError> {
    tokio::task::spawn_blocking(move || read_release_manifest_text(&url))
        .await
        .map_err(|error| {
            MachineExecutionError::Release(format!("release manifest task failed: {error}"))
        })?
        .map_err(MachineExecutionError::Release)
}

struct UpgradeMachine<'a> {
    row: &'a MachineLensRow,
    status: &'a MachineStatusLensRow,
}

fn upgrade_lenses<'machines, 'statuses>(
    machines: &'machines LensSnapshot,
    statuses: &'statuses LensSnapshot,
) -> Result<
    (
        &'machines [MachineLensRow],
        &'statuses [MachineStatusLensRow],
    ),
    MachineExecutionError,
> {
    let LensSnapshot::Machines { rows, .. } = machines else {
        return Err(MachineExecutionError::Snapshot(
            MachineSnapshotError::WrongLens {
                actual: snapshot_collection(machines),
            },
        ));
    };
    let LensSnapshot::MachineStatus { rows: status_rows } = statuses else {
        return Err(MachineExecutionError::Snapshot(
            MachineSnapshotError::WrongLens {
                actual: snapshot_collection(statuses),
            },
        ));
    };
    Ok((rows, status_rows))
}

fn select_upgrade_machines<'a>(
    selector: &MachineUpgradeSelector,
    machines: &'a [MachineLensRow],
    statuses: &'a [MachineStatusLensRow],
    target_version: Option<&str>,
) -> Result<Vec<UpgradeMachine<'a>>, MachineExecutionError> {
    let mut selected = match selector {
        MachineUpgradeSelector::Names(names) => names
            .iter()
            .map(|name| {
                let Some(row) = machines.iter().find(|row| row.document.name == *name) else {
                    return Err(MachineExecutionError::MachineNotFound {
                        machine: name.as_str().to_owned(),
                    });
                };
                let Some(status) = machine_status_for(statuses, &row.id) else {
                    return Err(MachineExecutionError::MissingMachineStatus {
                        machine: name.as_str().to_owned(),
                    });
                };
                Ok(UpgradeMachine { row, status })
            })
            .collect::<Result<Vec<_>, _>>()?,
        MachineUpgradeSelector::All => machines
            .iter()
            .map(|row| {
                let Some(status) = machine_status_for(statuses, &row.id) else {
                    return Err(MachineExecutionError::MissingMachineStatus {
                        machine: row.document.name.as_str().to_owned(),
                    });
                };
                Ok(UpgradeMachine { row, status })
            })
            .collect::<Result<Vec<_>, _>>()?,
        MachineUpgradeSelector::Outdated => {
            let target_version =
                target_version.ok_or(MachineExecutionError::ManualOutdatedSelector)?;
            machines
                .iter()
                .filter_map(|row| {
                    let status = machine_status_for(statuses, &row.id)?;
                    is_release_behind(&status.document.ployz_version, target_version)
                        .then_some(UpgradeMachine { row, status })
                })
                .collect()
        }
    };
    selected.sort_by(|left, right| {
        left.row
            .document
            .name
            .as_str()
            .cmp(right.row.document.name.as_str())
            .then_with(|| left.row.id.as_str().cmp(right.row.id.as_str()))
    });
    Ok(selected)
}

fn machine_status_for<'a>(
    statuses: &'a [MachineStatusLensRow],
    machine_id: &MachineName,
) -> Option<&'a MachineStatusLensRow> {
    statuses.iter().find(|status| status.id == *machine_id)
}

async fn resolve_machine_upgrade_request(
    source: &UpgradeSource,
    status: &MachineStatusLensRow,
    cache: &mut Vec<(ReleasePlatform, MachineUpgradeRequest)>,
) -> Result<MachineUpgradeRequest, MachineExecutionError> {
    let UpgradeSource::Release {
        version,
        manifest_base_url,
    } = source
    else {
        let UpgradeSource::Manual(request) = source else {
            unreachable!("upgrade source has only release and manual variants");
        };
        return Ok(request.clone());
    };
    let platform = release_platform_for_architecture(&status.document.architecture)?;
    if let Some((_, request)) = cache.iter().find(|(cached, _)| *cached == platform) {
        return Ok(request.clone());
    }
    let manifest_url = if manifest_base_url.as_str()
        == format!(
            "https://github.com/getployz/ployz/releases/download/{}",
            version.tag()
        ) {
        release_manifest_url_for_platform(version, platform.manifest_slug())
    } else {
        format!(
            "{manifest_base_url}/ployz-release-{}.env",
            platform.manifest_slug()
        )
    };
    let contents = read_manifest_text(manifest_url).await?;
    let manifest = ReleaseManifest::parse(&contents)
        .map_err(|error| MachineExecutionError::Release(error.to_string()))?;
    if manifest.platform() != platform || manifest.version() != version {
        return Err(MachineExecutionError::Release(format!(
            "release manifest does not match requested {} for {}",
            version.tag(),
            platform.manifest_slug()
        )));
    }
    let artifact = manifest
        .install_artifacts()
        .map_err(MachineExecutionError::Release)?
        .ployzd;
    let request = MachineUpgradeRequest {
        version: artifact.version,
        sha256: artifact.sha256,
        url: ployz_core::MachineUpgradeUrl::try_new(artifact.source.as_str().to_owned())
            .map_err(|error| MachineExecutionError::Release(error.to_string()))?,
    };
    cache.push((platform, request.clone()));
    Ok(request)
}

fn release_platform_for_architecture(
    architecture: &str,
) -> Result<ReleasePlatform, MachineExecutionError> {
    match architecture {
        "x86_64" | "amd64" => Ok(ReleasePlatform::LinuxAmd64),
        "aarch64" | "arm64" => Ok(ReleasePlatform::LinuxArm64),
        _ => Err(MachineExecutionError::UnsupportedArchitecture {
            architecture: architecture.to_owned(),
        }),
    }
}

fn format_upgrade_refusal(refusal: &MachineUpgradeRefusal) -> String {
    match refusal {
        MachineUpgradeRefusal::UnsupportedSupervisor { supervisor } => {
            format!("the target supervisor {supervisor:?} does not support keeper-first swaps")
        }
        MachineUpgradeRefusal::DownloadFailed { message }
        | MachineUpgradeRefusal::StagingFailed { message }
        | MachineUpgradeRefusal::KeeperRefused { message } => message.clone(),
        MachineUpgradeRefusal::Sha256Mismatch { expected, got } => {
            format!(
                "SHA-256 mismatch: expected {}, got {}",
                expected.as_str(),
                got.as_str()
            )
        }
    }
}

fn same_release_version(actual: &str, expected: &str) -> bool {
    actual.trim_start_matches('v') == expected.trim_start_matches('v')
}

fn is_release_behind(actual: &str, target: &str) -> bool {
    matches!(
        comparable_release_version(actual)
            .zip(comparable_release_version(target))
            .map(|(actual, target)| actual.cmp(&target)),
        Some(Ordering::Less)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComparableReleaseVersion {
    major: u64,
    minor: u64,
    patch: u64,
    pre_release: Option<Vec<ReleasePreIdentifier>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReleasePreIdentifier {
    Numeric(u64),
    Text(String),
}

impl Ord for ComparableReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre_release, &other.pre_release) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => compare_pre_release(left, right),
            })
    }
}

impl PartialOrd for ComparableReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_pre_release(left: &[ReleasePreIdentifier], right: &[ReleasePreIdentifier]) -> Ordering {
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (ReleasePreIdentifier::Numeric(left), ReleasePreIdentifier::Numeric(right)) => {
                left.cmp(right)
            }
            (ReleasePreIdentifier::Text(left), ReleasePreIdentifier::Text(right)) => {
                left.cmp(right)
            }
            (ReleasePreIdentifier::Numeric(_), ReleasePreIdentifier::Text(_)) => Ordering::Less,
            (ReleasePreIdentifier::Text(_), ReleasePreIdentifier::Numeric(_)) => Ordering::Greater,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn comparable_release_version(value: &str) -> Option<ComparableReleaseVersion> {
    let value = value.trim_start_matches('v');
    let value = value.split_once('+').map_or(value, |(value, _)| value);
    let (core, pre_release) = value
        .split_once('-')
        .map_or((value, None), |(core, pre_release)| {
            (core, Some(pre_release))
        });
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let pre_release = match pre_release {
        None => None,
        Some("") => return None,
        Some(pre_release) => Some(
            pre_release
                .split('.')
                .map(|identifier| {
                    if identifier.is_empty()
                        || !identifier
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    {
                        return None;
                    }
                    identifier
                        .parse::<u64>()
                        .map(ReleasePreIdentifier::Numeric)
                        .unwrap_or_else(|_| ReleasePreIdentifier::Text(identifier.to_owned()))
                        .into()
                })
                .collect::<Option<Vec<_>>>()?,
        ),
    };
    Some(ComparableReleaseVersion {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        patch: patch.parse().ok()?,
        pre_release,
    })
}

async fn endpoint_set(command: MachineEndpointSetCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = MachineEndpointSetRequest {
        machine_name: command.machine.clone(),
        endpoint: command.endpoint,
    };
    let reply = remote
        .request_json_with_refusal::<_, MachineEndpointSetReply, MachineEndpointSetRefusal>(
            hyper::Method::POST,
            MACHINE_ENDPOINT_ROUTE_PREFIX,
            Some(&request),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(MachineEndpointSetRefusal::NotFound { machine_name }) => {
            return Err(MachineExecutionError::MachineNotFound {
                machine: machine_name.as_str().to_owned(),
            });
        }
        JsonReply::Refused(MachineEndpointSetRefusal::ProviderDoesNotUseWireguard {
            machine_id,
        }) => return Err(MachineExecutionError::EndpointProviderMismatch { machine_id }),
        JsonReply::Refused(MachineEndpointSetRefusal::EndpointPortZero { machine_name }) => {
            return Err(MachineExecutionError::EndpointPortZero {
                machine: machine_name.as_str().to_owned(),
            });
        }
    };
    if reply.machine.name != command.machine
        || !matches!(
            &reply.machine.transport,
            MachineTransport::Wireguard {
                endpoint: Some(endpoint),
                ..
            } if *endpoint == command.endpoint
        )
    {
        return Err(MachineExecutionError::EndpointReplyMismatch);
    }
    Ok(render_endpoint_set(&command.machine, command.endpoint))
}

async fn join(command: MachineJoinCommand) -> Result<String, MachineExecutionError> {
    let MachineJoinCommand {
        blob,
        storage,
        wireguard_endpoint,
    } = command;
    let storage_choice = match storage {
        InitStorageChoice::Automatic => JoinStorageChoice::Automatic,
        InitStorageChoice::Flag { mode } => JoinStorageChoice::Flag { mode },
    };
    let mut door = JoinDoorClient::default();
    let outcome =
        run_linux_machine_join(blob, storage_choice, wireguard_endpoint, &mut door).await?;
    Ok(render_machine_join(
        outcome.kind,
        &outcome.machine_name,
        &outcome.machine_id,
    ))
}

async fn remove(command: MachineRemoveCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = MachineRemoveRequest {
        machine_name: command.machine.clone(),
    };
    let reply = remote
        .request_json_with_refusal::<_, MachineRemoveReply, MachineRemoveRefusal>(
            hyper::Method::POST,
            MACHINE_REMOVE_ROUTE,
            Some(&request),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(MachineRemoveRefusal::NotFound { machine_name }) => {
            return Err(MachineExecutionError::MachineRemovalNotFound { machine_name });
        }
        JsonReply::Refused(MachineRemoveRefusal::ConcurrentMutation { machine_name }) => {
            return Err(MachineExecutionError::MachineRemovalChanged { machine_name });
        }
    };
    if !matches!(&reply, MachineRemoveReply::Removed { machine_name } if machine_name == &command.machine)
    {
        return Err(MachineExecutionError::MachineRemovalReplyMismatch);
    }
    Ok(render_machine_removal(&reply))
}

fn reset() -> Result<String, MachineExecutionError> {
    run_linux_machine_reset().map_err(|message| MachineExecutionError::Reset { message })?;
    Ok(render_machine_reset().to_owned())
}

async fn list(command: MachineListCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let snapshot = remote.lens(LensCollection::Machines).await?;
    validate_snapshot_cluster(&snapshot, remote.cluster_id())?;
    render_machines(&snapshot).map_err(MachineExecutionError::from)
}
#[derive(Debug, thiserror::Error)]
pub enum MachineExecutionError {
    #[error("could not write upgrade progress: {0}")]
    Output(#[source] std::io::Error),
    #[error(
        "could not write upgrade progress after {machine}; {confirmed} machine(s) confirmed: {source}"
    )]
    UpgradeOutput {
        machine: String,
        confirmed: usize,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Target(#[from] MachineRemoteTargetError),
    #[error(transparent)]
    Snapshot(#[from] MachineSnapshotError),
    #[error("release resolution failed: {0}")]
    Release(String),
    #[error("machine {machine} was not found")]
    MachineNotFound { machine: String },
    #[error(
        "machine {machine} has no reported machine status; run `ployz machine ls` to inspect mesh reachability before upgrading it"
    )]
    MissingMachineStatus { machine: String },
    #[error(
        "machine reported unsupported architecture {architecture:?}; Ployz upgrades support linux x86_64/amd64 and aarch64/arm64"
    )]
    UnsupportedArchitecture { architecture: String },
    #[error(
        "--outdated needs a released target version; use machine names or --all with --url and --sha256"
    )]
    ManualOutdatedSelector,
    #[error("upgrade halted after {machine}; {confirmed} machine(s) confirmed. {reason}")]
    UpgradeHalted {
        machine: String,
        confirmed: usize,
        reason: String,
    },
    #[error("machine {machine_id} does not use builtin WireGuard; its endpoint cannot be set")]
    EndpointProviderMismatch { machine_id: MachineName },
    #[error("machine {machine} endpoint must use a nonzero WireGuard port")]
    EndpointPortZero { machine: String },
    #[error("cluster API endpoint-set reply did not match the requested machine and endpoint")]
    EndpointReplyMismatch,
    #[error(
        "machine {} is not in the roster; run `ployz machine ls` to inspect current machines",
        machine_name.as_str()
    )]
    MachineRemovalNotFound { machine_name: MachineName },
    #[error(
        "machine {machine_name} changed while its removal was being committed; retry from current reality"
    )]
    MachineRemovalChanged { machine_name: MachineName },
    #[error("cluster API machine-removal reply did not match the requested identity")]
    MachineRemovalReplyMismatch,
    #[error(transparent)]
    Join(#[from] MachineJoinFailure),
    #[error("{message}")]
    Reset { message: FailureMessage },
}

#[derive(Debug, thiserror::Error)]
pub enum MachineSnapshotError {
    #[error("cluster API returned {actual:?} instead of the machines lens")]
    WrongLens { actual: LensCollection },
    #[error("cluster API answered for cluster {actual}, expected {expected}")]
    WrongCluster {
        expected: ClusterName,
        actual: ClusterName,
    },
}

pub(crate) fn render_machines(snapshot: &LensSnapshot) -> Result<String, MachineSnapshotError> {
    let LensSnapshot::Machines { rows, .. } = snapshot else {
        return Err(MachineSnapshotError::WrongLens {
            actual: snapshot_collection(snapshot),
        });
    };
    let mut rows = rows.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.document
            .name
            .as_str()
            .cmp(right.document.name.as_str())
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });

    let mut output = String::from("NAME\tCONTROL ADDRESS\n");
    for row in rows {
        let address = match &row.document.transport {
            MachineTransport::Wireguard {
                pubkey: _,
                addr_v6,
                endpoint: _,
                subnet_v4: _,
            } => addr_v6.to_string(),
            MachineTransport::Tailscale { ip, subnet_v4: _ } => ip.to_string(),
        };
        output.push_str(row.document.name.as_str());
        output.push('\t');
        output.push_str(&address);
        output.push('\n');
    }
    Ok(output)
}

fn validate_snapshot_cluster(
    snapshot: &LensSnapshot,
    expected: &ClusterName,
) -> Result<(), MachineSnapshotError> {
    let LensSnapshot::Machines { cluster, .. } = snapshot else {
        return Err(MachineSnapshotError::WrongLens {
            actual: snapshot_collection(snapshot),
        });
    };
    if cluster.cluster_id != *expected {
        return Err(MachineSnapshotError::WrongCluster {
            expected: expected.clone(),
            actual: cluster.cluster_id.clone(),
        });
    }
    Ok(())
}

const fn snapshot_collection(snapshot: &LensSnapshot) -> LensCollection {
    match snapshot {
        LensSnapshot::Machines { .. } => LensCollection::Machines,
        LensSnapshot::Services { .. } => LensCollection::Services,
        LensSnapshot::Endpoints { .. } => LensCollection::Endpoints,
        LensSnapshot::MachineStatus { .. } => LensCollection::MachineStatus,
        LensSnapshot::Operations { .. } => LensCollection::Operations,
    }
}

#[must_use]
pub fn render_endpoint_set(machine: &MachineName, endpoint: std::net::SocketAddr) -> String {
    format!("{} endpoint {}\n", machine.as_str(), endpoint)
}

#[must_use]
pub fn render_machine_removal(reply: &MachineRemoveReply) -> String {
    match reply {
        MachineRemoveReply::Removed { machine_name } => {
            format!("Removed machine {}.\n", machine_name.as_str())
        }
    }
}

#[must_use]
pub fn render_machine_join(
    outcome: MachineJoinOutcomeKind,
    machine_name: &MachineName,
    machine_id: &MachineName,
) -> String {
    let verb = match outcome {
        MachineJoinOutcomeKind::Joined => "Joined",
        MachineJoinOutcomeKind::Resumed => "Resumed",
        MachineJoinOutcomeKind::NoOp => "No-op",
    };
    format!(
        "{verb} machine {} ({}).\n",
        machine_name.as_str(),
        machine_id.as_str()
    )
}

#[must_use]
pub const fn render_machine_reset() -> &'static str {
    "Ployz state reset. Join with a fresh token.\n"
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

    fn machines_snapshot() -> LensSnapshot {
        serde_json::from_value(json!({
            "collection": "machines",
            "cluster": {
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-04T10:00:00Z",
                "name": "acme",
                "storage_default": "plain",
                "hostname_mode": { "mode": "disabled" },
                "prefix": "10.210.0.0/16",
                "provider": "builtin_wireguard",
                "acme_directory_url": "https://acme.example/directory",
                "acme_contact": null
            },
            "rows": [
                {
                    "id": MACHINE_B,
                    "document": {
                        "v": 1,
                        "cluster_id": CLUSTER,
                        "written_by": { "kind": "peer", "peer_id": PEER },
                        "written_at": "2026-08-04T10:00:00Z",
                        "name": "zeta",
                        "lifecycle": "active",
                        "transport": {
                            "kind": "tailscale",
                            "ip": "100.64.0.20",
                            "subnet_v4": "10.210.20.0/24"
                        },
                        "storage": { "mode": "plain", "reason": { "kind": "default" } }
                    }
                },
                {
                    "id": MACHINE_A,
                    "document": {
                        "v": 1,
                        "cluster_id": CLUSTER,
                        "written_by": { "kind": "peer", "peer_id": PEER },
                        "written_at": "2026-08-04T10:00:00Z",
                        "name": "alpha",
                        "lifecycle": "active",
                        "transport": {
                            "kind": "wireguard",
                            "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                            "addr_v6": "fd00::20",
                            "endpoint": null,
                            "subnet_v4": "10.210.21.0/24"
                        },
                        "storage": { "mode": "plain", "reason": { "kind": "default" } }
                    }
                }
            ]
        }))
        .expect("machines snapshot fixture")
    }

    #[test]
    fn machines_render_as_stable_human_first_table() {
        assert_eq!(
            render_machines(&machines_snapshot()).expect("machine table"),
            "NAME\tCONTROL ADDRESS\nalpha\tfd00::20\nzeta\t100.64.0.20\n"
        );
    }

    #[test]
    fn endpoint_set_confirmation_is_terse() {
        assert_eq!(
            render_endpoint_set(
                &MachineName::try_new("edge-b").expect("machine name"),
                "203.0.113.7:51820".parse().expect("endpoint"),
            ),
            "edge-b endpoint 203.0.113.7:51820\n"
        );
    }

    #[test]
    fn endpoint_set_client_uses_the_typed_machine_route() {
        let request = MachineEndpointSetRequest {
            machine_name: MachineName::try_new("edge-b").expect("machine name"),
            endpoint: "203.0.113.7:51820".parse().expect("endpoint"),
        };
        assert_eq!(MACHINE_ENDPOINT_ROUTE_PREFIX, "/machines/endpoint");
        assert_eq!(request.machine_name.as_str(), "edge-b");
        assert_eq!(request.endpoint.to_string(), "203.0.113.7:51820");
    }

    #[test]
    fn machine_removal_uses_the_typed_route_and_renders_terminal_outcomes() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let request = MachineRemoveRequest {
            machine_name: machine_name.clone(),
        };
        assert_eq!(MACHINE_REMOVE_ROUTE, "/machines/remove");
        assert_eq!(request.machine_name, machine_name);
        assert_eq!(
            render_machine_removal(&MachineRemoveReply::Removed {
                machine_name: machine_name.clone(),
            }),
            "Removed machine edge-b.\n"
        );
    }

    #[test]
    fn upgrade_success_evidence_is_rendered_as_independent_lines() {
        let version = InstallArtifactVersion::try_new("0.1.0-alpha.7").expect("version");

        assert_eq!(
            render_upgrade_staged("edge-a"),
            "edge-a  staged, swap armed\n"
        );
        assert_eq!(
            render_upgrade_confirmed("edge-a", &version),
            "edge-a  now 0.1.0-alpha.7 ✓\n"
        );
    }

    struct FailedUpgradeProgress;

    impl std::io::Write for FailedUpgradeProgress {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "progress sink closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "progress sink closed",
            ))
        }
    }

    #[tokio::test]
    async fn accepted_upgrade_output_failure_still_confirms_before_stopping() {
        let version = InstallArtifactVersion::try_new("0.1.0-alpha.7").expect("version");
        let confirmation_attempted = Cell::new(false);
        let mut progress = UpgradeProgress::new(FailedUpgradeProgress);

        let result = complete_release_upgrade(&mut progress, "edge-a", &version, 0, || async {
            confirmation_attempted.set(true);
            true
        })
        .await;

        assert!(confirmation_attempted.get());
        assert!(matches!(
            result,
            Err(MachineExecutionError::UpgradeOutput {
                machine,
                confirmed: 1,
                source,
            }) if machine == "edge-a" && source.kind() == std::io::ErrorKind::BrokenPipe
        ));
    }

    #[tokio::test]
    async fn confirmation_failure_takes_priority_over_deferred_output_failure() {
        let version = InstallArtifactVersion::try_new("0.1.0-alpha.7").expect("version");
        let mut progress = UpgradeProgress::new(FailedUpgradeProgress);

        let result =
            complete_release_upgrade(&mut progress, "edge-a", &version, 2, || async { false })
                .await;

        assert!(matches!(
            result,
            Err(MachineExecutionError::UpgradeHalted {
                machine,
                confirmed: 2,
                ..
            }) if machine == "edge-a"
        ));
    }

    #[test]
    fn channel_release_and_outdated_selection_use_exact_release_versions() {
        let channel = parse_release_channel(
            "PLOYZ_CHANNEL=alpha\nPLOYZ_VERSION=0.1.0-alpha.7\nPLOYZ_RELEASE_BASE_URL=https://releases.example/ployz/v0.1.0-alpha.7\n",
        )
        .expect("channel parses");
        assert_eq!(channel.version.as_str(), "0.1.0-alpha.7");
        assert_eq!(
            channel.release_base_url,
            "https://releases.example/ployz/v0.1.0-alpha.7"
        );

        assert!(is_release_behind("v0.1.0-alpha.6", "0.1.0-alpha.7"));
        assert!(is_release_behind("0.0.9", "0.1.0-alpha.7"));
        assert!(!is_release_behind("0.1.0", "0.1.0-alpha.7"));
        assert!(!is_release_behind("not-a-version", "0.1.0-alpha.7"));
    }

    #[test]
    fn release_platform_uses_the_machine_testimony_not_the_callers_host() {
        assert_eq!(
            release_platform_for_architecture("x86_64").expect("amd64"),
            ReleasePlatform::LinuxAmd64
        );
        assert_eq!(
            release_platform_for_architecture("arm64").expect("arm64"),
            ReleasePlatform::LinuxArm64
        );
        assert!(matches!(
            release_platform_for_architecture("riscv64"),
            Err(MachineExecutionError::UnsupportedArchitecture { architecture }) if architecture == "riscv64"
        ));
    }

    #[test]
    fn machine_removal_not_found_tells_the_operator_how_to_continue() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        assert_eq!(
            MachineExecutionError::MachineRemovalNotFound { machine_name }.to_string(),
            "machine edge-b is not in the roster; run `ployz machine ls` to inspect current machines"
        );
    }

    #[test]
    fn a_manual_artifact_never_counts_as_confirmed_without_a_release_version() {
        let source = UpgradeSource::Manual(MachineUpgradeRequest {
            version: InstallArtifactVersion::try_new("manual").expect("manual label"),
            sha256: ployz_core::install::InstallSha256Digest::try_new("a".repeat(64))
                .expect("sha256"),
            url: ployz_core::MachineUpgradeUrl::try_new("https://releases.example/ployzd")
                .expect("HTTPS URL"),
        });

        assert_eq!(
            source.confirmation(),
            UpgradeConfirmation::UnavailableForManualArtifact
        );
    }

    #[test]
    fn join_outcomes_name_the_accepted_machine_identity() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let machine_id = MachineName::try_new(MACHINE_A).expect("machine id");
        for (outcome, expected) in [
            (MachineJoinOutcomeKind::Joined, "Joined"),
            (MachineJoinOutcomeKind::Resumed, "Resumed"),
            (MachineJoinOutcomeKind::NoOp, "No-op"),
        ] {
            assert_eq!(
                render_machine_join(outcome, &machine_name, &machine_id),
                format!("{expected} machine edge-b ({MACHINE_A}).\n")
            );
        }
    }

    #[test]
    fn reset_confirmation_names_the_next_primitive() {
        assert_eq!(
            render_machine_reset(),
            "Ployz state reset. Join with a fresh token.\n"
        );
    }

    #[test]
    fn machine_snapshot_must_belong_to_the_selected_cluster() {
        let wrong = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAZ").expect("cluster id");
        assert!(matches!(
            validate_snapshot_cluster(&machines_snapshot(), &wrong),
            Err(MachineSnapshotError::WrongCluster { expected, actual })
                if expected == wrong && actual.as_str() == CLUSTER
        ));
    }

    #[test]
    fn udp_timeout_presents_the_documented_ssh_fallback_without_running_it() {
        let endpoint = "203.0.113.7:51820".parse().expect("UDP endpoint");
        let api_target = "[fd00::20]:2020".parse().expect("API target");
        let error = MachineExecutionError::Remote(OperatorRemoteError::Connect(
            crate::mesh::MeshConnectError::UdpDialTimedOut {
                endpoint,
                target: api_target,
                founding_target: "root@machine.example".into(),
                docs_anchor: crate::mesh::UDP_BLOCKED_DOCS_ANCHOR,
                ssh_fallback: crate::mesh::render_ssh_fallback_command(
                    "root@machine.example",
                    api_target,
                )
                .into(),
            },
        ));
        assert_eq!(
            error.to_string(),
            "WireGuard dial to [fd00::20]:2020 through UDP endpoint 203.0.113.7:51820 timed out; UDP may be blocked. Fallback: ssh -N -L 127.0.0.1:2020:[fd00::20]:2020 root@machine.example. See docs/operations/cli-mesh-access.md#udp-blocked-networks"
        );
    }
}
