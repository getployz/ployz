use crate::error::{Error, Result};
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineRecord, PublicKey};
use std::sync::{Mutex, MutexGuard};

pub struct MemoryWireGuard {
    inner: Mutex<WgInner>,
}

struct WgInner {
    is_up: bool,
    peers: Vec<MachineRecord>,
    device_peers: Vec<DevicePeer>,
    set_peers_count: usize,
    fail_up: bool,
    fail_down: bool,
}

impl Default for MemoryWireGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryWireGuard {
    #[must_use] 
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(WgInner {
                is_up: false,
                peers: Vec::new(),
                device_peers: Vec::new(),
                set_peers_count: 0,
                fail_up: false,
                fail_down: false,
            }),
        }
    }

    fn lock_inner(&self) -> MutexGuard<'_, WgInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_fail_up(&self, fail: bool) {
        self.lock_inner().fail_up = fail;
    }

    pub fn set_fail_down(&self, fail: bool) {
        self.lock_inner().fail_down = fail;
    }

    pub fn is_up(&self) -> bool {
        self.lock_inner().is_up
    }

    pub fn set_peers_count(&self) -> usize {
        self.lock_inner().set_peers_count
    }

    pub fn current_peers(&self) -> Vec<MachineRecord> {
        self.lock_inner().peers.clone()
    }

    pub fn set_device_peers(&self, peers: Vec<DevicePeer>) {
        self.lock_inner().device_peers = peers;
    }
}

impl MeshNetwork for MemoryWireGuard {
    async fn up(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_up {
            return Err(Error::operation("wireguard up", "injected failure"));
        }
        inner.is_up = true;
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_down {
            return Err(Error::operation("wireguard down", "injected failure"));
        }
        inner.is_up = false;
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.peers = peers.to_vec();
        inner.set_peers_count += 1;
        Ok(())
    }
}

impl WireGuardDevice for MemoryWireGuard {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        Ok(self.lock_inner().device_peers.clone())
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        let mut inner = self.lock_inner();
        for peer in &mut inner.device_peers {
            if peer.public_key == *key {
                peer.endpoint = Some(endpoint.to_string());
            }
        }
        Ok(())
    }
}
