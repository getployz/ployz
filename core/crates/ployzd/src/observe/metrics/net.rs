//! A container's network byte counters, from `/proc/<pid>/net/dev`.
//!
//! The file lists every interface in the process's network namespace. Every
//! interface but loopback counts toward one total. A container reaches the
//! mesh and the public network through the same interface, so the mesh and
//! public split is a Machine metric, not a container one.

use std::fs;

use super::host;

const LOOPBACK: &str = "lo";

/// Raw cumulative byte totals over every interface but loopback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetCounters {
    pub rx: u64,
    pub tx: u64,
}

/// Reads the network counters of the namespace `pid` lives in. `None` when
/// the process is gone or the file is unreadable.
pub fn read_net_dev(pid: u32) -> Option<NetCounters> {
    let text = fs::read_to_string(format!("/proc/{pid}/net/dev")).ok()?;
    Some(total(&text))
}

fn total(text: &str) -> NetCounters {
    host::parse_net_dev(text)
        .iter()
        .filter(|interface| interface.name != LOOPBACK)
        .fold(NetCounters::default(), |sum, interface| NetCounters {
            rx: sum.rx + interface.rx,
            tx: sum.tx + interface.tx,
        })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn every_interface_but_loopback_is_summed() {
        let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:     900      9    0    0    0     0          0         0      900       9    0    0    0     0       0          0
ployz-wg:    1000     10    0    0    0     0          0         0     2000      20    0    0    0     0       0          0
  eth0:12345678901   40    0    0    0     0          0         0      400      30    0    0    0     0       0          0
";
        assert_eq!(
            total(text),
            NetCounters {
                rx: 12_345_679_901,
                tx: 2400,
            }
        );
    }

    #[test]
    fn a_missing_process_reads_none() {
        assert_eq!(read_net_dev(u32::MAX), None);
    }

    /// Start the container first:
    /// `docker run -d --rm --name m2b-probe alpine:3 sh -c 'wget -qO- http://example.com >/dev/null; sleep 300'`
    #[test]
    #[ignore = "requires Docker and a running m2b-probe container"]
    fn reads_a_running_containers_bytes() {
        let output = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Pid}}", "m2b-probe"])
            .output()
            .unwrap();
        assert!(output.status.success(), "m2b-probe is not running");
        let pid = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let counters = read_net_dev(pid).unwrap();
        println!("pid {pid}: {counters:?}");
        assert!(counters.rx > 0 && counters.tx > 0);
    }
}
