//! Docker v1 Volume plugin backed by bounded ZFS datasets.

use std::{
    collections::BTreeMap, fmt, io, os::unix::net::UnixListener as StdUnixListener, str::FromStr,
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::Response,
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;

mod capacity;
mod pool;
mod removal;
mod storage;

use storage::{
    CapacityAdmission, DATASET_ROOT, Dataset, MOUNT_ROOT, VolumeStorage, checked_command,
    parse_size,
};

type Result<T> = std::result::Result<T, VolumeError>;

#[derive(Debug, thiserror::Error)]
enum VolumeError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Capacity(#[from] ployz_core::StorageCapacityError),
}

impl From<String> for VolumeError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for VolumeError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

struct DockerVolumeName(String);

impl FromStr for DockerVolumeName {
    type Err = VolumeError;

    fn from_str(name: &str) -> Result<Self> {
        let mut bytes = name.bytes();
        if !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(format!("invalid Docker Volume name {name:?}").into());
        }
        Ok(Self(name.to_owned()))
    }
}

impl fmt::Display for DockerVolumeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl DockerVolumeName {
    fn mountpoint(&self) -> String {
        format!("{MOUNT_ROOT}/{self}")
    }
}

#[derive(Deserialize)]
struct CreateRequest {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Opts", default)]
    options: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct VolumeRequest {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    #[serde(rename = "Err")]
    error: String,
}

#[derive(Serialize)]
struct MountResponse {
    #[serde(rename = "Mountpoint")]
    mountpoint: String,
    #[serde(rename = "Err")]
    error: String,
}

/// Takes the one Unix listener supplied by systemd socket activation.
///
/// # Errors
///
/// Returns an error unless systemd supplied exactly one valid Unix listener.
pub(super) fn inherited_listener() -> io::Result<StdUnixListener> {
    let mut inherited = listenfd::ListenFd::from_env();
    if inherited.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Volume plugin requires exactly one systemd socket, received {}",
                inherited.len()
            ),
        ));
    }
    inherited.take_unix_listener(0)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "systemd did not pass the Volume plugin socket",
        )
    })
}

/// Serves the Docker Volume plugin on an activated Unix listener.
///
/// # Errors
///
/// Returns an error when the listener cannot become asynchronous or serving fails.
pub(super) async fn run(
    listener: StdUnixListener,
    data_dir: &std::path::Path,
    run_dir: &std::path::Path,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    serve(
        UnixListener::from_std(listener)?,
        VolumeStorage::new(data_dir, run_dir),
    )
    .await
}

async fn serve(listener: UnixListener, storage: VolumeStorage) -> io::Result<()> {
    let router = Router::new()
        .route("/Plugin.Activate", post(activate))
        .route("/Storage.Inspect", post(capacity::inspect))
        .route("/Storage.Prepare", post(capacity::prepare))
        .route("/VolumeDriver.Create", post(create))
        .route("/VolumeDriver.Remove", post(removal::remove))
        .route("/VolumeDriver.Get", post(removal::get))
        .route("/VolumeDriver.List", post(removal::list))
        .route("/VolumeDriver.Mount", post(mount))
        .route("/VolumeDriver.Unmount", post(unmount))
        .route("/VolumeDriver.Path", post(mount))
        .route("/VolumeDriver.Capabilities", post(capabilities))
        .layer(middleware::from_fn(legacy_plugin_json))
        .with_state(storage);
    axum::serve(listener, router).await
}

async fn legacy_plugin_json(mut request: Request, next: Next) -> Response {
    request
        .headers_mut()
        .entry(CONTENT_TYPE)
        .or_insert(HeaderValue::from_static("application/json"));
    next.run(request).await
}

async fn activate() -> Json<serde_json::Value> {
    Json(serde_json::json!({"Implements":["VolumeDriver"]}))
}

async fn create(
    State(storage): State<VolumeStorage>,
    Json(request): Json<CreateRequest>,
) -> Json<ErrorResponse> {
    let result = match request.name.parse::<DockerVolumeName>() {
        Ok(name) => storage.create(&name, &request.options).await,
        Err(error) => Err(error),
    };
    error_response(result)
}

fn error_response(result: Result<()>) -> Json<ErrorResponse> {
    Json(ErrorResponse {
        error: result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default(),
    })
}

async fn mount(
    State(storage): State<VolumeStorage>,
    Json(request): Json<VolumeRequest>,
) -> Json<MountResponse> {
    let result = match request.name.parse::<DockerVolumeName>() {
        Ok(name) => storage.mountpoint(&name).await,
        Err(error) => Err(error),
    };
    mount_response(result)
}

fn mount_response(result: Result<String>) -> Json<MountResponse> {
    match result {
        Ok(mountpoint) => Json(MountResponse {
            mountpoint,
            error: String::new(),
        }),
        Err(error) => Json(MountResponse {
            mountpoint: String::new(),
            error: error.to_string(),
        }),
    }
}

async fn unmount(Json(_request): Json<VolumeRequest>) -> Json<ErrorResponse> {
    Json(ErrorResponse {
        error: String::new(),
    })
}

async fn capabilities() -> Json<serde_json::Value> {
    Json(serde_json::json!({"Capabilities":{"Scope":"local"}}))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };

    use super::*;

    #[test]
    fn dataset_missing_usage_is_not_zero() {
        assert!(Dataset::parse("tank/ployz/data\t1073741824\t-\t/data\tyes\toff", "tank").is_err());
    }

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ployzd-volume-plugin-{}-{}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[path = "remove_tests.rs"]
    mod remove_tests;

    #[path = "first_pool_tests.rs"]
    mod first_pool_tests;

    #[path = "capacity_tests.rs"]
    mod capacity_tests;

    #[path = "fake_zfs.rs"]
    mod fake_zfs;

    #[path = "dataset_safety_tests.rs"]
    mod dataset_safety_tests;

    use fake_zfs::{USABLE_POOL, fake_zfs};

    #[tokio::test]
    async fn docker_can_create_and_mount_a_bounded_volume() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1g"}}),
            )
            .await,
            json!({"Err":""})
        );
        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Mount",
                json!({"Name":"data","ID":"container"}),
            )
            .await,
            json!({"Mountpoint":"/var/lib/ployz-volumes/data","Err":""})
        );

        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(log.contains(
            "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz"
        ));
        assert!(log.contains("zfs create -o refquota=1073741824 tank/ployz/data"));
        assert!(log.contains("zfs mount tank/ployz/data"));
        assert!(!log.contains("recordsize"));
        server.abort();
    }

    #[tokio::test]
    async fn docker_can_get_and_list_provisioned_volume_usage() {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("volume"), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
        let volume = json!({
            "Name":"data",
            "Mountpoint":"/var/lib/ployz-volumes/data",
            "Status":{"bound_bytes":1073741824,"used_bytes":966367642}
        });

        assert_eq!(
            post(&socket, "/VolumeDriver.List", json!({})).await,
            json!({"Volumes":[volume],"Err":""})
        );
        server.abort();
    }

    #[tokio::test]
    async fn docker_driver_accepts_an_exact_internal_byte_bound() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1b"}}),
            )
            .await,
            json!({"Err":""})
        );
        assert!(
            fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create -o refquota=1 tank/ployz/data")
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_rejects_an_existing_root_with_an_incompatible_mountpoint() {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("incompatible-root"), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        let response = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await;

        let message = error(&response);
        assert!(message.contains("tank/ployz"));
        assert!(message.contains("/tank/ployz"));
        assert!(message.contains(MOUNT_ROOT));
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create")
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_mount_and_remove_reject_a_child_outside_the_managed_root() {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("volume"), "").unwrap();
        fs::write(test.0.join("incompatible-volume"), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        for response in [
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1g"}}),
            )
            .await,
            post(
                &socket,
                "/VolumeDriver.Mount",
                json!({"Name":"data","ID":"container"}),
            )
            .await,
            post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await,
        ] {
            let message = error(&response);
            assert!(message.contains("tank/ployz/data"));
            assert!(message.contains("/srv/data"));
            assert!(message.contains("/var/lib/ployz-volumes/data"));
        }
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("zfs create"));
        assert!(!log.contains("zfs mount"));
        assert!(!log.contains("zfs destroy"));
        server.abort();
    }

    #[tokio::test]
    async fn invalid_requests_are_rejected_before_zfs_mutation() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert!(
            !error(&post(&socket, "/VolumeDriver.Create", json!({"Name":"data"}),).await)
                .is_empty()
        );
        for options in [
            json!({}),
            json!({"size":"0g"}),
            json!({"size":"garbage"}),
            json!({"size":"1024"}),
            json!({"size":"18446744073709551615t"}),
        ] {
            let response = post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":options}),
            )
            .await;
            assert!(!error(&response).is_empty(), "accepted options {options}");
        }
        assert!(
            !error(
                &post(
                    &socket,
                    "/VolumeDriver.Create",
                    json!({"Name":"../data","Opts":{"size":"1g"}}),
                )
                .await
            )
            .is_empty()
        );

        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap_or_default()
                .contains("zfs create")
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_is_idempotent_but_does_not_resize() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
        let request = json!({"Name":"data","Opts":{"size":"1g"}});

        assert_eq!(
            post(&socket, "/VolumeDriver.Create", request.clone()).await,
            json!({"Err":""})
        );
        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1024m"}}),
            )
            .await,
            json!({"Err":""})
        );
        let resized = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"2g"}}),
        )
        .await;
        assert!(error(&resized).contains("separate update"));
        assert_eq!(
            fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .matches("zfs create -o refquota=1073741824 tank/ployz/data")
                .count(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_uses_the_same_pool_eligibility_cases_as_observation() {
        for (pools, expected_error) in [
            (USABLE_POOL, None),
            (
                "tank\t4294967296\t0\t4294967296\tONLINE\ton\n",
                Some("no usable existing Machine Pool"),
            ),
            (
                "tank\t4294967296\t0\t4294967296\tFAULTED\toff\n",
                Some("no usable existing Machine Pool"),
            ),
            (
                "alpha\t4294967296\t0\t4294967296\tONLINE\toff\nbeta\t4294967296\t0\t4294967296\tDEGRADED\toff\n",
                Some("multiple usable Machine Pools"),
            ),
        ] {
            let test = TestDir::new();
            let (zpool, zfs) = fake_zfs(&test.0, pools);
            let socket = test.0.join("plugin.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

            let response = post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1g"}}),
            )
            .await;
            match expected_error {
                Some(expected) => assert!(error(&response).contains(expected)),
                None => assert_eq!(error(&response), ""),
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn create_rejects_malformed_pool_inspection_before_zfs_mutation() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(
            &test.0,
            "broken\tONLINE\ntank\t4294967296\t0\t4294967296\tONLINE\toff\n",
        );
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        let response = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await;

        let message = error(&response);
        assert!(message.contains("invalid ZFS Pool output"));
        assert!(message.contains("broken"));
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create")
        );
        server.abort();
    }

    #[tokio::test]
    async fn plugin_serves_activation_path_unmount_and_local_capabilities() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert_eq!(
            post(&socket, "/Plugin.Activate", json!({})).await,
            json!({"Implements":["VolumeDriver"]})
        );
        assert_eq!(
            post(&socket, "/VolumeDriver.Capabilities", json!({})).await,
            json!({"Capabilities":{"Scope":"local"}})
        );
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await;
        assert_eq!(
            post(&socket, "/VolumeDriver.Path", json!({"Name":"data"})).await,
            json!({"Mountpoint":"/var/lib/ployz-volumes/data","Err":""})
        );
        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Unmount",
                json!({"Name":"data","ID":"container"}),
            )
            .await,
            json!({"Err":""})
        );
        server.abort();
    }

    async fn post(socket: &Path, route: &str, body: Value) -> Value {
        let body = serde_json::to_vec(&body).unwrap();
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream
            .write_all(
                format!(
                    "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let body = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .and_then(|index| response.get(index + 4..))
            .unwrap();
        serde_json::from_slice(body).unwrap()
    }

    fn error(response: &Value) -> &str {
        response.get("Err").and_then(Value::as_str).unwrap()
    }
}
