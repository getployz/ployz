use std::collections::BTreeSet;
use std::time::Duration;

use crate::SupervisorBackend;
use crate::execution::firewall::tests::{RecordingRunner, active, inactive};

use super::{
    BuiltinWireguardEbpfConfig, BuiltinWireguardHost, BuiltinWireguardHostConfig,
    BuiltinWireguardPorts, CorrosionGossipPhase,
};

#[test]
fn desired_gossip_fence_precedes_interface_activation_and_api_admission() {
    let outputs = [
        Ok(inactive()),
        active(""),
        active("Status: active\n"),
        active(""),
        active(""),
        active("-P INPUT ACCEPT\n"),
        active(""),
        active(""),
        Ok(inactive()),
        active(""),
        active("Status: active\n"),
        active("51820/udp ALLOW IN Anywhere\n"),
        active(""),
        active(""),
    ];
    let ebpf = BuiltinWireguardEbpfConfig::try_new(
        "br-ployz".to_owned(),
        "/ctl".into(),
        "/bytecode".into(),
        "/pin".into(),
    )
    .expect("eBPF config");
    let config = BuiltinWireguardHostConfig::try_new(
        "/key".into(),
        "ployz0".to_owned(),
        BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020).expect("ports"),
        1_420,
        ebpf,
        SupervisorBackend::Systemd,
        Duration::from_secs(5),
    )
    .expect("host config");
    let mut host =
        BuiltinWireguardHost::with_runner(config, RecordingRunner::with_outputs(outputs));
    let sources = BTreeSet::from(["fd00::/112".parse().expect("source")]);

    host.activate_mesh_interface(CorrosionGossipPhase::Desired(&sources))
        .expect("activate desired mesh");

    let restore = host
        .runner
        .calls
        .iter()
        .position(|call| call.starts_with("ip6tables-restore --noflush --wait 5 "));
    let link_up = host
        .runner
        .calls
        .iter()
        .position(|call| call == "ip link set dev ployz0 up");
    let api = host
        .runner
        .calls
        .iter()
        .position(|call| call.contains("port 2020 proto tcp comment ployz-api-http"));
    assert!(matches!((restore, link_up, api), (Some(a), Some(b), Some(c)) if a < b && b < c));
}
