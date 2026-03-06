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
    overlay_ip: OverlayIp,
    public_key: X25519Public,
    task: JoinHandle<()>,
}

/// Outbound forward rule: localhost listener → overlay destination.
#[derive(Debug, Clone)]
pub struct OutboundForward {
    pub local_addr: SocketAddr,
    pub overlay_dest: SocketAddr,
}

/// Inbound forward rule: overlay port → localhost destination.
#[derive(Debug, Clone)]
pub struct InboundForward {
    pub overlay_port: u16,
    pub local_dest: SocketAddr,
}

impl OverlayBridge {
    /// Generate an ephemeral x25519 keypair for the bridge.
    /// Returns `(secret, public_key_bytes, overlay_ip)`.
    ///
    /// Call this before registering the bridge as a WG peer, then pass the
    /// secret to `start()` so the handshake happens after peer registration.
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
    /// - `inbound`: Forward rules for overlay→host TCP connections.
    pub async fn start(
        bridge_secret: StaticSecret,
        container_pubkey_bytes: &[u8; 32],
        container_overlay_ip: OverlayIp,
        peer_endpoint: SocketAddr,
        outbound: Vec<OutboundForward>,
        inbound: Vec<InboundForward>,
    ) -> std::io::Result<Self> {
        let bridge_public = X25519Public::from(&bridge_secret);
        let bridge_pub_bytes: [u8; 32] = bridge_public.to_bytes();
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
        let inbound_rules = inbound.clone();

        let task = tokio::spawn(async move {
            if let Err(e) = bridge_event_loop(
                bridge_secret,
                container_pubkey,
                container_overlay_ip.0,
                bridge_ip_v6,
                peer_endpoint,
                udp,
                listeners,
                inbound_rules,
            )
            .await
            {
                error!(?e, "bridge event loop exited with error");
            }
        });

        Ok(Self {
            overlay_ip,
            public_key: bridge_public,
            task,
        })
    }

    pub fn overlay_ip(&self) -> OverlayIp {
        self.overlay_ip
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.public_key.to_bytes()
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

async fn bridge_event_loop(
    bridge_secret: StaticSecret,
    container_pubkey: X25519Public,
    container_overlay_ip: Ipv6Addr,
    bridge_ip: Ipv6Addr,
    peer_endpoint: SocketAddr,
    udp: UdpSocket,
    listeners: Vec<(TcpListener, SocketAddr)>,
    _inbound_rules: Vec<InboundForward>,
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
        addrs.push(IpCidr::new(bridge_smoltcp_addr, 128)).unwrap();
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
    iface.routes_mut().add_default_ipv6_route(gateway).unwrap();
    debug!(bridge = %bridge_ip, %gateway, "smoltcp interface configured");

    let mut sockets = SocketSet::new(Vec::new());
    let mut relays: Vec<OutboundRelay> = Vec::new();

    let mut udp_recv_buf = vec![0u8; MAX_WG_PACKET];
    let mut wg_scratch = vec![0u8; MAX_WG_PACKET];
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(POLL_INTERVAL_MS));

    // Initiate WireGuard handshake (best-effort — container port may not be ready yet,
    // boringtun timers will retry automatically)
    let mut handshake_buf = vec![0u8; MAX_WG_PACKET];
    match tunn.format_handshake_initiation(&mut handshake_buf, false) {
        TunnResult::WriteToNetwork(data) => {
            diag.mark_handshake_send();
            match udp.send_to(data, peer_endpoint).await {
                Ok(_) => debug!("sent WG handshake initiation"),
                Err(e) => debug!(?e, "handshake send failed, will retry via timers"),
            }
        }
        _ => {}
    }

    loop {
        tokio::select! {
            // Timer: WG keepalives + smoltcp poll + relay data
            _ = interval.tick() => {
                let now = SmoltcpInstant::now();

                // WG timers
                let mut timer_buf = vec![0u8; MAX_WG_PACKET];
                match tunn.update_timers(&mut timer_buf) {
                    TunnResult::WriteToNetwork(data) => {
                        diag.mark_timer_send();
                        let _ = udp.send_to(data, peer_endpoint).await;
                    }
                    TunnResult::Err(e) => {
                        debug!(?e, "WG timer error");
                    }
                    _ => {}
                }

                // Relay data from host TCP → smoltcp sockets
                for relay in &mut relays {
                    let socket = sockets.get_mut::<TcpSocket>(relay.smoltcp_handle);

                    if !relay.connected && socket.may_send() {
                        relay.connected = true;
                        debug!(dest = %relay.overlay_dest, "smoltcp TCP connected");
                    }

                    // Host → overlay: read from host TCP, write to smoltcp socket
                    if socket.can_send() {
                        let mut buf = [0u8; 4096];
                        match relay.stream.try_read(&mut buf) {
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

                    // Overlay → host: read from smoltcp socket, write to host TCP
                    if socket.can_recv() {
                        let mut buf = [0u8; 4096];
                        match socket.recv_slice(&mut buf) {
                            Ok(n) if n > 0 => {
                                match relay.stream.try_write(&buf[..n]) {
                                    Ok(_) => {}
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        // Data will be retried next poll
                                    }
                                    Err(e) => {
                                        debug!(?e, "host TCP write error");
                                        socket.abort();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // Remove closed relays
                relays.retain(|r| {
                    let socket = sockets.get::<TcpSocket>(r.smoltcp_handle);
                    socket.is_open() || socket.may_recv() || socket.may_send()
                });

                // smoltcp poll — processes rx packets, generates tx packets
                iface.poll(now, &mut device, &mut sockets);

                // Send any outbound IP packets through WG tunnel
                while let Some(ip_packet) = device.pop_tx() {
                    match tunn.encapsulate(&ip_packet, &mut wg_scratch) {
                        TunnResult::WriteToNetwork(data) => {
                            diag.mark_data_send();
                            let _ = udp.send_to(data, peer_endpoint).await;
                        }
                        TunnResult::Err(e) => {
                            debug!(?e, "WG encapsulate error");
                        }
                        _ => {}
                    }
                }
            }

            // UDP recv: WG encrypted packets from container
            result = udp.recv_from(&mut udp_recv_buf) => {
                let (n, src) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        // On some platforms, transient ICMP unreachable can surface as UDP recv errors.
                        let refused_count = diag.mark_refused();
                        if refused_count <= 5 || refused_count % 5 == 0 {
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
                let packet = &udp_recv_buf[..n];

                let mut decode_buf = vec![0u8; MAX_WG_PACKET];
                match tunn.decapsulate(None, packet, &mut decode_buf) {
                    TunnResult::WriteToNetwork(data) => {
                        diag.mark_data_send();
                        let _ = udp.send_to(data, peer_endpoint).await;

                        // Check if there's more to process after handshake
                        let mut extra_buf = vec![0u8; MAX_WG_PACKET];
                        loop {
                            match tunn.decapsulate(None, &[], &mut extra_buf) {
                                TunnResult::WriteToNetwork(data) => {
                                    diag.mark_data_send();
                                    let _ = udp.send_to(data, peer_endpoint).await;
                                }
                                TunnResult::WriteToTunnelV6(data, _) => {
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
                                    device.push_rx(data.to_vec());
                                }
                                _ => break,
                            }
                        }
                    }
                    TunnResult::WriteToTunnelV6(data, _) => {
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
                        device.push_rx(data.to_vec());
                    }
                    TunnResult::Err(e) => {
                        debug!(?e, "WG decapsulate error");
                    }
                    _ => {}
                }

                // Poll smoltcp to process the received packet
                iface.poll(SmoltcpInstant::now(), &mut device, &mut sockets);

                // Send any tx packets generated by the poll
                while let Some(ip_packet) = device.pop_tx() {
                    match tunn.encapsulate(&ip_packet, &mut wg_scratch) {
                        TunnResult::WriteToNetwork(data) => {
                            diag.mark_data_send();
                            let _ = udp.send_to(data, peer_endpoint).await;
                        }
                        _ => {}
                    }
                }
            }

            // Accept new outbound TCP connections
            result = accept_any(&listeners) => {
                if let Some((stream, overlay_dest, local_addr)) = result {
                    debug!(%local_addr, %overlay_dest, "new outbound connection");

                    // Create smoltcp TCP socket and connect to overlay destination
                    let rx_buf = SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
                    let tx_buf = SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
                    let mut tcp = TcpSocket::new(rx_buf, tx_buf);

                    let dest_ip = match overlay_dest.ip() {
                        std::net::IpAddr::V6(v6) => ipv6_to_smoltcp(v6),
                        std::net::IpAddr::V4(_) => {
                            warn!("IPv4 overlay destinations not supported");
                            continue;
                        }
                    };

                    // Use an ephemeral local port
                    let local_port = 49152 + (relays.len() as u16 % 16000);
                    let cx = iface.context();
                    if let Err(e) = tcp.connect(cx, (dest_ip, overlay_dest.port()), local_port) {
                        warn!(?e, dest = %overlay_dest, "smoltcp connect failed");
                        continue;
                    }
                    let handle = sockets.add(tcp);
                    relays.push(OutboundRelay {
                        stream,
                        smoltcp_handle: handle,
                        overlay_dest,
                        connected: false,
                    });

                    // Immediately poll to generate the SYN packet
                    iface.poll(SmoltcpInstant::now(), &mut device, &mut sockets);
                    while let Some(ip_packet) = device.pop_tx() {
                        match tunn.encapsulate(&ip_packet, &mut wg_scratch) {
                            TunnResult::WriteToNetwork(data) => {
                                diag.mark_data_send();
                                let _ = udp.send_to(data, peer_endpoint).await;
                            }
                            TunnResult::Err(e) => {
                                debug!(?e, "WG encapsulate error");
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

fn ipv6_to_smoltcp(addr: Ipv6Addr) -> IpAddress {
    let segs = addr.segments();
    IpAddress::Ipv6(smoltcp::wire::Ipv6Address::new(
        segs[0], segs[1], segs[2], segs[3], segs[4], segs[5], segs[6], segs[7],
    ))
}

/// Accept a connection from any of the listeners.
async fn accept_any(
    listeners: &[(TcpListener, SocketAddr)],
) -> Option<(TcpStream, SocketAddr, SocketAddr)> {
    if listeners.is_empty() {
        // Never resolve — avoid busy loop
        std::future::pending::<()>().await;
        return None;
    }

    // Poll all listeners
    let futs: Vec<_> = listeners
        .iter()
        .map(|(listener, dest)| {
            let dest = *dest;
            async move {
                let (stream, addr) = listener.accept().await.ok()?;
                Some((stream, dest, addr))
            }
        })
        .collect();

    // Race them — first one to accept wins
    tokio::select! {
        result = async {
            // Use select on all futures
            for fut in futs {
                if let Some(r) = fut.await {
                    return Some(r);
                }
            }
            None
        } => result
    }
}
