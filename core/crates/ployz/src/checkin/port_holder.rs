//! Which program holds a UDP port on this host, read from `/proc`.

use std::{fs, path::Path};

/// The program bound to UDP `port`, as `name (pid N)`. A socket no process holds is the kernel's,
/// as a WireGuard interface's is: None when `own_interface`, Ployz's, is up, else another
/// interface's. None too when the port is free, or Ployz holds it: ployzd running WireGuard in
/// userspace, or this CLI listening for Cloud's port check.
pub(crate) fn udp_holder(proc: &Path, port: u16, own_interface: bool) -> Option<String> {
    let sockets: Vec<String> = ["net/udp", "net/udp6"]
        .iter()
        .filter_map(|table| fs::read_to_string(proc.join(table)).ok())
        .flat_map(|table| bound_sockets(&table, port))
        .map(|inode| format!("socket:[{inode}]"))
        .collect();
    if sockets.is_empty() {
        return None;
    }
    match owner(proc, &sockets) {
        Some((_, name)) if name == "ployzd" || name == "ployz" => None,
        Some((pid, name)) => Some(format!("{name} (pid {pid})")),
        None if own_interface => None,
        None => Some("another WireGuard interface".to_owned()),
    }
}

/// Inodes of the sockets a `/proc/net/udp` table lists on local `port`.
fn bound_sockets(table: &str, port: u16) -> Vec<u64> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (_, local_port) = fields.get(1)?.rsplit_once(':')?;
            if u16::from_str_radix(local_port, 16).ok()? != port {
                return None;
            }
            fields.get(9)?.parse().ok()
        })
        .collect()
}

/// The first process with an open descriptor on one of `sockets`, with its command name.
fn owner(proc: &Path, sockets: &[String]) -> Option<(u32, String)> {
    fs::read_dir(proc).ok()?.flatten().find_map(|entry| {
        let pid = entry.file_name().to_str()?.parse().ok()?;
        let holds = fs::read_dir(entry.path().join("fd"))
            .ok()?
            .flatten()
            .filter_map(|fd| fs::read_link(fd.path()).ok())
            .any(|target| {
                sockets
                    .iter()
                    .any(|socket| target.as_os_str() == socket.as_str())
            });
        holds.then(|| {
            let name = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            (pid, name.trim().to_owned())
        })
    })
}

#[cfg(test)]
#[path = "port_holder_tests.rs"]
mod tests;
