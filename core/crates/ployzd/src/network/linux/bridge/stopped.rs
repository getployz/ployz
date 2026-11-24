//! The bridge members a start stopped so it could move them, written down before
//! each stop. A start that dies, or leaves no bridge, before starting them again
//! leaves the note for the next start, which starts them once a bridge stands.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use bollard::Docker;
use ployz_core::{ContainerId, CopyRotation};

use super::DOCKER_NETWORK_NAME;
use crate::docker::MachineSpecStore;

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

    /// Starts every member written down that Docker shows on the bridge, one at a time, so
    /// each start's check of its Volume sees the one before it running. One the pin pass
    /// left off the bridge stays stopped and written down: Docker would run it detached, or
    /// on an address it picks. The next start's pin pass tries to attach it again. Copies in
    /// rotation go first, so of two holders of one Volume the routed copy starts, not a
    /// Pending one a failed Deploy left. Once an In copy fails to start, for any reason, copies out of rotation wait
    /// for the next start, so none of them takes a Volume its routed copy needs. Drops
    /// one Docker no longer has, and keeps every other it couldn't start for the next
    /// start: one refused beside a running holder starts there once that holder is gone.
    pub(super) async fn start(mut self, docker: &Docker) {
        let ids = std::mem::take(&mut self.ids);
        let mut routed_stayed = false;
        for (id, routed) in self.routed_first(ids).await {
            if routed_stayed && !routed {
                self.ids.insert(id);
                continue;
            }
            let started = match docker.inspect_container(&id, None).await {
                Ok(member) if !on_bridge(&member) => {
                    eprintln!(
                        "container {id} stays stopped until it rejoins the {DOCKER_NETWORK_NAME} network"
                    );
                    routed_stayed |= routed;
                    self.ids.insert(id);
                    continue;
                }
                Ok(member) => crate::docker::holder::start(docker, &member, None).await,
                Err(error) => Err(error.into()),
            };
            match started {
                Ok(()) => continue,
                Err(crate::docker::Error::Docker(error)) if gone(&error) => continue,
                Err(error @ crate::docker::Error::VolumeHeld { .. }) => {
                    eprintln!("container {id} stays stopped for now: {error}");
                }
                Err(error) => eprintln!(
                    "container {id} stayed stopped after rejoining the {DOCKER_NETWORK_NAME} network: {error}",
                    error = ployz_core::error_chain::inline(&error),
                ),
            }
            routed_stayed |= routed;
            self.ids.insert(id);
        }
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

    /// `ids` with In copies first, then Held ones, then the rest, each part in ID order,
    /// each with whether it is In. The rotation each copy last had is in the Machine's
    /// spec store, beside the note. Unread rotations count every copy as In.
    async fn routed_first(&self, ids: BTreeSet<String>) -> Vec<(String, bool)> {
        let all_routed = |ids: Vec<String>| ids.into_iter().map(|id| (id, true)).collect();
        let ids = Vec::from_iter(ids);
        let Some(dir) = self.path.parent().filter(|_| ids.len() > 1) else {
            return all_routed(ids);
        };
        let read = async {
            let specs = MachineSpecStore::open(dir.join(MachineSpecStore::FILE)).await?;
            let parsed = ids.iter().filter_map(|id| ContainerId::parse(id).ok());
            specs.rotations(parsed.collect()).await
        };
        let rotations = match read.await {
            Ok(rotations) => rotations,
            Err(error) => {
                eprintln!(
                    "starting stopped containers in ID order: their rotations are unreadable: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return all_routed(ids);
            }
        };
        let rank = |id: &String| match ContainerId::parse(id)
            .ok()
            .and_then(|id| rotations.get(&id))
        {
            Some(CopyRotation::In) => 0,
            Some(CopyRotation::Held(_)) => 1,
            _ => 2,
        };
        let mut ranked: Vec<_> = ids.into_iter().map(|id| (rank(&id), id)).collect();
        ranked.sort();
        ranked
            .into_iter()
            .map(|(rank, id)| (id, rank == 0))
            .collect()
    }

    fn write(&self) -> io::Result<()> {
        let body = serde_json::to_vec(&self.ids).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.path, &body, 0o600)
    }
}

/// Whether Docker shows `member` with an endpoint on the bridge.
fn on_bridge(member: &bollard::models::ContainerInspectResponse) -> bool {
    member
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .is_some_and(|networks| networks.contains_key(DOCKER_NETWORK_NAME))
}

fn gone(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError { status_code, message }
            if ployz_decide::bridge::gone(*status_code, message)
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use axum::{
        Router,
        extract::{Path as UrlPath, State},
        http::StatusCode,
        routing::post,
    };

    use super::*;

    /// What the fake Docker has started, and the Service Containers it holds on one
    /// Volume by ID, with each one's state.
    #[derive(Default)]
    struct Fake {
        started: Mutex<Vec<String>>,
        /// Wakes a holder's start when a second start arrives.
        second: tokio::sync::Notify,
        holders: Mutex<BTreeMap<String, &'static str>>,
    }

    impl Fake {
        fn holding(holders: &[(&str, &'static str)]) -> Arc<Self> {
            Arc::new(Self {
                started: Mutex::default(),
                second: tokio::sync::Notify::new(),
                holders: Mutex::new(
                    holders
                        .iter()
                        .map(|(id, state)| ((*id).to_owned(), *state))
                        .collect(),
                ),
            })
        }

        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
    }

    /// A Docker that starts `ok`, has never heard of `gone`, can't find the network of
    /// `netless`, and refuses anything else but the holders. A holder's start runs it
    /// only once a second start arrives, or after a while, as a slow Docker would.
    async fn docker(fake: Arc<Fake>) -> Docker {
        async fn start(
            State(fake): State<Arc<Fake>>,
            UrlPath(path): UrlPath<String>,
        ) -> (StatusCode, axum::Json<serde_json::Value>) {
            let Some(id) = path
                .strip_suffix("/start")
                .and_then(|path| path.rsplit('/').next())
            else {
                return (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({})));
            };
            let second = fake.second.notified();
            let starts = {
                let mut started = fake.started.lock().unwrap();
                started.push(id.to_owned());
                started.len()
            };
            if starts >= 2 {
                fake.second.notify_waiters();
            }
            if fake.holders.lock().unwrap().contains_key(id) {
                if starts < 2 {
                    let _ = tokio::time::timeout(Duration::from_millis(200), second).await;
                }
                fake.holders
                    .lock()
                    .unwrap()
                    .insert(id.to_owned(), "running");
                return (StatusCode::NO_CONTENT, axum::Json(serde_json::json!({})));
            }
            let (status, message) = match id {
                "ok" | "detached" => (StatusCode::OK, String::new()),
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
        async fn read(
            State(fake): State<Arc<Fake>>,
            UrlPath(path): UrlPath<String>,
        ) -> (StatusCode, axum::Json<serde_json::Value>) {
            let holders = fake.holders.lock().unwrap().clone();
            if path.ends_with("containers/json") {
                let listed: Vec<_> = holders
                    .keys()
                    .map(|id| serde_json::json!({ "Id": id }))
                    .collect();
                return (StatusCode::OK, axum::Json(serde_json::json!(listed)));
            }
            let id = path.trim_end_matches("/json").rsplit('/').next().unwrap();
            if let Some(state) = holders.get(id) {
                return (
                    StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "Id": id,
                        "Name": format!("/{id}"),
                        "State": {"Status": state},
                        "Config": {"Labels": {
                            "ployz.managed": "true",
                            "ployz.namespace": "app",
                            "ployz.service.id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "ployz.service.name": "web",
                        }},
                        "Mounts": [{"Type": "volume", "Name": "app_data", "Driver": "ployz"}],
                        "NetworkSettings": {"Networks": {"ployz": {}}},
                    })),
                );
            }
            match id {
                "gone" => (
                    StatusCode::NOT_FOUND,
                    axum::Json(
                        serde_json::json!({ "message": format!("No such container: {id}") }),
                    ),
                ),
                "detached" => (StatusCode::OK, axum::Json(serde_json::json!({ "Id": id }))),
                _ => (
                    StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "Id": id,
                        "NetworkSettings": {"Networks": {"ployz": {}}},
                    })),
                ),
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/{*path}", post(start).get(read))
            .with_state(fake);
        tokio::spawn(axum::serve(listener, app).into_future());
        Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap()
    }

    /// Two stopped holders of one Volume, both written down: one starts, and the other
    /// waits in the note.
    #[tokio::test]
    async fn two_written_down_holders_of_one_volume_start_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("a").unwrap();
        stopped.note("b").unwrap();
        let fake = Fake::holding(&[("a", "exited"), ("b", "exited")]);
        stopped.start(&docker(fake.clone()).await).await;
        assert_eq!(fake.started(), ["a"]);
        assert_eq!(Stopped::load(dir.path()).ids, BTreeSet::from(["b".into()]));
    }

    /// Of two written-down holders of one Volume, the copy in rotation starts even when
    /// a Pending copy a failed Deploy left has the lower ID.
    #[tokio::test]
    async fn the_routed_holder_of_a_volume_starts_before_a_pending_one() {
        let dir = tempfile::tempdir().unwrap();
        let pending = "1".repeat(64);
        let routed = "2".repeat(64);
        drop(
            MachineSpecStore::open(dir.path().join(MachineSpecStore::FILE))
                .await
                .unwrap(),
        );
        rusqlite::Connection::open(dir.path().join(MachineSpecStore::FILE))
            .unwrap()
            .execute(
                "INSERT INTO rotations (id, rotation) VALUES (?1, ?2)",
                [&routed, &serde_json::to_string(&CopyRotation::In).unwrap()],
            )
            .unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note(&pending).unwrap();
        stopped.note(&routed).unwrap();
        let fake = Fake::holding(&[(&pending, "exited"), (&routed, "exited")]);
        stopped.start(&docker(fake.clone()).await).await;
        assert_eq!(fake.started(), [routed]);
        assert_eq!(Stopped::load(dir.path()).ids, BTreeSet::from([pending]));
    }

    /// When the routed copy fails to start, a Pending copy waits for the next start
    /// rather than take the Volume the routed copy needs.
    #[tokio::test]
    async fn a_pending_copy_waits_when_the_routed_one_fails_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let pending = "1".repeat(64);
        let routed = "3".repeat(64);
        drop(
            MachineSpecStore::open(dir.path().join(MachineSpecStore::FILE))
                .await
                .unwrap(),
        );
        rusqlite::Connection::open(dir.path().join(MachineSpecStore::FILE))
            .unwrap()
            .execute(
                "INSERT INTO rotations (id, rotation) VALUES (?1, ?2)",
                [&routed, &serde_json::to_string(&CopyRotation::In).unwrap()],
            )
            .unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note(&pending).unwrap();
        stopped.note(&routed).unwrap();
        let fake = Fake::holding(&[(&pending, "exited")]);
        stopped.start(&docker(fake.clone()).await).await;
        assert_eq!(fake.started(), std::slice::from_ref(&routed));
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from([pending, routed])
        );
    }

    /// A member refused beside a running holder stays written down, and starts at the
    /// next start once that holder stopped.
    #[tokio::test]
    async fn a_member_beside_a_running_holder_starts_once_it_stops() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("held").unwrap();
        stopped.note("ok").unwrap();
        let fake = Fake::holding(&[("held", "exited"), ("writer", "running")]);
        let docker = docker(fake.clone()).await;
        stopped.start(&docker).await;
        assert_eq!(fake.started(), ["ok"]);

        fake.holders
            .lock()
            .unwrap()
            .insert("writer".into(), "exited");
        Stopped::load(dir.path()).start(&docker).await;
        assert_eq!(fake.started(), ["ok", "held"]);
        assert!(!dir.path().join(FILE).exists());
    }

    #[tokio::test]
    async fn a_start_that_died_leaves_its_stopped_members_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("ok").unwrap();
        stopped.note("gone").unwrap();
        drop(stopped);

        let fake = Arc::new(Fake::default());
        Stopped::load(dir.path())
            .start(&docker(fake.clone()).await)
            .await;
        assert_eq!(fake.started(), ["ok"]);
        assert!(!dir.path().join(FILE).exists());
    }

    #[tokio::test]
    async fn a_member_docker_would_not_start_stays_written_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("ok").unwrap();
        stopped.note("refused").unwrap();
        stopped.start(&docker(Arc::default()).await).await;
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from(["refused".into()])
        );
    }

    /// A member the pin pass left off the bridge stays stopped and written down, so the
    /// next start attaches it before it runs.
    #[tokio::test]
    async fn a_member_off_the_bridge_stays_stopped_and_written_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut stopped = Stopped::load(dir.path());
        stopped.note("detached").unwrap();
        stopped.note("ok").unwrap();
        let fake = Arc::new(Fake::default());
        stopped.start(&docker(fake.clone()).await).await;
        assert_eq!(fake.started(), ["ok"]);
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from(["detached".into()])
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
        stopped.start(&docker(Arc::default()).await).await;
        assert_eq!(
            Stopped::load(dir.path()).ids,
            BTreeSet::from(["netless".into()])
        );
    }
}
