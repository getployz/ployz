use std::collections::BTreeSet;

use ployz_store_api::{CertificateStore, RoutingSnapshotReader, StoreDriver};
use ployz_types::model::{
    CertificateRecord, CertificateState, RoutingState, ServiceRelease, ServiceReleaseRecord,
    ServiceRevisionRecord, ServiceRoutingPolicy,
};
use ployz_types::spec::{Namespace, RouteSpec, ServiceSpec};

use crate::certificates::account_id_for_issuer_url;
use crate::deploy::plan::ResolvedPlan;
use crate::error::{Error, Result};

pub(super) async fn warnings_for_plan(
    store: &StoreDriver,
    plan: &ResolvedPlan,
) -> Result<Vec<String>> {
    let domains = managed_hostnames_for_plan(plan);
    if domains.is_empty() {
        return Ok(Vec::new());
    }

    let certificates = store.list_certificates().await?;
    Ok(build_domain_warnings(&domains, &certificates))
}

pub(super) async fn ensure_certificate_intents(
    store: &StoreDriver,
    plan: &ResolvedPlan,
    issuer_url: &str,
) -> Result<Vec<String>> {
    let hostnames = managed_hostnames_for_plan(plan);
    let now = ployz_types::time::now_unix_secs();
    for hostname in &hostnames {
        if store.get_certificate(hostname).await?.is_some() {
            continue;
        }
        store
            .upsert_certificate(&CertificateRecord {
                hostname: hostname.clone(),
                issuer_url: issuer_url.to_string(),
                account_id: account_id_for_issuer_url(issuer_url),
                state: CertificateState::Pending,
                active_version_id: None,
                versions: Vec::new(),
                order_url: None,
                last_error: None,
                requested_at: now,
                updated_at: now,
                next_renewal_at: None,
            })
            .await?;
    }
    Ok(hostnames)
}

pub(super) async fn validate_hostname_ownership(
    store: &StoreDriver,
    plan: &ResolvedPlan,
) -> Result<()> {
    let desired = hostname_owners_for_plan(plan)?;
    if desired.is_empty() {
        return Ok(());
    }

    let routing_state = store.load_routing_state().await?;
    // This is admission validation, not a concurrency primitive. Same-namespace
    // deploys are already namespace-locked, but concurrent deploys in different
    // namespaces can still race for a brand-new hostname. Full serialization
    // belongs in a durable hostname ownership record or a scoped NATS lease.
    let committed = hostname_owners_for_routing_state(&routing_state, plan.namespace())?;
    for desired_owner in desired {
        let Some(existing_owner) = committed
            .iter()
            .find(|owner| owner.hostname == desired_owner.hostname)
        else {
            continue;
        };
        return Err(Error::operation(
            "deploy_preview",
            format!(
                "hostname '{}' is already owned by {}/{} and cannot be used by {}/{}",
                desired_owner.hostname,
                existing_owner.namespace,
                existing_owner.service,
                desired_owner.namespace,
                desired_owner.service
            ),
        ));
    }
    Ok(())
}

pub(super) fn managed_hostnames_for_plan(plan: &ResolvedPlan) -> Vec<String> {
    hostname_owners_for_plan(plan)
        .map(|owners| {
            owners
                .into_iter()
                .map(|owner| owner.hostname)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostnameOwner {
    hostname: String,
    namespace: Namespace,
    service: String,
}

fn hostname_owners_for_plan(plan: &ResolvedPlan) -> Result<Vec<HostnameOwner>> {
    let mut owners = Vec::new();
    let mut seen: Vec<HostnameOwner> = Vec::new();
    for service in plan.services() {
        let Some(spec) = &service.spec else {
            continue;
        };
        for hostname in hostnames_for_spec(spec) {
            let owner = HostnameOwner {
                hostname,
                namespace: plan.namespace().clone(),
                service: spec.name.clone(),
            };
            reject_duplicate_owner(&seen, &owner)?;
            seen.push(owner.clone());
            owners.push(owner);
        }
    }
    Ok(owners)
}

fn hostname_owners_for_routing_state(
    state: &RoutingState,
    deploying_namespace: &Namespace,
) -> Result<Vec<HostnameOwner>> {
    let mut owners = Vec::new();
    for release in &state.releases {
        if &release.namespace == deploying_namespace {
            continue;
        }
        let Some(revision) = active_revision_for_release(state, release) else {
            return Err(Error::operation(
                "deploy_preview",
                format!(
                    "missing active revision '{}' for committed service {}/{}",
                    routing_revision_hash(&release.release),
                    release.namespace,
                    release.service
                ),
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(&revision.spec_json).map_err(|error| {
            Error::operation(
                "deploy_preview",
                format!(
                    "decode committed service spec for {}/{}: {error}",
                    revision.namespace, revision.service
                ),
            )
        })?;
        for hostname in hostnames_for_spec(&spec) {
            owners.push(HostnameOwner {
                hostname,
                namespace: release.namespace.clone(),
                service: release.service.clone(),
            });
        }
    }
    Ok(owners)
}

fn active_revision_for_release<'a>(
    state: &'a RoutingState,
    release: &ServiceReleaseRecord,
) -> Option<&'a ServiceRevisionRecord> {
    let revision_hash = routing_revision_hash(&release.release);
    state.revisions.iter().find(|revision| {
        revision.namespace == release.namespace
            && revision.service == release.service
            && revision.revision_hash == revision_hash
    })
}

fn routing_revision_hash(release: &ServiceRelease) -> &str {
    match &release.routing {
        ServiceRoutingPolicy::Direct { revision_hash } => revision_hash.as_str(),
        ServiceRoutingPolicy::Split { .. } => release.primary_revision_hash.as_str(),
    }
}

fn hostnames_for_spec(spec: &ServiceSpec) -> Vec<String> {
    let mut hostnames = Vec::new();
    for route in &spec.routes {
        let RouteSpec::Http(route) = route else {
            continue;
        };
        for hostname in &route.hostnames {
            let hostname = normalize_hostname(hostname);
            if hostname.is_empty() || hostname.starts_with("*.") {
                continue;
            }
            hostnames.push(hostname);
        }
    }
    hostnames
}

fn reject_duplicate_owner(existing: &[HostnameOwner], next: &HostnameOwner) -> Result<()> {
    let Some(previous) = existing
        .iter()
        .find(|owner| owner.hostname == next.hostname)
    else {
        return Ok(());
    };
    Err(Error::operation(
        "deploy_preview",
        format!(
            "hostname '{}' is declared by both {}/{} and {}/{}",
            next.hostname, previous.namespace, previous.service, next.namespace, next.service
        ),
    ))
}

fn build_domain_warnings(domains: &[String], certificates: &[CertificateRecord]) -> Vec<String> {
    let mut warnings = Vec::new();
    for domain in domains {
        warnings.extend(tls_warnings_for_domain(domain, certificates));
    }
    warnings
}

fn tls_warnings_for_domain(domain: &str, certificates: &[CertificateRecord]) -> Vec<String> {
    let Some(record) = certificates.iter().find(|record| record.hostname == domain) else {
        return vec![format!(
            "TLS for {domain} is pending; HTTPS will activate when the certificate is ready"
        )];
    };

    match record.state {
        CertificateState::Active => Vec::new(),
        CertificateState::Pending | CertificateState::Issuing | CertificateState::RenewalDue => {
            vec![format!(
                "TLS for {domain} is {}; HTTPS will activate when the certificate is ready",
                record.state
            )]
        }
        CertificateState::Failed => {
            let detail = record
                .last_error
                .as_deref()
                .unwrap_or("certificate issuance failed");
            vec![format!("TLS for {domain} failed: {detail}")]
        }
    }
}

fn normalize_hostname(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{build_domain_warnings, normalize_hostname};
    use ployz_types::model::{CertificateRecord, CertificateState};

    #[test]
    fn domain_warnings_are_quiet_for_active_certificates() {
        let warnings = build_domain_warnings(
            &[String::from("api.example.com")],
            &[certificate(
                "api.example.com",
                CertificateState::Active,
                None,
            )],
        );

        assert!(warnings.is_empty());
    }

    #[test]
    fn domain_warnings_surface_pending_and_failed_certificate_state() {
        let warnings = build_domain_warnings(
            &[
                String::from("pending.example.com"),
                String::from("failed.example.com"),
                String::from("missing.example.com"),
            ],
            &[
                certificate("pending.example.com", CertificateState::Issuing, None),
                certificate(
                    "failed.example.com",
                    CertificateState::Failed,
                    Some("dns challenge timed out"),
                ),
            ],
        );

        assert_eq!(
            warnings,
            vec![
                "TLS for pending.example.com is issuing; HTTPS will activate when the certificate is ready",
                "TLS for failed.example.com failed: dns challenge timed out",
                "TLS for missing.example.com is pending; HTTPS will activate when the certificate is ready",
            ]
        );
    }

    #[test]
    fn normalize_hostname_matches_route_ownership_contract() {
        assert_eq!(normalize_hostname(" API.Example.COM. "), "api.example.com");
        assert_eq!(normalize_hostname("*.Example.COM"), "*.example.com");
    }

    fn certificate(
        hostname: &str,
        state: CertificateState,
        last_error: Option<&str>,
    ) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://acme.example/directory".into(),
            account_id: "acct".into(),
            state,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: last_error.map(String::from),
            requested_at: 1,
            updated_at: 1,
            next_renewal_at: None,
        }
    }
}
