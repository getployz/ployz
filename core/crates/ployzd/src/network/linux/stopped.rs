//! The bridge members a start stopped so it could move them, written down before
//! each stop. A start that dies, or leaves no bridge, before starting them again
//! leaves the note for the next start, which starts them once a bridge stands.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use bollard::Docker;
use futures_util::StreamExt as _;

use super::DOCKER_NETWORK_NAME;

const FILE: &str = "bridge-stopped-members.json";

pub(super) struct Stopped {
    path: PathBuf,
    ids: BTreeSet<String>,
}

impl Stopped {
    /// Picks up the members an earlier start stopped and never started.
    pub(super) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let ids = match std::fs::read(&path) {
            Ok(body) => serde_json::from_slice(&body).unwrap_or_else(|error| {
                eprintln!(
                    "ignoring {path}, which names no containers: {error}",
                    path = path.display(),
                );
                BTreeSet::new()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeSet::new(),
            Err(error) => {
                eprintln!(
                    "could not read the containers an earlier start stopped from {path}: {error}",
                    path = path.display(),
                );
                BTreeSet::new()
            }
        };
        Self { path, ids }
    }

    /// Writes `id` down. Stop it only once this succeeds.
    pub(super) fn note(&mut self, id: &str) -> io::Result<()> {
        if self.ids.insert(id.to_owned())
            && let Err(error) = self.write()
        {
            self.ids.remove(id);
            return Err(error);
        }
        Ok(())
    }

    /// Starts every member written down, a few at a time since each start takes Docker
    /// a moment. Keeps the ones Docker couldn't start for the next start.
    pub(super) async fn start(mut self, docker: &Docker) {
        let ids = std::mem::take(&mut self.ids);
        self.ids = futures_util::stream::iter(ids)
            .map(|id| async move {
                match docker.start_container(&id, None).await {
                    Ok(()) => None,
                    Err(error) if gone(&error) => None,
                    Err(error) => {
                        eprintln!(
                            "container {id} stayed stopped after rejoining the {DOCKER_NETWORK_NAME} network: {error}",
                            error = ployz_core::error_chain::inline(&error),
                        );
                        Some(id)
                    }
                }
            })
            .buffer_unordered(8)
            .filter_map(std::future::ready)
            .collect()
            .await;
        let written = if self.ids.is_empty() {
            match std::fs::remove_file(&self.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                removed => removed,
            }
        } else {
            self.write()
        };
        if let Err(error) = written {
            eprintln!(
                "could not update {path}: {error}",
                path = self.path.display(),
            );
        }
    }

    fn write(&self) -> io::Result<()> {
        let body = serde_json::to_vec(&self.ids).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.path, &body, 0o600)
    }
}

/// Docker answers 404 for a container whose network it can't find too, and that
/// container is still there to start once its network is back.
fn gone(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError { status_code: 404, message }
            if message.starts_with("No such container")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        extract::{Path as UrlPath, State},
        http::StatusCode,
        routing::post,
    };

    use super::*;

    /// A Docker that starts `ok`, has never heard of `gone`, can't find the network of
    /// `netless`, and refuses anything else.
    async fn docker(started: Arc<Mutex<Vec<String>>>) -> Docker {
        async fn start(
            State(started): State<Arc<Mutex<Vec<String>>>>,
            UrlPath(path): UrlPath<String>,
        ) -> (StatusCode, axum::Json<serde_json::Value>) {
            let Some(id) = path
                .strip_suffix("/start")
                .and_then(|path| path.rsplit('/').next())
            else {
                return (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({})));
            };
            started.lock().unwrap().push(id.to_owned());
            let (status, message) = match id {
                "ok" => (StatusCode::OK, String::new()),
                "gone" => (StatusCode::NOT_FOUND, format!("No such container: {id}")),
                "netless" => (
                    StatusCode::NOT_FOUND,
                    "failed to set up container networking: network 6d85cc7fe1c5 not found".into(),
                ),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "refused".into()),
            };
            (
                status,
                axum::Json(serde_json::json!({ "message": message })),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/{*path}", post(start))
            .with_state(started);
        tokio::spawn(axum::serve(listener, app).into_future());
        Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_start_that_died_leaves_its_stopped_members_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("ok").unwrap();
        stopped.note("gone").unwrap();
        drop(stopped);

        let started = Arc::new(Mutex::new(Vec::new()));
        Stopped::load(dir.path())
            .start(&docker(started.clone()).await)
            .await;
        let mut started = started.lock().unwrap().clone();
        started.sort();
        assert_eq!(started, ["gone", "ok"]);
        assert!(!dir.path().join(FILE).exists());
    }

    #[tokio::test]
    async fn a_member_docker_would_not_start_stays_written_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("ok").unwrap();
        stopped.note("refused").unwrap();
        stopped
            .start(&docker(Arc::new(Mutex::new(Vec::new()))).await)
            .await;
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from(["refused".into()])
        );
    }

    #[test]
    fn a_start_that_stopped_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        drop(Stopped::load(dir.path()));
        assert!(!dir.path().join(FILE).exists());
    }

    #[tokio::test]
    async fn a_member_whose_network_docker_cannot_find_stays_written_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("netless").unwrap();
        stopped.note("gone").unwrap();
        stopped
            .start(&docker(Arc::new(Mutex::new(Vec::new()))).await)
            .await;
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from(["netless".into()])
        );
    }
}
