//! The observe process: the metric sampler, the crash and request outboxes,
//! and the Docker events loop that settles each container exit.
//!
//! It only observes this Machine: it never places, moves, restarts or removes a
//! container, and no Deploy waits on it.

pub mod cleanup;
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
mod settled;
mod ship;
mod signal;
pub(crate) mod unit;

use std::{
    fs,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use bollard::Docker;
use fs2::FileExt;
use layout::StoreRoot;

const LEGACY_LOG_STORE: &str = "ployz-observe";
const LOCK_FILE: &str = "harvester.lock";

/// Runs the metric emitter, the shippers and the Docker events loop until
/// SIGTERM. `data_dir` holds the observe store and the
/// Machine record the harvester stamps markers with.
///
/// # Errors
///
/// Returns an error when Docker cannot be reached, another observe process holds
/// the store, or or the store cannot be created.
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
    tracing::info!(store = %store.path().display(), "observing this Machine");
    tokio::spawn(metrics::emit::run(store.clone()));
    let salts = Arc::new(salts::Salts::open(&store, ship::unix_seconds()));
    let crashes = ship::Outbox::open(&store)?;
    spawn_crash_sender(crashes.clone(), data_dir)?;
    if let Err(error) = harvest_requests(&store, run_dir, data_dir, salts) {
        tracing::warn!(%error, "cannot harvest requests; crash events still post");
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

fn spawn_shipper(
    outbox: Arc<ship::RequestOutbox>,
    salts: Arc<salts::Salts>,
    data_dir: &Path,
) -> io::Result<()> {
    let machine_dir = data_dir.to_path_buf();
    let shipper = ship::Shipper::new(outbox, salts, move || ship::push_target(&machine_dir))
        .map_err(io::Error::other)?;
    tokio::spawn(shipper.run());
    Ok(())
}

fn spawn_crash_sender(crashes: Arc<ship::Outbox>, data_dir: &Path) -> io::Result<()> {
    let machine_dir = data_dir.to_path_buf();
    let sender = ship::CrashSender::new(crashes, collector::CRASH_RECEIVER, move || {
        ship::push_target(&machine_dir).is_some()
    })
    .map_err(io::Error::other)?;
    tokio::spawn(sender.run());
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
