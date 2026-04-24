use crate::model::{OverlayIp, PublicKey, management_ip_from_key};
use std::collections::VecDeque;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use defguard_boringtun::noise::{Tunn, TunnResult};
use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer};
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{IpAddress, IpCidr};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

const WG_MTU: usize = 1420;
const TCP_BUF_SIZE: usize = 65535;
const POLL_INTERVAL_MS: u64 = 50;
const MAX_WG_PACKET: usize = 2048;
const RELAY_IO_BUF_SIZE: usize = 4096;

enum DecapAction {
    Stop,
    Continue,
    Drain,
}

struct BridgeDiag {
    started_at: Instant,
    timer_writes: AtomicU64,
    handshake_writes: AtomicU64,
    data_writes: AtomicU64,
    recv_packets: AtomicU64,
    recv_refused: AtomicU64,
    tunnel_packets: AtomicU64,
}

impl BridgeDiag {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            timer_writes: AtomicU64::new(0),
            handshake_writes: AtomicU64::new(0),
            data_writes: AtomicU64::new(0),
            recv_packets: AtomicU64::new(0),
            recv_refused: AtomicU64::new(0),
            tunnel_packets: AtomicU64::new(0),
        }
    }

    fn uptime_ms(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }

    fn mark_handshake_send(&self) {
        self.handshake_writes.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_timer_send(&self) {
        self.timer_writes.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_data_send(&self) {
        self.data_writes.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_recv_packet(&self) {
        self.recv_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_refused(&self) -> u64 {
        self.recv_refused.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn mark_tunnel_packet(&self) -> u64 {
        self.tunnel_packets.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64) {
        (
            self.handshake_writes.load(Ordering::Relaxed),
            self.timer_writes.load(Ordering::Relaxed),
            self.data_writes.load(Ordering::Relaxed),
            self.recv_packets.load(Ordering::Relaxed),
            self.recv_refused.load(Ordering::Relaxed),
            self.tunnel_packets.load(Ordering::Relaxed),
        )
    }
}

/// Bidirectional overlay bridge using boringtun (WireGuard) + smoltcp (userspace TCP/IP).
pub struct OverlayBridge {
    task: JoinHandle<()>,
}

/// Outbound forward rule: localhost listener → overlay destination.
#[derive(Debug, Clone)]
pub struct OutboundForward {
    pub local_addr: SocketAddr,
    pub overlay_dest: SocketAddr,
}

impl OverlayBridge {
    /// Generate an ephemeral x25519 keypair for the bridge.
    /// Returns `(secret, public_key_bytes, overlay_ip)`.
    ///
    /// Call this before registering the bridge as a WG peer, then pass the
    /// secret to `start()` so the handshake happens after peer registration.
    #[must_use]
    pub fn generate_keypair() -> (StaticSecret, [u8; 32], OverlayIp) {
        let secret = StaticSecret::random();
        let public = X25519Public::from(&secret);
        let pub_bytes = public.to_bytes();
        let overlay_ip = management_ip_from_key(&PublicKey(pub_bytes));
        (secret, pub_bytes, overlay_ip)
    }

    /// Start the bridge with a pre-generated keypair.
    ///
    /// - `bridge_secret`: The bridge's x25519 private key (from `generate_keypair()`).
    /// - `container_pubkey_bytes`: The WireGuard public key of the container peer.
    /// - `container_overlay_ip`: The overlay IPv6 assigned to the container interface.
    /// - `peer_endpoint`: UDP endpoint of the container WireGuard (e.g. `127.0.0.1:51820`).
    /// - `outbound`: Forward rules for host→overlay TCP connections.
    pub async fn start(
        bridge_secret: StaticSecret,
        container_pubkey_bytes: &[u8; 32],
        container_overlay_ip: OverlayIp,
        peer_endpoint: SocketAddr,
        outbound: Vec<OutboundForward>,
    ) -> std::io::Result<Self> {
        let bridge_pub_bytes = X25519Public::from(&bridge_secret).to_bytes();
        let overlay_ip = management_ip_from_key(&PublicKey(bridge_pub_bytes));

        let container_pubkey = X25519Public::from(*container_pubkey_bytes);

        // Bind local TCP listeners for outbound forwards
        let mut listeners = Vec::with_capacity(outbound.len());
        for fwd in &outbound {
            let listener = TcpListener::bind(fwd.local_addr).await?;
            info!(local = %fwd.local_addr, overlay = %fwd.overlay_dest, "bridge outbound listener");
            listeners.push((listener, fwd.overlay_dest));
        }

        // Bind UDP socket for WireGuard tunnel
        let udp = UdpSocket::bind("0.0.0.0:0").await?;
        let udp_local = udp.local_addr().ok();
        info!(%peer_endpoint, ?udp_local, bridge_ip = %overlay_ip, "bridge tunnel started");

        let bridge_ip_v6 = overlay_ip.0;

        let task = tokio::spawn(async move {
            if let Err(e) = bridge_event_loop(
                bridge_secret,
                container_pubkey,
                container_overlay_ip.0,
                bridge_ip_v6,
                peer_endpoint,
                udp,
                listeners,
            )
            .await
            {
                error!(?e, "bridge event loop exited with error");
            }
        });

        Ok(Self { task })
    }

    pub async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
        info!("bridge stopped");
    }
}

// --- Virtual device for smoltcp ---

struct VirtualDevice {
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
}

impl VirtualDevice {
    fn new() -> Self {
        Self {
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
        }
    }

    fn push_rx(&mut self, packet: Vec<u8>) {
        self.rx_queue.push_back(packet);
    }

    fn pop_tx(&mut self) -> Option<Vec<u8>> {
        self.tx_queue.pop_front()
    }
}

struct VirtualRxToken(Vec<u8>);

impl RxToken for VirtualRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

struct VirtualTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl<'a> TxToken for VirtualTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        self.0.push_back(buf);
        result
    }
}

impl Device for VirtualDevice {
    type RxToken<'a> = VirtualRxToken;
    type TxToken<'a> = VirtualTxToken<'a>;

    fn receive(
        &mut self,
        _timestamp: SmoltcpInstant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let packet = self.rx_queue.pop_front()?;
        Some((VirtualRxToken(packet), VirtualTxToken(&mut self.tx_queue)))
    }

    fn transmit(&mut self, _timestamp: SmoltcpInstant) -> Option<Self::TxToken<'_>> {
        Some(VirtualTxToken(&mut self.tx_queue))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = WG_MTU;
        caps
    }
}

// --- Event loop ---

/// State for a single outbound TCP relay (host TCP ↔ smoltcp TCP socket).
struct OutboundRelay {
    stream: TcpStream,
    smoltcp_handle: smoltcp::iface::SocketHandle,
    overlay_dest: SocketAddr,
    connected: bool,
}

struct BridgeState<'a, 'sockets> {
    diag: &'a BridgeDiag,
    udp: &'a UdpSocket,
    peer_endpoint: SocketAddr,
    tunn: &'a mut Tunn,
    iface: &'a mut Interface,
    device: &'a mut VirtualDevice,
    sockets: &'a mut SocketSet<'sockets>,
    relays: &'a mut Vec<OutboundRelay>,
    timer_scratch: &'a mut [u8],
    decode_scratch: &'a mut [u8],
    drain_scratch: &'a mut [u8],
    wg_scratch: &'a mut [u8],
}

struct BridgeScratch<'a> {
    timer: &'a mut [u8],
    decode: &'a mut [u8],
    drain: &'a mut [u8],
    wg: &'a mut [u8],
}

impl<'a, 'sockets> BridgeState<'a, 'sockets> {
    fn new(
        diag: &'a BridgeDiag,
        udp: &'a UdpSocket,
        peer_endpoint: SocketAddr,
        tunn: &'a mut Tunn,
        iface: &'a mut Interface,
        device: &'a mut VirtualDevice,
        sockets: &'a mut SocketSet<'sockets>,
        relays: &'a mut Vec<OutboundRelay>,
        scratch: BridgeScratch<'a>,
    ) -> Self {
        Self {
            diag,
            udp,
            peer_endpoint,
            tunn,
            iface,
            device,
            sockets,
            relays,
            timer_scratch: scratch.timer,
            decode_scratch: scratch.decode,
            drain_scratch: scratch.drain,
            wg_scratch: scratch.wg,
        }
    }

    async fn handle_tick(&mut self) {
        let now = SmoltcpInstant::now();

        match self.tunn.update_timers(self.timer_scratch) {
            TunnResult::WriteToNetwork(data) => {
                self.diag.mark_timer_send();
                let _ = self.udp.send_to(data, self.peer_endpoint).await;
            }
            TunnResult::Err(e) => {
                debug!(?e, "WG timer error");
            }
            TunnResult::Done
            | TunnResult::WriteToTunnelV4(..)
            | TunnResult::WriteToTunnelV6(..) => {}
        }

        pump_relays(self.relays, self.sockets);
        retain_open_relays(self.relays, self.sockets);

        self.iface.poll(now, self.device, self.sockets);
        self.flush_device_tx_to_wg().await;
    }

    async fn handle_udp_packet(&mut self, packet: &[u8]) {
        let result = self.tunn.decapsulate(None, packet, self.decode_scratch);
        if let DecapAction::Drain =
            handle_decapsulate_result(self.diag, self.udp, self.peer_endpoint, self.device, result)
                .await
        {
            self.drain_post_handshake_decaps().await;
        }

        self.iface
            .poll(SmoltcpInstant::now(), self.device, self.sockets);
        self.flush_device_tx_to_wg().await;
    }

    async fn handle_new_connection(
        &mut self,
        stream: TcpStream,
        overlay_dest: SocketAddr,
        local_addr: SocketAddr,
    ) {
        debug!(%local_addr, %overlay_dest, "new outbound connection");

        let rx_buf = SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
        let tx_buf = SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
        let mut tcp = TcpSocket::new(rx_buf, tx_buf);

        let dest_ip = match overlay_dest.ip() {
            std::net::IpAddr::V6(v6) => ipv6_to_smoltcp(v6),
            std::net::IpAddr::V4(_) => {
                warn!("IPv4 overlay destinations not supported");
                return;
            }
        };

        let local_port = 49152 + (self.relays.len() as u16 % 16000);
        let cx = self.iface.context();
        if let Err(e) = tcp.connect(cx, (dest_ip, overlay_dest.port()), local_port) {
            warn!(?e, dest = %overlay_dest, "smoltcp connect failed");
            return;
        }
        let handle = self.sockets.add(tcp);
        self.relays.push(OutboundRelay {
            stream,
            smoltcp_handle: handle,
            overlay_dest,
            connected: false,
        });

        self.iface
            .poll(SmoltcpInstant::now(), self.device, self.sockets);
        self.flush_device_tx_to_wg().await;
    }

    async fn drain_post_handshake_decaps(&mut self) {
        loop {
            let result = self.tunn.decapsulate(None, &[], self.drain_scratch);
            if matches!(
                handle_decapsulate_result(
                    self.diag,
                    self.udp,
                    self.peer_endpoint,
                    self.device,
                    result,
                )
                .await,
                DecapAction::Stop
            ) {
                break;
            }
        }
    }

    async fn flush_device_tx_to_wg(&mut self) {
        flush_device_tx_to_wg(
            self.diag,
            self.udp,
            self.peer_endpoint,
            self.tunn,
            self.device,
            self.wg_scratch,
        )
        .await;
    }
}

#[allow(clippy::indexing_slicing)]
async fn bridge_event_loop(
    bridge_secret: StaticSecret,
    container_pubkey: X25519Public,
    container_overlay_ip: Ipv6Addr,
    bridge_ip: Ipv6Addr,
    peer_endpoint: SocketAddr,
    udp: UdpSocket,
    listeners: Vec<(TcpListener, SocketAddr)>,
) -> std::io::Result<()> {
    let diag = Arc::new(BridgeDiag::new());

    // Create boringtun tunnel
    let mut tunn = Tunn::new(bridge_secret, container_pubkey, None, None, 0, None);

    // Create smoltcp interface
    let mut device = VirtualDevice::new();
    let config = IfaceConfig::new(smoltcp::wire::HardwareAddress::Ip);
    let mut iface = Interface::new(config, &mut device, SmoltcpInstant::now());

    let bridge_smoltcp_addr = ipv6_to_smoltcp(bridge_ip);
    iface.update_ip_addrs(|addrs| {
        addrs
            .push(IpCidr::new(bridge_smoltcp_addr, 128))
            .expect("smoltcp address capacity");
    });

    // Default IPv6 route — required for smoltcp to send to non-local overlay IPs.
    // Gateway is the container's overlay IP (all traffic goes through the WG tunnel).
    let container_segs = container_overlay_ip.segments();
    let gateway = smoltcp::wire::Ipv6Address::new(
        container_segs[0],
        container_segs[1],
        container_segs[2],
        container_segs[3],
        container_segs[4],
        container_segs[5],
        container_segs[6],
        container_segs[7],
    );
    iface
        .routes_mut()
        .add_default_ipv6_route(gateway)
        .expect("smoltcp default route capacity");
    debug!(bridge = %bridge_ip, %gateway, "smoltcp interface configured");

    let mut sockets = SocketSet::new(Vec::new());
    let mut relays: Vec<OutboundRelay> = Vec::new();

    let mut udp_recv_buf = [0u8; MAX_WG_PACKET];
    let mut timer_scratch = [0u8; MAX_WG_PACKET];
    let mut decode_scratch = [0u8; MAX_WG_PACKET];
    let mut drain_scratch = [0u8; MAX_WG_PACKET];
    let mut wg_scratch = [0u8; MAX_WG_PACKET];
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(POLL_INTERVAL_MS));

    // Initiate WireGuard handshake (best-effort — container port may not be ready yet,
    // boringtun timers will retry automatically)
    let mut handshake_buf = [0u8; MAX_WG_PACKET];
    if let TunnResult::WriteToNetwork(data) =
        tunn.format_handshake_initiation(&mut handshake_buf, false)
    {
        diag.mark_handshake_send();
        match udp.send_to(data, peer_endpoint).await {
            Ok(_) => debug!("sent WG handshake initiation"),
            Err(e) => debug!(?e, "handshake send failed, will retry via timers"),
        }
    }

    loop {
        tokio::select! {
            // Timer: WG keepalives + smoltcp poll + relay data
            _ = interval.tick() => {
                let mut state = BridgeState::new(
                    diag.as_ref(),
                    &udp,
                    peer_endpoint,
                    &mut tunn,
                    &mut iface,
                    &mut device,
                    &mut sockets,
                    &mut relays,
                    BridgeScratch {
                        timer: &mut timer_scratch,
                        decode: &mut decode_scratch,
                        drain: &mut drain_scratch,
                        wg: &mut wg_scratch,
                    },
                );
                state.handle_tick().await;
            }

            // UDP recv: WG encrypted packets from container
            result = udp.recv_from(&mut udp_recv_buf) => {
                let (n, src) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        // On some platforms, transient ICMP unreachable can surface as UDP recv errors.
                        let refused_count = diag.mark_refused();
                        if refused_count <= 5 || refused_count.is_multiple_of(5) {
                            let (h, t, d, r, c, tp) = diag.snapshot();
                            debug!(
                                ?e,
                                refused_count,
                                uptime_ms = diag.uptime_ms(),
                                handshake_sends = h,
                                timer_sends = t,
                                data_sends = d,
                                recv_packets = r,
                                refused_packets = c,
                                tunnel_packets = tp,
                                "UDP recv error"
                            );
                        }
                        continue;
                    }
                };

                if src != peer_endpoint {
                    debug!(%src, expected = %peer_endpoint, "ignoring UDP packet from unexpected source");
                    continue;
                }

                diag.mark_recv_packet();
                let mut state = BridgeState::new(
                    diag.as_ref(),
                    &udp,
                    peer_endpoint,
                    &mut tunn,
                    &mut iface,
                    &mut device,
                    &mut sockets,
                    &mut relays,
                    BridgeScratch {
                        timer: &mut timer_scratch,
                        decode: &mut decode_scratch,
                        drain: &mut drain_scratch,
                        wg: &mut wg_scratch,
                    },
                );
                state.handle_udp_packet(&udp_recv_buf[..n]).await;
            }

            // Accept new outbound TCP connections
            result = accept_any(&listeners) => {
                if let Some((stream, overlay_dest, local_addr)) = result {
                    let mut state = BridgeState::new(
                        diag.as_ref(),
                        &udp,
                        peer_endpoint,
                        &mut tunn,
                        &mut iface,
                        &mut device,
                        &mut sockets,
                        &mut relays,
                        BridgeScratch {
                            timer: &mut timer_scratch,
                            decode: &mut decode_scratch,
                            drain: &mut drain_scratch,
                            wg: &mut wg_scratch,
                        },
                    );
                    state
                        .handle_new_connection(stream, overlay_dest, local_addr)
                        .await;
                }
            }
        }
    }
}

async fn handle_decapsulate_result(
    diag: &BridgeDiag,
    udp: &UdpSocket,
    peer_endpoint: SocketAddr,
    device: &mut VirtualDevice,
    result: TunnResult<'_>,
) -> DecapAction {
    match result {
        TunnResult::WriteToNetwork(data) => {
            send_wg_packet(diag, udp, peer_endpoint, data).await;
            DecapAction::Drain
        }
        TunnResult::WriteToTunnelV6(data, _) => {
            queue_tunnel_packet(diag, device, data);
            DecapAction::Continue
        }
        TunnResult::Err(e) => {
            debug!(?e, "WG decapsulate error");
            DecapAction::Stop
        }
        TunnResult::Done | TunnResult::WriteToTunnelV4(..) => DecapAction::Stop,
    }
}

fn track_tunnel_packet(diag: &BridgeDiag) {
    let tunnel_count = diag.mark_tunnel_packet();
    if tunnel_count == 1 {
        let (h, t, d, r, c, tp) = diag.snapshot();
        info!(
            uptime_ms = diag.uptime_ms(),
            handshake_sends = h,
            timer_sends = t,
            data_sends = d,
            recv_packets = r,
            refused_packets = c,
            tunnel_packets = tp,
            "bridge received first tunnel packet"
        );
    }
}

fn queue_tunnel_packet(diag: &BridgeDiag, device: &mut VirtualDevice, data: &[u8]) {
    track_tunnel_packet(diag);
    device.push_rx(data.to_vec());
}

async fn send_wg_packet(
    diag: &BridgeDiag,
    udp: &UdpSocket,
    peer_endpoint: SocketAddr,
    data: &[u8],
) {
    diag.mark_data_send();
    let _ = udp.send_to(data, peer_endpoint).await;
}

async fn flush_device_tx_to_wg(
    diag: &BridgeDiag,
    udp: &UdpSocket,
    peer_endpoint: SocketAddr,
    tunn: &mut Tunn,
    device: &mut VirtualDevice,
    wg_scratch: &mut [u8],
) {
    while let Some(ip_packet) = device.pop_tx() {
        match tunn.encapsulate(&ip_packet, wg_scratch) {
            TunnResult::WriteToNetwork(data) => {
                send_wg_packet(diag, udp, peer_endpoint, data).await;
            }
            TunnResult::Err(e) => {
                debug!(?e, "WG encapsulate error");
            }
            TunnResult::Done
            | TunnResult::WriteToTunnelV4(..)
            | TunnResult::WriteToTunnelV6(..) => {}
        }
    }
}

fn pump_relays(relays: &mut [OutboundRelay], sockets: &mut SocketSet<'_>) {
    for relay in relays.iter_mut() {
        let socket = sockets.get_mut::<TcpSocket>(relay.smoltcp_handle);
        pump_relay(relay, socket);
    }
}

fn pump_relay(relay: &mut OutboundRelay, socket: &mut TcpSocket<'_>) {
    if !relay.connected && socket.may_send() {
        relay.connected = true;
        debug!(dest = %relay.overlay_dest, "smoltcp TCP connected");
    }

    read_host_into_overlay(relay, socket);
    write_overlay_to_host(relay, socket);
}

fn read_host_into_overlay(relay: &mut OutboundRelay, socket: &mut TcpSocket<'_>) {
    if !socket.can_send() {
        return;
    }

    let mut buf = [0u8; RELAY_IO_BUF_SIZE];
    handle_host_read_result(socket, relay.stream.try_read(&mut buf), &buf);
}

fn handle_host_read_result(socket: &mut TcpSocket<'_>, result: std::io::Result<usize>, buf: &[u8]) {
    match result {
        Ok(0) => {
            socket.close();
        }
        Ok(n) => {
            let _ = socket.send_slice(&buf[..n]);
        }
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => {
            debug!(?e, "host TCP read error");
            socket.abort();
        }
    }
}

fn write_overlay_to_host(relay: &mut OutboundRelay, socket: &mut TcpSocket<'_>) {
    if !socket.can_recv() {
        return;
    }

    let mut buf = [0u8; RELAY_IO_BUF_SIZE];
    match socket.recv_slice(&mut buf) {
        Ok(n) if n > 0 => match relay.stream.try_write(&buf[..n]) {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                debug!(?e, "host TCP write error");
                socket.abort();
            }
        },
        _ => {}
    }
}

fn retain_open_relays(relays: &mut Vec<OutboundRelay>, sockets: &SocketSet<'_>) {
    relays.retain(|relay| {
        let socket = sockets.get::<TcpSocket>(relay.smoltcp_handle);
        socket.is_open() || socket.may_recv() || socket.may_send()
    });
}

fn ipv6_to_smoltcp(addr: Ipv6Addr) -> IpAddress {
    let segs = addr.segments();
    IpAddress::Ipv6(smoltcp::wire::Ipv6Address::new(
        segs[0], segs[1], segs[2], segs[3], segs[4], segs[5], segs[6], segs[7],
    ))
}

/// Accept a connection from any of the listeners, racing all of them.
async fn accept_any(
    listeners: &[(TcpListener, SocketAddr)],
) -> Option<(TcpStream, SocketAddr, SocketAddr)> {
    if listeners.is_empty() {
        std::future::pending::<()>().await;
        return None;
    }

    // Build a FuturesUnordered set so all listeners are polled concurrently.
    let mut futs = futures_util::stream::FuturesUnordered::new();
    for (listener, dest) in listeners {
        let dest = *dest;
        futs.push(async move {
            let (stream, addr) = listener.accept().await.ok()?;
            Some((stream, dest, addr))
        });
    }

    // Return the first listener to accept a connection.
    use futures_util::StreamExt;
    while let Some(result) = futs.next().await {
        if result.is_some() {
            return result;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv6Addr};

    fn new_tcp_socket() -> TcpSocket<'static> {
        let rx_buf = SocketBuffer::new(vec![0u8; 1024]);
        let tx_buf = SocketBuffer::new(vec![0u8; 1024]);
        TcpSocket::new(rx_buf, tx_buf)
    }

    fn create_two_tuns() -> (Tunn, Tunn) {
        let secret_a = StaticSecret::from([1; 32]);
        let secret_b = StaticSecret::from([2; 32]);
        let pub_a = X25519Public::from(&secret_a);
        let pub_b = X25519Public::from(&secret_b);
        (
            Tunn::new(secret_a, pub_b, None, None, 0, None),
            Tunn::new(secret_b, pub_a, None, None, 0, None),
        )
    }

    fn create_handshake_init(tunn: &mut Tunn) -> Vec<u8> {
        let mut dst = [0u8; MAX_WG_PACKET];
        let TunnResult::WriteToNetwork(packet) = tunn.format_handshake_initiation(&mut dst, false)
        else {
            panic!("expected handshake init");
        };
        packet.to_vec()
    }

    fn create_handshake_response(tunn: &mut Tunn, packet: &[u8]) -> Vec<u8> {
        let mut dst = [0u8; MAX_WG_PACKET];
        let TunnResult::WriteToNetwork(response) = tunn.decapsulate(None, packet, &mut dst) else {
            panic!("expected handshake response");
        };
        response.to_vec()
    }

    fn parse_handshake_response(tunn: &mut Tunn, packet: &[u8]) -> Vec<u8> {
        let mut dst = [0u8; MAX_WG_PACKET];
        let TunnResult::WriteToNetwork(keepalive) = tunn.decapsulate(None, packet, &mut dst) else {
            panic!("expected keepalive");
        };
        keepalive.to_vec()
    }

    fn create_two_tuns_and_handshake() -> (Tunn, Tunn) {
        let (mut left, mut right) = create_two_tuns();
        let init = create_handshake_init(&mut left);
        let response = create_handshake_response(&mut right, &init);
        let keepalive = parse_handshake_response(&mut left, &response);

        let mut dst = [0u8; MAX_WG_PACKET];
        let result = right.decapsulate(None, &keepalive, &mut dst);
        assert!(matches!(result, TunnResult::Done));

        (left, right)
    }

    fn sample_ipv6_packet() -> Vec<u8> {
        let payload = [9u8, 8, 7, 6];
        let mut packet = vec![0u8; 40 + payload.len()];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        packet[6] = 17;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&Ipv6Addr::LOCALHOST.octets());
        packet[24..40].copy_from_slice(&Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2).octets());
        packet[40..].copy_from_slice(&payload);
        packet
    }

    fn tcp_flags(packet: &[u8]) -> u8 {
        packet[53]
    }

    fn ipv6_source(packet: &[u8]) -> Ipv6Addr {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&packet[8..24]);
        Ipv6Addr::from(octets)
    }

    fn ipv6_destination(packet: &[u8]) -> Ipv6Addr {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&packet[24..40]);
        Ipv6Addr::from(octets)
    }

    fn tcp_source_port(packet: &[u8]) -> u16 {
        u16::from_be_bytes([packet[40], packet[41]])
    }

    fn tcp_destination_port(packet: &[u8]) -> u16 {
        u16::from_be_bytes([packet[42], packet[43]])
    }

    async fn connected_tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp listener");
        let addr = listener.local_addr().expect("listener addr");
        let client = TcpStream::connect(addr).await.expect("connect tcp");
        let (server, _) = listener.accept().await.expect("accept tcp");
        (client, server)
    }

    #[tokio::test]
    async fn handle_decapsulate_result_sends_network_packets_and_requests_drain() {
        let diag = BridgeDiag::new();
        let send_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind send udp");
        let recv_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind recv udp");
        let peer_endpoint = recv_udp.local_addr().expect("recv addr");
        let mut device = VirtualDevice::new();
        let mut packet = [1u8, 2, 3, 4];

        let action = handle_decapsulate_result(
            &diag,
            &send_udp,
            peer_endpoint,
            &mut device,
            TunnResult::WriteToNetwork(&mut packet),
        )
        .await;

        assert!(matches!(action, DecapAction::Drain));
        let mut recv_buf = [0u8; 16];
        let (n, src) = recv_udp
            .recv_from(&mut recv_buf)
            .await
            .expect("recv udp packet");
        assert_eq!(src, send_udp.local_addr().expect("send addr"));
        assert_eq!(&recv_buf[..n], &[1u8, 2, 3, 4]);
        assert_eq!(diag.snapshot().2, 1);
        assert!(device.rx_queue.is_empty());
    }

    #[tokio::test]
    async fn handle_decapsulate_result_queues_tunnel_packet() {
        let diag = BridgeDiag::new();
        let send_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind send udp");
        let recv_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind recv udp");
        let peer_endpoint = recv_udp.local_addr().expect("recv addr");
        let mut device = VirtualDevice::new();
        let mut packet = sample_ipv6_packet();

        let action = handle_decapsulate_result(
            &diag,
            &send_udp,
            peer_endpoint,
            &mut device,
            TunnResult::WriteToTunnelV6(&mut packet, Ipv6Addr::LOCALHOST),
        )
        .await;

        assert!(matches!(action, DecapAction::Continue));
        let [queued] = device.rx_queue.make_contiguous() else {
            panic!("expected queued tunnel packet");
        };
        assert_eq!(queued, &sample_ipv6_packet());
        assert_eq!(diag.snapshot().5, 1);
    }

    #[tokio::test]
    async fn flush_device_tx_to_wg_sends_encrypted_packet_to_peer() {
        let diag = BridgeDiag::new();
        let send_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind send udp");
        let recv_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind recv udp");
        let peer_endpoint = recv_udp.local_addr().expect("recv addr");
        let (mut left_tunn, mut right_tunn) = create_two_tuns_and_handshake();
        let mut device = VirtualDevice::new();
        let packet = sample_ipv6_packet();
        device.tx_queue.push_back(packet.clone());
        let mut wg_scratch = [0u8; MAX_WG_PACKET];

        flush_device_tx_to_wg(
            &diag,
            &send_udp,
            peer_endpoint,
            &mut left_tunn,
            &mut device,
            &mut wg_scratch,
        )
        .await;

        let mut recv_buf = [0u8; MAX_WG_PACKET];
        let (n, _) = recv_udp
            .recv_from(&mut recv_buf)
            .await
            .expect("recv encrypted packet");
        let mut decode_scratch = [0u8; MAX_WG_PACKET];
        let result = right_tunn.decapsulate(None, &recv_buf[..n], &mut decode_scratch);
        let TunnResult::WriteToTunnelV6(recv_packet, recv_addr) = result else {
            panic!("expected tunnel packet");
        };

        assert_eq!(recv_addr, Ipv6Addr::LOCALHOST);
        assert_eq!(recv_packet, packet.as_slice());
        assert_eq!(diag.snapshot().2, 1);
        assert!(device.tx_queue.is_empty());
    }

    #[test]
    fn handle_host_read_result_closes_socket_on_eof() {
        let mut socket = new_tcp_socket();
        socket.listen(8080).expect("listen socket");
        assert!(socket.is_open());

        handle_host_read_result(&mut socket, Ok(0), &[]);

        assert!(!socket.is_open());
    }

    #[tokio::test]
    async fn retain_open_relays_drops_closed_relays() {
        let (stream_a, peer_a) = connected_tcp_pair().await;
        let (stream_b, peer_b) = connected_tcp_pair().await;

        let mut sockets = SocketSet::new(Vec::new());
        let mut open_socket = new_tcp_socket();
        open_socket.listen(8081).expect("listen socket");
        let open_handle = sockets.add(open_socket);
        let closed_handle = sockets.add(new_tcp_socket());

        let open_dest = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8081);
        let closed_dest = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8082);
        let mut relays = vec![
            OutboundRelay {
                stream: stream_a,
                smoltcp_handle: open_handle,
                overlay_dest: open_dest,
                connected: false,
            },
            OutboundRelay {
                stream: stream_b,
                smoltcp_handle: closed_handle,
                overlay_dest: closed_dest,
                connected: false,
            },
        ];

        retain_open_relays(&mut relays, &sockets);

        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].overlay_dest, open_dest);

        drop(peer_a);
        drop(peer_b);
    }

    #[tokio::test]
    async fn accept_and_new_connection_flushes_syn_through_wireguard() {
        let diag = BridgeDiag::new();
        let send_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind send udp");
        let recv_udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind recv udp");
        let peer_endpoint = recv_udp.local_addr().expect("recv addr");
        let bridge_ip = Ipv6Addr::LOCALHOST;
        let overlay_dest_ip = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2);
        let overlay_dest = SocketAddr::new(IpAddr::V6(overlay_dest_ip), 8080);
        let (mut left_tunn, mut right_tunn) = create_two_tuns_and_handshake();

        let mut device = VirtualDevice::new();
        let config = IfaceConfig::new(smoltcp::wire::HardwareAddress::Ip);
        let mut iface = Interface::new(config, &mut device, SmoltcpInstant::now());
        let bridge_smoltcp_addr = ipv6_to_smoltcp(bridge_ip);
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(bridge_smoltcp_addr, 128))
                .expect("smoltcp address capacity");
        });
        let overlay_dest_segments = overlay_dest_ip.segments();
        iface
            .routes_mut()
            .add_default_ipv6_route(smoltcp::wire::Ipv6Address::new(
                overlay_dest_segments[0],
                overlay_dest_segments[1],
                overlay_dest_segments[2],
                overlay_dest_segments[3],
                overlay_dest_segments[4],
                overlay_dest_segments[5],
                overlay_dest_segments[6],
                overlay_dest_segments[7],
            ))
            .expect("smoltcp default route capacity");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind outbound listener");
        let listener_addr = listener.local_addr().expect("listener addr");
        let client = TcpStream::connect(listener_addr)
            .await
            .expect("connect to outbound listener");
        let listeners = vec![(listener, overlay_dest)];
        let Some((stream, accepted_overlay_dest, local_addr)) = accept_any(&listeners).await else {
            panic!("expected accepted connection");
        };

        let mut sockets = SocketSet::new(Vec::new());
        let mut relays = Vec::new();
        let mut timer_scratch = [0u8; MAX_WG_PACKET];
        let mut decode_scratch = [0u8; MAX_WG_PACKET];
        let mut drain_scratch = [0u8; MAX_WG_PACKET];
        let mut wg_scratch = [0u8; MAX_WG_PACKET];

        let mut state = BridgeState {
            diag: &diag,
            udp: &send_udp,
            peer_endpoint,
            tunn: &mut left_tunn,
            iface: &mut iface,
            device: &mut device,
            sockets: &mut sockets,
            relays: &mut relays,
            timer_scratch: &mut timer_scratch,
            decode_scratch: &mut decode_scratch,
            drain_scratch: &mut drain_scratch,
            wg_scratch: &mut wg_scratch,
        };

        state
            .handle_new_connection(stream, accepted_overlay_dest, local_addr)
            .await;

        let mut recv_buf = [0u8; MAX_WG_PACKET];
        let (n, _) = recv_udp
            .recv_from(&mut recv_buf)
            .await
            .expect("recv encrypted syn");
        let mut peer_decode_scratch = [0u8; MAX_WG_PACKET];
        let result = right_tunn.decapsulate(None, &recv_buf[..n], &mut peer_decode_scratch);
        let TunnResult::WriteToTunnelV6(packet, recv_addr) = result else {
            panic!("expected tunneled ipv6 tcp packet");
        };

        assert_eq!(recv_addr, bridge_ip);
        assert_eq!(ipv6_source(packet), bridge_ip);
        assert_eq!(ipv6_destination(packet), overlay_dest_ip);
        assert_eq!(tcp_source_port(packet), 49152);
        assert_eq!(tcp_destination_port(packet), 8080);
        assert_ne!(tcp_flags(packet) & 0x02, 0);
        assert_eq!(tcp_flags(packet) & 0x10, 0);
        assert_eq!(state.relays.len(), 1);
        assert_eq!(state.relays[0].overlay_dest, overlay_dest);
        assert_eq!(diag.snapshot().2, 1);

        drop(client);
    }
}
