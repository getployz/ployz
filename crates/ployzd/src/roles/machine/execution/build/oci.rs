use ployz_core::image::{
    OCI_IMAGE_CONFIG_MEDIA_TYPE, OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE, OCI_IMAGE_MANIFEST_MEDIA_TYPE,
    OciDigest, OciPlatform,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

const MAX_OCI_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const OCI_IMAGE_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const OCI_IMAGE_LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
const OCI_IMAGE_LAYER_ZSTD_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+zstd";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedOciLayout {
    manifest_digest: OciDigest,
    image_id: OciDigest,
    blobs: Vec<ValidatedOciBlob>,
}

impl ValidatedOciLayout {
    #[must_use]
    pub const fn manifest_digest(&self) -> &OciDigest {
        &self.manifest_digest
    }

    #[must_use]
    pub const fn image_id(&self) -> &OciDigest {
        &self.image_id
    }

    #[must_use]
    pub fn blobs(&self) -> &[ValidatedOciBlob] {
        &self.blobs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedOciBlob {
    path: PathBuf,
    digest: OciDigest,
    size: u64,
}

impl ValidatedOciBlob {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn digest(&self) -> &OciDigest {
        &self.digest
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

pub fn validate_oci_layout(
    layout: &Path,
    expected_platform: &OciPlatform,
) -> Result<ValidatedOciLayout, OciLayoutError> {
    let root =
        std::fs::canonicalize(layout).map_err(|error| io("canonicalize OCI layout", error))?;
    let layout_version: OciLayoutVersion = read_json(&root, &root.join("oci-layout"))?;
    if layout_version.image_layout_version != "1.0.0" {
        return Err(OciLayoutError::UnsupportedLayoutVersion {
            version: layout_version.image_layout_version,
        });
    }
    let index: OciIndex = read_json(&root, &root.join("index.json"))?;
    if index.schema_version != 2 {
        return Err(OciLayoutError::InvalidSchemaVersion {
            document: "index",
            actual: index.schema_version,
        });
    }
    if index.media_type != OCI_IMAGE_INDEX_MEDIA_TYPE {
        return Err(OciLayoutError::UnexpectedMediaType {
            expected: OCI_IMAGE_INDEX_MEDIA_TYPE,
            actual: index.media_type,
        });
    }
    let matching = index
        .manifests
        .into_iter()
        .filter(|descriptor| {
            descriptor.platform.as_ref().is_some_and(|platform| {
                platform.os == expected_platform.os()
                    && platform.architecture == expected_platform.architecture()
            })
        })
        .collect::<Vec<_>>();
    let [manifest_descriptor] = matching.as_slice() else {
        return Err(OciLayoutError::PlatformSelection {
            platform: expected_platform.clone(),
            matches: matching.len(),
        });
    };
    if manifest_descriptor.media_type != OCI_IMAGE_MANIFEST_MEDIA_TYPE {
        return Err(OciLayoutError::UnexpectedMediaType {
            expected: OCI_IMAGE_MANIFEST_MEDIA_TYPE,
            actual: manifest_descriptor.media_type.clone(),
        });
    }
    let manifest_blob = validate_blob(&root, manifest_descriptor)?;
    let manifest: OciManifest = read_json(&root, manifest_blob.path())?;
    if manifest.schema_version != 2 {
        return Err(OciLayoutError::InvalidSchemaVersion {
            document: "manifest",
            actual: manifest.schema_version,
        });
    }
    if manifest.media_type != OCI_IMAGE_MANIFEST_MEDIA_TYPE {
        return Err(OciLayoutError::UnexpectedMediaType {
            expected: OCI_IMAGE_MANIFEST_MEDIA_TYPE,
            actual: manifest.media_type,
        });
    }
    if manifest.config.media_type != OCI_IMAGE_CONFIG_MEDIA_TYPE {
        return Err(OciLayoutError::UnexpectedMediaType {
            expected: OCI_IMAGE_CONFIG_MEDIA_TYPE,
            actual: manifest.config.media_type,
        });
    }
    let config_blob = validate_blob(&root, &manifest.config)?;
    let config: OciImageConfig = read_json(&root, config_blob.path())?;
    if config.os != expected_platform.os()
        || config.architecture != expected_platform.architecture()
    {
        return Err(OciLayoutError::ConfigPlatformMismatch {
            expected: expected_platform.clone(),
            actual_os: config.os,
            actual_architecture: config.architecture,
        });
    }
    let mut blobs = vec![manifest_blob, config_blob];
    for layer in manifest.layers {
        if !matches!(
            layer.media_type.as_str(),
            OCI_IMAGE_LAYER_MEDIA_TYPE
                | OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE
                | OCI_IMAGE_LAYER_ZSTD_MEDIA_TYPE
        ) {
            return Err(OciLayoutError::UnexpectedLayerMediaType {
                actual: layer.media_type,
            });
        }
        blobs.push(validate_blob(&root, &layer)?);
    }
    blobs.sort_by(|left, right| left.digest.as_str().cmp(right.digest.as_str()));
    blobs.dedup_by(|left, right| left.digest == right.digest);
    Ok(ValidatedOciLayout {
        manifest_digest: manifest_descriptor.digest.clone(),
        image_id: manifest.config.digest,
        blobs,
    })
}

fn validate_blob(
    root: &Path,
    descriptor: &OciDescriptor,
) -> Result<ValidatedOciBlob, OciLayoutError> {
    let encoded = descriptor
        .digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| OciLayoutError::InvalidDigestPath {
            digest: descriptor.digest.clone(),
        })?;
    let candidate = root.join("blobs").join("sha256").join(encoded);
    let path = canonical_descendant(root, &candidate)?;
    let metadata = std::fs::metadata(&path).map_err(|error| io("inspect OCI blob", error))?;
    if !metadata.is_file() {
        return Err(OciLayoutError::BlobNotFile { path });
    }
    if metadata.len() != descriptor.size {
        return Err(OciLayoutError::BlobSizeMismatch {
            digest: descriptor.digest.clone(),
            expected: descriptor.size,
            actual: metadata.len(),
        });
    }
    let actual = hash_file(&path)?;
    if actual != descriptor.digest {
        return Err(OciLayoutError::BlobDigestMismatch {
            expected: descriptor.digest.clone(),
            actual,
        });
    }
    Ok(ValidatedOciBlob {
        path,
        digest: descriptor.digest.clone(),
        size: descriptor.size,
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, path: &Path) -> Result<T, OciLayoutError> {
    let path = canonical_descendant(root, path)?;
    let metadata = std::fs::metadata(&path).map_err(|error| io("inspect OCI metadata", error))?;
    if !metadata.is_file() || metadata.len() > MAX_OCI_METADATA_BYTES {
        return Err(OciLayoutError::MetadataOutOfBounds {
            path,
            size: metadata.len(),
        });
    }
    let file = File::open(&path).map_err(|error| io("open OCI metadata", error))?;
    serde_json::from_reader(BufReader::new(file)).map_err(|error| OciLayoutError::InvalidJson {
        path,
        message: error.to_string(),
    })
}

fn canonical_descendant(root: &Path, candidate: &Path) -> Result<PathBuf, OciLayoutError> {
    let candidate =
        std::fs::canonicalize(candidate).map_err(|error| io("canonicalize OCI path", error))?;
    if !candidate.starts_with(root) {
        return Err(OciLayoutError::PathEscapesLayout { path: candidate });
    }
    Ok(candidate)
}

fn hash_file(path: &Path) -> Result<OciDigest, OciLayoutError> {
    let file = File::open(path).map_err(|error| io("open OCI blob", error))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| io("read OCI blob", error))?;
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            return Err(OciLayoutError::InvalidComputedDigest {
                message: "blob reader returned an impossible byte count".to_owned(),
            });
        };
        hasher.update(bytes);
    }
    OciDigest::try_new(format!("sha256:{:x}", hasher.finalize())).map_err(|error| {
        OciLayoutError::InvalidComputedDigest {
            message: error.to_string(),
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciLayoutVersion {
    #[serde(rename = "imageLayoutVersion")]
    image_layout_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciIndex {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    #[serde(rename = "mediaType")]
    media_type: String,
    manifests: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciDescriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    size: u64,
    digest: OciDigest,
    #[serde(default)]
    platform: Option<OciDescriptorPlatform>,
    #[serde(default, rename = "annotations")]
    _annotations: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciDescriptorPlatform {
    architecture: String,
    os: String,
}

#[derive(Deserialize)]
struct OciImageConfig {
    architecture: String,
    os: String,
}

fn io(action: &'static str, error: std::io::Error) -> OciLayoutError {
    OciLayoutError::Io {
        action,
        message: error.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OciLayoutError {
    #[error("unsupported OCI layout version {version:?}")]
    UnsupportedLayoutVersion { version: String },
    #[error("OCI {document} has schema version {actual}, expected 2")]
    InvalidSchemaVersion { document: &'static str, actual: u8 },
    #[error("OCI layout has {matches} manifests for requested platform {platform:?}")]
    PlatformSelection {
        platform: OciPlatform,
        matches: usize,
    },
    #[error("OCI descriptor media type is {actual:?}, expected {expected:?}")]
    UnexpectedMediaType {
        expected: &'static str,
        actual: String,
    },
    #[error("OCI layer media type {actual:?} is unsupported")]
    UnexpectedLayerMediaType { actual: String },
    #[error(
        "OCI image config platform {actual_os}/{actual_architecture} does not match {expected:?}"
    )]
    ConfigPlatformMismatch {
        expected: OciPlatform,
        actual_os: String,
        actual_architecture: String,
    },
    #[error("OCI digest cannot be mapped to a blob path: {digest}")]
    InvalidDigestPath { digest: OciDigest },
    #[error("computed OCI digest was invalid: {message}")]
    InvalidComputedDigest { message: String },
    #[error("OCI path escapes the layout: {path:?}")]
    PathEscapesLayout { path: PathBuf },
    #[error("OCI blob is not a regular file: {path:?}")]
    BlobNotFile { path: PathBuf },
    #[error("OCI blob {digest} size is {actual}, expected {expected}")]
    BlobSizeMismatch {
        digest: OciDigest,
        expected: u64,
        actual: u64,
    },
    #[error("OCI blob digest is {actual}, expected {expected}")]
    BlobDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    #[error("OCI metadata file {path:?} has unsupported size {size}")]
    MetadataOutOfBounds { path: PathBuf, size: u64 },
    #[error("OCI metadata file {path:?} is invalid: {message}")]
    InvalidJson { path: PathBuf, message: String },
    #[error("failed to {action}: {message}")]
    Io {
        action: &'static str,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn write_blob(root: &Path, value: serde_json::Value) -> (OciDigest, u64) {
        let bytes = serde_json::to_vec(&value).expect("json");
        let digest = OciDigest::sha256(&bytes);
        let encoded = digest.as_str().strip_prefix("sha256:").expect("sha");
        fs::create_dir_all(root.join("blobs/sha256")).expect("blobs");
        fs::write(root.join("blobs/sha256").join(encoded), &bytes).expect("blob");
        (digest, u64::try_from(bytes.len()).expect("size"))
    }

    fn layout(platform: &OciPlatform) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("temp");
        fs::write(
            temp.path().join("oci-layout"),
            r#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .expect("layout");
        let (config_digest, config_size) = write_blob(
            temp.path(),
            json!({"architecture": platform.architecture(), "os": platform.os()}),
        );
        let layer = b"layer";
        let layer_digest = OciDigest::sha256(layer);
        let layer_encoded = layer_digest.as_str().strip_prefix("sha256:").expect("sha");
        fs::write(temp.path().join("blobs/sha256").join(layer_encoded), layer).expect("layer");
        let (manifest_digest, manifest_size) = write_blob(
            temp.path(),
            json!({
                "schemaVersion": 2,
                "mediaType": OCI_IMAGE_MANIFEST_MEDIA_TYPE,
                "config": {"mediaType": OCI_IMAGE_CONFIG_MEDIA_TYPE, "size": config_size, "digest": config_digest},
                "layers": [{"mediaType": OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE, "size": layer.len(), "digest": layer_digest}]
            }),
        );
        fs::write(
            temp.path().join("index.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 2,
                "mediaType": OCI_IMAGE_INDEX_MEDIA_TYPE,
                "manifests": [{
                    "mediaType": OCI_IMAGE_MANIFEST_MEDIA_TYPE,
                    "size": manifest_size,
                    "digest": manifest_digest,
                    "platform": {"os": platform.os(), "architecture": platform.architecture()}
                }]
            }))
            .expect("index"),
        )
        .expect("index");
        temp
    }

    #[test]
    fn validates_exact_requested_platform_and_every_blob() {
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let temp = layout(&platform);
        let layout = validate_oci_layout(temp.path(), &platform).expect("valid layout");
        assert_eq!(layout.blobs().len(), 3);
        assert_ne!(layout.manifest_digest(), layout.image_id());
    }

    #[test]
    fn rejects_platform_mismatch_before_ingestion() {
        let amd64 = OciPlatform::try_new("linux", "amd64").expect("platform");
        let arm64 = OciPlatform::try_new("linux", "arm64").expect("platform");
        let temp = layout(&amd64);
        assert!(matches!(
            validate_oci_layout(temp.path(), &arm64),
            Err(OciLayoutError::PlatformSelection { matches: 0, .. })
        ));
    }

    #[test]
    fn rejects_tampered_blob_content() {
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let temp = layout(&platform);
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join("index.json")).expect("index"))
                .expect("json");
        let digest = index
            .get("manifests")
            .and_then(serde_json::Value::as_array)
            .and_then(|manifests| manifests.first())
            .and_then(|manifest| manifest.get("digest"))
            .and_then(serde_json::Value::as_str)
            .expect("digest")
            .trim_start_matches("sha256:");
        fs::write(temp.path().join("blobs/sha256").join(digest), "tampered").expect("tamper");
        assert!(matches!(
            validate_oci_layout(temp.path(), &platform),
            Err(OciLayoutError::BlobSizeMismatch { .. })
                | Err(OciLayoutError::BlobDigestMismatch { .. })
        ));
    }
}
