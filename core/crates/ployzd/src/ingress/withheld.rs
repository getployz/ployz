//! Certificates the Ingress Proxy leaves out of a projection: the one place that clears a
//! site's material.

use std::collections::BTreeMap;

use super::{
    certificates::certificate_file_stem,
    projection::{IngressProjection, IngressSite},
};

/// Why a certificate is left out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Withhold {
    /// Caddy refused it with this reason, which the site's `last_error` names in its place.
    Refused(String),
    /// Caddy has not loaded it yet, so the site stays as if the material were pending.
    Unloaded,
}

/// The certificates left out, by the file stem of the material the Caddyfile pins.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Withheld(BTreeMap<String, Withhold>);

impl Withheld {
    pub(super) fn refused<'a>(
        refusals: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> Self {
        Self(
            refusals
                .into_iter()
                .map(|(stem, error)| (stem.clone(), Withhold::Refused(error.clone())))
                .collect(),
        )
    }

    pub(super) fn unloaded(stems: impl IntoIterator<Item = String>) -> Self {
        Self(
            stems
                .into_iter()
                .map(|stem| (stem, Withhold::Unloaded))
                .collect(),
        )
    }
}

/// The file stem of the certificate the Caddyfile pins for `site`.
pub(super) fn pinned_stem(site: &IngressSite) -> Option<String> {
    site.pinned()
        .map(|material| certificate_file_stem(&site.hostname, material))
}

impl IngressProjection {
    /// The file stem of every certificate the Caddyfile pins, in site order.
    pub(super) fn pinned_stems(&self) -> impl Iterator<Item = String> {
        self.sites.iter().filter_map(pinned_stem)
    }

    /// This projection without the certificates in `withheld`, as if each were still pending.
    /// A refused one names its refusal in its place, so one site's certificate never blocks the
    /// rest of the config.
    #[must_use]
    pub(super) fn withhold(mut self, withheld: &Withheld) -> Self {
        for site in &mut self.sites {
            let Some(why) = pinned_stem(site).and_then(|stem| withheld.0.get(&stem)) else {
                continue;
            };
            let Some(certificate) = &mut site.certificate else {
                continue;
            };
            certificate.material = None;
            if let Withhold::Refused(error) = why {
                certificate.last_error = Some(format!("Caddy refused it: {error}"));
            }
        }
        self
    }
}
