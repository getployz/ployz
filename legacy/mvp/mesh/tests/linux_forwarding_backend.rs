#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

use mvp_bus::IslandId;
use mvp_mesh::{
    ContainerSubnet, ForwardingObservation, LinuxOverlayForwardingBackend,
    LinuxOverlayForwardingConfig, OverlayForwardingBackend, OverlayForwardingSnapshot,
};

#[test]
fn linux_forwarding_backend_attaches_routes_and_detaches_when_enabled() {
    if std::env::var("MVP_LINUX_FORWARDING_SMOKE").ok().as_deref() != Some("1") {
        eprintln!("skipping Linux forwarding smoke; set MVP_LINUX_FORWARDING_SMOKE=1 to run");
        return;
    }
    let uid = Command::new("id").arg("-u").output().expect("id -u").stdout;
    assert_eq!(
        String::from_utf8_lossy(&uid).trim(),
        "0",
        "Linux forwarding smoke requires root"
    );
    let bpfctl = std::env::var_os("MVP_BPFCTL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ployz-bpfctl"));
    let bridge = format!("plzbr{}", std::process::id());
    let wg = format!("plzwg{}", std::process::id());
    let temp = tempfile::tempdir().expect("tempdir");

    run_ip(["link", "add", &bridge, "type", "bridge"]);
    run_ip(["link", "set", &bridge, "up"]);
    run_ip(["link", "add", &wg, "type", "dummy"]);
    run_ip(["link", "set", &wg, "up"]);

    let result = std::panic::catch_unwind(|| {
        let local = subnet("10.210.83.0/24");
        let remote = subnet("10.210.84.0/24");
        let snapshot = OverlayForwardingSnapshot::new(
            &IslandId::new("prod"),
            1,
            &bridge,
            &wg,
            local,
            [remote],
        )
        .with_observation(ForwardingObservation::Disabled);
        let backend = LinuxOverlayForwardingBackend::new(
            LinuxOverlayForwardingConfig::new(IslandId::new("prod"), temp.path().join("fwd.json"))
                .with_bpfctl_path(bpfctl),
        );
        let report = backend.apply(&snapshot).expect("apply forwarding");
        assert_eq!(report.remote_route_count, 1);
        assert!(backend.last_applied().expect("last applied").is_some());
        backend.detach(&bridge).expect("detach forwarding");
    });

    let _ = Command::new("ip").args(["link", "del", &wg]).status();
    let _ = Command::new("ip").args(["link", "del", &bridge]).status();

    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn subnet(value: &str) -> ContainerSubnet {
    ContainerSubnet::new(value.parse().expect("valid subnet")).expect("container subnet")
}

fn run_ip<const N: usize>(args: [&str; N]) {
    let status = Command::new("ip").args(args).status().expect("run ip");
    assert!(status.success(), "ip command failed");
}
