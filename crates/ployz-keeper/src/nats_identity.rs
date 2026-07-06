//! Cluster NATS identity minting for first-machine install.
//!
//! Pure generation only: a self-signed cluster CA, a server certificate
//! covering the machine's reachable names, and the install-time NKey users
//! (Controller, operator User, Join). Callers own all file I/O.

use std::fmt;
use std::net::IpAddr;

use ployz_core::nats_config::{
    MintedNatsUser, NatsCaCertificatePem, NatsServerCertificatePem, NatsServerConfigError,
    is_valid_host_syntax,
};

const CA_COMMON_NAME: &str = "ployz-cluster-ca";
const LOOPBACK_SAN: &str = "127.0.0.1";

/// Everything minted once at first-machine install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNatsIdentity {
    pub ca: NatsCaCertificatePem,
    /// The CA signing key. Secret: wrapped with the operator recovery secret
    /// before it is persisted or mirrored (ADR 0031), so a promoted core can
    /// reconstruct the issuer and self-issue its own server certificate.
    pub ca_key: NatsCaKeyPem,
    pub server_cert: NatsServerCertificate,
    pub controller: MintedNatsUser,
    pub operator: MintedNatsUser,
    pub join: MintedNatsUser,
}

/// The cluster CA private key in PEM form. Secret material: `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct NatsCaKeyPem(String);

impl NatsCaKeyPem {
    #[must_use]
    pub fn new(pem: String) -> Self {
        Self(pem)
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for NatsCaKeyPem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NatsCaKeyPem([redacted])")
    }
}

/// The server's TLS certificate plus its private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerCertificate {
    pub cert_pem: NatsServerCertificatePem,
    pub key_pem: NatsServerKeyPem,
}

/// The server TLS private key in PEM form. Secret material: `Debug`
/// output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct NatsServerKeyPem(String);

impl NatsServerKeyPem {
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for NatsServerKeyPem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NatsServerKeyPem([redacted])")
    }
}

/// The subject alternative names the server certificate must cover:
/// loopback always, plus the machine's public IP and machine hostname when
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCertificateSans {
    machine_public_ip: Option<IpAddr>,
    hostname: Option<String>,
}

impl ServerCertificateSans {
    pub fn try_new(
        machine_public_ip: Option<IpAddr>,
        hostname: Option<String>,
    ) -> Result<Self, NatsIdentityError> {
        if let Some(hostname) = &hostname
            && !is_valid_host_syntax(hostname)
        {
            return Err(NatsIdentityError::InvalidHostname {
                value: hostname.clone(),
            });
        }
        Ok(Self {
            machine_public_ip,
            hostname,
        })
    }

    #[must_use]
    pub fn subject_alt_names(&self) -> Vec<String> {
        let mut names = vec![LOOPBACK_SAN.to_owned()];
        if let Some(ip) = self.machine_public_ip {
            let rendered = ip.to_string();
            if !names.contains(&rendered) {
                names.push(rendered);
            }
        }
        if let Some(hostname) = &self.hostname
            && !names.contains(hostname)
        {
            names.push(hostname.clone());
        }
        names
    }
}

pub fn generate_cluster_nats_identity(
    sans: &ServerCertificateSans,
) -> Result<ClusterNatsIdentity, NatsIdentityError> {
    let ca_key = rcgen::KeyPair::generate().map_err(certificate_error)?;
    let ca_params = ca_certificate_params()?;
    let ca_certificate = ca_params
        .clone()
        .self_signed(&ca_key)
        .map_err(certificate_error)?;
    let ca = NatsCaCertificatePem::try_new(ca_certificate.pem())
        .map_err(NatsIdentityError::InvalidGeneratedMaterial)?;

    // Capture the CA key PEM before it moves into the issuer; it is persisted
    // (wrapped with the recovery secret) so a future promotion can rebuild the
    // issuer and self-issue its own server certificate (ADR 0031).
    let ca_key_pem = ca_key.serialize_pem();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let server_cert = sign_server_certificate(&issuer, sans)?;

    Ok(ClusterNatsIdentity {
        ca,
        ca_key: NatsCaKeyPem::new(ca_key_pem),
        server_cert,
        controller: mint_nkey_user()?,
        operator: mint_nkey_user()?,
        join: mint_nkey_user()?,
    })
}

/// Re-issue a server certificate from the persisted CA key (a promotion
/// self-issuing for its own address). The CA key PEM comes from unwrapping the
/// mirrored recovery material; the CA's issuer parameters are the fixed
/// [`ca_certificate_params`], so the reconstructed issuer signs under the same
/// CN the existing CA cert carries — no cert parsing needed.
pub fn issue_server_certificate(
    ca_key_pem: &str,
    sans: &ServerCertificateSans,
) -> Result<NatsServerCertificate, NatsIdentityError> {
    let ca_key = rcgen::KeyPair::from_pem(ca_key_pem).map_err(certificate_error)?;
    let issuer = rcgen::Issuer::new(ca_certificate_params()?, ca_key);
    sign_server_certificate(&issuer, sans)
}

/// The cluster CA's certificate parameters — an unconstrained CA under a fixed
/// common name. Shared by first-machine generation and promotion re-issue so the
/// reconstructed issuer always matches the CA cert the fleet already trusts.
fn ca_certificate_params() -> Result<rcgen::CertificateParams, NatsIdentityError> {
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(certificate_error)?;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, CA_COMMON_NAME);
    Ok(params)
}

fn sign_server_certificate<S: rcgen::SigningKey>(
    issuer: &rcgen::Issuer<'_, S>,
    sans: &ServerCertificateSans,
) -> Result<NatsServerCertificate, NatsIdentityError> {
    let server_key = rcgen::KeyPair::generate().map_err(certificate_error)?;
    let server_params =
        rcgen::CertificateParams::new(sans.subject_alt_names()).map_err(certificate_error)?;
    let server_certificate = server_params
        .signed_by(&server_key, issuer)
        .map_err(certificate_error)?;
    Ok(NatsServerCertificate {
        cert_pem: NatsServerCertificatePem::try_new(server_certificate.pem())
            .map_err(NatsIdentityError::InvalidGeneratedMaterial)?,
        key_pem: NatsServerKeyPem(server_key.serialize_pem()),
    })
}

fn mint_nkey_user() -> Result<MintedNatsUser, NatsIdentityError> {
    MintedNatsUser::generate().map_err(NatsIdentityError::InvalidGeneratedMaterial)
}

fn certificate_error(error: rcgen::Error) -> NatsIdentityError {
    NatsIdentityError::CertificateGeneration {
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsIdentityError {
    #[error("machine hostname {value:?} is not a valid certificate host name")]
    InvalidHostname { value: String },
    #[error("failed to generate cluster TLS material: {message}")]
    CertificateGeneration { message: String },
    #[error("generated NATS material is invalid: {0}")]
    InvalidGeneratedMaterial(NatsServerConfigError),
}

#[cfg(test)]
mod tests {
    use super::{
        ClusterNatsIdentity, NatsIdentityError, ServerCertificateSans,
        generate_cluster_nats_identity, issue_server_certificate,
    };

    #[test]
    fn server_certificate_sans_cover_loopback_public_ip_and_hostname() {
        let sans = ServerCertificateSans::try_new(
            Some("203.0.113.10".parse().expect("valid IP")),
            Some("core-1.example.test".to_owned()),
        )
        .expect("valid SAN inputs");

        assert_eq!(
            sans.subject_alt_names(),
            vec![
                "127.0.0.1".to_owned(),
                "203.0.113.10".to_owned(),
                "core-1.example.test".to_owned(),
            ]
        );
    }

    #[test]
    fn server_certificate_sans_always_cover_loopback() {
        let sans = ServerCertificateSans::try_new(None, None).expect("valid SAN inputs");

        assert_eq!(sans.subject_alt_names(), vec!["127.0.0.1".to_owned()]);
    }

    #[test]
    fn server_certificate_sans_reject_invalid_hostnames() {
        assert_eq!(
            ServerCertificateSans::try_new(None, Some("bad host".to_owned())),
            Err(NatsIdentityError::InvalidHostname {
                value: "bad host".to_owned(),
            })
        );
    }

    #[test]
    fn generated_identity_round_trips_pem_and_seed_shapes() {
        let sans = ServerCertificateSans::try_new(
            Some("203.0.113.10".parse().expect("valid IP")),
            Some("core-1".to_owned()),
        )
        .expect("valid SAN inputs");

        let identity = generate_cluster_nats_identity(&sans).expect("identity generates");
        let ClusterNatsIdentity {
            ca,
            ca_key,
            server_cert,
            controller,
            operator,
            join,
        } = &identity;

        assert!(
            ca.as_str()
                .trim_start()
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
        assert!(
            ca_key
                .secret()
                .trim_start()
                .starts_with("-----BEGIN PRIVATE KEY-----")
        );
        assert!(
            server_cert
                .cert_pem
                .as_str()
                .trim_start()
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
        assert!(
            server_cert
                .key_pem
                .secret()
                .trim_start()
                .starts_with("-----BEGIN PRIVATE KEY-----")
        );
        for user in [controller, operator, join] {
            assert!(user.public.as_str().starts_with('U'));
            assert!(user.seed.secret().starts_with("SU"));
        }
        assert_ne!(controller.seed, operator.seed);
        assert_ne!(controller.seed, join.seed);
        assert_eq!(
            format!("{:?}", server_cert.key_pem),
            "NatsServerKeyPem([redacted])"
        );
    }

    #[test]
    fn issue_server_certificate_reissues_from_the_persisted_ca_key() {
        let identity = generate_cluster_nats_identity(
            &ServerCertificateSans::try_new(None, None).expect("sans"),
        )
        .expect("identity generates");

        // A promoted core self-issues a cert for its own address from the CA key.
        let reissued = issue_server_certificate(
            identity.ca_key.secret(),
            &ServerCertificateSans::try_new(Some("203.0.113.99".parse().expect("ip")), None)
                .expect("sans"),
        )
        .expect("reissue succeeds");

        assert!(
            reissued
                .cert_pem
                .as_str()
                .trim_start()
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
        // A fresh server key each time, distinct from the first-machine cert's.
        assert_ne!(
            reissued.key_pem.secret(),
            identity.server_cert.key_pem.secret()
        );
    }
}
