//! The boot commit of an admitted activation, and the rule it applies to each piece of
//! durable state.
//!
//! An admitted starting set leaves a pending activation and restarts the daemon. The next
//! process finishes it here, before it serves anything: every connection, stream, worker,
//! Build Grant and accepted want died with the old process, so nothing opened before the
//! commit can act after it. Durable state that could carry authority across the restart is
//! re-verified against the keys the commit keeps, or dropped. [`DURABLE`] names a rule for
//! every file and directory the daemon keeps in its data directory, and a test holds it to
//! what the daemon writes.

use std::{io, path::Path, process::Command};

use ployz_core::{DeviceCode, DeviceKey};
use thiserror::Error;

use crate::machine::{
    LocalMachineError, LocalMachineStore, StoreError, local_machine::PendingActivation,
};

#[cfg(test)]
#[path = "activation_tests.rs"]
mod tests;

/// What the commit does with one entry of the data directory. "Keep" means the entry only
/// refuses, bounds or describes, and admits nothing.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rule {
    /// Rewritten by the commit.
    Rewritten,
    /// Re-verified against the kept keys, dropping what fails.
    Reverified,
    /// Deleted.
    Deleted,
    /// Kept: it admits nothing.
    Kept(&'static str),
}

/// Every entry the daemon keeps in its data directory, by name or prefix, and its rule.
/// A name ending in `/` is a directory whose whole subtree takes the rule.
#[cfg(test)]
pub(crate) const DURABLE: &[(&str, Rule)] = &[
    ("machine.json", Rule::Rewritten),
    (
        ".machine.json.tmp",
        Rule::Kept("an interrupted write of the record, removed on open"),
    ),
    (".ployzd.lock", Rule::Kept("the store's exclusive lock")),
    (
        ".machine.reset.pending",
        Rule::Kept("a reset the next start finishes, removing everything"),
    ),
    (
        ".upgrade-active",
        Rule::Kept("an upgrade the start-up reconcile settles; it launches no worker"),
    ),
    (
        "prepare/",
        Rule::Kept("install preparation, removed when the install finishes"),
    ),
    ("activation-pending.json", Rule::Deleted),
    ("join-approvals.json", Rule::Reverified),
    ("restart-approvals.json", Rule::Reverified),
    (
        "upgrade-attempt.json",
        Rule::Kept(
            "boot launches no worker from a stored attempt, and a running one installs only a Ployz-signed release, forward",
        ),
    ),
    (
        "sealing.key",
        Rule::Kept(
            "the Cluster's identity; once a recipient is installed it opens only values sealed before activation",
        ),
    ),
    (
        "recipient.key",
        Rule::Kept("installed only under a rotation the trusted snapshot verified"),
    ),
    ("recipient-generated.key", Rule::Deleted),
    (
        "recipient-rotation.json",
        Rule::Kept("the signed rotation that named recipient.key, which a joiner verifies"),
    ),
    (
        "machine.db",
        Rule::Kept("Container specs restore only what was placed, with pinned images"),
    ),
    ("machine.db-wal", Rule::Kept("machine.db's journal")),
    ("machine.db-shm", Rule::Kept("machine.db's journal")),
    (
        "bridge-stopped-members.json",
        Rule::Kept("rebuilt from the activated record and the filtered store"),
    ),
    (
        "bridge-member-addresses.json",
        Rule::Kept("rebuilt from the activated record and the filtered store"),
    ),
    (
        "acme/",
        Rule::Kept("the ACME account; DNS and ACME authority stay with Cloud"),
    ),
    (
        "ingress/",
        Rule::Kept("Caddy's own data, served from the Ingress Projection"),
    ),
    (
        "corrosion/",
        Rule::Kept("the replicated store, read through accepts_machine"),
    ),
    (
        "tailscale/",
        Rule::Kept(
            "the router's node state, restored to the same node and tailnet with the pinned image",
        ),
    ),
    ("tailscale.forgotten/", Rule::Deleted),
    (".run/", Rule::Kept("runtime files each start rewrites")),
];

/// The rule for `relative`, a path under the data directory.
#[cfg(test)]
pub(crate) fn rule_of(relative: &Path) -> Option<Rule> {
    let text = relative.to_string_lossy();
    DURABLE.iter().find_map(|(name, rule)| {
        let hit = match name.strip_suffix('/') {
            Some(dir) => text == dir || text.starts_with(&format!("{dir}/")),
            None => text == *name,
        };
        hit.then_some(*rule)
    })
}

/// The Volume plugin, which runs apart from the daemon. Stopping its unit ends its cgroup,
/// and any `zfs send` or receive in it, before the commit.
pub(crate) trait PluginUnit {
    fn stop(&self) -> io::Result<()>;
    fn start(&self) -> io::Result<()>;
}

/// The plugin unit of this daemon: systemd's when the daemon runs as `ployz.service`,
/// and none otherwise.
pub(crate) fn plugin_unit() -> &'static dyn PluginUnit {
    match crate::runtime_unit::UnitSupervision::detect() {
        crate::runtime_unit::UnitSupervision::Systemd => &SystemdPlugin,
        crate::runtime_unit::UnitSupervision::External => &NoPlugin,
    }
}

/// The plugin as systemd runs it on an installed Machine.
struct SystemdPlugin;

impl PluginUnit for SystemdPlugin {
    fn stop(&self) -> io::Result<()> {
        systemctl(
            "stop",
            &[
                crate::units::Unit::VolumePluginSocket.name(),
                crate::units::Unit::VolumePluginService.name(),
            ],
        )
    }

    fn start(&self) -> io::Result<()> {
        systemctl(
            "start",
            &[
                crate::units::Unit::VolumePluginSocket.name(),
                crate::units::Unit::VolumePluginService.name(),
            ],
        )
    }
}

fn systemctl(verb: &str, units: &[&str]) -> io::Result<()> {
    let status = Command::new("systemctl").arg(verb).args(units).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "systemctl {verb} exited with {status}"
        )))
    }
}

/// No plugin unit: the daemon runs outside systemd, as in tests.
pub(crate) struct NoPlugin;

impl PluginUnit for NoPlugin {
    fn stop(&self) -> io::Result<()> {
        Ok(())
    }

    fn start(&self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CommitError {
    #[error("could not read or remove the pending activation")]
    Pending(#[source] io::Error),
    #[error("could not stop or start the Volume plugin around the activation")]
    Plugin(#[source] io::Error),
    #[error("could not apply the activation's rule to the data directory")]
    Durable(#[source] io::Error),
    #[error(transparent)]
    Log(#[from] LocalMachineError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Finish a pending activation, if one is waiting: stop the Volume plugin, apply every
/// re-verify and delete rule, make the one atomic write of the record, remove the pending
/// file, and start the plugin again. Call it before anything serves. A crash before the
/// write leaves the pending file for the next boot, and every step before it can run again.
/// A crash after it leaves a pending file the record already committed, which the next
/// boot removes before starting the plugin.
///
/// # Errors
/// Returns a [`CommitError`] naming the step that failed. The daemon then refuses to
/// start, and systemd tries again.
pub(crate) fn commit_pending(
    store: &mut LocalMachineStore,
    plugin: &dyn PluginUnit,
) -> Result<(), CommitError> {
    let data_dir = store.data_dir().to_path_buf();
    let Some(pending) = PendingActivation::read(&data_dir).map_err(CommitError::Pending)? else {
        return Ok(());
    };
    if store.record().activation() == Some(pending.operation) {
        PendingActivation::remove(&data_dir).map_err(CommitError::Pending)?;
        return plugin.start().map_err(CommitError::Plugin);
    }
    plugin.stop().map_err(CommitError::Plugin)?;
    let trusted = kept_devices(store, &pending);
    let dropped_proposals =
        crate::machine::local_machine::reverify_join_proposals(&data_dir, &trusted)
            .map_err(CommitError::Durable)?;
    let dropped =
        crate::machine::local_machine::ConsumptionLog::new(&data_dir).at_activation(&trusted)?;
    remove_dir(&data_dir.join("tailscale.forgotten"))?;
    remove_file(&data_dir.join("recipient-generated.key"))?;
    let retired = store.commit_activation(&pending)?;
    PendingActivation::remove(&data_dir).map_err(CommitError::Pending)?;
    plugin.start().map_err(CommitError::Plugin)?;
    tracing::info!(
        operation = %pending.operation,
        members = pending.members.len(),
        retired_devices = ?retired.devices,
        retired_passkeys = ?retired.passkeys,
        dropped_joins = ?dropped.joins,
        dropped_proposals = ?dropped_proposals,
        dropped_accepted = dropped.accepted,
        withdrawn_grants = dropped.grants,
        "activation committed"
    );
    Ok(())
}

/// The Device keys the commit keeps: those in the snapshot, the signer, and the kept keys.
fn kept_devices(store: &LocalMachineStore, pending: &PendingActivation) -> Vec<DeviceKey> {
    let record = store.record();
    let kept: Vec<DeviceCode> = pending.kept().map(|(code, _)| code).collect();
    record
        .held_keys()
        .devices
        .into_iter()
        .filter(|key| {
            kept.contains(&key.code())
                || record
                    .trusted_snapshot()
                    .is_some_and(|snapshot| snapshot.holds_key(key.code()))
        })
        .collect()
}

fn remove_dir(dir: &Path) -> Result<(), CommitError> {
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(CommitError::Durable(error)),
        _ => Ok(()),
    }
}

fn remove_file(file: &Path) -> Result<(), CommitError> {
    match std::fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(CommitError::Durable(error)),
        _ => Ok(()),
    }
}
