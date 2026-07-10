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
    ) -> Result<Option<ActiveCertState>, CertificateIntentStoreError> {
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
    ) -> Result<Option<ActiveCertState>, CertificateIntentStoreError> {
        let cert_id = cert_id.clone();
        self.store
            .call(move |conn| {
                let active_certificates: Vec<ActiveCertState> = query_json_list(
                    conn,
                    "SELECT json FROM custom_certificate_intent ORDER BY hostname",
                )?;
                Ok(active_certificates
                    .into_iter()
                    .find(|active| active.cert_id == cert_id))
            })
            .await
            .map_err(store_error)
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<ActiveCertState>, CertificateIntentStoreError> {
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

    pub(crate) async fn seed_active_metadata(
        &self,
        active_cert: ActiveCertState,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO custom_certificate_intent (hostname, json) VALUES (?1, ?2)
                     ON CONFLICT(hostname) DO UPDATE SET json = excluded.json",
                    params![active_cert.hostname.as_str(), to_json(&active_cert)?],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub(crate) fn prepare_active(
        &self,
        cert_id: CertId,
        hostname: RouteHostname,
        certificate_chain_pem: String,
        private_key_pem: String,
    ) -> Result<CustomCertBundle, CertificateIntentStoreError> {
        let validity =
            validate_and_read_validity(&certificate_chain_pem, &private_key_pem, &hostname)
                .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;

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

    pub(crate) fn write_prepared_material(
        &self,
        bundle: &CustomCertBundle,
    ) -> Result<(), CertificateIntentStoreError> {
        let path = self.bundle_path(bundle.active_cert())?;
        write_secret_bundle(&path, &bundle.material_bytes())
    }

    pub(crate) fn load_bundle(
        &self,
        active_cert: &ActiveCertState,
    ) -> Result<CustomCertBundle, CertificateIntentStoreError> {
        let path = self.bundle_path(active_cert)?;
        let material =
            std::fs::read(&path).map_err(|error| CertificateIntentStoreError::BundleFile {
                path: path.clone(),
                message: error.to_string(),
            })?;
        let Some(separator) = material.iter().position(|byte| *byte == 0) else {
            return Err(CertificateIntentStoreError::InvalidMaterial {
                message: "certificate bundle has no material separator".to_owned(),
            });
        };
        let certificate_chain_pem =
            String::from_utf8(material[..separator].to_vec()).map_err(|error| {
                CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                }
            })?;
        let private_key_pem =
            String::from_utf8(material[separator + 1..].to_vec()).map_err(|error| {
                CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                }
            })?;
        let bundle =
            CustomCertBundle::try_new(active_cert.clone(), certificate_chain_pem, private_key_pem)
                .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;
        let actual_validity = validate_and_read_validity(
            bundle.certificate_chain_pem(),
            bundle.private_key_pem(),
            &active_cert.hostname,
        )
        .map_err(|error| CertificateIntentStoreError::InvalidMaterial {
            message: error.to_string(),
        })?;
        if actual_validity != active_cert.validity {
            return Err(CertificateIntentStoreError::ValidityMismatch {
                expected: active_cert.validity,
                actual: actual_validity,
            });
        }
        Ok(bundle)
    }

    fn bundle_path(
        &self,
        active_cert: &ActiveCertState,
    ) -> Result<PathBuf, CertificateIntentStoreError> {
        let (digest, _) = active_cert.bundle_ref.artifact_parts().map_err(|error| {
            CertificateIntentStoreError::InvalidBundlePath {
                message: error.to_string(),
            }
        })?;
        Ok(self.state_dir.join("bundles").join(format!(
            "{}-{}.bundle",
            active_cert.cert_id.as_str(),
            digest.as_str()
        )))
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
    async fn active_metadata_and_local_bundle_round_trip_separately() {
        let directory = tempfile::tempdir().expect("certificate directory");
        let store = CertificateIntentStore::new(
            CoreStore::open_in_memory().await.expect("core store"),
            directory.path().to_path_buf(),
        );
        let (hostname, certificate_chain_pem, private_key_pem) = certificate_material();
        let prepared = store
            .prepare_active(
                cert_id("cert_app_example_com"),
                hostname.clone(),
                certificate_chain_pem,
                private_key_pem,
            )
            .expect("prepare bundle");
        store
            .write_prepared_material(&prepared)
            .expect("write material");
        store
            .seed_active_metadata(prepared.active_cert().clone())
            .await
            .expect("store metadata");

        let loaded = store
            .active_for_hostname(&hostname)
            .await
            .expect("load metadata")
            .expect("active metadata");
        assert_eq!(loaded, *prepared.active_cert());
        assert_eq!(store.load_bundle(&loaded).expect("load bundle"), prepared);
        let bundle_path = store.bundle_path(&loaded).expect("bundle path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(bundle_path)
                    .expect("bundle metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn promoted_metadata_survives_without_local_secret_material() {
        let source_directory = tempfile::tempdir().expect("source certificate directory");
        let source = CertificateIntentStore::new(
            CoreStore::open_in_memory()
                .await
                .expect("source core store"),
            source_directory.path().to_path_buf(),
        );
        let (hostname, certificate_chain_pem, private_key_pem) = certificate_material();
        let prepared = source
            .prepare_active(
                cert_id("cert_app_example_com"),
                hostname.clone(),
                certificate_chain_pem,
                private_key_pem,
            )
            .expect("prepare source bundle");
        let target_directory = tempfile::tempdir().expect("target certificate directory");
        let target = CertificateIntentStore::new(
            CoreStore::open_in_memory()
                .await
                .expect("target core store"),
            target_directory.path().to_path_buf(),
        );

        target
            .seed_active_metadata(prepared.active_cert().clone())
            .await
            .expect("seed active metadata");

        assert_eq!(
            target
                .active_for_hostname(&hostname)
                .await
                .expect("load metadata"),
            Some(prepared.active_cert().clone())
        );
        assert!(matches!(
            target.load_bundle(prepared.active_cert()),
            Err(CertificateIntentStoreError::BundleFile { .. })
        ));
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
