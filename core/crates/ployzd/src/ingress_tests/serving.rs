//! What counts as the same serving state, and what withholding a certificate changes.

use std::num::NonZeroU16;

use ployz_core::{ContainerAddress, ContainerId, DeploymentLogId};
use ployz_decide::caddy_load::Input;

use super::renderer_projection;
use crate::ingress::{
    CaddyCertificates, CaddyInput,
    certificates::certificate_file_stem,
    projection::{IngressEndpoint, IngressProjection},
};

fn input(projection: IngressProjection, caddy: Option<ContainerId>) -> CaddyInput {
    CaddyInput { projection, caddy }
}

fn clear_traces(projection: &mut IngressProjection) {
    for trace in projection.traces.values_mut() {
        trace.deploy = Some(DeploymentLogId::parse("elsewhere").unwrap());
        trace.placement.clear();
    }
}

fn secure_stem(projection: &IngressProjection) -> String {
    let site = projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap();
    certificate_file_stem(&site.hostname, site.pinned().unwrap())
}

fn site_mut<'a>(
    projection: &'a mut IngressProjection,
    hostname: &str,
) -> &'a mut crate::ingress::projection::IngressSite {
    projection
        .sites
        .iter_mut()
        .find(|site| site.hostname.as_str() == hostname)
        .unwrap()
}

fn secure(projection: &IngressProjection) -> &crate::ingress::projection::IngressSite {
    projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap()
}

/// A Container's start or removal changes the trace page while every route stays, and a
/// reload for it alone would clear Caddy's passive health counts (#2522).
#[test]
fn a_trace_only_change_serves_as_the_same_and_anything_else_does_not() {
    let base = input(renderer_projection(), None);

    let mut traced = renderer_projection();
    clear_traces(&mut traced);
    assert!(base.serves_as(&input(traced, None)));

    let mut rerouted = renderer_projection();
    site_mut(&mut rerouted, "example.com")
        .publication
        .as_mut()
        .unwrap()
        .http
        .as_mut()
        .unwrap()
        .push(IngressEndpoint {
            address: ContainerAddress("10.210.9.9".parse().unwrap()),
            port: NonZeroU16::new(80).unwrap(),
        });
    assert!(!base.serves_as(&input(rerouted, None)));

    let mut unpublished = renderer_projection();
    site_mut(&mut unpublished, "example.com").publication = None;
    assert!(!base.serves_as(&input(unpublished, None)));

    let mut unpinned = renderer_projection();
    site_mut(&mut unpinned, "secure.example.com")
        .certificate
        .as_mut()
        .unwrap()
        .material = None;
    assert!(!base.serves_as(&input(unpinned, None)));

    let mut other_machine = renderer_projection();
    other_machine.machine.id = ployz_core::MachineId::parse("c".repeat(32)).unwrap();
    assert!(!base.serves_as(&input(other_machine, None)));

    let caddy = ContainerId::parse("d".repeat(64)).unwrap();
    assert!(!base.serves_as(&input(renderer_projection(), Some(caddy))));
}

#[test]
fn an_unloaded_certificate_is_left_out_with_its_error_untouched() {
    let projection = renderer_projection();
    let stem = secure_stem(&projection);

    let mut certificates = CaddyCertificates::default();
    let withheld = certificates.loaded_only(&projection);
    assert!(
        secure(&withheld)
            .certificate
            .as_ref()
            .unwrap()
            .material
            .is_none()
    );
    assert!(
        secure(&withheld)
            .certificate
            .as_ref()
            .unwrap()
            .last_error
            .is_none()
    );

    certificates.accepted.insert(stem);
    assert_eq!(certificates.loaded_only(&projection), projection);
}

#[test]
fn a_refused_certificate_is_left_out_and_named_and_forgotten_once_its_site_is_gone() {
    let projection = renderer_projection();
    let stem = secure_stem(&projection);

    let mut certificates = CaddyCertificates::default();
    certificates
        .refused
        .insert(stem.clone(), "bad key".to_owned());
    certificates
        .refused
        .insert("gone".to_owned(), "old".to_owned());
    let projection = certificates.quarantine(projection);

    let certificate = secure(&projection).certificate.as_ref().unwrap();
    assert!(certificate.material.is_none());
    assert_eq!(
        certificate.last_error.as_deref(),
        Some("Caddy refused it: bad key")
    );
    assert_eq!(
        certificates.refused.keys().collect::<Vec<_>>(),
        vec![&stem],
        "a refusal for a site no longer pinned is dropped"
    );
}
