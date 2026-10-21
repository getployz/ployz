//! Whether a Container still serves a connection, from the `/proc/<pid>/net/tcp`
//! and `tcp6` tables of its network namespace.

use std::{
    collections::BTreeSet,
    fs, io,
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
};

/// The states, as the kernel prints them, in which the copy can still send the client
/// data it waits for: ESTABLISHED, SYN_RECV, FIN_WAIT1, CLOSE_WAIT, LAST_ACK and
/// CLOSING. A client that half-closes after its request leaves the copy in CLOSE_WAIT
/// until it replies. In FIN_WAIT1, LAST_ACK and CLOSING the copy's last data or FIN is
/// not yet acknowledged, and stopping the Container drops it.
const ANSWERING: [&str; 6] = ["01", "03", "04", "08", "09", "0B"];
const LISTEN: &str = "0A";
/// The lowest port Linux gives an outbound connection in a new network namespace,
/// where `net.ipv4.ip_local_port_range` starts at 32768 to 60999. A connection the
/// copy accepted keeps its listener's port, so one below this is inbound even after
/// that listener closed.
const FIRST_EPHEMERAL_PORT: u16 = 32768;

/// Whether the namespace `pid` lives in, under the `proc_root` procfs mount, has a
/// TCP connection the copy may still answer: one from off the namespace's loopback,
/// on a port in `listening` or below the ephemeral range, or on any port while
/// `listening` is empty. This read adds the ports it sees listening to `listening`,
/// so a drain that keeps the set across its reads still counts a port whose
/// listener closed while a connection to it lives.
///
/// # Errors
///
/// Returns when a table cannot be read, as when the process is gone. A kernel
/// without IPv6 has no `tcp6`, which reads as no IPv6 socket.
pub(super) fn holds_connection(
    proc_root: &Path,
    pid: u32,
    listening: &mut BTreeSet<u16>,
) -> io::Result<bool> {
    let net = proc_root.join(pid.to_string()).join("net");
    let tcp = fs::read_to_string(net.join("tcp"))?;
    let tcp6 = match fs::read_to_string(net.join("tcp6")) {
        Ok(table) => table,
        Err(error) if error.kind() == io::ErrorKind::NotFound && net.is_dir() => String::new(),
        Err(error) => return Err(error),
    };
    Ok(serves_connection(&[&tcp, &tcp6], listening))
}

/// Whether `tables`, the text of a namespace's `net/tcp` and `net/tcp6`, list an
/// inbound connection, after adding their listening ports to `listening`. A
/// dual-stack listener sits in `tcp6` while its IPv4 connections may sit in `tcp`, so
/// the listening ports come from every table. An outbound connection's local port is
/// usually ephemeral and not one the namespace listens on. A copy may close a listener
/// before the drain begins and still answer what it accepted, so a port below the
/// ephemeral range counts unseen, and before any listener is seen every port does.
/// A connection from loopback is the copy talking to itself, as nginx to php-fpm,
/// so it holds nothing, and a listener on loopback, such as Docker's DNS server on
/// 127.0.0.11, only ever accepts those, so it is not counted as one.
fn serves_connection(tables: &[&str], listening: &mut BTreeSet<u16>) -> bool {
    let rows = || tables.iter().flat_map(|table| sockets(table));
    listening.extend(
        rows()
            .filter(|socket| socket.state == LISTEN && !from_loopback(socket.local))
            .map(|socket| socket.port),
    );
    rows().any(|socket| {
        ANSWERING.contains(&socket.state)
            && (socket.port < FIRST_EPHEMERAL_PORT
                || listening.is_empty()
                || listening.contains(&socket.port))
            && !from_loopback(socket.remote)
    })
}

struct Socket<'table> {
    local: &'table str,
    port: u16,
    remote: &'table str,
    state: &'table str,
}

fn sockets(table: &str) -> impl Iterator<Item = Socket<'_>> {
    table.lines().skip(1).filter_map(|row| {
        let mut columns = row.split_whitespace().skip(1);
        let (local, remote, state) = (columns.next()?, columns.next()?, columns.next()?);
        let (local, port) = local.rsplit_once(':')?;
        let (remote, _) = remote.rsplit_once(':')?;
        Some(Socket {
            local,
            port: u16::from_str_radix(port, 16).ok()?,
            remote,
            state,
        })
    })
}

/// Whether `address`, as the kernel prints it, is 127.0.0.0/8, `::1` or
/// `::ffff:127.x.x.x`. The kernel prints each 32-bit word of the address in host
/// order. An address that does not parse reads as remote.
fn from_loopback(address: &str) -> bool {
    let word = |at: usize| -> Option<[u8; 4]> {
        let word = u32::from_str_radix(address.get(at * 8..at * 8 + 8)?, 16).ok()?;
        Some(word.to_ne_bytes())
    };
    match address.len() {
        8 => word(0).is_some_and(|word| Ipv4Addr::from(word).is_loopback()),
        32 => {
            let mut bytes = [0; 16];
            for (at, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                let Some(word) = word(at) else {
                    return false;
                };
                chunk.copy_from_slice(&word);
            }
            let ip = Ipv6Addr::from(bytes);
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
    const TCP6_HEADER: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    const TCP_LISTENER_8080: &str = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1001 1 0000000000000000 100 0 0 10 0";
    const TCP_INBOUND_8080: &str = "   1: 0A010005:1F90 0A010002:C350 01 00000000:00000000 02:000AFC7E 00000000     0        0 1002 1 0000000000000000 20 4 30 10 -1";
    const TCP6_LISTENER_80: &str = "   0: 00000000000000000000000000000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2001 1 0000000000000000 100 0 0 10 0";
    const TCP6_INBOUND_80: &str = "   1: 0000000000000000FFFF00000501010A:0050 0000000000000000FFFF00000201010A:D431 01 00000000:00000000 02:000AFC7E 00000000     0        0 2002 1 0000000000000000 20 4 30 10 -1";
    /// Docker's DNS server, which listens on 127.0.0.11 in every copy on a user-defined
    /// bridge such as `ployz`.
    const TCP_DOCKER_DNS: &str = "   9: 0B00007F:AD4B 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1009 1 0000000000000000 100 0 0 10 0";
    /// The copy's own connection to a database on 10.1.0.9:5432, from an ephemeral port.
    const TCP_OUTBOUND: &str = "   2: 0A010005:A2C4 0A010009:1538 01 00000000:00000000 02:000AFC7E 00000000     0        0 1003 1 0000000000000000 20 4 30 10 -1";

    fn serves(tables: &[&str]) -> bool {
        serves_connection(tables, &mut BTreeSet::new())
    }

    fn table(header: &str, rows: &[&str]) -> String {
        let mut text = header.to_owned();
        for row in rows {
            text.push_str(row);
            text.push('\n');
        }
        text
    }

    #[test]
    fn drain_idle_sees_an_ipv4_connection_to_a_listener_on_any_address() {
        let tcp = table(TCP_HEADER, &[TCP_LISTENER_8080, TCP_INBOUND_8080]);
        assert!(serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_sees_an_ipv6_connection_to_a_listener_on_any_address() {
        let tcp6 = table(TCP6_HEADER, &[TCP6_LISTENER_80, TCP6_INBOUND_80]);
        assert!(serves(&[TCP_HEADER, &tcp6]));
    }

    #[test]
    fn drain_idle_sees_an_ipv4_connection_to_a_dual_stack_listener() {
        let tcp = table(TCP_HEADER, &[TCP_INBOUND_8080]);
        let tcp6 = table(
            TCP6_HEADER,
            &[
                "   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2001 1 0000000000000000 100 0 0 10 0",
            ],
        );
        assert!(serves(&[&tcp, &tcp6]));
    }

    #[test]
    fn drain_idle_ignores_outbound_connections() {
        let tcp = table(TCP_HEADER, &[TCP_LISTENER_8080, TCP_OUTBOUND]);
        assert!(!serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_ignores_connections_from_loopback() {
        let tcp = table(
            TCP_HEADER,
            &[
                // php-fpm on 127.0.0.1:9000, and nginx's connection to it from both ends.
                "   0: 0100007F:2328 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0000000000000000 100 0 0 10 0",
                "   1: 0100007F:2328 0100007F:B3A0 01 00000000:00000000 02:000AFC7E 00000000     0        0 2 1 0000000000000000 20 4 30 10 -1",
                "   2: 0100007F:B3A0 0100007F:2328 01 00000000:00000000 02:000AFC7E 00000000     0        0 3 1 0000000000000000 20 4 30 10 -1",
                // A pool to the copy's own Redis on 0.0.0.0:6379, from 127.0.0.2.
                "   3: 00000000:18EB 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 4 1 0000000000000000 100 0 0 10 0",
                "   4: 0100007F:18EB 0200007F:C001 01 00000000:00000000 02:000AFC7E 00000000     0        0 5 1 0000000000000000 20 4 30 10 -1",
            ],
        );
        let tcp6 = table(
            TCP6_HEADER,
            &[
                TCP6_LISTENER_80,
                "   1: 00000000000000000000000001000000:0050 00000000000000000000000001000000:D431 01 00000000:00000000 02:000AFC7E 00000000     0        0 6 1 0000000000000000 20 4 30 10 -1",
                "   2: 0000000000000000FFFF00000100007F:0050 0000000000000000FFFF00000100007F:D432 01 00000000:00000000 02:000AFC7E 00000000     0        0 7 1 0000000000000000 20 4 30 10 -1",
            ],
        );
        assert!(!serves(&[&tcp, &tcp6]));
    }

    #[test]
    fn drain_idle_keeps_counting_a_port_whose_listener_closed() {
        let mut listening = BTreeSet::new();
        let first = table(TCP_HEADER, &[TCP_LISTENER_8080, TCP_INBOUND_8080]);
        assert!(serves_connection(&[&first, TCP6_HEADER], &mut listening));

        let later = table(TCP_HEADER, &[TCP_INBOUND_8080]);
        assert!(serves_connection(&[&later, TCP6_HEADER], &mut listening));
    }

    #[test]
    fn drain_idle_counts_a_connection_accepted_before_the_listener_closed() {
        let tcp = table(TCP_HEADER, &[TCP_DOCKER_DNS, TCP_INBOUND_8080]);
        assert!(serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_counts_a_connection_whose_listener_closed_beside_another_listener() {
        let tcp = table(
            TCP_HEADER,
            &[
                TCP_INBOUND_8080,
                // A metrics listener on 9100 the copy keeps open.
                "   2: 00000000:238C 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1004 1 0000000000000000 100 0 0 10 0",
            ],
        );
        assert!(serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_counts_a_connection_above_the_ephemeral_floor_whose_only_listener_closed() {
        let tcp = table(
            TCP_HEADER,
            &[
                TCP_DOCKER_DNS,
                // A request accepted on 0.0.0.0:40000, whose listener has closed.
                "   1: 0A010005:9C40 0A010002:C350 01 00000000:00000000 02:000AFC7E 00000000     0        0 1002 1 0000000000000000 20 4 30 10 -1",
            ],
        );
        assert!(serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_counts_an_outside_connection_of_a_copy_that_never_listened() {
        let tcp = table(TCP_HEADER, &[TCP_DOCKER_DNS, TCP_OUTBOUND]);
        assert!(serves(&[&tcp, TCP6_HEADER]));
    }

    #[test]
    fn drain_idle_tells_inbound_from_outbound_at_the_ephemeral_floor() {
        let at = |port: &str, row: &str| {
            format!(
                "   {row}: 0A010005:{port} 0A010002:C350 01 00000000:00000000 02:000AFC7E 00000000     0        0 1 1 0000000000000000 20 4 30 10 -1"
            )
        };
        let below = table(TCP_HEADER, &[TCP_LISTENER_8080, &at("7FFF", "1")]);
        assert!(serves(&[&below, TCP6_HEADER]), "32767 is inbound");
        let floor = table(TCP_HEADER, &[TCP_LISTENER_8080, &at("8000", "1")]);
        assert!(!serves(&[&floor, TCP6_HEADER]), "32768 is outbound");
    }

    #[test]
    fn drain_idle_finds_nothing_in_a_copy_that_never_listened_and_only_dials_or_talks_to_itself() {
        let tcp = table(
            TCP_HEADER,
            &[
                TCP_DOCKER_DNS,
                "   1: 0A010005:A2C5 0A010009:1538 02 00000000:00000000 01:00000064 00000000     0        0 1 1 0000000000000000 20 4 30 10 -1",
                "   2: 0100007F:B3A0 0100007F:2328 01 00000000:00000000 02:000AFC7E 00000000     0        0 2 1 0000000000000000 20 4 30 10 -1",
            ],
        );
        assert!(
            !serves(&[&tcp, TCP6_HEADER]),
            "SYN_SENT and loopback hold nothing"
        );
    }

    #[test]
    fn drain_idle_finds_nothing_in_a_copy_without_sockets() {
        assert!(!serves(&[TCP_HEADER, TCP6_HEADER]));
    }

    fn inbound_8080_in(state: &str) -> String {
        format!(
            "   1: 0A010005:1F90 0A010002:C350 {state} 00000000:00000000 00:00000000 00000000     0        0 0 1 0000000000000000 20 4 30 10 -1"
        )
    }

    #[test]
    fn drain_idle_counts_a_connection_the_copy_may_still_answer() {
        for (state, name) in [
            ("03", "SYN_RECV"),
            ("04", "FIN_WAIT1"),
            ("08", "CLOSE_WAIT"),
            ("09", "LAST_ACK"),
            ("0B", "CLOSING"),
        ] {
            let tcp = table(TCP_HEADER, &[TCP_LISTENER_8080, &inbound_8080_in(state)]);
            assert!(serves(&[&tcp, TCP6_HEADER]), "{name}");
        }
    }

    #[test]
    fn drain_idle_ignores_a_connection_whose_reply_was_delivered() {
        for (state, name) in [("05", "FIN_WAIT2"), ("06", "TIME_WAIT"), ("07", "CLOSE")] {
            let tcp = table(TCP_HEADER, &[TCP_LISTENER_8080, &inbound_8080_in(state)]);
            assert!(!serves(&[&tcp, TCP6_HEADER]), "{name}");
        }
    }

    #[test]
    fn drain_idle_reads_both_tables_of_the_pids_namespace() {
        let root = std::env::temp_dir().join(format!("ployzd-drain-idle-{}", std::process::id()));
        let net = root.join("4242/net");
        fs::create_dir_all(&net).unwrap();
        fs::write(net.join("tcp"), TCP_HEADER).unwrap();
        fs::write(
            net.join("tcp6"),
            table(TCP6_HEADER, &[TCP6_LISTENER_80, TCP6_INBOUND_80]),
        )
        .unwrap();
        assert!(holds_connection(&root, 4242, &mut BTreeSet::new()).unwrap());

        fs::remove_file(net.join("tcp6")).unwrap();
        assert!(
            !holds_connection(&root, 4242, &mut BTreeSet::new()).unwrap(),
            "a kernel without IPv6 has no tcp6"
        );
        assert!(
            holds_connection(&root, 4243, &mut BTreeSet::new()).is_err(),
            "a gone process is no evidence of idleness"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drain_idle_reads_a_live_loopback_connection_from_the_kernel() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = || {
            let tcp = fs::read_to_string("/proc/self/net/tcp").unwrap();
            sockets(&tcp)
                .find(|socket| socket.port == port && socket.state == "01")
                .map(|socket| from_loopback(socket.remote))
        };
        assert_eq!(served(), None);

        let _client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_served, _) = listener.accept().unwrap();
        assert_eq!(
            served(),
            Some(true),
            "the kernel lists the accepted connection, from loopback"
        );
    }

    #[test]
    fn drain_idle_reads_a_live_half_closed_request_in_an_answering_state() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut served, _) = listener.accept().unwrap();
        client.write_all(b"request").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut request = Vec::new();
        served.read_to_end(&mut request).unwrap();

        let tcp = fs::read_to_string("/proc/self/net/tcp").unwrap();
        let state = sockets(&tcp)
            .find(|socket| socket.port == port && socket.state != LISTEN)
            .map(|socket| socket.state.to_owned())
            .unwrap();
        assert_eq!(
            state, "08",
            "the kernel lists the served side in CLOSE_WAIT"
        );
        assert!(
            ANSWERING.contains(&state.as_str()),
            "the copy can still send its reply"
        );
    }
}
