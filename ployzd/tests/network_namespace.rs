use std::{
    env,
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Command, Stdio},
};

use ployzd::network::apply_firewall_rules;

const TEST_NAME: &str = "mesh_routing_preserves_container_source_and_internet_nat";

#[test]
#[ignore = "requires passwordless sudo and Linux network namespaces"]
fn mesh_routing_preserves_container_source_and_internet_nat() {
    if let Ok(subnet) = env::var("PLOYZ_FIREWALL_SUBNET") {
        apply_firewall_rules(subnet.parse().unwrap()).unwrap();
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

    apply_in_namespace(&source, "10.210.1.0/24");
    apply_in_namespace(&target, "10.210.2.0/24");

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

    let mut container_api = stream_server(&target, "10.210.2.1", 51000);
    connect(&source, "10.210.1.2", "10.210.2.1", 51000);
    assert_eq!(container_api.peer(), "10.210.1.2");

    let mut management_api = stream_server(&target, "fdcc::2", 51000);
    connect(&source, "fdcc::1", "fdcc::2", 51000);
    assert_eq!(management_api.peer(), "fdcc::1");
}

fn apply_in_namespace(namespace: &str, subnet: &str) {
    let executable = env::current_exe().unwrap();
    let status = Command::new("sudo")
        .args(["-n", "ip", "netns", "exec", namespace, "env"])
        .arg(format!("PLOYZ_FIREWALL_SUBNET={subnet}"))
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
            let _ = Command::new("sudo")
                .args(["-n", "ip", "netns", "delete", name])
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
    assert!(command.status().unwrap().success());
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
