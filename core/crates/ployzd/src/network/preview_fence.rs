//! The preview Namespaces' chains: one per Namespace with a Container on this Server's
//! bridge, each listing that Namespace's addresses. [`PREVIEW_CHAIN`] sends a new flow
//! between two preview addresses to the destination's chain, which lets it through only from
//! the same Namespace. The chains are never written to disk: a boot restores the fence with
//! [`PREVIEW_CHAIN`] closed, and the daemon writes them again from Docker once it runs.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    sync::atomic::{AtomicBool, Ordering},
};

use ployz_core::{AddressPart, Namespace};
use sha2::{Digest as _, Sha256};

use super::{
    NetworkError, conntrack,
    firewall::{PREVIEW_CHAIN, closed_preview_chain, restore, writer, xtables},
};

/// What starts every preview Namespace's chain name.
const NAMESPACE_CHAIN: &str = "PLOYZ-PV-";

/// Whether the chains last written list a preview Namespace.
static HELD: AtomicBool = AtomicBool::new(false);

/// A chain name fits iptables' 28 characters whatever the Namespace's length.
fn chain(namespace: &Namespace) -> String {
    let digest = Sha256::digest(namespace.as_str());
    format!(
        "{NAMESPACE_CHAIN}{}",
        hex::encode(digest.get(..8).unwrap_or_default())
    )
}

/// Write each preview Namespace's chain from `sets`, its addresses on this bridge, in one
/// restore, and drop the chains of Namespaces no longer here. Then forget every tracked flow
/// between two preview addresses of this bridge that no one set holds both of, so an
/// established flow never outlives the set that let it begin, whatever the daemon knew before.
///
/// # Errors
/// Returns why iptables refused the chains or the kernel refused the flows' delete.
pub(crate) fn write_preview_sets(
    sets: BTreeMap<Namespace, BTreeSet<Ipv4Addr>>,
) -> Result<(), NetworkError> {
    let _writer = writer();
    if sets.is_empty() && !preview_sets_held() {
        return Ok(());
    }
    let mut document = format!("*filter\n:{PREVIEW_CHAIN} - [0:0]\n");
    for (namespace, addresses) in &sets {
        document.push_str(&format!(":{} - [0:0]\n", chain(namespace)));
        for address in addresses {
            document.push_str(&format!(
                "-A {PREVIEW_CHAIN} -d {address} -g {}\n",
                chain(namespace)
            ));
        }
    }
    document.push_str(&format!("-A {PREVIEW_CHAIN} -j DROP\n"));
    for (namespace, addresses) in &sets {
        let name = chain(namespace);
        for address in addresses {
            document.push_str(&format!("-A {name} -s {address} -j RETURN\n"));
        }
        document.push_str(&format!("-A {name} -j DROP\n"));
    }
    let kept: BTreeSet<String> = sets.keys().map(chain).collect();
    for stale in namespace_chains()?.difference(&kept) {
        document.push_str(&format!("-F {stale}\n-X {stale}\n"));
    }
    document.push_str("COMMIT\n");
    restore("iptables", &document)?;
    HELD.store(!sets.is_empty(), Ordering::SeqCst);
    conntrack::forget_flows(|source, destination| crosses(&sets, source, destination))
        .map_err(NetworkError::Io)
}

/// Whether a flow from `source` to `destination` runs between two preview addresses of one
/// bridge that no one of `sets` holds both of: one that no new flow between them would pass.
fn crosses(
    sets: &BTreeMap<Namespace, BTreeSet<Ipv4Addr>>,
    source: Ipv4Addr,
    destination: Ipv4Addr,
) -> bool {
    let preview =
        |address: Ipv4Addr| AddressPart::of_last_octet(address.octets()[3]) == AddressPart::Preview;
    preview(source)
        && preview(destination)
        && source.octets()[..3] == destination.octets()[..3]
        && !sets
            .values()
            .any(|set| set.contains(&source) && set.contains(&destination))
}

/// Whether the last write listed a preview Namespace, so a removal here may take an address
/// out of its set.
pub(crate) fn preview_sets_held() -> bool {
    HELD.load(Ordering::SeqCst)
}

/// Note that the chains were closed.
pub(super) fn forget_written() {
    HELD.store(false, Ordering::SeqCst);
}

/// Close [`PREVIEW_CHAIN`] and drop every preview Namespace's chain, once nothing jumps to
/// them. Called with [`writer`] held.
///
/// # Errors
/// Returns why iptables refused.
pub(super) fn remove_preview_chains() -> Result<(), NetworkError> {
    let mut document = format!("*filter\n{}", closed_preview_chain());
    for stale in namespace_chains()? {
        document.push_str(&format!("-F {stale}\n-X {stale}\n"));
    }
    document.push_str(&format!("-F {PREVIEW_CHAIN}\n-X {PREVIEW_CHAIN}\nCOMMIT\n"));
    forget_written();
    restore("iptables", &document)
}

/// The preview Namespaces' chains iptables holds now.
fn namespace_chains() -> Result<BTreeSet<String>, NetworkError> {
    let listing = xtables("iptables", &["-t", "filter", "-S"])?;
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("-N "))
        .filter(|name| name.starts_with(NAMESPACE_CHAIN))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_flows_no_new_flow_would_repeat_are_forgotten() {
        let address = |last: u8| Ipv4Addr::new(10, 210, 3, last);
        let namespace = |nonce: &str| {
            Namespace::parse(format!("pv-0123456789abcdef-01234567-{nonce}")).unwrap()
        };
        let sets = BTreeMap::from([
            (
                namespace("0000aaaa"),
                BTreeSet::from([address(130), address(131)]),
            ),
            (namespace("0000bbbb"), BTreeSet::from([address(140)])),
        ]);
        let crosses = |source, destination| crosses(&sets, source, destination);
        // Within one preview, and from a preview to Open, a signed data Container or the
        // internet: kept.
        assert!(!crosses(address(130), address(131)));
        assert!(!crosses(address(130), address(10)));
        assert!(!crosses(address(130), address(170)));
        assert!(!crosses(address(130), Ipv4Addr::new(1, 2, 3, 140)));
        // Two previews, or an address no preview holds now, such as one that moved: forgotten.
        assert!(crosses(address(130), address(140)));
        assert!(crosses(address(140), address(131)));
        assert!(crosses(address(130), address(150)));
        assert!(crosses(address(150), address(151)));
    }
}
