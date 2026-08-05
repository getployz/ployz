use std::io;

use gotatun::packet::{Ip, Packet, PacketBufPool};
use gotatun::tun::{IpRecv, IpSend, MtuWatcher};
use smoltcp::phy::{Checksum, Device, DeviceCapabilities, Loopback, Medium, RxToken, TxToken};
use tokio::sync::mpsc;

pub(super) const INNER_MTU: usize = 1_280;

pub(super) struct GotatunIpSend {
    tx: mpsc::Sender<Vec<u8>>,
}

pub(super) struct GotatunIpRecv {
    rx: mpsc::Receiver<Vec<u8>>,
}

pub(super) struct SmoltcpIpDevice {
    rx: mpsc::Receiver<Vec<u8>>,
    pending_rx: Option<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
}

pub(super) fn in_memory_ip_link(
    capacity: usize,
) -> (GotatunIpSend, GotatunIpRecv, SmoltcpIpDevice) {
    let (wireguard_to_stack_tx, wireguard_to_stack_rx) = mpsc::channel(capacity);
    let (stack_to_wireguard_tx, stack_to_wireguard_rx) = mpsc::channel(capacity);
    (
        GotatunIpSend {
            tx: wireguard_to_stack_tx,
        },
        GotatunIpRecv {
            rx: stack_to_wireguard_rx,
        },
        SmoltcpIpDevice {
            rx: wireguard_to_stack_rx,
            pending_rx: None,
            tx: stack_to_wireguard_tx,
        },
    )
}

impl IpSend for GotatunIpSend {
    async fn send(&mut self, packet: Packet<Ip>) -> io::Result<()> {
        self.tx
            .send(packet.into_bytes().to_vec())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "smoltcp receiver closed"))
    }
}

impl IpRecv for GotatunIpRecv {
    async fn recv<'a>(
        &'a mut self,
        _pool: &mut PacketBufPool,
    ) -> io::Result<impl Iterator<Item = Packet<Ip>> + Send + 'a> {
        let Some(frame) = self.rx.recv().await else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "smoltcp sender closed",
            ));
        };
        let packet = Packet::copy_from(frame.as_slice())
            .try_into_ip()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        Ok(std::iter::once(packet))
    }

    fn mtu(&self) -> MtuWatcher {
        MtuWatcher::new(INNER_MTU as u16)
    }
}

pub(super) struct SmoltcpRxToken(Vec<u8>);

pub(super) struct SmoltcpTxToken(tokio::sync::mpsc::OwnedPermit<Vec<u8>>);

impl RxToken for SmoltcpRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

impl TxToken for SmoltcpTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = vec![0; len];
        let result = f(&mut frame);
        self.0.send(frame);
        result
    }
}

impl Device for SmoltcpIpDevice {
    type RxToken<'a> = SmoltcpRxToken;
    type TxToken<'a> = SmoltcpTxToken;

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.pending_rx.is_none() {
            self.pending_rx = self.rx.try_recv().ok();
        }
        let frame = self.pending_rx.take()?;
        match self.tx.clone().try_reserve_owned() {
            Ok(permit) => Some((SmoltcpRxToken(frame), SmoltcpTxToken(permit))),
            Err(_) => {
                self.pending_rx = Some(frame);
                None
            }
        }
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        self.tx.clone().try_reserve_owned().ok().map(SmoltcpTxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = Loopback::new(Medium::Ip).capabilities();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = INNER_MTU;
        capabilities.max_burst_size = Some(1);
        capabilities.checksum.ipv4 = Checksum::Both;
        capabilities.checksum.udp = Checksum::Both;
        capabilities.checksum.tcp = Checksum::Both;
        capabilities.checksum.icmpv6 = Checksum::Both;
        capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn ipv6_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0_u8; 40];
        *frame.get_mut(0).expect("IPv6 version byte exists") = 0x60;
        frame
            .get_mut(4..6)
            .expect("IPv6 payload length bytes exist")
            .copy_from_slice(&(payload.len() as u16).to_be_bytes());
        *frame.get_mut(6).expect("IPv6 next-header byte exists") = 6;
        *frame.get_mut(7).expect("IPv6 hop-limit byte exists") = 64;
        frame
            .get_mut(8..24)
            .expect("IPv6 source bytes exist")
            .copy_from_slice(&"fd00::1".parse::<Ipv6Addr>().expect("source").octets());
        frame
            .get_mut(24..40)
            .expect("IPv6 destination bytes exist")
            .copy_from_slice(&"fd00::2".parse::<Ipv6Addr>().expect("destination").octets());
        frame.extend_from_slice(payload);
        frame
    }

    #[tokio::test]
    async fn forwards_exact_ip_frames_in_both_directions() {
        let (mut gotatun_tx, mut gotatun_rx, mut device) = in_memory_ip_link(1);
        let inbound = ipv6_frame(b"from-wireguard");
        gotatun_tx
            .send(
                Packet::copy_from(inbound.as_slice())
                    .try_into_ip()
                    .expect("IP"),
            )
            .await
            .expect("send inbound");

        let (rx, reply_tx) = device
            .receive(smoltcp::time::Instant::ZERO)
            .expect("received frame");
        rx.consume(|actual| assert_eq!(actual, inbound));
        drop(reply_tx);

        let outbound = ipv6_frame(b"from-smoltcp");
        device
            .transmit(smoltcp::time::Instant::ZERO)
            .expect("transmit capacity")
            .consume(outbound.len(), |frame| frame.copy_from_slice(&outbound));
        let mut pool = PacketBufPool::new(1);
        let frames = gotatun_rx.recv(&mut pool).await.expect("receive outbound");
        let actual = frames
            .map(|packet| packet.into_bytes().to_vec())
            .collect::<Vec<_>>();
        let [actual] = actual.as_slice() else {
            panic!("expected exactly one outbound frame")
        };
        assert_eq!(actual, &outbound);
    }

    #[tokio::test]
    async fn advertises_conservative_ip_mtu_and_applies_bounded_backpressure() {
        let (_gotatun_tx, _gotatun_rx, mut device) = in_memory_ip_link(1);
        let capabilities = device.capabilities();
        assert_eq!(capabilities.medium, Medium::Ip);
        assert_eq!(capabilities.max_transmission_unit, 1_280);
        assert_eq!(capabilities.max_burst_size, Some(1));
        assert!(matches!(capabilities.checksum.tcp, Checksum::Both));

        device
            .transmit(smoltcp::time::Instant::ZERO)
            .expect("first slot")
            .consume(1, |frame| {
                let [byte] = frame else {
                    panic!("one-byte transmit frame")
                };
                *byte = 1;
            });
        assert!(device.transmit(smoltcp::time::Instant::ZERO).is_none());
    }

    #[tokio::test]
    async fn channel_close_is_observable_on_both_sides() {
        let (mut gotatun_tx, mut gotatun_rx, device) = in_memory_ip_link(1);
        drop(device);
        let packet = Packet::copy_from(ipv6_frame(b"closed").as_slice())
            .try_into_ip()
            .expect("IP");
        let send_error = gotatun_tx.send(packet).await.expect_err("stack closed");
        assert_eq!(send_error.kind(), io::ErrorKind::BrokenPipe);

        let mut pool = PacketBufPool::new(1);
        let receive_error = match gotatun_rx.recv(&mut pool).await {
            Ok(_) => panic!("stack sender should be closed"),
            Err(error) => error,
        };
        assert_eq!(receive_error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
