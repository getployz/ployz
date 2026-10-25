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

use crate::docker_image::is_not_found;

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
                    Err(error) if is_not_found(&error) => None,
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

    /// A Docker that starts `ok`, has never heard of `gone`, and refuses anything else.
    async fn docker(started: Arc<Mutex<Vec<String>>>) -> Docker {
        async fn start(
            State(started): State<Arc<Mutex<Vec<String>>>>,
            UrlPath(path): UrlPath<String>,
        ) -> StatusCode {
            let Some(id) = path
                .strip_suffix("/start")
                .and_then(|path| path.rsplit('/').next())
            else {
                return StatusCode::BAD_REQUEST;
            };
            started.lock().unwrap().push(id.to_owned());
            match id {
                "ok" => StatusCode::NO_CONTENT,
                "gone" => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            }
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
}
