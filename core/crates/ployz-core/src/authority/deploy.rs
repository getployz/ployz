//! The digest a signed version names for one Service's spec, and what an approver's
//! client shows before it signs one.
//!
//! A Machine recomputes the [`PayloadDigest`] from the spec it is asked to create, so
//! Cloud can carry a signature but cannot change the image, command, environment, Sealed
//! values, mounts, Config bytes, ports, privileges or hook it covers. The digest leaves out the run fields, which Cloud picks
//! per run: see [`run_fields_cleared`]. The image must also be content-addressed, since a
//! tag names whatever Cloud last wrote under it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::num::NonZeroU32;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    birth::{BirthError, Mounts},
    canonical::{self, CanonicalError, drop_nulls},
    rule_draft::settings,
    shown::Shown,
};
use crate::value::hex_newtype;
use crate::{
    ContainerKind, ContainerResources, Namespace, Placement, PortPublication, PullPolicy,
    RawVolumeSource, RequestedServiceSpec, ResolvedServiceSpec, ServiceContainerSpec, ServiceMode,
    ServiceName, ServiceNetwork, ServiceVolume, UpdateConfig, VolumeDriver,
};

hex_newtype!(
    /// SHA-256 of one Service's requested spec in canonical JSON, without its replica
    /// counts and rollout order, written as 64 lowercase hex digits.
    PayloadDigest, "payload digest", 32, "", "64 lowercase hex digits"
);

/// `spec` with every run field set to one fixed value. A run field is one Cloud picks per
/// run: where, how many, in what order, from which cache and within what limits. It never
/// changes what runs, what it reads or what it reaches.
///
/// Every field is named, so a new one fails to compile until it is filed as signed or run.
pub(super) fn run_fields_cleared(spec: &RequestedServiceSpec) -> RequestedServiceSpec {
    let RequestedServiceSpec {
        name,
        mode: _,
        container,
        placement,
        regions,
        ports,
        mount_graph,
        pre_deploy,
        update: _,
    } = spec;
    let host_data = placement_signed(spec);
    let ServiceContainerSpec {
        image,
        command,
        entrypoint,
        environment,
        sealed_identity,
        labels,
        hostname,
        extra_hosts,
        cap_add,
        cap_drop,
        healthcheck,
        pull_policy: _,
        init,
        user,
        working_directory,
        tty,
        open_stdin,
        privileged,
        pid_mode,
        resources,
        stop_timeout_secs,
        stop_signal,
        sysctls,
        restart,
        network,
        shared_writes,
    } = container;
    let ContainerResources {
        cpu_nanos: _,
        memory_bytes: _,
        memory_reservation_bytes: _,
        shared_memory_bytes,
        devices,
        device_reservations,
        ulimits,
    } = resources;
    RequestedServiceSpec {
        name: name.clone(),
        mode: ServiceMode::Global,
        container: ServiceContainerSpec {
            image: image.clone(),
            command: command.clone(),
            entrypoint: entrypoint.clone(),
            environment: environment.clone(),
            sealed_identity: sealed_identity.clone(),
            labels: labels.clone(),
            hostname: hostname.clone(),
            extra_hosts: extra_hosts.clone(),
            cap_add: cap_add.clone(),
            cap_drop: cap_drop.clone(),
            healthcheck: healthcheck.clone(),
            pull_policy: PullPolicy::Missing,
            init: *init,
            user: user.clone(),
            working_directory: working_directory.clone(),
            tty: *tty,
            open_stdin: *open_stdin,
            privileged: *privileged,
            pid_mode: pid_mode.clone(),
            resources: ContainerResources {
                cpu_nanos: None,
                memory_bytes: None,
                memory_reservation_bytes: None,
                shared_memory_bytes: *shared_memory_bytes,
                devices: devices.clone(),
                device_reservations: device_reservations.clone(),
                ulimits: ulimits.clone(),
            },
            stop_timeout_secs: *stop_timeout_secs,
            stop_signal: stop_signal.clone(),
            sysctls: sysctls.clone(),
            restart: *restart,
            network: *network,
            shared_writes: shared_writes.clone(),
        },
        placement: if host_data {
            placement.clone()
        } else {
            Placement::default()
        },
        // An empty map means every region, so for a spec that reads host data the
        // region keys stay signed and only the counts are cleared.
        regions: if host_data {
            regions
                .keys()
                .map(|region| (region.clone(), NonZeroU32::MIN))
                .collect()
        } else {
            BTreeMap::new()
        },
        ports: ports.clone(),
        mount_graph: mount_graph.with_bounds_cleared(),
        pre_deploy: pre_deploy.clone(),
        update: UpdateConfig::default(),
    }
}

/// Whether where `spec` runs is a signed choice. That holds when it mounts any Volume but a
/// tmpfs mount, since a named Volume, even one Docker backs with tmpfs, is found by name and
/// two Servers can hold same-named Volumes with different contents, or when it has any
/// [`host_reach`] beyond a Volume it only declares.
pub(super) fn placement_signed(spec: &RequestedServiceSpec) -> bool {
    spec.volume_graph()
        .mounted_volumes()
        .any(|volume| !matches!(volume.source.kind(), RawVolumeSource::Tmpfs { .. }))
        || !reach(spec, &[]).is_empty()
}

impl PayloadDigest {
    /// The digest of `spec` as it runs in `namespace`. Managed Volumes are scoped to
    /// `namespace` first, as the Machine's resolved spec carries them, and the run fields
    /// are cleared. A field serde writes as `null` is left out, as absent.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the spec has no canonical form, or when scoping
    /// its Volumes to `namespace` makes two of them conflict.
    pub fn of(spec: &RequestedServiceSpec, namespace: &Namespace) -> Result<Self, CanonicalError> {
        let mut spec = spec.clone();
        spec.mount_graph = spec
            .mount_graph
            .scope_to_namespace(namespace)
            .map_err(|error| CanonicalError::Parse(error.to_string()))?;
        Self::of_scoped(&spec, |_| {})
    }

    /// The digest of `spec` with its Volumes already scoped, after `edit` changed its JSON
    /// form, such as a preview template's placeholders.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the spec has no canonical form.
    pub(super) fn of_scoped(
        spec: &RequestedServiceSpec,
        edit: impl FnOnce(&mut Value),
    ) -> Result<Self, CanonicalError> {
        let mut value = serde_json::to_value(run_fields_cleared(spec))
            .map_err(|error| CanonicalError::Parse(error.to_string()))?;
        drop_nulls(&mut value);
        edit(&mut value);
        let body = canonical::to_canonical(&value)?;
        Ok(Self(Sha256::digest(body.as_bytes()).into()))
    }

    /// The first 12 hex digits, for a log line or a person to compare.
    #[must_use]
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(12);
        text
    }
}

/// Whether `image` names its content, as [`ImageRef::content`](crate::ImageRef::content) reads it.
#[must_use]
pub fn content_addressed(image: &str) -> bool {
    crate::ImageRef::new(image).content().is_some()
}

/// One Service as the approver's client shows it before signing: the digest it signs, its
/// image, what the spec asks of the host beyond an ordinary Container, and
/// every other setting it signs. The CLI and the browser show these, computed from the
/// specs they sign.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ApprovedChange {
    pub service: ServiceName,
    /// The image the Servers run, always by digest.
    pub image: String,
    /// The tag the Saved State names, when the client pinned it to `image`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tag: Option<String>,
    pub digest: PayloadDigest,
    /// Privileges, devices, capabilities, PID mode, sysctls, host ports, Bind Mounts and
    /// the hook.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub host: Vec<String>,
    /// Every field the digest covers that holds a value, beside the image, from
    /// [`settings`]. Cloud relays the spec, so a field it planted shows here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub settings: Vec<String>,
    /// The mount lines among `settings` the person must say yes to: data that differs
    /// from the approved version or from what the Servers run now, a first attach, and a
    /// new volume that may hide data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub confirm: Vec<String>,
}

/// Why [`ApprovedChange::of`] can't show a plan.
#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    #[error(transparent)]
    Digest(#[from] CanonicalError),
    #[error(transparent)]
    Mounts(#[from] BirthError),
}

impl ApprovedChange {
    /// One change per Service of `target`, sorted by name and distinct. A Service in `pins` is approved with its image set
    /// to the pinned reference, so the digest covers the spec the runner sends. `mounts`
    /// names the data each managed Volume mount holds.
    ///
    /// # Errors
    /// Returns [`ChangeError::Digest`] when a spec has no [`PayloadDigest`] in `namespace`,
    /// and [`ChangeError::Mounts`] when [`Mounts::lines`] refuses a mount.
    pub fn of(
        target: &[RequestedServiceSpec],
        namespace: &Namespace,
        pins: &BTreeMap<ServiceName, String>,
        mounts: &Mounts<'_>,
    ) -> Result<Vec<Self>, ChangeError> {
        let mut changes = target
            .iter()
            .map(|spec| {
                let mut pinned = spec.clone();
                let tag = pins
                    .get(&spec.name)
                    .map(|pin| std::mem::replace(&mut pinned.container.image, pin.clone()));
                let mounted = mounts.lines(spec)?;
                let confirm = mounted.confirm.iter().map(ToString::to_string).collect();
                let region = mounts.region(spec);
                Ok(Self {
                    service: spec.name.clone(),
                    digest: PayloadDigest::of(&pinned, namespace)?,
                    image: pinned.container.image,
                    tag,
                    host: host_sensitive(spec).into_iter().map(String::from).collect(),
                    settings: settings(spec, mounted)
                        .into_iter()
                        .chain(region)
                        .map(String::from)
                        .collect(),
                    confirm,
                })
            })
            .collect::<Result<Vec<_>, ChangeError>>()?;
        changes.sort_by(|left, right| left.service.cmp(&right.service));
        changes.dedup_by(|left, right| left.service == right.service);
        Ok(changes)
    }

    /// `Approved web: nginx@sha256:<64 hex>`, or `nginx:1.29-alpine → @sha256:0123456789ab…`
    /// for a tag this client pinned, then what the spec asks of the host. A digest the
    /// spec names shows whole, since Cloud relays it and a short prefix is cheap to match.
    /// A pin is this client's own registry read of the tag shown, so its prefix is enough.
    #[must_use]
    pub fn line(&self) -> String {
        let hex = crate::ImageRef::new(&self.image)
            .digest
            .and_then(|digest| digest.strip_prefix("sha256:"));
        let image = match (&self.tag, hex) {
            (Some(tag), Some(hex)) => Shown::text(tag)
                .and_words(" → @sha256:")
                .and_text(hex.get(..12).unwrap_or(hex))
                .and_words("…"),
            _ => Shown::text(&self.image),
        };
        let mut line: String = Shown::words("Approved ")
            .and_text(self.service.as_str())
            .and_words(": ")
            .and(image)
            .into();
        if !self.host.is_empty() {
            let _ = write!(line, " ({})", self.host.join(", "));
        }
        line
    }

    /// [`Self::line`], then each setting on its own line, indented, as the approver reads
    /// them before signing.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        std::iter::once(self.line())
            .chain(self.settings.iter().map(|setting| format!("  {setting}")))
            .collect()
    }
}

/// What `spec` asks of the host beyond an ordinary Container: its [`host_reach`], with the
/// hook's command before a privileged hook.
pub(super) fn host_sensitive(spec: &RequestedServiceSpec) -> Vec<Shown> {
    let mut host = host_reach(spec);
    if let Some(hook) = &spec.pre_deploy {
        let privileged = usize::from(hook.privileged == Some(true));
        host.insert(
            host.len().saturating_sub(privileged),
            hook.command
                .as_slice()
                .iter()
                .fold(Shown::words("hook"), |shown, word| {
                    shown.and_words(" ").and_text(word)
                }),
        );
    }
    host
}

/// What `spec` reaches on its Machine beyond its own Environment: privileges, devices and
/// device reservations, capabilities, PID mode, sysctls, host ports, Bind Mounts, Docker
/// volumes it names itself or that are not [`plain_local`], the Ingress
/// holder's network, and a privileged hook. Only a Device or a passkey's own prompt
/// approves any of it.
pub(crate) fn host_reach(spec: &RequestedServiceSpec) -> Vec<Shown> {
    reach(spec, spec.volume_graph().volumes())
}

/// [`host_reach`], naming only what `volumes` of `spec` reach.
fn reach(spec: &RequestedServiceSpec, volumes: &[ServiceVolume]) -> Vec<Shown> {
    let container = &spec.container;
    let mut host = Vec::new();
    if container.privileged {
        host.push(Shown::words("privileged"));
    }
    host.extend(container.resources.devices.iter().map(|device| {
        Shown::words("device ")
            .and_text(device.machine_path.as_str())
            .and_words(":")
            .and_text(device.container_path.as_str())
    }));
    host.extend(
        container
            .resources
            .device_reservations
            .iter()
            .map(|reservation| match &reservation.driver {
                Some(driver) => Shown::words("reserves ")
                    .and_text(driver)
                    .and_words(" devices"),
                None => Shown::words("reserves devices"),
            }),
    );
    host.extend(
        container
            .cap_add
            .iter()
            .map(|cap| Shown::words("adds ").and_text(cap)),
    );
    if let Some(pid_mode) = &container.pid_mode {
        host.push(Shown::words("pid ").and_text(&pid_mode.to_string()));
    }
    host.extend(
        container
            .sysctls
            .keys()
            .map(|key| Shown::words("sysctl ").and_text(key)),
    );
    host.extend(spec.ports.iter().filter_map(|port| match port {
        PortPublication::Host { published_port, .. } => {
            Some(Shown::words("host port ").and_text(&published_port.to_string()))
        }
        PortPublication::Ingress { .. } => None,
    }));
    host.extend(volumes.iter().filter_map(|volume| {
        match volume.source.kind() {
            RawVolumeSource::Bind { machine_path, .. } => {
                Some(Shown::words("binds ").and_text(machine_path.as_str()))
            }
            RawVolumeSource::External { name } => {
                Some(Shown::words("uses volume ").and_text(name.as_str()))
            }
            RawVolumeSource::Ordinary { name, driver, .. } if !plain_local(driver) => Some(
                Shown::words("volume ")
                    .and_text(name.as_str())
                    .and_words(" with driver ")
                    .and_text(driver.name())
                    .and_words(if driver.options().is_empty() {
                        ""
                    } else {
                        " options"
                    }),
            ),
            RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        }
    }));
    match container.network {
        Some(ServiceNetwork::IngressHolder) => host.push(Shown::words("joins the ingress network")),
        Some(ServiceNetwork::Tunnel | ServiceNetwork::TunnelOrigin) => {
            host.push(Shown::words("joins the tunnel network"));
        }
        Some(ServiceNetwork::Unrecognized) => host.push(Shown::words("joins an unknown network")),
        None => {}
    }
    if spec
        .pre_deploy
        .as_ref()
        .is_some_and(|hook| hook.privileged == Some(true))
    {
        host.push(Shown::words("privileged hook"));
    }
    host
}

/// Whether `driver` is Docker's `local` driver making a plain Volume, or a tmpfs with
/// only size, mode and ownership options. Any other option, such as `o=bind`, can mount a
/// host path or device.
fn plain_local(driver: &VolumeDriver) -> bool {
    const TMPFS: [&str; 6] = ["size", "mode", "uid", "gid", "nr_inodes", "nr_blocks"];
    let options = driver.options();
    let option = |key: &str| options.get(key).map(String::as_str);
    driver.name() == "local"
        && (options.is_empty()
            || (option("type") == Some("tmpfs")
                && option("device").is_none_or(|device| device == "tmpfs")
                && options
                    .keys()
                    .all(|key| ["type", "device", "o"].contains(&key.as_str()))
                && option("o").is_none_or(|mount| {
                    mount.split(',').all(|part| {
                        TMPFS.contains(&part.split_once('=').map_or(part, |(key, _)| key))
                    })
                })))
}

/// A Container this Machine is asked to create.
#[derive(Clone, Copy, Debug)]
pub struct Creation<'a> {
    pub service: &'a ServiceName,
    pub kind: ContainerKind,
    pub spec: &'a ResolvedServiceSpec,
}

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
