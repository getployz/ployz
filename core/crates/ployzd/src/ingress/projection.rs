//! The observer-local projection of ingress state that Caddy's config is rendered from.

use ployz_core::{
    CertificateHost, ContainerAddress, ContainerId, HttpProtocol, IngressHost, Machine, MachineId,
    PortPublication, ProxyRoutes, QualifiedService, ServiceContainer, hostname_owners,
    published_hostnames,
};
use serde::Serialize;
use std::{
    collections::BTreeMap, fs, io, num::NonZeroU16, os::unix::fs::PermissionsExt, path::Path,
};

use super::publication_trace::{NO_TRACE, PublicationTrace};
use crate::{
    corrosion::{CertificateChallenge, CertificateMaterial, CertificateRow, published_cover},
    filesystem::set_ployz_group,
};

/// Fully derived observer-local input to ingress rendering and application.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressProjection {
    /// Local Machine whose ingress process consumes this projection.
    pub(super) machine: Machine,
    /// Hostname-grouped ingress routes in stable hostname order.
    pub(super) sites: Vec<IngressSite>,
    /// What the public trace page reports per Service that owns a site. It is rendered, never
    /// served: a change here alone leaves Caddy's config as it is until a route changes.
    pub(super) traces: BTreeMap<QualifiedService, PublicationTrace>,
}

/// All projected ingress state for one hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressSite {
    /// Published hostname or certificate hostname.
    pub(super) hostname: IngressHost,
    /// Singular Hostname Owner and its protocol publications, when published.
    pub(super) publication: Option<IngressPublication>,
    /// Ployz-owned certificate state for this hostname, when present.
    pub(super) certificate: Option<ProjectedCertificate>,
}

/// A hostname's singular owner and at-most-one publication per protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressPublication {
    pub(super) owner: QualifiedService,
    /// Ordered Serving Container endpoints published over HTTP.
    pub(super) http: Option<Vec<IngressEndpoint>>,
    /// Ordered Serving Container endpoints published over HTTPS.
    pub(super) https: Option<Vec<IngressEndpoint>>,
}

/// One Serving Container address and container port.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressEndpoint {
    pub(super) address: ContainerAddress,
    /// Published container port.
    pub(super) port: NonZeroU16,
}

/// Certificate state that can affect ingress behavior or generated output.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct ProjectedCertificate {
    pub(super) challenge: Option<CertificateChallenge>,
    /// Certificate and private key served as given.
    pub(super) material: Option<CertificateMaterial>,
    /// Operator-visible issuance refusal included in generated output.
    pub(super) last_error: Option<String>,
}

/// What one listener of a site does with a request for its hostname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Serve<'a> {
    /// Proxies to the endpoints, or answers NOT_RUNNING when there are none.
    Proxy(&'a [IngressEndpoint]),
    /// Sends the visitor to the HTTPS route, which proxies to the endpoints.
    RedirectToHttps(&'a [IngressEndpoint]),
    Unrouted(Unrouted),
}

/// Why a listener routes a hostname nowhere.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Unrouted {
    /// Nothing is published for the hostname: only its challenge or its pinned material is left.
    NotDeployed,
}

impl IngressSite {
    fn route(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        let publication = self.publication.as_ref()?;
        match protocol {
            HttpProtocol::Http => publication.http.as_deref(),
            HttpProtocol::Https => publication.https.as_deref(),
        }
    }

    /// The public HTTP listener's answer, when it has a site for the hostname. An HTTPS-only
    /// hostname redirects HTTP to HTTPS; a challenge-only site publishes nothing else.
    #[must_use]
    pub(super) fn http(&self) -> Option<Serve<'_>> {
        if let Some(endpoints) = self.route(HttpProtocol::Http) {
            return Some(Serve::Proxy(endpoints));
        }
        if let Some((_, Serve::Proxy(endpoints))) = self.https() {
            return Some(Serve::RedirectToHttps(endpoints));
        }
        self.challenge()
            .map(|_| Serve::Unrouted(Unrouted::NotDeployed))
    }

    /// The public HTTPS listener's answer with the material Caddy pins for it, when it has a
    /// site for the hostname: for its HTTPS route, or for a hostname no Service publishes any
    /// more, so HTTPS still completes the handshake and answers like HTTP. Without material,
    /// Caddy renders no HTTPS site, so the route proxies nothing. An HTTP-only hostname pins
    /// nothing.
    #[must_use]
    pub(super) fn https(&self) -> Option<(&CertificateMaterial, Serve<'_>)> {
        let material = self.material()?;
        match self.route(HttpProtocol::Https) {
            Some(endpoints) => Some((material, Serve::Proxy(endpoints))),
            None if self.publication.is_none() => {
                Some((material, Serve::Unrouted(Unrouted::NotDeployed)))
            }
            None => None,
        }
    }

    /// The endpoints the loopback listener proxies to. The door terminates TLS itself, so
    /// material does not matter: the HTTPS route is what the visitor asked for through the
    /// door, and an HTTP-only hostname serves its HTTP route to the visitor the door sends on.
    #[must_use]
    pub(super) fn door(&self) -> Option<&[IngressEndpoint]> {
        self.route(HttpProtocol::Https)
            .or_else(|| self.route(HttpProtocol::Http))
    }

    /// Ordered endpoints Caddy proxies to for one HTTP protocol.
    #[must_use]
    pub(super) fn proxied(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        let serve = match protocol {
            HttpProtocol::Http => self.http(),
            HttpProtocol::Https => self.https().map(|(_, serve)| serve),
        };
        match serve {
            Some(Serve::Proxy(endpoints)) => Some(endpoints),
            _ => None,
        }
    }

    #[must_use]
    pub(super) fn challenge(&self) -> Option<&CertificateChallenge> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.challenge.as_ref())
    }

    /// The material Caddy pins for this hostname, which the HTTPS listener serves it under.
    #[must_use]
    pub(super) fn pinned(&self) -> Option<&CertificateMaterial> {
        self.https().map(|(material, _)| material)
    }

    #[must_use]
    pub(super) fn material(&self) -> Option<&CertificateMaterial> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.material.as_ref())
    }
}

impl IngressProjection {
    /// What the trace page reports about `owner`, which reports nothing for a Service it has
    /// no copy of.
    #[must_use]
    pub(super) fn trace(&self, owner: &QualifiedService) -> &PublicationTrace {
        self.traces.get(owner).unwrap_or(&NO_TRACE)
    }

    #[must_use]
    pub(crate) fn derive(
        machine: &Machine,
        containers: &[ServiceContainer],
        certificates: &BTreeMap<CertificateHost, CertificateRow>,
        routes: &ProxyRoutes,
    ) -> Self {
        let owners = hostname_owners(containers.iter().map(ServiceContainer::as_observation));
        let mut sites = BTreeMap::<IngressHost, SiteBuilder>::new();
        for (hostname, owner, protocol) in
            published_hostnames(containers.iter().map(ServiceContainer::as_observation))
        {
            sites.entry(hostname).or_default().publish(owner, protocol);
        }

        let mut serving: Vec<_> = routes.values().flatten().collect();
        serving.sort_by_key(|serving| container_order(&machine.id, serving.as_container()));
        for serving in serving {
            let observation = serving.as_observation();
            let address = serving.address();
            let owner = observation.identity();
            for port in &observation.resolved_spec.ports {
                let PortPublication::Ingress {
                    hostname,
                    container_port,
                    http_protocol,
                    ..
                } = port
                else {
                    continue;
                };
                if owners.get(hostname) == Some(&owner) {
                    sites
                        .get_mut(hostname)
                        .expect("published hostname was collected above")
                        .publish(owner.clone(), *http_protocol)
                        .push(IngressEndpoint {
                            address,
                            port: *container_port,
                        });
                }
            }
        }

        for (hostname, row) in certificates {
            // A wildcard is not a site; it only lends material to the sites it covers.
            let Ok(hostname) = IngressHost::parse(hostname.as_str()) else {
                continue;
            };
            sites.entry(hostname).or_default().certificate = Some(ProjectedCertificate {
                challenge: row.challenge().cloned(),
                material: row.material().cloned(),
                last_error: row.last_error().map(ToOwned::to_owned),
            });
        }
        for (hostname, site) in &mut sites {
            // A hostname no Service publishes any more keeps its own material pinned, so a
            // covering wildcard replaces it too: nothing renews ACME material once covered.
            let unpublished_with_material = site.owner.is_none()
                && site
                    .certificate
                    .as_ref()
                    .is_some_and(|certificate| certificate.material.is_some());
            if site.https.is_none() && !unpublished_with_material {
                continue;
            }
            // Published wildcard material wins over ACME material, which stops renewing once covered.
            if let Some(material) = published_cover(hostname, certificates) {
                site.certificate.get_or_insert_default().material = Some(material.clone());
            }
        }
        let mut traces = PublicationTrace::by_service(containers);
        traces.retain(|service, _| {
            sites
                .values()
                .any(|site| site.owner.as_ref() == Some(service))
        });
        let sites = sites
            .into_iter()
            .map(|(hostname, site)| site.into_site(hostname))
            .collect();
        Self {
            machine: machine.clone(),
            sites,
            traces,
        }
    }
}

#[derive(Default)]
struct SiteBuilder {
    owner: Option<QualifiedService>,
    http: Option<Vec<IngressEndpoint>>,
    https: Option<Vec<IngressEndpoint>>,
    certificate: Option<ProjectedCertificate>,
}

impl SiteBuilder {
    fn publish(
        &mut self,
        owner: QualifiedService,
        protocol: HttpProtocol,
    ) -> &mut Vec<IngressEndpoint> {
        debug_assert!(self.owner.as_ref().is_none_or(|current| current == &owner));
        self.owner.get_or_insert(owner);
        match protocol {
            HttpProtocol::Http => self.http.get_or_insert_default(),
            HttpProtocol::Https => self.https.get_or_insert_default(),
        }
    }

    fn into_site(self, hostname: IngressHost) -> IngressSite {
        let publication = self.owner.map(|owner| IngressPublication {
            owner,
            http: self.http,
            https: self.https,
        });
        IngressSite {
            hostname,
            publication,
            certificate: self.certificate,
        }
    }
}

fn container_order(
    local_machine: &MachineId,
    container: &ServiceContainer,
) -> (bool, QualifiedService, (i64, ContainerId)) {
    let observation = container.as_observation();
    (
        observation.machine_id != *local_machine,
        observation.identity(),
        observation.recency_key(),
    )
}

pub(super) fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_ployz_group(path)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use ployz_core::{ContainerAddress, IngressHost, QualifiedService};

    use super::{
        IngressEndpoint, IngressPublication, IngressSite, ProjectedCertificate, Serve, Unrouted,
    };
    use crate::{
        corrosion::CertificateChallenge, ingress::projection::HttpProtocol,
        ingress::tests::test_material,
    };

    fn endpoint(port: u16) -> IngressEndpoint {
        IngressEndpoint {
            address: ContainerAddress("10.210.1.2".parse().unwrap()),
            port: NonZeroU16::new(port).unwrap(),
        }
    }

    /// A site with what its HTTP listener, HTTPS listener and the door answer.
    type Case<'a> = (
        &'a str,
        IngressSite,
        Option<Serve<'a>>,
        Option<Serve<'a>>,
        Option<&'a [IngressEndpoint]>,
    );

    fn proxied(serve: Option<Serve<'_>>) -> Option<&[IngressEndpoint]> {
        match serve {
            Some(Serve::Proxy(endpoints)) => Some(endpoints),
            _ => None,
        }
    }

    fn site(
        http: Option<Vec<IngressEndpoint>>,
        https: Option<Vec<IngressEndpoint>>,
        published: bool,
        material: bool,
        challenge: bool,
    ) -> IngressSite {
        IngressSite {
            hostname: IngressHost::parse("web.example.com").unwrap(),
            publication: published.then(|| IngressPublication {
                owner: QualifiedService::parse("app/web").unwrap(),
                http,
                https,
            }),
            certificate: (material || challenge).then(|| ProjectedCertificate {
                challenge: challenge.then(|| {
                    CertificateChallenge::parse(
                        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
                        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    )
                    .unwrap()
                }),
                material: material.then(test_material),
                last_error: None,
            }),
        }
    }

    #[test]
    fn each_listener_answers_by_what_the_site_publishes_and_holds() {
        let one = [endpoint(8080)];
        let other = [endpoint(8443)];
        let some = |endpoints: &[IngressEndpoint]| Some(endpoints.to_vec());
        let not_deployed = Serve::Unrouted(Unrouted::NotDeployed);

        // (http route, https route, published, material, challenge) -> http, https, door
        let cases: Vec<Case> = vec![
            (
                "http only",
                site(some(&one), None, true, false, false),
                Some(Serve::Proxy(&one)),
                None,
                Some(&one),
            ),
            (
                "http only with material pins nothing",
                site(some(&one), None, true, true, false),
                Some(Serve::Proxy(&one)),
                None,
                Some(&one),
            ),
            (
                "http only, no endpoints",
                site(Some(Vec::new()), None, true, false, false),
                Some(Serve::Proxy(&[])),
                None,
                Some(&[]),
            ),
            (
                "https only with material redirects http",
                site(None, some(&other), true, true, false),
                Some(Serve::RedirectToHttps(&other)),
                Some(Serve::Proxy(&other)),
                Some(&other),
            ),
            (
                "https only without material has no public site",
                site(None, some(&other), true, false, false),
                None,
                None,
                Some(&other),
            ),
            (
                "https only without material keeps its challenge",
                site(None, some(&other), true, false, true),
                Some(not_deployed),
                None,
                Some(&other),
            ),
            (
                "both protocols",
                site(some(&one), some(&other), true, true, false),
                Some(Serve::Proxy(&one)),
                Some(Serve::Proxy(&other)),
                Some(&other),
            ),
            (
                "unpublished with material",
                site(None, None, false, true, false),
                None,
                Some(not_deployed),
                None,
            ),
            (
                "unpublished challenge only",
                site(None, None, false, false, true),
                Some(not_deployed),
                None,
                None,
            ),
            (
                "unpublished with material and challenge",
                site(None, None, false, true, true),
                Some(not_deployed),
                Some(not_deployed),
                None,
            ),
        ];
        for (name, site, http, https, door) in cases {
            assert_eq!(site.http(), http, "{name}: http");
            assert_eq!(site.https().map(|(_, serve)| serve), https, "{name}: https");
            assert_eq!(site.door(), door, "{name}: door");
            assert_eq!(
                site.pinned().is_some(),
                https.is_some(),
                "{name}: pinned material"
            );
            assert_eq!(
                site.proxied(HttpProtocol::Http),
                proxied(http),
                "{name}: proxied http"
            );
            assert_eq!(
                site.proxied(HttpProtocol::Https),
                proxied(https),
                "{name}: proxied https"
            );
        }
    }
}
