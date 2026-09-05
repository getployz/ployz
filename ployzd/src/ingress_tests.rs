//! Projection fixture shared by concrete renderer tests.

use super::{
    IngressEndpoint, IngressProjection, IngressPublication, IngressSite, ProjectedCertificate,
};
use crate::corrosion::{CertificateChallenge, CertificateMaterial};
use ployz_core::{
    ContainerAddress, IngressHost, Machine, MachineId, MachineName, ManagementAddress,
    QualifiedService, WireGuardPublicKey,
};
use std::{collections::BTreeMap, num::NonZeroU16};

/// Projection that exercises HTTP, HTTPS, challenge, ordered, and empty routes.
pub(crate) fn renderer_projection() -> IngressProjection {
    let machine_id = MachineId::parse("a".repeat(32)).unwrap();
    let owner = QualifiedService::parse("app/api").unwrap();
    let endpoint = |address: &str, port| IngressEndpoint {
        address: ContainerAddress(address.parse().unwrap()),
        port: NonZeroU16::new(port).unwrap(),
    };
    let publication = |http, https| {
        Some(IngressPublication {
            owner: owner.clone(),
            http,
            https,
        })
    };
    IngressProjection {
        machine: Machine {
            id: machine_id,
            name: MachineName::parse("node-a").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            management_address: ManagementAddress("fdcc::1".parse().unwrap()),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        },
        sites: vec![
            IngressSite {
                hostname: IngressHost::parse("empty.example.com").unwrap(),
                publication: publication(Some(Vec::new()), None),
                certificate: None,
            },
            IngressSite {
                hostname: IngressHost::parse("example.com").unwrap(),
                publication: publication(
                    Some(vec![
                        endpoint("10.210.1.2", 8080),
                        endpoint("10.210.2.2", 8080),
                    ]),
                    None,
                ),
                certificate: Some(ProjectedCertificate {
                    challenge: CertificateChallenge::new("token", "token.thumbprint"),
                    material: None,
                    last_error: None,
                }),
            },
            IngressSite {
                hostname: IngressHost::parse("secure.example.com").unwrap(),
                publication: publication(None, Some(vec![endpoint("10.210.1.3", 8443)])),
                certificate: Some(ProjectedCertificate {
                    challenge: None,
                    material: CertificateMaterial::new(
                        include_str!("../tests/fixtures/certificate-test-rsa.pem"),
                        include_str!("../tests/fixtures/certificate-test-rsa-key.pem"),
                    ),
                    last_error: None,
                }),
            },
        ],
        upstreams: BTreeMap::from([(
            owner,
            vec![
                ContainerAddress("10.210.1.2".parse().unwrap()),
                ContainerAddress("10.210.2.2".parse().unwrap()),
            ],
        )]),
        global_fragment: None,
        service_fragments: BTreeMap::new(),
    }
}

pub(crate) fn test_material() -> CertificateMaterial {
    let pair = rcgen::generate_simple_self_signed(["secure.example.com".to_owned()]).unwrap();
    CertificateMaterial::new(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
}
