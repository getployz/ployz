//! Machine-local staging for the keeper-first `ployzd` upgrade route.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use ployz_core::{
    MachineUpgradeRefusal, MachineUpgradeReply, MachineUpgradeRequest, MachineUpgradeSupervisor,
};
use ployz_host_runner::{
    ArtifactStoreError, ArtifactVerificationError, VerifiedArtifactFile, verify_artifact_file,
};
use tokio::io::AsyncWriteExt;

use crate::roles::upgrade::{UpgradeRequest, UpgradeResponse, request as keeper_request};

use super::mutations::{decode_request, typed_response};
use super::server::{ApiService, HttpBody, corrosion_unavailable_response};

const UPGRADE_DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const UPGRADE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_UPGRADE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

pub(super) async fn handle_machine_upgrade(
    service: &ApiService,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let request = match decode_request::<MachineUpgradeRequest>(request.into_body()).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Some(refusal) = unsupported_supervisor_refusal(service.upgrade_supervisor) {
        return typed_response(StatusCode::CONFLICT, &refusal);
    }
    if let Err(refusal) = download_and_stage(service, &request).await {
        return upgrade_refusal_response(refusal);
    }
    match keeper_request(
        &service.keeper_upgrade_socket_path,
        &UpgradeRequest::Arm {
            sha256: request.sha256.clone(),
        },
    )
    .await
    {
        Ok(UpgradeResponse::Armed) => commit_after_response_body(
            MachineUpgradeReply {
                version: request.version,
                sha256: request.sha256,
            },
            service.keeper_upgrade_socket_path.clone(),
        ),
        Ok(UpgradeResponse::Committed) => {
            upgrade_refusal_response(MachineUpgradeRefusal::KeeperRefused {
                message: "Keeper committed before it armed the requested candidate".to_owned(),
            })
        }
        Ok(UpgradeResponse::Refused { message }) => {
            upgrade_refusal_response(MachineUpgradeRefusal::KeeperRefused { message })
        }
        Err(error) => upgrade_refusal_response(MachineUpgradeRefusal::KeeperRefused {
            message: error.to_string(),
        }),
    }
}

async fn download_and_stage(
    service: &ApiService,
    request: &MachineUpgradeRequest,
) -> Result<(), MachineUpgradeRefusal> {
    let artifacts = service.upgrade_store.artifacts_path();
    tokio::fs::create_dir_all(&artifacts)
        .await
        .map_err(|error| MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        })?;
    let temporary = tempfile::NamedTempFile::new_in(&artifacts).map_err(|error| {
        MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        }
    })?;
    let temporary = temporary.into_temp_path();
    download_to_path(request.url.as_str(), &temporary).await?;
    verify_and_stage(service, &temporary, request)
}

fn verify_and_stage(
    service: &ApiService,
    candidate: &std::path::Path,
    request: &MachineUpgradeRequest,
) -> Result<(), MachineUpgradeRefusal> {
    let verified = verify_candidate(candidate, &request.sha256)?;
    service
        .upgrade_store
        .stage(&verified)
        .map_err(staging_refusal)?;
    Ok(())
}

fn verify_candidate(
    candidate: &std::path::Path,
    expected: &ployz_core::install::InstallSha256Digest,
) -> Result<VerifiedArtifactFile, MachineUpgradeRefusal> {
    match verify_artifact_file(candidate, expected) {
        Ok(verified) => Ok(verified),
        Err(ArtifactVerificationError::DigestMismatch {
            expected, actual, ..
        }) => Err(MachineUpgradeRefusal::Sha256Mismatch {
            expected,
            got: actual,
        }),
        Err(error) => Err(MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        }),
    }
}

fn unsupported_supervisor_refusal(
    supervisor: MachineUpgradeSupervisor,
) -> Option<MachineUpgradeRefusal> {
    match supervisor {
        MachineUpgradeSupervisor::Systemd => None,
        MachineUpgradeSupervisor::OpenRc => {
            Some(MachineUpgradeRefusal::UnsupportedSupervisor { supervisor })
        }
    }
}

async fn download_to_path(url: &str, path: &std::path::Path) -> Result<(), MachineUpgradeRefusal> {
    let client = reqwest::Client::builder()
        .connect_timeout(UPGRADE_DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(UPGRADE_DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| MachineUpgradeRefusal::DownloadFailed {
            message: error.to_string(),
        })?;
    let response =
        client
            .get(url)
            .send()
            .await
            .map_err(|error| MachineUpgradeRefusal::DownloadFailed {
                message: error.to_string(),
            })?;
    if !response.status().is_success() {
        return Err(MachineUpgradeRefusal::DownloadFailed {
            message: format!("HTTPS server returned {}", response.status()),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_UPGRADE_ARTIFACT_BYTES)
    {
        return Err(MachineUpgradeRefusal::DownloadFailed {
            message: format!("artifact exceeds {MAX_UPGRADE_ARTIFACT_BYTES}-byte limit"),
        });
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        }
    })?;
    let mut stream = response.bytes_stream();
    let mut total = 0_u64;
    while let Some(next) = stream.next().await {
        let bytes = next.map_err(|error| MachineUpgradeRefusal::DownloadFailed {
            message: error.to_string(),
        })?;
        let Some(next_total) = total.checked_add(bytes.len() as u64) else {
            return Err(MachineUpgradeRefusal::DownloadFailed {
                message: "artifact size overflowed the download limit".to_owned(),
            });
        };
        if next_total > MAX_UPGRADE_ARTIFACT_BYTES {
            return Err(MachineUpgradeRefusal::DownloadFailed {
                message: format!("artifact exceeds {MAX_UPGRADE_ARTIFACT_BYTES}-byte limit"),
            });
        }
        file.write_all(&bytes)
            .await
            .map_err(|error| MachineUpgradeRefusal::StagingFailed {
                message: error.to_string(),
            })?;
        total = next_total;
    }
    file.flush()
        .await
        .map_err(|error| MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        })?;
    file.sync_all()
        .await
        .map_err(|error| MachineUpgradeRefusal::StagingFailed {
            message: error.to_string(),
        })
}

fn staging_refusal(error: ArtifactStoreError) -> MachineUpgradeRefusal {
    MachineUpgradeRefusal::StagingFailed {
        message: error.to_string(),
    }
}

fn upgrade_refusal_response(refusal: MachineUpgradeRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        MachineUpgradeRefusal::UnsupportedSupervisor { .. } => StatusCode::CONFLICT,
        MachineUpgradeRefusal::DownloadFailed { .. }
        | MachineUpgradeRefusal::Sha256Mismatch { .. }
        | MachineUpgradeRefusal::StagingFailed { .. } => StatusCode::BAD_GATEWAY,
        MachineUpgradeRefusal::KeeperRefused { .. } => StatusCode::CONFLICT,
    };
    typed_response(status, &refusal)
}

fn commit_after_response_body(reply: MachineUpgradeReply, socket: PathBuf) -> Response<HttpBody> {
    let body = match serde_json::to_vec(&reply) {
        Ok(body) => Bytes::from(body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode machine upgrade acknowledgement");
            return corrosion_unavailable_response();
        }
    };
    let stream = futures_util::stream::unfold(
        (Some(body), Some(socket)),
        |(body, socket)| async move {
            if let Some(body) = body {
                return Some((Ok::<_, Infallible>(Frame::data(body)), (None, socket)));
            }
            let socket = socket?;
            match keeper_request(&socket, &UpgradeRequest::Commit).await {
                Ok(UpgradeResponse::Committed) => {
                    tracing::info!("Keeper accepted armed machine upgrade commit");
                }
                Ok(UpgradeResponse::Armed) | Ok(UpgradeResponse::Refused { .. }) | Err(_) => {
                    tracing::error!(
                        "Keeper did not commit the armed machine upgrade after acknowledgement output"
                    );
                }
            }
            None
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use ployz_core::install::InstallArtifactVersion;

    use super::*;
    use crate::roles::upgrade::{read_request, write_response};

    #[tokio::test]
    async fn acknowledgement_body_precedes_the_keeper_commit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("socket listener");
        let (committed, mut observed_commit) = tokio::sync::mpsc::channel(1);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("one connection");
            assert_eq!(
                read_request(&mut stream).await.expect("commit request"),
                UpgradeRequest::Commit
            );
            committed.send(()).await.expect("commit observation");
            write_response(&mut stream, &UpgradeResponse::Committed)
                .await
                .expect("commit response");
        });
        let reply = MachineUpgradeReply {
            version: InstallArtifactVersion::try_new("1.2.3").expect("version"),
            sha256: ployz_core::install::InstallSha256Digest::try_new("a".repeat(64))
                .expect("digest"),
        };
        let mut body = commit_after_response_body(reply, socket).into_body();

        let frame = body
            .frame()
            .await
            .expect("acknowledgement frame")
            .expect("infallible body");
        assert!(frame.into_data().is_ok());
        assert!(observed_commit.try_recv().is_err());

        assert!(body.frame().await.is_none());
        observed_commit
            .recv()
            .await
            .expect("commit after body output");
        server.await.expect("server task");
    }

    #[test]
    fn openrc_is_refused_before_any_upgrade_download_or_keeper_effect() {
        assert_eq!(
            unsupported_supervisor_refusal(MachineUpgradeSupervisor::OpenRc),
            Some(MachineUpgradeRefusal::UnsupportedSupervisor {
                supervisor: MachineUpgradeSupervisor::OpenRc,
            })
        );
    }

    #[test]
    fn hash_mismatch_reports_expected_and_got_before_keeper_arm() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let candidate = directory.path().join("candidate");
        std::fs::write(&candidate, b"candidate bytes").expect("candidate bytes");
        let expected =
            ployz_core::install::InstallSha256Digest::try_new("a".repeat(64)).expect("digest");

        assert!(matches!(
            verify_candidate(&candidate, &expected),
            Err(MachineUpgradeRefusal::Sha256Mismatch { expected: actual_expected, got })
                if actual_expected == expected && got != actual_expected
        ));
    }
}
