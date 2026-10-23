use std::{fs, os::unix::fs::symlink, path::Path};

use super::udp_holder;

const HEADER: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n";

fn table(rows: &[(&str, u64)]) -> String {
    let mut text = HEADER.to_owned();
    for (sl, (local, inode)) in rows.iter().enumerate() {
        text.push_str(&format!(
            "  {sl}: {local} 00000000:0000 07 00000000:00000000 00:00000000 00000000   107        0 {inode} 2 0000000000000000 0\n"
        ));
    }
    text
}

fn process(proc: &Path, pid: u32, name: &str, sockets: &[u64]) {
    let fd = proc.join(pid.to_string()).join("fd");
    fs::create_dir_all(&fd).unwrap();
    fs::write(proc.join(pid.to_string()).join("comm"), format!("{name}\n")).unwrap();
    for (n, inode) in sockets.iter().enumerate() {
        symlink(format!("socket:[{inode}]"), fd.join(n.to_string())).unwrap();
    }
}

fn proc_with(udp: &[(&str, u64)], udp6: &[(&str, u64)]) -> tempfile::TempDir {
    let proc = tempfile::tempdir().unwrap();
    fs::create_dir_all(proc.path().join("net")).unwrap();
    fs::write(proc.path().join("net/udp"), table(udp)).unwrap();
    fs::write(proc.path().join("net/udp6"), table(udp6)).unwrap();
    proc
}

#[test]
fn a_program_on_the_port_is_named_with_its_pid() {
    // 0xCA6C is 51820; 0x14E9 is 5353, avahi's own mDNS port.
    let proc = proc_with(&[("00000000:14E9", 41), ("00000000:CA6C", 42)], &[]);
    process(proc.path(), 812, "avahi-daemon", &[41, 42]);
    process(proc.path(), 900, "sshd", &[7]);

    assert_eq!(
        udp_holder(proc.path(), 51820, false).as_deref(),
        Some("avahi-daemon (pid 812)")
    );
}

#[test]
fn an_ipv6_socket_counts_too() {
    let proc = proc_with(&[], &[("00000000000000000000000000000000:CA6C", 43)]);
    process(proc.path(), 77, "dnsmasq", &[43]);

    assert_eq!(
        udp_holder(proc.path(), 51820, false).as_deref(),
        Some("dnsmasq (pid 77)")
    );
}

#[test]
fn a_free_port_holds_nothing() {
    let free = proc_with(&[("00000000:14E9", 41)], &[]);
    assert_eq!(udp_holder(free.path(), 51820, false), None);
}

#[test]
fn ployz_itself_is_not_another_holder() {
    // ployzd running WireGuard in userspace, and the CLI listening for Cloud's port check.
    for name in ["ployzd", "ployz"] {
        let proc = proc_with(&[("00000000:CA6C", 44)], &[]);
        process(proc.path(), 55, name, &[44]);

        assert_eq!(udp_holder(proc.path(), 51820, false), None, "{name}");
    }
}

#[test]
fn a_socket_no_process_holds_is_ployzs_wireguard_only_while_its_interface_is_up() {
    // Kernel WireGuard's sockets have inodes but no process holding them, as on a real Server.
    let proc = proc_with(
        &[("00000000:CA6C", 18205)],
        &[("00000000000000000000000000000000:CA6C", 18206)],
    );
    process(proc.path(), 900, "sshd", &[7]);

    assert_eq!(udp_holder(proc.path(), 51820, true), None);
    assert_eq!(
        udp_holder(proc.path(), 51820, false).as_deref(),
        Some("another WireGuard interface")
    );
}
