//! Which program holds a UDP port on this host, read from `/proc`.

use std::{fs, path::Path};

/// The program bound to UDP `port`, as `name (pid N)`. None when the port is free, or Ployz holds
/// it: ployzd running WireGuard in userspace, or this CLI listening for Cloud's port check. A socket
/// no process holds is the kernel's, as WireGuard's is; whether it is Ployz's interface or another
/// can't be read here, and a program that just let go leaves one too, so it names no one.
pub(crate) fn udp_holder(proc: &Path, port: u16) -> Option<String> {
    let sockets: Vec<String> = ["net/udp", "net/udp6"]
        .iter()
        .filter_map(|table| fs::read_to_string(proc.join(table)).ok())
        .flat_map(|table| bound_sockets(&table, port))
        .map(|inode| format!("socket:[{inode}]"))
        .collect();
    if sockets.is_empty() {
        return None;
    }
    let (pid, name) = owner(proc, &sockets)?;
    (name != "ployzd" && name != "ployz").then(|| format!("{name} (pid {pid})"))
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

/// The first process with an open descriptor on one of `sockets`, with its command name. One that
/// exits before its name is read is skipped.
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
        if !holds {
            return None;
        }
        let name = fs::read_to_string(entry.path().join("comm")).ok()?;
        let name = name.trim();
        (!name.is_empty()).then(|| (pid, name.to_owned()))
    })
}

#[cfg(test)]
#[path = "port_holder_tests.rs"]
mod tests;
