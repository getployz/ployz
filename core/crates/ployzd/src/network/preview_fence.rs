//! The preview Namespaces' chains: one per Namespace with a Container on this Server's
//! bridge, each listing that Namespace's addresses. [`PREVIEW_CHAIN`] sends each packet
//! between two preview addresses to the destination's chain, which marks it only from the
//! same Namespace. Whatever none marks, an emptied chain's included, meets the fence's drop.
//! The chains are never written to disk: a boot restores the fence with [`PREVIEW_CHAIN`]
//! closed, and the daemon writes them again from Docker once it runs.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

use ployz_core::Namespace;
use sha2::{Digest as _, Sha256};

use super::{
    NetworkError,
    firewall::{
        PREVIEW_CHAIN, PREVIEW_MARK, closed_preview_chain, command_succeeds, restore, writer,
        xtables,
    },
};

/// What starts every preview Namespace's chain name.
const NAMESPACE_CHAIN: &str = "PLOYZ-PV-";

/// A chain name fits iptables' 28 characters whatever the Namespace's length.
fn chain(namespace: &Namespace) -> String {
    let digest = Sha256::digest(namespace.as_str());
    format!(
        "{NAMESPACE_CHAIN}{}",
        hex::encode(digest.get(..8).unwrap_or_default())
    )
}

/// Write each preview Namespace's chain from `sets`, its addresses on this bridge, in one
/// restore, and drop the chains of Namespaces no longer here. A flow between two addresses
/// no one set holds now stops at its next packet, since the fence reads every packet.
///
/// # Errors
/// Returns why iptables refused the chains.
pub fn write_preview_sets(
    sets: BTreeMap<Namespace, BTreeSet<Ipv4Addr>>,
) -> Result<(), NetworkError> {
    let _writer = writer();
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
            document.push_str(&format!(
                "-A {name} -s {address} -j MARK --set-xmark {PREVIEW_MARK}/{PREVIEW_MARK}\n"
            ));
        }
    }
    let kept: BTreeSet<String> = sets.keys().map(chain).collect();
    for stale in namespace_chains()?.difference(&kept) {
        document.push_str(&format!("-F {stale}\n-X {stale}\n"));
    }
    document.push_str("COMMIT\n");
    restore("iptables", &document)
}

/// Close [`PREVIEW_CHAIN`] and drop every preview Namespace's chain, once nothing jumps to
/// them. With [`PREVIEW_CHAIN`] gone there is nothing to take down: only it jumps to them.
/// Called with [`writer`] held.
///
/// # Errors
/// Returns why iptables refused.
pub(super) fn remove_preview_chains() -> Result<(), NetworkError> {
    if !command_succeeds("iptables", &["-t", "filter", "-n", "-L", PREVIEW_CHAIN]) {
        return Ok(());
    }
    let mut document = format!("*filter\n{}", closed_preview_chain());
    for stale in namespace_chains()? {
        document.push_str(&format!("-F {stale}\n-X {stale}\n"));
    }
    document.push_str(&format!("-F {PREVIEW_CHAIN}\n-X {PREVIEW_CHAIN}\nCOMMIT\n"));
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
