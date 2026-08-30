use std::{
    net::{IpAddr, Ipv6Addr},
    process::{self, Command, Output},
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    CORROSION_API_PORT, CreateDomainRecordsRequest, DnsRecord, DnsRecordType, MachineUpdate,
    PublicIpUpdate, op,
};
use ployz_testkit::{Cluster, ClusterPlan};
use reqwest::{Client as HttpClient, redirect::Policy};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Clone, Debug)]
struct CapturedRequest {
    head: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct FakeHostedService {
    state: Arc<Mutex<FakeHostedState>>,
}

#[derive(Default)]
struct FakeHostedState {
    requests: Vec<CapturedRequest>,
    reject_record_type: Option<String>,
}

impl FakeHostedService {
    fn requests(&self) -> Vec<CapturedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    fn reject_record_type(&self, record_type: &str) {
        self.state.lock().unwrap().reject_record_type = Some(record_type.into());
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn hosted_dns_reservation_and_reachable_ingress_records_survive_real_cluster_boundaries() {
    let plan = ClusterPlan::new(&format!("l3-hosted-dns-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    for index in 0..machines.len() {
        poison_production_dns(&cluster, index);
    }

    let first_ip = outer_ipv4(&cluster, 0);
    let second_ip = outer_ipv4(&cluster, 1);
    let mapped_second = IpAddr::V6(second_ip.to_ipv6_mapped());
    cluster
        .update_machine(
            0,
            machines[0].id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Set(IpAddr::V4(first_ip)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    cluster
        .update_machine(
            0,
            machines[1].id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Set(mapped_second),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_exact_ingress(IpAddr::V4(first_ip), machines[0].id.as_str().as_bytes()).await;
    wait_exact_ingress(mapped_second, machines[1].id.as_str().as_bytes()).await;

    let (port, hosted) = fake_hosted_service().await;
    let gateway = cluster
        .machine_shell(0, "ip route show default | awk '{print $3; exit}'")
        .unwrap();
    let endpoint = format!("http://{}:{port}", gateway.trim());
    cli(
        &direct,
        &["dns", "reserve", "--endpoint", endpoint.as_str()],
    );

    let stored = cluster
        .machine_shell(
            0,
            &format!(r#"token=$(cat /var/lib/ployz/corrosion/.api-token); curl --fail --silent --show-error --http2-prior-knowledge -H "Authorization: Bearer $token" -H 'Content-Type: application/json' --data-binary '{{"query":"SELECT value FROM cluster WHERE key = ?","params":["hosted_dns"]}}' http://127.0.0.1:{CORROSION_API_PORT}/v1/queries"#),
        )
        .unwrap();
    assert!(stored.contains("raw-token"), "stored reservation: {stored}");
    assert!(stored.contains("opaque.ployz.example"));

    let initial = hosted.requests();
    assert_eq!(domain_requests(&initial), 1);
    let reservation_request = initial
        .iter()
        .find(|request| request.head.starts_with("POST /domains HTTP/1.1"))
        .unwrap();
    assert!(reservation_request.body.is_empty());
    assert!(
        !reservation_request
            .head
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    assert_eq!(record_requests(&initial).len(), 2);
    assert_record(record_request(&initial, 0), "A", &[first_ip.to_string()]);
    assert_record(
        record_request(&initial, 1),
        "AAAA",
        &[mapped_second.to_string()],
    );

    let second_direct = cluster.api_address(1).unwrap();
    assert_eq!(
        cli(&second_direct, &["dns", "show"]).trim(),
        "opaque.ployz.example"
    );
    cluster.restart(1).unwrap();
    assert_eq!(
        wait_cli_success(&second_direct, &["dns", "show"])
            .await
            .trim(),
        "opaque.ployz.example"
    );
    let duplicate = run_cli(
        &second_direct,
        &["dns", "reserve", "--endpoint", endpoint.as_str()],
    );
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already reserved"));
    assert_eq!(domain_requests(&hosted.requests()), 1);

    cluster
        .update_machine(
            0,
            machines[1].id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Set("2001:db8::2".parse().unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    let partial = hosted.requests();
    assert_eq!(record_requests(&partial).len(), 3);
    assert_record(record_request(&partial, 2), "A", &[first_ip.to_string()]);

    cluster
        .update_machine(
            0,
            machines[0].id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Set("192.0.2.1".parse().unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let before_empty = hosted.requests().len();
    let empty = run_cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no publicly reachable"));
    assert_eq!(hosted.requests().len(), before_empty);
    assert_eq!(
        cli(&direct, &["dns", "show"]).trim(),
        "opaque.ployz.example"
    );

    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    hosted.reject_record_type("AAAA");
    let error = client
        .call::<op::CreateDomainRecords>(
            CreateDomainRecordsRequest {
                records: vec![
                    DnsRecord {
                        name: "*".into(),
                        record_type: DnsRecordType::A,
                        values: vec![first_ip.to_string()],
                    },
                    DnsRecord {
                        name: "*".into(),
                        record_type: DnsRecordType::Aaaa,
                        values: vec![Ipv6Addr::LOCALHOST.to_string()],
                    },
                ],
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTP 500"));
    let after_rejection = hosted.requests();
    assert_eq!(record_requests(&after_rejection).len(), 5);
    assert_record(
        record_request(&after_rejection, 3),
        "A",
        &[first_ip.to_string()],
    );
    assert_record(
        record_request(&after_rejection, 4),
        "AAAA",
        &[Ipv6Addr::LOCALHOST.to_string()],
    );
    assert!(after_rejection.iter().all(|request| {
        request.head.starts_with("POST ")
            && !request
                .head
                .to_ascii_lowercase()
                .contains("dns.uncloud.run")
    }));

    let before_release = after_rejection.len();
    assert_eq!(
        cli(&direct, &["dns", "release"]).trim(),
        "Released Cluster domain: opaque.ployz.example"
    );
    let missing = run_cli(&direct, &["dns", "show"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("was not found"));
    let after_release = hosted.requests();
    assert_eq!(after_release.len(), before_release + 1);
    let purge = after_release.last().unwrap();
    assert!(
        purge
            .head
            .starts_with("POST /domains/opaque.ployz.example/purgerecords HTTP/1.1\r\n")
    );
    assert!(
        purge
            .head
            .to_ascii_lowercase()
            .contains("authorization: bearer raw-token\r\n")
    );
    assert!(purge.body.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn hosted_dns_wildcard_follows_machine_membership() {
    let plan = ClusterPlan::new(&format!("l3-dns-member-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_three().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    for index in 0..machines.len() {
        poison_production_dns(&cluster, index);
    }

    let first_ip = outer_ipv4(&cluster, 0);
    let second_ip = outer_ipv4(&cluster, 1);
    let third_ip = outer_ipv4(&cluster, 2);
    for (machine, address) in machines.iter().zip([first_ip, second_ip, third_ip]) {
        cluster
            .update_machine(
                0,
                machine.id.as_str(),
                MachineUpdate {
                    public_ip: PublicIpUpdate::Set(IpAddr::V4(address)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_exact_ingress(IpAddr::V4(first_ip), machines[0].id.as_str().as_bytes()).await;
    wait_exact_ingress(IpAddr::V4(second_ip), machines[1].id.as_str().as_bytes()).await;
    wait_exact_ingress(IpAddr::V4(third_ip), machines[2].id.as_str().as_bytes()).await;

    let (port, hosted) = fake_hosted_service().await;
    let gateway = cluster
        .machine_shell(0, "ip route show default | awk '{print $3; exit}'")
        .unwrap();
    let endpoint = format!("http://{}:{port}", gateway.trim());
    cli(
        &direct,
        &["dns", "reserve", "--endpoint", endpoint.as_str()],
    );
    assert_eq!(
        authoritative_wildcard_a(&hosted.requests()),
        vec![
            first_ip.to_string(),
            second_ip.to_string(),
            third_ip.to_string()
        ],
    );

    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    // Same refresh `machine add` runs after membership is saved, including when
    // the add-time Ingress Proxy Deploy failed or was skipped.
    let before_add_refresh = record_requests(&hosted.requests()).len();
    ployz::dns::update_records_if_reserved(&mut client)
        .await
        .unwrap();
    let after_add_refresh = hosted.requests();
    assert_eq!(
        record_requests(&after_add_refresh).len(),
        before_add_refresh + 1
    );
    assert_eq!(
        authoritative_wildcard_a(&after_add_refresh),
        vec![
            first_ip.to_string(),
            second_ip.to_string(),
            third_ip.to_string()
        ],
        "last published * A at the authority must include a Machine that passes the Ingress Proxy probe"
    );

    let before_remove = record_requests(&after_add_refresh).len();
    cli(
        &direct,
        &["machine", "rm", machines[1].id.as_str(), "--yes"],
    );
    let after_remove = hosted.requests();
    assert_eq!(record_requests(&after_remove).len(), before_remove + 1);
    assert_eq!(
        authoritative_wildcard_a(&after_remove),
        vec![first_ip.to_string(), third_ip.to_string()],
        "last published * A at the authority must not include the removed Machine"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn hosted_dns_wildcard_omits_non_ingress_members_after_machine_removal() {
    let plan = ClusterPlan::new(&format!("l3-dns-mixed-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_three().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    for index in 0..machines.len() {
        poison_production_dns(&cluster, index);
    }

    let first_ip = outer_ipv4(&cluster, 0);
    let second_ip = outer_ipv4(&cluster, 1);
    let third_ip = outer_ipv4(&cluster, 2);
    for (machine, address) in machines.iter().zip([first_ip, second_ip, third_ip]) {
        cluster
            .update_machine(
                0,
                machine.id.as_str(),
                MachineUpdate {
                    public_ip: PublicIpUpdate::Set(IpAddr::V4(address)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_exact_ingress(IpAddr::V4(first_ip), machines[0].id.as_str().as_bytes()).await;
    wait_exact_ingress(IpAddr::V4(second_ip), machines[1].id.as_str().as_bytes()).await;
    wait_exact_ingress(IpAddr::V4(third_ip), machines[2].id.as_str().as_bytes()).await;

    cluster
        .machine_shell(
            2,
            "id=$(docker ps -aq --filter label=ployz.service.name=ingress | head -n1); [ -n \"$id\" ] && docker rm -f \"$id\"",
        )
        .unwrap();
    wait_ingress_absent(IpAddr::V4(third_ip)).await;

    let (port, hosted) = fake_hosted_service().await;
    let gateway = cluster
        .machine_shell(0, "ip route show default | awk '{print $3; exit}'")
        .unwrap();
    let endpoint = format!("http://{}:{port}", gateway.trim());
    cli(
        &direct,
        &["dns", "reserve", "--endpoint", endpoint.as_str()],
    );
    assert_eq!(
        authoritative_wildcard_a(&hosted.requests()),
        vec![first_ip.to_string(), second_ip.to_string()],
        "ordinary refresh must omit a public Machine that does not serve ingress"
    );

    let before_remove = record_requests(&hosted.requests()).len();
    cli(
        &direct,
        &["machine", "rm", machines[1].id.as_str(), "--yes"],
    );
    let after_partial = hosted.requests();
    assert_eq!(record_requests(&after_partial).len(), before_remove + 1);
    assert_eq!(
        authoritative_wildcard_a(&after_partial),
        vec![first_ip.to_string()],
        "removing one ingress Machine must not publish a remaining non-ingress member"
    );

    let third_direct = cluster.api_address(2).unwrap();
    cli(
        &third_direct,
        &["machine", "rm", machines[0].id.as_str(), "--yes"],
    );
    assert_eq!(
        authoritative_wildcard_a(&hosted.requests()),
        vec![first_ip.to_string()],
        "removing the last ingress Machine must keep last-known-good records, not the non-ingress member"
    );
}

fn poison_production_dns(cluster: &Cluster, index: usize) {
    cluster
        .machine_shell(
            index,
            "printf '127.0.0.1 dns.uncloud.run\\n::1 dns.uncloud.run\\n' >> /etc/hosts && test \"$(getent ahostsv4 dns.uncloud.run | awk 'NR == 1 { print $1 }')\" = 127.0.0.1 && test \"$(getent ahostsv6 dns.uncloud.run | awk 'NR == 1 { print $1 }')\" = ::1",
        )
        .unwrap();
}

fn outer_ipv4(cluster: &Cluster, index: usize) -> std::net::Ipv4Addr {
    cluster
        .machine_shell(index, "hostname -i | awk '{print $1}'")
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

async fn wait_exact_ingress(address: IpAddr, expected: &[u8]) {
    let http = HttpClient::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(response) = http
                .get(format!(
                    "http://{}{}",
                    std::net::SocketAddr::new(address, 80),
                    ployz_core::INGRESS_VERIFY_PATH
                ))
                .send()
                .await
                && response.status().as_u16() == 200
                && response.bytes().await.ok().as_deref() == Some(expected)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("Ingress Proxy verification endpoint did not become reachable");
}

async fn wait_ingress_absent(address: IpAddr) {
    let http = HttpClient::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let reachable = http
                .get(format!(
                    "http://{}{}",
                    std::net::SocketAddr::new(address, 80),
                    ployz_core::INGRESS_VERIFY_PATH
                ))
                .send()
                .await
                .ok()
                .is_some_and(|response| response.status().as_u16() == 200);
            if !reachable {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("Ingress Proxy verification endpoint stayed reachable");
}

fn cli(direct: &str, args: &[&str]) -> String {
    let output = run_cli(direct, args);
    assert!(
        output.status.success(),
        "ployz {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_cli(direct: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            direct,
            "--ployz-config",
            "/missing-ployz-test-config",
        ])
        .args(args)
        .env("PLOYZ_HEALTH_MONITOR_PERIOD", "0s")
        .output()
        .unwrap()
}

async fn wait_cli_success(direct: &str, args: &[&str]) -> String {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let output = run_cli(direct, args);
            if output.status.success() {
                return String::from_utf8(output.stdout).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("CLI did not become ready")
}

async fn fake_hosted_service() -> (u16, FakeHostedService) {
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = FakeHostedService::default();
    let server = service.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let path = request
                .head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap()
                .to_owned();
            let (status, response) = if path == "/domains" {
                (
                    200,
                    json!({"name":"opaque.ployz.example","token":"raw-token"}),
                )
            } else if path == "/domains/opaque.ployz.example/purgerecords" {
                (202, json!({"name":"opaque.ployz.example"}))
            } else {
                assert_eq!(path, "/domains/opaque.ployz.example/records");
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let reject = {
                    let mut state = server.state.lock().unwrap();
                    if state.reject_record_type.as_deref()
                        == body.get("type").and_then(Value::as_str)
                    {
                        state.reject_record_type = None;
                        true
                    } else {
                        false
                    }
                };
                if reject {
                    (500, json!({"error":"rejected later record"}))
                } else {
                    (
                        200,
                        json!({
                            "name": body.get("name").unwrap(),
                            "type": body.get("type").unwrap(),
                            "values": body.get("values").unwrap(),
                            "fqdn": "*.opaque.ployz.example"
                        }),
                    )
                }
            };
            server.state.lock().unwrap().requests.push(request);
            let body = response.to_string();
            let reason = if status == 200 {
                "OK"
            } else {
                "Internal Server Error"
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    (port, service)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> CapturedRequest {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(chunk.get(..read).unwrap());
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8(bytes.get(..header_end).unwrap().to_vec()).unwrap();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or_default();
    while bytes.len() < header_end + content_length {
        let mut chunk = [0; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(chunk.get(..read).unwrap());
    }
    CapturedRequest {
        head,
        body: bytes
            .get(header_end..header_end + content_length)
            .unwrap()
            .to_vec(),
    }
}

fn domain_requests(requests: &[CapturedRequest]) -> usize {
    requests
        .iter()
        .filter(|request| request.head.starts_with("POST /domains HTTP/1.1"))
        .count()
}

fn record_requests(requests: &[CapturedRequest]) -> Vec<&CapturedRequest> {
    requests
        .iter()
        .filter(|request| request.head.contains("/records HTTP/1.1"))
        .collect()
}

fn record_request(requests: &[CapturedRequest], index: usize) -> &CapturedRequest {
    requests
        .iter()
        .filter(|request| request.head.contains("/records HTTP/1.1"))
        .nth(index)
        .unwrap()
}

fn authoritative_wildcard_a(requests: &[CapturedRequest]) -> Vec<String> {
    record_requests(requests)
        .into_iter()
        .rev()
        .find_map(|request| {
            let body = serde_json::from_slice::<Value>(&request.body).ok()?;
            if body.get("name")?.as_str()? != "*" || body.get("type")?.as_str()? != "A" {
                return None;
            }
            Some(
                body.get("values")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

#[test]
fn fresh_never_cached_label_reads_the_last_published_wildcard_a() {
    let older = CapturedRequest {
        head: "POST /domains/opaque.ployz.example/records HTTP/1.1".into(),
        body: br#"{"name":"*","type":"A","values":["192.0.2.1","198.51.100.1"]}"#.to_vec(),
    };
    let newer = CapturedRequest {
        head: "POST /domains/opaque.ployz.example/records HTTP/1.1".into(),
        body: br#"{"name":"*","type":"A","values":["192.0.2.1"]}"#.to_vec(),
    };
    assert_eq!(
        authoritative_wildcard_a(&[older, newer]),
        vec!["192.0.2.1".to_string()]
    );
}

fn assert_record(request: &CapturedRequest, record_type: &str, values: &[String]) {
    assert!(
        request
            .head
            .to_ascii_lowercase()
            .contains("authorization: bearer raw-token\r\n")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"name":"*","type":record_type,"values":values})
    );
}
