//! Docker v1 Volume plugin backed by bounded ZFS datasets.

use std::{
    collections::BTreeMap, fmt, io, os::unix::net::UnixListener as StdUnixListener, path::PathBuf,
    str::FromStr, sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use ployzd::machine_pool::{self, MachinePool};
use serde::{Deserialize, Serialize};
use tokio::{net::UnixListener, process::Command, sync::Mutex};

const DATASET_ROOT: &str = "ployz";
const MOUNT_ROOT: &str = "/var/lib/ployz-volumes";
type Result<T> = std::result::Result<T, VolumeError>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct VolumeError(String);

impl From<String> for VolumeError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for VolumeError {
    fn from(message: &str) -> Self {
        Self(message.to_owned())
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

#[derive(Clone)]
struct VolumeStorage {
    zpool: PathBuf,
    zfs: PathBuf,
    mutation: Arc<Mutex<()>>,
}

impl VolumeStorage {
    fn new() -> Self {
        Self::with_programs("zpool", "zfs")
    }

    fn with_programs(zpool: impl Into<PathBuf>, zfs: impl Into<PathBuf>) -> Self {
        Self {
            zpool: zpool.into(),
            zfs: zfs.into(),
            mutation: Arc::new(Mutex::new(())),
        }
    }

    async fn create(
        &self,
        name: &DockerVolumeName,
        options: &BTreeMap<String, String>,
    ) -> Result<()> {
        let requested = parse_size(options)?;
        let _guard = self.mutation.lock().await;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        let volume = format!("{root}/{name}");

        if let Some(dataset) = datasets.iter().find(|dataset| dataset.name == root) {
            dataset.require_mountpoint(MOUNT_ROOT)?;
        }

        if let Some(existing) = datasets.iter().find(|dataset| dataset.name == volume) {
            existing.require_mountpoint(&name.mountpoint())?;
            return if existing.refquota == requested {
                Ok(())
            } else {
                Err(format!(
                    "Volume {name} already has a {}-byte bound; changing it to {requested} bytes is a separate update operation",
                    existing.refquota
                )
                .into())
            };
        }

        let available = datasets
            .iter()
            .find(|dataset| dataset.name == pool.name())
            .ok_or_else(|| {
                format!(
                    "ZFS did not report the root dataset for Pool {}",
                    pool.name()
                )
            })?
            .available;
        let outstanding = datasets
            .iter()
            .filter(|dataset| dataset.name.starts_with(&format!("{root}/")))
            .try_fold(0_u64, |total, dataset| {
                total
                    .checked_add(dataset.refquota.saturating_sub(dataset.referenced))
                    .ok_or_else(|| "Provisioned Volume commitments overflowed u64".to_owned())
            })?;
        let uncommitted = available.saturating_sub(outstanding);
        if requested > uncommitted {
            return Err(format!(
                "Pool {} has {uncommitted} uncommitted bytes but Volume {name} requires {requested}; grow the Machine Pool before retrying (automatic growth is tracked by #548)",
                pool.name()
            )
            .into());
        }

        if !datasets.iter().any(|dataset| dataset.name == root) {
            self.zfs(&[
                "create",
                "-o",
                "canmount=off",
                "-o",
                &format!("mountpoint={MOUNT_ROOT}"),
                &root,
            ])
            .await?;
        }
        self.zfs(&["create", "-o", &format!("refquota={requested}"), &volume])
            .await?;
        Ok(())
    }

    async fn mountpoint(&self, name: &DockerVolumeName) -> Result<String> {
        let _guard = self.mutation.lock().await;
        let pool = self.one_pool().await?;
        let volume = format!("{}/{DATASET_ROOT}/{name}", pool.name());
        let expected_mountpoint = name.mountpoint();
        let mut dataset = self
            .datasets(&pool)
            .await?
            .into_iter()
            .find(|dataset| dataset.name == volume)
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_mountpoint(&expected_mountpoint)?;
        if !dataset.mounted {
            self.zfs(&["mount", &volume]).await?;
            dataset = self
                .datasets(&pool)
                .await?
                .into_iter()
                .find(|dataset| dataset.name == volume)
                .ok_or_else(|| format!("Provisioned Volume {name} disappeared while mounting"))?;
            dataset.require_mountpoint(&expected_mountpoint)?;
        }
        if !dataset.mounted {
            return Err(format!("Provisioned Volume {name} did not mount").into());
        }
        Ok(dataset.mountpoint)
    }

    async fn one_pool(&self) -> Result<MachinePool> {
        let output =
            checked_command(&self.zpool, &["list", "-Hp", "-o", "name,health,readonly"]).await?;
        machine_pool::one_usable(&output)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "no usable existing Machine Pool; automatic Pool creation is tracked by #541".into()
            })
    }

    async fn datasets(&self, pool: &MachinePool) -> Result<Vec<Dataset>> {
        let output = self
            .zfs(&[
                "list",
                "-Hp",
                "-o",
                "name,refquota,referenced,available,mountpoint,mounted",
                "-r",
                pool.name(),
            ])
            .await?;
        output
            .lines()
            .map(|line| Dataset::parse(line, pool.name()))
            .collect()
    }

    async fn zfs(&self, args: &[&str]) -> Result<String> {
        checked_command(&self.zfs, args).await
    }
}

struct Dataset {
    name: String,
    refquota: u64,
    referenced: u64,
    available: u64,
    mountpoint: String,
    mounted: bool,
}

impl Dataset {
    fn parse(line: &str, pool: &str) -> Result<Self> {
        let mut fields = line.split('\t');
        let invalid = || format!("invalid ZFS dataset output for Pool {pool}: {line}");
        let (
            Some(name),
            Some(refquota),
            Some(referenced),
            Some(available),
            Some(mountpoint),
            Some(mounted),
            None,
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            return Err(invalid().into());
        };
        Ok(Self {
            name: name.to_owned(),
            refquota: parse_zfs_bytes(refquota)?,
            referenced: parse_zfs_bytes(referenced)?,
            available: parse_zfs_bytes(available)?,
            mountpoint: mountpoint.to_owned(),
            mounted: mounted == "yes",
        })
    }

    fn require_mountpoint(&self, expected: &str) -> Result<()> {
        if self.mountpoint == expected {
            return Ok(());
        }
        Err(format!(
            "ZFS dataset {} has incompatible mountpoint {}; set it to {expected} before retrying",
            self.name, self.mountpoint
        )
        .into())
    }
}

fn parse_zfs_bytes(value: &str) -> Result<u64> {
    match value {
        "none" | "-" => Ok(0),
        _ => value
            .parse()
            .map_err(|_| VolumeError::from(format!("invalid byte count from ZFS: {value}"))),
    }
}

fn parse_size(options: &BTreeMap<String, String>) -> Result<u64> {
    if options.len() != 1 || !options.contains_key("size") {
        return Err("Volume option size is required and is the only supported option".into());
    }
    let value = options
        .get("size")
        .expect("the only accepted option is size");
    let (amount, suffix) = value.split_at(value.len().saturating_sub(1));
    let multiplier = match suffix {
        "k" => 1024_u64,
        "m" => 1024_u64.pow(2),
        "g" => 1024_u64.pow(3),
        "t" => 1024_u64.pow(4),
        _ => {
            return Err(format!(
                "invalid Volume size {value:?}; use a positive integer followed by k, m, g, or t"
            )
            .into());
        }
    };
    let amount = amount.parse::<u64>().map_err(|_| {
        format!("invalid Volume size {value:?}; use a positive integer followed by k, m, g, or t")
    })?;
    if amount == 0 {
        return Err("Volume size must be greater than zero".into());
    }
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("Volume size {value:?} overflows bytes").into())
}

async fn checked_command(program: &PathBuf, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not run {}: {error}", program.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    String::from_utf8(output.stdout)
        .map_err(|_| format!("{} returned non-UTF-8 output", program.display()).into())
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
pub(super) async fn run(listener: StdUnixListener) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    serve(UnixListener::from_std(listener)?, VolumeStorage::new()).await
}

async fn serve(listener: UnixListener, storage: VolumeStorage) -> io::Result<()> {
    let router = Router::new()
        .route("/Plugin.Activate", post(activate))
        .route("/VolumeDriver.Create", post(create))
        .route("/VolumeDriver.Mount", post(mount))
        .route("/VolumeDriver.Unmount", post(unmount))
        .route("/VolumeDriver.Path", post(mount))
        .route("/VolumeDriver.Capabilities", post(capabilities))
        .with_state(storage);
    axum::serve(listener, router).await
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
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };

    use super::*;

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

    #[tokio::test]
    async fn docker_can_create_and_mount_a_bounded_volume() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
    async fn create_rejects_an_existing_root_with_an_incompatible_mountpoint() {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("incompatible-root"), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
    async fn create_and_mount_reject_an_existing_child_outside_the_managed_root() {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("volume"), "").unwrap();
        fs::write(test.0.join("incompatible-volume"), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
        ] {
            let message = error(&response);
            assert!(message.contains("tank/ployz/data"));
            assert!(message.contains("/srv/data"));
            assert!(message.contains("/var/lib/ployz-volumes/data"));
        }
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("zfs create"));
        assert!(!log.contains("zfs mount"));
        server.abort();
    }

    #[tokio::test]
    async fn invalid_requests_are_rejected_before_zfs_mutation() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
    async fn existing_unused_bounds_reduce_uncommitted_capacity() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
        let response = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"2g"}}),
        )
        .await;
        assert!(error(&response).contains("uncommitted bytes"));
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("tank/ployz/other")
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_refuses_uncommitted_capacity_without_overcommitting() {
        let test = TestDir::new();
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        let response = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"too-big","Opts":{"size":"3g"}}),
        )
        .await;
        let message = error(&response);
        assert!(message.contains("tank") && message.contains("grow the Machine Pool"));
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create")
        );
        server.abort();
    }

    #[tokio::test]
    async fn create_uses_the_same_pool_eligibility_cases_as_observation() {
        for (pools, expected_error) in [
            ("", Some("no usable existing Machine Pool")),
            ("tank\tONLINE\toff\n", None),
            (
                "tank\tONLINE\ton\n",
                Some("no usable existing Machine Pool"),
            ),
            (
                "tank\tFAULTED\toff\n",
                Some("no usable existing Machine Pool"),
            ),
            (
                "alpha\tONLINE\toff\nbeta\tDEGRADED\toff\n",
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
        let (zpool, zfs) = fake_zfs(&test.0, "broken\tONLINE\ntank\tONLINE\toff\n");
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
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
                    "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

    fn fake_zfs(directory: &Path, pools: &str) -> (PathBuf, PathBuf) {
        let script = directory.join("fake-zfs");
        let commands = directory.join("commands");
        let root = directory.join("root");
        let incompatible_root = directory.join("incompatible-root");
        let volume = directory.join("volume");
        let incompatible_volume = directory.join("incompatible-volume");
        let mounted = directory.join("mounted");
        let script_body = format!(
            r#"#!/bin/sh
set -eu
name=${{0##*/}}
printf '%s %s\n' "$name" "$*" >> '{commands}'
if [ "$name" = zpool ]; then
  printf '{pools}'
  exit 0
fi
case "$*" in
  'list -Hp -o name,refquota,referenced,available,mountpoint,mounted -r tank')
    printf 'tank\t0\t24576\t2147459072\t/tank\tyes\n'
    if [ -e '{root}' ]; then
      if [ -e '{incompatible_root}' ]; then root_mountpoint=/tank/ployz; else root_mountpoint=/var/lib/ployz-volumes; fi
      printf 'tank/ployz\t0\t24576\t2147459072\t%s\tno\n' "$root_mountpoint"
    fi
    if [ -e '{volume}' ]; then
      if [ -e '{mounted}' ]; then state=yes; else state=no; fi
      if [ -e '{incompatible_volume}' ]; then volume_mountpoint=/srv/data; else volume_mountpoint=/var/lib/ployz-volumes/data; fi
      printf 'tank/ployz/data\t1073741824\t24576\t1073717248\t%s\t%s\n' "$volume_mountpoint" "$state"
    fi
    ;;
  'create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz') touch '{root}' ;;
  'create -o refquota=1073741824 tank/ployz/data') touch '{volume}' ;;
  'mount tank/ployz/data') touch '{mounted}' ;;
  *) echo "unexpected fake zfs command: $*" >&2; exit 2 ;;
esac
"#,
            commands = commands.display(),
            pools = pools.escape_default(),
            root = root.display(),
            incompatible_root = incompatible_root.display(),
            volume = volume.display(),
            incompatible_volume = incompatible_volume.display(),
            mounted = mounted.display(),
        );
        fs::write(&script, script_body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let zpool = directory.join("zpool");
        let zfs = directory.join("zfs");
        std::os::unix::fs::symlink(&script, &zpool).unwrap();
        std::os::unix::fs::symlink(&script, &zfs).unwrap();
        (zpool, zfs)
    }
}
