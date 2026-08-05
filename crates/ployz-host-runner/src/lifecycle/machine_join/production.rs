//! Linux production effects for resumable machine join.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use ployz_core::corrosion::{
    DesiredBuiltinWireguardLocal, MachineDocument, MachineTransport, QueryEvent, SqliteParameter,
    SqliteValue, Statement, StorageMode, StoredRow, derive_builtin_wireguard_member,
    read_named_roster_rows,
};
use ployz_core::deploy::ZfsPoolName;
use ployz_core::founding::MINIMUM_ZFS_MEMORY_BYTES;
use ployz_core::join::{
    JOIN_DOOR_PORT, JoinBlob, JoinStorageChoice, JoinStorageFacts, ValidatedMachineJoinAccepted,
};
use ployz_core::machine::MachineName;
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

use crate::builtin_wireguard::{
    BuiltinWireguardEbpfConfig, BuiltinWireguardHost, BuiltinWireguardHostConfig,
    BuiltinWireguardJoinSeed, BuiltinWireguardPorts,
};
use crate::{
    ArtifactKind, ArtifactSourceView, FileMode, HostPlatformProfile, HostRunnerCommandRunner,
    PloyzdRole, PloyzdRoleEnvironmentFile, PoolSelection, SupervisorBackend, SupervisorChange,
    SupervisorDirectories, SupervisorUnitSpec, SystemHostRunnerCommandRunner,
    acquire_remote_artifact_content_addressed, artifact_target, detect_host_platform,
    install_verified_artifact, prepare_storage, verify_artifact_file, write_durable_file,
};

use super::{
    JOIN_SUBSTRATE_ENV, MachineJoinDoor, MachineJoinFailure, MachineJoinHostEffects,
    MachineJoinIdentity, MachineJoinInput, MachineJoinInspection, MachineJoinLock,
    MachineJoinOutcome, MachineJoinStateDirectory, PreparedMachineJoin,
    execute_machine_join_locked, prepare_machine_join_locked,
};

const WIREGUARD_KEY_FILE: &str = "wireguard.key";
const DOOR_KEY_FILE: &str = "door.key";
const DOOR_CERTIFICATE_FILE: &str = "door.crt";
const DOOR_FINGERPRINT_FILE: &str = "door.fingerprint";
const ENV_FILE: &str = "ployzd.env";
const CORROSION_CONFIG_FILE: &str = "corrosion.toml";
const CORROSION_TOKEN_FILE: &str = "corrosion-token";
const BOOTSTRAP_SEED_FILE: &str = "join-bootstrap-seed.json";
const API_PORT: u16 = 2_020;
const CORROSION_API_PORT: u16 = 8_080;
const CORROSION_GOSSIP_PORT: u16 = 8_787;
const WIREGUARD_PORT: u16 = 51_820;
const CORROSION_QUERY_BODY_LIMIT: u64 = 64 * 1024;
const CORROSION_CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(30);
const CORROSION_QUERY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JoinStorageInventory {
    PlainRequested {
        total_memory_bytes: u64,
    },
    ZfsSelected {
        total_memory_bytes: u64,
        pool: ZfsPoolName,
    },
    Automatic {
        total_memory_bytes: u64,
        imported_pools: Vec<ZfsPoolName>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedStorageAction {
    Plain { volumes_path: PathBuf },
    Zfs { pool: ZfsPoolName },
}

impl JoinStorageInventory {
    fn facts(&self) -> JoinStorageFacts {
        match self {
            Self::PlainRequested { total_memory_bytes } => JoinStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: *total_memory_bytes,
            },
            Self::ZfsSelected {
                total_memory_bytes, ..
            } => JoinStorageFacts {
                imported_zfs_pool: true,
                total_memory_bytes: *total_memory_bytes,
            },
            Self::Automatic {
                total_memory_bytes,
                imported_pools,
            } => JoinStorageFacts {
                imported_zfs_pool: !imported_pools.is_empty(),
                total_memory_bytes: *total_memory_bytes,
            },
        }
    }

    fn matches_choice(&self, choice: JoinStorageChoice) -> bool {
        matches!(
            (self, choice),
            (
                Self::PlainRequested { .. },
                JoinStorageChoice::Flag {
                    mode: StorageMode::Plain
                }
            ) | (
                Self::ZfsSelected { .. },
                JoinStorageChoice::Flag {
                    mode: StorageMode::Zfs
                }
            ) | (Self::Automatic { .. }, JoinStorageChoice::Automatic)
        )
    }

    fn choice(&self) -> JoinStorageChoice {
        match self {
            Self::PlainRequested { .. } => JoinStorageChoice::Flag {
                mode: StorageMode::Plain,
            },
            Self::ZfsSelected { .. } => JoinStorageChoice::Flag {
                mode: StorageMode::Zfs,
            },
            Self::Automatic { .. } => JoinStorageChoice::Automatic,
        }
    }

    fn selected_pool(&self) -> Option<&ZfsPoolName> {
        match self {
            Self::ZfsSelected { pool, .. } => Some(pool),
            Self::Automatic {
                total_memory_bytes,
                imported_pools,
            } if *total_memory_bytes >= MINIMUM_ZFS_MEMORY_BYTES => {
                let [pool] = imported_pools.as_slice() else {
                    return None;
                };
                Some(pool)
            }
            Self::PlainRequested { .. } | Self::Automatic { .. } => None,
        }
    }
}

fn selected_storage_action(
    state: &MachineJoinStateDirectory,
    mode: StorageMode,
) -> Result<SelectedStorageAction, FailureMessage> {
    match mode {
        StorageMode::Plain => Ok(SelectedStorageAction::Plain {
            volumes_path: state.path().join("volumes"),
        }),
        StorageMode::Zfs => {
            let inventory = state
                .read_storage_inventory::<JoinStorageInventory>()
                .map_err(failure)?
                .ok_or_else(|| failure("accepted ZFS join has no retained storage inventory"))?;
            let pool = inventory
                .selected_pool()
                .cloned()
                .ok_or_else(|| failure("accepted ZFS join has no unambiguous retained pool"))?;
            Ok(SelectedStorageAction::Zfs { pool })
        }
    }
}

/// Production boundary used by the CLI: preflight, durable preparation, and activation.
pub async fn run_linux_machine_join(
    blob: JoinBlob,
    storage_choice: JoinStorageChoice,
    endpoint: Option<std::net::SocketAddr>,
    door: &mut impl MachineJoinDoor,
) -> Result<MachineJoinOutcome, MachineJoinFailure> {
    let mut runner = SystemHostRunnerCommandRunner::default();
    require_linux_root(&mut runner)
        .map_err(|message| MachineJoinFailure::HostPreflight { message })?;
    let state = MachineJoinStateDirectory::initialize_host_default()?;
    let lock = state.try_lock()?;
    let prepared = prepare_linux_machine_join_locked(
        &state,
        &lock,
        blob,
        storage_choice,
        endpoint,
        &mut runner,
    )?;
    execute_linux_machine_join_locked(&state, &lock, &prepared, door).await
}

/// Performs bounded machine-local probes and durably prepares canonical join admission.
///
/// A persisted request wins on resume, so changing CLI flags cannot rewrite an
/// admission that may already have been presented to the door.
pub fn prepare_linux_machine_join(
    state: &MachineJoinStateDirectory,
    blob: JoinBlob,
    storage_choice: JoinStorageChoice,
    endpoint: Option<std::net::SocketAddr>,
) -> Result<PreparedMachineJoin, MachineJoinFailure> {
    let lock = state.try_lock()?;
    prepare_linux_machine_join_locked(
        state,
        &lock,
        blob,
        storage_choice,
        endpoint,
        &mut SystemHostRunnerCommandRunner::default(),
    )
}

fn prepare_linux_machine_join_locked(
    state: &MachineJoinStateDirectory,
    lock: &MachineJoinLock,
    blob: JoinBlob,
    storage_choice: JoinStorageChoice,
    endpoint: Option<std::net::SocketAddr>,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<PreparedMachineJoin, MachineJoinFailure> {
    match state.inspect(blob.door_cert_fingerprint())? {
        MachineJoinInspection::Refused { refusal } => {
            return Err(MachineJoinFailure::Refused { refusal });
        }
        MachineJoinInspection::Clean
        | MachineJoinInspection::ReadyToRedeem { .. }
        | MachineJoinInspection::ReadyToActivate { .. }
        | MachineJoinInspection::NoOp { .. } => {}
    }
    let input = if let Some(request) = state.read_request()? {
        let inventory = state
            .read_storage_inventory::<JoinStorageInventory>()?
            .ok_or_else(|| host_preflight("persisted join request has no storage inventory"))?;
        if !inventory.matches_choice(request.storage_choice)
            || inventory.facts() != request.storage_facts
        {
            return Err(host_preflight(
                "persisted join request disagrees with retained storage inventory",
            ));
        }
        MachineJoinInput {
            name: request.name,
            endpoint: request.endpoint,
            storage_choice: request.storage_choice,
            storage_facts: request.storage_facts,
        }
    } else {
        require_linux_root(runner)
            .map_err(|message| MachineJoinFailure::HostPreflight { message })?;
        let name = probe_machine_name(runner)
            .map_err(|message| MachineJoinFailure::HostPreflight { message })?;
        let inventory = match state.read_storage_inventory::<JoinStorageInventory>()? {
            Some(inventory) => inventory,
            None => {
                let inventory = join_storage_inventory(runner, storage_choice)
                    .map_err(|message| MachineJoinFailure::HostPreflight { message })?;
                state.persist_storage_inventory(&inventory)?;
                inventory
            }
        };
        MachineJoinInput {
            name,
            endpoint,
            storage_choice: inventory.choice(),
            storage_facts: inventory.facts(),
        }
    };
    prepare_machine_join_locked(state, lock, blob, input)
}

fn host_preflight(message: impl std::fmt::Display) -> MachineJoinFailure {
    MachineJoinFailure::HostPreflight {
        message: failure(message),
    }
}

/// Runs the complete on-host workflow; callers provide only pinned door redemption.
pub async fn execute_linux_machine_join(
    state: &MachineJoinStateDirectory,
    prepared: &PreparedMachineJoin,
    door: &mut impl MachineJoinDoor,
) -> Result<MachineJoinOutcome, MachineJoinFailure> {
    let lock = state.try_lock()?;
    execute_linux_machine_join_locked(state, &lock, prepared, door).await
}

async fn execute_linux_machine_join_locked(
    state: &MachineJoinStateDirectory,
    lock: &MachineJoinLock,
    prepared: &PreparedMachineJoin,
    door: &mut impl MachineJoinDoor,
) -> Result<MachineJoinOutcome, MachineJoinFailure> {
    let mut effects = LinuxMachineJoinHostEffects::new(state.clone())
        .map_err(|message| MachineJoinFailure::HostPreflight { message })?;
    execute_machine_join_locked(state, lock, prepared, door, &mut effects).await
}

/// Host Runner-owned production executor. Every method is safe to repeat.
pub struct LinuxMachineJoinHostEffects {
    state: MachineJoinStateDirectory,
    runner: SystemHostRunnerCommandRunner,
    profile: Option<HostPlatformProfile>,
    supervisor_directories: SupervisorDirectories,
}

impl LinuxMachineJoinHostEffects {
    pub fn new(state: MachineJoinStateDirectory) -> Result<Self, FailureMessage> {
        let mut runner = SystemHostRunnerCommandRunner::default();
        require_linux_root(&mut runner)?;
        Ok(Self {
            state,
            runner,
            profile: None,
            supervisor_directories: SupervisorDirectories::host_defaults(),
        })
    }

    fn profile(&mut self) -> Result<&HostPlatformProfile, FailureMessage> {
        if self.profile.is_none() {
            let release = self.runner.read_os_release()?;
            self.profile = Some(detect_host_platform(&release).map_err(failure)?);
        }
        Ok(self.profile.as_ref().expect("profile was populated"))
    }

    fn supervisor(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.profile()?.supervisor().into())
    }

    fn require(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.runner.command(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(failure(output.failure))
        }
    }

    fn run_supervisor(
        &mut self,
        change: SupervisorChange,
        target: crate::SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in backend.commands(change, &target) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    fn install_artifact(
        &mut self,
        spec: &ployz_core::install::InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        let kind = artifact_kind(spec.install_path.as_str())?;
        let target = artifact_target(kind, spec).map_err(failure)?;
        let verified = match target.source_view() {
            ArtifactSourceView::LocalPath(path) => {
                verify_artifact_file(path, &target.digest).map_err(failure)?
            }
            ArtifactSourceView::RemoteUrl(url) => {
                let downloads = self.state.path().join("downloads");
                acquire_remote_artifact_content_addressed(
                    url,
                    &target.digest,
                    &downloads,
                    |staged| self.runner.download(url, staged),
                )
                .map_err(failure)?
            }
        };
        install_verified_artifact(&verified, &target).map_err(failure)?;
        Ok(())
    }

    fn write_environment(
        &self,
        accepted: &ValidatedMachineJoinAccepted,
        addr_v6: std::net::Ipv6Addr,
        corrosion_token: &str,
    ) -> Result<(), FailureMessage> {
        let env = render_environment(&self.state, accepted, addr_v6, corrosion_token);
        write_durable_file(
            self.state.path(),
            ENV_FILE,
            FileMode::Secret0600,
            env.as_bytes(),
        )
    }

    fn write_corrosion_config(
        &self,
        accepted: &ValidatedMachineJoinAccepted,
        addr_v6: std::net::Ipv6Addr,
        corrosion_token: &str,
    ) -> Result<(), FailureMessage> {
        let schema = accepted
            .accepted()
            .substrate
            .artifacts()
            .iter()
            .find(|artifact| artifact.install_path.as_str().contains("corrosion-schema"))
            .ok_or_else(|| failure("accepted substrate has no Corrosion schema artifact"))?;
        let subscriptions = self.state.path().join("subscriptions");
        fs::create_dir_all(&subscriptions).map_err(failure)?;
        let corrosion = render_corrosion_config(
            &self.state,
            accepted,
            schema.install_path.as_str(),
            addr_v6,
            corrosion_token,
        );
        write_durable_file(
            self.state.path(),
            CORROSION_CONFIG_FILE,
            FileMode::Secret0600,
            corrosion.as_bytes(),
        )
    }

    fn install_corrosion_unit(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let config = self.state.path().join(CORROSION_CONFIG_FILE);
        let (name, contents) = match backend {
            SupervisorBackend::Systemd => (
                "ployz-corrosion.service",
                format!(
                    "[Unit]\nDescription=Ployz Corrosion\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart=/usr/local/bin/corrosion --config {} agent\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
                    config.display()
                ),
            ),
            SupervisorBackend::OpenRc => (
                "ployz-corrosion",
                format!(
                    "#!/sbin/openrc-run\nname=ployz-corrosion\nsupervisor=supervise-daemon\ncommand=/usr/local/bin/corrosion\ncommand_args=\"--config {} agent\"\nrespawn_delay=5\n\ndepend() {{ need net; }}\n",
                    config.display()
                ),
            ),
        };
        write_durable_file(
            self.supervisor_directories.directory(backend),
            name,
            FileMode::Executable0755,
            contents.as_bytes(),
        )?;
        match backend {
            SupervisorBackend::Systemd => {
                self.require("systemctl", &["daemon-reload"])?;
                self.require("systemctl", &["enable", "ployz-corrosion.service"])
            }
            SupervisorBackend::OpenRc => {
                self.require("rc-update", &["add", "ployz-corrosion", "default"])
            }
        }
    }

    fn wait_for_service(
        &mut self,
        target: crate::SupervisorUnitTarget,
        description: &str,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for _ in 0..30 {
            let mut healthy = true;
            for (program, args) in backend.commands(SupervisorChange::IsActive, &target) {
                let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                healthy &= self.runner.command(program, &refs)?.success;
            }
            if healthy {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(format!(
            "{description} did not become ready within 30 seconds"
        )))
    }
}

fn render_environment(
    state: &MachineJoinStateDirectory,
    accepted: &ValidatedMachineJoinAccepted,
    addr_v6: std::net::Ipv6Addr,
    corrosion_token: &str,
) -> String {
    let accepted = accepted.accepted();
    format!(
        "PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nPLOYZ_CORROSION_BEARER_TOKEN={corrosion_token}\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_API_LISTEN_ADDR=[{addr_v6}]:{API_PORT}\nPLOYZ_API_DOOR_LISTEN_ADDR=[::]:{JOIN_DOOR_PORT}\nPLOYZ_API_DOOR_PRIVATE_KEY_PATH={}\nPLOYZ_API_DOOR_CERTIFICATE_PATH={}\nPLOYZ_API_DOOR_FINGERPRINT_PATH={}\n{JOIN_SUBSTRATE_ENV}={}\nPLOYZ_JOIN_DOOR_PORT={JOIN_DOOR_PORT}\nPLOYZ_CORROSION_GOSSIP_PORT={CORROSION_GOSSIP_PORT}\nPLOYZ_BUILD={}\nPLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}\nPLOYZ_CORROSION_VERSION={}\n",
        accepted.cluster.cluster_id,
        accepted.machine.machine_id,
        state.path().join(DOOR_KEY_FILE).display(),
        state.path().join(DOOR_CERTIFICATE_FILE).display(),
        state.path().join(DOOR_FINGERPRINT_FILE).display(),
        state.join_substrate_path().display(),
        accepted.substrate.ployz_version().as_str(),
        state.path().join(WIREGUARD_KEY_FILE).display(),
        accepted.substrate.corrosion_version(),
    )
}

fn render_corrosion_config(
    state: &MachineJoinStateDirectory,
    accepted: &ValidatedMachineJoinAccepted,
    schema_path: &str,
    addr_v6: std::net::Ipv6Addr,
    corrosion_token: &str,
) -> String {
    format!(
        "[db]\npath = {db:?}\nschema_paths = [{schema:?}]\nsubscriptions_path = {subscriptions:?}\n\n[gossip]\naddr = {gossip:?}\nbootstrap = [{bootstrap:?}]\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {api:?}\nauthz.bearer-token = {token:?}\n\n[admin]\npath = {admin:?}\n",
        db = state.path().join("corrosion.db").display().to_string(),
        schema = schema_path,
        subscriptions = state.path().join("subscriptions").display().to_string(),
        gossip = format!("[{addr_v6}]:{CORROSION_GOSSIP_PORT}"),
        bootstrap = accepted
            .accepted()
            .corrosion
            .seed_gossip_address
            .to_string(),
        api = format!("127.0.0.1:{CORROSION_API_PORT}"),
        token = corrosion_token,
        admin = state
            .path()
            .join("corrosion-admin.sock")
            .display()
            .to_string(),
    )
}

fn require_linux_root(runner: &mut impl HostRunnerCommandRunner) -> Result<(), FailureMessage> {
    if !runner.is_linux() {
        return Err(failure("ployz machine join requires Linux"));
    }
    if runner.current_uid()? != 0 {
        return Err(failure("ployz machine join must run as root"));
    }
    Ok(())
}

fn probe_machine_name(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<MachineName, FailureMessage> {
    let hostname = runner.command("cat", &["/etc/hostname"])?;
    if !hostname.success {
        return Err(failure(hostname.failure));
    }
    if hostname.stdout_truncated {
        return Err(failure("machine hostname output was truncated"));
    }
    let short = hostname.stdout.trim().split('.').next().unwrap_or_default();
    if short.is_empty() {
        return Err(failure("machine hostname is empty"));
    }
    MachineName::try_new(short.to_owned()).map_err(failure)
}

fn join_storage_inventory(
    runner: &mut impl HostRunnerCommandRunner,
    choice: JoinStorageChoice,
) -> Result<JoinStorageInventory, FailureMessage> {
    let memory = runner.command("cat", &["/proc/meminfo"])?;
    if !memory.success {
        return Err(failure(memory.failure));
    }
    if memory.stdout_truncated {
        return Err(failure("/proc/meminfo output was truncated"));
    }
    let total_memory_bytes = memory
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1_024))
        .ok_or_else(|| failure("/proc/meminfo has no valid MemTotal"))?;
    if choice
        == (JoinStorageChoice::Flag {
            mode: StorageMode::Plain,
        })
    {
        return Ok(JoinStorageInventory::PlainRequested { total_memory_bytes });
    }
    let pools = imported_zfs_pools(runner)?;
    match choice {
        JoinStorageChoice::Flag {
            mode: StorageMode::Plain,
        } => unreachable!("plain returned before the ZFS probe"),
        JoinStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } => {
            let [pool] = pools.as_slice() else {
                return Err(failure(match pools.len() {
                    0 => "ZFS storage requires exactly one imported pool; found none".to_owned(),
                    count => {
                        format!("ZFS storage requires exactly one imported pool; found {count}")
                    }
                }));
            };
            Ok(JoinStorageInventory::ZfsSelected {
                total_memory_bytes,
                pool: pool.clone(),
            })
        }
        JoinStorageChoice::Automatic
            if total_memory_bytes >= MINIMUM_ZFS_MEMORY_BYTES && pools.len() > 1 =>
        {
            Err(failure(format!(
                "automatic ZFS storage requires an unambiguous imported pool; found {}",
                pools.len()
            )))
        }
        JoinStorageChoice::Automatic => Ok(JoinStorageInventory::Automatic {
            total_memory_bytes,
            imported_pools: pools,
        }),
    }
}

fn imported_zfs_pools(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<Vec<ZfsPoolName>, FailureMessage> {
    let mut pools = match runner.command("zpool", &["list", "-H", "-o", "name"]) {
        Ok(output) if output.success => {
            if output.stdout_truncated {
                return Err(failure("imported ZFS pool inventory was truncated"));
            }
            output
                .stdout
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| ZfsPoolName::try_new(name.to_owned()).map_err(failure))
                .collect::<Result<Vec<_>, _>>()?
        }
        Ok(_) | Err(_) => Vec::new(),
    };
    pools.sort();
    pools.dedup();
    Ok(pools)
}

#[derive(Clone, PartialEq, Eq)]
struct CorrosionBearerToken(String);

impl std::fmt::Debug for CorrosionBearerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CorrosionBearerToken([REDACTED])")
    }
}

impl CorrosionBearerToken {
    fn from_file(path: &Path) -> Result<Self, FailureMessage> {
        let value = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(failure("durable Corrosion bearer token is absent"));
            }
            Err(error) => {
                return Err(failure(format!(
                    "could not read durable Corrosion bearer token: {error}"
                )));
            }
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(failure("durable Corrosion bearer token is empty"));
        }
        Ok(Self(value.to_owned()))
    }

    fn authorization_header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorrosionRosterQuery {
    url: String,
    body: String,
}

fn corrosion_roster_query(
    accepted: &ValidatedMachineJoinAccepted,
) -> Result<CorrosionRosterQuery, FailureMessage> {
    let accepted = accepted.accepted();
    let statement = Statement::with_params(
        "SELECT id, document FROM machines WHERE id = ?1 OR name = ?2",
        vec![
            SqliteParameter::Text(accepted.machine.machine_id.as_str().to_owned()),
            SqliteParameter::Text(accepted.machine.document.name.as_str().to_owned()),
        ],
    );
    Ok(CorrosionRosterQuery {
        url: format!("http://127.0.0.1:{CORROSION_API_PORT}/v1/queries"),
        body: serde_json::to_string(&statement).map_err(failure)?,
    })
}

fn query_corrosion_roster(
    agent: &ureq::Agent,
    query: &CorrosionRosterQuery,
    token: &CorrosionBearerToken,
) -> Result<Vec<StoredRow>, FailureMessage> {
    let authorization = token.authorization_header();
    let mut response = agent
        .post(&query.url)
        .header("Authorization", authorization)
        .header("Accept", "application/x-ndjson")
        .content_type("application/json")
        .send(query.body.as_bytes())
        .map_err(|error| failure(format!("local Corrosion query failed: {error}")))?;
    let body = response
        .body_mut()
        .with_config()
        .limit(CORROSION_QUERY_BODY_LIMIT)
        .read_to_string()
        .map_err(|error| failure(format!("local Corrosion response failed: {error}")))?;
    decode_corrosion_rows(&body)
}

fn decode_corrosion_rows(body: &str) -> Result<Vec<StoredRow>, FailureMessage> {
    if body.len() as u64 > CORROSION_QUERY_BODY_LIMIT {
        return Err(failure("local Corrosion response exceeded 65536 bytes"));
    }
    let mut saw_columns = false;
    let mut saw_end = false;
    let mut rows = Vec::new();
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        if saw_end {
            return Err(failure(
                "local Corrosion response continued after end-of-query",
            ));
        }
        let event: QueryEvent = serde_json::from_str(line)
            .map_err(|error| failure(format!("invalid local Corrosion frame: {error}")))?;
        match event {
            QueryEvent::Columns(columns) if !saw_columns && columns == ["id", "document"] => {
                saw_columns = true;
            }
            QueryEvent::Columns(_) if saw_columns => {
                return Err(failure("local Corrosion response repeated columns"));
            }
            QueryEvent::Columns(_) => {
                return Err(failure(
                    "local Corrosion response columns were not id and document",
                ));
            }
            QueryEvent::Row(_, _values) if !saw_columns => {
                return Err(failure("local Corrosion row preceded columns"));
            }
            QueryEvent::Row(_, values) => {
                let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice()
                else {
                    return Err(failure(
                        "local Corrosion row was not a text id and document pair",
                    ));
                };
                rows.push(StoredRow::new(key.clone(), document.clone()));
            }
            QueryEvent::EndOfQuery(_end) if !saw_columns => {
                return Err(failure("local Corrosion end-of-query preceded columns"));
            }
            QueryEvent::EndOfQuery(end) if end.change_id.is_some() => {
                return Err(failure(
                    "local Corrosion query end unexpectedly carried a change id",
                ));
            }
            QueryEvent::EndOfQuery(_) => saw_end = true,
            QueryEvent::Error(message) => {
                return Err(failure(format!("local Corrosion query error: {message}")));
            }
        }
    }
    if !saw_end {
        return Err(failure("local Corrosion response omitted end-of-query"));
    }
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RosterConvergenceDisposition {
    Converged,
    Missing,
    Skipped,
    Shadowed,
    Divergent,
}

impl std::fmt::Display for RosterConvergenceDisposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Converged => formatter.write_str("converged"),
            Self::Missing => formatter.write_str("accepted machine row is missing"),
            Self::Skipped => formatter.write_str("accepted machine row is invalid locally"),
            Self::Shadowed => formatter.write_str("accepted machine name is shadowed locally"),
            Self::Divergent => formatter.write_str("accepted machine row differs locally"),
        }
    }
}

fn roster_convergence_disposition(
    accepted: &ValidatedMachineJoinAccepted,
    rows: Vec<StoredRow>,
) -> RosterConvergenceDisposition {
    let accepted = accepted.accepted();
    let report = read_named_roster_rows::<MachineDocument>(&accepted.cluster, rows);
    let winner = report
        .accepted
        .iter()
        .find(|row| row.value.name == accepted.machine.document.name);
    if let Some(winner) = winner {
        if winner.source.key != accepted.machine.machine_id.as_str() {
            return RosterConvergenceDisposition::Shadowed;
        }
        if winner.value == accepted.machine.document {
            return RosterConvergenceDisposition::Converged;
        }
        return RosterConvergenceDisposition::Divergent;
    }
    if report
        .skipped
        .iter()
        .any(|row| row.source.key == accepted.machine.machine_id.as_str())
    {
        RosterConvergenceDisposition::Skipped
    } else if !report.shadows.is_empty() {
        RosterConvergenceDisposition::Shadowed
    } else {
        RosterConvergenceDisposition::Missing
    }
}

impl MachineJoinHostEffects for LinuxMachineJoinHostEffects {
    fn ensure_exact_artifacts(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        for artifact in accepted.accepted().substrate.artifacts() {
            self.install_artifact(artifact)?;
        }
        let output = self
            .runner
            .command("/usr/local/bin/corrosion", &["--version"])?;
        if output.success
            && output.stdout.trim() == accepted.accepted().substrate.corrosion_version()
        {
            Ok(())
        } else {
            Err(failure(
                "installed Corrosion version does not match acceptance",
            ))
        }
    }

    fn ensure_selected_storage(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        match selected_storage_action(
            &self.state,
            accepted.accepted().machine.document.storage.mode,
        )? {
            SelectedStorageAction::Plain { volumes_path } => {
                fs::create_dir_all(volumes_path).map_err(failure)
            }
            SelectedStorageAction::Zfs { pool } => {
                let profile = self.profile()?.clone();
                prepare_storage(
                    &mut self.runner,
                    &profile,
                    &PoolSelection::Explicit(pool),
                    self.state.path(),
                    Path::new("/etc/systemd/system/docker.service.d"),
                )
                .map(|_| ())
                .map_err(failure)
            }
        }
    }

    fn ensure_docker(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        if !self.runner.docker_is_installed() {
            let script = self.state.path().join("get-docker.sh");
            self.runner.download("https://get.docker.com", &script)?;
            self.require("sh", &[script.to_string_lossy().as_ref()])?;
        }
        let backend = self.supervisor()?;
        for (program, args) in backend.docker_commands(SupervisorChange::InstallAndStart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    fn ensure_shared_door_material(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let door = &accepted.accepted().door;
        write_durable_file(
            self.state.path(),
            DOOR_KEY_FILE,
            FileMode::Secret0600,
            door.private_key_pem.expose().as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_CERTIFICATE_FILE,
            FileMode::Plain,
            door.certificate_pem.as_str().as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_FINGERPRINT_FILE,
            FileMode::Plain,
            format!("{}\n", door.fingerprint.as_str()).as_bytes(),
        )
    }

    fn ensure_configuration(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let MachineTransport::Wireguard { addr_v6, .. } =
            &accepted.accepted().machine.document.transport
        else {
            return Err(failure("accepted machine transport is not WireGuard"));
        };
        let corrosion_token =
            read_or_generate_secret(&self.state.path().join(CORROSION_TOKEN_FILE))?;
        write_durable_file(
            self.state.path(),
            "cluster-id",
            FileMode::Secret0600,
            format!("{}\n", accepted.accepted().cluster.cluster_id).as_bytes(),
        )?;
        self.write_environment(accepted, *addr_v6, &corrosion_token)?;
        self.write_corrosion_config(accepted, *addr_v6, &corrosion_token)
    }

    fn ensure_temporary_seed_wireguard(
        &mut self,
        identity: &MachineJoinIdentity,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        write_durable_file(
            self.state.path(),
            WIREGUARD_KEY_FILE,
            FileMode::Secret0600,
            format!("{}\n", identity.expose_private_key()).as_bytes(),
        )?;
        let backend = self.supervisor()?;
        let config = wireguard_config(self.state.path(), backend)?;
        let mut host = BuiltinWireguardHost::new(config);
        let local_identity = derive_builtin_wireguard_member(
            &accepted.accepted().cluster.cluster_id,
            identity.public_key(),
        );
        let local = DesiredBuiltinWireguardLocal {
            public_key: identity.public_key().clone(),
            subnet_v6: local_identity.subnet(),
            bind_address: local_identity.bind_address(),
        };
        let MachineTransport::Wireguard {
            pubkey,
            endpoint: Some(endpoint),
            subnet_v4,
            ..
        } = &accepted.accepted().seed.transport
        else {
            return Err(failure("accepted seed is not reachable over WireGuard"));
        };
        let seed_identity =
            derive_builtin_wireguard_member(&accepted.accepted().cluster.cluster_id, pubkey);
        let seed = BuiltinWireguardJoinSeed {
            public_key: pubkey.clone(),
            subnet_v6: seed_identity.subnet(),
            endpoint: *endpoint,
            subnet_v4: subnet_v4.clone(),
        };
        host.bootstrap_join_seed(&local, &seed).map_err(failure)?;
        let bytes = serde_json::to_vec(&accepted.accepted().seed).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            BOOTSTRAP_SEED_FILE,
            FileMode::Plain,
            &bytes,
        )
    }

    fn ensure_units_installed_stopped(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let ployzd = accepted
            .accepted()
            .substrate
            .artifacts()
            .iter()
            .find(|artifact| artifact.install_path.as_str().ends_with("/ployzd"))
            .ok_or_else(|| failure("accepted substrate has no ployzd artifact"))?;
        let target = artifact_target(ArtifactKind::Ployzd, ployzd).map_err(failure)?;
        let environment =
            PloyzdRoleEnvironmentFile::new(self.state.path().join(ENV_FILE)).map_err(failure)?;
        for role in [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ] {
            let spec = SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: target.clone(),
                environment_file: environment.clone(),
            };
            let rendered = backend.render(&spec).map_err(failure)?;
            write_durable_file(
                self.supervisor_directories.directory(backend),
                rendered.file_name(),
                FileMode::Executable0755,
                rendered.contents().as_bytes(),
            )?;
            let change = match role {
                PloyzdRole::Keeper | PloyzdRole::Api => SupervisorChange::Enable,
                PloyzdRole::Gateway | PloyzdRole::Dns => SupervisorChange::Disable,
            };
            self.run_supervisor(change, spec.target())?;
            if matches!(role, PloyzdRole::Gateway | PloyzdRole::Dns) {
                self.run_supervisor(SupervisorChange::Stop, spec.target())?;
            }
        }
        self.install_corrosion_unit()
    }

    fn ensure_corrosion_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        match self.supervisor()? {
            SupervisorBackend::Systemd => {
                self.require("systemctl", &["restart", "ployz-corrosion.service"])
            }
            SupervisorBackend::OpenRc => {
                self.require("rc-service", &["ployz-corrosion", "restart"])
            }
        }
    }

    fn await_roster_convergence(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let token = CorrosionBearerToken::from_file(&self.state.path().join(CORROSION_TOKEN_FILE))?;
        let query = corrosion_roster_query(accepted)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(CORROSION_QUERY_ATTEMPT_TIMEOUT))
            .timeout_resolve(Some(CORROSION_QUERY_ATTEMPT_TIMEOUT))
            .timeout_connect(Some(CORROSION_QUERY_ATTEMPT_TIMEOUT))
            .build()
            .into();
        let deadline = Instant::now() + CORROSION_CONVERGENCE_TIMEOUT;
        let last = loop {
            let observed = match query_corrosion_roster(&agent, &query, &token) {
                Ok(rows) => {
                    let disposition = roster_convergence_disposition(accepted, rows);
                    if disposition == RosterConvergenceDisposition::Converged {
                        return Ok(());
                    }
                    disposition.to_string()
                }
                Err(error) => error.to_string(),
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break observed;
            }
            thread::sleep(Duration::from_secs(1).min(remaining));
            if Instant::now() >= deadline {
                break observed;
            }
        };
        Err(failure(format!(
            "accepted machine row did not converge in local Corrosion within 30 seconds: {last}"
        )))
    }

    fn ensure_keeper_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.run_supervisor(
            SupervisorChange::Restart,
            crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Keeper),
        )
    }

    fn ensure_api_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.run_supervisor(
            SupervisorChange::Restart,
            crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Api),
        )
    }

    fn await_machine_ready(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.wait_for_service(
            crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Keeper),
            "Keeper",
        )?;
        self.wait_for_service(
            crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Api),
            "API",
        )
    }

    fn remove_temporary_bootstrap(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        match fs::remove_file(self.state.path().join(BOOTSTRAP_SEED_FILE)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(error)),
        }
    }
}

fn artifact_kind(path: &str) -> Result<ArtifactKind, FailureMessage> {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure("accepted artifact has no file name"))?;
    match file_name {
        "ployzd" => Ok(ArtifactKind::Ployzd),
        "corrosion" => Ok(ArtifactKind::Corrosion),
        "corrosion-schema-v1.sql" => Ok(ArtifactKind::CorrosionSchema),
        "ployz-ebpf-tc" => Ok(ArtifactKind::EbpfBytecode),
        "ployz-ebpf-ctl" => Ok(ArtifactKind::EbpfCtl),
        "railpack" => Ok(ArtifactKind::Railpack),
        _ => Err(failure(format!(
            "accepted artifact install path has unknown file {file_name:?}"
        ))),
    }
}

fn wireguard_config(
    state: &Path,
    supervisor: SupervisorBackend,
) -> Result<BuiltinWireguardHostConfig, FailureMessage> {
    let ports = BuiltinWireguardPorts::try_new(
        WIREGUARD_PORT,
        CORROSION_GOSSIP_PORT,
        API_PORT,
        JOIN_DOOR_PORT,
    )
    .map_err(failure)?;
    let ebpf = BuiltinWireguardEbpfConfig::try_new(
        "ployz".to_owned(),
        PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"),
        PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
        PathBuf::from("/sys/fs/bpf/ployz/tcx"),
    )
    .map_err(failure)?;
    BuiltinWireguardHostConfig::try_new(
        state.join(WIREGUARD_KEY_FILE),
        "ployz0".to_owned(),
        ports,
        1_420,
        ebpf,
        supervisor,
        Duration::from_secs(30),
    )
    .map_err(failure)
}

fn read_or_generate_secret(path: &Path) -> Result<String, FailureMessage> {
    match fs::read_to_string(path) {
        Ok(secret) if !secret.trim().is_empty() => Ok(secret.trim().to_owned()),
        Ok(_) => Err(failure(format!("secret file {} is empty", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let secret = defguard_wireguard_rs::key::Key::generate().to_string();
            let parent = path
                .parent()
                .ok_or_else(|| failure("secret path has no parent"))?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| failure("secret path has no file name"))?;
            write_durable_file(
                parent,
                file_name,
                FileMode::Secret0600,
                format!("{secret}\n").as_bytes(),
            )?;
            Ok(secret)
        }
        Err(error) => Err(failure(error)),
    }
}

fn failure(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("machine join host effect failed").expect("constant is non-empty")
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ployz_core::join::JoinStorageChoice;
    use ployz_core::machine::MachineLifecycle;

    use super::super::orchestration::tests::{accepted, join_blob, join_input};
    use super::*;
    use crate::HostRunnerCommandOutput;

    #[derive(Default)]
    struct RecordingRunner {
        outputs: VecDeque<HostRunnerCommandOutput>,
        calls: Vec<String>,
    }

    impl RecordingRunner {
        fn with_outputs(outputs: impl IntoIterator<Item = HostRunnerCommandOutput>) -> Self {
            Self {
                outputs: outputs.into_iter().collect(),
                calls: Vec::new(),
            }
        }
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            self.outputs
                .pop_front()
                .ok_or_else(|| failure("unexpected test command"))
        }

        fn is_linux(&mut self) -> bool {
            true
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Err(failure("unexpected test download"))
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_is_installed(&mut self) -> bool {
            true
        }

        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            Ok(false)
        }

        fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
            Ok(false)
        }
    }

    fn output(stdout: impl Into<String>) -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: stdout.into(),
            stdout_truncated: false,
            failure: String::new(),
        }
    }

    fn memory(gib: u64) -> HostRunnerCommandOutput {
        output(format!("MemTotal: {} kB\n", gib * 1024 * 1024))
    }

    fn validated_fixture() -> (
        tempfile::TempDir,
        MachineJoinStateDirectory,
        PreparedMachineJoin,
        ValidatedMachineJoinAccepted,
    ) {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let prepared = super::super::prepare_machine_join(&state, join_blob(), join_input())
            .expect("prepared");
        let accepted = accepted(prepared.request(), prepared.blob().door_cert_fingerprint())
            .try_validate(prepared.request(), prepared.blob().door_cert_fingerprint())
            .expect("validated acceptance");
        (directory, state, prepared, accepted)
    }

    #[test]
    fn storage_inventory_applies_only_reachable_zfs_ambiguity() {
        let mut plain = RecordingRunner::with_outputs([memory(4)]);
        let plain_inventory = join_storage_inventory(
            &mut plain,
            JoinStorageChoice::Flag {
                mode: StorageMode::Plain,
            },
        )
        .expect("plain skips ZFS inventory");
        assert!(matches!(
            plain_inventory,
            JoinStorageInventory::PlainRequested { .. }
        ));
        assert_eq!(plain.calls, ["cat /proc/meminfo"]);

        let mut missing = RecordingRunner::with_outputs([memory(4), output("")]);
        assert!(
            join_storage_inventory(
                &mut missing,
                JoinStorageChoice::Flag {
                    mode: StorageMode::Zfs,
                },
            )
            .is_err()
        );

        let mut low_memory = RecordingRunner::with_outputs([memory(1), output("alpha\nbeta\n")]);
        let automatic = join_storage_inventory(&mut low_memory, JoinStorageChoice::Automatic)
            .expect("low-memory automatic selection must be plain");
        assert!(automatic.facts().imported_zfs_pool);
        assert_eq!(automatic.selected_pool(), None);

        let mut ambiguous = RecordingRunner::with_outputs([memory(4), output("alpha\nbeta\n")]);
        assert!(join_storage_inventory(&mut ambiguous, JoinStorageChoice::Automatic).is_err());
    }

    #[test]
    fn persisted_storage_inventory_resumes_without_host_probes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let lock = state.try_lock().expect("lock");
        let mut first = RecordingRunner::with_outputs([
            output("edge-a.example\n"),
            memory(4),
            output("tank\n"),
        ]);
        let prepared = prepare_linux_machine_join_locked(
            &state,
            &lock,
            join_blob(),
            JoinStorageChoice::Automatic,
            None,
            &mut first,
        )
        .expect("first preparation");
        drop(lock);

        let lock = state.try_lock().expect("resume lock");
        let mut resumed = RecordingRunner::default();
        let resumed_prepared = prepare_linux_machine_join_locked(
            &state,
            &lock,
            join_blob(),
            JoinStorageChoice::Flag {
                mode: StorageMode::Plain,
            },
            None,
            &mut resumed,
        )
        .expect("persisted request and inventory win");

        assert_eq!(resumed_prepared.request(), prepared.request());
        assert!(resumed.calls.is_empty());
    }

    #[test]
    fn selected_storage_action_uses_plain_path_or_retained_zfs_pool() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        assert_eq!(
            selected_storage_action(&state, StorageMode::Plain).expect("plain action"),
            SelectedStorageAction::Plain {
                volumes_path: directory.path().join("volumes")
            }
        );

        let inventory = JoinStorageInventory::ZfsSelected {
            total_memory_bytes: 4 * 1024 * 1024 * 1024,
            pool: ZfsPoolName::try_new("tank").expect("pool"),
        };
        state
            .persist_storage_inventory(&inventory)
            .expect("inventory");
        assert_eq!(
            selected_storage_action(&state, StorageMode::Zfs).expect("ZFS action"),
            SelectedStorageAction::Zfs {
                pool: ZfsPoolName::try_new("tank").expect("pool")
            }
        );
    }

    #[test]
    fn corrosion_decoder_is_strict_and_bounded() {
        let valid = concat!(
            "{\"columns\":[\"id\",\"document\"]}\n",
            "{\"row\":[1,[\"machine\",\"{}\"]]}\n",
            "{\"eoq\":{\"time\":0.1}}\n"
        );
        assert_eq!(
            decode_corrosion_rows(valid).expect("valid frames"),
            [StoredRow::new("machine", "{}")]
        );
        for invalid in [
            "{\"columns\":[\"document\",\"id\"]}\n{\"eoq\":{\"time\":0.1}}\n",
            "{\"columns\":[\"id\",\"document\"]}\n{\"row\":[1,[1,\"{}\"]]}\n{\"eoq\":{\"time\":0.1}}\n",
            "{\"columns\":[\"id\",\"document\"]}\n{\"error\":\"nope\"}\n",
            "{\"columns\":[\"id\",\"document\"]}\n",
            "{\"columns\":[\"id\",\"document\"]}\n{\"eoq\":{\"time\":0.1}}\n{\"eoq\":{\"time\":0.2}}\n",
        ] {
            assert!(
                decode_corrosion_rows(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(
            decode_corrosion_rows(&"x".repeat(CORROSION_QUERY_BODY_LIMIT as usize + 1)).is_err()
        );
    }

    #[test]
    fn convergence_requires_exact_accepted_name_winner() {
        let (_directory, _state, _prepared, accepted) = validated_fixture();
        let expected = accepted.accepted().machine.clone();
        let document = serde_json::to_string(&expected.document).expect("document");
        assert_eq!(
            roster_convergence_disposition(
                &accepted,
                vec![StoredRow::new(expected.machine_id.as_str(), document)]
            ),
            RosterConvergenceDisposition::Converged
        );
        assert_eq!(
            roster_convergence_disposition(&accepted, Vec::new()),
            RosterConvergenceDisposition::Missing
        );

        let mut divergent = expected.document.clone();
        divergent.lifecycle = MachineLifecycle::Draining;
        assert_eq!(
            roster_convergence_disposition(
                &accepted,
                vec![StoredRow::new(
                    expected.machine_id.as_str(),
                    serde_json::to_string(&divergent).expect("document")
                )]
            ),
            RosterConvergenceDisposition::Divergent
        );
        assert_eq!(
            roster_convergence_disposition(
                &accepted,
                vec![StoredRow::new(
                    "00000000000000000000000000",
                    serde_json::to_string(&expected.document).expect("document")
                )]
            ),
            RosterConvergenceDisposition::Shadowed
        );

        let mut foreign = expected.document;
        foreign.cluster_id = ployz_core::ids::ClusterId::generate();
        assert_eq!(
            roster_convergence_disposition(
                &accepted,
                vec![StoredRow::new(
                    expected.machine_id.as_str(),
                    serde_json::to_string(&foreign).expect("document")
                )]
            ),
            RosterConvergenceDisposition::Skipped
        );
    }

    #[test]
    fn query_token_environment_and_artifact_contracts_are_exact() {
        let (directory, state, prepared, accepted) = validated_fixture();
        let secret_path = directory.path().join(CORROSION_TOKEN_FILE);
        fs::write(&secret_path, b"super-secret\n").expect("secret");
        let token = CorrosionBearerToken::from_file(&secret_path).expect("token");
        assert_eq!(format!("{token:?}"), "CorrosionBearerToken([REDACTED])");
        assert!(!format!("{token:?}").contains("super-secret"));

        let query = corrosion_roster_query(&accepted).expect("query");
        assert_eq!(query.url, "http://127.0.0.1:8080/v1/queries");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&query.body).expect("body"),
            serde_json::json!([
                "SELECT id, document FROM machines WHERE id = ?1 OR name = ?2",
                [prepared.request().machine_id.as_str(), "edge-a"]
            ])
        );

        let MachineTransport::Wireguard { addr_v6, .. } =
            accepted.accepted().machine.document.transport
        else {
            panic!("fixture transport")
        };
        let environment = render_environment(&state, &accepted, addr_v6, "super-secret");
        for line in [
            format!(
                "PLOYZ_API_DOOR_PRIVATE_KEY_PATH={}",
                directory.path().join(DOOR_KEY_FILE).display()
            ),
            format!(
                "PLOYZ_API_DOOR_CERTIFICATE_PATH={}",
                directory.path().join(DOOR_CERTIFICATE_FILE).display()
            ),
            format!(
                "PLOYZ_API_DOOR_FINGERPRINT_PATH={}",
                directory.path().join(DOOR_FINGERPRINT_FILE).display()
            ),
            format!(
                "{JOIN_SUBSTRATE_ENV}={}",
                state.join_substrate_path().display()
            ),
            format!(
                "PLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}",
                directory.path().join(WIREGUARD_KEY_FILE).display()
            ),
            "PLOYZ_CORROSION_VERSION=0.3.1".to_owned(),
        ] {
            assert!(environment.lines().any(|candidate| candidate == line));
        }
        let config = render_corrosion_config(
            &state,
            &accepted,
            "/usr/local/lib/ployz/corrosion-schema-v1.sql",
            addr_v6,
            "super-secret",
        );
        assert!(config.contains("authz.bearer-token = \"super-secret\""));

        assert_eq!(
            [
                "/usr/local/bin/ployzd",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                "/usr/local/bin/ployz-ebpf-ctl",
                "/usr/local/bin/corrosion",
                "/usr/local/lib/ployz/corrosion-schema-v1.sql",
                "/usr/local/bin/railpack",
            ]
            .map(|path| artifact_kind(path).expect("known artifact")),
            [
                ArtifactKind::Ployzd,
                ArtifactKind::EbpfBytecode,
                ArtifactKind::EbpfCtl,
                ArtifactKind::Corrosion,
                ArtifactKind::CorrosionSchema,
                ArtifactKind::Railpack,
            ]
        );
    }
}
