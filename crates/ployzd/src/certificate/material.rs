use pingora::tls::pkey::PKey;
use pingora::tls::x509::{X509, X509Ref};
use ployz_core::cert::{CertValidAt, CertValidityWindow};
use ployz_core::ops::RouteHostname;
use time::PrimitiveDateTime;

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
