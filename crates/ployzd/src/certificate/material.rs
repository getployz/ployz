use std::path::{Path, PathBuf};

use pingora::tls::pkey::PKey;
use pingora::tls::x509::{X509, X509Ref};
use ployz_core::cert::{
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CustomCertBundle,
    ManagedCertBundle, custom_bundle_digest,
};
use ployz_core::ids::CertId;
use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
use ployz_core::install::{AbsoluteInstallPath, InstallSha256Digest};
use ployz_core::ops::RouteHostname;
use time::PrimitiveDateTime;

use crate::adapters::atomic_file::{
    restrict_secret_file_permissions, write_secret_file_atomically,
};

pub(crate) fn prepare_custom_certificate(
    state_dir: &Path,
    cert_id: CertId,
    hostname: RouteHostname,
    certificate_chain_pem: String,
    private_key_pem: String,
) -> Result<CustomCertBundle, CertificateMaterialError> {
    let validity = validate_and_read_validity(&certificate_chain_pem, &private_key_pem, &hostname)?;
    let digest =
        custom_bundle_digest(&certificate_chain_pem, &private_key_pem).map_err(invalid_material)?;
    let path = certificate_material_path_for_digest(state_dir, &cert_id, &digest);
    let Some(path_text) = path.to_str() else {
        return Err(CertificateMaterialError::NonUtf8Path { path });
    };
    let path = AbsoluteInstallPath::try_new(path_text).map_err(invalid_material)?;
    let bundle_ref = CertBundleRef::for_bundle(&digest, &path).map_err(invalid_material)?;
    CustomCertBundle::try_new(
        ActiveCertState {
            cert_id,
            hostname,
            bundle_ref,
            validity,
        },
        certificate_chain_pem,
        private_key_pem,
    )
    .map_err(invalid_material)
}

/// Adapts Worker-issued wildcard material into the same validated artifact
/// path used by exact-route certificates.
pub(crate) fn prepare_ployz_wildcard_certificate(
    state_dir: &Path,
    bundle: ManagedCertBundle,
) -> Result<(ActiveCertificateMetadata, CustomCertBundle), CertificateMaterialError> {
    let hostname = RouteHostname::try_new(bundle.dns_names[1].clone()).map_err(invalid_material)?;
    let cert_id = CertId::try_new(format!("cert_ployz_{}", bundle.lease.as_str()))
        .map_err(invalid_material)?;
    let material = prepare_custom_certificate(
        state_dir,
        cert_id,
        hostname,
        bundle.certificate_chain_pem,
        bundle.private_key_pem,
    )?;
    Ok((
        ActiveCertificateMetadata {
            owner: CertificateOwner::PloyzAutomaticNamespace,
            active: material.active_cert().clone(),
        },
        material,
    ))
}

pub(crate) fn write_custom_certificate(
    state_dir: &Path,
    bundle: &CustomCertBundle,
) -> Result<(), CertificateMaterialError> {
    let path = custom_certificate_material_path(state_dir, bundle.active_cert())?;
    write_secret_file_atomically(&path, &bundle.material_bytes()).map_err(|error| {
        CertificateMaterialError::ArtifactFile {
            path,
            message: error.to_string(),
        }
    })
}

pub(crate) fn load_custom_certificate(
    state_dir: &Path,
    active_cert: &ActiveCertState,
) -> Result<CustomCertBundle, CertificateMaterialError> {
    let path = custom_certificate_material_path(state_dir, active_cert)?;
    restrict_secret_file_permissions(&path).map_err(|error| {
        CertificateMaterialError::ArtifactFile {
            path: path.clone(),
            message: error.to_string(),
        }
    })?;
    let material =
        std::fs::read(&path).map_err(|error| CertificateMaterialError::ArtifactFile {
            path,
            message: error.to_string(),
        })?;
    let bundle = CustomCertBundle::try_from_material_bytes(active_cert.clone(), &material)
        .map_err(invalid_material)?;
    validate_custom_certificate(&bundle)?;
    Ok(bundle)
}

pub(crate) fn custom_certificate_material_path(
    state_dir: &Path,
    active_cert: &ActiveCertState,
) -> Result<PathBuf, CertificateMaterialError> {
    let (digest, _) = active_cert
        .bundle_ref
        .artifact_parts()
        .map_err(invalid_material)?;
    Ok(certificate_material_path_for_digest(
        state_dir,
        &active_cert.cert_id,
        &digest,
    ))
}

pub(crate) fn certificate_material_path_for_digest(
    state_dir: &Path,
    cert_id: &CertId,
    digest: &InstallSha256Digest,
) -> PathBuf {
    state_dir
        .join("bundles")
        .join(format!("{}-{}.bundle", cert_id.as_str(), digest.as_str()))
}

pub(crate) fn validate_custom_certificate(
    bundle: &CustomCertBundle,
) -> Result<(), CertificateMaterialError> {
    let active = bundle.active_cert();
    let actual = validate_and_read_validity(
        bundle.certificate_chain_pem(),
        bundle.private_key_pem(),
        &active.hostname,
    )?;
    if actual != active.validity {
        return Err(CertificateMaterialError::ValidityMismatch {
            expected: active.validity,
            actual,
        });
    }
    Ok(())
}

pub(crate) fn validate_custom_certificate_for_activation(
    active: &ActiveCertState,
    now_seconds: u64,
) -> Result<(), CertificateMaterialError> {
    if !active.is_usable_at(now_seconds) {
        return Err(CertificateMaterialError::NotActivationEligible {
            now_seconds,
            not_before: active.validity.not_before.unix_seconds(),
            not_after: active.validity.not_after.unix_seconds(),
        });
    }
    Ok(())
}

pub(crate) fn validate_and_read_validity(
    certificate_chain_pem: &str,
    private_key_pem: &str,
    hostname: &RouteHostname,
) -> Result<CertValidityWindow, CertificateMaterialError> {
    let certificates = X509::stack_from_pem(certificate_chain_pem.as_bytes()).map_err(|error| {
        CertificateMaterialError::InvalidChain {
            message: error.to_string(),
        }
    })?;
    let Some(leaf) = certificates.first() else {
        return Err(CertificateMaterialError::EmptyChain);
    };
    let private_key = PKey::private_key_from_pem(private_key_pem.as_bytes()).map_err(|error| {
        CertificateMaterialError::InvalidPrivateKey {
            message: error.to_string(),
        }
    })?;
    let public_key = leaf
        .public_key()
        .map_err(|error| CertificateMaterialError::InvalidChain {
            message: error.to_string(),
        })?;
    if !public_key.public_eq(&private_key) {
        return Err(CertificateMaterialError::KeyMismatch);
    }
    let covers_hostname = leaf.subject_alt_names().is_some_and(|names| {
        names.iter().any(|name| {
            name.dnsname()
                .is_some_and(|dns_name| dns_name_covers(dns_name, hostname.as_str()))
        })
    });
    if !covers_hostname {
        return Err(CertificateMaterialError::HostnameMismatch {
            hostname: hostname.clone(),
        });
    }

    CertValidityWindow::try_new(
        CertValidAt::try_new(asn1_unix_seconds(leaf, ValidityBound::NotBefore)?).map_err(
            |error| CertificateMaterialError::InvalidValidity {
                message: error.to_string(),
            },
        )?,
        CertValidAt::try_new(asn1_unix_seconds(leaf, ValidityBound::NotAfter)?).map_err(
            |error| CertificateMaterialError::InvalidValidity {
                message: error.to_string(),
            },
        )?,
    )
    .map_err(|error| CertificateMaterialError::InvalidValidity {
        message: error.to_string(),
    })
}

fn dns_name_covers(dns_name: &str, hostname: &str) -> bool {
    if dns_name.eq_ignore_ascii_case(hostname) {
        return true;
    }
    let Some(suffix) = dns_name.strip_prefix("*.") else {
        return false;
    };
    let Some(label) = hostname
        .strip_suffix(suffix)
        .and_then(|prefix| prefix.strip_suffix('.'))
    else {
        return false;
    };
    !label.is_empty() && !label.contains('.')
}

enum ValidityBound {
    NotBefore,
    NotAfter,
}

fn asn1_unix_seconds(
    certificate: &X509Ref,
    bound: ValidityBound,
) -> Result<u64, CertificateMaterialError> {
    let rendered = match bound {
        ValidityBound::NotBefore => certificate.not_before().to_string(),
        ValidityBound::NotAfter => certificate.not_after().to_string(),
    };
    let Some(timestamp) = rendered.strip_suffix(" GMT") else {
        return Err(CertificateMaterialError::InvalidValidity { message: rendered });
    };
    let format = time::format_description::parse(
        "[month repr:short] [day padding:space] [hour]:[minute]:[second] [year]",
    )
    .map_err(|error| CertificateMaterialError::InvalidValidity {
        message: error.to_string(),
    })?;
    let timestamp = PrimitiveDateTime::parse(timestamp, &format).map_err(|error| {
        CertificateMaterialError::InvalidValidity {
            message: error.to_string(),
        }
    })?;
    u64::try_from(timestamp.assume_utc().unix_timestamp()).map_err(|error| {
        CertificateMaterialError::InvalidValidity {
            message: error.to_string(),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CertificateMaterialError {
    #[error("certificate material path is not UTF-8: {}", path.display())]
    NonUtf8Path { path: PathBuf },
    #[error("invalid certificate material: {message}")]
    InvalidMaterial { message: String },
    #[error("certificate material file {}: {message}", path.display())]
    ArtifactFile { path: PathBuf, message: String },
    #[error("certificate chain is empty")]
    EmptyChain,
    #[error("invalid certificate chain: {message}")]
    InvalidChain { message: String },
    #[error("invalid certificate private key: {message}")]
    InvalidPrivateKey { message: String },
    #[error("certificate does not match its private key")]
    KeyMismatch,
    #[error(
        "certificate does not cover requested hostname {}",
        hostname.as_str()
    )]
    HostnameMismatch { hostname: RouteHostname },
    #[error("invalid certificate validity: {message}")]
    InvalidValidity { message: String },
    #[error(
        "certificate validity differs from active metadata: expected {expected:?}, actual {actual:?}"
    )]
    ValidityMismatch {
        expected: CertValidityWindow,
        actual: CertValidityWindow,
    },
    #[error(
        "certificate is not eligible for activation at {now_seconds}; validity is {not_before} through {not_after}"
    )]
    NotActivationEligible {
        now_seconds: u64,
        not_before: u64,
        not_after: u64,
    },
}

fn invalid_material(error: impl std::fmt::Display) -> CertificateMaterialError {
    CertificateMaterialError::InvalidMaterial {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ployz_core::cert::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, LeaseBearerToken,
        ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
    };
    use ployz_core::ids::CertId;
    use ployz_core::ops::RouteHostname;
    use ployz_test_lease_worker::{LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker};

    use super::*;

    #[test]
    fn local_path_uses_typed_identity_and_digest_not_referenced_path() {
        let digest = "a".repeat(64);
        let active = ActiveCertState {
            cert_id: CertId::try_new("cert_example").expect("cert id"),
            hostname: RouteHostname::try_new("example.com").expect("hostname"),
            bundle_ref: CertBundleRef::try_new(format!("sha256:{digest}:/foreign/forged.bundle"))
                .expect("bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1_000).expect("not before"),
                CertValidAt::try_new(2_000).expect("not after"),
            )
            .expect("validity"),
        };

        assert_eq!(
            custom_certificate_material_path(Path::new("/var/lib/ployz/certificates"), &active)
                .expect("local path"),
            Path::new("/var/lib/ployz/certificates")
                .join("bundles")
                .join(format!("cert_example-{digest}.bundle"))
        );
    }

    #[test]
    fn worker_wildcard_adapts_to_namespace_owned_gateway_material() {
        let mut worker = StubLeaseWorker::new();
        let LeaseWorkerResponse::LeaseAcquired(acquired) = worker
            .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
                acquisition_id: ManagedLeaseAcquisitionId::try_new("a1").expect("acquisition id"),
                token: LeaseBearerToken::try_new("token").expect("token"),
                ipv4: Vec::new(),
                ipv6: Vec::new(),
            }))
            .expect("acquire")
        else {
            panic!("acquire response");
        };
        let _ = worker
            .handle(LeaseWorkerRequest::DownloadBundle {
                lease: acquired.lease.name.clone(),
                token: acquired.lease.token.clone(),
            })
            .expect("pending download");
        let LeaseWorkerResponse::Bundle(bundle) = worker
            .handle(LeaseWorkerRequest::DownloadBundle {
                lease: acquired.lease.name.clone(),
                token: acquired.lease.token,
            })
            .expect("bundle download")
        else {
            panic!("ready bundle");
        };
        let state = tempfile::tempdir().expect("state directory");

        let (metadata, material) =
            prepare_ployz_wildcard_certificate(state.path(), bundle).expect("valid wildcard");

        assert_eq!(metadata.owner, CertificateOwner::PloyzAutomaticNamespace);
        assert_eq!(metadata.active, *material.active_cert());
        assert_eq!(
            metadata.active.hostname.as_str(),
            acquired.lease.name.hostname_suffix()
        );
        validate_custom_certificate(&material).expect("gateway material validates");
    }
}
