//! Port 53 sockets and the supervisor that keeps them across restarts.
//!
//! `ployzd dns` binds its own sockets and parks copies in systemd's fd store, so
//! the kernel queues queries while the process restarts instead of answering ICMP
//! port-unreachable. Nothing outside this file knows fd names, `SO_TYPE`, or
//! sd_notify.

use std::{
    io,
    net::{SocketAddr, SocketAddrV4, TcpListener, UdpSocket},
    os::fd::{AsFd, OwnedFd},
};

use hickory_server::{Server, server::RequestHandler};
use sd_notify::NotifyState;
use socket2::{Domain, SockAddr, Socket, Type};

use super::{TCP_REQUEST_TIMEOUT, TCP_RESPONSE_BUFFER};

const TCP_BACKLOG: i32 = 1024;

/// The pair of sockets one spec serves on, both bound to `listen`.
#[derive(Debug)]
pub(crate) struct Listeners {
    listen: SocketAddrV4,
    udp: UdpSocket,
    tcp: TcpListener,
}

impl Listeners {
    /// Bind `listen` for UDP and TCP with `IP_FREEBIND`, so the bind succeeds
    /// before the bridge that owns the gateway address exists. `SO_REUSEADDR`
    /// goes on TCP only; neither socket uses `SO_REUSEPORT`, so a second binder
    /// fails with `EADDRINUSE`.
    ///
    /// # Errors
    ///
    /// Returns the socket, setsockopt, bind, or listen error.
    pub(crate) fn bind(listen: SocketAddrV4) -> io::Result<Self> {
        let udp = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
        udp.set_freebind_v4(true)?;
        udp.set_nonblocking(true)?;
        udp.bind(&SockAddr::from(listen))?;
        let listen = bound_v4(&udp)?;
        let tcp = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        tcp.set_freebind_v4(true)?;
        tcp.set_reuse_address(true)?;
        tcp.set_nonblocking(true)?;
        tcp.bind(&SockAddr::from(listen))?;
        tcp.listen(TCP_BACKLOG)?;
        Ok(Self {
            listen,
            udp: udp.into(),
            tcp: tcp.into(),
        })
    }

    pub(crate) fn listen(&self) -> SocketAddrV4 {
        self.listen
    }

    #[cfg(test)]
    pub(crate) fn bind_ephemeral() -> Self {
        loop {
            let udp_port_already_held_by_another_tcp_socket =
                match Self::bind(SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0)) {
                    Ok(listeners) => return listeners,
                    Err(error) => error.kind() == io::ErrorKind::AddrInUse,
                };
            assert!(udp_port_already_held_by_another_tcp_socket);
        }
    }

    /// Hand `server` duplicates. The originals stay here and in the fd store, so
    /// the server closing its copies never closes the port.
    ///
    /// # Errors
    ///
    /// Returns the dup or tokio registration error.
    pub(crate) fn register<H: RequestHandler>(&self, server: &mut Server<H>) -> io::Result<()> {
        let udp = tokio::net::UdpSocket::from_std(self.udp.try_clone()?)?;
        let tcp = tokio::net::TcpListener::from_std(self.tcp.try_clone()?)?;
        server.register_socket(udp);
        server.register_listener(tcp, TCP_REQUEST_TIMEOUT, TCP_RESPONSE_BUFFER);
        Ok(())
    }

    pub(crate) fn names(&self) -> [String; 2] {
        [
            fd_name(SocketKind::Udp, self.listen),
            fd_name(SocketKind::Tcp, self.listen),
        ]
    }
}

/// One fd handed over by the supervisor, classified from the kernel rather
/// than from its name.
pub(crate) struct InheritedFd {
    pub(crate) kind: SocketKind,
    pub(crate) local: Option<SocketAddr>,
    pub(crate) name: String,
    pub(crate) fd: OwnedFd,
}

impl InheritedFd {
    fn classify(fd: OwnedFd, name: String) -> Self {
        let socket = Socket::from(fd);
        let kind = match socket.r#type() {
            Ok(Type::DGRAM) => SocketKind::Udp,
            Ok(Type::STREAM) => SocketKind::Tcp,
            _ => SocketKind::Other,
        };
        let local = socket.local_addr().ok().and_then(|addr| addr.as_socket());
        Self {
            kind,
            local,
            name,
            fd: socket.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SocketKind {
    Udp,
    Tcp,
    Other,
}

/// Inherited fds matched against the spec.
pub(crate) struct Adopted {
    /// `Some` iff exactly one UDP and one TCP fd are bound to `listen`.
    pub(crate) listeners: Option<Listeners>,
    /// Everything else: a previous subnet's pair, duplicates, a half pair,
    /// unknown fds. The caller forgets their names so the store drops them.
    pub(crate) stale: Vec<InheritedFd>,
}

pub(crate) fn adopt(inherited: Vec<InheritedFd>, listen: SocketAddrV4) -> Adopted {
    let mut udp = None;
    let mut tcp = None;
    let mut stale = Vec::new();
    for fd in inherited {
        let slot = match fd.kind {
            SocketKind::Udp => &mut udp,
            SocketKind::Tcp => &mut tcp,
            SocketKind::Other => &mut None,
        };
        if fd.local == Some(SocketAddr::V4(listen)) && slot.is_none() {
            *slot = Some(fd);
        } else {
            stale.push(fd);
        }
    }
    match (udp, tcp) {
        (Some(udp), Some(tcp)) => Adopted {
            listeners: Some(Listeners {
                listen,
                udp: UdpSocket::from(udp.fd),
                tcp: TcpListener::from(tcp.fd),
            }),
            stale,
        },
        (udp, tcp) => {
            stale.extend(udp);
            stale.extend(tcp);
            Adopted {
                listeners: None,
                stale,
            }
        }
    }
}

pub(crate) fn names(fds: &[InheritedFd]) -> Vec<String> {
    fds.iter().map(|fd| fd.name.clone()).collect()
}

/// The supervisor seam: systemd in production, memory in tests where one
/// `MemoryKeeper` outlives several simulated processes.
pub(crate) trait Keeper: Send + Sync {
    /// Fds this process was started with.
    fn inherited(&self) -> io::Result<Vec<InheritedFd>>;

    /// Park copies of `listeners` under their names. Idempotent.
    fn keep(&self, listeners: &Listeners) -> io::Result<()>;

    /// Drop the named fds from the store.
    fn forget(&self, names: &[String]) -> io::Result<()>;

    /// `READY=1` with a status, sent once the spec's sockets are held.
    fn ready(&self, status: &str) -> io::Result<()>;

    /// A status line for `systemctl status`.
    fn status(&self, text: &str) -> io::Result<()>;
}

/// Without `NOTIFY_SOCKET` every call is a no-op, so an externally supervised
/// `ployzd dns` runs the same code and has no store.
pub(crate) struct SystemdKeeper;

impl Keeper for SystemdKeeper {
    /// listenfd owns the raw-fd adoption; this crate forbids `unsafe`. An fd
    /// that is neither an IPv4 UDP socket nor an IPv4 TCP listener stays with
    /// systemd until the unit stops.
    fn inherited(&self) -> io::Result<Vec<InheritedFd>> {
        let names: Vec<String> = std::env::var("LISTEN_FDNAMES")
            .map(|names| names.split(':').map(str::to_owned).collect())
            .unwrap_or_default();
        let mut passed = listenfd::ListenFd::from_env();
        let mut inherited = Vec::new();
        for index in 0..passed.len() {
            let name = names.get(index).cloned().unwrap_or_default();
            let fd = match passed.take_udp_socket(index) {
                Ok(Some(udp)) => OwnedFd::from(udp),
                Ok(None) => continue,
                Err(_) => match passed.take_tcp_listener(index) {
                    Ok(Some(tcp)) => OwnedFd::from(tcp),
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(index, name, %error, "ignoring an inherited fd");
                        continue;
                    }
                },
            };
            inherited.push(InheritedFd::classify(fd, name));
        }
        Ok(inherited)
    }

    fn keep(&self, listeners: &Listeners) -> io::Result<()> {
        let [udp, tcp] = listeners.names();
        sd_notify::notify_with_fds(
            &[NotifyState::FdStore, NotifyState::FdName(&udp)],
            &[listeners.udp.as_fd()],
        )?;
        sd_notify::notify_with_fds(
            &[NotifyState::FdStore, NotifyState::FdName(&tcp)],
            &[listeners.tcp.as_fd()],
        )
    }

    fn forget(&self, names: &[String]) -> io::Result<()> {
        for name in names {
            sd_notify::notify(&[NotifyState::FdStoreRemove, NotifyState::FdName(name)])?;
        }
        Ok(())
    }

    fn ready(&self, status: &str) -> io::Result<()> {
        sd_notify::notify(&[NotifyState::Ready, NotifyState::Status(status)])
    }

    fn status(&self, text: &str) -> io::Result<()> {
        sd_notify::notify(&[NotifyState::Status(text)])
    }
}

/// Test supervisor. Clone it into the next simulated process to reproduce a
/// restart: queries sent while no process runs wait in the stored socket.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct MemoryKeeper {
    store: std::sync::Arc<std::sync::Mutex<Vec<(String, OwnedFd)>>>,
    statuses: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
impl MemoryKeeper {
    pub(crate) fn stored_names(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub(crate) fn statuses(&self) -> Vec<String> {
        self.statuses.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Keeper for MemoryKeeper {
    fn inherited(&self) -> io::Result<Vec<InheritedFd>> {
        self.store
            .lock()
            .unwrap()
            .iter()
            .map(|(name, fd)| Ok(InheritedFd::classify(fd.try_clone()?, name.clone())))
            .collect()
    }

    fn keep(&self, listeners: &Listeners) -> io::Result<()> {
        let [udp, tcp] = listeners.names();
        let mut store = self.store.lock().unwrap();
        for (name, fd) in [
            (udp, listeners.udp.as_fd().try_clone_to_owned()?),
            (tcp, listeners.tcp.as_fd().try_clone_to_owned()?),
        ] {
            if !store.iter().any(|(stored, _)| *stored == name) {
                store.push((name, fd));
            }
        }
        Ok(())
    }

    fn forget(&self, names: &[String]) -> io::Result<()> {
        self.store
            .lock()
            .unwrap()
            .retain(|(name, _)| !names.contains(name));
        Ok(())
    }

    fn ready(&self, status: &str) -> io::Result<()> {
        self.status(status)
    }

    fn status(&self, text: &str) -> io::Result<()> {
        self.statuses.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

fn fd_name(kind: SocketKind, listen: SocketAddrV4) -> String {
    let kind = match kind {
        SocketKind::Udp => "udp",
        SocketKind::Tcp => "tcp",
        SocketKind::Other => "other",
    };
    format!("{kind}-{}-{}", listen.ip(), listen.port())
}

fn bound_v4(socket: &Socket) -> io::Result<SocketAddrV4> {
    socket
        .local_addr()?
        .as_socket_ipv4()
        .ok_or_else(|| io::Error::other("DNS socket is not bound to an IPv4 address"))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn loopback_pair() -> Listeners {
        Listeners::bind_ephemeral()
    }

    fn inherited_from(listeners: &Listeners, names: [&str; 2]) -> Vec<InheritedFd> {
        [
            (&listeners.udp as &dyn AsFd, names[0]),
            (&listeners.tcp as &dyn AsFd, names[1]),
        ]
        .into_iter()
        .map(|(fd, name)| {
            InheritedFd::classify(fd.as_fd().try_clone_to_owned().unwrap(), name.to_owned())
        })
        .collect()
    }

    #[test]
    fn adopt_takes_matching_pair() {
        let bound = loopback_pair();
        let adopted = adopt(inherited_from(&bound, ["udp", "tcp"]), bound.listen());
        let listeners = adopted.listeners.unwrap();
        assert_eq!(listeners.listen(), bound.listen());
        assert_eq!(listeners.udp.local_addr().unwrap(), bound.listen().into());
        assert_eq!(listeners.tcp.local_addr().unwrap(), bound.listen().into());
        assert!(adopted.stale.is_empty());
    }

    #[test]
    fn adopt_returns_other_subnet_stale() {
        let bound = loopback_pair();
        let other = SocketAddrV4::new(Ipv4Addr::LOCALHOST, bound.listen().port() + 1);
        let adopted = adopt(inherited_from(&bound, ["udp", "tcp"]), other);
        assert!(adopted.listeners.is_none());
        assert_eq!(names(&adopted.stale), ["udp", "tcp"]);
    }

    #[test]
    fn adopt_drops_half_pair() {
        let bound = loopback_pair();
        let mut inherited = inherited_from(&bound, ["udp", "tcp"]);
        inherited.truncate(1);
        let adopted = adopt(inherited, bound.listen());
        assert!(adopted.listeners.is_none());
        assert_eq!(names(&adopted.stale), ["udp"]);
    }

    #[test]
    fn adopt_ignores_names() {
        let bound = loopback_pair();
        let adopted = adopt(inherited_from(&bound, ["tcp-garbage", ""]), bound.listen());
        assert!(adopted.listeners.is_some());
        assert!(adopted.stale.is_empty());
    }

    #[test]
    fn adopt_keeps_duplicates_stale() {
        let bound = loopback_pair();
        let mut inherited = inherited_from(&bound, ["udp", "tcp"]);
        inherited.extend(inherited_from(&bound, ["udp-again", "tcp-again"]));
        let adopted = adopt(inherited, bound.listen());
        assert!(adopted.listeners.is_some());
        assert_eq!(names(&adopted.stale), ["udp-again", "tcp-again"]);
    }

    #[test]
    fn bind_freebind_nonlocal_address_and_refuses_a_second_binder() {
        let probe = loopback_pair();
        let nonlocal = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 77), probe.listen().port());
        drop(probe);
        let first = Listeners::bind(nonlocal).unwrap();
        assert_eq!(first.listen(), nonlocal);
        let second = Listeners::bind(nonlocal).unwrap_err();
        assert_eq!(second.kind(), io::ErrorKind::AddrInUse);
    }

    #[test]
    fn memory_keeper_round_trip() {
        let keeper = MemoryKeeper::default();
        let bound = loopback_pair();
        keeper.keep(&bound).unwrap();
        keeper.keep(&bound).unwrap();
        assert_eq!(keeper.stored_names(), bound.names());
        let inherited = keeper.inherited().unwrap();
        assert_eq!(inherited.len(), 2);
        assert!(
            inherited
                .iter()
                .all(|fd| fd.local == Some(bound.listen().into()))
        );
        let adopted = adopt(inherited, bound.listen());
        assert!(adopted.listeners.is_some());
        keeper.forget(&bound.names()).unwrap();
        assert!(keeper.stored_names().is_empty());
        assert!(keeper.inherited().unwrap().is_empty());
    }

    #[test]
    fn fd_names_carry_kind_and_address() {
        let listen = SocketAddrV4::new(Ipv4Addr::new(10, 210, 3, 1), 53);
        assert_eq!(fd_name(SocketKind::Udp, listen), "udp-10.210.3.1-53");
        assert_eq!(fd_name(SocketKind::Tcp, listen), "tcp-10.210.3.1-53");
    }
}
