//! The pinned BuildKit builder and the dedicated cache volume it leaves behind.
//! A daemon keeps the builder running after an attempt that confirmed a clean
//! end, and the next attempt in its slot starts in it.
//!
//! A Machine running several Builds at once gives each its own build slot:
//! a lock, builder container, cache volume, and upload staging directory.
//! Slot 0 keeps the historical names, so local CLI Builds share its cache.

use std::{
    fs,
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::PathBuf,
};

use crate::{Access, BUILDKIT_IMAGE, BuildError, Docker, Streams};

/// Longest a command waits for another local build to release the builder.
const QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Exclusive use of the Ployz builder and its retained cache for one attempt.
pub(crate) struct Builder<'a> {
    docker: &'a Docker<'a>,
    name: String,
    access: Access,
    lock: Option<Lock>,
    // Released only after Drop removes the container that uses the bridge.
    bridge: Option<Bridge>,
    fence: Option<(crate::FenceInstaller, Vec<u16>)>,
    reuse: Reuse,
    spec: String,
    /// The ID of the worker this Build runs on, from the inspect that showed
    /// it is Ployz's. Only that container is ever kept.
    worker: std::sync::OnceLock<String>,
}

/// Whether an attempt may start in the builder its slot's last attempt kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reuse {
    Never,
    WhenClean,
}

/// The worker a clean attempt left running.
#[derive(Debug, PartialEq, Eq)]
struct Kept {
    builder: String,
    container: String,
    spec: String,
}

/// A bridge this process still holds for a slot's worker after its attempt:
/// one it kept, or one whose removal Docker never confirmed.
struct Parked {
    kept: Option<Kept>,
    bridge: Bridge,
}

/// This process's parked bridges, by slot. Docker, not this, says which
/// workers exist: a claim is a lock on an open file, so it lasts only as long
/// as this process, and another process never sees it.
static PARKED: std::sync::Mutex<Vec<(PathBuf, Parked)>> = std::sync::Mutex::new(Vec::new());

impl<'a> Builder<'a> {
    /// Provision a container for this attempt, keeping the retained cache.
    ///
    /// An attempt that never confirmed its end may have left work running in
    /// its container, so it leaves its mark set and recovery removes the
    /// container. Only a container kept after a confirmed clean end, still
    /// running as recorded with the same configuration, is started in again.
    ///
    /// # Errors
    /// Fails when Buildx is unavailable or the container cannot be created.
    pub(crate) fn acquire(
        docker: &'a Docker<'a>,
        lock: Lock,
        access: Access,
        resources: &crate::policy::Resources,
        fence: Option<(crate::FenceInstaller, Vec<u16>)>,
        reuse: Reuse,
    ) -> Result<Self, BuildError> {
        let name = access.builder(lock.slot);
        let mut builder = Self {
            docker,
            name,
            access,
            lock: Some(lock),
            bridge: None,
            fence,
            reuse,
            spec: String::new(),
            worker: std::sync::OnceLock::new(),
        };
        // Mark before any Docker mutation so a killed daemon cannot silently
        // reuse state whose termination was never observed.
        builder.lock.as_mut().expect("owned lock").quarantine()?;
        if let Err(error) = builder.provision(resources) {
            return builder.finish(Err(error));
        }
        Ok(builder)
    }

    fn provision(&mut self, resources: &crate::policy::Resources) -> Result<(), BuildError> {
        let mut parked = self.lock.as_ref().expect("owned lock").take_parked();
        if let Some(Parked {
            kept: Some(kept),
            bridge,
        }) = parked.take_if(|parked| {
            parked
                .kept
                .as_ref()
                .is_some_and(|kept| self.reusable(kept, parked.bridge.index, resources))
        }) {
            let index = bridge.index;
            self.bridge = Some(bridge);
            self.spec = kept.spec;
            self.worker = kept.container.into();
            return create(self.docker, &self.name, index, resources);
        }
        // A worker this process did not keep for this attempt, such as one a
        // restarted daemon finds, may run another policy, so it is never
        // adopted. A parked bridge stays claimed until its worker is gone.
        if let Err(error) =
            refuse_foreign(self.docker, &self.name).and_then(|()| discard(self.docker, self.slot()))
        {
            if let Some(parked) = parked {
                self.lock.as_ref().expect("owned lock").park(Parked {
                    kept: None,
                    bridge: parked.bridge,
                });
            }
            return Err(error);
        }
        drop(parked);
        remove(&self.docker.releasing(), &self.name)?;
        label_cache(self.docker, &self.name)?;
        let bridge = Bridge::claim(&bridge_directory(), NETWORK_INTERFACES)?;
        let index = bridge.index;
        self.bridge = Some(bridge);
        self.spec = spec(resources, index);
        create(self.docker, &self.name, index, resources)
    }

    /// Whether the kept worker is still the one this process kept, running
    /// unchanged under this attempt's policy.
    fn reusable(&self, kept: &Kept, bridge: u8, resources: &crate::policy::Resources) -> bool {
        self.reuse == Reuse::WhenClean
            && kept.builder == self.name
            && kept.spec == spec(resources, bridge)
            && self.running().as_ref() == Some(&kept.container)
    }

    /// The running worker container's ID.
    fn running(&self) -> Option<String> {
        let state = self
            .docker
            .run(
                "inspect the BuildKit worker",
                &[
                    "inspect",
                    "--format",
                    "{{.Id}} {{.State.Running}}",
                    &container(&self.name),
                ],
                Streams::Captured,
            )
            .ok()?;
        state
            .trim()
            .strip_suffix(" true")
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    }

    /// Record the worker Buildx started for this Build, once one inspect
    /// shows it is Ployz's. A reused worker must be the one this process kept.
    fn take_worker(&self) -> Result<(), BuildError> {
        let container = container(&self.name);
        let Holder::Ployz(id) = holder(self.docker, &container)? else {
            return Err(BuildError::Prerequisite(format!(
                "{container} is not the BuildKit worker Ployz created for this Build"
            )));
        };
        if *self.worker.get_or_init(|| id.clone()) != id {
            return Err(BuildError::Prerequisite(format!(
                "{container} was replaced while this Build used it"
            )));
        }
        Ok(())
    }

    /// Leave the worker running for the slot's next attempt, only if it is
    /// still the container this Build ran on. Its bridge stays claimed by
    /// this process, so no other builder takes it meanwhile.
    fn keep(&mut self) -> Result<(), BuildError> {
        let container = self
            .running()
            .filter(|running| self.worker.get() == Some(running))
            .ok_or_else(|| {
                BuildError::Prerequisite(
                    "the BuildKit worker stopped or was replaced before it could be kept".into(),
                )
            })?;
        let bridge = self
            .bridge
            .take()
            .expect("a provisioned builder holds its bridge");
        self.lock.as_ref().expect("owned lock").park(Parked {
            kept: Some(Kept {
                builder: self.name.clone(),
                container,
                spec: self.spec.clone(),
            }),
            bridge,
        });
        Ok(())
    }

    /// Hold the bridge while Docker may still run the worker that uses it.
    fn hold_bridge(&mut self) {
        if let (Some(bridge), Some(lock)) = (self.bridge.take(), self.lock.as_ref()) {
            lock.park(Parked { kept: None, bridge });
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    fn slot(&self) -> usize {
        self.lock.as_ref().expect("owned lock").slot
    }

    /// Read the running worker, not the Machine's advertised architecture.
    ///
    /// # Errors
    /// Refuses unsupported image stores, hosts, resource limits, or requested worker platforms.
    pub(crate) fn native_platform(
        &self,
        targets: &[crate::Target],
        resources: &crate::policy::Resources,
    ) -> Result<String, BuildError> {
        let info = self.docker.run(
            "inspect the image store",
            &["info", "--format", "{{json .}}"],
            Streams::Captured,
        )?;
        let info: serde_json::Value = serde_json::from_str(&info).map_err(|_| {
            BuildError::Prerequisite("Docker reported invalid image-store capability".into())
        })?;
        resources.check_support(&info)?;
        if !info
            .get("DriverStatus")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row == &serde_json::json!(["driver-type", "io.containerd.snapshotter.v1"])
                })
            })
        {
            return Err(BuildError::Prerequisite(
                "Ployz Builds require Docker's containerd image store".into(),
            ));
        }
        let architecture = match info.get("Architecture").and_then(serde_json::Value::as_str) {
            Some("x86_64" | "amd64") => "amd64",
            Some("aarch64" | "arm64") => "arm64",
            _ => {
                return Err(BuildError::Prerequisite(
                    "the build host must support Linux AMD64 or ARM64".into(),
                ));
            }
        };
        if info.get("OSType").and_then(serde_json::Value::as_str) != Some("linux") {
            return Err(BuildError::Prerequisite(
                "the build host must run Linux containers".into(),
            ));
        }
        if info
            .get("CgroupVersion")
            .and_then(serde_json::Value::as_str)
            != Some("2")
        {
            return Err(BuildError::Prerequisite(
                "Ployz Builds require cgroup v2 on the build host".into(),
            ));
        }
        let native = format!("linux/{architecture}");
        self.docker.run(
            "start the BuildKit worker",
            &["buildx", "inspect", &self.name, "--bootstrap"],
            Streams::Captured,
        )?;
        self.take_worker()?;
        self.fence()?;
        let workers = self.docker.run(
            "inspect BuildKit platforms",
            &["buildx", "ls", "--format", "{{json .}}"],
            Streams::Captured,
        )?;
        let builder = workers
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|builder| {
                builder.get("Name").and_then(serde_json::Value::as_str) == Some(&self.name)
            });
        let nodes = builder
            .as_ref()
            .and_then(|builder| builder.get("Nodes"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                BuildError::Prerequisite(
                    "the selected BuildKit worker reported no capability".into(),
                )
            })?;
        for requested in targets.iter().flat_map(|target| {
            target
                .platforms
                .iter()
                .map(String::as_str)
                .chain(target.is_native().then_some(native.as_str()))
        }) {
            if !nodes.iter().any(|node| {
                node.get("Status").and_then(serde_json::Value::as_str) == Some("running")
                    && node
                        .get("Platforms")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|platforms| {
                            platforms
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .any(|platform| crate::covers(platform, requested))
                        })
            }) {
                return Err(BuildError::Prerequisite(format!(
                    "the running BuildKit worker cannot build {requested}"
                )));
            }
        }
        Ok(native)
    }

    pub(crate) fn run(
        &self,
        arguments: &[String],
        sealed: bool,
        started: impl FnOnce(),
    ) -> Result<(), BuildError> {
        // A restarted worker is a new cgroup the old fence no longer matches.
        self.fence()?;
        let mut borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let Some(progress) = self.docker.progress else {
            return self
                .docker
                .run_started("the build", &borrowed, Streams::Inherited, started)
                .map(|_| ());
        };
        // Structured progress: every consumer sees BuildKit's own step tree.
        // The flag and its parser live together so they cannot drift apart;
        // Buildx accepts flags after the targets.
        borrowed.push("--progress=rawjson");
        let parser = std::sync::Mutex::new(crate::solve::SolveParser::new(sealed));
        let structured = |event| {
            if let crate::Progress::Output(bytes) = event {
                parser
                    .lock()
                    .expect("parsing never panics while holding the parser")
                    .feed(&bytes, progress);
            } else {
                progress(event);
            }
        };
        let result = Docker {
            progress: Some(&structured),
            ..*self.docker
        }
        .run_started("the build", &borrowed, Streams::Inherited, started);
        let mut parser = parser
            .lock()
            .expect("parsing never panics while holding the parser");
        parser.finish(progress);
        match result {
            Ok(_) => Ok(()),
            Err(error @ BuildError::Docker { .. }) => Err(parser.failure().unwrap_or(error)),
            Err(error) => Err(error),
        }
    }

    /// Fence the running worker's own traffic, or refuse to solve.
    fn fence(&self) -> Result<(), BuildError> {
        let Some((install, image_ports)) = &self.fence else {
            return Ok(());
        };
        let refuse = |reason: String| {
            BuildError::Prerequisite(format!("fence the BuildKit worker: {reason}"))
        };
        let container = container(&self.name);
        let state = self.docker.run(
            "inspect the BuildKit worker",
            &["inspect", "--format", "{{.Id}} {{.State.Pid}}", &container],
            Streams::Captured,
        )?;
        let (id, pid) = state
            .trim()
            .split_once(' ')
            .filter(|(_, pid)| pid.parse::<u32>().is_ok_and(|pid| pid > 0))
            .ok_or_else(|| refuse(format!("{container} is not running")))?;
        if self.worker.get().is_some_and(|worker| worker != id) {
            return Err(refuse(format!(
                "{container} is not the worker this Build ran on"
            )));
        }
        let cgroups = fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .map_err(|error| refuse(format!("read its cgroup: {error}")))?;
        let cgroup = crate::fence::container_cgroup(&cgroups, id)
            .ok_or_else(|| refuse(format!("no cgroup names container {id}")))?;
        install(&crate::Fence {
            slot: self.slot(),
            cgroup,
            image_ports: image_ports.clone(),
            mesh_images: self.access == Access::Full,
        })
        .map_err(refuse)
    }

    /// Confirm teardown before releasing retained state; failure preserves its
    /// quarantine across daemon restart as well as competing direct calls.
    pub(crate) fn finish<T>(mut self, result: Result<T, BuildError>) -> Result<T, BuildError> {
        // Read once, so a cancellation arriving now cannot keep the builder
        // and still report the attempt cancelled.
        let cancelled = self
            .docker
            .cancellation
            .is_some_and(crate::Cancellation::is_cancelled);
        let cleanup = if self.reuse == Reuse::WhenClean && result.is_ok() && !cancelled {
            self.keep()
                .or_else(|_| release(&self.docker.releasing(), &self.name))
        } else {
            release(&self.docker.releasing(), &self.name)
        };
        if cleanup.is_err() {
            self.hold_bridge();
        }
        let stage = result
            .as_ref()
            .err()
            .map_or(crate::Stage::Cleanup, BuildError::stage);
        // A slot left marked, by this release or by any teardown earlier in
        // the Build such as a replaced worker's, is only Docker's to hold now
        // the Build returned: the next admission retries, in this process too.
        let lock = self.lock.as_mut().expect("owned lock");
        let cleared = if cleanup.is_ok() && !result.as_ref().is_err_and(|error| error.is_unknown())
        {
            lock.clear()
        } else {
            lock.disown()
        };
        self.lock.take();
        if let Err(error) = cleared {
            return Err(match result {
                Ok(_) => error.at(stage),
                Err(cause) => {
                    BuildError::Result(format!("{cause}; cleanup failed: {error}")).at(stage)
                }
            });
        }
        match cleanup {
            Ok(()) => result.and_then(|output| {
                if cancelled {
                    Err(BuildError::Cancelled)
                } else {
                    Ok(output)
                }
            }),
            Err(error) => Err(BuildError::UncertainTermination(match result {
                Ok(_) => format!("output handling completed; cleanup failed: {error}"),
                Err(cause) => format!("{cause}; cleanup failed: {error}"),
            })
            .at(stage)),
        }
    }
}

impl Drop for Builder<'_> {
    fn drop(&mut self) {
        // Panic/unwind still attempts cleanup. The marker was persisted before
        // execution, so a failed cleanup cannot unlock unsafe retained state.
        if self.lock.is_none() {
            return;
        }
        if release(&self.docker.releasing(), &self.name).is_ok() {
            let _ = self.lock.as_mut().expect("owned lock").clear();
        } else {
            self.hold_bridge();
            let _ = self.lock.as_mut().expect("owned lock").disown();
        }
    }
}

fn create(
    docker: &Docker<'_>,
    name: &str,
    bridge: u8,
    resources: &crate::policy::Resources,
) -> Result<(), BuildError> {
    // Always supply a config, so ambient Buildx configuration cannot replace
    // execution-host policy.
    let config = docker.working_dir.join(format!("{name}.buildkitd.toml"));
    fs::write(&config, resources.buildkit_config(bridge)).map_err(|error| {
        BuildError::Prerequisite(format!("write BuildKit host configuration: {error}"))
    })?;
    let mut arguments = vec![
        "buildx".into(),
        "create".into(),
        "--name".into(),
        name.into(),
        "--driver".into(),
        "docker-container".into(),
        "--driver-opt".into(),
        format!("image={BUILDKIT_IMAGE}"),
        // BuildKit itself stays on the host to pull mesh image contexts and push
        // to the local registry; build steps run in the worker's bridge network.
        "--driver-opt".into(),
        "network=host".into(),
        // A restarted worker sits in a new cgroup the fence no longer matches.
        // A worker that dies stays dead, so its Build fails instead.
        "--driver-opt".into(),
        "restart-policy=no".into(),
        "--driver-opt".into(),
        format!("env.{OWNER}={}", rustix::process::getuid().as_raw()),
        "--buildkitd-config".into(),
        config.to_string_lossy().into_owned(),
    ];
    arguments.extend(resources.worker_arguments());
    docker
        .run(
            "create the build container",
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
            Streams::Captured,
        )
        .map(|_| ())
}

fn spec(resources: &crate::policy::Resources, bridge: u8) -> String {
    format!(
        "{BUILDKIT_IMAGE}\n{}\n{}",
        resources.worker_arguments().join(" "),
        resources.buildkit_config(bridge)
    )
}

/// Marks a worker, by its environment, and a cache volume, by its label, as
/// this host user's Ployz builder: Buildx puts no labels on a worker it
/// creates, and a name alone could be anyone's container.
pub(crate) const OWNER: &str = "PLOYZ_BUILD_OWNER";
const OWNER_LABEL: &str = "ployz.build.owner";

pub(crate) fn owner() -> String {
    rustix::process::getuid().as_raw().to_string()
}

/// Every container name a build slot's `access` builder uses: its worker,
/// then its helpers.
pub(crate) fn slot_names(access: Access, slot: usize) -> Vec<String> {
    let builder = access.builder(slot);
    std::iter::once(container(&builder))
        .chain(HELPERS.iter().map(|helper| format!("{builder}-{helper}")))
        .collect()
}

/// The exact image references Ployz runs each of its containers from: its
/// BuildKit worker, then each helper under its own name. When a pin changes,
/// the earlier reference stays listed, so containers it made stay Ployz's.
const WORKER_IMAGES: [&str; 2] = [crate::BUILDKIT_IMAGE.as_str(), "moby/buildkit:v0.26.2"];
const HELPER_IMAGES: [(&str, &[&str]); 3] = [
    ("railpack", &[crate::railpack::IMAGE]),
    ("prepare", &[crate::railpack::PREPARATION_IMAGE]),
    ("assemble", &[crate::index::REGCTL]),
];
const WORKER_PREFIX: &str = "buildx_buildkit_";
/// Buildx sets this on every worker it starts.
const BUILDX_WORKER_VARIABLE: &str = "BUILDKIT_SETUP_CGROUPV2_ROOT";

/// How Docker reports a container was made.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct Made {
    #[serde(rename = "Image", default)]
    image: Option<String>,
    #[serde(rename = "Env", default)]
    environment: Option<Vec<String>>,
}

/// An environment variable's name.
fn key(variable: &str) -> &str {
    variable.split_once('=').map_or(variable, |(key, _)| key)
}

/// Whether host user `uid`'s Ployz made the container named `name`, as `made`
/// reports. Its image is exactly one Ployz pins for that name, and it carries
/// one owner marker, naming `uid`. A container with no owner marker at all is
/// an earlier Ployz's, which marked none: a helper, or a worker Buildx started.
fn made_by_ployz(name: &str, made: &Made, uid: &str) -> bool {
    let pinned: &[&str] = if name.starts_with(WORKER_PREFIX) {
        &WORKER_IMAGES
    } else {
        HELPER_IMAGES
            .iter()
            .find(|(helper, _)| name.ends_with(&format!("-{helper}")))
            .map_or(&[], |(_, images)| images)
    };
    if !made
        .image
        .as_deref()
        .is_some_and(|image| pinned.contains(&image))
    {
        return false;
    }
    let environment = made.environment.as_deref().unwrap_or_default();
    let mut owners = environment.iter().filter(|variable| key(variable) == OWNER);
    match (owners.next(), owners.next()) {
        (Some(marker), None) => *marker == format!("{OWNER}={uid}"),
        (Some(_), Some(_)) => false,
        (None, _) => {
            !name.starts_with(WORKER_PREFIX)
                || environment
                    .iter()
                    .any(|variable| key(variable) == BUILDX_WORKER_VARIABLE)
        }
    }
}

/// Who holds a slot's container name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Holder {
    Nobody,
    /// This host user's Ployz made it: the container's ID, from the same
    /// inspect that showed so. Ployz acts on that ID, never on the name, which
    /// another container can take meanwhile.
    Ployz(String),
    /// A container this host user's Ployz did not make: never adopted or removed.
    Other,
}

/// The ID of the container named `container`, when it exists and this host
/// user's Ployz made it.
fn owned(docker: &Docker<'_>, container: &str) -> Result<Option<String>, BuildError> {
    Ok(match holder(docker, container)? {
        Holder::Ployz(id) => Some(id),
        Holder::Nobody | Holder::Other => None,
    })
}

pub(crate) fn holder(docker: &Docker<'_>, container: &str) -> Result<Holder, BuildError> {
    match docker.run(
        "inspect a build container",
        &[
            "inspect",
            "--type",
            "container",
            "--format",
            "{{json .}}",
            container,
        ],
        Streams::Captured,
    ) {
        // Docker prints an existing container, so nothing names none.
        Ok(inspected) if inspected.trim().is_empty() => Ok(Holder::Nobody),
        Ok(inspected) => {
            #[derive(serde::Deserialize)]
            struct Inspected {
                #[serde(rename = "Id", default)]
                id: String,
                #[serde(rename = "Config", default)]
                made: Made,
            }
            let unreadable = |reason: String| {
                BuildError::Prerequisite(format!(
                    "Docker reported an unreadable container {container}: {reason}"
                ))
            };
            let inspected: Inspected = serde_json::from_str(inspected.trim())
                .map_err(|error| unreadable(error.to_string()))?;
            if inspected.id.is_empty() {
                return Err(unreadable("it has no ID".into()));
            }
            Ok(if made_by_ployz(container, &inspected.made, &owner()) {
                Holder::Ployz(inspected.id)
            } else {
                Holder::Other
            })
        }
        Err(BuildError::Docker { diagnostic, .. })
            if diagnostic.to_ascii_lowercase().contains("no such") =>
        {
            Ok(Holder::Nobody)
        }
        Err(error) => Err(error),
    }
}

/// Create the slot's cache volume with this user's label before Buildx would
/// create it without one. An existing volume keeps what it has.
fn label_cache(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    docker
        .run(
            "create the build cache",
            &[
                "volume",
                "create",
                "--label",
                &format!("{OWNER_LABEL}={}", owner()),
                &format!("{}_state", container(name)),
            ],
            Streams::Captured,
        )
        .map(drop)
}

/// Remove a slot's workers that this host user's Ployz made, whichever
/// access each ran with. Another container with a worker's name is left.
fn discard(docker: &Docker<'_>, slot: usize) -> Result<(), BuildError> {
    [Access::Full, Access::Free].iter().try_for_each(|access| {
        match owned(docker, &container(&access.builder(slot)))? {
            Some(id) => tear_down(docker, &id),
            None => Ok(()),
        }
    })
}

/// How long BuildKit gets to shut down, deleting its bridge, before Docker
/// kills it. Within the cleanup budget.
const WORKER_STOP_SECONDS: &str = "20";

/// Tear down the worker with ID `id`: stop it gracefully, so BuildKit deletes
/// its bridge, then remove it. A worker that fails to stop and still runs is
/// left running, since a forced removal would leave its bridge behind; the
/// error keeps the slot marked for a later recovery to retry.
fn tear_down(docker: &Docker<'_>, id: &str) -> Result<(), BuildError> {
    let releasing = docker.releasing();
    if let Err(error) = releasing.run(
        "stop the build container",
        &["stop", "--time", WORKER_STOP_SECONDS, id],
        Streams::Captured,
    ) {
        match releasing.run(
            "inspect the build container",
            &[
                "inspect",
                "--type",
                "container",
                "--format",
                "{{.State.Running}}",
                id,
            ],
            Streams::Captured,
        ) {
            Ok(running) if running.trim() == "false" => {}
            Err(BuildError::Docker { diagnostic, .. })
                if diagnostic.to_ascii_lowercase().contains("no such") =>
            {
                return Ok(());
            }
            _ => {
                return Err(BuildError::UncertainTermination(format!(
                    "the BuildKit worker {id} did not stop, so it is left running: {error}"
                )));
            }
        }
    }
    docker.remove_container(id)
}

/// Refuse a builder whose worker name another container holds: Buildx would
/// start in that container as if it were the worker.
fn refuse_foreign(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    let container = container(name);
    if holder(docker, &container)? == Holder::Other {
        return Err(foreign(&container));
    }
    Ok(())
}

pub(crate) fn foreign(container: &str) -> BuildError {
    foreign_all(&[container.to_owned()])
}

pub(crate) fn foreign_all(containers: &[String]) -> BuildError {
    BuildError::Prerequisite(match containers {
        [container] => format!(
            "a container Ployz did not create is named {container}, a name a Ployz build slot needs; remove or rename it"
        ),
        containers => format!(
            "containers Ployz did not create are named {}, names Ployz build slots need; remove or rename them",
            containers.join(", ")
        ),
    })
}

/// Remove this attempt's builder, including a container it started in
/// without registering, so an unmarked slot keeps none but a recorded one.
/// A container Ployz did not create is not this attempt's, so it stays.
fn release(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    let held = holder(docker, &container(name))?;
    if held == Holder::Other {
        return Ok(());
    }
    remove(docker, name)?;
    match held {
        Holder::Ployz(id) => tear_down(docker, &id),
        Holder::Nobody | Holder::Other => Ok(()),
    }
}

pub(crate) fn container(builder: &str) -> String {
    format!("{WORKER_PREFIX}{builder}0")
}

/// Forget the builder in Buildx without touching any container: Buildx would
/// remove whichever container holds the worker's name by then.
fn remove(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    match docker.run(
        "remove the build container",
        &["buildx", "rm", "--keep-state", "--keep-daemon", name],
        Streams::Captured,
    ) {
        Ok(_) => Ok(()),
        // Already gone is the state this asks for.
        Err(BuildError::Docker { diagnostic, .. }) if diagnostic.contains("no builder") => Ok(()),
        Err(error) => Err(error),
    }
}

/// Serializes local attempts sharing one build slot's builder and retained cache.
///
/// Held for the whole attempt. Uncertain termination quarantines the slot
/// until its mark's processes are observed gone and its builder is removed.
#[derive(Clone)]
pub(crate) struct Lock {
    file: std::sync::Arc<LockedFile>,
    pub(crate) directory: PathBuf,
    slot: usize,
}

struct LockedFile {
    file: fs::File,
    // The slot's claim in shared memory, which every mount namespace sees.
    claim: fs::File,
    // Serializes writes to the mark between threads of one attempt.
    writes: std::sync::Mutex<()>,
}

/// Records each Docker CLI an attempt spawns in its mark, while it still
/// holds the slot.
#[derive(Clone)]
pub(crate) struct Recorder(std::sync::Weak<LockedFile>);

/// Waits on its stdin for `start` before it runs `program`, so a Docker CLI
/// never runs unless the mark names it first. A parent killed before then
/// closes the pipe, and the child exits without running anything.
const GATE: &str = r#"IFS= read -r go && [ "$go" = start ] && exec "$0" "$@" </dev/null; exit 125"#;

pub(crate) fn gated(program: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", GATE])
        .arg(program)
        .stdin(std::process::Stdio::piped());
    command
}

impl Recorder {
    /// Name a [`gated`] child in the mark, with the Docker endpoint it uses,
    /// then let it run. On failure the child is released without running.
    pub(crate) fn record(
        &self,
        child: &mut std::process::Child,
        endpoint: &str,
    ) -> Result<(), BuildError> {
        use std::io::Write as _;
        let mut gate = child.stdin.take().expect("a gated child has a piped stdin");
        self.append(child.id(), endpoint)?;
        gate.write_all(b"start\n").map_err(lock_error)
    }

    fn append(&self, pid: u32, endpoint: &str) -> Result<(), BuildError> {
        use std::os::unix::fs::FileExt as _;
        let Some(locked) = self.0.upgrade() else {
            return Ok(());
        };
        let _writes = locked.writes.lock().expect("mark writes never panic");
        let length = locked.file.metadata().map_err(lock_error)?.len();
        // An unmarked slot has nothing running that a later attempt must wait for.
        if length == 0 {
            return Ok(());
        }
        let Some(process) = crate::owner::Process::observe(pid).map_err(lock_error)? else {
            return Ok(());
        };
        locked
            .file
            .write_all_at(
                crate::owner::Mark::docker_line(process, endpoint).as_bytes(),
                length,
            )
            .map_err(lock_error)
    }
}

impl Lock {
    /// Wait for exclusive use of this user's builder.
    ///
    /// # Errors
    /// Fails when the lock file cannot be opened or locked, or stays quarantined.
    pub(crate) fn acquire(
        docker: &std::path::Path,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, BuildError> {
        Self::acquire_in(&directory(), docker, cancellation)
    }

    fn acquire_in(
        directory: &std::path::Path,
        docker: &std::path::Path,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, BuildError> {
        let deadline = std::time::Instant::now() + QUEUE_TIMEOUT;
        loop {
            if cancellation.is_cancelled() {
                return Err(BuildError::Cancelled);
            }
            match Self::try_acquire_slot(directory, 0, docker) {
                Err(BuildError::Busy) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                result => return result,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn try_acquire_in(directory: &std::path::Path) -> Result<Self, BuildError> {
        Self::try_acquire_slot(directory, 0, std::path::Path::new("docker"))
    }

    /// Take a free slot, first recovering it if an earlier attempt left a mark.
    pub(crate) fn try_acquire_slot(
        directory: &std::path::Path,
        slot: usize,
        docker: &std::path::Path,
    ) -> Result<Self, BuildError> {
        let mut lock = Self::open_locked(directory, slot)?;
        if lock.marked()? {
            lock.recover(docker, false)?;
        }
        Ok(lock)
    }

    /// Take `slot` as [`Self::try_acquire_slot`] does, from the process whose own Build
    /// left its mark. Holding the slot shows that Build returned, so this process, its
    /// owner, no longer keeps it quarantined; the Build's Docker CLIs and builder still do.
    pub(crate) fn try_acquire_own_slot(
        directory: &std::path::Path,
        slot: usize,
        docker: &std::path::Path,
    ) -> Result<Self, BuildError> {
        let mut lock = Self::open_locked(directory, slot)?;
        if lock.marked()? {
            lock.recover(docker, true)?;
        }
        Ok(lock)
    }

    /// Slots that have ever been used in `directory`. Slot 0 always exists.
    pub(crate) fn known_slots(directory: &std::path::Path) -> Vec<usize> {
        (0..=usize::from(u8::MAX))
            .filter(|&slot| slot == 0 || directory.join(lock_file(slot)).exists())
            .collect()
    }

    /// Slots with a lock in the policy's directory or a worker in Docker, and
    /// with `caches`, a cache volume too. Slot 0 always exists. Lock files
    /// can vanish in a restart or sit in another process's private /tmp, but
    /// Docker sees every slot's workers and caches.
    pub(crate) fn slots(
        policy: &crate::HostPolicy,
        environment: &std::collections::BTreeMap<String, String>,
        caches: bool,
    ) -> (Vec<usize>, Result<(), BuildError>) {
        // Command output lands in a directory of its own, private to this listing.
        let working = policy
            .state_directory
            .join(format!("slots-{}", uuid::Uuid::new_v4()));
        if let Err(error) = fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&working)
        {
            return (
                Self::known_slots(&policy.state_directory),
                Err(lock_error(error)),
            );
        }
        let docker = Docker {
            program: &policy.docker,
            environment,
            working_dir: &working,
            deadline: crate::Deadline::starting_now(crate::CLEANUP_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let names = |listed: &str| {
            listed
                .lines()
                .map(|name| name.trim().to_owned())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let named = |slot: usize, suffix: &str| {
            [Access::Full, Access::Free]
                .map(|access| format!("{}{suffix}", container(&access.builder(slot))))
        };
        let mut found = Self::known_slots(&policy.state_directory);
        // A cache clear runs in the caller's environment, and another
        // Docker's containers are not this host's workers.
        if let Err(error) = if caches {
            docker.require_local()
        } else {
            Ok(())
        } {
            let _ = fs::remove_dir_all(&working);
            return (found, Err(error));
        }
        // Act on whichever listing Docker answered, and report the other.
        let workers = docker
            .run(
                "list build containers",
                &["ps", "--all", "--format", "{{.Names}}"],
                Streams::Captured,
            )
            .and_then(|listed| {
                let listed = names(&listed);
                for slot in 0..=usize::from(u8::MAX) {
                    // A cache clear also names a container another made under
                    // any of a slot's names, so it visits that slot too.
                    if caches
                        && [Access::Full, Access::Free]
                            .into_iter()
                            .flat_map(|access| slot_names(access, slot))
                            .any(|name| listed.contains(&name))
                    {
                        found.push(slot);
                        continue;
                    }
                    for worker in named(slot, "") {
                        if listed.contains(&worker) && owned(&docker, &worker)?.is_some() {
                            found.push(slot);
                        }
                    }
                }
                Ok(())
            });
        let caches = if caches {
            docker
                .run(
                    "list build caches",
                    &[
                        "volume",
                        "ls",
                        "--filter",
                        &format!("label={OWNER_LABEL}={}", owner()),
                        "--format",
                        "{{.Name}}",
                    ],
                    Streams::Captured,
                )
                .map(|listed| {
                    let listed = names(&listed);
                    found.extend((0..=usize::from(u8::MAX)).filter(|&slot| {
                        named(slot, "_state")
                            .iter()
                            .any(|cache| listed.contains(cache))
                    }));
                })
        } else {
            Ok(())
        };
        let _ = fs::remove_dir_all(&working);
        found.sort_unstable();
        found.dedup();
        (found, workers.and(caches))
    }

    pub(crate) fn name(&self) -> String {
        slot_name(self.slot)
    }

    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    pub(crate) fn recorder(&self) -> Recorder {
        Recorder(std::sync::Arc::downgrade(&self.file))
    }

    /// Private staging for this slot's uploads.
    pub(crate) fn upload_directory(&self) -> PathBuf {
        self.directory.join(match self.slot {
            0 => "build-upload".to_owned(),
            slot => format!("build-upload-{slot}"),
        })
    }

    fn parking(&self) -> PathBuf {
        self.directory.join(lock_file(self.slot))
    }

    /// Take the bridge this process parked for the slot, if any. Only the
    /// slot's holder may: until its worker is started in or removed, that
    /// worker is unaccounted for.
    fn take_parked(&self) -> Option<Parked> {
        let slot = self.parking();
        let mut parked = PARKED.lock().expect("parked bridges never panic");
        parked
            .iter()
            .position(|(parking, _)| *parking == slot)
            .map(|index| parked.swap_remove(index).1)
    }

    fn park(&self, parked: Parked) {
        PARKED
            .lock()
            .expect("parked bridges never panic")
            .push((self.parking(), parked));
    }

    /// A container Ployz did not create that holds one of this slot's names.
    /// A worker this process kept is its own, so it is not asked about.
    pub(crate) fn foreign_holder(&self, program: &std::path::Path) -> Option<String> {
        let slot = self.parking();
        if PARKED
            .lock()
            .expect("parked bridges never panic")
            .iter()
            .any(|(parking, parked)| *parking == slot && parked.kept.is_some())
        {
            return None;
        }
        let environment = crate::upload::environment(&self.upload_directory());
        let docker = Docker {
            program,
            environment: &environment,
            working_dir: &self.directory,
            deadline: crate::Deadline::starting_now(crate::CLEANUP_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        // This environment names no other Docker. Docker failing to answer is
        // reported by the attempt itself. One listing finds which names exist.
        let builders = [Access::Full, Access::Free].map(|access| access.builder(self.slot));
        let mut listing = vec!["ps", "--all", "--format", "{{.Names}}"];
        let filters = builders
            .iter()
            .map(|builder| format!("name={builder}"))
            .collect::<Vec<_>>();
        for filter in &filters {
            listing.extend(["--filter", filter]);
        }
        let listed = docker
            .run("list build containers", &listing, Streams::Captured)
            .ok()?;
        let listed = listed.lines().map(str::trim).collect::<Vec<_>>();
        [Access::Full, Access::Free]
            .into_iter()
            .flat_map(|access| slot_names(access, self.slot))
            .filter(|name| listed.contains(&name.as_str()))
            .find(|name| holder(&docker, name).is_ok_and(|holder| holder == Holder::Other))
    }

    fn marked(&self) -> Result<bool, BuildError> {
        Ok(self.file.file.metadata().map_err(lock_error)?.len() != 0)
    }

    /// Clear an earlier attempt's mark once nothing it started can still run.
    ///
    /// Holding the file lock proves no live attempt owns the slot. The mark's
    /// processes, matched by PID and start time on their boot, and any Buildx
    /// process naming the builder must be gone. Recovery then rewrites the mark
    /// to name this boot and the endpoints to clean, and no owner. Its own
    /// Docker CLIs are recorded there before they run. So a recovery killed
    /// mid-removal leaves those CLIs for the next one to wait on, and one that
    /// fails leaves nothing alive that blocks a retry. The mark clears once
    /// Docker confirms the builder and its helpers are gone on every endpoint.
    fn recover(&mut self, docker: &std::path::Path, owned_here: bool) -> Result<(), BuildError> {
        let path = self.directory.join(lock_file(self.slot));
        let name = self.name();
        // Either access's builder may have run in this slot last.
        let builders = [Access::Full, Access::Free].map(|access| access.builder(self.slot));
        let quarantined = |observed: String| {
            BuildError::UncertainTermination(format!(
                "builder {name} stays quarantined: the Build that last used it never confirmed its end, and {observed}. A later Build, daemon start or cache clear recovers it once that changes; its mark is {}",
                path.display()
            ))
        };
        let mark = crate::owner::Mark::parse(&fs::read_to_string(&path).map_err(lock_error)?);
        let mut survivors = Vec::new();
        for builder in &builders {
            for survivor in mark
                .survivors(builder)
                .map_err(|error| quarantined(format!("its processes could not be read: {error}")))?
            {
                if !survivors.contains(&survivor) {
                    survivors.push(survivor);
                }
            }
        }
        if owned_here {
            let here = crate::owner::Process::current().map_err(|error| {
                quarantined(format!("its processes could not be read: {error}"))
            })?;
            survivors.retain(|survivor| {
                *survivor != crate::owner::Survivor::Recorded(crate::owner::Role::Owner, here)
            });
        }
        if !survivors.is_empty() {
            let unnamed = if mark == crate::owner::Mark::Unnamed {
                " (its mark predates recorded processes)"
            } else {
                ""
            };
            return Err(quarantined(format!(
                "still running{unnamed}: {}",
                survivors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let endpoints = mark.endpoints();
        // Builds refuse any other Docker, so a mark naming one was not written by them.
        if let Some(endpoint) = endpoints
            .iter()
            .find(|endpoint| !endpoint.starts_with("unix://"))
        {
            return Err(quarantined(format!(
                "none of its processes is running, but it used Docker at {endpoint}, which is not this Machine's"
            )));
        }
        self.take_over(&endpoints)?;
        let recorder = self.recorder();
        for endpoint in endpoints {
            let mut environment = crate::upload::environment(&self.upload_directory());
            environment.insert("DOCKER_HOST".into(), endpoint.clone());
            let docker = Docker {
                program: docker,
                environment: &environment,
                working_dir: &self.directory,
                deadline: crate::Deadline::starting_now(crate::CLEANUP_TIMEOUT),
                cancellation: None,
                progress: None,
                recorder: Some(&recorder),
            };
            // Ask Docker itself, not a Buildx configuration the attempt may not have shared.
            builders
                .iter()
                .try_for_each(|name| {
                    std::iter::once(container(name))
                        .chain(HELPERS.iter().map(|helper| format!("{name}-{helper}")))
                        .try_for_each(|held| match holder(&docker, &held)? {
                            // Only a worker has a bridge to delete.
                            Holder::Ployz(id) if held.starts_with(WORKER_PREFIX) => {
                                tear_down(&docker, &id)
                            }
                            Holder::Ployz(id) => docker.remove_container(&id),
                            Holder::Nobody | Holder::Other => Ok(()),
                        })
                })
                .map_err(|error| {
                    quarantined(format!(
                        "none of its processes is running, but removing its builder from {endpoint} failed: {error}"
                    ))
                })?;
        }
        drop(self.take_parked());
        self.clear()
    }

    /// One bounded recovery of every abandoned builder slot on daemon startup.
    /// A slot whose last attempt may still run stays quarantined. Every slot is
    /// attempted; the first failure is reported.
    pub(crate) fn cleanup_abandoned(policy: &crate::HostPolicy) -> Result<(), BuildError> {
        let environment = crate::upload::environment(&policy.state_directory);
        // Docker failing to list its workers still leaves the known slots to clean.
        let (slots, listed) = Self::slots(policy, &environment, false);
        slots
            .into_iter()
            .map(|slot| Self::cleanup_abandoned_slot(policy, slot))
            .fold(listed, Result::and)
    }

    fn cleanup_abandoned_slot(policy: &crate::HostPolicy, slot: usize) -> Result<(), BuildError> {
        let mut lock = Self::try_acquire_slot(&policy.state_directory, slot, &policy.docker)?;
        crate::upload::remove_abandoned(&lock.upload_directory()).map_err(|error| {
            BuildError::Prerequisite(format!("remove abandoned Build upload: {error}"))
        })?;
        // A worker the last daemon kept outlives it, so startup removes it.
        // The mark names no owner, only the Docker CLIs recorded below: a
        // removal that fails leaves the slot for the next admission to
        // recover, not held by this process.
        lock.take_over(&[])?;
        let environment = crate::upload::environment(&lock.upload_directory());
        let recorder = lock.recorder();
        let directory = lock.directory.clone();
        let docker = Docker {
            program: &policy.docker,
            environment: &environment,
            working_dir: &directory,
            deadline: crate::Deadline::starting_now(crate::CLEANUP_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: Some(&recorder),
        };
        discard(&docker, slot)?;
        lock.clear()
    }

    fn open_locked(directory: &std::path::Path, slot: usize) -> Result<Self, BuildError> {
        use rustix::fs::{FlockOperation, flock};
        use std::os::unix::fs::MetadataExt as _;
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(lock_error(error)),
        }
        let metadata = fs::symlink_metadata(directory).map_err(lock_error)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(BuildError::Prerequisite(
                "builder state must be an owned directory without group/other write access".into(),
            ));
        }
        let path = directory.join(lock_file(slot));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(lock_error)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => return Err(BuildError::Busy),
            Err(error) => return Err(lock_error(error.into())),
        }
        let claim = claim_slot(directory, slot)?;
        Ok(Self {
            file: std::sync::Arc::new(LockedFile {
                file,
                claim,
                writes: std::sync::Mutex::default(),
            }),
            directory: directory.to_owned(),
            slot,
        })
    }

    /// Whether `slot` in `directory` carries a mark: a Build runs there, or
    /// it waits for recovery.
    #[cfg(test)]
    pub(crate) fn marked_in(directory: &std::path::Path, slot: usize) -> bool {
        fs::metadata(directory.join(lock_file(slot))).is_ok_and(|mark| mark.len() != 0)
    }

    /// Drop the owner from the mark, keeping the Docker CLIs it records: once
    /// a Build returns, only those and its builder can still hold the slot.
    pub(crate) fn disown(&mut self) -> Result<(), BuildError> {
        use std::os::unix::fs::FileExt as _;
        let _writes = self.file.writes.lock().expect("mark writes never panic");
        let mark = {
            let length = self.file.file.metadata().map_err(lock_error)?.len();
            let mut mark = vec![0; usize::try_from(length).unwrap_or(usize::MAX)];
            self.file
                .file
                .read_exact_at(&mut mark, 0)
                .map_err(lock_error)?;
            String::from_utf8_lossy(&mark).into_owned()
        };
        let kept = mark
            .lines()
            .filter(|line| !line.starts_with("owner "))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        self.file
            .file
            .write_all_at(kept.as_bytes(), 0)
            .map_err(lock_error)?;
        self.file
            .file
            .set_len(kept.len() as u64)
            .map_err(lock_error)?;
        self.file.file.sync_all().map_err(lock_error)
    }

    pub(crate) fn clear(&mut self) -> Result<(), BuildError> {
        let _writes = self.file.writes.lock().expect("mark writes never panic");
        self.file.file.set_len(0).map_err(lock_error)?;
        self.file.file.sync_all().map_err(lock_error)
    }

    /// Replace the mark with a recovery's. The mark is never empty meanwhile:
    /// the new one overwrites it in place before the rest is cut.
    fn take_over(&mut self, endpoints: &[String]) -> Result<(), BuildError> {
        use std::os::unix::fs::FileExt as _;
        let _writes = self.file.writes.lock().expect("mark writes never panic");
        let mark = crate::owner::Mark::recovery(endpoints).map_err(lock_error)?;
        self.file
            .file
            .write_all_at(mark.as_bytes(), 0)
            .map_err(lock_error)?;
        self.file
            .file
            .set_len(mark.len() as u64)
            .map_err(lock_error)?;
        self.file.file.sync_all().map_err(lock_error)
    }

    /// Persist uncertainty across process exit before releasing the OS lock,
    /// naming this process as the owner. A slot this attempt already marked
    /// keeps the Docker processes recorded since.
    pub(crate) fn quarantine(&mut self) -> Result<(), BuildError> {
        use std::os::unix::fs::FileExt as _;
        let _writes = self.file.writes.lock().expect("mark writes never panic");
        if self.file.file.metadata().map_err(lock_error)?.len() != 0 {
            return Ok(());
        }
        let header = crate::owner::Mark::header().map_err(lock_error)?;
        self.file
            .file
            .write_all_at(header.as_bytes(), 0)
            .map_err(lock_error)?;
        self.file.file.sync_all().map_err(lock_error)
    }
}

/// Helper containers named after a builder, removed with it.
const HELPERS: [&str; 3] = ["railpack", "prepare", "assemble"];

impl Drop for LockedFile {
    fn drop(&mut self) {
        // Another thread may have forked a child that briefly inherited this
        // CLOEXEC descriptor. Release ownership now, without waiting for exec.
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
        let _ = rustix::fs::flock(&self.claim, rustix::fs::FlockOperation::Unlock);
    }
}

const NETWORK_INTERFACES: &str = "/sys/class/net";

/// A host-wide claim on one build bridge and its subnet.
///
/// Every builder runs BuildKit on the host network, whichever Unix user and
/// slot it belongs to, so bridges are claimed across all of them. A bridge
/// that still exists belongs to a BuildKit that may outlive its claim, such as
/// one orphaned by a killed daemon, and is skipped.
struct Bridge {
    _file: fs::File,
    index: u8,
}

impl Bridge {
    fn claim(directory: &std::path::Path, interfaces: &str) -> Result<Self, BuildError> {
        use rustix::fs::{FlockOperation, flock};
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let shared = fs::Permissions::from_mode(0o1777);
        match fs::DirBuilder::new().mode(0o1777).create(directory) {
            Ok(()) => fs::set_permissions(directory, shared).map_err(bridge_error)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(bridge_error(error)),
        }
        if !fs::symlink_metadata(directory)
            .map_err(bridge_error)?
            .is_dir()
        {
            return Err(BuildError::Prerequisite(format!(
                "{} must be a directory",
                directory.display()
            )));
        }
        let mut unusable = None;
        for index in 0..=u8::MAX {
            let file = match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o666)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(directory.join(format!("{index}.lock")))
            {
                Ok(file) => file,
                // Another user's private claim file, or anything else at the
                // path, takes that index out of use rather than every build.
                Err(error) => {
                    unusable = Some(error);
                    continue;
                }
            };
            let metadata = file.metadata().map_err(bridge_error)?;
            if metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.mode() & 0o666 != 0o666
            {
                file.set_permissions(fs::Permissions::from_mode(0o666))
                    .map_err(bridge_error)?;
            }
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(rustix::io::Errno::WOULDBLOCK) => continue,
                Err(error) => return Err(bridge_error(error.into())),
            }
            if std::path::Path::new(interfaces)
                .join(bridge_name(index))
                .exists()
            {
                continue;
            }
            return Ok(Self { _file: file, index });
        }
        Err(match unusable {
            Some(error) => bridge_error(error),
            None => BuildError::Prerequisite("every build bridge on this host is in use".into()),
        })
    }
}

pub(crate) fn bridge_name(index: u8) -> String {
    format!("{}{index}", crate::BUILD_BRIDGE_PREFIX)
}

/// Claim a slot where every process sees it. `ployz.service` runs with
/// `PrivateTmp`, so a CLI's lock file for the same slot is another file, and
/// only this claim keeps a cache clear out of the daemon's running Build.
/// Directories name the claim by their path, which the daemon and its host's
/// CLI share, so unrelated state directories never contend.
fn claim_slot(directory: &std::path::Path, slot: usize) -> Result<fs::File, BuildError> {
    use rustix::fs::{FlockOperation, flock};
    use sha2::Digest as _;
    use std::os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _};
    let claims = slot_claim_directory();
    match fs::DirBuilder::new().mode(0o1777).create(&claims) {
        Ok(()) => {
            fs::set_permissions(&claims, fs::Permissions::from_mode(0o1777)).map_err(lock_error)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(lock_error(error)),
    }
    let digest = sha2::Sha256::digest(directory.as_os_str().as_bytes());
    let name = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(claims.join(format!("{name}-{slot}.lock")))
        .map_err(lock_error)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(BuildError::Busy),
        Err(error) => Err(lock_error(error.into())),
    }
}

fn slot_claim_directory() -> PathBuf {
    PathBuf::from("/dev/shm/ployz-build-slots")
}

/// `ployz.service` runs with `PrivateTmp`, so `/var/tmp` would hide its claims
/// from builders outside it, and some distributions make `/run/lock` root-only.
/// `/dev/shm` is shared and writable by every user.
fn bridge_directory() -> PathBuf {
    PathBuf::from("/dev/shm/ployz-build-bridges")
}

fn bridge_error(error: std::io::Error) -> BuildError {
    BuildError::Prerequisite(format!("claim a build bridge: {error}"))
}

/// A slot is named after its Full builder: slot 0 keeps the historical name, and the
/// others are numbered after it.
fn slot_name(slot: usize) -> String {
    Access::Full.builder(slot)
}

fn lock_file(slot: usize) -> String {
    format!("{}.lock", slot_name(slot))
}

/// Stable across HOME, captures, and Docker configuration directories: a local
/// CLI and daemon with the same builder name must lock the same retained state.
pub(crate) fn directory() -> PathBuf {
    PathBuf::from("/var/tmp").join(format!(
        "ployz-build-{}",
        rustix::process::getuid().as_raw()
    ))
}

fn lock_error(error: std::io::Error) -> BuildError {
    BuildError::Prerequisite(format!("access the build lock: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn bridge_claims_never_share_a_bridge() {
        let root = std::env::temp_dir().join(format!("ployz-bridges-{}", uuid::Uuid::new_v4()));
        let interfaces = root.join("net");
        fs::create_dir_all(interfaces.join(bridge_name(1))).unwrap();
        let claims = root.join("claims");
        fs::create_dir_all(claims.join("3.lock")).unwrap();

        let first = Bridge::claim(&claims, interfaces.to_str().unwrap()).unwrap();
        let second = Bridge::claim(&claims, interfaces.to_str().unwrap()).unwrap();
        let third = Bridge::claim(&claims, interfaces.to_str().unwrap()).unwrap();
        assert_eq!(
            (first.index, second.index, third.index),
            (0, 2, 4),
            "a held claim, a live bridge and an unusable claim file are all skipped"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bridge_claims_report_why_no_claim_file_opens() {
        let root = std::env::temp_dir().join(format!("ployz-bridges-{}", uuid::Uuid::new_v4()));
        let claims = root.join("claims");
        for index in 0..=u8::MAX {
            fs::create_dir_all(claims.join(format!("{index}.lock"))).unwrap();
        }

        let Err(error) = Bridge::claim(&claims, root.join("net").to_str().unwrap()) else {
            panic!("no claim file can be opened");
        };
        assert!(
            error.to_string().contains("Is a directory"),
            "the open failure is reported, not exhaustion: {error}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dockerfile_platforms_follow_the_running_workers_capabilities() {
        let directory =
            std::env::temp_dir().join(format!("ployz-platform-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let program = directory.join("docker");
        crate::tests::executable(
            &program,
            &crate::tests::as_ployz(&format!(
                r#"#!/bin/sh
case "$1 $2" in
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64","linux/386","linux/arm/v7","linux/ppc64le"]}}]}}' ;;
esac
"#,
                crate::builder_name()
            )),
        );
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: &directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let resources = crate::policy::Resources::default();
        let builder = Builder::acquire(
            &docker,
            Lock::try_acquire_in(&directory).unwrap(),
            Access::Full,
            &resources,
            None,
            Reuse::Never,
        )
        .unwrap();
        for platform in ["linux/386", "linux/arm/v7", "linux/ppc64le", "linux/s390x"] {
            let result = builder.native_platform(
                &[crate::Target {
                    name: "api".into(),
                    platforms: vec![platform.into()],
                }],
                &resources,
            );
            assert_eq!(
                result.is_ok(),
                platform != "linux/s390x",
                "{platform}: {result:?}"
            );
        }
        builder.finish(Ok(())).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quarantine_clear_failure_preserves_cleanup_and_prior_failure_stages() {
        for prior_failure in [false, true] {
            let directory =
                std::env::temp_dir().join(format!("ployz-clear-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(
                &directory,
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
            let program = directory.join("docker");
            crate::tests::executable(&program, "#!/bin/sh\nexit 0\n");
            let environment = BTreeMap::new();
            let docker = Docker {
                program: &program,
                environment: &environment,
                working_dir: &directory,
                deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
                cancellation: None,
                progress: None,
                recorder: None,
            };
            let lock = Lock::try_acquire_in(&directory).unwrap();
            let mut builder = Builder::acquire(
                &docker,
                lock,
                Access::Full,
                &crate::policy::Resources::default(),
                None,
                Reuse::Never,
            )
            .unwrap();
            // A read-only descriptor deterministically makes marker truncation fail.
            builder.lock.as_mut().unwrap().file = std::sync::Arc::new(LockedFile {
                file: fs::File::open(directory.join(format!("{}.lock", crate::builder_name())))
                    .unwrap(),
                claim: fs::File::open("/dev/null").unwrap(),
                writes: std::sync::Mutex::default(),
            });
            let result = if prior_failure {
                Err(BuildError::Result("earlier build failure".into()).at(crate::Stage::Building))
            } else {
                Ok(())
            };
            let error = builder.finish(result).unwrap_err();
            assert_eq!(
                error.stage(),
                if prior_failure {
                    crate::Stage::Building
                } else {
                    crate::Stage::Cleanup
                }
            );
            assert!(!error.is_unknown(), "Docker termination was confirmed");
            if prior_failure {
                assert!(error.to_string().contains("earlier build failure"));
            }
            assert!(error.to_string().contains("build lock"));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    fn quarantined_slot(slot: usize, mark: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(
            directory.path(),
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let docker = directory.path().join("docker");
        crate::tests::executable(
            &docker,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf '%s\\n' \"$*\" >> '{}/calls'\n{}[ \"$1 $2\" = 'inspect --type' ] && made_by_ployz \"$6\"\nexit 0\n",
                directory.path().display(),
                crate::tests::made_by_ployz_sh()
            ),
        );
        fs::write(directory.path().join(lock_file(slot)), mark).unwrap();
        (directory, docker)
    }

    fn sleeper(arguments: &[&str]) -> std::process::Child {
        std::process::Command::new("sh")
            .args(["-c", "sleep 30; :"])
            .args(arguments)
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_dead_owner_whose_docker_child_survives_stays_quarantined() {
        use crate::owner::{Process, Role};
        let slot = 7;
        let mut owner = sleeper(&[]);
        let dead_owner = Process::observe(owner.id()).unwrap().unwrap();
        owner.kill().unwrap();
        owner.wait().unwrap();
        let mut child = sleeper(&[]);
        let surviving = Process::observe(child.id()).unwrap().unwrap();
        let (directory, docker) = quarantined_slot(
            slot,
            &crate::owner::named(&[(Role::Owner, dead_owner), (Role::Docker, surviving)]),
        );

        let refused = Lock::try_acquire_slot(directory.path(), slot, &docker)
            .err()
            .unwrap();
        assert!(refused.is_unknown());
        let message = refused.to_string();
        assert!(
            message.contains(&format!(
                "Docker CLI process {} the Build started",
                child.id()
            )),
            "{message}"
        );
        assert!(
            !message.contains(&format!("process {} that owned", owner.id())),
            "{message}"
        );
        assert!(
            !directory.path().join("calls").exists(),
            "recovery touched the builder while the Build's Docker CLI ran"
        );

        child.kill().unwrap();
        child.wait().unwrap();
        let lock = Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap();
        assert!(!lock.marked().unwrap());
        let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
        let name = slot_name(slot);
        for removed in [
            format!("rm --force id-buildx_buildkit_{name}0"),
            format!("rm --force id-{name}-railpack"),
            format!("rm --force id-{name}-prepare"),
            format!("rm --force id-{name}-assemble"),
        ] {
            assert!(calls.lines().any(|call| call == removed), "{calls}");
        }
    }

    #[test]
    fn a_gated_child_whose_parent_never_records_it_runs_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let ran = directory.path().join("ran");
        let mut child = gated(std::path::Path::new("touch"))
            .arg(&ran)
            .spawn()
            .unwrap();
        drop(child.stdin.take());
        assert_eq!(child.wait().unwrap().code(), Some(125));
        assert!(!ran.exists(), "the child ran before it was recorded");
    }

    #[test]
    fn recovery_removes_the_builder_from_the_docker_the_build_used() {
        use crate::owner::Process;
        let slot = 10;
        let mut gone = sleeper(&[]);
        let docker_process = Process::observe(gone.id()).unwrap().unwrap();
        gone.kill().unwrap();
        gone.wait().unwrap();
        let mark = format!(
            "{}{}",
            crate::owner::named(&[]),
            crate::owner::Mark::docker_line(docker_process, "unix:///run/user/7/docker.sock")
        );
        let (directory, docker) = quarantined_slot(slot, &mark);
        crate::tests::executable(
            &docker,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf '%s %s\\n' \"$DOCKER_HOST\" \"$*\" >> '{}/calls'\n{}[ \"$1 $2\" = 'inspect --type' ] && made_by_ployz \"$6\"\nexit 0\n",
                directory.path().display(),
                crate::tests::made_by_ployz_sh()
            ),
        );
        drop(Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap());
        let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
        // The slot ran a Build or a Free Build: recovery can't tell which, so it removes both.
        for access in [Access::Full, Access::Free] {
            assert!(
                calls.lines().any(|call| call
                    == format!(
                        "unix:///run/user/7/docker.sock rm --force id-buildx_buildkit_{}0",
                        access.builder(slot)
                    )),
                "{calls}"
            );
        }
        assert!(!calls.contains("unix:///var/run/docker.sock"), "{calls}");
    }

    #[test]
    fn recovery_names_its_own_docker_clis_on_this_boot_before_they_run() {
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
        let before_reboot = format!(
            "termination unconfirmed\nboot an-earlier-boot\nowner 1 1\ndocker 2 2 {}\n",
            crate::owner::DEFAULT_ENDPOINT
        );
        for (slot, mark) in [
            (11, "termination unconfirmed\n"),
            (12, before_reboot.as_str()),
        ] {
            let (directory, docker) = quarantined_slot(slot, mark);
            let marker = directory.path().join(lock_file(slot));
            crate::tests::executable(
                &docker,
                &format!(
                    "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nif grep -qx 'boot {}' '{}' && grep -q \"^docker $$ \" '{}' && ! grep -q '^owner ' '{}'; then echo named; else echo unnamed; cat '{}'; fi >> '{}/seen'\n",
                    boot.trim(),
                    marker.display(),
                    marker.display(),
                    marker.display(),
                    marker.display(),
                    directory.path().display()
                ),
            );
            drop(Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap());
            let seen = fs::read_to_string(directory.path().join("seen")).unwrap();
            assert!(
                !seen.is_empty() && seen.lines().all(|line| line == "named"),
                "a recovery Docker CLI ran before a mark on this boot named it: {seen}"
            );
        }
    }

    #[test]
    fn a_failed_recovery_leaves_nothing_that_blocks_a_retry() {
        let custom = format!(
            "{}docker 2 2 unix:///run/user/7/docker.sock\n",
            crate::owner::named(&[])
        );
        for (slot, mark, endpoint) in [
            (
                13,
                "termination unconfirmed\n",
                crate::owner::DEFAULT_ENDPOINT,
            ),
            (14, custom.as_str(), "unix:///run/user/7/docker.sock"),
        ] {
            let (directory, docker) = quarantined_slot(slot, mark);
            crate::tests::executable(
                &docker,
                &format!(
                    "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\n[ -e '{root}/docker-up' ] || {{ echo 'Cannot connect to the Docker daemon' >&2; exit 1; }}\nprintf '%s %s\\n' \"$DOCKER_HOST\" \"$*\" >> '{root}/calls'\n{made}[ \"$1 $2\" = 'inspect --type' ] && made_by_ployz \"$6\"\nexit 0\n",
                    root = directory.path().display(),
                    made = crate::tests::made_by_ployz_sh()
                ),
            );
            let refused = Lock::try_acquire_slot(directory.path(), slot, &docker)
                .err()
                .unwrap();
            assert!(
                refused.to_string().contains("removing its builder from"),
                "{refused}"
            );

            fs::write(directory.path().join("docker-up"), "").unwrap();
            let retried = Lock::try_acquire_slot(directory.path(), slot, &docker);
            assert!(
                retried.is_ok(),
                "a retry after Docker returned was refused: {}",
                retried.err().unwrap()
            );
            let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
            assert!(
                calls.lines().any(|call| call
                    == format!(
                        "{endpoint} rm --force id-buildx_buildkit_{}0",
                        slot_name(slot)
                    )),
                "the retry forgot where the builder lives: {calls}"
            );
        }
    }

    #[test]
    fn a_mark_that_names_no_processes_waits_for_buildx_using_its_builder() {
        let slot = 8;
        let (directory, docker) = quarantined_slot(slot, "termination unconfirmed\n");
        let mut buildx = sleeper(&["buildx", &slot_name(slot)]);

        let refused = Lock::try_acquire_slot(directory.path(), slot, &docker)
            .err()
            .unwrap();
        let message = refused.to_string();
        assert!(
            message.contains(&format!("Buildx process {} using the builder", buildx.id()))
                && message.contains("predates recorded processes"),
            "{message}"
        );
        assert!(!directory.path().join("calls").exists());

        buildx.kill().unwrap();
        buildx.wait().unwrap();
        drop(Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap());
        assert!(
            fs::read_to_string(directory.path().join(lock_file(slot)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn recovery_keeps_the_mark_when_docker_cannot_confirm_the_builder_is_gone() {
        let slot = 9;
        let (directory, docker) = quarantined_slot(slot, "termination unconfirmed\n");
        crate::tests::executable(
            &docker,
            "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\necho 'Cannot connect to the Docker daemon' >&2\nexit 1\n",
        );
        let refused = Lock::try_acquire_slot(directory.path(), slot, &docker)
            .err()
            .unwrap();
        assert!(refused.is_unknown());
        assert!(
            refused
                .to_string()
                .contains("none of its processes is running, but removing its builder from"),
            "{refused}"
        );
        assert!(
            !fs::read_to_string(directory.path().join(lock_file(slot)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn admission_refuses_competitors_and_uncertain_builder_ownership() {
        let directory =
            std::env::temp_dir().join(format!("ployz-admission-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(
            &directory,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let lock = Lock::try_acquire_in(&directory).unwrap();
        assert!(matches!(
            Lock::try_acquire_in(&directory),
            Err(BuildError::Busy)
        ));
        drop(lock);
        let mut lock = Lock::try_acquire_in(&directory).unwrap();
        lock.quarantine().unwrap();
        drop(lock);
        assert!(matches!(
            Lock::try_acquire_in(&directory),
            Err(BuildError::UncertainTermination(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    /// A slot directory whose fake Docker logs each call, reports the worker
    /// container as `<directory>/worker` says, lists `<directory>/names` as
    /// its containers, of which those in `owned` carry this user's marker and
    /// those in `foreign` none, lists `labelled` as the volumes carrying this
    /// user's label (unless `fail-volumes` exists) and `volumes` as all, and
    /// fails removals while `fail-rm` exists.
    fn host_slot() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(
            directory.path(),
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        crate::tests::executable(
            &directory.path().join("docker"),
            &format!(
                r#"#!/bin/sh
[ "$1" = --ready ] && exit 0
printf '%s\n' "$*" >> '{0}/calls'
[ -f '{0}/docker-down' ] && {{ echo 'Cannot connect to the Docker daemon' >&2; exit 1; }}
{2}if [ "$1 $2 $5" = 'inspect --type {{{{.State.Running}}}}' ]; then
  cat '{0}/running' 2>/dev/null || {{ echo "Error: No such container: $6" >&2; exit 1; }}
  exit 0
fi
case "$1 $2" in
  'stop '*) [ -f '{0}/fail-stop' ] && {{ echo 'Error response from daemon: cannot stop container' >&2; exit 1; }} ;;
  'inspect --type')
    if grep -qx "$6" '{0}/owned' 2>/dev/null; then made_by_ployz "$6"
    elif grep -qx "$6" '{0}/foreign' 2>/dev/null; then
      printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"alpine","Env":["PATH=/bin"]}}}}'
    elif grep -qx "$6" '{0}/borrowed' 2>/dev/null; then
      printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"alpine:3","Env":["NOTE=an earlier pin\nmoby/buildkit:v0.26.2\nghcr.io/railwayapp/railpack-frontend@sha256:db24dc37640b6887c3d455b40876ea30f75182964479670cba6e4cde7ffef103","BUILDKIT_SETUP_CGROUPV2_ROOT=1"]}}}}'
    elif grep -qx "$6" '{0}/forged' 2>/dev/null; then
      printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"moby/buildkit:v0.26.2","Env":["NOTE=x\n{OWNER}={1}\nBUILDKIT_SETUP_CGROUPV2_ROOT=1"]}}}}'
    elif grep -qx "$6" '{0}/earlier' 2>/dev/null; then
      printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"moby/buildkit:v0.26.2","Env":["PATH=/bin","BUILDKIT_SETUP_CGROUPV2_ROOT=1"]}}}}'
    elif grep -qx "$6" '{0}/unpinned' 2>/dev/null; then
      printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"buildpack-deps:bookworm","Env":["PATH=/bin","BUILDKIT_SETUP_CGROUPV2_ROOT=1"]}}}}'
    else echo "Error: No such container: $6" >&2; exit 1; fi ;;
  'inspect --format') cat '{0}/worker' 2>/dev/null ;;
  'ps --all') cat '{0}/names' 2>/dev/null ;;
  'volume ls')
    case "$*" in
      *label=*) [ -f '{0}/fail-volumes' ] && exit 1; cat '{0}/labelled' 2>/dev/null ;;
      *--quiet*) ;;
      *) cat '{0}/volumes' 2>/dev/null ;;
    esac ;;
  'context show') echo default ;;
  'rm --force'|'buildx rm') [ -f '{0}/fail-rm' ] && exit 1 ;;
  'create --name')
    if [ -f '{0}/create-conflict' ]; then
      echo "Error response from daemon: Conflict. The container name \"/$3\" is already in use" >&2; exit 1
    fi
    echo "id-$3" ;;
esac
exit 0
"#,
                directory.path().display(),
                owner(),
                crate::tests::made_by_ployz_sh(),
            ),
        );
        directory
    }

    /// Park a kept worker for the slot `lock` holds, as a clean attempt
    /// would, under `older` policy or the current one.
    fn park_kept(lock: &Lock, older: Option<&str>) -> u8 {
        let bridge = Bridge::claim(&bridge_directory(), NETWORK_INTERFACES).unwrap();
        let index = bridge.index;
        lock.park(Parked {
            kept: Some(Kept {
                builder: Access::Full.builder(lock.slot),
                container: "c0ffee".into(),
                spec: older.map_or_else(
                    || spec(&crate::policy::Resources::default(), index),
                    str::to_owned,
                ),
            }),
            bridge,
        });
        index
    }

    /// Whether this process still parks a bridge for `directory`'s `slot`.
    /// Another test may claim a freed index at once, so a release is read
    /// here rather than from the claim file.
    fn parks(directory: &std::path::Path, slot: usize) -> bool {
        let slot = directory.join(lock_file(slot));
        PARKED
            .lock()
            .unwrap()
            .iter()
            .any(|(parking, _)| *parking == slot)
    }

    fn claimed(index: u8) -> bool {
        let claim = fs::File::open(bridge_directory().join(format!("{index}.lock"))).unwrap();
        match rustix::fs::flock(&claim, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => false,
            Err(rustix::io::Errno::WOULDBLOCK) => true,
            Err(error) => panic!("{error}"),
        }
    }

    fn calls_since(directory: &std::path::Path, seen: &mut usize) -> Vec<String> {
        let calls = fs::read_to_string(directory.join("calls")).unwrap_or_default();
        let lines = calls
            .lines()
            .skip(*seen)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        *seen += lines.len();
        lines
    }

    /// What an attempt runs before creating its worker, when it starts in
    /// none: it removes those of the slot's workers in `owned`.
    fn fresh_calls(directory: &std::path::Path, slot: usize, owned: &[String]) -> Vec<String> {
        let name = Access::Full.builder(slot);
        let mut calls = vec![inspect_owner(&container(&name))];
        for access in [Access::Full, Access::Free] {
            let worker = container(&access.builder(slot));
            calls.push(inspect_owner(&worker));
            if owned.contains(&worker) {
                calls.push(format!("stop --time {WORKER_STOP_SECONDS} id-{worker}"));
                calls.push(format!("rm --force id-{worker}"));
            }
        }
        calls.push(format!("buildx rm --keep-state --keep-daemon {name}"));
        calls.push(format!(
            "volume create --label {OWNER_LABEL}={} {}_state",
            owner(),
            container(&name)
        ));
        calls.push(create_call(directory, &name));
        calls
    }

    fn inspect_owner(worker: &str) -> String {
        ["inspect --type container --format ", "{{json .}} ", worker].concat()
    }

    fn create_call(directory: &std::path::Path, name: &str) -> String {
        format!(
            "buildx create --name {name} --driver docker-container --driver-opt image={BUILDKIT_IMAGE} --driver-opt network=host --driver-opt restart-policy=no --driver-opt env.{OWNER}={} --buildkitd-config {}",
            owner(),
            directory.join(format!("{name}.buildkitd.toml")).display()
        )
    }

    #[test]
    fn a_clean_attempt_keeps_its_builder_for_the_next_and_a_failed_one_removes_it() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let resources = crate::policy::Resources::default();
        let name = crate::builder_name();
        let worker = format!(
            "inspect --format {{{{.Id}}}} {{{{.State.Running}}}} {}",
            container(&name)
        );
        let mut seen = 0;
        let attempt = || {
            Builder::acquire(
                &docker,
                Lock::try_acquire_in(directory).unwrap(),
                Access::Full,
                &resources,
                None,
                Reuse::WhenClean,
            )
            .unwrap()
        };

        let first = attempt();
        let bridge = first.bridge.as_ref().unwrap().index;
        assert_eq!(
            calls_since(directory, &mut seen),
            fresh_calls(directory, 0, &[])
        );
        // Buildx starts the worker.
        fs::write(directory.join("owned"), container(&name)).unwrap();
        fs::write(
            directory.join("worker"),
            format!("id-{} true\n", container(&name)),
        )
        .unwrap();
        first.take_worker().unwrap();
        assert_eq!(
            calls_since(directory, &mut seen),
            [inspect_owner(&container(&name))]
        );
        first.finish(Ok(())).unwrap();
        assert_eq!(
            calls_since(directory, &mut seen),
            std::slice::from_ref(&worker)
        );
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());
        assert!(
            claimed(bridge),
            "no other builder claims the kept builder's bridge"
        );

        let second = attempt();
        assert_eq!(
            calls_since(directory, &mut seen),
            [worker, create_call(directory, &name)],
            "the kept worker is started in, not replaced"
        );
        assert_eq!(second.bridge.as_ref().unwrap().index, bridge);
        let error = second
            .finish::<()>(Err(BuildError::Result("failed".into())))
            .unwrap_err();
        assert!(!error.is_unknown());
        assert_eq!(
            calls_since(directory, &mut seen),
            [
                inspect_owner(&container(&name)),
                format!("buildx rm --keep-state --keep-daemon {name}"),
                format!("stop --time {WORKER_STOP_SECONDS} id-{}", container(&name)),
                format!("rm --force id-{}", container(&name)),
            ],
            "teardown keeps the cache volume"
        );
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());
    }

    #[test]
    fn a_worker_without_a_matching_record_is_removed_before_the_attempt_starts() {
        let resources = crate::policy::Resources::default();
        for (case, kept, worker, reuse) in [
            (
                "kept by a daemon that restarted",
                None,
                "c0ffee true",
                Reuse::WhenClean,
            ),
            (
                "policy changed",
                Some(Some("an older policy")),
                "c0ffee true",
                Reuse::WhenClean,
            ),
            (
                "worker replaced",
                Some(None),
                "decaf true",
                Reuse::WhenClean,
            ),
            (
                "worker stopped",
                Some(None),
                "c0ffee false",
                Reuse::WhenClean,
            ),
            ("never reused", Some(None), "c0ffee true", Reuse::Never),
        ] {
            let directory = host_slot();
            let directory = directory.path();
            let program = directory.join("docker");
            fs::write(directory.join("worker"), worker).unwrap();
            let worker = container(&crate::builder_name());
            fs::write(directory.join("owned"), &worker).unwrap();
            let owned = vec![worker];
            let lock = Lock::try_acquire_in(directory).unwrap();
            let parked = kept.map(|older| park_kept(&lock, older));
            let environment = BTreeMap::new();
            let docker = Docker {
                program: &program,
                environment: &environment,
                working_dir: directory,
                deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
                cancellation: None,
                progress: None,
                recorder: None,
            };
            let builder =
                Builder::acquire(&docker, lock, Access::Full, &resources, None, reuse).unwrap();
            let calls = calls_since(directory, &mut 0);
            assert!(
                calls.ends_with(&fresh_calls(directory, 0, &owned)),
                "{case}: {calls:?}"
            );
            assert!(
                parked.is_none() || !parks(directory, 0),
                "{case}: the removed worker's bridge stayed held"
            );
            drop(builder);
        }
    }

    #[test]
    fn an_attempt_never_starts_in_or_removes_a_container_that_only_shares_its_worker_name() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let foreign = container(&crate::builder_name());
        fs::write(directory.join("foreign"), &foreign).unwrap();
        fs::write(directory.join("worker"), "decaf true").unwrap();
        let lock = Lock::try_acquire_in(directory).unwrap();
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let refused = Builder::acquire(
            &docker,
            lock,
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        );
        assert!(
            matches!(&refused, Err(BuildError::Prerequisite(reason)) if reason.contains(&foreign)),
            "{:?}",
            refused.err()
        );
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.iter().all(|call| call.starts_with("inspect")),
            "an attempt touched {foreign}: {calls:?}"
        );
    }

    #[test]
    fn a_cache_clear_skips_and_names_a_slot_whose_worker_name_another_container_holds() {
        let directory = host_slot();
        let directory = directory.path();
        let foreign = container(&Access::Full.builder(0));
        let worker = container(&Access::Full.builder(1));
        fs::write(directory.join("names"), format!("{foreign}\n{worker}")).unwrap();
        fs::write(directory.join("foreign"), &foreign).unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        fs::write(directory.join("worker"), "c0ffee true").unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let cleared = crate::clear_cache(&policy);
        assert!(
            matches!(&cleared, Err(BuildError::Prerequisite(reason)) if reason.contains(&foreign)),
            "{cleared:?}"
        );
        let calls = calls_since(directory, &mut 0);
        assert!(
            !calls
                .iter()
                .any(|call| call.ends_with(&foreign) && !call.starts_with("inspect")),
            "the clear touched {foreign}: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("buildx")
                && call.split(' ').any(|word| word == Access::Full.builder(0))),
            "the clear started a builder in {foreign}: {calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.starts_with("buildx prune")
                && call.contains(&format!("--builder {} ", Access::Full.builder(1)))),
            "the other slot was not cleared: {calls:?}"
        );
    }

    /// An earlier Ployz crashed mid-attempt: its mark names Docker processes
    /// now gone, and its worker, made before workers carried a marker, runs.
    #[test]
    fn a_worker_an_earlier_ployz_left_mid_attempt_is_recovered_and_the_slot_builds() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        fs::write(directory.join("earlier"), &worker).unwrap();
        fs::write(directory.join("worker"), "c0ffee true").unwrap();
        let mut gone = sleeper(&[]);
        let docker_process = crate::owner::Process::observe(gone.id()).unwrap().unwrap();
        gone.kill().unwrap();
        gone.wait().unwrap();
        let state = directory.join("state");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        fs::write(
            state.join(lock_file(0)),
            format!(
                "{}{}",
                crate::owner::named(&[]),
                crate::owner::Mark::docker_line(docker_process, crate::owner::DEFAULT_ENDPOINT)
            ),
        )
        .unwrap();
        let policy = crate::HostPolicy {
            state_directory: state,
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let admission = crate::Admission::try_acquire_with(&policy).unwrap();
        assert_eq!(admission.slot(), 0);
        let program = policy.docker;
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let builder = Builder::acquire(
            &docker,
            admission.lock,
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        )
        .unwrap();
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.contains(&format!("rm --force id-{worker}")),
            "the earlier worker stayed: {calls:?}"
        );
        assert!(
            calls
                .last()
                .is_some_and(|call| call.starts_with("buildx create")),
            "{calls:?}"
        );
        drop(builder);
    }

    #[test]
    fn a_build_passes_over_a_slot_whose_worker_name_another_container_holds() {
        let directory = host_slot();
        let directory = directory.path();
        let foreign = container(&Access::Full.builder(0));
        fs::write(directory.join("names"), &foreign).unwrap();
        fs::write(directory.join("foreign"), &foreign).unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let admission = crate::Admission::try_acquire_with(&policy).unwrap();
        assert_eq!(admission.slot(), 1);
    }

    /// A slot an earlier Ployz left mid-attempt: its mark names Docker
    /// processes now gone.
    fn crashed_slot(directory: &std::path::Path) -> crate::HostPolicy {
        let mut gone = sleeper(&[]);
        let docker_process = crate::owner::Process::observe(gone.id()).unwrap().unwrap();
        gone.kill().unwrap();
        gone.wait().unwrap();
        let state = directory.join("state");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        fs::write(
            state.join(lock_file(0)),
            format!(
                "{}{}",
                crate::owner::named(&[]),
                crate::owner::Mark::docker_line(docker_process, crate::owner::DEFAULT_ENDPOINT)
            ),
        )
        .unwrap();
        crate::HostPolicy {
            state_directory: state,
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        }
    }

    #[test]
    fn recovery_leaves_a_container_that_only_shares_a_helpers_name() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        let helper = format!("{}-railpack", Access::Full.builder(0));
        fs::write(directory.join("earlier"), &worker).unwrap();
        fs::write(directory.join("foreign"), &helper).unwrap();
        fs::write(directory.join("names"), format!("{worker}\n{helper}")).unwrap();
        let policy = crashed_slot(directory);
        Lock::cleanup_abandoned(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.contains(&format!("rm --force id-{worker}")),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.ends_with(&helper) && !call.starts_with("inspect")),
            "recovery touched {helper}: {calls:?}"
        );
    }

    /// A public image whose tag only resembles one Ployz pinned is not an
    /// earlier Ployz's, under a worker's name or a helper's.
    #[test]
    fn recovery_leaves_containers_running_an_image_ployz_never_pinned() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        let helper = format!("{}-prepare", Access::Full.builder(0));
        fs::write(directory.join("unpinned"), format!("{worker}\n{helper}")).unwrap();
        fs::write(directory.join("names"), format!("{worker}\n{helper}")).unwrap();
        let policy = crashed_slot(directory);
        Lock::cleanup_abandoned(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        for held in [&worker, &helper] {
            assert!(
                !calls
                    .iter()
                    .any(|call| call.ends_with(held.as_str()) && !call.starts_with("inspect")),
                "startup touched {held}: {calls:?}"
            );
        }
    }

    /// An earlier Ployz's image named anywhere but the container's own image,
    /// here in an environment value spanning lines, does not make it Ployz's.
    #[test]
    fn recovery_leaves_a_container_whose_own_image_is_not_the_one_ployz_pinned() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        let helper = format!("{}-railpack", Access::Full.builder(0));
        fs::write(directory.join("borrowed"), format!("{worker}\n{helper}")).unwrap();
        fs::write(directory.join("names"), format!("{worker}\n{helper}")).unwrap();
        let policy = crashed_slot(directory);
        Lock::cleanup_abandoned(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        for held in [&worker, &helper] {
            assert!(
                !calls
                    .iter()
                    .any(|call| call.ends_with(held.as_str()) && !call.starts_with("inspect")),
                "startup touched {held}: {calls:?}"
            );
        }
    }

    /// The owner marker and Buildx's variable, written inside another
    /// variable's value, mark nothing.
    #[test]
    fn recovery_leaves_a_container_whose_markers_are_only_inside_another_variable() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        let helper = format!("{}-railpack", Access::Full.builder(0));
        fs::write(directory.join("forged"), format!("{worker}\n{helper}")).unwrap();
        fs::write(directory.join("names"), format!("{worker}\n{helper}")).unwrap();
        let policy = crashed_slot(directory);
        Lock::cleanup_abandoned(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        for held in [&worker, &helper] {
            assert!(
                !calls
                    .iter()
                    .any(|call| call.ends_with(held.as_str()) && !call.starts_with("inspect")),
                "startup touched {held}: {calls:?}"
            );
        }
    }

    #[test]
    fn a_cache_clear_names_every_container_that_holds_a_slot_name_ployz_did_not_create() {
        let directory = host_slot();
        let directory = directory.path();
        // A slot Ployz never recorded, and a Free worker whose builder has no cache.
        let unrecorded = container(&Access::Full.builder(9));
        let free = container(&Access::Free.builder(0));
        fs::write(directory.join("names"), format!("{unrecorded}\n{free}")).unwrap();
        fs::write(directory.join("foreign"), format!("{unrecorded}\n{free}")).unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let cleared = crate::clear_cache(&policy);
        assert!(
            matches!(&cleared, Err(BuildError::Prerequisite(reason))
                if reason.contains(&unrecorded) && reason.contains(&free)),
            "{cleared:?}"
        );
    }

    #[test]
    fn daemon_start_removes_every_worker_docker_lists_whatever_slot_state_survived() {
        let directory = host_slot();
        let directory = directory.path();
        let workers = [
            container(&Access::Full.builder(0)),
            container(&Access::Full.builder(1)),
            container(&Access::Free.builder(3)),
        ];
        fs::write(directory.join("names"), workers.join("\n")).unwrap();
        fs::write(directory.join("owned"), workers.join("\n")).unwrap();
        fs::write(directory.join("worker"), "c0ffee true").unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.to_owned(),
            docker: directory.join("docker"),
            ..Default::default()
        };
        Lock::cleanup_abandoned(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        for worker in &workers {
            assert!(
                calls.contains(&format!("rm --force id-{worker}")),
                "{worker}: {calls:?}"
            );
        }
        for slot in [0, 1, 3] {
            assert!(
                fs::read(directory.join(lock_file(slot)))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_cache_clear_reaches_slots_only_docker_knows() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(2));
        fs::write(directory.join("names"), &worker).unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("private-tmp-elsewhere"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        crate::clear_cache(&policy).unwrap();
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.contains(&format!("rm --force id-{worker}")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_container_that_only_shares_a_worker_name_is_left_alone() {
        let directory = host_slot();
        let directory = directory.path();
        let foreign = container(&Access::Full.builder(77));
        fs::write(directory.join("names"), &foreign).unwrap();
        fs::write(directory.join("foreign"), &foreign).unwrap();
        fs::write(
            directory.join("volumes"),
            format!("{}_state", container(&Access::Full.builder(78))),
        )
        .unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        Lock::cleanup_abandoned(&policy).unwrap();
        let cleared = crate::clear_cache(&policy);
        assert!(
            matches!(&cleared, Err(BuildError::Prerequisite(reason)) if reason.contains(&foreign)),
            "{cleared:?}"
        );
        let calls = calls_since(directory, &mut 0);
        assert!(
            !calls
                .iter()
                .any(|call| call.ends_with(&foreign) && !call.starts_with("inspect")),
            "{calls:?}"
        );
        let unlabelled = [Access::Full, Access::Free]
            .into_iter()
            .flat_map(|access| {
                let builder = access.builder(78);
                let mut names = slot_names(access, 78);
                names.push(format!("{}_state", container(&builder)));
                names.push(builder);
                names
            })
            .collect::<Vec<_>>();
        assert!(
            !calls.iter().any(|call| call
                .split(' ')
                .any(|word| unlabelled.iter().any(|name| name == word))),
            "a volume without this user's label named a slot: {calls:?}"
        );
    }

    #[test]
    fn a_cache_clear_acts_on_the_workers_docker_listed_when_volumes_cannot_be_listed() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(2));
        fs::write(directory.join("names"), &worker).unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        fs::write(directory.join("fail-volumes"), "").unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let error = crate::clear_cache(&policy).unwrap_err();
        assert!(error.to_string().contains("list build caches"), "{error}");
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.contains(&format!("rm --force id-{worker}")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_cache_clear_from_another_mount_namespace_leaves_a_running_builds_slot() {
        let directory = host_slot();
        let directory = directory.path();
        let state = directory.join("state");
        let worker = container(&Access::Full.builder(1));
        fs::write(directory.join("names"), &worker).unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        let program = directory.join("docker");
        let building = Lock::try_acquire_slot(&state, 1, &program).unwrap();
        // The daemon's PrivateTmp hides its lock file: the CLI opens another.
        fs::rename(
            state.join(lock_file(1)),
            directory.join("daemon-private.lock"),
        )
        .unwrap();
        let policy = crate::HostPolicy {
            state_directory: state,
            configuration_file: directory.join("build.yaml"),
            docker: program,
            ..Default::default()
        };
        assert!(matches!(crate::clear_cache(&policy), Err(BuildError::Busy)));
        let calls = calls_since(directory, &mut 0);
        assert!(
            !calls
                .iter()
                .any(|call| call.ends_with(&worker) && !call.starts_with("inspect")),
            "{calls:?}"
        );
        drop(building);
    }

    #[test]
    fn a_teardown_docker_cannot_confirm_keeps_its_bridge_until_recovery_removes_the_worker() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let builder = Builder::acquire(
            &docker,
            Lock::try_acquire_in(directory).unwrap(),
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        )
        .unwrap();
        let index = builder.bridge.as_ref().unwrap().index;
        fs::write(directory.join("fail-rm"), "").unwrap();
        let error = builder
            .finish::<()>(Err(BuildError::Result("failed".into())))
            .unwrap_err();
        assert!(error.is_unknown(), "{error}");
        assert!(
            claimed(index),
            "the bridge was freed while its worker may run"
        );

        fs::remove_file(directory.join("fail-rm")).unwrap();
        drop(Lock::try_acquire_own_slot(directory, 0, &program).unwrap());
        assert!(
            !parks(directory, 0),
            "recovery removed the worker but kept its bridge"
        );
    }

    #[test]
    fn an_attempt_that_never_confirmed_its_end_leaves_its_kept_builder_to_recovery() {
        let slot = 0;
        let (directory, docker) = quarantined_slot(slot, &crate::owner::named(&[]));
        let name = Access::Full.builder(slot);
        park_kept(&Lock::open_locked(directory.path(), slot).unwrap(), None);
        let lock = Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap();
        let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
        assert!(
            calls.contains(&format!("rm --force id-{}", container(&name))),
            "{calls}"
        );
        assert!(
            !parks(directory.path(), slot),
            "recovery left the removed worker's bridge held"
        );
        fs::remove_file(directory.path().join("calls")).unwrap();
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &docker,
            environment: &environment,
            working_dir: directory.path(),
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let builder = Builder::acquire(
            &docker,
            lock,
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        )
        .unwrap();
        // This Docker reports every worker as one Ployz made.
        let workers = [Access::Full, Access::Free].map(|access| container(&access.builder(slot)));
        assert_eq!(
            calls_since(directory.path(), &mut 0),
            fresh_calls(directory.path(), slot, &workers)
        );
        drop(builder);
    }

    /// Another container takes the worker's name after the solve: the Build
    /// keeps no record of it, and removes nothing under that name.
    #[test]
    fn a_worker_name_taken_over_after_the_solve_is_not_kept() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let worker = container(&crate::builder_name());
        let builder = Builder::acquire(
            &docker,
            Lock::try_acquire_in(directory).unwrap(),
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        )
        .unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        builder.take_worker().unwrap();
        let mut seen = 0;
        calls_since(directory, &mut seen);
        // The solve ends, and another container now holds the name.
        fs::remove_file(directory.join("owned")).unwrap();
        fs::write(directory.join("foreign"), &worker).unwrap();
        fs::write(directory.join("worker"), "intruder true").unwrap();
        builder.finish(Ok(())).unwrap();
        assert!(
            !parks(directory, 0),
            "the Build kept a record of another container"
        );
        let calls = calls_since(directory, &mut seen);
        assert!(
            calls.iter().all(|call| call.starts_with("inspect")),
            "the Build acted on the name another container took: {calls:?}"
        );
    }

    /// Another container takes a helper's name between Ployz's look and its
    /// create: the failed create reports no ID, so nothing is removed.
    #[test]
    fn a_helper_whose_create_lost_its_name_removes_nothing() {
        let directory = host_slot();
        let directory = directory.path();
        fs::write(directory.join("create-conflict"), "").unwrap();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let helper = format!("{}-railpack", crate::builder_name());
        let created = docker.with_container(&helper, &[crate::railpack::IMAGE], |_| Ok(()));
        assert!(created.is_err());
        let calls = calls_since(directory, &mut 0);
        assert!(
            !calls.iter().any(|call| call.starts_with("rm")),
            "a helper whose create failed removed a container: {calls:?}"
        );

        fs::remove_file(directory.join("create-conflict")).unwrap();
        let mut worked = String::new();
        docker
            .with_container(&helper, &[crate::railpack::IMAGE], |id| {
                worked = id.to_owned();
                Ok(())
            })
            .unwrap();
        assert_eq!(worked, format!("id-{helper}"));
        assert!(
            calls_since(directory, &mut 0).contains(&format!("rm --force id-{helper}")),
            "the helper was not removed by its ID"
        );
    }

    /// Recovery of a crashed slot whose names no container holds removes
    /// nothing: a container that takes one meanwhile is not the slot's.
    #[test]
    fn recovery_removes_nothing_under_a_name_no_container_held() {
        let directory = host_slot();
        let directory = directory.path();
        let policy = crashed_slot(directory);
        drop(Lock::try_acquire_slot(&policy.state_directory, 0, &policy.docker).unwrap());
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.iter().all(|call| call.starts_with("inspect")),
            "recovery removed by name: {calls:?}"
        );
    }

    #[test]
    fn only_a_container_made_from_its_pinned_image_with_one_owner_marker_is_ployzs() {
        let worker = container(&crate::builder_name());
        let helper = format!("{}-railpack", crate::builder_name());
        let made = |image: Option<&str>, environment: &[&str]| Made {
            image: image.map(str::to_owned),
            environment: Some(
                environment
                    .iter()
                    .map(|&variable| variable.to_owned())
                    .collect(),
            ),
        };
        let pinned = Some(crate::BUILDKIT_IMAGE.as_str());
        let legacy = Some("moby/buildkit:v0.26.2");
        for (case, name, made, ployzs) in [
            (
                "marked worker",
                &worker,
                made(pinned, &["PLOYZ_BUILD_OWNER=1000"]),
                true,
            ),
            (
                "earlier worker",
                &worker,
                made(legacy, &["BUILDKIT_SETUP_CGROUPV2_ROOT=1"]),
                true,
            ),
            (
                "earlier helper",
                &helper,
                made(Some(crate::railpack::IMAGE), &[]),
                true,
            ),
            (
                "no image",
                &worker,
                made(None, &["PLOYZ_BUILD_OWNER=1000"]),
                false,
            ),
            (
                "a second, conflicting marker",
                &worker,
                made(
                    pinned,
                    &["PLOYZ_BUILD_OWNER=1000x", "PLOYZ_BUILD_OWNER=1000"],
                ),
                false,
            ),
            (
                "the same marker twice",
                &worker,
                made(
                    pinned,
                    &["PLOYZ_BUILD_OWNER=1000", "PLOYZ_BUILD_OWNER=1000"],
                ),
                false,
            ),
            (
                "earlier worker with another user's marker",
                &worker,
                made(
                    legacy,
                    &["BUILDKIT_SETUP_CGROUPV2_ROOT=1", "PLOYZ_BUILD_OWNER=1000x"],
                ),
                false,
            ),
            (
                "earlier helper with an empty marker",
                &helper,
                made(Some(crate::railpack::IMAGE), &["PLOYZ_BUILD_OWNER"]),
                false,
            ),
            (
                "another user's worker",
                &worker,
                made(pinned, &["PLOYZ_BUILD_OWNER=1001"]),
                false,
            ),
            (
                "marked helper from the worker's image",
                &helper,
                made(pinned, &["PLOYZ_BUILD_OWNER=1000"]),
                false,
            ),
            (
                "unmarked worker Buildx never started",
                &worker,
                made(legacy, &[]),
                false,
            ),
        ] {
            assert_eq!(made_by_ployz(name, &made, "1000"), ployzs, "{case}");
        }
    }

    /// Docker fails once while the daemon starts: the slot is left for the
    /// next admission to recover, not held by the running daemon.
    #[test]
    fn a_failed_startup_look_leaves_the_slot_for_the_next_admission_to_recover() {
        let directory = host_slot();
        let directory = directory.path();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        fs::write(directory.join("docker-down"), "").unwrap();
        assert!(Lock::cleanup_abandoned(&policy).is_err());
        fs::remove_file(directory.join("docker-down")).unwrap();
        let admission = crate::Admission::try_acquire_with(&policy);
        assert!(admission.is_ok(), "{:?}", admission.err());
    }

    /// Two slots recover at once in one state directory, each reading only
    /// what its own Docker calls printed.
    #[test]
    fn concurrent_recoveries_read_only_their_own_docker_output() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::set_permissions(
            root,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let [first, second] = [20, 21].map(|slot| container(&Access::Full.builder(slot)));
        // The first slot's worker look prints, then waits until the second
        // slot's look has printed too.
        crate::tests::executable(
            &root.join("docker"),
            &format!(
                r#"#!/bin/sh
[ "$1" = --ready ] && exit 0
printf '%s\n' "$*" >> '{root}/calls'
{made}if [ "$1 $2" = 'inspect --type' ]; then
  case "$6" in
    {first}) made_by_ployz "$6"; : > '{root}/first-printed'
      for _ in $(seq 500); do [ -f '{root}/second-printed' ] && break; sleep 0.01; done ;;
    {second}) for _ in $(seq 500); do [ -f '{root}/first-printed' ] && break; sleep 0.01; done
      made_by_ployz "$6"; : > '{root}/second-printed' ;;
    *) echo "Error: No such container: $6" >&2; exit 1 ;;
  esac
fi
exit 0
"#,
                root = root.display(),
                made = crate::tests::made_by_ployz_sh(),
            ),
        );
        let state = root.join("state");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        for slot in [20, 21] {
            let mut gone = sleeper(&[]);
            let docker_process = crate::owner::Process::observe(gone.id()).unwrap().unwrap();
            gone.kill().unwrap();
            gone.wait().unwrap();
            fs::write(
                state.join(lock_file(slot)),
                format!(
                    "{}{}",
                    crate::owner::named(&[]),
                    crate::owner::Mark::docker_line(docker_process, crate::owner::DEFAULT_ENDPOINT)
                ),
            )
            .unwrap();
        }
        let docker = root.join("docker");
        std::thread::scope(|scope| {
            for slot in [20, 21] {
                let (state, docker) = (&state, &docker);
                scope.spawn(move || drop(Lock::try_acquire_slot(state, slot, docker).unwrap()));
            }
        });
        let calls = calls_since(root, &mut 0);
        let removed = calls
            .iter()
            .filter_map(|call| call.strip_prefix("rm --force "))
            .collect::<Vec<_>>();
        let mut expected = vec![format!("id-{first}"), format!("id-{second}")];
        expected.sort();
        let mut removed = removed.into_iter().map(str::to_owned).collect::<Vec<_>>();
        removed.sort();
        assert_eq!(removed, expected, "{calls:?}");
    }

    /// A quarantined slot counts against the limit even when a lower slot is
    /// free, and the next admission retries it first.
    #[test]
    fn a_free_lower_slot_never_hides_a_quarantined_one() {
        let directory = host_slot();
        let directory = directory.path();
        let foreign = container(&Access::Full.builder(0));
        let worker = container(&Access::Full.builder(1));
        fs::write(directory.join("names"), &foreign).unwrap();
        fs::write(directory.join("foreign"), &foreign).unwrap();
        let policy = crate::HostPolicy {
            state_directory: directory.join("state"),
            configuration_file: directory.join("build.yaml"),
            docker: directory.join("docker"),
            ..Default::default()
        };
        let one = ployz_core::BuildConcurrency::ONE;
        let admission = crate::Admission::try_acquire_within(&policy, one).unwrap();
        assert_eq!(admission.slot(), 1);
        drop(admission);
        // Slot 1's Build died, leaving its worker running, and it won't stop.
        let mut gone = sleeper(&[]);
        let docker_process = crate::owner::Process::observe(gone.id()).unwrap().unwrap();
        gone.kill().unwrap();
        gone.wait().unwrap();
        fs::write(
            policy.state_directory.join(lock_file(1)),
            format!(
                "{}{}",
                crate::owner::named(&[]),
                crate::owner::Mark::docker_line(docker_process, crate::owner::DEFAULT_ENDPOINT)
            ),
        )
        .unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        fs::write(directory.join("fail-stop"), "").unwrap();
        fs::write(directory.join("running"), "true").unwrap();
        // The other container is renamed away, freeing slot 0.
        fs::remove_file(directory.join("foreign")).unwrap();
        fs::remove_file(directory.join("names")).unwrap();
        let mut seen = 0;
        calls_since(directory, &mut seen);

        let refused = crate::Admission::try_acquire_within(&policy, one);
        assert!(
            refused.as_ref().is_err_and(BuildError::is_unknown),
            "a Build started while slot 1's worker runs: slot {:?}",
            refused.map(|admission| admission.slot())
        );
        assert!(
            calls_since(directory, &mut seen)
                .contains(&format!("stop --time {WORKER_STOP_SECONDS} id-{worker}")),
            "slot 1 was not retried"
        );

        // The worker stops now: admission recovers slot 1 and admits.
        fs::remove_file(directory.join("fail-stop")).unwrap();
        let admission = crate::Admission::try_acquire_within(&policy, one);
        assert!(admission.is_ok(), "{:?}", admission.err());
        assert!(
            fs::read(policy.state_directory.join(lock_file(1)))
                .unwrap()
                .is_empty()
        );
    }

    /// A slot counts from the moment admission takes it, through its upload,
    /// before its Build marks it.
    #[test]
    fn a_slot_held_through_its_upload_counts_against_the_limit() {
        let directory = host_slot();
        let directory = directory.path();
        let two = "2".parse::<ployz_core::BuildConcurrency>().unwrap();
        // Slot 3's Build died, leaving its worker running, and it won't stop.
        let policy = crashed_slot(directory);
        fs::rename(
            policy.state_directory.join(lock_file(0)),
            policy.state_directory.join(lock_file(3)),
        )
        .unwrap();
        fs::write(directory.join("owned"), container(&Access::Full.builder(3))).unwrap();
        fs::write(directory.join("fail-stop"), "").unwrap();
        fs::write(directory.join("running"), "true").unwrap();
        // Containers Ployz did not create hold slots 0 and 1's worker names.
        let foreign = [0, 1].map(|slot| container(&Access::Full.builder(slot)));
        fs::write(directory.join("foreign"), foreign.join("\n")).unwrap();
        fs::write(directory.join("names"), foreign.join("\n")).unwrap();

        // A Build takes slot 2 and uploads, with no mark yet.
        let uploading = crate::Admission::try_acquire_within(&policy, two).unwrap();
        assert_eq!(uploading.slot(), 2);
        assert!(!Lock::marked_in(&policy.state_directory, 2));

        // The other containers go, freeing slots 0 and 1.
        fs::remove_file(directory.join("foreign")).unwrap();
        fs::remove_file(directory.join("names")).unwrap();
        let third = crate::Admission::try_acquire_within(&policy, two);
        assert!(
            third.is_err(),
            "a third Build started at limit 2: slot {:?}",
            third.map(|admission| admission.slot())
        );

        // The upload ends: its slot frees and the next Build is admitted.
        drop(uploading);
        assert!(crate::Admission::try_acquire_within(&policy, two).is_ok());
    }

    /// Recovery stops a worker before removing it, and leaves one that
    /// fails to stop and still runs, with the slot marked.
    #[test]
    fn recovery_stops_a_worker_before_removing_it_and_leaves_one_that_will_not_stop() {
        let directory = host_slot();
        let directory = directory.path();
        let worker = container(&Access::Full.builder(0));
        fs::write(directory.join("owned"), &worker).unwrap();
        fs::write(directory.join("fail-stop"), "").unwrap();
        fs::write(directory.join("running"), "true").unwrap();
        let policy = crashed_slot(directory);
        let refused = Lock::try_acquire_slot(&policy.state_directory, 0, &policy.docker);
        assert!(
            refused.as_ref().is_err_and(BuildError::is_unknown),
            "{:?}",
            refused.err()
        );
        let calls = calls_since(directory, &mut 0);
        assert!(
            calls.contains(&format!("stop --time {WORKER_STOP_SECONDS} id-{worker}")),
            "{calls:?}"
        );
        assert!(
            !calls.contains(&format!("rm --force id-{worker}")),
            "a running worker was force-removed: {calls:?}"
        );
        assert!(
            !fs::read_to_string(policy.state_directory.join(lock_file(0)))
                .unwrap()
                .is_empty(),
            "the slot's mark cleared while its worker runs"
        );

        // The worker stops on the next try.
        fs::remove_file(directory.join("fail-stop")).unwrap();
        drop(Lock::try_acquire_slot(&policy.state_directory, 0, &policy.docker).unwrap());
        let calls = calls_since(directory, &mut 0);
        let stop = calls
            .iter()
            .position(|call| *call == format!("stop --time {WORKER_STOP_SECONDS} id-{worker}"));
        let remove = calls
            .iter()
            .position(|call| *call == format!("rm --force id-{worker}"));
        assert!(
            stop.is_some() && remove.is_some() && stop < remove,
            "{calls:?}"
        );
    }

    /// A worker that fails to stop but has stopped is removed; one already
    /// gone needs nothing, and either way the slot recovers.
    #[test]
    fn recovery_removes_a_worker_that_stopped_anyway_and_skips_one_already_gone() {
        for (state, removed) in [(Some("false"), true), (None, false)] {
            let directory = host_slot();
            let directory = directory.path();
            let worker = container(&Access::Full.builder(0));
            fs::write(directory.join("owned"), &worker).unwrap();
            fs::write(directory.join("fail-stop"), "").unwrap();
            if let Some(state) = state {
                fs::write(directory.join("running"), state).unwrap();
            }
            let policy = crashed_slot(directory);
            let recovered = Lock::try_acquire_slot(&policy.state_directory, 0, &policy.docker);
            assert!(recovered.is_ok(), "{state:?}: {:?}", recovered.err());
            assert_eq!(
                calls_since(directory, &mut 0).contains(&format!("rm --force id-{worker}")),
                removed,
                "{state:?}"
            );
        }
    }

    /// A Build's teardown leaves a worker that fails to stop and still runs,
    /// keeping the slot marked.
    #[test]
    fn a_released_worker_that_will_not_stop_is_not_force_removed() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let worker = container(&crate::builder_name());
        let builder = Builder::acquire(
            &docker,
            Lock::try_acquire_in(directory).unwrap(),
            Access::Full,
            &crate::policy::Resources::default(),
            None,
            Reuse::WhenClean,
        )
        .unwrap();
        fs::write(directory.join("owned"), &worker).unwrap();
        fs::write(directory.join("fail-stop"), "").unwrap();
        fs::write(directory.join("running"), "true").unwrap();
        let mut seen = 0;
        calls_since(directory, &mut seen);
        let error = builder
            .finish::<()>(Err(BuildError::Result("failed".into())))
            .unwrap_err();
        assert!(error.is_unknown(), "{error}");
        let calls = calls_since(directory, &mut seen);
        assert!(
            !calls.contains(&format!("rm --force id-{worker}")),
            "a running worker was force-removed: {calls:?}"
        );
        assert!(!fs::read(directory.join(lock_file(0))).unwrap().is_empty());

        // The worker stops now: the next admission in this process retries.
        fs::remove_file(directory.join("fail-stop")).unwrap();
        let retried = Lock::try_acquire_slot(directory, 0, &program);
        assert!(retried.is_ok(), "{:?}", retried.err());
        assert!(
            calls_since(directory, &mut seen).contains(&format!("rm --force id-{worker}")),
            "the retry did not remove the worker"
        );
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());
    }

    #[test]
    fn a_replaced_worker_that_will_not_stop_leaves_its_slot_to_the_next_admission() {
        let directory = host_slot();
        let directory = directory.path();
        let program = directory.join("docker");
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
            recorder: None,
        };
        let attempt = |access| {
            Builder::acquire(
                &docker,
                Lock::try_acquire_in(directory).unwrap(),
                access,
                &crate::policy::Resources::default(),
                None,
                Reuse::WhenClean,
            )
        };
        // A Full Build keeps its worker.
        let full = container(&Access::Full.builder(0));
        let kept = attempt(Access::Full).unwrap();
        fs::write(directory.join("owned"), &full).unwrap();
        fs::write(directory.join("worker"), format!("id-{full} true\n")).unwrap();
        kept.take_worker().unwrap();
        kept.finish(Ok(())).unwrap();

        // A Free Build replaces it, and the Full worker will not stop.
        fs::write(directory.join("fail-stop"), "").unwrap();
        fs::write(directory.join("running"), "true").unwrap();
        let error = attempt(Access::Free).err().expect("the replacement failed");
        assert!(error.is_unknown(), "{error}");
        let mark = fs::read_to_string(directory.join(lock_file(0))).unwrap();
        assert!(
            !mark.is_empty(),
            "the slot was released with the worker running"
        );
        assert!(
            !mark.lines().any(|line| line.starts_with("owner ")),
            "the slot stays owned by a Build that returned: {mark:?}"
        );

        // Docker can stop the Full worker now: the next admission recovers.
        fs::remove_file(directory.join("fail-stop")).unwrap();
        let mut seen = 0;
        calls_since(directory, &mut seen);
        let retried = Lock::try_acquire_slot(directory, 0, &program);
        assert!(retried.is_ok(), "{:?}", retried.err());
        assert!(
            calls_since(directory, &mut seen).contains(&format!("rm --force id-{full}")),
            "the retry did not remove the Full worker"
        );
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());
    }

    #[test]
    fn a_failed_build_reports_the_failing_step_and_its_exit_code() {
        let directory = std::env::temp_dir().join(format!("ployz-step-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let program = directory.join("docker");
        crate::tests::executable(
            &program,
            r#"#!/bin/sh
case "$1" in --ready) exit 0 ;; esac
echo '{"vertexes":[{"digest":"sha256:a","name":"npm run build","started":"2026-10-18T15:09:30Z","completed":"2026-10-18T15:09:31Z","error":"process \"npm run build\" did not complete successfully: exit code: 3"}]}'
echo '{"vertexes":[{"digest":"sha256:b","name":"copy /mise/installs","started":"2026-10-18T15:09:30Z","completed":"2026-10-18T15:09:31Z","error":"context canceled: context canceled"}]}'
echo 'ERROR: failed to solve: process "npm run build" did not complete successfully: exit code: 3' >&2
exit 1
"#,
        );
        let environment = BTreeMap::new();
        let progress = |_| {};
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: &directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: Some(&progress),
            recorder: None,
        };
        let builder = Builder {
            access: Access::Full,
            docker: &docker,
            name: crate::builder_name(),
            lock: None,
            bridge: None,
            fence: None,
            reuse: Reuse::Never,
            spec: String::new(),
            worker: std::sync::OnceLock::new(),
        };
        let error = builder.run(&[], false, || {}).unwrap_err();
        assert_eq!(
            error.to_string(),
            r#"npm run build: process "npm run build" did not complete successfully: exit code: 3"#
        );
        assert_eq!(error.stage(), crate::Stage::Building);
        drop(builder);
        fs::remove_dir_all(directory).unwrap();
    }
}
