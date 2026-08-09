use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use gotatun::device::{DeviceBuilder, Peer};
use gotatun::udp::socket::{SockOpt, UdpSocket};
use gotatun::udp::{UdpTransportFactory, UdpTransportFactoryParams};
use gotatun::x25519::{PublicKey, StaticSecret};
use ipnetwork::{IpNetwork, Ipv6Network};
use ployz_core::corrosion::BuiltinWireguardMemberSubnet;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv6Address};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::netstack::{INNER_MTU, SmoltcpIpDevice, in_memory_ip_link};

pub const UDP_BLOCKED_DOCS_ANCHOR: &str = "docs/operations/cli-mesh-access.md#udp-blocked-networks";

const KEEPALIVE_SECONDS: u16 = 25;
const IP_LINK_CAPACITY: usize = 32;
const APPLICATION_BUFFER_BYTES: usize = 64 * 1024;
const TCP_BUFFER_BYTES: usize = 64 * 1024;
const TCP_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshDialTimeouts {
    pub connect: Duration,
}

impl MeshDialTimeouts {
    #[must_use]
    pub const fn new(connect: Duration) -> Self {
        Self { connect }
    }
}

impl Default for MeshDialTimeouts {
    fn default() -> Self {
        Self::new(Duration::from_secs(8))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardPeer {
    pub public_key: [u8; 32],
    pub endpoint: SocketAddr,
    pub allowed_subnet: BuiltinWireguardMemberSubnet,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BuiltinWireguardDial {
    private_key: [u8; 32],
    source_addr: Ipv6Addr,
    machine_peer: BuiltinWireguardPeer,
    ssh_target: String,
}

impl BuiltinWireguardDial {
    #[must_use]
    pub fn new(
        private_key: [u8; 32],
        source_addr: Ipv6Addr,
        machine_peer: BuiltinWireguardPeer,
        ssh_target: impl Into<String>,
    ) -> Self {
        Self {
            private_key,
            source_addr,
            machine_peer,
            ssh_target: ssh_target.into(),
        }
    }
}

impl fmt::Debug for BuiltinWireguardDial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltinWireguardDial")
            .field("private_key", &"[REDACTED]")
            .field("source_addr", &self.source_addr)
            .field("machine_peer", &self.machine_peer)
            .field("ssh_target", &self.ssh_target)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct MeshConnector {
    dial: BuiltinWireguardDial,
    timeouts: MeshDialTimeouts,
}

impl MeshConnector {
    #[must_use]
    pub const fn builtin_wireguard(dial: BuiltinWireguardDial, timeouts: MeshDialTimeouts) -> Self {
        Self { dial, timeouts }
    }

    pub async fn connect(&self, target: SocketAddr) -> Result<MeshStream, MeshConnectError> {
        connect_builtin_wireguard(&self.dial, target, self.timeouts).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MeshConnectError {
    #[error("builtin WireGuard target {target} is outside machine peer subnet {allowed_subnet}")]
    TargetOutsidePeerSubnet {
        target: SocketAddr,
        allowed_subnet: BuiltinWireguardMemberSubnet,
    },
    #[error("could not start the userspace WireGuard device: {0}")]
    WireguardDevice(#[source] gotatun::device::Error),
    #[error("could not configure the userspace TCP stack: {0}")]
    NetstackConfiguration(String),
    #[error(
        "WireGuard dial to {target} through UDP endpoint {endpoint} timed out; UDP may be blocked. Fallback: {ssh_fallback}. See {docs_anchor}"
    )]
    UdpDialTimedOut {
        endpoint: SocketAddr,
        target: SocketAddr,
        founding_target: Box<str>,
        docs_anchor: &'static str,
        ssh_fallback: Box<str>,
    },
    #[error("userspace mesh TCP task ended before connecting to {target}: {detail}")]
    NetstackEnded { target: SocketAddr, detail: String },
}

pub struct MeshStream {
    inner: tokio::io::DuplexStream,
    worker: Option<JoinHandle<()>>,
    terminal: watch::Receiver<Option<StreamTerminal>>,
    terminal_reported: bool,
}

impl fmt::Debug for MeshStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MeshStream")
            .field("transport", &"userspace_wireguard")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamTerminal {
    Clean,
    ConnectionReset,
    TaskFailed,
}

impl MeshStream {
    fn new(
        inner: tokio::io::DuplexStream,
        worker: JoinHandle<()>,
        terminal: watch::Receiver<Option<StreamTerminal>>,
    ) -> Self {
        Self {
            inner,
            worker: Some(worker),
            terminal,
            terminal_reported: false,
        }
    }

    fn poll_terminal(&mut self) -> Poll<io::Result<()>> {
        let terminal = *self.terminal.borrow_and_update();
        if self.terminal_reported {
            return Poll::Ready(Ok(()));
        }
        match terminal {
            Some(StreamTerminal::Clean) | None => Poll::Ready(Ok(())),
            Some(StreamTerminal::ConnectionReset) => {
                self.terminal_reported = true;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "mesh TCP peer reset the connection",
                )))
            }
            Some(StreamTerminal::TaskFailed) => {
                self.terminal_reported = true;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "userspace mesh TCP task stopped unexpectedly",
                )))
            }
        }
    }
}

impl AsyncRead for MeshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(context, buffer);
        match polled {
            Poll::Ready(Ok(())) => {
                if buffer.filled().len() == before {
                    self.poll_terminal()
                } else {
                    Poll::Ready(Ok(()))
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for MeshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

impl Drop for MeshStream {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

async fn connect_builtin_wireguard(
    dial: &BuiltinWireguardDial,
    target: SocketAddr,
    timeouts: MeshDialTimeouts,
) -> Result<MeshStream, MeshConnectError> {
    let IpAddr::V6(target_ip) = target.ip() else {
        return Err(MeshConnectError::TargetOutsidePeerSubnet {
            target,
            allowed_subnet: dial.machine_peer.allowed_subnet,
        });
    };
    if !dial
        .machine_peer
        .allowed_subnet
        .network()
        .contains(&target_ip)
    {
        return Err(MeshConnectError::TargetOutsidePeerSubnet {
            target,
            allowed_subnet: dial.machine_peer.allowed_subnet,
        });
    }

    let (gotatun_send, gotatun_recv, smoltcp_device) = in_memory_ip_link(IP_LINK_CAPACITY);
    let mut peer = Peer::new(PublicKey::from(dial.machine_peer.public_key))
        .with_endpoint(dial.machine_peer.endpoint)
        .with_allowed_ip(IpNetwork::V6(
            Ipv6Network::new(
                dial.machine_peer.allowed_subnet.network().network(),
                dial.machine_peer.allowed_subnet.network().prefix_len(),
            )
            .map_err(|error| MeshConnectError::NetstackConfiguration(error.to_string()))?,
        ));
    peer.keepalive = Some(KEEPALIVE_SECONDS);
    let device = DeviceBuilder::new()
        .with_udp(SingleUdpSocketFactory::new(dial.machine_peer.endpoint))
        .with_ip_pair(gotatun_send, gotatun_recv)
        .with_private_key(StaticSecret::from(dial.private_key))
        .with_peer(peer)
        .build()
        .await
        .map_err(MeshConnectError::WireguardDevice)?;

    let (application, worker_stream) = tokio::io::duplex(APPLICATION_BUFFER_BYTES);
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
    let (terminal_tx, terminal_rx) = watch::channel(None);
    let source_addr = dial.source_addr;
    let worker = tokio::spawn(async move {
        drive_tcp(
            device,
            smoltcp_device,
            TcpDriveTarget {
                source_addr,
                target_addr: target_ip,
                target_port: target.port(),
            },
            worker_stream,
            connected_tx,
            terminal_tx,
        )
        .await;
    });

    match timeout(timeouts.connect, connected_rx).await {
        Ok(Ok(Ok(()))) => Ok(MeshStream::new(application, worker, terminal_rx)),
        Ok(Ok(Err(detail))) => {
            worker.abort();
            Err(MeshConnectError::NetstackEnded { target, detail })
        }
        Ok(Err(_)) => {
            worker.abort();
            Err(MeshConnectError::NetstackEnded {
                target,
                detail: "connection task stopped without a result".to_owned(),
            })
        }
        Err(_) => {
            worker.abort();
            Err(MeshConnectError::UdpDialTimedOut {
                endpoint: dial.machine_peer.endpoint,
                target,
                founding_target: dial.ssh_target.clone().into_boxed_str(),
                docs_anchor: UDP_BLOCKED_DOCS_ANCHOR,
                ssh_fallback: render_ssh_fallback_command(&dial.ssh_target, target)
                    .into_boxed_str(),
            })
        }
    }
}

#[must_use]
pub fn render_ssh_fallback_command(ssh_target: &str, target: SocketAddr) -> String {
    let remote = match target.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    format!(
        "ssh -N -L 127.0.0.1:{port}:{remote}:{port} {ssh_target}",
        port = target.port()
    )
}

#[derive(Debug, Clone, Copy)]
struct TcpDriveTarget {
    source_addr: Ipv6Addr,
    target_addr: Ipv6Addr,
    target_port: u16,
}

async fn drive_tcp<T>(
    device: gotatun::device::Device<T>,
    mut network_device: SmoltcpIpDevice,
    target: TcpDriveTarget,
    mut application: tokio::io::DuplexStream,
    connected: tokio::sync::oneshot::Sender<Result<(), String>>,
    terminal: watch::Sender<Option<StreamTerminal>>,
) where
    T: gotatun::device::DeviceTransports,
{
    let started = Instant::now();
    let mut random = [0_u8; 8];
    if let Err(error) = getrandom::fill(&mut random) {
        let _ = connected.send(Err(format!("could not seed TCP stack: {error}")));
        let _ = terminal.send(Some(StreamTerminal::TaskFailed));
        device.stop().await;
        return;
    }
    let random_seed = u64::from_ne_bytes(random);
    let mut config = Config::new(HardwareAddress::Ip);
    config.random_seed = random_seed;
    let mut interface = Interface::new(config, &mut network_device, smoltcp_now(started));
    let source = Ipv6Address::from(target.source_addr.octets());
    interface.update_ip_addrs(|addresses| {
        addresses
            .push(IpCidr::new(IpAddress::Ipv6(source), 112))
            .expect("one configured address fits the smoltcp address set");
    });
    if let Err(error) = interface
        .routes_mut()
        .add_default_ipv6_route(Ipv6Address::from(target.target_addr.octets()))
    {
        let _ = connected.send(Err(format!("could not install IPv6 route: {error:?}")));
        let _ = terminal.send(Some(StreamTerminal::TaskFailed));
        device.stop().await;
        return;
    }

    let tcp_rx = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]);
    let tcp_tx = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]);
    let tcp_socket = tcp::Socket::new(tcp_rx, tcp_tx);
    let mut sockets = SocketSet::new(Vec::new());
    let tcp_handle = sockets.add(tcp_socket);
    let local_port = 49_152 + (random_seed as u16 % 16_384);
    if let Err(error) = sockets.get_mut::<tcp::Socket>(tcp_handle).connect(
        interface.context(),
        (
            IpAddress::Ipv6(Ipv6Address::from(target.target_addr.octets())),
            target.target_port,
        ),
        local_port,
    ) {
        let _ = connected.send(Err(format!("could not start TCP connect: {error:?}")));
        let _ = terminal.send(Some(StreamTerminal::TaskFailed));
        device.stop().await;
        return;
    }

    let mut connected = Some(connected);
    let mut was_established = false;
    let mut application_read = vec![0; INNER_MTU];
    let mut pending_application = Vec::new();
    let mut pending_offset = 0_usize;
    loop {
        interface.poll(smoltcp_now(started), &mut network_device, &mut sockets);
        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if socket.state() == tcp::State::Established {
            if !was_established {
                was_established = true;
                if let Some(sender) = connected.take() {
                    let _ = sender.send(Ok(()));
                }
            }
        } else if was_established && !socket.is_active() {
            let state = if socket.state() == tcp::State::Closed {
                StreamTerminal::Clean
            } else {
                StreamTerminal::ConnectionReset
            };
            let _ = terminal.send(Some(state));
            break;
        } else if was_established && socket.state() == tcp::State::CloseWait && !socket.may_recv() {
            let _ = terminal.send(Some(StreamTerminal::Clean));
            break;
        }

        if pending_offset < pending_application.len() && socket.can_send() {
            let remaining = pending_application
                .get(pending_offset..)
                .expect("pending offset stays within the application buffer");
            match socket.send_slice(remaining) {
                Ok(written) => pending_offset += written,
                Err(_) => {
                    let _ = terminal.send(Some(StreamTerminal::ConnectionReset));
                    break;
                }
            }
            if pending_offset == pending_application.len() {
                pending_application.clear();
                pending_offset = 0;
            }
        }

        if socket.can_recv() {
            let received = socket.recv(|data| {
                let amount = data.len().min(INNER_MTU);
                let bytes = data
                    .get(..amount)
                    .expect("bounded receive length stays within the TCP buffer");
                (amount, bytes.to_vec())
            });
            match received {
                Ok(bytes) if !bytes.is_empty() => {
                    if application.write_all(&bytes).await.is_err() {
                        let _ = terminal.send(Some(StreamTerminal::Clean));
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = terminal.send(Some(StreamTerminal::ConnectionReset));
                    break;
                }
            }
        }

        if pending_application.is_empty() {
            match timeout(TCP_POLL_INTERVAL, application.read(&mut application_read)).await {
                Ok(Ok(0)) => {
                    socket.close();
                    let _ = terminal.send(Some(StreamTerminal::Clean));
                    break;
                }
                Ok(Ok(read)) => {
                    let bytes = application_read
                        .get(..read)
                        .expect("Tokio read length stays within the supplied buffer");
                    pending_application.extend_from_slice(bytes);
                }
                Ok(Err(_)) => {
                    let _ = terminal.send(Some(StreamTerminal::Clean));
                    break;
                }
                Err(_) => {}
            }
        } else {
            tokio::time::sleep(TCP_POLL_INTERVAL).await;
        }
    }
    if let Some(sender) = connected.take() {
        let _ = sender.send(Err(
            "TCP socket closed before the handshake completed".to_owned()
        ));
    }
    device.stop().await;
}

fn smoltcp_now(started: Instant) -> smoltcp::time::Instant {
    let elapsed = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    smoltcp::time::Instant::from_millis(elapsed)
}

struct SingleUdpSocketFactory {
    endpoint: SocketAddr,
}

impl SingleUdpSocketFactory {
    const fn new(endpoint: SocketAddr) -> Self {
        Self { endpoint }
    }
}

impl UdpTransportFactory for SingleUdpSocketFactory {
    type SendV4 = UdpSocket;
    type SendV6 = UdpSocket;
    type RecvV4 = UdpSocket;
    type RecvV6 = UdpSocket;

    async fn bind(
        &mut self,
        params: &UdpTransportFactoryParams,
    ) -> io::Result<((Self::SendV4, Self::RecvV4), (Self::SendV6, Self::RecvV6))> {
        let bind_addr = match self.endpoint {
            SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, params.port)),
            SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, params.port)),
        };
        let socket = UdpSocket::bind(bind_addr, SockOpt::default())?;
        Ok(((socket.clone(), socket.clone()), (socket.clone(), socket)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::derive_builtin_wireguard_member;
    use ployz_core::ids::ClusterName;
    use ployz_core::network::WireGuardPublicKey;

    fn builtin_dial(endpoint: SocketAddr) -> (BuiltinWireguardDial, Ipv6Addr) {
        let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let local_secret = StaticSecret::from([7_u8; 32]);
        let local_public = WireGuardPublicKey::try_new(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            PublicKey::from(&local_secret).as_bytes(),
        ))
        .expect("local public key");
        let remote_secret = StaticSecret::from([9_u8; 32]);
        let remote_public_bytes = *PublicKey::from(&remote_secret).as_bytes();
        let remote_public = WireGuardPublicKey::try_new(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            remote_public_bytes,
        ))
        .expect("remote public key");
        let local = derive_builtin_wireguard_member(&cluster_id, &local_public);
        let remote = derive_builtin_wireguard_member(&cluster_id, &remote_public);
        (
            BuiltinWireguardDial::new(
                local_secret.to_bytes(),
                local.bind_address().get(),
                BuiltinWireguardPeer {
                    public_key: remote_public_bytes,
                    endpoint,
                    allowed_subnet: remote.subnet(),
                },
                "root@machine.example",
            ),
            remote.bind_address().get(),
        )
    }

    #[tokio::test]
    async fn wireguard_factory_reuses_one_bound_udp_socket() {
        let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, 51_820));
        let mut factory = SingleUdpSocketFactory::new(endpoint);
        let params = UdpTransportFactoryParams {
            addr_v4: Ipv4Addr::UNSPECIFIED,
            addr_v6: Ipv6Addr::UNSPECIFIED,
            port: 0,
            #[cfg(target_os = "linux")]
            fwmark: None,
        };
        let ((send_v4, receive_v4), (send_v6, receive_v6)) =
            factory.bind(&params).await.expect("bind one socket");
        let expected = send_v4.local_addr().expect("bound address");
        assert_eq!(receive_v4.local_addr().expect("v4 receive"), expected);
        assert_eq!(send_v6.local_addr().expect("v6 send"), expected);
        assert_eq!(receive_v6.local_addr().expect("v6 receive"), expected);
    }

    #[tokio::test]
    async fn silent_udp_peer_returns_typed_timeout_with_exact_ssh_fallback() {
        let silent = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind silent endpoint");
        let endpoint = silent.local_addr().expect("silent endpoint address");
        let (dial, target_ip) = builtin_dial(endpoint);
        let target = SocketAddr::from((target_ip, 2_020));
        let connector = MeshConnector::builtin_wireguard(
            dial,
            MeshDialTimeouts::new(Duration::from_millis(150)),
        );

        let error = connector.connect(target).await.expect_err("UDP is silent");
        let MeshConnectError::UdpDialTimedOut {
            endpoint: actual_endpoint,
            target: actual_target,
            founding_target,
            docs_anchor,
            ssh_fallback,
        } = error
        else {
            panic!("unexpected error: {error}");
        };
        assert_eq!(actual_endpoint, endpoint);
        assert_eq!(actual_target, target);
        assert_eq!(founding_target.as_ref(), "root@machine.example");
        assert_eq!(docs_anchor, UDP_BLOCKED_DOCS_ANCHOR);
        assert_eq!(
            ssh_fallback.as_ref(),
            format!("ssh -N -L 127.0.0.1:2020:[{target_ip}]:2020 root@machine.example")
        );
    }

    #[tokio::test]
    async fn stream_reports_worker_failure_instead_of_silent_eof() {
        let (client, worker) = tokio::io::duplex(16);
        drop(worker);
        let (terminal_tx, terminal_rx) = watch::channel(None);
        terminal_tx
            .send(Some(StreamTerminal::TaskFailed))
            .expect("stream observes terminal state");
        let worker_task = tokio::spawn(std::future::pending());
        let mut stream = MeshStream::new(client, worker_task, terminal_rx);

        let mut byte = [0_u8; 1];
        let error = stream.read(&mut byte).await.expect_err("worker failed");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }
}
