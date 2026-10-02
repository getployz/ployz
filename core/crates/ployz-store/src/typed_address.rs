//! Typed Addresses: another Service's private address typed into a variable's value
//! instead of referenced. The value stays as typed; an edit answers with whose
//! addresses it types and what to set instead.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;

use ployz_core::config::{
    PRIVATE_DOMAIN_KEY, SavedServiceIntent, ValuePart, ValuePartOwner, private_domain,
    render_variable_parts,
};
use ployz_core::{RpcError, ServiceName};
use regex::Regex;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error;
use crate::variables::VariableKey;

/// What to set instead of a value with Typed Addresses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Instead {
    /// The value with each address referenced, ready to set.
    Reference { value: String },
    /// Nothing to show: the value is sealed, and a secret can't hold a reference.
    /// Seal only the password, in its own variable, and build the value from
    /// references to it and to the address.
    Sealed,
}

/// The Typed Addresses in one new value: whose they are, and what to set instead.
pub(crate) struct Typed {
    pub(crate) services: Vec<ServiceName>,
    pub(crate) instead: Instead,
}

/// A host, with its port if any: all of `site.internal.example.com`, never just
/// `site.internal`.
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("([A-Za-z0-9.-]+)(?::[0-9]+)?").expect("the host pattern compiles")
});
/// A URL's host, after its scheme and any user info.
static URL_HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("://(?:[^/?#@\\s]*@)?([A-Za-z0-9.-]+)").expect("the URL host pattern compiles")
});

/// What a plain value of `key`, as `parts`, types of `others`' private addresses,
/// with the value referencing each of them instead.
///
/// # Errors
/// Returns `internal` for a Service whose name is corrupt.
pub(crate) fn in_plain(
    parts: &[ValuePart],
    key: &VariableKey,
    others: &[&SavedServiceIntent],
    names: &BTreeMap<String, String>,
) -> Result<Option<Typed>, RpcError> {
    // One text, each reference a NUL: no host holds one, and a URL's user info may.
    // Text never holds one itself (`validate_text` refuses it), so each NUL is a reference.
    let text: String = parts
        .iter()
        .map(|part| match part {
            ValuePart::Text { value } => value.as_str(),
            ValuePart::Ref { .. } => "\0",
        })
        .collect();
    let hosts = hosts(&text, key, others);
    let Some(services) = services(&hosts)? else {
        return Ok(None);
    };
    let mut refs = parts
        .iter()
        .filter(|part| matches!(part, ValuePart::Ref { .. }))
        .cloned();
    let mut referenced = Vec::new();
    let mut from = 0;
    for (host, other) in &hosts {
        unflatten(
            text.get(from..host.start).unwrap_or_default(),
            &mut refs,
            &mut referenced,
        );
        referenced.push(ValuePart::Ref {
            owner: ValuePartOwner::Service {
                lineage_id: other.lineage_id.clone(),
            },
            key: PRIVATE_DOMAIN_KEY.to_owned(),
        });
        from = host.end;
    }
    unflatten(
        text.get(from..).unwrap_or_default(),
        &mut refs,
        &mut referenced,
    );
    Ok(Some(Typed {
        services,
        instead: Instead::Reference {
            value: render_variable_parts(&referenced, names),
        },
    }))
}

/// What a sealed value of `key` types of `others`' private addresses. Its value is
/// never shown.
///
/// # Errors
/// Returns `internal` for a Service whose name is corrupt.
pub(crate) fn in_sealed(
    plaintext: &str,
    key: &VariableKey,
    others: &[&SavedServiceIntent],
) -> Result<Option<Typed>, RpcError> {
    Ok(
        services(&hosts(plaintext, key, others))?.map(|services| Typed {
            services,
            instead: Instead::Sealed,
        }),
    )
}

/// Where `text` types a private address of one of `others`: `NAME.internal`
/// anywhere, or a bare `NAME` where only a host can stand, a URL's host or the whole
/// value of a key ending in HOST, HOSTNAME, ADDR or ADDRESS.
fn hosts<'env>(
    text: &str,
    key: &VariableKey,
    others: &[&'env SavedServiceIntent],
) -> Vec<(Range<usize>, &'env SavedServiceIntent)> {
    let host_key = ["HOST", "HOSTNAME", "ADDR", "ADDRESS"]
        .iter()
        .any(|end| key.as_str().ends_with(end));
    let url_hosts: Vec<usize> = URL_HOST
        .captures_iter(text)
        .filter_map(|url| url.get(1))
        .map(|host| host.start())
        .collect();
    HOST.captures_iter(text)
        .filter_map(|found| {
            let (whole, host) = (found.get(0)?, found.get(1)?);
            let bare =
                url_hosts.contains(&host.start()) || (host_key && whole.range() == (0..text.len()));
            let other = others.iter().copied().find(|other| {
                let name = &other.config.private_dns;
                host.as_str().eq_ignore_ascii_case(&private_domain(name))
                    || (bare && host.as_str().eq_ignore_ascii_case(name.as_str()))
            })?;
            Some((host.range(), other))
        })
        .collect()
}

/// The Services `hosts` belong to, each once; none when there are none.
fn services(
    hosts: &[(Range<usize>, &SavedServiceIntent)],
) -> Result<Option<Vec<ServiceName>>, RpcError> {
    let mut services = Vec::new();
    for (_, other) in hosts {
        let name =
            ServiceName::parse(other.slug.as_str()).map_err(|_| error::corrupt("Service name"))?;
        if !services.contains(&name) {
            services.push(name);
        }
    }
    Ok((!services.is_empty()).then_some(services))
}

/// `text` as parts, each NUL in it the next of `refs` again.
fn unflatten(text: &str, refs: &mut impl Iterator<Item = ValuePart>, parts: &mut Vec<ValuePart>) {
    for (index, piece) in text.split('\0').enumerate() {
        if index > 0 {
            parts.extend(refs.next());
        }
        parts.push(ValuePart::Text {
            value: piece.to_owned(),
        });
    }
}
