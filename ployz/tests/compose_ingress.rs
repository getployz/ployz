use ployz::compose::parse_normalized;
use ployz_core::{HttpProtocol, IngressHostname, PortPublication};

#[test]
fn http_ingress_records_hostname_intent_and_rejects_transport_ingress() {
    let assigned =
        parse_normalized("services: {app: {image: app, x-ports: ['80/http']}}", ".").unwrap();
    assert_eq!(
        assigned.services.get("app").unwrap().ports.first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 80.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        })
    );
    let chosen = parse_normalized(
        "services: {app: {image: app, x-ports: ['api:80/http']}}",
        ".",
    )
    .unwrap();
    assert_eq!(
        chosen.services.get("app").unwrap().ports.first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain_label("api").unwrap(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 80.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        })
    );
    let explicit = parse_normalized(
        "services: {app: {image: app, x-ports: ['api.example.com:80/http']}}",
        ".",
    )
    .unwrap();
    assert_eq!(
        explicit.services.get("app").unwrap().ports.first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::explicit("api.example.com").unwrap(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 80.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        })
    );
    let too_long = format!("{}:80/http", "a".repeat(64));
    assert!(
        parse_normalized(
            &format!("services: {{app: {{image: app, x-ports: ['{too_long}']}}}}"),
            ".",
        )
        .unwrap_err()
        .to_string()
        .contains("Cluster Domain label")
    );
    for yaml in [
        "services: {app: {image: app, ports: ['80:80']}}",
        "services: {app: {image: app, x-ports: ['8080:80']}}",
        "services: {app: {image: app, x-ports: ['8080:80/udp']}}",
    ] {
        let error = parse_normalized(yaml, ".").unwrap_err().to_string();
        assert!(
            error.contains("host publication"),
            "{error:?} did not guide toward host publication"
        );
    }
    assert!(
        parse_normalized(
            "services: {app: {image: app, x-ports: ['EXAMPLE.COM:80/http']}}",
            ".",
        )
        .unwrap_err()
        .to_string()
        .contains("Ingress Hostname")
    );
}
