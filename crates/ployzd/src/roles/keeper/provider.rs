//! Keeper's closed provider seam over privileged Host Runner effects.

use ployz_core::corrosion::{
    BuiltinWireguardMeshOutcome, DesiredBuiltinWireguardLocal, derive_builtin_wireguard_member,
};
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::network::WireGuardPublicKey;
use ployz_core::operation::FailureMessage;
use ployz_host_runner::builtin_wireguard::{
    BuiltinWireguardConfigError, BuiltinWireguardEbpfConfig, BuiltinWireguardHost,
    BuiltinWireguardHostConfig, BuiltinWireguardHostError, BuiltinWireguardHostOutcome,
    EbpfPinning, WireguardLocalBinding,
};
use ployz_host_runner::{
    HostRunnerCommandOutput, HostRunnerCommandRunner, SystemHostRunnerCommandRunner,
};

use super::KeeperRoleConfig;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The exhaustive set of shipped Keeper mesh providers.
enum KeeperMeshProviderState {
    BuiltinWireguard(BuiltinWireguardHost<CancellableHostRunner>),
}

/// An async-safe owner for bounded, blocking host effects.
#[derive(Clone)]
pub(super) struct KeeperMeshProvider {
    state: Arc<Mutex<KeeperMeshProviderState>>,
    fold_timeout: Duration,
    cancelled: Arc<AtomicBool>,
}

impl KeeperMeshProvider {
    pub(super) fn from_config(config: &KeeperRoleConfig) -> Result<Self, KeeperProviderError> {
        let ebpf = BuiltinWireguardEbpfConfig::try_new(
            config.bridge_interface().to_owned(),
            config.ebpf_ctl_path().to_path_buf(),
            config.ebpf_bytecode_path().to_path_buf(),
            EbpfPinning::Explicit(config.ebpf_pin_path().to_path_buf()),
        )?;
        let host_config = BuiltinWireguardHostConfig::try_new(
            config.private_key_path().to_path_buf(),
            config.wireguard_interface().to_owned(),
            config.wireguard_listen_port().get(),
            config.wireguard_mtu(),
            ebpf,
            config.supervisor_backend(),
            config.host_command_timeout(),
        )?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let runner = CancellableHostRunner {
            inner: SystemHostRunnerCommandRunner::new(config.host_command_timeout()),
            cancelled: Arc::clone(&cancelled),
        };
        Ok(Self {
            state: Arc::new(Mutex::new(KeeperMeshProviderState::BuiltinWireguard(
                BuiltinWireguardHost::with_runner(host_config, runner),
            ))),
            fold_timeout: config.host_fold_timeout(),
            cancelled,
        })
    }

    /// Supplies local identity material for future admission workflows.
    pub(super) async fn provision_join(&self) -> Result<WireGuardPublicKey, KeeperProviderError> {
        self.run_blocking("provision local WireGuard identity", |state| match state {
            KeeperMeshProviderState::BuiltinWireguard(host) => host
                .provision_and_read_public_key()
                .map_err(KeeperProviderError::Host),
        })
        .await
    }

    /// Binds the derived local `/112` without observing or changing peers.
    pub(super) async fn bind_ip(
        &self,
        cluster_id: &ClusterId,
        machine_id: &MachineRowId,
    ) -> Result<BoundKeeperIdentity, KeeperProviderError> {
        let public_key = self.provision_join().await?;
        let cluster_id = cluster_id.clone();
        let machine_id = machine_id.clone();
        self.run_blocking("bind local WireGuard identity", move |state| {
            let KeeperMeshProviderState::BuiltinWireguard(host) = state;
            let local = desired_local(&cluster_id, &machine_id, public_key.clone());
            let evidence = host.bind_local(&local)?;
            Ok(BoundKeeperIdentity {
                public_key,
                local,
                evidence,
            })
        })
        .await
    }

    /// Applies only an already-fenced Core roster outcome.
    pub(super) async fn converge_peers(
        &self,
        outcome: &BuiltinWireguardMeshOutcome,
        bound: &BoundKeeperIdentity,
    ) -> Result<KeeperHostFold, KeeperProviderError> {
        let outcome = outcome.clone();
        let local = bound.local.clone();
        self.run_blocking("converge WireGuard peers", move |state| {
            match (state, outcome) {
                (
                    KeeperMeshProviderState::BuiltinWireguard(_),
                    BuiltinWireguardMeshOutcome::NoRoster { .. }
                    | BuiltinWireguardMeshOutcome::KeyMismatch { .. },
                ) => Ok(KeeperHostFold::Skipped),
                (
                    KeeperMeshProviderState::BuiltinWireguard(host),
                    BuiltinWireguardMeshOutcome::Fenced { .. },
                ) => host
                    .fence(&local)
                    .map(KeeperHostFold::Applied)
                    .map_err(KeeperProviderError::Host),
                (
                    KeeperMeshProviderState::BuiltinWireguard(host),
                    BuiltinWireguardMeshOutcome::Desired(desired),
                ) => host
                    .converge(&desired)
                    .map(KeeperHostFold::Applied)
                    .map_err(KeeperProviderError::Host),
            }
        })
        .await
    }

    async fn run_blocking<Output, Run>(
        &self,
        operation: &'static str,
        run: Run,
    ) -> Result<Output, KeeperProviderError>
    where
        Output: Send + 'static,
        Run: FnOnce(&mut KeeperMeshProviderState) -> Result<Output, KeeperProviderError>
            + Send
            + 'static,
    {
        let state = Arc::clone(&self.state);
        self.cancelled.store(false, Ordering::Release);
        let mut task = tokio::task::spawn_blocking(move || {
            let mut state = state.lock().map_err(|_| KeeperProviderError::Poisoned)?;
            run(&mut state)
        });
        tokio::select! {
            result = &mut task => result.map_err(|source| KeeperProviderError::Task {
                operation,
                detail: source.to_string(),
            })?,
            () = tokio::time::sleep(self.fold_timeout) => {
                self.cancelled.store(true, Ordering::Release);
                let result = task.await.map_err(|source| KeeperProviderError::Task {
                    operation,
                    detail: source.to_string(),
                })?;
                if matches!(result, Err(KeeperProviderError::Poisoned)) {
                    return result;
                }
                Err(KeeperProviderError::TimedOut {
                    operation,
                    timeout: self.fold_timeout,
                })
            }
        }
    }
}

#[derive(Debug)]
struct CancellableHostRunner {
    inner: SystemHostRunnerCommandRunner,
    cancelled: Arc<AtomicBool>,
}

impl CancellableHostRunner {
    fn check(&self) -> Result<(), FailureMessage> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(FailureMessage::try_new("Keeper host fold was cancelled")
                .expect("static cancellation message is valid"));
        }
        Ok(())
    }
}

impl HostRunnerCommandRunner for CancellableHostRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.check()?;
        self.inner.command(program, args)
    }

    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.check()?;
        self.inner.command_with_timeout(program, args, timeout)
    }

    fn read_os_release(&mut self) -> Result<String, FailureMessage> {
        self.check()?;
        self.inner.read_os_release()
    }

    fn is_linux(&mut self) -> bool {
        self.check().is_ok() && self.inner.is_linux()
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        self.check()?;
        self.inner.current_uid()
    }

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        self.check()?;
        self.inner.download(url, destination)
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        self.check()?;
        self.inner.docker_info()
    }

    fn docker_is_installed(&mut self) -> bool {
        self.check().is_ok() && self.inner.docker_is_installed()
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        self.check()?;
        self.inner.docker_uses_containerd_snapshotter()
    }

    fn docker_has_insecure_registry(&mut self, cidr: &str) -> Result<bool, FailureMessage> {
        self.check()?;
        self.inner.docker_has_insecure_registry(cidr)
    }

    fn dataplane_host_ready(&mut self) -> bool {
        self.check().is_ok() && self.inner.dataplane_host_ready()
    }

    fn build_host_ready(&mut self) -> bool {
        self.check().is_ok() && self.inner.build_host_ready()
    }
}

fn desired_local(
    cluster_id: &ClusterId,
    machine_id: &MachineRowId,
    public_key: WireGuardPublicKey,
) -> DesiredBuiltinWireguardLocal {
    let identity = derive_builtin_wireguard_member(cluster_id, &public_key);
    DesiredBuiltinWireguardLocal {
        machine_id: machine_id.clone(),
        public_key,
        subnet_v6: identity.subnet(),
        bind_address: identity.bind_address(),
    }
}

/// The stable local identity established before the Corrosion dependency.
#[derive(Clone)]
pub(super) struct BoundKeeperIdentity {
    pub(super) public_key: WireGuardPublicKey,
    local: DesiredBuiltinWireguardLocal,
    pub(super) evidence: WireguardLocalBinding,
}

pub(super) enum KeeperHostFold {
    Skipped,
    Applied(BuiltinWireguardHostOutcome),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum KeeperProviderError {
    #[error("invalid builtin WireGuard host configuration: {0}")]
    Configuration(#[from] BuiltinWireguardConfigError),
    #[error(transparent)]
    Host(#[from] BuiltinWireguardHostError),
    #[error("Keeper mesh provider state was poisoned")]
    Poisoned,
    #[error("Keeper host operation {operation} timed out after {timeout:?}")]
    TimedOut {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("Keeper host operation {operation} task failed: {detail}")]
    Task {
        operation: &'static str,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_host_runner::SupervisorBackend;

    fn provider_for_timeout_test(timeout: Duration) -> KeeperMeshProvider {
        let ebpf = BuiltinWireguardEbpfConfig::try_new(
            "br-ployz".to_owned(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            EbpfPinning::Explicit("/sys/fs/bpf/ployz".into()),
        )
        .expect("eBPF config");
        let host_config = BuiltinWireguardHostConfig::try_new(
            "/etc/ployz/wireguard.key".into(),
            "ployz0".to_owned(),
            51_820,
            1_420,
            ebpf,
            SupervisorBackend::Systemd,
            Duration::from_millis(20),
        )
        .expect("host config");
        let cancelled = Arc::new(AtomicBool::new(false));
        let runner = CancellableHostRunner {
            inner: SystemHostRunnerCommandRunner::new(Duration::from_millis(20)),
            cancelled: Arc::clone(&cancelled),
        };
        KeeperMeshProvider {
            state: Arc::new(Mutex::new(KeeperMeshProviderState::BuiltinWireguard(
                BuiltinWireguardHost::with_runner(host_config, runner),
            ))),
            fold_timeout: timeout,
            cancelled,
        }
    }

    #[tokio::test]
    async fn overall_timeout_cancels_and_joins_the_blocking_fold() {
        let provider = provider_for_timeout_test(Duration::from_millis(5));
        let cancelled = Arc::clone(&provider.cancelled);
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_in_task = Arc::clone(&stopped);
        let result = provider
            .run_blocking("test fold", move |_| {
                while !cancelled.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                stopped_in_task.store(true, Ordering::Release);
                Ok(())
            })
            .await;

        assert!(matches!(result, Err(KeeperProviderError::TimedOut { .. })));
        assert!(
            stopped.load(Ordering::Acquire),
            "timeout must join the cooperatively stopped blocking task"
        );
    }
}
