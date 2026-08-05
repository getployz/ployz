//! Keeper's closed provider seam over privileged Host Runner effects.

use ployz_core::corrosion::{
    DesiredBuiltinWireguardLocal, DesiredBuiltinWireguardMesh, derive_builtin_wireguard_member,
};
use ployz_core::ids::ClusterId;
use ployz_core::network::WireGuardPublicKey;
use ployz_core::operation::FailureMessage;
use ployz_host_runner::builtin_wireguard::{
    BuiltinWireguardHost, BuiltinWireguardHostError, BuiltinWireguardHostOutcome,
    WireguardLocalBinding,
};
use ployz_host_runner::container_isolation::{ContainerIsolationHost, ContainerIsolationHostError};
use ployz_host_runner::{
    HostRunnerCommandOutput, HostRunnerCommandRunner, SystemHostRunnerCommandRunner,
};

use super::KeeperRoleConfig;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// An async-safe owner for bounded, blocking host effects.
#[derive(Clone)]
pub(super) struct KeeperMeshProvider {
    host: Arc<Mutex<BuiltinWireguardHost<CancellableHostRunner>>>,
    isolation_host: Arc<Mutex<ContainerIsolationHost<CancellableHostRunner>>>,
    fold_timeout: Duration,
    cancelled: Arc<AtomicBool>,
}

impl KeeperMeshProvider {
    pub(super) fn from_config(config: &KeeperRoleConfig) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let runner = CancellableHostRunner {
            inner: SystemHostRunnerCommandRunner::new(config.host().command_timeout()),
            cancelled: Arc::clone(&cancelled),
        };
        Self {
            host: Arc::new(Mutex::new(BuiltinWireguardHost::with_runner(
                config.host().clone(),
                CancellableHostRunner {
                    inner: SystemHostRunnerCommandRunner::new(config.host().command_timeout()),
                    cancelled: Arc::clone(&cancelled),
                },
            ))),
            isolation_host: Arc::new(Mutex::new(ContainerIsolationHost::with_runner(
                config.isolation_host().clone(),
                runner,
            ))),
            fold_timeout: config.host_fold_timeout(),
            cancelled,
        }
    }

    pub(super) async fn converge_isolation(
        &self,
        desired: &ployz_core::corrosion::DesiredContainerIsolation,
    ) -> Result<(), KeeperProviderError> {
        let desired = desired.clone();
        let host = Arc::clone(&self.isolation_host);
        self.run_isolation_blocking("converge container isolation", move || {
            let mut host = host.lock().map_err(|_| KeeperProviderError::Poisoned)?;
            host.converge(&desired)
                .map_err(KeeperProviderError::Isolation)
        })
        .await
    }

    async fn run_isolation_blocking<Output, Run>(
        &self,
        operation: &'static str,
        run: Run,
    ) -> Result<Output, KeeperProviderError>
    where
        Output: Send + 'static,
        Run: FnOnce() -> Result<Output, KeeperProviderError> + Send + 'static,
    {
        let (send, mut receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("ployz-keeper-isolation-fold".to_owned())
            .spawn(move || {
                let _ = send.send(run());
            })
            .map_err(|source| KeeperProviderError::Task {
                operation,
                detail: source.to_string(),
            })?;
        tokio::select! {
            result = &mut receive => result.map_err(|source| KeeperProviderError::Task {
                operation,
                detail: source.to_string(),
            })?,
            () = tokio::time::sleep(self.fold_timeout) => {
                self.cancelled.store(true, Ordering::Release);
                Err(KeeperProviderError::TimedOut { operation, timeout: self.fold_timeout })
            }
        }
    }

    /// Supplies local identity material for future admission workflows.
    pub(super) async fn provision_join(&self) -> Result<WireGuardPublicKey, KeeperProviderError> {
        self.run_blocking("provision local WireGuard identity", |host| {
            host.provision_and_read_public_key()
                .map_err(KeeperProviderError::Host)
        })
        .await
    }

    /// Binds the derived local `/112` without observing or changing peers.
    pub(super) async fn bind_ip(
        &self,
        cluster_id: &ClusterId,
    ) -> Result<BoundKeeperIdentity, KeeperProviderError> {
        let public_key = self.provision_join().await?;
        let cluster_id = cluster_id.clone();
        self.run_blocking("bind local WireGuard identity", move |host| {
            let local = desired_local(&cluster_id, public_key.clone());
            let evidence = host.bind_local(&local)?;
            Ok(BoundKeeperIdentity {
                public_key,
                local,
                evidence,
            })
        })
        .await
    }

    /// Removes every peer after Core fenced the local machine.
    pub(super) async fn fence(
        &self,
        bound: &BoundKeeperIdentity,
    ) -> Result<BuiltinWireguardHostOutcome, KeeperProviderError> {
        let local = bound.local.clone();
        self.run_blocking("fence WireGuard peers", move |host| {
            host.fence(&local).map_err(KeeperProviderError::Host)
        })
        .await
    }

    /// Applies a complete desired mesh already fenced and validated by Core.
    pub(super) async fn converge_peers(
        &self,
        desired: &DesiredBuiltinWireguardMesh,
    ) -> Result<BuiltinWireguardHostOutcome, KeeperProviderError> {
        let desired = desired.clone();
        self.run_blocking("converge WireGuard peers", move |host| {
            host.converge(&desired).map_err(KeeperProviderError::Host)
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
        Run: FnOnce(
                &mut BuiltinWireguardHost<CancellableHostRunner>,
            ) -> Result<Output, KeeperProviderError>
            + Send
            + 'static,
    {
        let host = Arc::clone(&self.host);
        let (send, mut receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("ployz-keeper-host-fold".to_owned())
            .spawn(move || {
                let result = match host.lock() {
                    Ok(mut host) => run(&mut host),
                    Err(_) => Err(KeeperProviderError::Poisoned),
                };
                let _ = send.send(result);
            })
            .map_err(|source| KeeperProviderError::Task {
                operation,
                detail: source.to_string(),
            })?;
        tokio::select! {
            result = &mut receive => result.map_err(|source| KeeperProviderError::Task {
                operation,
                detail: source.to_string(),
            })?,
            () = tokio::time::sleep(self.fold_timeout) => {
                self.cancelled.store(true, Ordering::Release);
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
    public_key: WireGuardPublicKey,
) -> DesiredBuiltinWireguardLocal {
    let identity = derive_builtin_wireguard_member(cluster_id, &public_key);
    DesiredBuiltinWireguardLocal {
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

#[derive(Debug, thiserror::Error)]
pub(super) enum KeeperProviderError {
    #[error(transparent)]
    Host(#[from] BuiltinWireguardHostError),
    #[error(transparent)]
    Isolation(#[from] ContainerIsolationHostError),
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
    use ployz_host_runner::builtin_wireguard::{
        BuiltinWireguardEbpfConfig, BuiltinWireguardHostConfig, BuiltinWireguardPorts,
    };
    use ployz_host_runner::container_isolation::ContainerIsolationHostConfig;

    fn provider_for_timeout_test(timeout: Duration) -> KeeperMeshProvider {
        let ebpf = BuiltinWireguardEbpfConfig::try_new(
            "br-ployz".to_owned(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/sys/fs/bpf/ployz".into(),
        )
        .expect("eBPF config");
        let host_config = BuiltinWireguardHostConfig::try_new(
            "/etc/ployz/wireguard.key".into(),
            "ployz0".to_owned(),
            BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
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
            host: Arc::new(Mutex::new(BuiltinWireguardHost::with_runner(
                host_config,
                runner,
            ))),
            isolation_host: Arc::new(Mutex::new(ContainerIsolationHost::with_runner(
                ContainerIsolationHostConfig::try_new(
                    "/usr/local/bin/ployz-ebpf-ctl".into(),
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
                    "/sys/fs/bpf/ployz".into(),
                    "/sys/fs/cgroup".into(),
                    "/run/ployz/container-isolation.rows".into(),
                    Duration::from_millis(20),
                )
                .expect("isolation config"),
                CancellableHostRunner {
                    inner: SystemHostRunnerCommandRunner::new(Duration::from_millis(20)),
                    cancelled: Arc::clone(&cancelled),
                },
            ))),
            fold_timeout: timeout,
            cancelled,
        }
    }

    #[tokio::test]
    async fn overall_timeout_returns_without_waiting_for_the_blocking_fold() {
        let provider = provider_for_timeout_test(Duration::from_millis(5));
        let started = std::time::Instant::now();
        let result = provider
            .run_blocking("test fold", move |_| {
                std::thread::sleep(Duration::from_millis(250));
                Ok(())
            })
            .await;

        assert!(matches!(result, Err(KeeperProviderError::TimedOut { .. })));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "the outer timeout must not wait for a blocking fold that ignores cancellation"
        );
    }
}
