use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use ployz_api::ImageReceiveSessionPayload;
use ployz_runtime_api::ImageArchiveReader;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tar::{Archive, Builder, Header};
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::daemon::handlers::image::registry::ImageRegistry;

const DOCKER_MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";
const DOCKER_LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar";
const RECEIVER_HTTP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RECEIVER_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImageArchiveError {
    #[error("{operation}: {message}")]
    Io {
        operation: &'static str,
        message: String,
    },
    #[error("docker archive manifest is invalid: {message}")]
    InvalidManifest { message: String },
    #[error("docker archive is missing member '{path}'")]
    MissingMember { path: String },
    #[error("receiver endpoint is invalid: {message}")]
    InvalidEndpoint { message: String },
    #[error("receiver request failed: {message}")]
    Receiver { message: String },
    #[error("registry import failed: {message}")]
    Registry { message: String },
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedImageArchive {
    pub(crate) config: ArchiveBlob,
    pub(crate) layers: Vec<ArchiveBlob>,
    pub(crate) repo_tags: Vec<String>,
    pub(crate) distribution_manifest: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveBlob {
    pub(crate) digest: String,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ReceiverUploadReport {
    pub(crate) uploaded_blobs: usize,
    pub(crate) skipped_blobs: usize,
    pub(crate) bytes_uploaded: u64,
    pub(crate) manifest_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerArchiveManifestEntry {
    config: String,
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    repo_tags: Vec<String>,
    layers: Vec<String>,
}

#[derive(Debug)]
struct ExtractedMember {
    digest: String,
    size: u64,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DistributionManifest {
    schema_version: u8,
    media_type: &'static str,
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnedDistributionManifest {
    pub(crate) schema_version: u8,
    pub(crate) media_type: String,
    pub(crate) config: OwnedDescriptor,
    pub(crate) layers: Vec<OwnedDescriptor>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    media_type: &'static str,
    size: u64,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnedDescriptor {
    pub(crate) media_type: String,
    pub(crate) size: u64,
    pub(crate) digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct DockerLoadManifestEntry<'a> {
    config: &'a str,
    repo_tags: &'a [String],
    layers: Vec<String>,
}

pub(crate) async fn parse_image_archive(
    mut reader: ImageArchiveReader<'_>,
    work_dir: &Path,
) -> Result<ParsedImageArchive, ImageArchiveError> {
    tokio::fs::create_dir_all(work_dir)
        .await
        .map_err(|error| io_error("create image archive work dir", error))?;
    let archive_path = work_dir.join("source.tar");
    let mut archive_file = tokio::fs::File::create(&archive_path)
        .await
        .map_err(|error| io_error("create image archive spool", error))?;
    tokio::io::copy(&mut reader, &mut archive_file)
        .await
        .map_err(|error| io_error("spool image archive", error))?;
    archive_file
        .flush()
        .await
        .map_err(|error| io_error("flush image archive spool", error))?;
    let work_dir = work_dir.to_path_buf();
    tokio::task::spawn_blocking(move || parse_spooled_archive(&archive_path, &work_dir))
        .await
        .map_err(|error| ImageArchiveError::Io {
            operation: "join image archive parser",
            message: error.to_string(),
        })?
}

pub(crate) async fn upload_archive_to_receiver(
    session: &ImageReceiveSessionPayload,
    reference: &str,
    archive: &ParsedImageArchive,
) -> Result<ReceiverUploadReport, ImageArchiveError> {
    let client = reqwest::Client::builder()
        .connect_timeout(RECEIVER_HTTP_CONNECT_TIMEOUT)
        .timeout(RECEIVER_HTTP_TIMEOUT)
        .build()
        .map_err(|error| ImageArchiveError::Receiver {
            message: format!("build receiver HTTP client: {error}"),
        })?;
    let mut endpoint = session
        .endpoint
        .trim_end_matches('/')
        .parse::<reqwest::Url>()
        .map_err(|error| ImageArchiveError::InvalidEndpoint {
            message: error.to_string(),
        })?;
    let endpoint_path = endpoint.path().trim_end_matches('/').to_string();
    endpoint.set_path(&format!("{endpoint_path}/"));
    let mut uploaded_blobs = 0_usize;
    let mut skipped_blobs = 0_usize;
    let mut bytes_uploaded = 0_u64;

    for blob in std::iter::once(&archive.config).chain(archive.layers.iter()) {
        if receiver_has_blob(&client, endpoint.clone(), &session.headers, &blob.digest).await? {
            skipped_blobs += 1;
            continue;
        }
        upload_blob(&client, endpoint.clone(), &session.headers, blob).await?;
        uploaded_blobs += 1;
        bytes_uploaded = bytes_uploaded.saturating_add(blob.size);
    }

    let manifest_digest = upload_manifest(
        &client,
        endpoint,
        &session.headers,
        reference,
        archive.distribution_manifest.clone(),
    )
    .await?;

    Ok(ReceiverUploadReport {
        uploaded_blobs,
        skipped_blobs,
        bytes_uploaded,
        manifest_digest,
    })
}

pub(crate) async fn reconstruct_received_archive(
    registry: &ImageRegistry,
    repository: &str,
    reference: &str,
    repo_tags: &[String],
    output_path: PathBuf,
) -> Result<PathBuf, ImageArchiveError> {
    let manifest = registry
        .received_manifest(repository, reference)
        .await
        .map_err(|error| ImageArchiveError::Registry {
            message: error.to_string(),
        })?
        .ok_or_else(|| ImageArchiveError::Registry {
            message: format!("manifest '{repository}:{reference}' was not found"),
        })?;
    let distribution: OwnedDistributionManifest =
        serde_json::from_slice(&manifest.body).map_err(|error| {
            ImageArchiveError::InvalidManifest {
                message: error.to_string(),
            }
        })?;
    let registry = registry.clone();
    let repo_tags = repo_tags.to_vec();
    tokio::task::spawn_blocking(move || {
        build_docker_load_archive(&registry, distribution, repo_tags, &output_path)?;
        Ok(output_path)
    })
    .await
    .map_err(|error| ImageArchiveError::Io {
        operation: "join received archive builder",
        message: error.to_string(),
    })?
}

fn parse_spooled_archive(
    archive_path: &Path,
    work_dir: &Path,
) -> Result<ParsedImageArchive, ImageArchiveError> {
    let file = File::open(archive_path).map_err(|error| io_error("open image archive", error))?;
    let mut archive = Archive::new(file);
    let members_dir = work_dir.join("members");
    fs::create_dir_all(&members_dir)
        .map_err(|error| io_error("create extracted image archive members dir", error))?;
    let mut extracted = BTreeMap::new();

    let entries = archive
        .entries()
        .map_err(|error| io_error("read image archive entries", error))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| io_error("read image archive entry", error))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let member_path = entry
            .path()
            .map_err(|error| io_error("read image archive member path", error))?
            .to_string_lossy()
            .replace('\\', "/");
        let output = members_dir.join(Uuid::new_v4().to_string());
        let extracted_member = copy_member(&mut entry, &output)?;
        extracted.insert(member_path, extracted_member);
    }

    let manifest_member =
        extracted
            .get("manifest.json")
            .ok_or_else(|| ImageArchiveError::MissingMember {
                path: "manifest.json".into(),
            })?;
    let manifest_body = fs::read(&manifest_member.path)
        .map_err(|error| io_error("read docker archive manifest", error))?;
    let mut manifests: Vec<DockerArchiveManifestEntry> = serde_json::from_slice(&manifest_body)
        .map_err(|error| ImageArchiveError::InvalidManifest {
            message: error.to_string(),
        })?;
    let Some(manifest) = manifests.pop() else {
        return Err(ImageArchiveError::InvalidManifest {
            message: "manifest array is empty".into(),
        });
    };

    let config_member =
        extracted
            .get(&manifest.config)
            .ok_or_else(|| ImageArchiveError::MissingMember {
                path: manifest.config.clone(),
            })?;
    let config = ArchiveBlob {
        digest: config_member.digest.clone(),
        size: config_member.size,
        path: config_member.path.clone(),
    };
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for layer_path in &manifest.layers {
        let layer = extracted
            .get(layer_path)
            .ok_or_else(|| ImageArchiveError::MissingMember {
                path: layer_path.clone(),
            })?;
        layers.push(ArchiveBlob {
            digest: layer.digest.clone(),
            size: layer.size,
            path: layer.path.clone(),
        });
    }

    let distribution_manifest = distribution_manifest(&config, &layers)?;
    Ok(ParsedImageArchive {
        config,
        layers,
        repo_tags: manifest.repo_tags,
        distribution_manifest,
    })
}

fn copy_member<R: Read>(
    reader: &mut R,
    output: &Path,
) -> Result<ExtractedMember, ImageArchiveError> {
    let mut file =
        File::create(output).map_err(|error| io_error("create archive member", error))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| io_error("read archive member", error))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| io_error("write archive member", error))?;
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as u64);
    }
    file.flush()
        .map_err(|error| io_error("flush archive member", error))?;
    Ok(ExtractedMember {
        digest: format_digest(hasher.finalize()),
        size,
        path: output.to_path_buf(),
    })
}

fn distribution_manifest(
    config: &ArchiveBlob,
    layers: &[ArchiveBlob],
) -> Result<Vec<u8>, ImageArchiveError> {
    let manifest = DistributionManifest {
        schema_version: 2,
        media_type: DOCKER_MANIFEST_MEDIA_TYPE,
        config: Descriptor {
            media_type: DOCKER_CONFIG_MEDIA_TYPE,
            size: config.size,
            digest: config.digest.clone(),
        },
        layers: layers
            .iter()
            .map(|layer| Descriptor {
                media_type: DOCKER_LAYER_MEDIA_TYPE,
                size: layer.size,
                digest: layer.digest.clone(),
            })
            .collect(),
    };
    serde_json::to_vec(&manifest).map_err(|error| ImageArchiveError::InvalidManifest {
        message: error.to_string(),
    })
}

async fn receiver_has_blob(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    headers: &BTreeMap<String, String>,
    digest: &str,
) -> Result<bool, ImageArchiveError> {
    let response = client
        .head(join_endpoint(endpoint, &format!("blobs/{digest}"))?)
        .headers(session_headers(headers)?)
        .send()
        .await
        .map_err(|error| ImageArchiveError::Receiver {
            message: error.to_string(),
        })?;
    match response.status() {
        StatusCode::OK => Ok(true),
        StatusCode::NOT_FOUND => Ok(false),
        status => Err(ImageArchiveError::Receiver {
            message: format!("receiver blob HEAD returned {status}"),
        }),
    }
}

async fn upload_blob(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    headers: &BTreeMap<String, String>,
    blob: &ArchiveBlob,
) -> Result<(), ImageArchiveError> {
    let file = tokio::fs::File::open(&blob.path)
        .await
        .map_err(|error| io_error("open archive blob for upload", error))?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let response = client
        .post(join_endpoint(
            endpoint,
            &format!("blobs/uploads/?digest={}", blob.digest),
        )?)
        .headers(session_headers(headers)?)
        .body(body)
        .send()
        .await
        .map_err(|error| ImageArchiveError::Receiver {
            message: error.to_string(),
        })?;
    if response.status() == StatusCode::CREATED {
        return Ok(());
    }
    Err(ImageArchiveError::Receiver {
        message: format!("receiver blob upload returned {}", response.status()),
    })
}

async fn upload_manifest(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    headers: &BTreeMap<String, String>,
    reference: &str,
    manifest: Vec<u8>,
) -> Result<String, ImageArchiveError> {
    let response = client
        .put(join_endpoint(endpoint, &format!("manifests/{reference}"))?)
        .headers(session_headers(headers)?)
        .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_MEDIA_TYPE)
        .body(manifest)
        .send()
        .await
        .map_err(|error| ImageArchiveError::Receiver {
            message: error.to_string(),
        })?;
    if response.status() != StatusCode::CREATED {
        return Err(ImageArchiveError::Receiver {
            message: format!("receiver manifest upload returned {}", response.status()),
        });
    }
    let digest = response
        .headers()
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ImageArchiveError::Receiver {
            message: "receiver manifest upload did not return docker-content-digest".into(),
        })?
        .to_string();
    Ok(digest)
}

fn build_docker_load_archive(
    registry: &ImageRegistry,
    manifest: OwnedDistributionManifest,
    repo_tags: Vec<String>,
    output_path: &Path,
) -> Result<(), ImageArchiveError> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create received archive output dir", error))?;
    }
    let file =
        File::create(output_path).map_err(|error| io_error("create received archive", error))?;
    let mut builder = Builder::new(file);
    let config_name = archive_config_name(&manifest.config.digest)?;
    let config_path = registry
        .blob_path_for_digest(&manifest.config.digest)
        .map_err(|error| ImageArchiveError::Registry {
            message: error.to_string(),
        })?
        .ok_or_else(|| ImageArchiveError::Registry {
            message: format!("blob '{}' was not found", manifest.config.digest),
        })?;
    append_file(&mut builder, &config_path, &config_name)?;

    let mut layer_names = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let layer_name = archive_layer_name(&layer.digest)?;
        let layer_path = registry
            .blob_path_for_digest(&layer.digest)
            .map_err(|error| ImageArchiveError::Registry {
                message: error.to_string(),
            })?
            .ok_or_else(|| ImageArchiveError::Registry {
                message: format!("blob '{}' was not found", layer.digest),
            })?;
        append_file(&mut builder, &layer_path, &layer_name)?;
        layer_names.push(layer_name);
    }

    let load_manifest = vec![DockerLoadManifestEntry {
        config: &config_name,
        repo_tags: &repo_tags,
        layers: layer_names,
    }];
    let body =
        serde_json::to_vec(&load_manifest).map_err(|error| ImageArchiveError::InvalidManifest {
            message: error.to_string(),
        })?;
    append_bytes(&mut builder, "manifest.json", &body)?;
    builder
        .finish()
        .map_err(|error| io_error("finish received archive", error))
}

fn append_file(
    builder: &mut Builder<File>,
    path: &Path,
    name: &str,
) -> Result<(), ImageArchiveError> {
    let mut file =
        File::open(path).map_err(|error| io_error("open received archive blob", error))?;
    builder
        .append_file(name, &mut file)
        .map_err(|error| io_error("append received archive blob", error))
}

fn append_bytes(
    builder: &mut Builder<File>,
    name: &str,
    body: &[u8],
) -> Result<(), ImageArchiveError> {
    let mut header = Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, name, body)
        .map_err(|error| io_error("append received archive manifest", error))
}

fn archive_config_name(digest: &str) -> Result<String, ImageArchiveError> {
    Ok(format!("{}.json", digest_hex(digest)?))
}

fn archive_layer_name(digest: &str) -> Result<String, ImageArchiveError> {
    Ok(format!("{}/layer.tar", digest_hex(digest)?))
}

fn digest_hex(digest: &str) -> Result<String, ImageArchiveError> {
    digest
        .strip_prefix("sha256:")
        .map(ToOwned::to_owned)
        .ok_or_else(|| ImageArchiveError::InvalidManifest {
            message: format!("digest '{digest}' is not sha256"),
        })
}

fn join_endpoint(endpoint: reqwest::Url, suffix: &str) -> Result<reqwest::Url, ImageArchiveError> {
    endpoint
        .join(suffix.trim_start_matches('/'))
        .map_err(|error| ImageArchiveError::InvalidEndpoint {
            message: error.to_string(),
        })
}

fn session_headers(
    headers: &BTreeMap<String, String>,
) -> Result<reqwest::header::HeaderMap, ImageArchiveError> {
    let mut result = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            ImageArchiveError::Receiver {
                message: format!("invalid receiver header name '{name}': {error}"),
            }
        })?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
            ImageArchiveError::Receiver {
                message: format!("invalid receiver header value for '{name}': {error}"),
            }
        })?;
        result.insert(name, value);
    }
    Ok(result)
}

fn format_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in bytes.as_ref() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("write digest into string");
    }
    encoded
}

fn deserialize_nullable_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

fn io_error(operation: &'static str, error: io::Error) -> ImageArchiveError {
    ImageArchiveError::Io {
        operation,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, Request};
    use ployz_types::model::MachineId;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn image_archive_parser_extracts_config_layers_and_tags() {
        let work_dir = unique_temp_dir("ployz-image-archive-parse");
        let archive = docker_archive_bytes();

        let parsed = parse_image_archive(Box::pin(std::io::Cursor::new(archive)), &work_dir)
            .await
            .expect("parse archive");

        assert_eq!(parsed.repo_tags, vec!["example/app:latest"]);
        assert_eq!(parsed.layers.len(), 2);
        assert_eq!(
            parsed.config.size,
            br#"{"architecture":"amd64"}"#.len() as u64
        );
        assert!(!parsed.distribution_manifest.is_empty());
    }

    #[tokio::test]
    async fn image_archive_parser_accepts_null_repo_tags() {
        let work_dir = unique_temp_dir("ployz-image-archive-null-tags");
        let archive = docker_archive_bytes_with_manifest(
            br#"[{"Config":"config.json","RepoTags":null,"Layers":["layer-one/layer.tar"]}]"#,
        );

        let parsed = parse_image_archive(Box::pin(std::io::Cursor::new(archive)), &work_dir)
            .await
            .expect("parse archive");

        assert!(parsed.repo_tags.is_empty());
        assert_eq!(parsed.layers.len(), 1);
    }

    #[tokio::test]
    async fn image_archive_parser_rejects_missing_config_member() {
        let work_dir = unique_temp_dir("ployz-image-archive-missing-config");
        let archive = docker_archive_bytes_with_manifest(
            br#"[{"Config":"missing.json","RepoTags":["example/app:latest"],"Layers":["layer-one/layer.tar"]}]"#,
        );

        let error = parse_image_archive(Box::pin(std::io::Cursor::new(archive)), &work_dir)
            .await
            .expect_err("missing config should fail");

        assert!(matches!(error, ImageArchiveError::MissingMember { .. }));
    }

    #[tokio::test]
    async fn upload_adapter_skips_existing_receiver_blobs() {
        let root = unique_temp_dir("ployz-image-archive-upload");
        let registry = ImageRegistry::new(root.clone());
        let session = registry
            .register_session("op-1", MachineId::new("founder"), "ployz/op-1")
            .await;
        let router = registry.clone().router();
        let work_dir = unique_temp_dir("ployz-image-archive-upload-work");
        let parsed = parse_image_archive(
            Box::pin(std::io::Cursor::new(docker_archive_bytes())),
            &work_dir,
        )
        .await
        .expect("parse archive");
        let digest = parsed.config.digest.clone();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/v2/ployz/op-1/blobs/uploads/?digest={digest}"))
            .header("x-ployz-image-operation", "op-1")
            .header("x-ployz-source-machine", "founder")
            .header("x-ployz-image-session", session.token.as_str())
            .body(axum::body::Body::from(
                tokio::fs::read(&parsed.config.path)
                    .await
                    .expect("config bytes"),
            ))
            .expect("request");
        let response = router.oneshot(request).await.expect("preload blob");
        assert_eq!(response.status(), StatusCode::CREATED);

        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("addr"),
            registry,
        )
        .await
        .expect("serve registry");
        let endpoint = format!("http://{}/v2/ployz/op-1", listener.bind_addr());
        let mut headers = BTreeMap::new();
        headers.insert("x-ployz-image-operation".into(), "op-1".into());
        headers.insert("x-ployz-source-machine".into(), "founder".into());
        headers.insert("x-ployz-image-session".into(), session.token);

        let report = upload_archive_to_receiver(
            &ImageReceiveSessionPayload {
                target_machine: MachineId::new("target"),
                endpoint,
                token: String::new(),
                expires_at_unix_secs: 1,
                headers,
            },
            "op-1",
            &parsed,
        )
        .await
        .expect("upload archive");

        assert_eq!(report.skipped_blobs, 1);
        assert_eq!(report.uploaded_blobs, 2);
        listener.shutdown().await;
    }

    fn docker_archive_bytes() -> Vec<u8> {
        docker_archive_bytes_with_manifest(
            br#"[{"Config":"config.json","RepoTags":["example/app:latest"],"Layers":["layer-one/layer.tar","layer-two/layer.tar"]}]"#,
        )
    }

    fn docker_archive_bytes_with_manifest(manifest: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut builder = Builder::new(&mut output);
            append_test_member(&mut builder, "config.json", br#"{"architecture":"amd64"}"#);
            append_test_member(&mut builder, "layer-one/layer.tar", b"layer-one");
            append_test_member(&mut builder, "layer-two/layer.tar", b"layer-two");
            append_test_member(&mut builder, "manifest.json", manifest);
            builder.finish().expect("finish tar");
        }
        output
    }

    fn append_test_member(builder: &mut Builder<&mut Vec<u8>>, name: &str, body: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, body)
            .expect("append test member");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}
