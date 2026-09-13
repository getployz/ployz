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

#[test]
fn numeric_two_field_ingress_is_always_a_range_checked_published_port() {
    let parse = |port: &str| {
        parse_normalized(
            &format!("services: {{app: {{image: app, x-ports: [{port}]}}}}"),
            ".",
        )
        .map(|project| {
            project
                .services
                .into_values()
                .next()
                .expect("test project has one service")
                .ports
                .into_iter()
                .next()
                .expect("test service has one port")
        })
    };

    for published in [1, 9000, u16::MAX] {
        assert_eq!(
            parse(&format!("{published}:80/http")).unwrap(),
            PortPublication::Ingress {
                hostname: IngressHostname::cluster_domain(),
                load_balancer_port: published.try_into().unwrap(),
                container_port: 80.try_into().unwrap(),
                http_protocol: HttpProtocol::Http,
            }
        );
    }
    for (published, expected) in [
        ("0", "published port must be non-zero"),
        ("65536", "invalid port '65536'"),
        ("900000", "invalid port '900000'"),
    ] {
        let error = parse(&format!("{published}:80/http"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected),
            "published port {published} returned {error:?}"
        );
    }
    assert_eq!(
        parse("900000:80:80/http").unwrap(),
        PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain_label("900000").unwrap(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 80.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        }
    );
    assert_eq!(
        parse("9000:80/http").unwrap(),
        parse("'9000:80/http'").unwrap()
    );
}
