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
}

/// Whether an attempt may start in the builder its slot's last attempt kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reuse {
    Never,
    WhenClean,
}

/// The builder a clean attempt left running, recorded beside its slot's lock.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Kept {
    builder: String,
    container: String,
    bridge: u8,
    spec: String,
}

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
        if let Some(kept) = self.lock.as_ref().expect("owned lock").take_kept()? {
            if let Some(bridge) = self.reusable(&kept, resources)? {
                self.bridge = Some(bridge);
                self.spec = kept.spec;
                return create(self.docker, &self.name, kept.bridge, resources);
            }
            discard(self.docker, self.slot())?;
        }
        remove(&self.docker.releasing(), &self.name)?;
        let bridge = Bridge::claim(&bridge_directory(), NETWORK_INTERFACES)?;
        let index = bridge.index;
        self.bridge = Some(bridge);
        self.spec = spec(resources, index);
        create(self.docker, &self.name, index, resources)
    }

    fn reusable(
        &self,
        kept: &Kept,
        resources: &crate::policy::Resources,
    ) -> Result<Option<Bridge>, BuildError> {
        if self.reuse == Reuse::Never
            || kept.builder != self.name
            || kept.spec != spec(resources, kept.bridge)
            || self.running().as_ref() != Some(&kept.container)
        {
            return Ok(None);
        }
        Bridge::claim_index(&bridge_directory(), kept.bridge)
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

    /// Leave the worker running for the slot's next attempt.
    fn keep(&self) -> Result<(), BuildError> {
        let container = self.running().ok_or_else(|| {
            BuildError::Prerequisite("the BuildKit worker stopped before it could be kept".into())
        })?;
        let bridge = self.bridge.as_ref().expect("a provisioned builder holds its bridge");
        self.lock.as_ref().expect("owned lock").keep(&Kept {
            builder: self.name.clone(),
            container,
            bridge: bridge.index,
            spec: self.spec.clone(),
        })
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
        let cancelled = || {
            self.docker
                .cancellation
                .is_some_and(crate::Cancellation::is_cancelled)
        };
        let cleanup = if self.reuse == Reuse::WhenClean && result.is_ok() && !cancelled() {
            self.keep()
                .or_else(|_| release(&self.docker.releasing(), &self.name))
        } else {
            release(&self.docker.releasing(), &self.name)
        };
        let stage = result
            .as_ref()
            .err()
            .map_or(crate::Stage::Cleanup, BuildError::stage);
        let cleared = if cleanup.is_ok() && !result.as_ref().is_err_and(|error| error.is_unknown())
        {
            self.lock.as_mut().expect("owned lock").clear()
        } else {
            Ok(())
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
                if cancelled() {
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
        if let Some(lock) = self.lock.as_mut()
            && release(&self.docker.releasing(), &self.name).is_ok()
        {
            let _ = lock.clear();
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

/// Remove the builder a slot's last attempt kept, whichever access it ran with.
fn discard(docker: &Docker<'_>, slot: usize) -> Result<(), BuildError> {
    [Access::Full, Access::Free].iter().try_for_each(|access| {
        let container = container(&access.builder(slot));
        // A graceful stop lets BuildKit delete its bridge; a forced removal leaves it behind.
        let _ = docker.releasing().run(
            "stop the kept build container",
            &["stop", &container],
            Streams::Captured,
        );
        docker.remove_container(&container)
    })
}

/// Remove this attempt's builder, including a container it started in
/// without registering, so an unmarked slot keeps none but a recorded one.
fn release(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    remove(docker, name)?;
    docker.remove_container(&container(name))
}

fn container(builder: &str) -> String {
    format!("buildx_buildkit_{builder}0")
}

fn remove(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    match docker.run(
        "remove the build container",
        &["buildx", "rm", "--keep-state", name],
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
    pub(crate) fn known_slots(directory: &std::path::Path) -> impl Iterator<Item = usize> + '_ {
        (0..=usize::from(u8::MAX))
            .filter(|&slot| slot == 0 || directory.join(lock_file(slot)).exists())
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

    fn kept_file(&self) -> PathBuf {
        self.directory.join(format!("{}.kept", slot_name(self.slot)))
    }

    /// Remove the slot's record of a kept builder. Only a marked slot may
    /// take it: until the builder it names is started in or removed, that
    /// builder is unaccounted for.
    fn take_kept(&self) -> Result<Option<Kept>, BuildError> {
        let path = self.kept_file();
        let record = match fs::read(&path) {
            Ok(record) => record,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(lock_error(error)),
        };
        fs::remove_file(&path).map_err(lock_error)?;
        // An unreadable record still means a builder may be running.
        Ok(Some(serde_json::from_slice(&record).unwrap_or_default()))
    }

    fn keep(&self, kept: &Kept) -> Result<(), BuildError> {
        use std::io::Write as _;
        let path = self.kept_file();
        let partial = path.with_extension("kept.partial");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&partial)
            .map_err(lock_error)?;
        file.write_all(&serde_json::to_vec(kept).expect("a record serializes"))
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&partial, &path))
            .and_then(|()| fs::File::open(&self.directory)?.sync_all())
            .map_err(lock_error)
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
                .flat_map(|name| {
                    std::iter::once(container(name))
                        .chain(HELPERS.iter().map(move |helper| format!("{name}-{helper}")))
                })
                .try_for_each(|container| docker.remove_container(&container))
                .map_err(|error| {
                    quarantined(format!(
                        "none of its processes is running, but removing its builder from {endpoint} failed: {error}"
                    ))
                })?;
        }
        self.take_kept()?;
        self.clear()
    }

    /// One bounded recovery of every abandoned builder slot on daemon startup.
    /// A slot whose last attempt may still run stays quarantined. Every slot is
    /// attempted; the first failure is reported.
    pub(crate) fn cleanup_abandoned(policy: &crate::HostPolicy) -> Result<(), BuildError> {
        Self::known_slots(&policy.state_directory)
            .map(|slot| Self::cleanup_abandoned_slot(policy, slot))
            .fold(Ok(()), Result::and)
    }

    fn cleanup_abandoned_slot(policy: &crate::HostPolicy, slot: usize) -> Result<(), BuildError> {
        let mut lock = Self::try_acquire_slot(&policy.state_directory, slot, &policy.docker)?;
        crate::upload::remove_abandoned(&lock.upload_directory()).map_err(|error| {
            BuildError::Prerequisite(format!("remove abandoned Build upload: {error}"))
        })?;
        if !lock.kept_file().exists() {
            return Ok(());
        }
        lock.quarantine()?;
        lock.take_kept()?;
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
        Ok(Self {
            file: std::sync::Arc::new(LockedFile {
                file,
                writes: std::sync::Mutex::default(),
            }),
            directory: directory.to_owned(),
            slot,
        })
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
        prepare_claims(directory)?;
        let mut unusable = None;
        for index in 0..=u8::MAX {
            let file = match claim_file(directory, index) {
                Ok(file) => file,
                // Another user's private claim file, or anything else at the
                // path, takes that index out of use rather than every build.
                Err(error) => {
                    unusable = Some(error);
                    continue;
                }
            };
            if !hold_claim(&file)? {
                continue;
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

    /// Claim the bridge a kept builder still runs on, or nothing when another
    /// builder holds it.
    fn claim_index(directory: &std::path::Path, index: u8) -> Result<Option<Self>, BuildError> {
        prepare_claims(directory)?;
        let file = claim_file(directory, index).map_err(bridge_error)?;
        Ok(hold_claim(&file)?.then_some(Self { _file: file, index }))
    }
}

fn prepare_claims(directory: &std::path::Path) -> Result<(), BuildError> {
    use std::os::unix::fs::PermissionsExt as _;
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
    Ok(())
}

fn claim_file(directory: &std::path::Path, index: u8) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o666)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(directory.join(format!("{index}.lock")))
}

/// Whether this process now holds the claim.
fn hold_claim(file: &fs::File) -> Result<bool, BuildError> {
    use rustix::fs::{FlockOperation, flock};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = file.metadata().map_err(bridge_error)?;
    if metadata.uid() == rustix::process::getuid().as_raw() && metadata.mode() & 0o666 != 0o666 {
        file.set_permissions(fs::Permissions::from_mode(0o666))
            .map_err(bridge_error)?;
    }
    match flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(error) => Err(bridge_error(error.into())),
    }
}

pub(crate) fn bridge_name(index: u8) -> String {
    format!("{}{index}", crate::BUILD_BRIDGE_PREFIX)
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
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64","linux/386","linux/arm/v7","linux/ppc64le"]}}]}}' ;;
esac
"#,
                crate::builder_name()
            ),
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
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf '%s\\n' \"$*\" >> '{}/calls'\n",
                directory.path().display()
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
            format!("rm --force buildx_buildkit_{name}0"),
            format!("rm --force {name}-railpack"),
            format!("rm --force {name}-prepare"),
            format!("rm --force {name}-assemble"),
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
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf '%s %s\\n' \"$DOCKER_HOST\" \"$*\" >> '{}/calls'\n",
                directory.path().display()
            ),
        );
        drop(Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap());
        let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
        // The slot ran a Build or a Free Build: recovery can't tell which, so it removes both.
        for access in [Access::Full, Access::Free] {
            assert!(
                calls.lines().any(|call| call
                    == format!(
                        "unix:///run/user/7/docker.sock rm --force buildx_buildkit_{}0",
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
                    "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\n[ -e '{root}/docker-up' ] || {{ echo 'Cannot connect to the Docker daemon' >&2; exit 1; }}\nprintf '%s %s\\n' \"$DOCKER_HOST\" \"$*\" >> '{root}/calls'\n",
                    root = directory.path().display()
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
                    == format!("{endpoint} rm --force buildx_buildkit_{}0", slot_name(slot))),
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

    /// A slot directory whose fake Docker logs each call and reports the
    /// worker container as `<directory>/worker` says.
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
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf '%s\\n' \"$*\" >> '{0}/calls'\n[ \"$1\" = inspect ] && cat '{0}/worker' 2>/dev/null\nexit 0\n",
                directory.path().display()
            ),
        );
        directory
    }

    fn calls_since(directory: &std::path::Path, seen: &mut usize) -> Vec<String> {
        let calls = fs::read_to_string(directory.join("calls")).unwrap_or_default();
        let lines = calls.lines().skip(*seen).map(str::to_owned).collect::<Vec<_>>();
        *seen += lines.len();
        lines
    }

    fn create_call(directory: &std::path::Path, name: &str) -> String {
        format!(
            "buildx create --name {name} --driver docker-container --driver-opt image={BUILDKIT_IMAGE} --driver-opt network=host --driver-opt restart-policy=no --buildkitd-config {}",
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
        let worker = format!("inspect --format {{{{.Id}}}} {{{{.State.Running}}}} {}", container(&name));
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
            [format!("buildx rm --keep-state {name}"), create_call(directory, &name)]
        );
        fs::write(directory.join("worker"), "c0ffee true\n").unwrap();
        first.finish(Ok(())).unwrap();
        assert_eq!(calls_since(directory, &mut seen), [worker.clone()]);
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());

        let second = attempt();
        assert_eq!(
            calls_since(directory, &mut seen),
            [worker.clone(), create_call(directory, &name)],
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
                format!("buildx rm --keep-state {name}"),
                format!("rm --force {}", container(&name)),
            ],
            "teardown keeps the cache volume"
        );
        assert!(!directory.join(format!("{name}.kept")).exists());
        assert!(fs::read(directory.join(lock_file(0))).unwrap().is_empty());
    }

    #[test]
    fn a_kept_builder_that_differs_from_this_attempt_is_removed_before_it_starts() {
        let resources = crate::policy::Resources::default();
        let name = crate::builder_name();
        let free = Access::Free.builder(0);
        let current = spec(&resources, 9);
        for (case, kept_spec, worker, reuse) in [
            ("policy changed", "an older policy", "c0ffee true", Reuse::WhenClean),
            ("worker replaced", current.as_str(), "decaf true", Reuse::WhenClean),
            ("worker stopped", current.as_str(), "c0ffee false", Reuse::WhenClean),
            ("never reused", current.as_str(), "c0ffee true", Reuse::Never),
        ] {
            let directory = host_slot();
            let directory = directory.path();
            let program = directory.join("docker");
            fs::write(directory.join("worker"), worker).unwrap();
            fs::write(
                directory.join(format!("{name}.kept")),
                serde_json::to_vec(&Kept {
                    builder: name.clone(),
                    container: "c0ffee".into(),
                    bridge: 9,
                    spec: kept_spec.into(),
                })
                .unwrap(),
            )
            .unwrap();
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
                &resources,
                None,
                reuse,
            )
            .unwrap();
            let calls = calls_since(directory, &mut 0);
            let removed = [
                format!("stop {}", container(&name)),
                format!("rm --force {}", container(&name)),
                format!("stop {}", container(&free)),
                format!("rm --force {}", container(&free)),
                format!("buildx rm --keep-state {name}"),
                create_call(directory, &name),
            ];
            assert_eq!(calls[calls.len() - removed.len()..], removed, "{case}");
            assert!(!directory.join(format!("{name}.kept")).exists(), "{case}");
            drop(builder);
        }
    }

    #[test]
    fn an_attempt_that_never_confirmed_its_end_leaves_its_kept_builder_to_recovery() {
        let slot = 0;
        let (directory, docker) = quarantined_slot(slot, &crate::owner::named(&[]));
        let name = Access::Full.builder(slot);
        fs::write(
            directory.path().join(format!("{name}.kept")),
            serde_json::to_vec(&Kept {
                builder: name.clone(),
                container: "c0ffee".into(),
                bridge: 9,
                spec: spec(&crate::policy::Resources::default(), 9),
            })
            .unwrap(),
        )
        .unwrap();
        let lock = Lock::try_acquire_slot(directory.path(), slot, &docker).unwrap();
        assert!(!directory.path().join(format!("{name}.kept")).exists());
        let calls = fs::read_to_string(directory.path().join("calls")).unwrap();
        assert!(
            calls.contains(&format!("rm --force {}", container(&name))),
            "{calls}"
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
        assert_eq!(
            calls_since(directory.path(), &mut 0),
            [
                format!("buildx rm --keep-state {name}"),
                create_call(directory.path(), &name),
            ]
        );
        drop(builder);
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
