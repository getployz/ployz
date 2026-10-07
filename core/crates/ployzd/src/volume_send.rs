//! Serves a Volume's send streams on the management address, for the Machine mirroring it.
//!
//! A request names one stream with [`SendStream`]: a full send of a snapshot, an
//! incremental one between two, or the rest of an interrupted one by its ZFS resume
//! token. The body is `zfs send`'s stdout, so the mirroring Machine pipes it straight
//! into `zfs receive`. The firewall admits the port from the mesh only.

use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ployz_core::{DATASET_ROOT, SendSource, SendStream};
use tokio::{net::TcpListener, process::Command};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::machine_pool;

#[derive(Clone)]
struct Programs {
    zfs: PathBuf,
    zpool: PathBuf,
}

/// Serves send streams until `shutdown` fires.
///
/// # Errors
///
/// Returns the error that stopped the server.
pub async fn serve(
    listener: TcpListener,
    zfs: PathBuf,
    zpool: PathBuf,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let router = Router::new()
        .fallback(send)
        .with_state(Programs { zfs, zpool });
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
}

async fn send(State(programs): State<Programs>, request: Request) -> Response {
    if request.method() != axum::http::Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(stream) = SendStream::parse(request.uri().path(), request.uri().query()) else {
        return (StatusCode::NOT_FOUND, "no such send stream").into_response();
    };
    match stream_args(&programs, &stream).await {
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
        Err(Refusal::Unknown(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(Refusal::Zfs(error)) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    }
}

enum Refusal {
    Unknown(String),
    Zfs(String),
}

/// The `zfs send` arguments for `stream`, once its names resolve on this Machine.
async fn stream_args(programs: &Programs, stream: &SendStream) -> Result<Vec<String>, Refusal> {
    if let SendSource::Resume { token } = &stream.source {
        return Ok(vec!["-t".to_owned(), token.clone()]);
    }
    let pools = lines(
        &programs.zpool,
        &[
            "list",
            "-Hp",
            "-o",
            "name,size,allocated,free,health,readonly",
        ],
    )
    .await?;
    let pool = machine_pool::one_usable(&pools.join("\n"))
        .map_err(|error| Refusal::Zfs(error.to_string()))?
        .ok_or_else(|| {
            Refusal::Unknown("no Machine Pool is imported on this Machine".to_owned())
        })?;
    let root = format!("{}/{DATASET_ROOT}/{}", pool.name(), stream.name);
    let roots = lines(
        &programs.zfs,
        &["list", "-H", "-o", "name", "-t", "filesystem"],
    )
    .await?;
    if !roots.contains(&root) {
        return Err(Refusal::Unknown(format!(
            "Volume {} has no writer on this Machine",
            stream.name
        )));
    }
    let snapshots = lines(
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
        SendSource::Resume { .. } => unreachable!("resumes return above"),
    };
    let target = format!("{root}@{target}");
    if !snapshots.iter().any(|(name, _)| *name == target) {
        return Err(Refusal::Unknown(format!(
            "snapshot {target} does not exist"
        )));
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

async fn lines(program: &Path, args: &[&str]) -> Result<Vec<String>, Refusal> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| Refusal::Zfs(format!("could not run {}: {error}", program.display())))?;
    if !output.status.success() {
        return Err(Refusal::Zfs(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
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

    async fn start(dir: &TestDir, pools: &str) -> (String, CancellationToken) {
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
  'list -H -o name -t filesystem') printf 'old\nold/ployz\nold/ployz/data\ntank\ntank/ployz\ntank/ployz/data\ntank/ployz-mirror/other\n' ;;
  'list -H -o name,guid -t snapshot -d 1 tank/ployz/data') printf 'tank/ployz/data@w-1-1\t11\ntank/ployz/data@w-1-2\t12\n' ;;
  'send tank/ployz/data@w-1-2') printf 'full stream' ;;
  'send -i tank/ployz/data@w-1-1 tank/ployz/data@w-1-2') printf 'incremental stream' ;;
  'send -t token-1') printf 'resumed stream' ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&zfs, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve(listener, zfs, zpool, shutdown.clone()));
        (format!("http://{address}"), shutdown)
    }

    async fn get(base: &str, path: &str) -> (StatusCode, String) {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
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
            get(&base, "/volume-send/data?token=token-1").await,
            (StatusCode::OK, "resumed stream".to_owned())
        );
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
                "/volume-send/data?target=w-1-2&token=t",
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
}
