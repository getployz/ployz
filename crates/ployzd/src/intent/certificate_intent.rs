use std::path::{Path, PathBuf};

use ployz_core::cert::{
    AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertValidityWindow, CustomCertBundle,
    custom_bundle_digest,
};
use ployz_core::ids::CertId;
use ployz_core::install::AbsoluteInstallPath;
use ployz_core::ops::RouteHostname;
use rusqlite::{OptionalExtension, params};

use crate::adapters::atomic_file::write_file_atomically;
use crate::certificate::material::validate_and_read_validity;
use crate::core_store::{CoreStore, CoreStoreError, query_json, query_json_list, to_json};

#[derive(Debug, Clone)]
pub struct CertificateIntentStore {
    store: CoreStore,
    state_dir: PathBuf,
}

impl CertificateIntentStore {
    #[must_use]
    pub fn new(store: CoreStore, state_dir: PathBuf) -> Self {
        Self { store, state_dir }
    }

    #[must_use]
    pub fn reader(store: CoreStore) -> Self {
        Self {
            store,
            state_dir: PathBuf::new(),
        }
    }

    pub async fn active_for_hostname(
        &self,
        hostname: &RouteHostname,
    ) -> Result<Option<CustomCertBundle>, CertificateIntentStoreError> {
        let hostname = hostname.as_str().to_owned();
        self.store
            .call(move |conn| {
                query_json(
                    conn,
                    "SELECT json FROM custom_certificate_intent WHERE hostname = ?1",
                    &hostname,
                )
            })
            .await
            .map_err(store_error)
    }

    pub async fn active_for_cert_id(
        &self,
        cert_id: &CertId,
    ) -> Result<Option<CustomCertBundle>, CertificateIntentStoreError> {
        let cert_id = cert_id.clone();
        self.store
            .call(move |conn| {
                let bundles: Vec<CustomCertBundle> = query_json_list(
                    conn,
                    "SELECT json FROM custom_certificate_intent ORDER BY hostname",
                )?;
                Ok(bundles
                    .into_iter()
                    .find(|bundle| bundle.active_cert.cert_id == cert_id))
            })
            .await
            .map_err(store_error)
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<CustomCertBundle>, CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM custom_certificate_intent ORDER BY hostname",
                )
            })
            .await
            .map_err(store_error)
    }

    pub async fn commit_active(
        &self,
        cert_id: CertId,
        hostname: RouteHostname,
        validity: CertValidityWindow,
        certificate_chain_pem: String,
        private_key_pem: String,
    ) -> Result<CustomCertBundle, CertificateIntentStoreError> {
        let bundle = self.prepare_active(
            cert_id,
            hostname,
            validity,
            certificate_chain_pem,
            private_key_pem,
        )?;
        self.commit_prepared(bundle.clone()).await?;
        Ok(bundle)
    }

    pub(crate) fn prepare_active(
        &self,
        cert_id: CertId,
        hostname: RouteHostname,
        validity: CertValidityWindow,
        certificate_chain_pem: String,
        private_key_pem: String,
    ) -> Result<CustomCertBundle, CertificateIntentStoreError> {
        let material_validity =
            validate_and_read_validity(&certificate_chain_pem, &private_key_pem, &hostname)
                .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;
        if material_validity != validity {
            return Err(CertificateIntentStoreError::ValidityMismatch {
                expected: validity,
                actual: material_validity,
            });
        }

        let digest =
            custom_bundle_digest(&certificate_chain_pem, &private_key_pem).map_err(|error| {
                CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                }
            })?;
        let bundle_path = self.state_dir.join("bundles").join(format!(
            "{}-{}.bundle",
            cert_id.as_str(),
            digest.as_str()
        ));
        let bundle_path_text =
            bundle_path
                .to_str()
                .ok_or_else(|| CertificateIntentStoreError::NonUtf8BundlePath {
                    path: bundle_path.clone(),
                })?;
        let bundle_ref = CertBundleRef::for_bundle(
            &digest,
            &AbsoluteInstallPath::try_new(bundle_path_text).map_err(|error| {
                CertificateIntentStoreError::InvalidBundlePath {
                    message: error.to_string(),
                }
            })?,
        )
        .map_err(|error| CertificateIntentStoreError::InvalidBundlePath {
            message: error.to_string(),
        })?;
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
        .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
            message: error.to_string(),
        })
    }

    pub(crate) async fn commit_prepared(
        &self,
        bundle: CustomCertBundle,
    ) -> Result<(), CertificateIntentStoreError> {
        CustomCertBundle::try_new(
            bundle.active_cert.clone(),
            bundle.certificate_chain_pem.clone(),
            bundle.private_key_pem.clone(),
        )
        .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
            message: error.to_string(),
        })?;
        let (_, path) = bundle
            .active_cert
            .bundle_ref
            .artifact_parts()
            .map_err(|error| CertificateIntentStoreError::InvalidBundlePath {
                message: error.to_string(),
            })?;
        let bundle_path = PathBuf::from(path.as_str());
        let bundle_directory = self.state_dir.join("bundles");
        if bundle_path.parent() != Some(bundle_directory.as_path()) {
            return Err(CertificateIntentStoreError::InvalidBundlePath {
                message: format!(
                    "{} is outside {}",
                    bundle_path.display(),
                    bundle_directory.display()
                ),
            });
        }
        write_secret_bundle(&bundle_path, &bundle.material_bytes())?;
        let stored = bundle.clone();
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO custom_certificate_intent (hostname, json) VALUES (?1, ?2)
                     ON CONFLICT(hostname) DO UPDATE SET json = excluded.json",
                    params![stored.active_cert.hostname.as_str(), to_json(&stored)?,],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)?;
        Ok(())
    }

    pub async fn store_challenge(
        &self,
        challenge: AcmeHttp01Challenge,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO acme_http01_challenges (hostname, token, json)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(hostname, token) DO UPDATE SET json = excluded.json",
                    params![
                        challenge.hostname().as_str(),
                        challenge.token().as_str(),
                        to_json(&challenge)?,
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn challenges(
        &self,
    ) -> Result<Vec<AcmeHttp01Challenge>, CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM acme_http01_challenges ORDER BY hostname, token",
                )
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_challenges_for_hostname(
        &self,
        hostname: &RouteHostname,
    ) -> Result<(), CertificateIntentStoreError> {
        let hostname = hostname.as_str().to_owned();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM acme_http01_challenges WHERE hostname = ?1",
                    [hostname],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_all_challenges(&self) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                conn.execute("DELETE FROM acme_http01_challenges", [])?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn account_credentials(
        &self,
        directory_url: &str,
    ) -> Result<Option<String>, CertificateIntentStoreError> {
        let directory_url = directory_url.to_owned();
        self.store
            .call(move |conn| {
                conn.query_row(
                    "SELECT credentials_json FROM acme_accounts WHERE directory_url = ?1",
                    [directory_url],
                    |row| row.get(0),
                )
                .optional()
            })
            .await
            .map_err(store_error)
    }

    pub async fn store_account_credentials(
        &self,
        directory_url: String,
        credentials_json: String,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO acme_accounts (directory_url, credentials_json) VALUES (?1, ?2)
                     ON CONFLICT(directory_url) DO UPDATE SET credentials_json = excluded.credentials_json",
                    params![directory_url, credentials_json],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }
}

fn write_secret_bundle(path: &Path, contents: &[u8]) -> Result<(), CertificateIntentStoreError> {
    let Some(parent) = path.parent() else {
        return Err(CertificateIntentStoreError::InvalidBundlePath {
            message: "bundle path has no parent directory".to_owned(),
        });
    };
    std::fs::create_dir_all(parent).map_err(|error| CertificateIntentStoreError::BundleFile {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    restrict_permissions(parent, path)?;
    write_file_atomically(path, contents).map_err(|error| {
        CertificateIntentStoreError::BundleFile {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    restrict_permissions(parent, path)
}

#[cfg(unix)]
fn restrict_permissions(directory: &Path, file: &Path) -> Result<(), CertificateIntentStoreError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| CertificateIntentStoreError::BundleFile {
            path: directory.to_path_buf(),
            message: error.to_string(),
        },
    )?;
    if file.exists() {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| CertificateIntentStoreError::BundleFile {
                path: file.to_path_buf(),
                message: error.to_string(),
            },
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(
    _directory: &Path,
    _file: &Path,
) -> Result<(), CertificateIntentStoreError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CertificateIntentStoreError {
    #[error("certificate intent store: {message}")]
    Store { message: String },
    #[error("certificate bundle path is not UTF-8: {}", path.display())]
    NonUtf8BundlePath { path: PathBuf },
    #[error("invalid certificate bundle path: {message}")]
    InvalidBundlePath { message: String },
    #[error("invalid certificate material: {message}")]
    InvalidMaterial { message: String },
    #[error(
        "certificate validity differs from its material: expected {expected:?}, actual {actual:?}"
    )]
    ValidityMismatch {
        expected: CertValidityWindow,
        actual: CertValidityWindow,
    },
    #[error("certificate bundle file {}: {message}", path.display())]
    BundleFile { path: PathBuf, message: String },
}

fn store_error(error: CoreStoreError) -> CertificateIntentStoreError {
    CertificateIntentStoreError::Store {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::cert::{
        AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    };
    use ployz_lease_worker::{LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker};
    use ployz_test_support::ids::{cert_id, route_hostname};

    use super::*;

    #[tokio::test]
    async fn active_bundle_round_trips_through_the_store_and_file() {
        let directory = tempfile::tempdir().expect("certificate directory");
        let store = CertificateIntentStore::new(
            CoreStore::open_in_memory().await.expect("core store"),
            directory.path().to_path_buf(),
        );
        let (hostname, certificate_chain_pem, private_key_pem) = certificate_material();
        let validity =
            validate_and_read_validity(&certificate_chain_pem, &private_key_pem, &hostname)
                .expect("certificate validity");

        let committed = store
            .commit_active(
                cert_id("cert_app_example_com"),
                hostname.clone(),
                validity,
                certificate_chain_pem,
                private_key_pem,
            )
            .await
            .expect("commit bundle");

        let loaded = store
            .active_for_hostname(&hostname)
            .await
            .expect("load bundle");
        let (_, bundle_path) = committed
            .active_cert
            .bundle_ref
            .artifact_parts()
            .expect("bundle reference");
        assert_eq!(loaded, Some(committed.clone()));
        assert_eq!(
            std::fs::read(bundle_path.as_str()).expect("bundle file"),
            committed.material_bytes()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(bundle_path.as_str())
                    .expect("bundle metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn challenge_removal_clears_the_published_challenge() {
        let directory = tempfile::tempdir().expect("certificate directory");
        let store = CertificateIntentStore::new(
            CoreStore::open_in_memory().await.expect("core store"),
            directory.path().to_path_buf(),
        );
        let hostname = route_hostname("app.example.com");
        store
            .store_challenge(challenge(hostname.clone()))
            .await
            .expect("publish challenge");

        store
            .remove_challenges_for_hostname(&hostname)
            .await
            .expect("remove challenge");

        assert!(
            store
                .challenges()
                .await
                .expect("list challenges")
                .is_empty()
        );
    }

    fn challenge(hostname: RouteHostname) -> AcmeHttp01Challenge {
        AcmeHttp01Challenge::try_new(
            hostname,
            AcmeChallengeToken::try_new("token").expect("token"),
            AcmeChallengeValue::try_new("token.account-thumbprint").expect("value"),
            AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
        )
        .expect("challenge")
    }

    fn certificate_material() -> (RouteHostname, String, String) {
        let mut worker = StubLeaseWorker::new();
        let LeaseWorkerResponse::LeaseAcquired(acquired) = worker
            .handle(LeaseWorkerRequest::Acquire(
                ployz_core::cert::ManagedLeaseAcquireRequest {
                    ipv4: Vec::new(),
                    ipv6: Vec::new(),
                },
            ))
            .expect("acquire fixture certificate")
        else {
            panic!("acquire returns certificate");
        };
        let hostname =
            RouteHostname::try_new(acquired.bundle.dns_names[1].clone()).expect("fixture hostname");
        (
            hostname,
            acquired.bundle.certificate_chain_pem,
            acquired.bundle.private_key_pem,
        )
    }
}
