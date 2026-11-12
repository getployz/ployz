//! Serves a Volume's send streams on the management address, for the Machine mirroring it.
//!
//! A request names one stream with [`SendStream`]: a full send of a snapshot, an
//! incremental one between two, or the rest of an interrupted one by its ZFS resume
//! token. The body is `zfs send`'s stdout, so the mirroring Machine pipes it straight
//! into `zfs receive`. The firewall admits the port from the mesh only, and WireGuard
//! binds each mesh source address to its peer's key. Before any `zfs` call the server asks
//! [`Recipients`] whether the connection's source may read that stream.

use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ployz_core::{
    DATASET_ROOT, DockerVolumeName, MIRROR_ROOT, MachineId, MachineName, SendSource, SendStream,
    SnapshotName,
};
use tokio::{net::TcpListener, process::Command};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

pub mod protocol;

use crate::machine_pool;

/// Whether this Machine sends a Volume to a mesh peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Sends {
    /// The Machine doesn't enforce, so it sends to any mesh peer.
    Open,
    /// A live grant to this accepted Machine covers the stream.
    Granted(MachineId),
    /// No accepted Machine has the address, or, when `recipient` names one, it holds no
    /// live grant that covers the stream.
    Refused { recipient: Option<MachineName> },
}

/// Who this Machine sends a Volume's streams to.
pub trait Recipients: Clone + Send + Sync + 'static {
    /// Whether the mesh address `peer` may read `snapshot` of `volume`, or with none the
    /// bound of the copy. An error is a record this Machine could not read.
    fn sends(
        &self,
        peer: IpAddr,
        volume: &DockerVolumeName,
        snapshot: Option<&SnapshotName>,
    ) -> impl Future<Output = Result<Sends, String>> + Send;
}

impl Recipients for crate::machine::LocalMachine {
    async fn sends(
        &self,
        peer: IpAddr,
        volume: &DockerVolumeName,
        snapshot: Option<&SnapshotName>,
    ) -> Result<Sends, String> {
        self.sends_volume(peer, volume, snapshot)
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct Programs {
    zfs: PathBuf,
    zpool: PathBuf,
}

#[derive(Clone)]
struct Sender<R> {
    programs: Programs,
    recipients: R,
}

/// Serves send streams to the peers `recipients` admits until `shutdown` fires.
///
/// # Errors
///
/// Returns the error that stopped the server.
pub async fn serve<R: Recipients>(
    listener: TcpListener,
    (zfs, zpool): (PathBuf, PathBuf),
    recipients: R,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let router = Router::new().fallback(send::<R>).with_state(Sender {
        programs: Programs { zfs, zpool },
        recipients,
    });
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await
}

async fn send<R: Recipients>(
    State(sender): State<Sender<R>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    if request.method() != axum::http::Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let programs = &sender.programs;
    if let Some(name) = protocol::bound_volume(request.uri().path()) {
        let Ok(name) = name.parse::<DockerVolumeName>() else {
            return (StatusCode::NOT_FOUND, "no such Volume").into_response();
        };
        if let Err(refusal) = admit(&sender.recipients, peer.ip(), &name, None).await {
            return refusal;
        }
        return bound(programs, &name).await.into_response();
    }
    let Some(stream) = SendStream::parse(request.uri().path(), request.uri().query()) else {
        return (StatusCode::NOT_FOUND, "no such send stream").into_response();
    };
    let target = match &stream.source {
        SendSource::Full { target }
        | SendSource::Incremental { target, .. }
        | SendSource::Resume { target, .. } => target,
    };
    if let Err(refusal) = admit(&sender.recipients, peer.ip(), &stream.name, Some(target)).await {
        return refusal;
    }
    match stream_args(programs, &stream).await {
        Ok(args) => {
            let mut child = match Command::new(&programs.zfs)
                .arg("send")
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("could not run zfs send: {error}"),
                    )
                        .into_response();
                }
            };
            let stdout = child.stdout.take().expect("zfs send stdout is piped");
            // The child outlives this handler: ZFS stops on its own when the body's
            // reader hangs up, and the runtime reaps it.
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Body::from_stream(ReaderStream::new(stdout)).into_response()
        }
        Err(refusal) => refusal.into_response(),
    }
}

/// Refuse with no bytes of the Volume unless `recipients` sends `snapshot` of `volume` to
/// `peer`. A refusal names the Server the address belongs to in this Machine's journal.
async fn admit<R: Recipients>(
    recipients: &R,
    peer: IpAddr,
    volume: &DockerVolumeName,
    snapshot: Option<&SnapshotName>,
) -> Result<(), Response> {
    match recipients.sends(peer, volume, snapshot).await {
        Ok(Sends::Open | Sends::Granted(_)) => Ok(()),
        Ok(Sends::Refused { recipient }) => {
            let server = recipient.as_ref().map_or("no Server", |name| name.as_str());
            tracing::warn!(
                %peer,
                %server,
                %volume,
                snapshot = snapshot.map(SnapshotName::as_str),
                "send stream refused: no live grant to this address"
            );
            Err((
                StatusCode::FORBIDDEN,
                format!("this Server sends {volume} only to a Server approved to receive it"),
            )
                .into_response())
        }
        Err(error) => {
            tracing::warn!(%peer, %volume, %error, "send stream refused: records unread");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("this Server could not read who may receive {volume}"),
            )
                .into_response())
        }
    }
}

enum Refusal {
    Unknown(String),
    Zfs(String),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Unknown(what) => (StatusCode::NOT_FOUND, what).into_response(),
            Self::Zfs(error) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
        }
    }
}

/// The bound in bytes of the copy this Machine holds for `name`, so a Machine about to
/// pull it can size its slot before any stream flows.
async fn bound(programs: &Programs, name: &DockerVolumeName) -> Result<String, Refusal> {
    let root = resolve_root(programs, name).await?;
    let mut output = lines(
        &programs.zfs,
        &["get", "-Hp", "-o", "value", "refquota", &root],
    )
    .await?;
    match output.pop() {
        Some(value) if value.parse::<u64>().is_ok() => Ok(value),
        other => Err(Refusal::Zfs(format!(
            "{root} reports refquota {other:?}, not a byte count"
        ))),
    }
}

/// The dataset holding this Machine's copy of `name`. A stopped writer parks its root in
/// the slot until the target promotes, and the target still pulls the final increment
/// from there.
async fn resolve_root(programs: &Programs, name: &DockerVolumeName) -> Result<String, Refusal> {
    let pools = lines(&programs.zpool, &machine_pool::ZPOOL_LIST).await?;
    let pool = machine_pool::one_usable(&pools.join("\n"))
        .map_err(|error| Refusal::Zfs(error.to_string()))?
        .ok_or_else(|| {
            Refusal::Unknown("no Machine Pool is imported on this Machine".to_owned())
        })?;
    let filesystems = lines(
        &programs.zfs,
        &["list", "-H", "-o", "name", "-t", "filesystem"],
    )
    .await?;
    [
        format!("{}/{DATASET_ROOT}/{name}", pool.name()),
        format!("{}/{MIRROR_ROOT}/{name}/fs", pool.name()),
    ]
    .into_iter()
    .find(|dataset| filesystems.contains(dataset))
    .ok_or_else(|| Refusal::Unknown(format!("Volume {name} has no writer on this Machine")))
}

/// The `zfs send` arguments for `stream`, once its names resolve on this Machine.
async fn stream_args(programs: &Programs, stream: &SendStream) -> Result<Vec<String>, Refusal> {
    let root = resolve_root(programs, &stream.name).await?;
    let snapshots = complete_lines(
        &programs.zfs,
        &[
            "list",
            "-H",
            "-o",
            "name,guid",
            "-t",
            "snapshot",
            "-d",
            "1",
            &root,
        ],
    )
    .await?;
    let snapshots: Vec<(&str, &str)> = snapshots
        .iter()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let (target, base) = match &stream.source {
        SendSource::Full { target } => (target, None),
        SendSource::Incremental { base, target } => (target, Some(base)),
        SendSource::Resume { target, .. } => (target, None),
    };
    let target = format!("{root}@{target}");
    if !snapshots.iter().any(|(name, _)| *name == target) {
        return Err(Refusal::Unknown(format!(
            "snapshot {target} does not exist"
        )));
    }
    if let SendSource::Resume { token, .. } = &stream.source {
        let output = Command::new(&programs.zfs)
            .args(["send", "-nvP", "-t", token])
            .env("LC_ALL", "C")
            .output()
            .await
            .map_err(|error| Refusal::Zfs(format!("could not inspect resume token: {error}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            // A token naming a snapshot this writer no longer holds names a stream this
            // Machine cannot send, so the puller drops its partial receive and asks for a
            // full send. That is judged against the listing above, which succeeded, not by
            // ZFS's message: a lookup that failed for another reason reads the same. Any
            // other failure is this Machine's, and the puller keeps its copy.
            let message = format!("resume token cannot be sent from {root}: {}", stderr.trim());
            let contents = stdout.lines().chain(stderr.lines());
            return Err(if names_a_missing_snapshot(contents, &snapshots) {
                Refusal::Unknown(message)
            } else {
                Refusal::Zfs(message)
            });
        }
        let mut names = stdout
            .lines()
            .chain(stderr.lines())
            .filter_map(|line| line.trim().strip_prefix("toname = "));
        if names.next() != Some(target.as_str()) || names.next().is_some() {
            return Err(Refusal::Unknown(format!(
                "resume token does not match requested snapshot {target}"
            )));
        }
        return Ok(vec!["-t".to_owned(), token.clone()]);
    }
    let mut args = Vec::new();
    if let Some(base) = base {
        let base_guid = base.to_string();
        let Some((base_name, _)) = snapshots.iter().find(|(_, guid)| *guid == base_guid) else {
            return Err(Refusal::Unknown(format!(
                "snapshot {base} is not on {root}; the mirror's base must come from this writer"
            )));
        };
        args.push("-i".to_owned());
        args.push((*base_name).to_owned());
    }
    args.push(target);
    Ok(args)
}

/// Whether the `toguid` or `fromguid` that `zfs send -v -t` prints for a token is a
/// snapshot missing from `snapshots`, this Machine's `(name, guid)` listing.
fn names_a_missing_snapshot<'a>(
    contents: impl Iterator<Item = &'a str>,
    snapshots: &[(&str, &str)],
) -> bool {
    contents
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("toguid = 0x")
                .or_else(|| line.strip_prefix("fromguid = 0x"))
        })
        .filter_map(|hex| u64::from_str_radix(hex, 16).ok())
        .filter(|guid| *guid != 0)
        .any(|guid| {
            !snapshots
                .iter()
                .any(|(_, held)| held.parse::<u64>() == Ok(guid))
        })
}

async fn lines(program: &Path, args: &[&str]) -> Result<Vec<String>, Refusal> {
    lines_with(program, args, false).await
}

/// Like [`lines`], for a listing a 404 is judged from. ZFS exits 0 when it stopped
/// iterating on an error and says so only on stderr, so any stderr fails the listing.
async fn complete_lines(program: &Path, args: &[&str]) -> Result<Vec<String>, Refusal> {
    lines_with(program, args, true).await
}

async fn lines_with(program: &Path, args: &[&str], quiet: bool) -> Result<Vec<String>, Refusal> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| Refusal::Zfs(format!("could not run {}: {error}", program.display())))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() || (quiet && !stderr.trim().is_empty()) {
        return Err(Refusal::Zfs(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;
    use crate::test_dir::TestDir;

    const POOLS: &str = "tank\t4294967296\t0\t4294967296\tONLINE\toff\nold\t4294967296\t0\t4294967296\tONLINE\ton\n";

    /// Serves every peer, as a Machine that doesn't enforce.
    #[derive(Clone)]
    struct ToAll;

    impl Recipients for ToAll {
        async fn sends(
            &self,
            _: IpAddr,
            _: &DockerVolumeName,
            _: Option<&SnapshotName>,
        ) -> Result<Sends, String> {
            Ok(Sends::Open)
        }
    }

    type Asks = std::sync::Arc<std::sync::Mutex<Vec<(IpAddr, Option<String>)>>>;

    /// Answers each ask from a fixed table, and counts the asks.
    #[derive(Clone)]
    struct Asked {
        answer: fn(IpAddr, Option<&SnapshotName>) -> Result<Sends, String>,
        asks: Asks,
    }

    impl Recipients for Asked {
        async fn sends(
            &self,
            peer: IpAddr,
            _: &DockerVolumeName,
            snapshot: Option<&SnapshotName>,
        ) -> Result<Sends, String> {
            self.asks
                .lock()
                .unwrap()
                .push((peer, snapshot.map(|name| name.as_str().to_owned())));
            (self.answer)(peer, snapshot)
        }
    }

    async fn start(dir: &TestDir, pools: &str) -> (String, CancellationToken) {
        start_for(dir, pools, ToAll).await
    }

    async fn start_for(
        dir: &TestDir,
        pools: &str,
        recipients: impl Recipients,
    ) -> (String, CancellationToken) {
        fs::create_dir_all(&dir.0).unwrap();
        let zpool = dir.0.join("zpool");
        fs::write(
            &zpool,
            format!("#!/bin/sh\nprintf '{}'\n", pools.escape_default()),
        )
        .unwrap();
        fs::set_permissions(&zpool, fs::Permissions::from_mode(0o755)).unwrap();
        let zfs = dir.0.join("zfs");
        fs::write(
            &zfs,
            r#"#!/bin/sh
case "$*" in
  'list -H -o name -t filesystem') printf 'old\nold/ployz\nold/ployz/data\ntank\ntank/ployz\ntank/ployz/data\ntank/ployz-mirror/other\ntank/ployz-mirror/slotted\ntank/ployz-mirror/slotted/fs\ntank/ployz/flaky\n' ;;
  'list -H -o name,guid -t snapshot -d 1 tank/ployz/data') printf 'tank/ployz/data@w-1-1\t11\ntank/ployz/data@w-1-2\t12\n' ;;
  'list -H -o name,guid -t snapshot -d 1 tank/ployz-mirror/slotted/fs') printf 'tank/ployz-mirror/slotted/fs@w-1-1\t21\ntank/ployz-mirror/slotted/fs@w-1-2\t22\n' ;;
  'list -H -o name,guid -t snapshot -d 1 tank/ployz/flaky') printf 'tank/ployz/flaky@w-1-2\t12\n'; echo 'cannot iterate snapshots: I/O error' >&2 ;;
  'send tank/ployz/data@w-1-2') printf 'full stream' ;;
  'send tank/ployz-mirror/slotted/fs@w-1-2') printf 'full slot stream' ;;
  'send -i tank/ployz-mirror/slotted/fs@w-1-1 tank/ployz-mirror/slotted/fs@w-1-2') printf 'incremental slot stream' ;;
  'send -nvP -t slot-token') printf 'resume token contents:\n\ttoname = tank/ployz-mirror/slotted/fs@w-1-2\n' ;;
  'send -t slot-token') printf 'resumed slot stream' ;;
  'send -i tank/ployz/data@w-1-1 tank/ployz/data@w-1-2') printf 'incremental stream' ;;
  'send -nvP -t token-1') printf 'resume token contents:\n\ttoname = tank/ployz/data@w-1-2\n' >&2 ;;
  'send -nvP -t token-other') printf 'resume token contents:\n\ttoname = tank/ployz/sibling@w-1-2\n' ;;
  'send -nvP -t token-old') printf 'resume token contents:\n\ttoname = old/ployz/data@w-1-2\n' ;;
  'send -nvP -t token-wrong-target') printf 'resume token contents:\n\ttoname = tank/ployz/data@w-1-1\n' ;;
  'send -nvP -t token-missing') printf 'resume token contents:\n\ttoname = tank/ployz/data@w-1-3\n' ;;
  'send -nvP -t token-child') printf 'resume token contents:\n\ttoname = tank/ployz/data/child@w-1-2\n' ;;
  'send -nvP -t token-pruned') printf 'resume token contents:\nnvlist version: 0\n\tfromguid = 0x1f\n\ttoguid = 0xc\n\ttoname = tank/ployz/data@w-1-2\ncannot resume send: incremental source 0x1f no longer exists\n' >&2; exit 1 ;;
  'send -nvP -t token-io') printf 'resume token contents:\nnvlist version: 0\n\tfromguid = 0xb\n\ttoguid = 0xc\n\ttoname = tank/ployz/data@w-1-2\ncannot resume send: incremental source 0xb no longer exists\n' >&2; exit 1 ;;
  'send -t token-other') printf 'unrelated stream' ;;
  'send -t token-1') printf 'resumed stream' ;;
  'get -Hp -o value refquota tank/ployz/data') printf '1073741824\n' ;;
  'get -Hp -o value refquota tank/ployz-mirror/slotted/fs') printf 'none\n' ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&zfs, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve(listener, (zfs, zpool), recipients, shutdown.clone()));
        (format!("http://{address}"), shutdown)
    }

    async fn get(base: &str, path: &str) -> (StatusCode, String) {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    /// `get` from the loopback source address `from`.
    async fn get_from(from: IpAddr, base: &str, path: &str) -> (StatusCode, String) {
        let client = reqwest::Client::builder()
            .local_address(from)
            .build()
            .unwrap();
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    const GRANTED: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
    const STRANGER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 2));

    /// A grant to one peer that covers `w-1-2` only, and a Server with no grant.
    fn granted_w_1_2(peer: IpAddr, snapshot: Option<&SnapshotName>) -> Result<Sends, String> {
        let covered = snapshot.is_none_or(|name| name.as_str() == "w-1-2");
        Ok(if peer == GRANTED && covered {
            Sends::Granted(MachineId::random())
        } else {
            Sends::Refused {
                recipient: Some(MachineName::parse("web-3").unwrap()),
            }
        })
    }

    #[tokio::test]
    async fn serves_a_granted_peer_and_refuses_any_other_before_zfs_runs() {
        let dir = TestDir::new("ployzd-volume-send");
        let asked = Asked {
            answer: granted_w_1_2,
            asks: Default::default(),
        };
        let (base, shutdown) = start_for(&dir, POOLS, asked.clone()).await;
        assert_eq!(
            get_from(GRANTED, &base, "/volume-send/data?target=w-1-2&base=11").await,
            (StatusCode::OK, "incremental stream".to_owned())
        );
        assert_eq!(
            get_from(GRANTED, &base, "/volume-bound/data").await,
            (StatusCode::OK, "1073741824".to_owned())
        );
        for (from, path) in [
            (STRANGER, "/volume-send/data?target=w-1-2"),
            (STRANGER, "/volume-send/data?target=w-1-2&token=token-1"),
            (STRANGER, "/volume-bound/data"),
            (GRANTED, "/volume-send/data?target=w-1-1"),
            (GRANTED, "/volume-send/other?target=w-1-1"),
        ] {
            let (status, body) = get_from(from, &base, path).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
            assert!(body.contains("only to a Server approved"), "{path}: {body}");
            assert!(!body.contains("stream"), "no bytes of the Volume: {body}");
        }
        assert_eq!(
            asked.asks.lock().unwrap().get(..3),
            Some(
                &[
                    (GRANTED, Some("w-1-2".to_owned())),
                    (GRANTED, None),
                    (STRANGER, Some("w-1-2".to_owned())),
                ][..]
            ),
            "the stream asks for the snapshot it would send, and the bound for none"
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn refuses_every_stream_while_its_records_are_unread() {
        let dir = TestDir::new("ployzd-volume-send");
        let unread = Asked {
            answer: |_, _| Err("the approvals log is unreadable".to_owned()),
            asks: Default::default(),
        };
        let (base, shutdown) = start_for(&dir, POOLS, unread).await;
        for path in ["/volume-send/data?target=w-1-2", "/volume-bound/data"] {
            let (status, body) = get(&base, path).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
            assert!(!body.contains("unreadable"), "{path}: {body}");
        }
        shutdown.cancel();
    }

    #[tokio::test]
    async fn streams_full_incremental_and_resumed_sends() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        assert_eq!(
            get(&base, "/volume-send/data?target=w-1-2").await,
            (StatusCode::OK, "full stream".to_owned())
        );
        assert_eq!(
            get(&base, "/volume-send/data?target=w-1-2&base=11").await,
            (StatusCode::OK, "incremental stream".to_owned())
        );
        assert_eq!(
            get(&base, "/volume-send/data?target=w-1-2&token=token-1").await,
            (StatusCode::OK, "resumed stream".to_owned())
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn answers_the_bound_of_the_copy_it_holds() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        assert_eq!(
            get(&base, "/volume-bound/data").await,
            (StatusCode::OK, "1073741824".to_owned())
        );
        let (status, body) = get(&base, "/volume-bound/slotted").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(body.contains("not a byte count"), "{body}");
        let (status, body) = get(&base, "/volume-bound/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("has no writer on this Machine"), "{body}");
        shutdown.cancel();
    }

    #[tokio::test]
    async fn streams_from_the_slot_of_a_stopped_writer() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        assert_eq!(
            get(&base, "/volume-send/slotted?target=w-1-2").await,
            (StatusCode::OK, "full slot stream".to_owned())
        );
        assert_eq!(
            get(&base, "/volume-send/slotted?target=w-1-2&base=21").await,
            (StatusCode::OK, "incremental slot stream".to_owned())
        );
        assert_eq!(
            get(&base, "/volume-send/slotted?target=w-1-2&token=slot-token").await,
            (StatusCode::OK, "resumed slot stream".to_owned())
        );
        for (path, reason) in [
            (
                "/volume-send/slotted?target=w-1-2&base=11",
                "not on tank/ployz-mirror/slotted/fs",
            ),
            (
                "/volume-send/slotted?target=w-1-2&token=token-1",
                "does not match",
            ),
            (
                "/volume-send/data?target=w-1-2&token=slot-token",
                "does not match",
            ),
        ] {
            let (status, body) = get(&base, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(body.contains(reason), "{path}: {body}");
        }
        shutdown.cancel();
    }

    #[tokio::test]
    async fn refuses_streams_this_machine_cannot_send() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        for (path, reason) in [
            ("/volume-send/other?target=w-1-2", "no writer"),
            ("/volume-send/data?target=w-1-3", "does not exist"),
            (
                "/volume-send/data?target=w-1-2&base=99",
                "not on tank/ployz/data",
            ),
            ("/volume-send/data", "no such send stream"),
            (
                "/volume-send/data?target=w-1-2&base=11&token=t",
                "no such send stream",
            ),
            ("/other", "no such send stream"),
        ] {
            let (status, body) = get(&base, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(body.contains(reason), "{path}: {body}");
        }
        shutdown.cancel();
    }

    #[tokio::test]
    async fn refuses_streams_without_one_usable_machine_pool() {
        let both_writable = POOLS.replace("ONLINE\ton", "ONLINE\toff");
        for (pools, status, reason) in [
            ("", StatusCode::NOT_FOUND, "no Machine Pool"),
            (
                both_writable.as_str(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "ambiguous",
            ),
        ] {
            let dir = TestDir::new("ployzd-volume-send");
            let (base, shutdown) = start(&dir, pools).await;
            let (actual, body) = get(&base, "/volume-send/data?target=w-1-2").await;
            assert_eq!(actual, status, "{body}");
            assert!(body.contains(reason), "{body}");
            shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn resumed_sends_require_the_requested_root_and_target() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        let (status, body) = get(&base, "/volume-send/data?token=token-other").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        for token in [
            "token-other",
            "token-old",
            "token-wrong-target",
            "token-missing",
            "token-child",
        ] {
            let path = format!("/volume-send/data?target=w-1-2&token={token}");
            let (status, body) = get(&base, &path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(body.contains("does not match"), "{path}: {body}");
        }
        let (status, body) = get(&base, "/volume-send/other?target=w-1-2&token=token-other").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = get(&base, "/volume-send/data?target=w-1-2&token=token-1").await;
        assert_eq!(
            (status, body),
            (StatusCode::OK, "resumed stream".to_owned())
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn a_token_whose_base_the_writer_pruned_is_no_stream_but_a_zfs_failure_is() {
        // The puller answers 404 by dropping its partial receive and asking for a full
        // send, so a stale token must not read as this Machine failing.
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, POOLS).await;
        let (status, body) = get(&base, "/volume-send/data?target=w-1-2&token=token-pruned").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("no longer exists"), "{body}");
        assert!(!body.contains("token-pruned"), "{body}");
        // A lookup that failed while this writer still holds both snapshots is this
        // Machine's failure, whatever ZFS says, and the puller must keep its copy.
        let (status, body) = get(&base, "/volume-send/data?target=w-1-2&token=token-io").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        // A listing ZFS cut short on an error exits 0, and is no evidence of a pruned base.
        let (status, body) = get(&base, "/volume-send/flaky?target=w-1-2&token=token-pruned").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        shutdown.cancel();
    }

    #[tokio::test]
    async fn resumed_sends_require_a_usable_pool() {
        let dir = TestDir::new("ployzd-volume-send");
        let (base, shutdown) = start(&dir, "").await;
        let (status, body) = get(&base, "/volume-send/data?target=w-1-2&token=token-1").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("no Machine Pool"), "{body}");
        shutdown.cancel();
    }
}
