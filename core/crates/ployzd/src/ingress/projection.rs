//! The observer-local projection of ingress state that Caddy's config is rendered from.

use ployz_core::labels;
use ployz_core::{
    CertificateHost, ContainerAddress, ContainerId, DeploymentLogId, HttpProtocol, IngressHost,
    Machine, MachineId, PortPublication, ProxyRoutes, QualifiedService, ServiceContainer,
    hostname_owners,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs, io,
    num::{NonZeroU16, NonZeroUsize},
    os::unix::fs::PermissionsExt,
    path::Path,
};

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
    /// Qualified Service that owns the hostname.
    pub(super) owner: QualifiedService,
    /// Ordered Serving Container endpoints published over HTTP.
    pub(super) http: Option<Vec<IngressEndpoint>>,
    /// Ordered Serving Container endpoints published over HTTPS.
    pub(super) https: Option<Vec<IngressEndpoint>>,
    /// What the public trace page reports about the owner.
    pub(super) trace: PublicationTrace,
}

/// The owner facts the trace page shows, independent of health and endpoint eligibility.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct PublicationTrace {
    /// Deployment attempt of the owner's newest Service Container that is In, or when none
    /// is, the newest held or pending one, or else the newest of any; when it carries a
    /// valid one. So it changes only with the In set, which changes only with a route.
    pub(super) deploy: Option<DeploymentLogId>,
    /// Count of the owner's observed Service Containers per Machine, in any runtime state,
    /// leaving out copies rotated Out: only their removal follows, and that removal changes
    /// no route, so counting them would keep a deploy's old copies on the page.
    pub(super) placement: BTreeMap<MachineId, NonZeroUsize>,
}

impl PublicationTrace {
    /// Trace facts for every Service, from one pass over its observed Service Containers.
    fn by_service<'a>(
        containers: impl IntoIterator<Item = &'a ServiceContainer>,
    ) -> BTreeMap<QualifiedService, Self> {
        let mut grouped = BTreeMap::<
            QualifiedService,
            (BTreeMap<MachineId, NonZeroUsize>, &'a ServiceContainer),
        >::new();
        for container in containers {
            let observation = container.as_observation();
            let (placement, newest) = grouped
                .entry(observation.identity())
                .or_insert_with(|| (BTreeMap::new(), container));
            if !retired(container) {
                placement
                    .entry(observation.machine_id)
                    .and_modify(|count| *count = count.saturating_add(1))
                    .or_insert(NonZeroUsize::MIN);
            }
            if (
                naming_rank(container),
                container.as_observation().recency_key(),
            ) > (naming_rank(newest), newest.as_observation().recency_key())
            {
                *newest = container;
            }
        }
        grouped
            .into_iter()
            .map(|(service, (placement, newest))| {
                // Only the newest Container names the attempt; an older sibling's label would be stale.
                let deploy = newest
                    .as_observation()
                    .labels
                    .get(labels::DEPLOYMENT_ID)
                    .and_then(|label| DeploymentLogId::parse(label).ok());
                (service, Self { deploy, placement })
            })
            .collect()
    }
}

/// Which copies name the trace page's deploy first: In, then held or pending, then Out. A
/// copy leaving or joining without a route change then never changes the name.
fn naming_rank(container: &ServiceContainer) -> u8 {
    match container.as_observation().rotation() {
        ployz_core::CopyRotation::In => 2,
        ployz_core::CopyRotation::Held(_) | ployz_core::CopyRotation::Pending => 1,
        ployz_core::CopyRotation::Out => 0,
    }
}

/// Rotated Out for good: only its removal follows, which changes no route.
fn retired(container: &ServiceContainer) -> bool {
    matches!(
        container.as_observation().rotation(),
        ployz_core::CopyRotation::Out
    )
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

impl IngressSite {
    /// Ordered endpoints published for one HTTP protocol, when published.
    #[must_use]
    pub(super) fn route(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        let publication = self.publication.as_ref()?;
        match protocol {
            HttpProtocol::Http => publication.http.as_deref(),
            HttpProtocol::Https => publication.https.as_deref(),
        }
    }

    /// The HTTPS route with the certificate Caddy serves it under. Without material,
    /// Caddy renders no HTTPS site, so the route proxies nothing.
    #[must_use]
    pub(super) fn https(&self) -> Option<(&CertificateMaterial, &[IngressEndpoint])> {
        self.material().zip(self.route(HttpProtocol::Https))
    }

    /// Ordered endpoints Caddy proxies to for one HTTP protocol.
    #[must_use]
    pub(super) fn proxied(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        match protocol {
            HttpProtocol::Http => self.route(HttpProtocol::Http),
            HttpProtocol::Https => self.https().map(|(_, endpoints)| endpoints),
        }
    }

    #[must_use]
    pub(super) fn challenge(&self) -> Option<&CertificateChallenge> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.challenge.as_ref())
    }

    /// The material Caddy pins for this hostname: for its HTTPS route, or for a hostname
    /// no Service publishes any more, so HTTPS still completes the handshake and answers
    /// like HTTP. An HTTP-only hostname pins nothing, as before.
    #[must_use]
    pub(super) fn pinned(&self) -> Option<&CertificateMaterial> {
        self.material()
            .filter(|_| self.publication.is_none() || self.route(HttpProtocol::Https).is_some())
    }

    #[must_use]
    pub(super) fn material(&self) -> Option<&CertificateMaterial> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.material.as_ref())
    }
}

impl IngressProjection {
    #[must_use]
    pub(crate) fn derive(
        machine: &Machine,
        containers: &[ServiceContainer],
        certificates: &BTreeMap<CertificateHost, CertificateRow>,
        routes: &ProxyRoutes,
    ) -> Self {
        let owners = hostname_owners(containers.iter().map(ServiceContainer::as_observation));
        let mut sites = BTreeMap::<IngressHost, SiteBuilder>::new();
        for container in containers {
            let observation = container.as_observation();
            let owner = observation.identity();
            for port in &observation.resolved_spec.ports {
                let PortPublication::Ingress {
                    hostname,
                    http_protocol,
                    ..
                } = port
                else {
                    continue;
                };
                if owners.get(hostname) == Some(&owner) {
                    sites
                        .entry(hostname.clone())
                        .or_default()
                        .publish(owner.clone(), *http_protocol);
                }
            }
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
        let traces = PublicationTrace::by_service(containers);
        let sites = sites
            .into_iter()
            .map(|(hostname, site)| {
                let trace = site
                    .owner
                    .as_ref()
                    .and_then(|owner| traces.get(owner))
                    .cloned()
                    .unwrap_or_default();
                site.into_site(hostname, trace)
            })
            .collect();
        Self {
            machine: machine.clone(),
            sites,
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

    fn into_site(self, hostname: IngressHost, trace: PublicationTrace) -> IngressSite {
        let publication = self.owner.map(|owner| IngressPublication {
            owner,
            http: self.http,
            https: self.https,
            trace,
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
