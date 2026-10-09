use std::{
    fs, io,
    net::SocketAddrV4,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::StreamExt;
use hickory_server::Server;
use ployz_core::{ContainerObservation, MachineId};
use tokio::signal::unix::{SignalKind, signal};
use tokio_stream::wrappers::IntervalStream;
use tokio_util::sync::CancellationToken;

use super::{
    Answers, Handler, ProjectionInputs, Upstreams,
    listeners::{Keeper, Listeners, SystemdKeeper, adopt, names},
    load_down_machines, run_server,
    service::{Probe, Successor},
    spec::{CorrosionEndpoint, DnsSpec, SpecFile},
};
use crate::corrosion::{
    AdminClient, ApiClient, Error as CorrosionError, MachineView, ReplicatedStore, Subscription,
};

/// Exit status for "the installed binary cannot serve DNS; port 53 was handed
/// back". The unit has `RestartPreventExitStatus=78` and `SuccessExitStatus=78`,
/// so systemd neither restarts the process nor reports a failure.
pub const ABDICATED: u8 = 78;

const TICK: Duration = Duration::from_secs(1);
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(2);

/// Why the process ended. `main.rs` maps it to an exit status.
#[derive(Debug, PartialEq, Eq)]
pub enum DnsExit {
    /// SIGTERM or SIGINT. The sockets stay in the fd store.
    Stopped,
    /// The published spec differs from the one this process serves, or vanished.
    SpecChanged,
    /// The binary installed at this process's path fails `dns --probe`. The
    /// sockets were forgotten so the next owner of port 53 can bind.
    Abdicated,
}

/// Serve Internal DNS for whatever `{run_dir}/dns.json` says until a signal
/// arrives or the spec changes.
///
/// # Errors
///
/// Returns an I/O error only for failures a restart can fix: inherited-fd
/// classification, an unreadable spec, a poisoned lock, or the DNS server
/// itself failing. Corrosion being down, the token being missing, and port 53
/// being busy are retried in-process with a status line.
pub async fn serve(run_dir: &Path) -> io::Result<DnsExit> {
    let shutdown = CancellationToken::new();
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let signalled = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        signalled.cancel();
    });
    serve_with(
        Fixture {
            spec: SpecFile::in_run_dir(run_dir),
            keeper: &SystemdKeeper,
            resolv_conf: PathBuf::from("/etc/resolv.conf"),
            exe: InstalledExe::current()?,
            tick: TICK,
            backoff: Backoff::default(),
        },
        shutdown,
    )
    .await
}

pub(crate) struct Fixture<'a> {
    pub(crate) spec: SpecFile,
    pub(crate) keeper: &'a dyn Keeper,
    pub(crate) resolv_conf: PathBuf,
    pub(crate) exe: InstalledExe,
    pub(crate) tick: Duration,
    pub(crate) backoff: Backoff,
}

#[derive(Clone, Copy)]
pub(crate) struct Backoff {
    pub(crate) min: Duration,
    pub(crate) max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) fn between(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            next: min,
        }
    }

    fn take(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(self.max);
        delay
    }

    fn reset(&mut self) {
        self.next = self.min;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::between(BACKOFF_MIN, BACKOFF_MAX)
    }
}

pub(crate) async fn serve_with(
    fixture: Fixture<'_>,
    shutdown: CancellationToken,
) -> io::Result<DnsExit> {
    let inherited = fixture.keeper.inherited()?;
    let Some(spec) = fixture.spec.read()? else {
        fixture.keeper.forget(&names(&inherited))?;
        drop(inherited);
        fixture.keeper.ready("idle")?;
        return idle_until_spec(&fixture, &shutdown).await;
    };
    let adopted = adopt(inherited, spec.listen);
    fixture.keeper.forget(&names(&adopted.stale))?;
    // A rejected half pair still holds the address; the rebind below needs it released.
    drop(adopted.stale);
    let listeners = match adopted.listeners {
        Some(listeners) => listeners,
        None => match bind_until_free(spec.listen, &fixture, &shutdown).await? {
            Some(listeners) => listeners,
            None => return Ok(DnsExit::Stopped),
        },
    };
    fixture.keeper.keep(&listeners)?;
    fixture.keeper.ready("loading")?;

    let upstreams = Upstreams::of(&spec.upstreams, &fixture.resolv_conf, *spec.listen.ip());
    let mut tick = Tick {
        spec: &fixture.spec,
        current: &spec,
        exe: fixture.exe,
        upstreams: upstreams.clone(),
        every: fixture.tick,
    };
    let mut session = Session::new(spec.machine, spec.corrosion.clone(), fixture.backoff);
    let (answers, connection) = tokio::select! {
        loaded = session.first_load(spec.local_subnet, upstreams, fixture.keeper, &shutdown) => match loaded? {
            Some(loaded) => loaded,
            None => return Ok(DnsExit::Stopped),
        },
        exit = tick.until_exit(fixture.keeper, &listeners) => return Ok(exit),
    };

    let handler = Handler::new(answers.clone());
    let in_flight = handler.in_flight();
    let mut server = Server::new(handler);
    listeners.register(&mut server)?;
    fixture.keeper.status(&format!("serving {}", spec.listen))?;

    let serving = shutdown.child_token();
    let server = run_server(server, in_flight, serving.clone());
    tokio::pin!(server);
    let exit = tokio::select! {
        served = &mut server => return served.map(|()| DnsExit::Stopped),
        followed = session.follow(&answers, connection, fixture.tick, fixture.keeper, &shutdown) => {
            followed?;
            DnsExit::Stopped
        }
        exit = tick.until_exit(fixture.keeper, &listeners) => exit,
    };
    serving.cancel();
    server.await?;
    Ok(exit)
}

struct Session {
    endpoint: CorrosionEndpoint,
    inputs: ProjectionInputs,
    backoff: Backoff,
}

struct Connection {
    replicated: ReplicatedStore,
    changes: Subscription,
    machines: MachineView,
    admin: AdminClient,
    token: String,
    view_shutdown: CancellationToken,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.view_shutdown.cancel();
    }
}

enum Attempt {
    NoToken,
    Corrosion(CorrosionError),
}

impl From<CorrosionError> for Attempt {
    fn from(error: CorrosionError) -> Self {
        Self::Corrosion(error)
    }
}

impl Session {
    fn new(machine: MachineId, endpoint: CorrosionEndpoint, backoff: Backoff) -> Self {
        Self {
            endpoint,
            inputs: ProjectionInputs {
                local_id: machine,
                observations: Vec::new(),
                down_machines: None,
            },
            backoff,
        }
    }

    async fn first_load(
        &mut self,
        local_subnet: ipnet::Ipv4Net,
        upstreams: Upstreams,
        keeper: &dyn Keeper,
        shutdown: &CancellationToken,
    ) -> io::Result<Option<(Answers, Connection)>> {
        loop {
            match self.connect().await {
                Ok((connection, snapshot)) => {
                    self.inputs.observations = snapshot;
                    let answers = Answers::loaded(self.inputs.build(), local_subnet, upstreams);
                    return Ok(Some((answers, connection)));
                }
                Err(Attempt::NoToken) => keeper.status("waiting for the Corrosion token")?,
                Err(Attempt::Corrosion(error)) => {
                    keeper.status("loading: Corrosion unavailable")?;
                    eprintln!(
                        "Internal DNS cannot load from Corrosion yet: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                }
            }
            if !self.pause(shutdown).await {
                return Ok(None);
            }
        }
    }

    async fn follow(
        &mut self,
        answers: &Answers,
        mut connection: Connection,
        tick: Duration,
        keeper: &dyn Keeper,
        shutdown: &CancellationToken,
    ) -> io::Result<()> {
        loop {
            match watch_projection(
                &mut connection,
                answers,
                &mut self.inputs,
                &self.endpoint.token_file,
                tick,
                shutdown,
            )
            .await?
            {
                Watched::Shutdown => return Ok(()),
                Watched::Lost(lost) => {
                    keeper.status("reconnecting to Corrosion")?;
                    eprintln!("Internal DNS keeps its last answers while reconnecting: {lost}");
                }
            }
            drop(connection);
            connection = loop {
                if !self.pause(shutdown).await {
                    return Ok(());
                }
                match self.connect().await {
                    Ok((connection, snapshot)) => {
                        self.inputs.observations = snapshot;
                        answers.replace(self.inputs.build())?;
                        keeper.status("serving after reconnecting to Corrosion")?;
                        self.backoff.reset();
                        break connection;
                    }
                    Err(Attempt::NoToken) => {
                        keeper.status("reconnecting: waiting for the Corrosion token")?;
                    }
                    Err(Attempt::Corrosion(_)) => {}
                }
            };
        }
    }

    async fn connect(&self) -> Result<(Connection, Vec<ContainerObservation>), Attempt> {
        let token = read_token(&self.endpoint.token_file).map_err(|_| Attempt::NoToken)?;
        let replicated = ReplicatedStore::new(ApiClient::new(self.endpoint.api, &token)?);
        let (changes, snapshot) = subscribe_then_snapshot(&replicated).await?;
        let view_shutdown = CancellationToken::new();
        let machines = MachineView::start(replicated.clone(), view_shutdown.clone());
        let admin = AdminClient::new(&self.endpoint.admin_socket);
        Ok((
            Connection {
                replicated,
                changes,
                machines,
                admin,
                token,
                view_shutdown,
            },
            snapshot,
        ))
    }

    async fn pause(&mut self, shutdown: &CancellationToken) -> bool {
        tokio::select! {
            () = tokio::time::sleep(self.backoff.take()) => true,
            () = shutdown.cancelled() => false,
        }
    }
}

async fn subscribe_then_snapshot(
    replicated: &ReplicatedStore,
) -> Result<(Subscription, Vec<ContainerObservation>), CorrosionError> {
    let changes = replicated.subscribe_container_changes().await?;
    let snapshot = replicated.containers().await?.observations;
    Ok((changes, snapshot))
}

fn read_token(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

enum Watched {
    Shutdown,
    Lost(Lost),
}

#[derive(Debug, thiserror::Error)]
enum Lost {
    #[error("subscription ended: {0}")]
    Stream(CorrosionError),
    #[error("the Corrosion token changed")]
    TokenRotated,
}

async fn watch_projection(
    connection: &mut Connection,
    answers: &Answers,
    inputs: &mut ProjectionInputs,
    token_file: &Path,
    tick: Duration,
    shutdown: &CancellationToken,
) -> io::Result<Watched> {
    let Connection {
        replicated,
        changes,
        machines,
        admin,
        token,
        ..
    } = connection;
    let local_id = inputs.local_id;
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + tick, tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `Then` retains an in-flight membership read when another select branch wins, so slow
    // membership I/O never delays Container-change withdrawal.
    let membership =
        IntervalStream::new(interval).then(|_| load_down_machines(machines, admin, &local_id));
    tokio::pin!(membership);
    let mut pending_snapshot = false;
    loop {
        let rebuild = tokio::select! {
            changed = changes.changed() => {
                if let Err(error) = changed {
                    return Ok(Watched::Lost(Lost::Stream(error)));
                }
                pending_snapshot = true;
                reload_snapshot(replicated, inputs, &mut pending_snapshot).await
            }
            Some(result) = membership.next() => {
                if read_token(token_file).is_ok_and(|current| &current != token) {
                    return Ok(Watched::Lost(Lost::TokenRotated));
                }
                let membership = inputs.update_membership(result);
                let retried = pending_snapshot
                    && reload_snapshot(replicated, inputs, &mut pending_snapshot).await;
                membership || retried
            }
            () = shutdown.cancelled() => return Ok(Watched::Shutdown),
        };
        if rebuild {
            answers.replace(inputs.build())?;
        }
    }
}

async fn reload_snapshot(
    replicated: &ReplicatedStore,
    inputs: &mut ProjectionInputs,
    pending: &mut bool,
) -> bool {
    match replicated.containers().await {
        Ok(next) => {
            inputs.observations = next.observations;
            *pending = false;
            true
        }
        Err(error) => {
            eprintln!(
                "failed to rebuild the DNS projection; retrying on the next tick: {error}",
                error = ployz_core::error_chain::inline(&error),
            );
            false
        }
    }
}

struct Tick<'a> {
    spec: &'a SpecFile,
    current: &'a DnsSpec,
    exe: InstalledExe,
    upstreams: Upstreams,
    every: Duration,
}

impl Tick<'_> {
    async fn until_exit(&mut self, keeper: &dyn Keeper, listeners: &Listeners) -> DnsExit {
        loop {
            tokio::time::sleep(self.every).await;
            if let Ok(next) = self.spec.read()
                && next.as_ref() != Some(self.current)
            {
                return DnsExit::SpecChanged;
            }
            if let Some(identity) = self.exe.changed() {
                match Successor::at(self.exe.path()).probe().await {
                    Probe::CannotServe => {
                        if let Err(error) = keeper.forget(&listeners.names()) {
                            eprintln!(
                                "failed to forget the Internal DNS sockets while abdicating: {error}"
                            );
                        }
                        return DnsExit::Abdicated;
                    }
                    Probe::Serves => self.exe.identity = Some(identity),
                    Probe::Unknown => {}
                }
            }
            self.upstreams.reload_if_changed();
        }
    }
}

/// `/proc/self/exe` reads `(deleted)` after a swap, so the path is captured at
/// start and stat'ed afterwards.
pub(crate) struct InstalledExe {
    path: PathBuf,
    identity: Option<(u64, u64)>,
}

impl InstalledExe {
    pub(crate) fn current() -> io::Result<Self> {
        Self::at(fs::read_link("/proc/self/exe")?)
    }

    pub(crate) fn at(path: PathBuf) -> io::Result<Self> {
        let identity = identity_of(&path)?;
        Ok(Self {
            path,
            identity: Some(identity),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> Option<(u64, u64)> {
        self.identity
    }

    fn changed(&self) -> Option<(u64, u64)> {
        let identity = identity_of(&self.path).ok()?;
        (self.identity != Some(identity)).then_some(identity)
    }
}

pub(crate) fn identity_of(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

async fn idle_until_spec(
    fixture: &Fixture<'_>,
    shutdown: &CancellationToken,
) -> io::Result<DnsExit> {
    loop {
        tokio::select! {
            () = tokio::time::sleep(fixture.tick) => {}
            () = shutdown.cancelled() => return Ok(DnsExit::Stopped),
        }
        if fixture.spec.read()?.is_some() {
            return Ok(DnsExit::SpecChanged);
        }
    }
}

async fn bind_until_free(
    listen: SocketAddrV4,
    fixture: &Fixture<'_>,
    shutdown: &CancellationToken,
) -> io::Result<Option<Listeners>> {
    let mut backoff = fixture.backoff;
    loop {
        match Listeners::bind(listen) {
            Ok(listeners) => return Ok(Some(listeners)),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                fixture.keeper.status(&format!(
                    "waiting for port {} on {}",
                    listen.port(),
                    listen.ip()
                ))?;
            }
            Err(error) => return Err(error),
        }
        tokio::select! {
            () = tokio::time::sleep(backoff.take()) => {}
            () = shutdown.cancelled() => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
