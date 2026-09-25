//! Whether an Ingress Hostname reaches this Cluster, judged by fetching a verify path through it.

use std::net::IpAddr;

use crate::{IngressHost, MachineId};

/// Path every ingress Machine answers with its Machine id, probed through an Ingress Hostname.
///
/// It sits under the ACME challenge prefix so the one proxy exemption HTTP-01 needs also lets
/// the probe through. Challenge tokens are at least 22 characters, so it never shadows one.
pub const HOSTNAME_VERIFY_PATH: &str = "/.well-known/acme-challenge/ployz-verify";

/// What a `GET http://<hostname>` + [`HOSTNAME_VERIFY_PATH`] returned, without following redirects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifyAnswer {
    /// No HTTP response: refused, reset, or timed out.
    NoAnswer,
    /// Any HTTP response. `location` is empty when the header is missing.
    Answered {
        status: u16,
        location: String,
        body: String,
    },
}

/// How a hostname that reaches this Cluster gets here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterRoute {
    /// A resolved address that answered is a Machine public address.
    Direct,
    /// A Machine answered, but only through addresses that are not Machine public addresses.
    ViaProxy,
}

/// Why a hostname does not reach this Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    DoesNotResolve,
    Unreachable,
    RedirectsToHttps,
    ReachesElsewhere,
}

/// What one Machine saw fetching [`HOSTNAME_VERIFY_PATH`] through an Ingress Hostname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostnameVerdict {
    Refused(Refusal),
    ReachesCluster(ClusterRoute),
}

/// Judge the verify answer from each resolved address against this Cluster's Machines.
///
/// Any address that reaches this Cluster is enough, directly before via a proxy; otherwise
/// the first address decides. No addresses is "does not resolve", including lookup failure.
#[must_use]
pub fn hostname_verdict(
    answers: &[(IpAddr, VerifyAnswer)],
    machine_ids: &[MachineId],
    cluster_addresses: &[IpAddr],
) -> HostnameVerdict {
    let verdicts: Vec<_> = answers
        .iter()
        .map(|(address, answer)| judge(*address, answer, machine_ids, cluster_addresses))
        .collect();
    [ClusterRoute::Direct, ClusterRoute::ViaProxy]
        .map(HostnameVerdict::ReachesCluster)
        .into_iter()
        .find(|best| verdicts.contains(best))
        .or(verdicts.first().copied())
        .unwrap_or(HostnameVerdict::Refused(Refusal::DoesNotResolve))
}

fn judge(
    address: IpAddr,
    answer: &VerifyAnswer,
    machine_ids: &[MachineId],
    cluster_addresses: &[IpAddr],
) -> HostnameVerdict {
    let VerifyAnswer::Answered {
        status,
        location,
        body,
    } = answer
    else {
        return HostnameVerdict::Refused(Refusal::Unreachable);
    };
    if (300..400).contains(status) && location.starts_with("https://") {
        return HostnameVerdict::Refused(Refusal::RedirectsToHttps);
    }
    let ours = (200..300).contains(status)
        && MachineId::parse(body.trim()).is_ok_and(|id| machine_ids.contains(&id));
    if !ours {
        return HostnameVerdict::Refused(Refusal::ReachesElsewhere);
    }
    HostnameVerdict::ReachesCluster(if cluster_addresses.contains(&address) {
        ClusterRoute::Direct
    } else {
        ClusterRoute::ViaProxy
    })
}

/// Why a hostname does not reach this Cluster, with one fix.
#[must_use]
pub fn refusal_reason(
    hostname: &IngressHost,
    refusal: Refusal,
    cluster_addresses: &[IpAddr],
) -> String {
    match refusal {
        Refusal::DoesNotResolve => format!(
            "{hostname} does not resolve. Add a DNS record pointing at {}.",
            join_addresses(cluster_addresses)
        ),
        Refusal::Unreachable => {
            format!("{hostname} did not answer on port 80. Open port 80 to this Cluster.")
        }
        // ============================================================================
        // TODO: DOCS PAGE NEEDED — custom domains behind a proxy (Cloudflare and other
        // CDNs). Nothing documents this yet. When docs are written, cover:
        //   - why "Always Use HTTPS" blocks the first certificate (the edge redirects
        //     HTTP-01 to HTTPS before this Cluster has a certificate to answer with);
        //   - the fix: a proxy rule exempting /.well-known/acme-challenge/* from HTTPS
        //     redirects (Cloudflare: a Configuration Rule);
        //   - the alternative: upload a proxy origin certificate as Certificate Material;
        //   - SSL mode: use Full (strict) once issued, never Flexible.
        // Then link the page from this message and from the dashboard domain row.
        // ============================================================================
        Refusal::RedirectsToHttps => format!(
            "{hostname} redirects HTTP to HTTPS before reaching this Cluster. \
             Exempt /.well-known/acme-challenge/* from HTTPS redirects in your proxy."
        ),
        Refusal::ReachesElsewhere => format!(
            "{hostname} answers from another server. Point it at {}.",
            join_addresses(cluster_addresses)
        ),
    }
}

fn join_addresses(addresses: &[IpAddr]) -> String {
    if addresses.is_empty() {
        return "this Cluster's Machine addresses (none are published)".into();
    }
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{
        ClusterRoute, HostnameVerdict, Refusal, VerifyAnswer, hostname_verdict, refusal_reason,
    };
    use crate::{IngressHost, MachineId};

    const OURS: &str = "0123456789abcdef0123456789abcdef";
    const THEIRS: &str = "fedcba9876543210fedcba9876543210";
    const PROXY: &str = "198.51.100.10";
    const MACHINE: &str = "192.0.2.1";

    #[test]
    fn verdict_covers_every_answer() {
        let ids = [MachineId::parse(OURS).unwrap()];
        let judge =
            |answer: VerifyAnswer| hostname_verdict(&[(ip(PROXY), answer)], &ids, &[ip(MACHINE)]);

        assert_eq!(
            hostname_verdict(&[], &ids, &[ip(MACHINE)]),
            HostnameVerdict::Refused(Refusal::DoesNotResolve)
        );
        assert_eq!(
            judge(VerifyAnswer::NoAnswer),
            HostnameVerdict::Refused(Refusal::Unreachable)
        );
        assert_eq!(
            judge(answered(301, "https://app.example.com/", "")),
            HostnameVerdict::Refused(Refusal::RedirectsToHttps)
        );
        assert_eq!(
            judge(answered(302, "http://elsewhere.example.com/", "")),
            HostnameVerdict::Refused(Refusal::ReachesElsewhere)
        );
        assert_eq!(
            judge(answered(200, "", THEIRS)),
            HostnameVerdict::Refused(Refusal::ReachesElsewhere)
        );
        assert_eq!(
            judge(answered(404, "", OURS)),
            HostnameVerdict::Refused(Refusal::ReachesElsewhere)
        );
        assert_eq!(
            judge(answered(200, "", &format!("{OURS}\n"))),
            HostnameVerdict::ReachesCluster(ClusterRoute::ViaProxy)
        );
    }

    #[test]
    fn a_direct_answer_beats_a_proxied_one_and_otherwise_the_first_address_decides() {
        let ids = [MachineId::parse(OURS).unwrap()];
        let cluster = [ip(MACHINE)];

        assert_eq!(
            hostname_verdict(
                &[
                    (ip(PROXY), answered(200, "", OURS)),
                    (ip(MACHINE), answered(200, "", OURS)),
                ],
                &ids,
                &cluster
            ),
            HostnameVerdict::ReachesCluster(ClusterRoute::Direct)
        );
        assert_eq!(
            hostname_verdict(
                &[
                    (ip(PROXY), answered(200, "", THEIRS)),
                    (ip(MACHINE), VerifyAnswer::NoAnswer),
                ],
                &ids,
                &cluster
            ),
            HostnameVerdict::Refused(Refusal::ReachesElsewhere)
        );
    }

    #[test]
    fn reason_names_one_fix_per_refusal() {
        let hostname = IngressHost::parse("app.example.com").unwrap();
        let cluster = [ip(MACHINE), ip("192.0.2.2")];
        let reason = |refusal| refusal_reason(&hostname, refusal, &cluster);

        assert_eq!(
            reason(Refusal::DoesNotResolve),
            "app.example.com does not resolve. Add a DNS record pointing at 192.0.2.1 or 192.0.2.2."
        );
        assert_eq!(
            reason(Refusal::Unreachable),
            "app.example.com did not answer on port 80. Open port 80 to this Cluster."
        );
        assert_eq!(
            reason(Refusal::RedirectsToHttps),
            "app.example.com redirects HTTP to HTTPS before reaching this Cluster. \
             Exempt /.well-known/acme-challenge/* from HTTPS redirects in your proxy."
        );
        assert_eq!(
            reason(Refusal::ReachesElsewhere),
            "app.example.com answers from another server. Point it at 192.0.2.1 or 192.0.2.2."
        );
        assert_eq!(
            refusal_reason(&hostname, Refusal::ReachesElsewhere, &[]),
            "app.example.com answers from another server. Point it at this Cluster's Machine addresses (none are published)."
        );
    }

    fn answered(status: u16, location: &str, body: &str) -> VerifyAnswer {
        VerifyAnswer::Answered {
            status,
            location: location.into(),
            body: body.into(),
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }
}
