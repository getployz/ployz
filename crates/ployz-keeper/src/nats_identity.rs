//! Cluster NATS identity minting for first-node install.
//!
//! Pure generation only: a self-signed cluster CA, a server certificate
//! covering the machine's reachable names, and the install-time NKey users
//! (Controller, operator User, Join). Callers own all file I/O.

use std::fmt;
use std::net::IpAddr;

use ployz_core::nats_config::{
    NatsCaCertificatePem, NatsServerCertificatePem, NatsServerConfigError, NatsUserPublicKey,
    NatsUserSeed, is_valid_host_syntax,
};

const CA_COMMON_NAME: &str = "ployz-cluster-ca";
const LOOPBACK_SAN: &str = "127.0.0.1";

/// Everything minted once at first-node install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNatsIdentity {
    pub ca: NatsCaCertificatePem,
    pub server_cert: NatsServerCertificate,
    pub controller: MintedNatsUser,
    pub operator: MintedNatsUser,
    pub join: MintedNatsUser,
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

/// One freshly minted NKey user: public key for the authorization file,
/// seed for the owning machine's `0600` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedNatsUser {
    pub public: NatsUserPublicKey,
    pub seed: NatsUserSeed,
}

/// The subject alternative names the server certificate must cover:
/// loopback always, plus the node's public IP and machine hostname when
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCertificateSans {
    node_public_ip: Option<IpAddr>,
    hostname: Option<String>,
}

impl ServerCertificateSans {
    pub fn try_new(
        node_public_ip: Option<IpAddr>,
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
            node_public_ip,
            hostname,
        })
    }

    #[must_use]
    pub fn subject_alt_names(&self) -> Vec<String> {
        let mut names = vec![LOOPBACK_SAN.to_owned()];
        if let Some(ip) = self.node_public_ip {
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
    let mut ca_params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(certificate_error)?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, CA_COMMON_NAME);
    let ca_certificate = ca_params
        .clone()
        .self_signed(&ca_key)
        .map_err(certificate_error)?;
    let ca = NatsCaCertificatePem::try_new(ca_certificate.pem())
        .map_err(NatsIdentityError::InvalidGeneratedMaterial)?;

    let server_key = rcgen::KeyPair::generate().map_err(certificate_error)?;
    let server_params =
        rcgen::CertificateParams::new(sans.subject_alt_names()).map_err(certificate_error)?;
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let server_certificate = server_params
        .signed_by(&server_key, &issuer)
        .map_err(certificate_error)?;

    Ok(ClusterNatsIdentity {
        ca,
        server_cert: NatsServerCertificate {
            cert_pem: NatsServerCertificatePem::try_new(server_certificate.pem())
                .map_err(NatsIdentityError::InvalidGeneratedMaterial)?,
            key_pem: NatsServerKeyPem(server_key.serialize_pem()),
        },
        controller: mint_nkey_user()?,
        operator: mint_nkey_user()?,
        join: mint_nkey_user()?,
    })
}

fn mint_nkey_user() -> Result<MintedNatsUser, NatsIdentityError> {
    let pair = nkeys::KeyPair::new_user();
    let seed = pair
        .seed()
        .map_err(|error| NatsIdentityError::NkeyGeneration {
            message: error.to_string(),
        })?;
    Ok(MintedNatsUser {
        public: NatsUserPublicKey::try_new(pair.public_key())
            .map_err(NatsIdentityError::InvalidGeneratedMaterial)?,
        seed: NatsUserSeed::try_new(seed).map_err(NatsIdentityError::InvalidGeneratedMaterial)?,
    })
}

fn certificate_error(error: rcgen::Error) -> NatsIdentityError {
    NatsIdentityError::CertificateGeneration {
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsIdentityError {
    InvalidHostname { value: String },
    CertificateGeneration { message: String },
    NkeyGeneration { message: String },
    InvalidGeneratedMaterial(NatsServerConfigError),
}

impl fmt::Display for NatsIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHostname { value } => {
                write!(
                    formatter,
                    "machine hostname {value:?} is not a valid certificate host name"
                )
            }
            Self::CertificateGeneration { message } => {
                write!(
                    formatter,
                    "failed to generate cluster TLS material: {message}"
                )
            }
            Self::NkeyGeneration { message } => {
                write!(formatter, "failed to generate NKey user: {message}")
            }
            Self::InvalidGeneratedMaterial(error) => {
                write!(formatter, "generated NATS material is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for NatsIdentityError {}

#[cfg(test)]
mod tests {
    use super::{
        ClusterNatsIdentity, NatsIdentityError, ServerCertificateSans,
        generate_cluster_nats_identity,
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
}
