use std::path::{Path, PathBuf};

use pingora::tls::pkey::PKey;
use pingora::tls::x509::{X509, X509Ref};
use ployz_core::certificate::{
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CustomCertBundle,
    custom_bundle_digest,
};
use ployz_core::ids::CertId;
use ployz_core::install::{AbsoluteInstallPath, InstallSha256Digest};
use ployz_core::operation::RouteHostname;
use time::PrimitiveDateTime;

/// Validates issued PEM material and packages it as a content-addressed
/// gateway artifact. Issuance and distribution remain role orchestration.
pub fn prepare_custom_certificate(
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

pub(crate) fn certificate_material_path_for_digest(
    state_dir: &Path,
    cert_id: &CertId,
    digest: &InstallSha256Digest,
) -> PathBuf {
    state_dir
        .join("bundles")
        .join(format!("{}-{}.bundle", cert_id.as_str(), digest.as_str()))
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
pub enum CertificateMaterialError {
    #[error("certificate material path is not UTF-8: {}", path.display())]
    NonUtf8Path { path: PathBuf },
    #[error("invalid certificate material: {message}")]
    InvalidMaterial { message: String },
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
}

fn invalid_material(error: impl std::fmt::Display) -> CertificateMaterialError {
    CertificateMaterialError::InvalidMaterial {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use ployz_core::ids::CertId;
    use ployz_core::install::InstallSha256Digest;
    use ployz_core::operation::RouteHostname;
    use rcgen::generate_simple_self_signed;

    #[test]
    fn issued_material_becomes_a_valid_content_addressed_bundle() {
        let state = tempfile::tempdir().expect("state directory");
        let generated =
            generate_simple_self_signed(["api.example.com".to_owned()]).expect("certificate");

        let bundle = prepare_custom_certificate(
            state.path(),
            CertId::try_new("cert_api_example_com").expect("cert id"),
            RouteHostname::try_new("api.example.com").expect("hostname"),
            generated.cert.pem(),
            generated.signing_key.serialize_pem(),
        )
        .expect("valid bundle");

        let (digest, referenced_path) = bundle
            .active_cert()
            .bundle_ref
            .artifact_parts()
            .expect("artifact reference");
        assert_eq!(
            Path::new(referenced_path.as_str()),
            certificate_material_path_for_digest(
                state.path(),
                &bundle.active_cert().cert_id,
                &digest,
            )
        );
    }

    #[test]
    fn local_path_uses_typed_identity_and_digest_not_referenced_path() {
        let digest = InstallSha256Digest::try_new("a".repeat(64)).expect("digest");
        let cert_id = CertId::try_new("cert_example").expect("cert id");

        assert_eq!(
            certificate_material_path_for_digest(
                Path::new("/var/lib/ployz/certificates"),
                &cert_id,
                &digest,
            ),
            Path::new("/var/lib/ployz/certificates")
                .join("bundles")
                .join(format!("cert_example-{}.bundle", digest.as_str()))
        );
    }
}
