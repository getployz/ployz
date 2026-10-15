use std::{
    env,
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Command, Stdio},
};

use defguard_wireguard_rs::{
    Kernel, WGApi, WireguardInterfaceApi, host::Peer, key::Key, net::IpAddrMask,
};
use ployz_core::{CORROSION_GOSSIP_PORT, MACHINE_API_PORT, ManagementAddress, UNREGISTRY_PORT};
use ployzd::network::{NetworkError, apply_firewall_rules, fence_builder};

const TEST_NAME: &str = "mesh_firewall_policy";
const ERROR_TEST_NAME: &str = "firewall_surfaces_command_start_errors";
const ERROR_TEST_CHILD: &str = "PLOYZ_FIREWALL_ERROR_TEST_CHILD";
const LAN: &str = "192.168.77.2";
const METADATA: &str = "169.254.169.254";
const INTERNET: &str = "198.51.100.2";
const PUBLIC: &str = "203.0.113.1";

#[test]
#[ignore = "requires passwordless sudo and Linux network namespaces"]
fn wireguard_multipart_dump_preserves_peer() {
    const CHILD: &str = "PLOYZ_WIREGUARD_MULTIPART_TEST_CHILD";
    if env::var_os(CHILD).is_some() {
        let mut wireguard = WGApi::<Kernel>::new("multipart-wg").unwrap();
        wireguard.create_interface().unwrap();
        let mut peer = Peer::new(Key::new([7; 32]));
        peer.endpoint = Some("192.0.2.2:51820".parse().unwrap());
        peer.persistent_keepalive_interval = Some(25);
        // This exceeds Linux's 8 KiB maximum NLMSG_GOODSIZE, even with 64 KiB pages.
        peer.allowed_ips = (0..1024)
            .map(|index| {
                IpAddrMask::host(std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                    (0xfdcc_u128 << 112) | index,
                )))
            })
            .collect();
        wireguard.configure_peer(&peer).unwrap();
        let mut host = wireguard.read_interface_data().unwrap();
        assert_eq!(host.peers.len(), 1);
        let mut installed = host.peers.remove(&peer.public_key).unwrap();
        assert_eq!(installed.endpoint, peer.endpoint);
        assert_eq!(installed.persistent_keepalive_interval, Some(25));
        installed.allowed_ips.sort_by_key(|address| address.address);
        assert_eq!(installed.allowed_ips, peer.allowed_ips);
        return;
    }

    let namespace = format!("p35-wg-dump-{}", std::process::id());
    let _namespaces = Namespaces::new([&namespace]);
    let output = Command::new("sudo")
        .args(["-n", "ip", "netns", "exec", &namespace, "env"])
        .arg(format!("{CHILD}=1"))
        .arg(env::current_exe().unwrap())
        .args([
            "--exact",
            "wireguard_multipart_dump_preserves_peer",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "WireGuard multipart dump failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn firewall_surfaces_command_start_errors() {
    if env::var_os(ERROR_TEST_CHILD).is_some() {
        let error = apply_firewall_rules(
            "10.210.2.0/24".parse().unwrap(),
            ManagementAddress("fdcc::2".parse().unwrap()),
        )
        .unwrap_err();
        assert!(matches!(error, NetworkError::Io(_)));
        return;
    }

    let status = Command::new(env::current_exe().unwrap())
        .env(ERROR_TEST_CHILD, "1")
        .env("PATH", "/path/that/does/not/exist")
        .args(["--exact", ERROR_TEST_NAME])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "requires passwordless sudo and Linux network namespaces"]
fn mesh_firewall_policy() {
    if let Ok(subnet) = env::var("PLOYZ_FIREWALL_SUBNET") {
        let management_address = env::var("PLOYZ_FIREWALL_MANAGEMENT_ADDRESS")
            .unwrap()
            .parse()
            .unwrap();
        apply_firewall_rules(
            subnet.parse().unwrap(),
            ManagementAddress(management_address),
        )
        .unwrap();
        return;
    }

    sudo(&["-n", "true"]);
    let suffix = std::process::id();
    let source = format!("p35-source-{suffix}");
    let target = format!("p35-target-{suffix}");
    let container = format!("p35-container-{suffix}");
    let internet = format!("p35-internet-{suffix}");
    let _namespaces = Namespaces::new([&source, &target, &container, &internet]);

    sudo(&[
        "ip",
        "link",
        "add",
        "source-wg",
        "type",
        "veth",
        "peer",
        "name",
        "target-wg",
    ]);
    sudo(&["ip", "link", "set", "source-wg", "netns", &source]);
    sudo(&["ip", "link", "set", "target-wg", "netns", &target]);
    ns(
        &source,
        "ip",
        &["link", "set", "source-wg", "name", "ployz-wg"],
    );
    ns(
        &target,
        "ip",
        &["link", "set", "target-wg", "name", "ployz-wg"],
    );

    sudo(&[
        "ip",
        "link",
        "add",
        "target-ctr",
        "type",
        "veth",
        "peer",
        "name",
        "container-if",
    ]);
    sudo(&["ip", "link", "set", "target-ctr", "netns", &target]);
    sudo(&["ip", "link", "set", "container-if", "netns", &container]);

    sudo(&[
        "ip",
        "link",
        "add",
        "source-net",
        "type",
        "veth",
        "peer",
        "name",
        "internet-if",
    ]);
    sudo(&["ip", "link", "set", "source-net", "netns", &source]);
    sudo(&["ip", "link", "set", "internet-if", "netns", &internet]);

    for namespace in [&source, &target, &container, &internet] {
        ns(namespace, "ip", &["link", "set", "lo", "up"]);
    }
    ns(&source, "ip", &["link", "add", "ployz", "type", "bridge"]);
    ns(&source, "ip", &["link", "set", "ployz", "up"]);
    ns(&target, "ip", &["link", "add", "ployz", "type", "bridge"]);
    ns(
        &target,
        "ip",
        &["link", "set", "target-ctr", "master", "ployz"],
    );

    configure_link(&source, "ployz-wg", "192.0.2.1/30");
    configure_link(&target, "ployz-wg", "192.0.2.2/30");
    configure_link(&target, "ployz", "10.210.2.1/24");
    ns(&target, "ip", &["link", "set", "target-ctr", "up"]);
    configure_link(&container, "container-if", "10.210.2.2/24");
    ns(
        &target,
        "ip",
        &[
            "-6",
            "address",
            "add",
            "fd00::1/64",
            "dev",
            "ployz",
            "nodad",
        ],
    );
    ns(
        &container,
        "ip",
        &[
            "-6",
            "address",
            "add",
            "fd00::2/64",
            "dev",
            "container-if",
            "nodad",
        ],
    );
    configure_link(&source, "source-net", "198.51.100.1/30");
    configure_link(&internet, "internet-if", "198.51.100.2/30");
    ns(
        &source,
        "ip",
        &["address", "add", "10.210.1.2/32", "dev", "lo"],
    );
    ns(
        &source,
        "ip",
        &["route", "add", "10.210.2.0/24", "via", "192.0.2.2"],
    );
    ns(
        &target,
        "ip",
        &["route", "add", "10.210.1.0/24", "via", "192.0.2.1"],
    );
    ns(
        &container,
        "ip",
        &["route", "add", "default", "via", "10.210.2.1"],
    );
    ns(
        &container,
        "ip",
        &["-6", "route", "add", "fdcc::2/128", "via", "fd00::1"],
    );
    ns(
        &source,
        "ip",
        &[
            "-6",
            "address",
            "add",
            "fdcc::1/128",
            "dev",
            "ployz-wg",
            "nodad",
        ],
    );
    ns(
        &target,
        "ip",
        &[
            "-6",
            "address",
            "add",
            "fdcc::2/128",
            "dev",
            "ployz-wg",
            "nodad",
        ],
    );
    ns(
        &source,
        "ip",
        &["-6", "route", "add", "fdcc::2/128", "dev", "ployz-wg"],
    );
    ns(
        &target,
        "ip",
        &["-6", "route", "add", "fdcc::1/128", "dev", "ployz-wg"],
    );
    ns(&target, "sysctl", &["-q", "-w", "net.ipv4.ip_forward=1"]);

    for namespace in [&source, &target] {
        ns(namespace, "iptables", &["-N", "DOCKER-USER"]);
        ns(
            namespace,
            "iptables",
            &["-I", "FORWARD", "1", "-j", "DOCKER-USER"],
        );
    }
    ns(
        &target,
        "iptables",
        &[
            "-I",
            "FORWARD",
            "1",
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ],
    );
    ns(&target, "iptables", &["-P", "FORWARD", "DROP"]);
    ns(&target, "iptables", &["-P", "INPUT", "DROP"]);
    ns(&target, "ip6tables", &["-P", "INPUT", "DROP"]);
    for namespace in [&source, &target] {
        ns(
            namespace,
            "ip6tables",
            &["-I", "INPUT", "1", "-p", "ipv6-icmp", "-j", "ACCEPT"],
        );
    }
    ns(
        &source,
        "iptables",
        &[
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            "10.210.1.0/24",
            "!",
            "-o",
            "ployz",
            "-j",
            "MASQUERADE",
        ],
    );

    apply_in_namespace(&source, "10.210.1.0/24", "fdcc::1");
    apply_in_namespace(&target, "10.210.2.0/24", "fdcc::2");

    let mut dns_udp = datagram_server(&target, "10.210.2.1", 53);
    send_datagram(&container, "10.210.2.2", "10.210.2.1", 53);
    assert_eq!(dns_udp.peer(), "10.210.2.2");

    let mut dns_tcp = stream_server(&target, "10.210.2.1", 53);
    connect(&container, "10.210.2.2", "10.210.2.1", 53);
    assert_eq!(dns_tcp.peer(), "10.210.2.2");

    let mut mesh = datagram_server(&container, "10.210.2.2", 40101);
    send_datagram(&source, "10.210.1.2", "10.210.2.2", 40101);
    assert_eq!(mesh.peer(), "10.210.1.2");

    let mut external = datagram_server(&internet, "198.51.100.2", 40102);
    send_datagram(&source, "10.210.1.2", "198.51.100.2", 40102);
    assert_eq!(external.peer(), "198.51.100.1");

    let _gateway_api = stream_server(&target, "10.210.2.1", MACHINE_API_PORT);
    assert_connection_denied(&source, "10.210.1.2", "10.210.2.1", MACHINE_API_PORT);

    let mut management_api = stream_server(&target, "fdcc::2", MACHINE_API_PORT);
    connect(&source, "fdcc::1", "fdcc::2", MACHINE_API_PORT);
    assert_eq!(management_api.peer(), "fdcc::1");

    let mut gossip = datagram_server(&target, "fdcc::2", CORROSION_GOSSIP_PORT);
    send_datagram(&source, "fdcc::1", "fdcc::2", CORROSION_GOSSIP_PORT);
    assert_eq!(gossip.peer(), "fdcc::1");

    let mut local_ingest = stream_server(&target, "fdcc::2", UNREGISTRY_PORT);
    connect(&target, "fdcc::2", "fdcc::2", UNREGISTRY_PORT);
    assert_eq!(local_ingest.peer(), "fdcc::2");

    let mut remote_ingest = stream_server(&target, "fdcc::2", UNREGISTRY_PORT);
    connect(&source, "fdcc::1", "fdcc::2", UNREGISTRY_PORT);
    assert_eq!(remote_ingest.peer(), "fdcc::1");

    ns(&target, "ip6tables", &["-P", "INPUT", "ACCEPT"]);
    let mut unrestricted = stream_server(&target, "fdcc::2", 51501);
    connect(&container, "fd00::2", "fdcc::2", 51501);
    assert_eq!(unrestricted.peer(), "fd00::2");

    let _ingest = stream_server(&target, "fdcc::2", UNREGISTRY_PORT);
    assert_connection_denied(&container, "fd00::2", "fdcc::2", UNREGISTRY_PORT);

    let _management_api = stream_server(&target, "fdcc::2", MACHINE_API_PORT);
    assert_connection_denied(&container, "fd00::2", "fdcc::2", MACHINE_API_PORT);

    let mut gossip = datagram_server(&target, "fdcc::2", CORROSION_GOSSIP_PORT);
    send_datagram(&container, "fd00::2", "fdcc::2", CORROSION_GOSSIP_PORT);
    assert_eq!(
        gossip.peer_within(1),
        None,
        "gossip arrived from outside the mesh"
    );
}

#[test]
#[ignore = "requires passwordless sudo and Linux network namespaces"]
fn build_steps_reach_the_internet_and_dns_but_never_the_host_or_the_lan() {
    sudo(&["-n", "true"]);
    let suffix = std::process::id();
    let host = format!("p35-host-{suffix}");
    let step = format!("p35-step-{suffix}");
    let service = format!("p35-svc-{suffix}");
    let lan = format!("p35-lan-{suffix}");
    let _namespaces = Namespaces::new([&host, &step, &service, &lan]);
    sudo(&[
        "ip",
        "link",
        "add",
        "host-build",
        "type",
        "veth",
        "peer",
        "name",
        "step-if",
    ]);
    sudo(&["ip", "link", "set", "host-build", "netns", &host]);
    sudo(&["ip", "link", "set", "step-if", "netns", &step]);
    sudo(&[
        "ip", "link", "add", "host-svc", "type", "veth", "peer", "name", "svc-if",
    ]);
    sudo(&["ip", "link", "set", "host-svc", "netns", &host]);
    sudo(&["ip", "link", "set", "svc-if", "netns", &service]);
    sudo(&[
        "ip", "link", "add", "host-lan", "type", "veth", "peer", "name", "lan-if",
    ]);
    sudo(&["ip", "link", "set", "host-lan", "netns", &host]);
    sudo(&["ip", "link", "set", "lan-if", "netns", &lan]);
    for namespace in [&host, &step, &service, &lan] {
        ns(namespace, "ip", &["link", "set", "lo", "up"]);
    }
    ns(&host, "ip", &["link", "set", "host-svc", "name", "ployz"]);
    configure_link(&host, "ployz", "10.210.1.1/24");
    configure_link(&service, "svc-if", "10.210.1.2/24");
    ns(
        &service,
        "ip",
        &["route", "add", "default", "via", "10.210.1.1"],
    );
    // Behind the host sit its LAN, a metadata service and the internet.
    configure_link(&host, "host-lan", "192.168.77.1/24");
    configure_link(&lan, "lan-if", "192.168.77.2/24");
    for address in [METADATA, INTERNET] {
        ns(
            &lan,
            "ip",
            &["address", "add", &format!("{address}/32"), "dev", "lan-if"],
        );
        ns(
            &host,
            "ip",
            &["route", "add", &format!("{address}/32"), "via", LAN],
        );
    }
    ns(
        &lan,
        "ip",
        &["route", "add", "default", "via", "192.168.77.1"],
    );
    ns(&host, "sysctl", &["-q", "-w", "net.ipv4.ip_forward=1"]);
    ns(
        &host,
        "ip",
        &["link", "set", "host-build", "name", "ployz-build0"],
    );
    configure_link(&host, "ployz-build0", "10.10.0.1/24");
    configure_link(&step, "step-if", "10.10.0.2/24");
    ns(
        &step,
        "ip",
        &["route", "add", "default", "via", "10.10.0.1"],
    );
    ns(&host, "iptables", &["-N", "DOCKER-USER"]);
    ns(
        &host,
        "iptables",
        &["-I", "FORWARD", "1", "-j", "DOCKER-USER"],
    );
    // BuildKit's CNI accepts its bridge's traffic ahead of DOCKER-USER.
    ns(
        &host,
        "iptables",
        &["-I", "FORWARD", "1", "-s", "10.10.0.2/32", "-j", "ACCEPT"],
    );

    let mut before = stream_server(&host, "10.10.0.1", 51503);
    connect(&step, "10.10.0.2", "10.10.0.1", 51503);
    assert_eq!(before.peer(), "10.10.0.2");
    let mut before = stream_server(&service, "10.210.1.2", 51503);
    connect(&step, "10.10.0.2", "10.210.1.2", 51503);
    assert_eq!(before.peer(), "10.10.0.2");
    for address in [LAN, METADATA] {
        let mut before = stream_server(&lan, address, 51503);
        connect(&step, "10.10.0.2", address, 51503);
        assert_eq!(before.peer(), "10.10.0.2");
    }

    // A port published on the host's public address and one on its LAN address.
    ns(
        &host,
        "ip",
        &["address", "add", &format!("{PUBLIC}/32"), "dev", "lo"],
    );
    for address in [PUBLIC, "192.168.77.1"] {
        ns(
            &host,
            "iptables",
            &[
                "-t",
                "nat",
                "-A",
                "PREROUTING",
                "-d",
                address,
                "-p",
                "tcp",
                "--dport",
                "51504",
                "-j",
                "DNAT",
                "--to-destination",
                "10.210.1.2:51505",
            ],
        );
    }
    // Rules an earlier daemon hooked; one of them dropped link-local DNS.
    let loose = [
        "-i ployz-build+ -o ployz -m conntrack ! --ctstate DNAT -j DROP",
        "-i ployz-build+ -o ployz-wg -j DROP",
        "-i ployz-build+ -d 169.254.0.0/16 -j DROP",
    ];
    for rule in loose {
        let mut args = vec!["-t", "mangle", "-A", "FORWARD"];
        args.extend(rule.split(' '));
        ns(&host, "iptables", &args);
    }

    apply_in_namespace(&host, "10.210.1.0/24", "fdcc::1");
    assert_eq!(
        rules(&host, "mangle", "FORWARD"),
        ["-A FORWARD -i ployz-build+ -j PLOYZ-BUILD-FORWARD"],
        "a loose rule survived"
    );
    for address in ["10.10.0.1", "10.210.1.1"] {
        let _server = stream_server(&host, address, 51503);
        assert_connection_denied(&step, "10.10.0.2", address, 51503);
    }
    let _server = stream_server(&service, "10.210.1.2", 51503);
    assert_connection_denied(&step, "10.10.0.2", "10.210.1.2", 51503);
    for address in [LAN, METADATA] {
        let _server = stream_server(&lan, address, 51503);
        assert_connection_denied(&step, "10.10.0.2", address, 51503);
    }

    let mut resolver = datagram_server(&lan, LAN, 53);
    send_datagram(&step, "10.10.0.2", LAN, 53);
    assert_eq!(
        resolver.peer_within(3).as_deref(),
        Some("10.10.0.2"),
        "DNS on the LAN was dropped"
    );
    let mut resolver = datagram_server(&lan, METADATA, 53);
    send_datagram(&step, "10.10.0.2", METADATA, 53);
    assert_eq!(
        resolver.peer_within(3).as_deref(),
        Some("10.10.0.2"),
        "DNS on link-local was dropped"
    );
    let mut service_dns = datagram_server(&service, "10.210.1.2", 53);
    send_datagram(&step, "10.10.0.2", "10.210.1.2", 53);
    assert_eq!(
        service_dns.peer_within(3),
        None,
        "DNS reached a service on the overlay"
    );
    let mut internet = stream_server(&lan, INTERNET, 51503);
    connect(&step, "10.10.0.2", INTERNET, 51503);
    assert_eq!(internet.peer(), "10.10.0.2");

    let mut published = stream_server(&service, "10.210.1.2", 51505);
    connect(&step, "10.10.0.2", PUBLIC, 51504);
    assert_eq!(published.peer(), "10.10.0.2");
    let _published = stream_server(&service, "10.210.1.2", 51505);
    assert_connection_denied(&step, "10.10.0.2", "192.168.77.1", 51504);
}

#[test]
#[ignore = "requires passwordless sudo, cgroup v2 and Linux network namespaces"]
fn the_build_worker_reaches_the_image_servers_but_not_the_host_or_the_mesh() {
    const CHILD: &str = "PLOYZ_FENCE_CGROUP";
    if let Ok(cgroup) = env::var(CHILD) {
        fence_builder(&ployz_build::Fence {
            slot: 0,
            cgroup: ployz_build::CgroupPath::new(&cgroup).unwrap(),
            image_ports: vec![51505],
        })
        .unwrap();
        return;
    }

    sudo(&["-n", "true"]);
    let suffix = std::process::id();
    let host = format!("p35-fhost-{suffix}");
    let peer = format!("p35-fpeer-{suffix}");
    let _namespaces = Namespaces::new([&host, &peer]);
    let cgroup = Cgroup::new(&format!("/p35-fence-{suffix}"));
    sudo(&[
        "ip", "link", "add", "fence-wg", "type", "veth", "peer", "name", "fpeer-wg",
    ]);
    sudo(&["ip", "link", "set", "fence-wg", "netns", &host]);
    sudo(&["ip", "link", "set", "fpeer-wg", "netns", &peer]);
    ns(
        &host,
        "ip",
        &["link", "set", "fence-wg", "name", "ployz-wg"],
    );
    for (namespace, interface, address) in [
        (&host, "ployz-wg", "fdcc::1/64"),
        (&peer, "fpeer-wg", "fdcc::2/64"),
    ] {
        ns(namespace, "ip", &["link", "set", "lo", "up"]);
        ns(
            namespace,
            "ip",
            &["-6", "address", "add", address, "dev", interface, "nodad"],
        );
        ns(namespace, "ip", &["link", "set", interface, "up"]);
    }
    ns(
        &host,
        "ip",
        &["address", "add", "127.0.0.53/8", "dev", "lo"],
    );

    let status = Command::new("sudo")
        .args(["-n", "ip", "netns", "exec", &host, "env"])
        .arg(format!("{CHILD}={}", cgroup.0))
        .arg(env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "the_build_worker_reaches_the_image_servers_but_not_the_host_or_the_mesh",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "fence child failed in {host}");
    assert_eq!(
        rules(&host, "mangle", "FORWARD"),
        ["-A FORWARD -i ployz-build+ -j PLOYZ-BUILD-FORWARD"],
        "the fence left build steps unfenced"
    );

    let mut image_server = stream_server(&peer, "fdcc::2", UNREGISTRY_PORT);
    cgroup.connect(&host, "fdcc::1", "fdcc::2", UNREGISTRY_PORT);
    assert_eq!(image_server.peer(), "fdcc::1");
    let mut image_context = stream_server(&host, "127.0.0.1", 51505);
    cgroup.connect(&host, "127.0.0.1", "127.0.0.1", 51505);
    assert_eq!(image_context.peer(), "127.0.0.1");
    let mut resolver = stream_server(&host, "127.0.0.53", 53);
    cgroup.connect(&host, "127.0.0.1", "127.0.0.53", 53);
    assert_eq!(resolver.peer(), "127.0.0.1");

    for (namespace, address, port) in [
        (&host, "127.0.0.1", 51504),
        (&host, "fdcc::1", MACHINE_API_PORT),
        (&peer, "fdcc::2", MACHINE_API_PORT),
    ] {
        let source = if address.contains(':') {
            "fdcc::1"
        } else {
            "127.0.0.1"
        };
        let mut server = stream_server(namespace, address, port);
        assert!(
            !cgroup.connects(&host, source, address, port),
            "the fenced worker reached {address}:{port}"
        );
        connect(&host, source, address, port);
        assert_eq!(
            server.peer(),
            source,
            "the fence caught a process outside it"
        );
    }
}

/// A cgroup v2 directory standing in for the BuildKit container's.
struct Cgroup(String);

impl Cgroup {
    fn new(path: &str) -> Self {
        sudo(&["mkdir", &format!("/sys/fs/cgroup{path}")]);
        Self(path.to_owned())
    }

    /// Joins the cgroup before entering the namespace: `ip netns exec`
    /// remounts /sys, which hides the cgroup tree.
    fn connects(&self, namespace: &str, source: &str, target: &str, port: u16) -> bool {
        Command::new("sudo")
            .args([
                "-n",
                "sh",
                "-c",
                "echo $$ > \"/sys/fs/cgroup$1/cgroup.procs\" && shift && exec ip netns exec \"$@\"",
                "sh",
                &self.0,
                namespace,
                "python3",
                "-c",
                "import socket,sys; a,b,p=sys.argv[1:]; s=socket.socket(socket.AF_INET6 if ':' in a else socket.AF_INET); s.bind((a,0)); s.settimeout(1); s.connect((b,int(p)))",
                source,
                target,
            ])
            .arg(port.to_string())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn connect(&self, namespace: &str, source: &str, target: &str, port: u16) {
        assert!(
            self.connects(namespace, source, target, port),
            "the fenced worker could not reach {target}:{port}"
        );
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup{}", self.0)])
            .status();
    }
}

#[test]
#[ignore = "requires passwordless sudo and Linux network namespaces"]
fn reapplying_the_firewall_never_drops_admitted_traffic() {
    sudo(&["-n", "true"]);
    let namespace = format!("p35-reapply-{}", std::process::id());
    let _namespaces = Namespaces::new([&namespace]);
    ns(&namespace, "ip", &["link", "set", "lo", "up"]);
    ns(&namespace, "iptables", &["-N", "DOCKER-USER"]);
    ns(&namespace, "iptables", &["-P", "INPUT", "DROP"]);
    ns(&namespace, "ip6tables", &["-P", "INPUT", "DROP"]);
    apply_in_namespace(&namespace, "10.210.1.0/24", "fdcc::1");

    let stop = env::temp_dir().join(format!("{namespace}.stop"));
    let mut probe = Server::start(
        &namespace,
        &format!(
            "import os,socket,sys,threading,time; a,p=sys.argv[1:]; r=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); r.bind((a,int(p))); r.settimeout(.1); got=set(); done=threading.Event()\n\
def receive():\n while not done.is_set():\n  try: got.add(int(r.recv(16)))\n  except socket.timeout: pass\n\
t=threading.Thread(target=receive); t.start(); print('READY',flush=True); sent=0\n\
while not os.path.exists({stop:?}):\n s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.sendto(str(sent).encode(),(a,int(p))); s.close(); sent+=1; time.sleep(.001)\n\
time.sleep(.3); done.set(); t.join(); print(f'{{len(set(range(sent))-got)}} of {{sent}}',flush=True)"
        ),
        "127.0.0.1",
        51820,
    );
    for _ in 0..4 {
        apply_in_namespace(&namespace, "10.210.1.0/24", "fdcc::1");
    }
    ns(
        &namespace,
        "iptables",
        &["-I", "INPUT", "1", "-p", "icmp", "-j", "ACCEPT"],
    );
    ns(
        &namespace,
        "iptables",
        &["-t", "nat", "-I", "POSTROUTING", "1", "-j", "RETURN"],
    );
    apply_in_namespace(&namespace, "10.210.1.0/24", "fdcc::1");
    std::fs::write(&stop, "").unwrap();
    let lost = probe.peer();
    std::fs::remove_file(&stop).unwrap();
    assert!(
        lost.starts_with("0 of "),
        "lost {lost} datagrams to the mesh port"
    );

    assert_eq!(
        rules(&namespace, "filter", "INPUT"),
        ["-A INPUT -j PLOYZ-INPUT", "-A INPUT -p icmp -j ACCEPT"]
    );
    assert_eq!(
        rules(&namespace, "nat", "POSTROUTING"),
        [
            "-A POSTROUTING -s 10.210.1.0/24 -o ployz-wg -j RETURN",
            "-A POSTROUTING -j RETURN"
        ]
    );
    let jumped = jump_packets(&namespace);
    apply_in_namespace(&namespace, "10.210.1.0/24", "fdcc::1");
    assert_eq!(
        jump_packets(&namespace),
        jumped,
        "a converged INPUT jump was replaced"
    );
}

fn rules(namespace: &str, table: &str, chain: &str) -> Vec<String> {
    let output = Command::new("sudo")
        .args([
            "-n", "ip", "netns", "exec", namespace, "iptables", "-t", table, "-S", chain,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("-A "))
        .map(str::to_owned)
        .collect()
}

fn jump_packets(namespace: &str) -> String {
    let output = Command::new("sudo")
        .args([
            "-n", "ip", "netns", "exec", namespace, "iptables", "-L", "INPUT", "-v", "-x", "-n",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let listing = String::from_utf8(output.stdout).unwrap();
    let jump = listing
        .lines()
        .find(|line| line.contains("PLOYZ-INPUT"))
        .unwrap();
    let packets = jump.split_whitespace().next().unwrap().to_owned();
    assert_ne!(packets, "0", "no traffic crossed the INPUT jump");
    packets
}

fn apply_in_namespace(namespace: &str, subnet: &str, management_address: &str) {
    let executable = env::current_exe().unwrap();
    let status = Command::new("sudo")
        .args(["-n", "ip", "netns", "exec", namespace, "env"])
        .arg(format!("PLOYZ_FIREWALL_SUBNET={subnet}"))
        .arg(format!(
            "PLOYZ_FIREWALL_MANAGEMENT_ADDRESS={management_address}"
        ))
        .arg(executable)
        .args(["--ignored", "--exact", TEST_NAME])
        .status()
        .unwrap();
    assert!(status.success(), "firewall child failed in {namespace}");
}

fn configure_link(namespace: &str, interface: &str, address: &str) {
    ns(
        namespace,
        "ip",
        &["address", "add", address, "dev", interface],
    );
    ns(namespace, "ip", &["link", "set", interface, "up"]);
}

fn send_datagram(namespace: &str, source: &str, target: &str, port: u16) {
    python(
        namespace,
        "import socket,sys; a,b,p=sys.argv[1:]; f=socket.AF_INET6 if ':' in a else socket.AF_INET; s=socket.socket(f,socket.SOCK_DGRAM); s.bind((a,0)); s.sendto(b'x',(b,int(p)))",
        &[source, target, &port.to_string()],
    );
}

fn connect(namespace: &str, source: &str, target: &str, port: u16) {
    python(
        namespace,
        "import socket,sys; a,b,p=sys.argv[1:]; f=socket.AF_INET6 if ':' in a else socket.AF_INET; s=socket.socket(f); s.bind((a,0)); s.settimeout(3); s.connect((b,int(p)))",
        &[source, target, &port.to_string()],
    );
}

fn assert_connection_denied(namespace: &str, source: &str, target: &str, port: u16) {
    let status = Command::new("sudo")
        .args([
            "-n",
            "ip",
            "netns",
            "exec",
            namespace,
            "python3",
            "-c",
            "import socket,sys; a,b,p=sys.argv[1:]; s=socket.socket(socket.AF_INET6 if ':' in a else socket.AF_INET); s.bind((a,0)); s.settimeout(1); s.connect((b,int(p)))",
            source,
            target,
        ])
        .arg(port.to_string())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "connection unexpectedly succeeded");
}

fn datagram_server(namespace: &str, address: &str, port: u16) -> Server {
    Server::start(
        namespace,
        "import socket,sys; a,p=sys.argv[1:]; f=socket.AF_INET6 if ':' in a else socket.AF_INET; s=socket.socket(f,socket.SOCK_DGRAM); s.bind((a,int(p))); print('READY',flush=True); _,peer=s.recvfrom(1); print(peer[0],flush=True)",
        address,
        port,
    )
}

fn stream_server(namespace: &str, address: &str, port: u16) -> Server {
    Server::start(
        namespace,
        "import socket,sys; a,p=sys.argv[1:]; f=socket.AF_INET6 if ':' in a else socket.AF_INET; s=socket.socket(f); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind((a,int(p))); s.listen(1); print('READY',flush=True); _,peer=s.accept(); print(peer[0],flush=True)",
        address,
        port,
    )
}

struct Server {
    child: Child,
    output: BufReader<ChildStdout>,
}

impl Server {
    fn start(namespace: &str, script: &str, address: &str, port: u16) -> Self {
        let mut child = Command::new("sudo")
            .args([
                "-n", "ip", "netns", "exec", namespace, "python3", "-c", script, address,
            ])
            .arg(port.to_string())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "READY");
        Self { child, output }
    }

    fn peer_within(&mut self, seconds: u64) -> Option<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return Some(self.peer());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }

    fn peer(&mut self) -> String {
        let mut peer = String::new();
        self.output.read_line(&mut peer).unwrap();
        assert!(self.child.wait().unwrap().success());
        peer.trim().to_owned()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Namespaces(Vec<String>);

impl Namespaces {
    fn new<'a>(names: impl IntoIterator<Item = &'a String>) -> Self {
        let names = names.into_iter().cloned().collect::<Vec<_>>();
        for name in &names {
            sudo(&["ip", "netns", "add", name]);
        }
        Self(names)
    }
}

impl Drop for Namespaces {
    fn drop(&mut self) {
        for name in &self.0 {
            // Server::drop kills sudo, not its root python child; a server whose
            // connection was denied would outlive the test and hold its stdout.
            let _ = Command::new("sudo")
                .args([
                    "-n",
                    "sh",
                    "-c",
                    "ip netns pids \"$1\" | xargs -r kill; ip netns delete \"$1\"",
                    "sh",
                    name,
                ])
                .status();
        }
    }
}

fn python(namespace: &str, script: &str, args: &[&str]) {
    let mut command = Command::new("sudo");
    command.args([
        "-n", "ip", "netns", "exec", namespace, "python3", "-c", script,
    ]);
    command.args(args);
    let success = command.status().unwrap().success();
    assert!(success, "python failed in {namespace} with {args:?}");
}

fn ns(namespace: &str, program: &str, args: &[&str]) {
    let mut command = Command::new("sudo");
    command.args(["-n", "ip", "netns", "exec", namespace, program]);
    command.args(args);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} failed in {namespace}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sudo(args: &[&str]) {
    let output = Command::new("sudo").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "sudo {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
