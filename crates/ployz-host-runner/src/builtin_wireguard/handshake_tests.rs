use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::time::Duration;

use defguard_wireguard_rs::key::Key;
use ployz_core::corrosion::{
    BuiltinWireguardRosterEvidence, DesiredBuiltinWireguardLocal, DesiredBuiltinWireguardMesh,
    DesiredBuiltinWireguardRoamingPeer, derive_builtin_wireguard_member,
};
use ployz_core::ids::{ClusterId, PeerId};
use ployz_core::operation::FailureMessage;

use crate::{HostRunnerCommandOutput, HostRunnerCommandRunner, SupervisorBackend};

use super::{
    BuiltinWireguardEbpfConfig, BuiltinWireguardHost, BuiltinWireguardHostConfig,
    BuiltinWireguardLatestHandshake, BuiltinWireguardPorts,
};

#[test]
fn convergence_reports_the_fifth_dump_field_as_an_absolute_latest_handshake() {
    let fixture = TestFixture::new([peer_dump(1_786_000_123)]);
    let expected_peer = fixture.peer_key.clone();
    let mut host = fixture.host;

    let outcome = host.converge(&fixture.desired).expect("mesh converges");

    assert_eq!(
        outcome.latest_handshakes,
        BTreeMap::from([(
            expected_peer,
            BuiltinWireguardLatestHandshake::AtUnixSeconds {
                unix_seconds: 1_786_000_123,
            }
        )])
    );
    assert!(!format!("{outcome:?}").contains("test-interface-private-key"));
}

#[test]
fn convergence_reports_a_new_peer_post_apply_handshake_as_never() {
    let fixture = TestFixture::new([no_peers_dump(), peer_dump(0)]);
    let expected_peer = fixture.peer_key.clone();
    let mut host = fixture.host;

    let outcome = host.converge(&fixture.desired).expect("mesh converges");

    assert_eq!(
        outcome.latest_handshakes,
        BTreeMap::from([(expected_peer, BuiltinWireguardLatestHandshake::Never)])
    );
}

#[test]
fn malformed_latest_handshake_refuses_without_exposing_the_interface_private_key() {
    let fixture = TestFixture::new([malformed_peer_dump("not-a-unix-timestamp")]);
    let mut host = fixture.host;

    let error = host
        .converge(&fixture.desired)
        .expect_err("malformed latest handshake must refuse convergence");
    let message = error.to_string();

    assert!(message.contains("latest handshake \"not-a-unix-timestamp\""));
    assert!(!message.contains("test-interface-private-key"));
}

#[test]
fn fence_reports_an_empty_post_apply_latest_handshake_map() {
    let fixture = TestFixture::new([peer_dump(1_786_000_123), no_peers_dump()]);
    let local = fixture.desired.local.clone();
    let mut host = fixture.host;

    let outcome = host.fence(&local).expect("mesh fences");

    assert!(outcome.latest_handshakes.is_empty());
}

struct TestFixture {
    _directory: tempfile::TempDir,
    desired: DesiredBuiltinWireguardMesh,
    peer_key: ployz_core::network::WireGuardPublicKey,
    host: BuiltinWireguardHost<WireguardRunner>,
}

impl TestFixture {
    fn new(handshake_dumps: impl IntoIterator<Item = WireguardDump>) -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let private_key_path = directory.path().join("wireguard.key");
        let private_key = Key::generate();
        fs::write(&private_key_path, format!("{private_key}\n")).expect("write private key");
        let local_key =
            ployz_core::network::WireGuardPublicKey::try_new(private_key.public_key().to_string())
                .expect("local public key");
        let peer_private_key = Key::generate();
        let peer_key = ployz_core::network::WireGuardPublicKey::try_new(
            peer_private_key.public_key().to_string(),
        )
        .expect("peer public key");
        let cluster = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster");
        let local = derive_builtin_wireguard_member(&cluster, &local_key);
        let peer = derive_builtin_wireguard_member(&cluster, &peer_key);
        let desired = DesiredBuiltinWireguardMesh {
            local: DesiredBuiltinWireguardLocal {
                public_key: local_key.clone(),
                subnet_v6: local.subnet(),
                bind_address: local.bind_address(),
            },
            local_container_subnet: ployz_core::network::MachineEndpointSubnet::try_new(
                "10.210.1.0/24",
            )
            .expect("local subnet"),
            cluster_container_prefix: ployz_core::network::MachineEndpointSupernet::try_new(
                "10.210.0.0/16",
            )
            .expect("cluster prefix"),
            machine_peers: Vec::new(),
            roaming_peers: vec![DesiredBuiltinWireguardRoamingPeer {
                peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("peer id"),
                public_key: peer_key.clone(),
                subnet_v6: peer.subnet(),
                endpoint: None,
            }],
            ebpf_routes: Vec::new(),
            evidence: empty_roster_evidence(),
        };
        let runner = WireguardRunner {
            local_public_key: local_key.as_str().to_owned(),
            local_bind_address: format!("{}/112", local.bind_address().get()),
            peer_public_key: peer_key.as_str().to_owned(),
            peer_allowed_ips: peer.subnet().network().to_string(),
            handshake_dumps: handshake_dumps.into_iter().collect(),
            peer_present: false,
        };
        let config = host_config(private_key_path);
        let host = BuiltinWireguardHost::with_runner(config, runner);
        Self {
            _directory: directory,
            desired,
            peer_key,
            host,
        }
    }
}

fn peer_dump(latest_handshake: u64) -> WireguardDump {
    WireguardDump::Peer {
        latest_handshake: latest_handshake.to_string(),
    }
}

fn malformed_peer_dump(latest_handshake: &str) -> WireguardDump {
    WireguardDump::Peer {
        latest_handshake: latest_handshake.to_owned(),
    }
}

fn no_peers_dump() -> WireguardDump {
    WireguardDump::NoPeers
}

fn empty_roster_evidence() -> BuiltinWireguardRosterEvidence {
    BuiltinWireguardRosterEvidence {
        machine_skipped: Vec::new(),
        machine_shadows: Vec::new(),
        peer_skipped: Vec::new(),
        peer_shadows: Vec::new(),
        address_mismatches: Vec::new(),
        identity_conflicts: Vec::new(),
    }
}

fn host_config(private_key_path: std::path::PathBuf) -> BuiltinWireguardHostConfig {
    let ebpf = BuiltinWireguardEbpfConfig::try_new(
        "br-ployz".to_owned(),
        "/missing/ployz-ebpf-ctl".into(),
        "/missing/ployz-ebpf".into(),
        "/sys/fs/bpf/ployz".into(),
    )
    .expect("eBPF config");
    BuiltinWireguardHostConfig::try_new(
        private_key_path,
        "ployz0".to_owned(),
        BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
        1_420,
        ebpf,
        SupervisorBackend::Systemd,
        Duration::from_secs(5),
    )
    .expect("host config")
}

struct WireguardRunner {
    local_public_key: String,
    local_bind_address: String,
    peer_public_key: String,
    peer_allowed_ips: String,
    handshake_dumps: VecDeque<WireguardDump>,
    peer_present: bool,
}

enum WireguardDump {
    NoPeers,
    Peer { latest_handshake: String },
}

impl HostRunnerCommandRunner for WireguardRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        let output = match (program, args) {
            ("sysctl", ["-n", name]) => success(match *name {
                "net.ipv6.conf.all.disable_ipv6"
                | "net.ipv6.conf.default.disable_ipv6"
                | "net.ipv6.conf.ployz0.disable_ipv6"
                | "net.ipv4.conf.all.rp_filter"
                | "net.ipv4.conf.default.rp_filter"
                | "net.ipv4.conf.ployz0.rp_filter" => "0".to_owned(),
                "net.ipv6.conf.all.forwarding"
                | "net.ipv6.conf.default.forwarding"
                | "net.ipv4.ip_forward" => "1".to_owned(),
                unexpected => panic!("unexpected sysctl observation: {unexpected}"),
            }),
            ("systemctl", ["is-active", "--quiet", _]) => inactive(),
            ("sh", ["-c", _]) => absent(),
            (
                "systemctl",
                [
                    "list-units",
                    "--type=service",
                    "--state=active",
                    "--no-legend",
                    "--plain",
                ],
            ) => success(String::new()),
            ("wg", ["show", "ployz0", "public-key"]) => {
                success(format!("{}\n", self.local_public_key))
            }
            ("wg", ["show", "ployz0", "dump"]) => {
                let latest_handshake = self
                    .handshake_dumps
                    .pop_front()
                    .expect("one literal dump per observation");
                self.peer_present = matches!(&latest_handshake, WireguardDump::Peer { .. });
                success(match latest_handshake {
                    WireguardDump::Peer { latest_handshake } => format!(
                        "test-interface-private-key\t{}\t51820\toff\n{}\t(none)\t(none)\t{}\t{}\t31\t47\toff\n",
                        self.local_public_key,
                        self.peer_public_key,
                        self.peer_allowed_ips,
                        latest_handshake,
                    ),
                    WireguardDump::NoPeers => format!(
                        "test-interface-private-key\t{}\t51820\toff\n",
                        self.local_public_key,
                    ),
                })
            }
            (
                "ip",
                [
                    "-o",
                    "-6",
                    "address",
                    "show",
                    "dev",
                    "ployz0",
                    "scope",
                    "global",
                ],
            ) => success(format!(
                "7: ployz0 inet6 {} scope global nodad\n",
                self.local_bind_address
            )),
            (
                "ip",
                [
                    "-o",
                    "-4",
                    "route",
                    "show",
                    "dev",
                    "ployz0",
                    "proto",
                    "boot",
                ],
            ) => success(String::new()),
            (
                "ip",
                [
                    "-o",
                    "-6",
                    "route",
                    "show",
                    "dev",
                    "ployz0",
                    "proto",
                    "boot",
                ],
            ) => {
                if self.peer_present {
                    success(format!(
                        "{} dev ployz0 proto boot scope link\n",
                        self.peer_allowed_ips
                    ))
                } else {
                    success(String::new())
                }
            }
            ("ip", ["link", "show", "dev", "br-ployz"]) => inactive(),
            ("sysctl", ["-w", _])
            | ("ip", _)
            | ("ip6tables", _)
            | ("ip6tables-restore", _)
            | ("wg", ["set", ..]) => success(String::new()),
            _ => panic!("unexpected command: {program} {}", args.join(" ")),
        };
        Ok(output)
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, _: &str, _: &Path) -> Result<(), FailureMessage> {
        unreachable!()
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        unreachable!()
    }

    fn docker_is_installed(&mut self) -> bool {
        false
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        unreachable!()
    }

    fn docker_has_insecure_registry(&mut self, _: &str) -> Result<bool, FailureMessage> {
        unreachable!()
    }
}

fn success(stdout: String) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout,
        stdout_truncated: false,
        failure: String::new(),
    }
}

fn inactive() -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(3),
        stdout: String::new(),
        stdout_truncated: false,
        failure: "inactive".to_owned(),
    }
}

fn absent() -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(1),
        stdout: String::new(),
        stdout_truncated: false,
        failure: "absent".to_owned(),
    }
}
