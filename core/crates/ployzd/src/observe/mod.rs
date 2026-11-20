//! The observe process: the metric sampler, the crash and request outboxes,
//! and the Docker events loop that settles each container exit.
//!
//! It only observes this Machine: it never places, moves, restarts or removes a
//! container, and no Deploy waits on it.

pub mod cleanup;
mod client;
mod clock;
pub(crate) mod collector;
mod exit_ledger;
#[cfg(test)]
mod fixtures;
mod harvest;
pub mod layout;
mod meta;
pub(crate) mod metrics;
mod request;
mod salts;
mod serve;
mod settled;
mod ship;
mod signal;
pub(crate) mod unit;
mod wire;

use std::{
    fs,
    fs::File,
    io,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use bollard::Docker;
pub use client::ObserveClient;
use fs2::FileExt;
use layout::StoreRoot;
use tokio::net::UnixListener;

const LEGACY_LOG_STORE: &str = "ployz-observe";
const LOCK_FILE: &str = "harvester.lock";

/// Runs the metric sampler, the shippers and the Docker events loop until
/// SIGTERM. `data_dir` holds the observe store and the
/// Machine record the sampler stamps rows with.
///
/// # Errors
///
/// Returns an error when Docker cannot be reached, another observe process holds
/// the store, or the store or socket cannot be created.
pub async fn run(run_dir: &Path, data_dir: &Path) -> io::Result<()> {
    crate::faults::check()?;
    crate::faults::leak_memory();
    let docker = Docker::connect_with_defaults().map_err(io::Error::other)?;
    let docker_root = docker
        .info()
        .await
        .map_err(io::Error::other)?
        .docker_root_dir
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Docker did not report its root dir"))?;
    let store = StoreRoot::under(data_dir);
    let _lock = lock_store(&store)?;
    store.prepare()?;
    remove_legacy_log_store(&docker_root);
    let listener = bind_socket(&ployz_core::MachineLayout::observe_socket_in(run_dir))?;
    tracing::info!(store = %store.path().display(), "observing this Machine");
    tokio::spawn(serve::serve(listener, store.clone()));
    // Cleanup deletes raw hours older than a day, so a long downtime's hours
    // are folded into rollups before it runs.
    if let Err(error) = metrics::rollup::fold_pending(&store, std::time::SystemTime::now()) {
        tracing::warn!(%error, "cannot fold metric hours");
    }
    tokio::spawn(metrics::sampler::run(
        docker.clone(),
        store.clone(),
        data_dir.to_path_buf(),
    ));
    let salts = Arc::new(salts::Salts::open(&store, ship::unix_seconds()));
    let crashes = ship::Outbox::open(&store)?;
    spawn_shipper(crashes.clone(), salts.clone(), data_dir)?;
    if let Err(error) = harvest_requests(&store, run_dir, data_dir, salts) {
        tracing::warn!(%error, "cannot harvest requests; crash reports still ship");
    }
    harvest::Harvester::run(docker, store, data_dir.to_path_buf(), crashes).await
}

fn harvest_requests(
    store: &StoreRoot,
    run_dir: &Path,
    data_dir: &Path,
    salts: Arc<salts::Salts>,
) -> io::Result<()> {
    let requests = ship::RequestOutbox::open(store)?;
    spawn_shipper(requests.clone(), salts.clone(), data_dir)?;
    request::spawn(run_dir, salts, requests)
}

fn spawn_shipper<S: ship::Stream>(
    outbox: Arc<S>,
    salts: Arc<salts::Salts>,
    data_dir: &Path,
) -> io::Result<()> {
    let machine_dir = data_dir.to_path_buf();
    let shipper = ship::Shipper::new(outbox, salts, move || ship::push_target(&machine_dir))
        .map_err(io::Error::other)?;
    tokio::spawn(shipper.run());
    Ok(())
}

/// Deletes the Log Store an earlier release kept in Docker's root dir. Its
/// files were links to Docker's own, so removing them frees only what the
/// store alone held, and Docker's logs stay.
fn remove_legacy_log_store(docker_root: &Path) {
    let legacy = docker_root.join(LEGACY_LOG_STORE);
    match fs::remove_dir_all(&legacy) {
        Ok(()) => tracing::info!(path = %legacy.display(), "deleted the old Log Store"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %legacy.display(), %error, "cannot delete the old Log Store");
        }
    }
}

fn lock_store(store: &StoreRoot) -> io::Result<File> {
    layout::create_store_dir(store.base())?;
    let lock = File::create(store.base().join(LOCK_FILE))?;
    lock.try_lock_exclusive().map_err(|error| {
        io::Error::new(
            error.kind(),
            "another ployzd observe is already running on this Machine",
        )
    })?;
    Ok(lock)
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
