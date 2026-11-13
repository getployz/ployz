//! The certificate files Caddy reads from the config directory.

use ployz_core::IngressHost;
use std::{collections::BTreeSet, fs, io, path::Path};

use super::projection::IngressSite;
use crate::{corrosion::CertificateMaterial, filesystem::atomic_write};

const CERTS_DIR: &str = "certs";

pub(crate) fn write_certificate_files(
    config_file: &Path,
    sites: &[IngressSite],
    key_mode: u32,
    prepare: fn(&Path) -> io::Result<()>,
    set_group: fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = certificate_directory(config_file)?;
    prepare(&directory)?;
    for site in sites {
        let Some(material) = site.pinned() else {
            continue;
        };
        let stem = certificate_file_stem(&site.hostname, material);
        let cert_path = directory.join(format!("{stem}.crt"));
        let key_path = directory.join(format!("{stem}.key"));
        atomic_write(&cert_path, material.certificate().as_bytes(), 0o644)?;
        set_group(&cert_path)?;
        atomic_write(&key_path, material.private_key().as_bytes(), key_mode)?;
        set_group(&key_path)?;
    }
    Ok(())
}

pub(crate) fn remove_stale_certificate_files(
    config_file: &Path,
    sites: &[IngressSite],
) -> io::Result<()> {
    let directory = certificate_directory(config_file)?;
    if !directory.exists() {
        return Ok(());
    }
    let keep: BTreeSet<String> = sites
        .iter()
        .filter_map(|site| {
            site.pinned().map(|material| {
                let stem = certificate_file_stem(&site.hostname, material);
                [format!("{stem}.crt"), format!("{stem}.key")]
            })
        })
        .flatten()
        .collect();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !keep.contains(name) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn certificate_directory(config_file: &Path) -> io::Result<std::path::PathBuf> {
    Ok(config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?
        .join(CERTS_DIR))
}

/// Stable filename stem shared by certificate writing and renderers. It is a fixed-length
/// digest of the hostname and material, so a 253-character hostname stays under NAME_MAX.
#[must_use]
pub(crate) fn certificate_file_stem(
    hostname: &IngressHost,
    material: &CertificateMaterial,
) -> String {
    use hex::encode as hex_encode;
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update((hostname.as_str().len() as u64).to_le_bytes());
    digest.update(hostname.as_str().as_bytes());
    digest.update((material.certificate().len() as u64).to_le_bytes());
    digest.update(material.certificate().as_bytes());
    digest.update((material.private_key().len() as u64).to_le_bytes());
    digest.update(material.private_key().as_bytes());
    hex_encode(digest.finalize())
}
