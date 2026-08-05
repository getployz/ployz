//! Linux production effects for resumable machine join.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use ployz_core::corrosion::{
    DesiredBuiltinWireguardLocal, MachineTransport, StorageMode, derive_builtin_wireguard_member,
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
    ArtifactKind, FileMode, HostPlatformProfile, HostRunnerCommandRunner, PloyzdRole,
    PloyzdRoleEnvironmentFile, PoolSelection, SupervisorBackend, SupervisorChange,
    SupervisorDirectories, SystemHostRunnerCommandRunner, artifact_target, prepare_storage,
    write_durable_file,
};

use crate::lifecycle::production::{
    CorrosionBootstrap, CorrosionConfig, CorrosionServiceChange, GeneratedSecretPersistence,
    LinuxSubstrate, machine_endpoint_gateway, read_or_generate_secret,
    render_corrosion_config as render_shared_corrosion_config,
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

mod query;

use query::*;

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

    fn substrate(&mut self) -> LinuxSubstrate<'_, SystemHostRunnerCommandRunner> {
        LinuxSubstrate::new(
            self.state.path(),
            &mut self.runner,
            &mut self.profile,
            &self.supervisor_directories,
        )
    }

    fn profile(&mut self) -> Result<HostPlatformProfile, FailureMessage> {
        Ok(self.substrate().profile()?.clone())
    }

    fn supervisor(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        self.substrate().supervisor()
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

    fn wait_for_service(
        &mut self,
        target: crate::SupervisorUnitTarget,
        description: &str,
    ) -> Result<(), FailureMessage> {
        let crate::SupervisorUnitTarget::PloyzdRole(role) = target;
        self.substrate().wait_for_role(role, description)
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
        "PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nPLOYZ_CORROSION_BEARER_TOKEN={corrosion_token}\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_API_LISTEN_ADDR=[{addr_v6}]:{API_PORT}\nPLOYZ_API_DOOR_LISTEN_ADDR=[::]:{JOIN_DOOR_PORT}\nPLOYZ_API_DOOR_PRIVATE_KEY_PATH={}\nPLOYZ_API_DOOR_CERTIFICATE_PATH={}\nPLOYZ_API_DOOR_FINGERPRINT_PATH={}\n{JOIN_SUBSTRATE_ENV}={}\nPLOYZ_CORROSION_GOSSIP_PORT={CORROSION_GOSSIP_PORT}\nPLOYZ_BUILD={}\nPLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}\nPLOYZ_CORROSION_VERSION={}\n",
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
    let seed = accepted
        .accepted()
        .corrosion
        .seed_gossip_address
        .to_string();
    render_shared_corrosion_config(CorrosionConfig {
        state: state.path(),
        schema_path,
        gossip_addr: addr_v6,
        bootstrap: CorrosionBootstrap::Seed(&seed),
        bearer_token: corrosion_token,
    })
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

impl LinuxMachineJoinHostEffects {
    fn ensure_exact_artifacts(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        for (kind, artifact) in accepted.accepted().substrate.artifacts_by_kind() {
            self.substrate().install_artifact(kind, artifact)?;
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
                let profile = self.profile()?;
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
        self.substrate().ensure_docker()
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
        let corrosion_token = read_or_generate_secret(
            &self.state.path().join(CORROSION_TOKEN_FILE),
            GeneratedSecretPersistence::Durable,
        )?;
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
        let config = self.state.path().join(CORROSION_CONFIG_FILE);
        let mut substrate = self.substrate();
        substrate.install_ployzd_units(&target, &environment)?;
        substrate.install_corrosion_unit(&config)?;
        substrate.change_corrosion_service(CorrosionServiceChange::Enable)
    }

    fn ensure_corrosion_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.substrate()
            .change_corrosion_service(CorrosionServiceChange::Restart)
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
        self.substrate().run_supervisor(
            SupervisorChange::Restart,
            &crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Keeper),
        )
    }

    fn ensure_api_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.substrate().run_supervisor(
            SupervisorChange::Restart,
            &crate::SupervisorUnitTarget::PloyzdRole(PloyzdRole::Api),
        )
    }

    fn await_endpoint_network(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let gateway = accepted_endpoint_gateway(accepted);
        self.substrate().await_endpoint_network_gateway(gateway)
    }

    fn ensure_dns_started(
        &mut self,
        _accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        self.substrate().enable_and_start_dns()
    }

    fn await_dns_ready(
        &mut self,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        let gateway = accepted_endpoint_gateway(accepted);
        self.substrate().await_dns_readiness(gateway)
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

impl MachineJoinHostEffects for LinuxMachineJoinHostEffects {
    fn apply_milestone(
        &mut self,
        milestone: super::MachineJoinMilestone,
        identity: &MachineJoinIdentity,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage> {
        match milestone {
            super::MachineJoinMilestone::Artifacts => self.ensure_exact_artifacts(accepted),
            super::MachineJoinMilestone::Storage => self.ensure_selected_storage(accepted),
            super::MachineJoinMilestone::Docker => self.ensure_docker(accepted),
            super::MachineJoinMilestone::DoorMaterial => self.ensure_shared_door_material(accepted),
            super::MachineJoinMilestone::Configuration => self.ensure_configuration(accepted),
            super::MachineJoinMilestone::BootstrapWireguard => {
                self.ensure_temporary_seed_wireguard(identity, accepted)
            }
            super::MachineJoinMilestone::UnitsInstalled => {
                self.ensure_units_installed_stopped(accepted)
            }
            super::MachineJoinMilestone::CorrosionStarted => {
                self.ensure_corrosion_started(accepted)
            }
            super::MachineJoinMilestone::RosterConverged => self.await_roster_convergence(accepted),
            super::MachineJoinMilestone::KeeperStarted => self.ensure_keeper_started(accepted),
            super::MachineJoinMilestone::ApiStarted => self.ensure_api_started(accepted),
            super::MachineJoinMilestone::EndpointNetworkReady => {
                self.await_endpoint_network(accepted)
            }
            super::MachineJoinMilestone::DnsStarted => self.ensure_dns_started(accepted),
            super::MachineJoinMilestone::DnsReady => self.await_dns_ready(accepted),
            super::MachineJoinMilestone::Ready => self.await_machine_ready(accepted),
            super::MachineJoinMilestone::BootstrapCleaned => {
                self.remove_temporary_bootstrap(accepted)
            }
        }
    }
}

fn accepted_endpoint_gateway(accepted: &ValidatedMachineJoinAccepted) -> std::net::Ipv4Addr {
    machine_endpoint_gateway(&accepted.accepted().machine.document.transport)
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

fn failure(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("machine join host effect failed").expect("constant is non-empty")
    })
}

#[cfg(test)]
mod tests;
