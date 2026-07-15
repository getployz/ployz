use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NamespaceId, OperationId};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const DEPLOY_HISTORY_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterFingerprint(String);

impl ClusterFingerprint {
    pub fn from_connection(nats_url: &str, ca_file: &Path) -> Result<Self, DeployHistoryError> {
        let ca = fs::read(ca_file).map_err(|error| DeployHistoryError::ReadCa {
            path: ca_file.to_owned(),
            message: error.to_string(),
        })?;
        let mut hasher = Sha256::new();
        hash_frame(&mut hasher, b"ployz.deploy-history.cluster.v1");
        hash_frame(&mut hasher, nats_url.as_bytes());
        hash_frame(&mut hasher, &ca);
        Ok(Self(format!("{:x}", hasher.finalize())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeployHistoryTimestamp(u64);

impl DeployHistoryTimestamp {
    #[must_use]
    pub const fn from_unix_seconds(unix_seconds: u64) -> Self {
        Self(unix_seconds)
    }

    #[must_use]
    pub const fn unix_seconds(self) -> u64 {
        self.0
    }

    pub fn now() -> Result<Self, DeployHistoryError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| DeployHistoryError::Clock {
                message: error.to_string(),
            })?;
        Ok(Self(duration.as_secs()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployHistoryEntry {
    pub recorded_at: DeployHistoryTimestamp,
    pub operation_id: OperationId,
    pub request: DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployHistory {
    root: PathBuf,
    cluster: ClusterFingerprint,
    namespace_id: NamespaceId,
}

impl DeployHistory {
    #[must_use]
    pub fn new(root: PathBuf, cluster: ClusterFingerprint, namespace_id: NamespaceId) -> Self {
        Self {
            root,
            cluster,
            namespace_id,
        }
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root
            .join(self.cluster.as_str())
            .join(format!("{}.jsonl", self.namespace_id.as_str()))
    }

    pub fn append_success(&self, entry: DeployHistoryEntry) -> Result<(), DeployHistoryError> {
        self.validate(&entry)?;
        let mut entries = self.load()?;
        entries.push(entry);
        let expired = entries.len().saturating_sub(DEPLOY_HISTORY_LIMIT);
        entries.drain(..expired);
        self.rewrite(&entries)
    }

    pub fn load(&self) -> Result<Vec<DeployHistoryEntry>, DeployHistoryError> {
        let path = self.path();
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(DeployHistoryError::Read {
                    path,
                    message: error.to_string(),
                });
            }
        };
        contents
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let entry =
                    serde_json::from_str(line).map_err(|error| DeployHistoryError::Parse {
                        path: path.clone(),
                        line: index + 1,
                        message: error.to_string(),
                    })?;
                self.validate(&entry)?;
                Ok(entry)
            })
            .collect()
    }

    fn validate(&self, entry: &DeployHistoryEntry) -> Result<(), DeployHistoryError> {
        if entry.request.namespace_id != self.namespace_id {
            return Err(DeployHistoryError::NamespaceMismatch {
                expected: self.namespace_id.as_str().to_owned(),
                actual: entry.request.namespace_id.as_str().to_owned(),
            });
        }
        if let Some(service) = entry
            .request
            .services
            .iter()
            .find(|service| service.image.pinned_digest().is_none())
        {
            return Err(DeployHistoryError::UnpinnedImage {
                image: service.image.as_str().to_owned(),
            });
        }
        Ok(())
    }

    fn rewrite(&self, entries: &[DeployHistoryEntry]) -> Result<(), DeployHistoryError> {
        let path = self.path();
        let Some(cluster_dir) = path.parent() else {
            return Err(DeployHistoryError::Write {
                path,
                message: "history path has no parent directory".to_owned(),
            });
        };
        prepare_private_directory(&self.root)?;
        prepare_private_directory(cluster_dir)?;

        let mut temporary = tempfile::NamedTempFile::new_in(cluster_dir).map_err(|error| {
            DeployHistoryError::Write {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;
        for entry in entries {
            serde_json::to_writer(&mut temporary, entry).map_err(|error| {
                DeployHistoryError::Write {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
            temporary
                .write_all(b"\n")
                .map_err(|error| DeployHistoryError::Write {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| DeployHistoryError::Write {
                path: path.clone(),
                message: error.to_string(),
            })?;
        temporary
            .persist(&path)
            .map_err(|error| DeployHistoryError::Write {
                path,
                message: error.error.to_string(),
            })?;
        Ok(())
    }
}

#[must_use]
pub fn default_deploy_history_root() -> Option<PathBuf> {
    deploy_history_root(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

fn deploy_history_root(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    xdg_state_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .map(|root| root.join("ployz/deploy-history"))
}

#[must_use]
pub fn render_history(entries: &[DeployHistoryEntry]) -> String {
    let mut rendered = String::new();
    for entry in entries {
        let service_count = entry.request.services.len();
        rendered.push_str(&format!(
            "{}  {}  {}",
            entry.recorded_at.unix_seconds(),
            entry.operation_id.as_str(),
            entry.request.namespace_id.as_str(),
        ));
        if let Some(origin) = &entry.request.origin {
            rendered.push_str(&format!("  {}", origin.as_str()));
        }
        rendered.push_str(&format!(
            "  {service_count} service{}",
            if service_count == 1 { "" } else { "s" }
        ));
        for service in &entry.request.services {
            rendered.push_str(&format!(
                "  {}={}",
                service.service_id.as_str(),
                service.image.as_str()
            ));
        }
        rendered.push('\n');
    }
    rendered
}

fn hash_frame(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value);
}

fn prepare_private_directory(path: &Path) -> Result<(), DeployHistoryError> {
    fs::create_dir_all(path).map_err(|error| DeployHistoryError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            DeployHistoryError::Write {
                path: path.to_owned(),
                message: error.to_string(),
            }
        })?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployHistoryError {
    #[error("cannot read cluster CA {path}: {message}")]
    ReadCa { path: PathBuf, message: String },
    #[error("cannot read deploy history {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("cannot parse deploy history {path} line {line}: {message}")]
    Parse {
        path: PathBuf,
        line: usize,
        message: String,
    },
    #[error("cannot write deploy history {path}: {message}")]
    Write { path: PathBuf, message: String },
    #[error("cannot record deploy history timestamp: {message}")]
    Clock { message: String },
    #[error("deploy history namespace {actual} does not match stream namespace {expected}")]
    NamespaceMismatch { expected: String, actual: String },
    #[error("deploy history image is not digest-pinned: {image}")]
    UnpinnedImage { image: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployServiceSpec, ImageReference, ImageSource, ReplicaCount,
    };
    use ployz_core::ids::ServiceId;
    use std::ffi::OsString;

    fn request(namespace: &str, image: &str) -> DeployRequest {
        DeployRequest {
            namespace_id: NamespaceId::try_new(namespace).expect("valid namespace"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: vec![DeployServiceSpec {
                keep: None,
                service_id: ServiceId::try_new("web").expect("valid service"),
                image: ImageReference::try_new(image).expect("valid image"),
                image_source: ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        }
    }

    fn entry(sequence: u64) -> DeployHistoryEntry {
        DeployHistoryEntry {
            recorded_at: DeployHistoryTimestamp::from_unix_seconds(1_750_000_000 + sequence),
            operation_id: OperationId::try_new(format!("op_{sequence}"))
                .expect("valid operation id"),
            request: request(
                "prod",
                "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        }
    }

    #[test]
    fn connection_fingerprint_separates_clusters_for_the_same_namespace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first_ca = temporary.path().join("first-ca.pem");
        let second_ca = temporary.path().join("second-ca.pem");
        std::fs::write(&first_ca, "first CA").expect("write first CA");
        std::fs::write(&second_ca, "second CA").expect("write second CA");

        let first = ClusterFingerprint::from_connection("tls://core:4222", &first_ca)
            .expect("first fingerprint");
        let second = ClusterFingerprint::from_connection("tls://core:4222", &second_ca)
            .expect("second fingerprint");
        let third = ClusterFingerprint::from_connection("tls://other-core:4222", &first_ca)
            .expect("third fingerprint");
        let namespace = NamespaceId::try_new("prod").expect("valid namespace");

        assert_ne!(
            DeployHistory::new(
                temporary.path().to_owned(),
                first.clone(),
                namespace.clone()
            )
            .path(),
            DeployHistory::new(temporary.path().to_owned(), second, namespace.clone()).path()
        );
        assert_ne!(
            DeployHistory::new(temporary.path().to_owned(), first, namespace.clone()).path(),
            DeployHistory::new(temporary.path().to_owned(), third, namespace).path()
        );
    }

    #[test]
    fn append_persists_only_the_newest_fifty_pinned_payloads() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let ca_file = temporary.path().join("ca.pem");
        std::fs::write(&ca_file, "cluster CA").expect("write CA");
        let fingerprint =
            ClusterFingerprint::from_connection("tls://core:4222", &ca_file).expect("fingerprint");
        let root = temporary.path().join("history");
        let namespace = NamespaceId::try_new("prod").expect("valid namespace");
        let history = DeployHistory::new(root.clone(), fingerprint.clone(), namespace.clone());

        for sequence in 1..=52 {
            history
                .append_success(entry(sequence))
                .expect("append entry");
        }

        let restarted = DeployHistory::new(root, fingerprint, namespace);
        let loaded = restarted.load().expect("load history after restart");
        assert_eq!(loaded, (3..=52).map(entry).collect::<Vec<_>>());
    }

    #[test]
    fn append_rejects_unpinned_images() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = DeployHistory::new(
            temporary.path().to_owned(),
            ClusterFingerprint("a".repeat(64)),
            NamespaceId::try_new("prod").expect("valid namespace"),
        );
        let unpinned = DeployHistoryEntry {
            request: request("prod", "ghcr.io/acme/web:latest"),
            ..entry(1)
        };

        assert!(matches!(
            history.append_success(unpinned),
            Err(DeployHistoryError::UnpinnedImage { .. })
        ));
    }

    #[test]
    fn append_rejects_the_wrong_namespace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = DeployHistory::new(
            temporary.path().to_owned(),
            ClusterFingerprint("a".repeat(64)),
            NamespaceId::try_new("prod").expect("valid namespace"),
        );
        let wrong_namespace = DeployHistoryEntry {
            request: request(
                "staging",
                "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            ..entry(2)
        };

        assert!(matches!(
            history.append_success(wrong_namespace),
            Err(DeployHistoryError::NamespaceMismatch { .. })
        ));
    }

    #[test]
    fn state_root_prefers_xdg_and_falls_back_to_home() {
        assert_eq!(
            deploy_history_root(
                Some(OsString::from("/state")),
                Some(OsString::from("/home/me"))
            ),
            Some(PathBuf::from("/state/ployz/deploy-history"))
        );
        assert_eq!(
            deploy_history_root(None, Some(OsString::from("/home/me"))),
            Some(PathBuf::from("/home/me/.local/state/ployz/deploy-history"))
        );
    }

    #[test]
    fn inspector_renders_concise_oldest_to_newest_lines() {
        let mut from_compose = entry(2);
        from_compose.request.origin = Some(
            ployz_core::deploy::DeployOrigin::try_new("compose: production").expect("valid origin"),
        );

        assert_eq!(
            render_history(&[entry(1), from_compose]),
            concat!(
                "1750000001  op_1  prod  1 service  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
                "1750000002  op_2  prod  compose: production  1 service  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            )
        );
    }

    #[test]
    fn loader_rejects_invalid_jsonl_entries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = DeployHistory::new(
            temporary.path().to_owned(),
            ClusterFingerprint("a".repeat(64)),
            NamespaceId::try_new("prod").expect("valid namespace"),
        );
        let parent = history.path().parent().expect("history parent").to_owned();
        std::fs::create_dir_all(parent).expect("create history parent");
        std::fs::write(history.path(), "{not json}\n").expect("write invalid history");

        assert!(matches!(
            history.load(),
            Err(DeployHistoryError::Parse { line: 1, .. })
        ));
    }

    #[test]
    fn loader_rejects_valid_json_with_an_unpinned_payload() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = DeployHistory::new(
            temporary.path().to_owned(),
            ClusterFingerprint("a".repeat(64)),
            NamespaceId::try_new("prod").expect("valid namespace"),
        );
        let parent = history.path().parent().expect("history parent").to_owned();
        std::fs::create_dir_all(parent).expect("create history parent");
        let unpinned = DeployHistoryEntry {
            request: request("prod", "ghcr.io/acme/web:latest"),
            ..entry(1)
        };
        std::fs::write(
            history.path(),
            serde_json::to_string(&unpinned).expect("serialize entry") + "\n",
        )
        .expect("write history");

        assert!(matches!(
            history.load(),
            Err(DeployHistoryError::UnpinnedImage { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn persisted_history_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("history");
        let history = DeployHistory::new(
            root.clone(),
            ClusterFingerprint("a".repeat(64)),
            NamespaceId::try_new("prod").expect("valid namespace"),
        );

        history.append_success(entry(1)).expect("append entry");

        assert_eq!(
            std::fs::metadata(root)
                .expect("root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(history.path())
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
