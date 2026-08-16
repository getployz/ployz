use ployz::compose::parse_normalized;
use ployz_core::{HttpProtocol, IngressHostname, PortPublication};

#[test]
fn http_ingress_records_hostname_intent_and_rejects_transport_ingress() {
    let assigned =
        parse_normalized("services: {app: {image: app, x-ports: ['80/http']}}", ".").unwrap();
    assert!(matches!(
        assigned.services.get("app").unwrap().ports.first(),
        Some(PortPublication::Ingress {
            hostname: IngressHostname::AssignFromClusterDomain,
            http_protocol: HttpProtocol::Http,
            ..
        })
    ));
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
