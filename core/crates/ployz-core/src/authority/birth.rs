//! A Volume's birth as the approver's client mints it, and the line a person reads for
//! each mount before signing.
//!
//! The Config Store lists which Volumes a plan's specs mount for the first time. Only the
//! signer mints, and only it says "a new, empty volume": a birth Cloud relays in the spec
//! reads as the data it names, whatever Cloud claims about it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    TrustedKeys,
    canonical::Timestamp,
    deploy::PayloadDigest,
    keys::Signed,
    shown::Shown,
    version::{self, ServiceVersion},
};
use crate::{
    BirthNonce, ContainerKind, ContainerObservation, ContainerRuntimeObservation, DockerVolumeName,
    MountOrigins, Namespace, RawVolumeSource, RequestedServiceSpec, SealingPublicKey, ServiceMount,
    ServiceName, VolumeBirth, VolumeDriver, VolumeOrigin,
};

/// A Volume a plan's specs mount, as the Config Store names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PlannedVolume {
    /// What Ployz Cloud calls it now.
    pub name: String,
    /// No approval has mounted it and no Server can hold its data yet, so the approval
    /// that first mounts it mints its birth.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub unborn: bool,
}

/// Each Volume a plan mounts, by the name its specs declare it under.
pub type PlannedVolumes = BTreeMap<DockerVolumeName, PlannedVolume>;

/// The births one approval minted, by the name the specs declare each Volume under.
pub type Births = BTreeMap<DockerVolumeName, VolumeBirth>;

/// Why a spec can't be signed with the births it carries.
#[derive(Debug, thiserror::Error)]
pub enum BirthError {
    #[error("{0} mounts a volume from another environment. Nothing signed.")]
    OtherEnvironment(ServiceName),
    #[error(
        "{service} mounts {volume}, data from before Ployz recorded where data began, but \
         {why} Nothing signed."
    )]
    Legacy {
        service: ServiceName,
        volume: DockerVolumeName,
        why: Unvouched,
    },
}

/// Why nothing vouches for data from before births.
#[derive(Debug, thiserror::Error)]
pub enum Unvouched {
    /// A mount never drops the birth it had.
    #[error("the version you approved mounts it with a birth.")]
    Unbirthed,
    #[error("{UNASKED}. Retry with --connect ssh://<server>.")]
    Unasked,
    /// With the Servers that failed or didn't answer, by name.
    #[error(
        "no version you approved mounts it and no server lists a container using it{}. Start \
         its old container once, or restore the data into a new volume.",
        missing(.0)
    )]
    Unused(Vec<String>),
}

/// Give every mount in `target`'s specs of a Volume `volumes` lists as unborn, and whose
/// spec carries no birth for it, a birth minted now: one per Volume, named as `volumes` names
/// it and made in `namespace`, with the nonce `nonce` gives it.
///
/// # Errors
/// Returns [`BirthError::OtherEnvironment`] for a spec carrying a birth from another
/// Namespace, which every Server refuses.
pub fn mint<'a>(
    target: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
    (namespace, volumes): (&Namespace, &PlannedVolumes),
    made_at: Timestamp,
    mut nonce: impl FnMut(&DockerVolumeName) -> BirthNonce,
) -> Result<Births, BirthError> {
    let mut births = Births::new();
    for spec in target {
        if !spec.volume_graph().born_in(namespace) {
            return Err(BirthError::OtherEnvironment(spec.name.clone()));
        }
        spec.mount_graph = spec.mount_graph.with_births(|name| {
            let planned = volumes.get(name).filter(|planned| planned.unborn)?;
            let birth = births.entry(name.clone()).or_insert_with(|| VolumeBirth {
                namespace: namespace.clone(),
                name: planned.name.clone(),
                made_at,
                nonce: nonce(name),
            });
            Some(birth.clone())
        });
    }
    Ok(births)
}

/// The nonce minted for the Volume declared as `name` from `seed`, 16 random bytes one
/// approval draws once for all its births.
#[must_use]
pub fn nonce(seed: &[u8; 16], name: &DockerVolumeName) -> BirthNonce {
    let mut hash = Sha256::new();
    hash.update(b"ployz.volume.birth.v1\0");
    hash.update(seed);
    hash.update(name.as_str().as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest[..16]);
    BirthNonce::from_bytes(nonce)
}

/// 16 random bytes for [`nonce`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn seed() -> [u8; 16] {
    use rand_core::{OsRng, RngCore as _, TryRngCore as _};
    let mut seed = [0; 16];
    OsRng.unwrap_err().fill_bytes(&mut seed);
    seed
}

/// The births of `minted` that `spec` carries, which its signed version posts.
#[must_use]
pub fn carried(spec: &RequestedServiceSpec, minted: &Births) -> Births {
    spec.volume_graph()
        .volumes()
        .iter()
        .filter_map(|volume| {
            let name = volume.source.docker_volume_name()?;
            let birth = volume.source.birth()?;
            (minted.get(name) == Some(birth)).then(|| (name.clone(), birth.clone()))
        })
        .collect()
}

/// A parentless version a person or a Full CLI key signed, with the spec its digest
/// covers. Only [`Approved::verify`] makes one. It proves who signed, not that it is the
/// newest.
#[derive(Clone, Debug)]
pub struct Approved {
    spec: RequestedServiceSpec,
    signed_at: Timestamp,
}

impl Approved {
    /// `spec` as `statement` approved it, when `statement` opens as a root under `trusted`
    /// for `cluster`, names `service` in `namespace`, and `spec` comes to its digest.
    #[must_use]
    pub fn verify(
        statement: &Signed<ServiceVersion>,
        spec: RequestedServiceSpec,
        (trusted, cluster): (&TrustedKeys, SealingPublicKey),
        (namespace, service): (&Namespace, &ServiceName),
    ) -> Option<Self> {
        let version = version::open_root(statement, trusted, cluster).ok()?;
        let digest = PayloadDigest::of(&spec, namespace).ok();
        (version.service == *service
            && version.namespace == *namespace
            && digest == Some(version.digest))
        .then_some(Self {
            spec,
            signed_at: version.signed_at,
        })
    }

    #[must_use]
    pub fn spec(&self) -> &RequestedServiceSpec {
        &self.spec
    }
}

/// A Service's last approved version, as the signer could check it.
#[derive(Clone, Debug)]
pub enum Before {
    Approved(Approved),
    /// No Deployment ran the Service.
    New,
    Unknown(Unknown),
}

/// Why a Service has no approved version to compare with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unknown {
    /// It runs, but no Deploy a person approved is on record.
    NoRecord,
    /// The version on record doesn't check against the Servers' keys.
    Unchecked,
    /// The Servers' keys weren't read over a route that answers for itself.
    Unread,
    /// Ployz Cloud didn't say which version it is.
    Unsaid,
}

impl Unknown {
    fn why(self) -> &'static str {
        match self {
            Self::NoRecord => "it runs, but no deploy a person approved is on record",
            Self::Unchecked => {
                "the version Ployz Cloud returned doesn't check against your servers' keys"
            }
            Self::Unread => "your servers' keys weren't read over a route that answers for itself",
            Self::Unsaid => "Ployz Cloud didn't say",
        }
    }
}

/// The named-volume mounts of each Service Container in one Namespace, as its Servers
/// list them.
#[derive(Clone, Debug, Default)]
pub struct Held {
    containers: Vec<HeldContainer>,
    /// Each Server that failed or didn't answer, by name.
    unanswered: Vec<String>,
}

#[derive(Clone, Debug)]
struct HeldContainer {
    service: ServiceName,
    /// Running, paused or restarting, so it uses its data now.
    running: bool,
    /// By target, each mount that holds data: the physical name mounted there and the
    /// origin the Container's label expects of it. `None` for a bind mount, or a label that
    /// doesn't parse or leaves the name out.
    mounts: BTreeMap<String, Option<(DockerVolumeName, VolumeOrigin)>>,
}

impl Held {
    /// The Service Containers of `namespace` among `containers`, with the Servers in
    /// `unanswered` that failed or didn't answer. A Container without the origins label
    /// expects Legacy data at every mount.
    #[must_use]
    pub fn of(
        namespace: &Namespace,
        containers: &[ContainerObservation],
        unanswered: Vec<String>,
    ) -> Self {
        let containers = containers
            .iter()
            .filter(|seen| {
                seen.namespace == *namespace && seen.kind == ContainerKind::ServiceContainer
            })
            .map(|seen| {
                let label = seen.labels.get(crate::labels::VOLUME_ORIGINS);
                let origins = label.map(|label| MountOrigins::parse(label).ok());
                let graph = seen.resolved_spec.volume_graph();
                let mounts = graph.mounts().iter().filter_map(|mount| {
                    let source = &graph.volume_for(mount).source;
                    let named = source.docker_volume_name().and_then(|physical| {
                        let origin = match &origins {
                            None => Some(VolumeOrigin::legacy(physical)),
                            Some(origins) => origins.as_ref().and_then(|held| held.get(physical)),
                        };
                        Some((physical.clone(), origin?))
                    });
                    holds_data(source.kind()).then(|| (mount.target.as_str().to_owned(), named))
                });
                HeldContainer {
                    service: seen.resolved_spec.name.clone(),
                    running: matches!(
                        seen.runtime,
                        ContainerRuntimeObservation::Running { .. }
                            | ContainerRuntimeObservation::Paused
                            | ContainerRuntimeObservation::Restarting
                    ),
                    mounts: mounts.collect(),
                }
            })
            .collect();
        Self {
            containers,
            unanswered,
        }
    }

    /// A Container that mounts `mounted`'s data, a running one when any runs.
    fn holder(&self, mounted: &Mounted<'_>) -> Option<&HeldContainer> {
        let mut holders = self
            .containers
            .iter()
            .filter(|container| (container.mounts.values()).any(|held| mounted.is(held.as_ref())));
        (holders.clone().find(|container| container.running)).or_else(|| holders.next())
    }
}

/// What a person's Deploy compares each mount with.
#[derive(Clone, Copy, Debug)]
pub struct Against<'a> {
    pub before: &'a BTreeMap<ServiceName, Before>,
    /// `None` when no route that answers for itself listed the Containers.
    pub held: Option<&'a Held>,
}

/// What the approver's client knows about the Volumes a spec mounts beyond the spec.
#[derive(Clone, Copy, Debug)]
pub struct Mounts<'a> {
    /// The Namespace the approval signs for, which names each Volume on its Servers.
    pub namespace: &'a Namespace,
    pub volumes: &'a PlannedVolumes,
    /// The births this approval minted, the only ones that read as new.
    pub minted: &'a Births,
    pub now: Timestamp,
    /// `None` shows every mount in full and asks nothing: a silent child's mounts are
    /// pinned to its parent. A rule reads its lines from [`Self::asked`].
    pub against: Option<Against<'a>>,
}

/// The lines for a spec's mounts, the targets they cover, and the lines a person must say
/// yes to before signing.
#[derive(Clone, Debug, Default)]
pub struct MountLines {
    pub lines: Vec<Shown>,
    pub targets: Vec<String>,
    pub confirm: Vec<Shown>,
}

/// What one mount's line says after its settings.
enum Said {
    /// Counted in one line with the other mounts that keep what the version signed at
    /// this time approved.
    Kept(Timestamp),
    Told(Shown),
    Asked(Shown),
}

impl Mounts<'_> {
    /// One line for each mount of `spec` that a managed or Docker volume backs, such as
    /// `mounts managed volume previews at /srv/previews, read-write: a new, empty volume`,
    /// and the targets of those mounts. Each line names the data its Servers check: the
    /// birth, or for data from before births, the name the Volume has on its Servers. It
    /// carries every other setting the mount signs. Every other mount shows with the
    /// spec's other settings.
    ///
    /// Against a person's Deploy, mounts that keep the data the approved version mounts,
    /// as every listed Container of the Service does, collapse into one line. Any other
    /// existing data, and a new volume that may hide data, also goes in `confirm`.
    ///
    /// # Errors
    /// Returns [`BirthError::Legacy`] for data from before births that neither the approved
    /// version nor a listed Container vouches for.
    pub fn lines(&self, spec: &RequestedServiceSpec) -> Result<MountLines, BirthError> {
        match self.against {
            None => Ok(self.shown(spec)),
            Some(against) => self.each(spec, |mounted| self.review(against, spec, mounted)),
        }
    }

    /// [`Self::lines`] with nothing compared: every mount in full, none asked.
    #[must_use]
    pub fn shown(&self, spec: &RequestedServiceSpec) -> MountLines {
        let told = |mounted: &Mounted<'_>| {
            Ok::<_, std::convert::Infallible>(Said::Told(self.data(mounted, Shown::default())))
        };
        let Ok(lines) = self.each(spec, told);
        lines
    }

    /// [`Self::lines`] for a rule, which has no verified version to compare with: every
    /// mount in full, and each one with data that is not new also asked. A mount of an
    /// external Docker volume or a host path is asked too. It keeps its place among the
    /// spec's other settings.
    #[must_use]
    pub fn asked(&self, spec: &RequestedServiceSpec) -> MountLines {
        let said = |mounted: &Mounted<'_>| {
            let data = self.data(mounted, Shown::default());
            Ok::<_, std::convert::Infallible>(match self.is_minted(mounted) {
                true => Said::Told(data),
                false => Said::Asked(data),
            })
        };
        let Ok(mut lines) = self.each(spec, said);
        let graph = spec.volume_graph();
        lines.confirm.extend(
            (graph.mounts().iter())
                .filter_map(|mount| outside(mount, graph.volume_for(mount).source.kind())),
        );
        lines
    }

    /// Whether `mounted` is a Volume this approval minted, which holds no data yet.
    fn is_minted(&self, mounted: &Mounted<'_>) -> bool {
        mounted.volume.birth.is_some()
            && self.minted.get(mounted.volume.name) == mounted.volume.birth
    }

    /// The lines for `spec`'s mounts, each as `say` says it.
    fn each<E>(
        &self,
        spec: &RequestedServiceSpec,
        mut say: impl FnMut(&Mounted<'_>) -> Result<Said, E>,
    ) -> Result<MountLines, E> {
        let mut out = MountLines::default();
        let mut kept = Vec::new();
        for mounted in mounts_of(self.namespace, spec) {
            out.targets.push(mounted.mount.target.as_str().to_owned());
            let said = say(&mounted)?;
            let line = |data| self.prefix(&mounted).and(data);
            match said {
                Said::Kept(signed_at) => kept.push(signed_at),
                Said::Told(data) => out.lines.push(line(data)),
                Said::Asked(data) => {
                    out.confirm.push(line(data.clone()));
                    out.lines.push(line(data));
                }
            }
        }
        if let Some(signed_at) = kept.first() {
            let count = match kept.len() {
                1 => Shown::words("1 volume keeps"),
                count => Shown::text(&count.to_string()).and_words(" volumes keep"),
            };
            let line = count
                .and_words(" the data you approved (signed ")
                .and(when(*signed_at, self.now))
                .and_words(")");
            out.lines.insert(0, line);
        }
        let unknown = match self.against.map(|against| against.before.get(&spec.name)) {
            Some(Some(Before::Unknown(why))) => Some(*why),
            Some(None) => Some(Unknown::Unsaid),
            _ => None,
        };
        if let Some(why) = unknown
            && !out.lines.is_empty()
        {
            let heading =
                Shown::words("no approved version to compare with: ").and_words(why.why());
            out.lines.insert(0, heading);
        }
        Ok(out)
    }

    /// `mounts managed volume previews at /srv/previews, read-write: `.
    fn prefix(&self, mounted: &Mounted<'_>) -> Shown {
        let (mount, volume) = (mounted.mount, &mounted.volume);
        let line = Shown::words(match volume.driver {
            None => "mounts managed volume ",
            Some(_) => "mounts Docker volume ",
        });
        let line = match volume.birth {
            Some(birth) => line.and_text(&birth.name),
            None => line.and_text(mounted.physical.as_str()),
        };
        let mut line = line.and_words(" at ").and_text(mount.target.as_str());
        if let Some(subpath) = &mount.subpath {
            line = line.and_words(", subpath ").and_text(subpath);
        }
        if mount.no_copy {
            line = line.and_words(", no initial copy");
        }
        if !volume.labels.is_empty() {
            line = line.and_words(", labels ").and_value(volume.labels);
        }
        if let Some(driver) = volume
            .driver
            .filter(|driver| driver.name() != "local" || !driver.options().is_empty())
        {
            line = line.and_words(", driver ").and_value(driver);
        }
        line.and_words(match mount.read_only {
            true => ", read-only: ",
            false => ", read-write: ",
        })
    }

    /// The data `mounted` holds, with `witness` before the name Ployz Cloud gives it.
    fn data(&self, mounted: &Mounted<'_>, witness: Shown) -> Shown {
        let name = mounted.volume.name;
        let called = self.volumes.get(name).map(|planned| planned.name.as_str());
        match mounted.volume.birth {
            Some(birth) if self.minted.get(name) == Some(birth) => {
                Shown::words("a new, empty volume").and(witness)
            }
            Some(birth) => calls(
                self.made(birth),
                called.filter(|called| *called != birth.name),
            )
            .and(witness),
            None => calls(Shown::words(LEGACY), called).and(witness),
        }
    }

    /// What `mounted` says against the approved version and the listed Containers.
    fn review(
        &self,
        against: Against<'_>,
        spec: &RequestedServiceSpec,
        mounted: &Mounted<'_>,
    ) -> Result<Said, BirthError> {
        let (service, target, held) = (&spec.name, mounted.mount.target.as_str(), against.held);
        let mine: Vec<&HeldContainer> = held
            .into_iter()
            .flat_map(|held| &held.containers)
            .filter(|container| container.service == *service)
            .collect();
        let complete = held.is_some_and(|held| held.unanswered.is_empty());
        let base = match against.before.get(service) {
            Some(Before::Approved(base)) => Some(base),
            _ => None,
        };
        let was = base.map_or_else(Vec::new, |base| mounts_of(self.namespace, &base.spec));
        let at_target = was
            .iter()
            .find(|was| was.mount.target == mounted.mount.target);
        if self.is_minted(mounted) {
            let replaced = base.is_some_and(|base| holds_at(&base.spec, target))
                || mine
                    .iter()
                    .any(|container| container.mounts.contains_key(target));
            return Ok(match (replaced, complete) {
                (true, _) => Said::Asked(
                    Shown::words("a new, empty volume in place of the data at ")
                        .and_text(target)
                        .and_words(". That data stays, but ")
                        .and_text(service.as_str())
                        .and_words(" no longer sees it."),
                ),
                (false, true) => Said::Told(Shown::words("a new, empty volume")),
                (false, false) => {
                    Said::Asked(Shown::words("a new, empty volume").and(unseen(held)))
                }
            });
        }
        let holder = held.and_then(|held| held.holder(mounted));
        let seen = match (holder, held) {
            (Some(holder), _) => witness(holder),
            (None, Some(_)) => Shown::words(", that no server lists a container using"),
            (None, None) => Shown::default(),
        }
        .and(unseen(held));
        if mounted.volume.birth.is_none() {
            let named = || was.iter().filter(|was| was.physical == mounted.physical);
            let refused = |why| BirthError::Legacy {
                service: service.clone(),
                volume: mounted.physical.clone(),
                why,
            };
            if named().any(|was| was.volume.birth.is_some()) {
                return Err(refused(Unvouched::Unbirthed));
            }
            if named().next().is_none() {
                return match (holder, held) {
                    (Some(_), _) => Ok(Said::Asked(self.data(mounted, seen))),
                    (None, None) => Err(refused(Unvouched::Unasked)),
                    (None, Some(held)) => Err(refused(Unvouched::Unused(held.unanswered.clone()))),
                };
            }
        }
        let agrees = |container: &&HeldContainer| {
            mounted.is(container.mounts.get(target).and_then(Option::as_ref))
        };
        let current =
            complete && mine.iter().any(|container| container.running) && mine.iter().all(agrees);
        if let Some(base) = base {
            match at_target {
                Some(was) if was.same_data(mounted) => {
                    let settings = |mounted: &Mounted<'_>| {
                        let mount = mounted.mount;
                        (mount.read_only, mount.no_copy, mount.subpath.clone())
                    };
                    return Ok(if was.volume.driver != mounted.volume.driver {
                        Said::Asked(Shown::words("the data you approved, on another driver"))
                    } else if !current {
                        Said::Asked(self.stale(base, service, !mine.iter().all(agrees), held))
                    } else if settings(was) == settings(mounted)
                        && was.volume.labels == mounted.volume.labels
                    {
                        Said::Kept(base.signed_at)
                    } else {
                        Said::Told(Shown::words("the data you approved"))
                    });
                }
                Some(was) => return Ok(Said::Asked(self.different(was, mounted))),
                None if current && was.iter().any(|was| was.same_data(mounted)) => {
                    return Ok(Said::Told(Shown::words("the data you approved")));
                }
                None => {}
            }
        }
        Ok(Said::Asked(self.data(mounted, seen)))
    }

    /// `the data of your version signed 1 Oct 09:00 UTC`, and that a listed Container of
    /// `service` `disagrees`, or that `service` may not use it now.
    fn stale(
        &self,
        base: &Approved,
        service: &ServiceName,
        disagrees: bool,
        held: Option<&Held>,
    ) -> Shown {
        let then =
            Shown::words("the data of your version signed ").and(when(base.signed_at, self.now));
        match disagrees {
            true => then
                .and_words(", not what a container of ")
                .and_text(service.as_str())
                .and_words(" mounts"),
            false => then
                .and_words(", which ")
                .and_text(service.as_str())
                .and_words(" may not use now")
                .and(unseen(held)),
        }
    }

    /// `different data than the version you approved: was …, now …`. When the two read
    /// the same, each carries a prefix of its origin long enough to tell them apart.
    fn different(&self, was: &Mounted<'_>, now: &Mounted<'_>) -> Shown {
        let (mut then, mut later) = (self.named(was), self.named(now));
        if then == later {
            let origins = [was, now].map(|mounted| {
                mounted
                    .origin()
                    .map(|origin| origin.to_string())
                    .unwrap_or_default()
            });
            let [one, two] = &origins;
            let length = (6..64).find(|length| one.get(..*length) != two.get(..*length));
            let short = |origin: &str| {
                origin
                    .get(..length.unwrap_or(64))
                    .unwrap_or(origin)
                    .to_owned()
            };
            then = then.and_words(" #").and_text(&short(one));
            later = later.and_words(" #").and_text(&short(two));
        }
        Shown::words("different data than the version you approved: was ")
            .and(then)
            .and_words(", now ")
            .and(later)
    }

    /// `previews, 3 Nov 14:02 UTC`, or the name data from before births has on its Servers.
    fn named(&self, mounted: &Mounted<'_>) -> Shown {
        match mounted.volume.birth {
            Some(birth) => Shown::text(&birth.name)
                .and_words(", ")
                .and(when(birth.made_at, self.now)),
            None => Shown::text(mounted.physical.as_str()),
        }
    }

    /// `data made as previews, 3 Nov 14:02 UTC, 16 days ago`.
    fn made(&self, birth: &VolumeBirth) -> Shown {
        let made = Shown::words("data made as ")
            .and_text(&birth.name)
            .and_words(", ")
            .and(when(birth.made_at, self.now));
        let ago = self
            .now
            .as_millis()
            .checked_sub(birth.made_at.as_millis())
            .map(|millis| millis / 86_400_000);
        match ago {
            Some(0) => made.and_words(", today"),
            Some(1) => made.and_words(", 1 day ago"),
            Some(days) => made
                .and_words(", ")
                .and_text(&days.to_string())
                .and_words(" days ago"),
            None => made,
        }
    }
}

const LEGACY: &str = "data from before this version of Ployz recorded where data began";
const UNASKED: &str = "your servers weren't asked over a route that answers for itself";

/// `3 Nov 14:02 UTC`, with the year when it isn't `now`'s.
fn when(at: Timestamp, now: Timestamp) -> Shown {
    let mut shown = Shown::default();
    for (index, part) in at.day_and_time(now).split(' ').enumerate() {
        if index > 0 {
            shown = shown.and_words(" ");
        }
        shown = shown.and_text(part);
    }
    shown
}

/// `, that db uses now`, or `, that a container of db mounts` for one that doesn't run.
fn witness(holder: &HeldContainer) -> Shown {
    let service = holder.service.as_str();
    match holder.running {
        true => Shown::words(", that ")
            .and_text(service)
            .and_words(" uses now"),
        false => Shown::words(", that a container of ")
            .and_text(service)
            .and_words(" mounts"),
    }
}

/// What the Container list left out: the Servers that didn't answer, or that no route
/// that answers for itself listed them.
fn unseen(held: Option<&Held>) -> Shown {
    match held {
        None => Shown::words("; ").and_words(UNASKED),
        Some(held) => missing(&held.unanswered),
    }
}

/// `; no answer from web-2. Try again when it answers, or remove it with `ployz server rm``,
/// or nothing when every Server answered.
fn missing(unanswered: &[String]) -> Shown {
    match unanswered {
        [] => Shown::default(),
        names => Shown::words("; no answer from ")
            .and(Shown::list(names.iter().map(|name| Shown::text(name))))
            .and_words(". Try again when it answers, or remove it with `ployz server rm`"),
    }
}

/// One mount of a managed or Docker volume, with the name its Servers know the volume by.
struct Mounted<'a> {
    mount: &'a ServiceMount,
    volume: Lined<'a>,
    physical: DockerVolumeName,
}

impl Mounted<'_> {
    fn origin(&self) -> Option<VolumeOrigin> {
        VolumeOrigin::expected(self.volume.birth, &self.physical).ok()
    }

    /// Whether a Container's mount, `held`, holds this mount's data.
    fn is(&self, held: Option<&(DockerVolumeName, VolumeOrigin)>) -> bool {
        held.is_some_and(|(physical, origin)| {
            *physical == self.physical && self.origin().as_ref() == Some(origin)
        })
    }

    fn same_data(&self, other: &Mounted<'_>) -> bool {
        match (self.volume.birth, other.volume.birth) {
            (Some(one), Some(two)) => one == two,
            (None, None) => self.physical == other.physical,
            _ => false,
        }
    }
}

/// Each mount of `spec` a managed or Docker volume backs, in order.
fn mounts_of<'a>(namespace: &Namespace, spec: &'a RequestedServiceSpec) -> Vec<Mounted<'a>> {
    let graph = spec.volume_graph();
    graph
        .mounts()
        .iter()
        .filter_map(|mount| {
            let volume = Lined::of(graph.volume_for(mount).source.kind())?;
            Some(Mounted {
                mount,
                physical: namespace.volume_name(volume.name),
                volume,
            })
        })
        .collect()
}

/// Whether `spec` mounts data at `target`.
fn holds_at(spec: &RequestedServiceSpec, target: &str) -> bool {
    let graph = spec.volume_graph();
    (graph.mounts().iter()).any(|mount| {
        mount.target.as_str() == target && holds_data(graph.volume_for(mount).source.kind())
    })
}

/// `mounts external Docker volume customer-db at /archive, read-write: data from outside
/// Ployz`, or the same for a host path, when `mount` uses data Ployz didn't make.
fn outside(mount: &ServiceMount, source: &RawVolumeSource) -> Option<Shown> {
    let line = match source {
        RawVolumeSource::External { name } => {
            Shown::words("mounts external Docker volume ").and_text(name.as_str())
        }
        RawVolumeSource::Bind { machine_path, .. } => {
            Shown::words("mounts host path ").and_text(machine_path.as_str())
        }
        RawVolumeSource::Ordinary { .. }
        | RawVolumeSource::Provisioned { .. }
        | RawVolumeSource::Tmpfs { .. } => return None,
    };
    let mut line = line.and_words(" at ").and_text(mount.target.as_str());
    if let Some(subpath) = &mount.subpath {
        line = line.and_words(", subpath ").and_text(subpath);
    }
    Some(line.and_words(match mount.read_only {
        true => ", read-only: data from outside Ployz",
        false => ", read-write: data from outside Ployz",
    }))
}

/// Whether a mount of `source` holds data a new volume in its place hides: any but a tmpfs.
fn holds_data(source: &RawVolumeSource) -> bool {
    !matches!(source, RawVolumeSource::Tmpfs { .. })
}

/// `shown`, then the name Ployz Cloud gives the Volume, when it gives one.
fn calls(shown: Shown, called: Option<&str>) -> Shown {
    match called {
        Some(called) => shown
            .and_words(" (Ployz Cloud calls it ")
            .and_text(called)
            .and_words(")"),
        None => shown,
    }
}

/// A managed or Docker volume a mount's line names, with the settings it signs.
struct Lined<'a> {
    name: &'a DockerVolumeName,
    birth: Option<&'a VolumeBirth>,
    labels: &'a BTreeMap<String, String>,
    /// A Docker volume's driver; `None` for a managed volume.
    driver: Option<&'a VolumeDriver>,
}

impl<'a> Lined<'a> {
    fn of(source: &'a RawVolumeSource) -> Option<Self> {
        match source {
            RawVolumeSource::Ordinary {
                name,
                driver,
                labels,
                birth,
            } => Some(Self {
                name,
                birth: birth.as_ref(),
                labels,
                driver: Some(driver),
            }),
            RawVolumeSource::Provisioned {
                name,
                labels,
                birth,
                ..
            } => Some(Self {
                name,
                birth: birth.as_ref(),
                labels,
                driver: None,
            }),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Bind { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "birth_tests.rs"]
mod tests;
