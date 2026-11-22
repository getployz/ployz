//! The loop that joins Docker's events and inspects to what ployz-observe
//! records: the [`ExitLedger`] settles each exit once.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerConfig, ContainerInspectResponse, EventMessage},
    query_parameters::{EventsOptionsBuilder, ListContainersOptionsBuilder},
};
use futures_util::{Stream, StreamExt};
use ployz_core::{
    ContainerId, MachineId, labels,
    metrics::{MarkerKind, MetricMarker},
};
use tokio::sync::mpsc;

use super::{
    clock::now_nanos,
    exit_ledger::{ExitLedger, Late},
    layout::{ContainerKind, NANOS_PER_SEC, StoreRoot},
    meta::container_meta,
    metrics,
    ship::Outbox,
    signal::stop_signal,
};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const EVENTS_RETRY: Duration = Duration::from_secs(2);
const INSPECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_CODE_ATTRIBUTE: &str = "exitCode";
const SIGNAL_ATTRIBUTE: &str = "signal";
/// The settlements a resumed stream may pin before it is reopened from the
/// newest event.
const SETTLED_LIMIT: usize = 64;
/// How long a settlement may stay unfinished before it is given up.
const UNFINISHED_FOR_NANOS: i64 = 24 * 60 * 60 * NANOS_PER_SEC;

type DockerEvents = Pin<Box<dyn Stream<Item = Result<EventMessage, DockerError>> + Send>>;

enum Inspected {
    Found(Box<ContainerInspectResponse>),
    Gone,
    Unanswered,
}

pub(super) struct Harvester {
    docker: Docker,
    store: StoreRoot,
    /// Inspects in flight; `true` asks for another once this one answers.
    inspecting: HashMap<ContainerId, bool>,
    inspected: mpsc::UnboundedSender<(ContainerId, Inspected)>,
    /// The managed containers the last listing found, so a rescan inspects
    /// only the ones it has not met.
    listed: HashSet<ContainerId>,
    /// Holds the Machine record that stamps metric markers.
    data_dir: PathBuf,
    /// The newest Docker event seen. A reconnect resumes from it, so events
    /// sent while the stream was down still leave their markers.
    last_event_nanos: Option<i64>,
    /// The `since` the open stream resumed from, which it can repeat events
    /// at or after.
    stream_since: Option<i64>,
    /// The `last_event_nanos` stored for the next process to resume from.
    resume_from: Option<i64>,
    /// The newest time of each container's events, by action. Removing a
    /// container does not forget it, so a resumed stream does not replay its
    /// events; the rescan that finds it gone from Docker does.
    delivered: HashMap<(ContainerId, String), i64>,
    ledger: ExitLedger,
}

impl Harvester {
    pub(super) async fn run(
        docker: Docker,
        store: StoreRoot,
        data_dir: PathBuf,
        crashes: Arc<Outbox>,
    ) -> io::Result<()> {
        let (inspected, mut inspections) = mpsc::unbounded_channel();
        let mut harvester = Self::new(docker, store, inspected, data_dir, crashes);
        let mut docker_events = Some(harvester.docker_events());
        let mut reconnect_at = None;
        harvester.rescan().await;
        harvester.finish_settlements();
        harvester.clean();

        let mut maintenance = tokio::time::interval(MAINTENANCE_INTERVAL);
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        maintenance.tick().await;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                Some((id, inspected)) = inspections.recv() => harvester.on_inspected(id, inspected),
                event = next_docker_event(&mut docker_events) => match event {
                    Some(Ok(message)) => harvester.on_docker_event(message),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Docker events stream failed; reconnecting");
                        docker_events = None;
                        reconnect_at = Some(tokio::time::Instant::now() + EVENTS_RETRY);
                    }
                    None => {
                        docker_events = None;
                        reconnect_at = Some(tokio::time::Instant::now() + EVENTS_RETRY);
                    }
                },
                () = tokio::time::sleep_until(reconnect_at.unwrap_or_else(tokio::time::Instant::now)), if reconnect_at.is_some() => {
                    reconnect_at = None;
                    docker_events = Some(harvester.docker_events());
                    harvester.rescan().await;
                }
                _ = maintenance.tick() => harvester.maintain(&mut docker_events).await,
                _ = terminate.recv() => return Ok(()),
                result = tokio::signal::ctrl_c() => return result,
            }
        }
    }

    fn new(
        docker: Docker,
        store: StoreRoot,
        inspected: mpsc::UnboundedSender<(ContainerId, Inspected)>,
        data_dir: PathBuf,
        crashes: Arc<Outbox>,
    ) -> Self {
        let resume_from = read_resume_from(&store);
        Self {
            docker,
            ledger: ExitLedger::new(&store, crashes),
            store,
            inspecting: HashMap::new(),
            inspected,
            listed: HashSet::new(),
            data_dir,
            last_event_nanos: resume_from,
            stream_since: resume_from,
            resume_from,
            delivered: HashMap::new(),
        }
    }

    async fn maintain(&mut self, docker_events: &mut Option<DockerEvents>) {
        self.rescan().await;
        self.upkeep(docker_events);
    }

    fn upkeep(&mut self, docker_events: &mut Option<DockerEvents>) {
        self.finish_settlements();
        if docker_events.is_some() && self.stream_pins_too_many_settlements() {
            *docker_events = Some(self.docker_events());
        }
        self.clean();
    }

    /// A resumed stream can repeat any event since it resumed, so it pins
    /// their settlements until it is reopened from the newest event.
    fn stream_pins_too_many_settlements(&self) -> bool {
        if !self
            .stream_since
            .is_some_and(|since| Some(since) < self.last_event_nanos)
        {
            return false;
        }
        match self.ledger.settler.count() {
            Ok(count) => count >= SETTLED_LIMIT,
            Err(error) => {
                tracing::warn!(%error, "cannot count settlements");
                false
            }
        }
    }

    fn docker_events(&mut self) -> DockerEvents {
        let filters = HashMap::from([
            ("type", vec!["container"]),
            ("event", vec!["start", "kill", "stop", "oom", "die"]),
        ]);
        let mut options = EventsOptionsBuilder::default().filters(&filters);
        self.stream_since = self.last_event_nanos;
        if let Some(since) = self.stream_since.map(docker_time) {
            options = options.since(&since);
        }
        Box::pin(self.docker.events(Some(options.build())))
    }

    /// Inspects the containers met for the first time: an inspect is where a
    /// container's stop signal is read.
    async fn rescan(&mut self) {
        let Some(present) = list_managed(&self.docker).await else {
            return;
        };
        for id in self.meet(present) {
            self.request_inspect(id);
        }
    }

    fn meet(&mut self, present: HashSet<ContainerId>) -> Vec<ContainerId> {
        self.delivered.retain(|(id, _), _| present.contains(id));
        self.ledger.keep_present(&present);
        let met = present.difference(&self.listed).copied().collect();
        self.listed = present;
        met
    }

    /// A Docker event only asks for an inspect: Docker can send a run's events
    /// late or out of order, so a run's exit comes from inspect alone.
    fn on_docker_event(&mut self, message: EventMessage) {
        let Some(id) = message
            .actor
            .as_ref()
            .and_then(|actor| actor.id.as_deref())
            .and_then(|id| ContainerId::parse(id).ok())
        else {
            return;
        };
        if self.first_delivery(id, &message) {
            self.mark(id, &message);
        }
        let starting = message.action.as_deref() == Some("start");
        let service = container_meta(&labelled_by_event(&message))
            .is_some_and(|meta| meta.kind == ContainerKind::Service);
        if service && (starting || self.ledger.waits_on(&id)) {
            self.request_inspect(id);
        }
    }

    /// Records the event's time and says whether it is new: a stream resumed
    /// from the newest event repeats that event. Containers that die together
    /// send events at the same time.
    fn first_delivery(&mut self, id: ContainerId, message: &EventMessage) -> bool {
        let Some(at) = message.time_nano else {
            return true;
        };
        let last = self
            .delivered
            .entry((id, message.action.clone().unwrap_or_default()))
            .or_insert(i64::MIN);
        if at <= *last {
            return false;
        }
        *last = at;
        if Some(at) > self.last_event_nanos {
            self.last_event_nanos = Some(at);
            match crate::filesystem::atomic_write(
                &self.store.resume_from(),
                at.to_string().as_bytes(),
                0o600,
            ) {
                Ok(()) => self.resume_from = Some(at),
                Err(error) => tracing::warn!(%error, "cannot store where Docker events resume"),
            }
        }
        true
    }

    fn mark(&mut self, id: ContainerId, message: &EventMessage) {
        let Some(machine_id) = metrics::machine_id(&self.data_dir) else {
            return;
        };
        let Some((attributes, marker)) = service_event(machine_id, id, message) else {
            return;
        };
        match message.action.as_deref() {
            Some("kill") => {
                if let Some(signal) = attributes
                    .get(SIGNAL_ATTRIBUTE)
                    .and_then(|signal| signal.parse().ok())
                {
                    self.ledger.late(id, Late::Kill(signal));
                }
            }
            Some("stop") => self.ledger.late(id, Late::Stop),
            Some("oom") => {
                self.ledger.late(id, Late::Oom);
                self.ledger.settler.settle(marker(MarkerKind::Oom));
            }
            Some("die") => self.ledger.died(
                marker(MarkerKind::Exit {
                    exit_code: attributes
                        .get(EXIT_CODE_ATTRIBUTE)
                        .and_then(|code| code.parse().ok()),
                    oom: false,
                    stopped: false,
                }),
                self.stream_since.is_some(),
            ),
            Some("start") => self.ledger.started(id),
            _ => {}
        }
    }

    /// Finishes the settlements a failed write or a stopped harvester left
    /// unfinished, and gives up those a day old that no stream can repeat.
    fn finish_settlements(&mut self) {
        let give_up_before = self.forget_before().map_or(i64::MIN, |before| {
            before.min(
                self.last_event_nanos
                    .unwrap_or(i64::MIN)
                    .saturating_sub(UNFINISHED_FOR_NANOS),
            )
        });
        self.ledger.settler.resume_unfinished(give_up_before);
    }

    /// Settlements of events older than this cannot be repeated: the open
    /// stream resumed after them, or a fresh one has seen a newer event, and
    /// the next process resumes after them.
    fn forget_before(&self) -> Option<i64> {
        let before = self.stream_since.or(self.last_event_nanos)?;
        Some(self.resume_from.map_or(before, |resume| before.min(resume)))
    }

    fn request_inspect(&mut self, id: ContainerId) {
        if let Some(again) = self.inspecting.get_mut(&id) {
            *again = true;
            return;
        }
        self.inspecting.insert(id, false);
        let docker = self.docker.clone();
        let inspected = self.inspected.clone();
        tokio::spawn(async move {
            drop(inspected.send((id, inspect(&docker, &id).await)));
        });
    }

    fn on_inspected(&mut self, id: ContainerId, inspected: Inspected) {
        if let Inspected::Found(found) = &inspected
            && let Some(signal) = stop_signal(found)
        {
            self.ledger.note_stop_signal(id, signal);
        }
        // An inspect asked for again began before the newest event, so it
        // cannot settle an exit that event reported.
        if self.inspecting.remove(&id) == Some(true) {
            self.request_inspect(id);
        } else {
            let found = match &inspected {
                Inspected::Found(inspected) => Some(&**inspected),
                Inspected::Gone | Inspected::Unanswered => None,
            };
            self.ledger.settle_exits(
                &id,
                found.and_then(|inspected| inspected.state.as_ref()),
                found.and_then(stop_signal),
            );
        }
        if matches!(inspected, Inspected::Gone) {
            self.ledger.forget(&id);
        }
    }

    fn clean(&self) {
        if let Some(before) = self.forget_before()
            && let Err(error) = self.ledger.settler.forget_before(before)
        {
            tracing::warn!(%error, "cannot forget settlements no stream can repeat");
        }
    }
}

async fn list_managed(docker: &Docker) -> Option<HashSet<ContainerId>> {
    let filters = HashMap::from([("label", vec![labels::MANAGED])]);
    let options = ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filters)
        .build();
    match tokio::time::timeout(INSPECT_TIMEOUT, docker.list_containers(Some(options))).await {
        Ok(Ok(listed)) => Some(
            listed
                .into_iter()
                .filter_map(|container| container.id)
                .filter_map(|id| ContainerId::parse(id).ok())
                .collect(),
        ),
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot list Docker's containers");
            None
        }
        Err(_) => {
            tracing::warn!("Docker did not list its containers in time");
            None
        }
    }
}

async fn inspect(docker: &Docker, id: &ContainerId) -> Inspected {
    let inspect = docker.inspect_container(id.as_str(), None);
    match tokio::time::timeout(INSPECT_TIMEOUT, inspect).await {
        Ok(Ok(inspected)) => Inspected::Found(Box::new(inspected)),
        Ok(Err(DockerError::DockerResponseServerError {
            status_code: 404, ..
        })) => Inspected::Gone,
        Ok(Err(error)) => {
            tracing::warn!(container = %id, %error, "cannot inspect container");
            Inspected::Unanswered
        }
        Err(_) => {
            tracing::warn!(container = %id, "Docker did not answer an inspect in time");
            Inspected::Unanswered
        }
    }
}

async fn next_docker_event(
    events: &mut Option<DockerEvents>,
) -> Option<Result<EventMessage, DockerError>> {
    match events {
        Some(events) => events.next().await,
        None => std::future::pending().await,
    }
}

/// Where the last process left the event stream. A missing or unreadable
/// record leaves the stream to start from now.
fn read_resume_from(store: &StoreRoot) -> Option<i64> {
    match fs::read_to_string(store.resume_from()) {
        Ok(resume) => resume.trim().parse().ok(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(%error, "cannot read where Docker events resume");
            None
        }
    }
}

/// A Docker event's labels and name, shaped like an inspect that has no
/// state: an event says nothing about a run's exit.
fn labelled_by_event(message: &EventMessage) -> ContainerInspectResponse {
    let attributes = message
        .actor
        .as_ref()
        .and_then(|actor| actor.attributes.clone())
        .unwrap_or_default();
    ContainerInspectResponse {
        name: attributes.get("name").cloned(),
        config: Some(ContainerConfig {
            labels: Some(attributes),
            ..ContainerConfig::default()
        }),
        ..ContainerInspectResponse::default()
    }
}

/// The attributes of a Docker event on a Service container, and the marker
/// it leaves of a given kind. System containers and pre-deploy hooks get
/// none: no Service shows them, and a hook is meant to exit.
fn service_event(
    machine_id: MachineId,
    id: ContainerId,
    message: &EventMessage,
) -> Option<(
    HashMap<String, String>,
    impl FnOnce(MarkerKind) -> MetricMarker,
)> {
    let labelled = labelled_by_event(message);
    let meta = container_meta(&labelled).filter(|meta| meta.kind == ContainerKind::Service)?;
    let attributes = labelled.config.and_then(|config| config.labels)?;
    let (service_id, service) = labels::service_pair(&attributes)?;
    let ts = message
        .time_nano
        .or_else(|| message.time.map(|secs| secs.saturating_mul(NANOS_PER_SEC)))
        .unwrap_or_else(now_nanos);
    let marker = move |kind| MetricMarker {
        machine_id,
        container_id: id,
        service_id,
        service,
        deployment_id: meta.deployment,
        replica: meta.replica,
        ts,
        kind,
    };
    Some((attributes, marker))
}

/// Docker's `since` takes seconds with a fractional part.
fn docker_time(nanos: i64) -> String {
    format!(
        "{}.{:09}",
        nanos.div_euclid(NANOS_PER_SEC),
        nanos.rem_euclid(NANOS_PER_SEC)
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::Path,
    };

    use bollard::{
        Docker,
        models::{ContainerState, EventActor, EventMessage},
    };
    use ployz_core::{ContainerId, MachineId, metrics::MarkerKind};
    use tokio::sync::mpsc;

    use super::{Harvester, Inspected, SETTLED_LIMIT, UNFINISHED_FOR_NANOS, read_resume_from};
    use crate::{
        machine::{LocalMachineBody, LocalMachineRecord},
        network::WireGuardPrivateKey,
        observe::{
            fixtures::{T0, labelled},
            layout::StoreRoot,
            settled::Unfinished,
            ship::{self, Outbox},
        },
        test_dir::TestDir,
    };

    struct Host {
        root: TestDir,
        store: StoreRoot,
    }

    struct Running {
        harvester: Harvester,
        _inspections: mpsc::UnboundedReceiver<(ContainerId, Inspected)>,
    }

    const ID: &str = "abababababababababababababababababababababababababababababababab";

    fn host() -> Host {
        let root = TestDir::new("ployzd-observe-harvester");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        Host { root, store }
    }

    impl Host {
        fn id(&self) -> ContainerId {
            ContainerId::parse(ID).unwrap()
        }

        /// A harvester whose Docker never answers, so a test hands it each
        /// inspect and event.
        fn harvester(&self) -> Running {
            let socket = self.root.0.join("no-docker.sock");
            fs::write(&socket, b"").unwrap();
            let docker = Docker::connect_with_socket(
                socket.to_str().unwrap(),
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .unwrap();
            let (inspected, inspections) = mpsc::unbounded_channel();
            let harvester = Harvester::new(
                docker,
                self.store.clone(),
                inspected,
                self.root.0.clone(),
                Outbox::open(&self.store).unwrap(),
            );
            Running {
                harvester,
                _inspections: inspections,
            }
        }
    }

    const WEB: [(&str, &str); 4] = [
        ("ployz.managed", "true"),
        ("ployz.namespace", "default"),
        ("ployz.service.name", "web"),
        ("ployz.deployment.id", "d1"),
    ];

    fn managed() -> Inspected {
        let mut inspected = labelled(&WEB);
        inspected.state = Some(ContainerState {
            running: Some(true),
            ..ContainerState::default()
        });
        Inspected::Found(Box::new(inspected))
    }

    fn event(action: &str, at: i64, extra: &[(&str, &str)]) -> EventMessage {
        EventMessage {
            action: Some(action.to_owned()),
            actor: Some(EventActor {
                id: Some(ID.to_owned()),
                attributes: Some(
                    WEB.iter()
                        .chain(extra)
                        .chain(&[("name", "web-1")])
                        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                        .collect(),
                ),
            }),
            time_nano: Some(at),
            ..EventMessage::default()
        }
    }

    fn at(nanos: i64) -> String {
        format!("2025-10-09T08:53:20.{nanos:09}Z")
    }

    /// Docker's state for the run it started at `T0 + started`, still running.
    fn running(started: i64) -> ContainerState {
        ContainerState {
            running: Some(true),
            started_at: Some(at(started)),
            ..ContainerState::default()
        }
    }

    /// Docker's state for a later run it started at `T0 + started`, still
    /// running: FinishedAt is still the previous run's and ExitCode is reset.
    fn running_after(started: i64, previous_finished: i64) -> ContainerState {
        ContainerState {
            finished_at: Some(at(previous_finished)),
            exit_code: Some(0),
            oom_killed: Some(false),
            ..running(started)
        }
    }

    /// Docker's state for the run it started at `T0 + started` once it
    /// exited at `T0 + finished`.
    fn exited(started: i64, finished: i64, code: i64, oom: bool) -> ContainerState {
        ContainerState {
            running: Some(false),
            started_at: Some(at(started)),
            finished_at: Some(at(finished)),
            exit_code: Some(code),
            oom_killed: Some(oom),
            ..ContainerState::default()
        }
    }

    /// The same exited run while Docker waits to restart it.
    fn restarting(started: i64, finished: i64, code: i64, oom: bool) -> ContainerState {
        ContainerState {
            running: Some(true),
            restarting: Some(true),
            ..exited(started, finished, code, oom)
        }
    }

    fn inspected(state: &ContainerState) -> Inspected {
        let Inspected::Found(mut found) = managed() else {
            unreachable!("managed() describes a found container")
        };
        found.state = Some(state.clone());
        Inspected::Found(found)
    }

    const SERVICE_ID: &str = "cccccccccccccccccccccccccccccccc";

    impl Host {
        fn with_machine(self) -> Self {
            let record = LocalMachineRecord::parse(
                LocalMachineBody::Uninitialized {
                    id: MachineId::parse("a".repeat(32)).unwrap(),
                },
                WireGuardPrivateKey::from_bytes([0; 32]),
            )
            .unwrap();
            fs::write(
                self.root.0.join("machine.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            self
        }

        /// The exits and OOM kills the store settled or is settling.
        fn markers(&self) -> Vec<(i64, MarkerKind)> {
            let Ok(settlements) = fs::read_dir(self.store.settled()) else {
                return Vec::new();
            };
            let mut markers: Vec<(i64, MarkerKind)> = settlements
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.is_file() && path.extension().is_none_or(|ext| ext == "pending")
                })
                .map(|path| {
                    serde_json::from_slice::<Unfinished>(&fs::read(path).unwrap())
                        .unwrap()
                        .marker
                })
                .map(|marker| {
                    assert_eq!(marker.service_id.as_str(), SERVICE_ID);
                    assert_eq!(marker.service.to_string(), "default/web");
                    assert_eq!(marker.replica, "web-1");
                    assert_eq!(marker.deployment_id.as_deref(), Some("d1"));
                    (marker.ts, marker.kind)
                })
                .collect();
            markers.sort_by_key(|(ts, _)| *ts);
            markers
        }
    }

    #[tokio::test]
    async fn an_oom_kill_and_its_exit_leave_one_marker_each() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = [("ployz.service.id", SERVICE_ID)];
        let die = event("die", T0 + 2, &[service[0], ("exitCode", "137")]);
        running
            .harvester
            .on_docker_event(event("oom", T0 + 1, &service));
        running.harvester.on_docker_event(die.clone());
        running
            .harvester
            .on_docker_event(event("start", T0 + 3, &service));
        running
            .harvester
            .on_docker_event(event("start", T0 + 4, &service));
        // A stream resumed from the newest event delivers it again.
        running
            .harvester
            .on_docker_event(event("start", T0 + 4, &service));
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Unanswered);

        assert_eq!(
            host.markers(),
            [
                (T0 + 1, MarkerKind::Oom),
                (
                    T0 + 2,
                    MarkerKind::Exit {
                        exit_code: Some(137),
                        oom: true,
                        stopped: false,
                    }
                ),
            ]
        );
    }

    impl Running {
        /// Answers the inspects an exit waits on, including the one re-asked
        /// for an event that came while the first was in flight.
        fn settle_exits(&mut self, inspected: impl Fn() -> Inspected) {
            let id = ContainerId::parse(ID).unwrap();
            for _ in 0..3 {
                if !self.harvester.ledger.waits_on(&id) {
                    return;
                }
                self.harvester.on_inspected(id, inspected());
            }
            panic!("the exit never settled");
        }
    }

    /// An inspect of a container whose image stops it with `signal`.
    fn stopping_on(signal: &str, state: &ContainerState) -> Inspected {
        let Inspected::Found(mut found) = inspected(state) else {
            unreachable!("inspected() describes a found container")
        };
        found.config.as_mut().unwrap().stop_signal = Some(signal.to_owned());
        Inspected::Found(found)
    }

    fn crashes(host: &Host) -> Vec<MarkerKind> {
        Outbox::open(&host.store)
            .unwrap()
            .oldest(ship::CAP)
            .unwrap()
            .into_iter()
            .map(|entry| entry.marker.kind)
            .collect()
    }

    #[tokio::test]
    async fn an_oom_kill_queues_one_oom_crash_whatever_order_docker_reports_it_in() {
        let service = ("ployz.service.id", SERVICE_ID);
        let oom = |at| event("oom", at, &[service]);
        let die = |at| event("die", at, &[service, ("exitCode", "137")]);
        let orders = [
            ("oom then die", vec![oom(T0 + 1), die(T0 + 2)], true),
            ("die then oom", vec![die(T0 + 1), oom(T0 + 2)], true),
            ("die with oom dropped", vec![die(T0 + 1)], false),
        ];
        for (order, events, oom_event) in orders {
            for inspected_after_restart in [false, true] {
                let host = host().with_machine();
                let mut running = host.harvester();
                running
                    .harvester
                    .on_docker_event(event("start", T0, &[service]));
                for event in events.clone() {
                    running.harvester.on_docker_event(event);
                }
                let restart = event("start", T0 + 3, &[service]);
                if inspected_after_restart {
                    // Docker clears OOMKilled when it starts the next run.
                    running.harvester.on_docker_event(restart);
                    running.settle_exits(|| inspected(&running_after(3, 1)));
                } else {
                    running.settle_exits(|| inspected(&restarting(0, 1, 137, true)));
                    running.harvester.on_docker_event(restart);
                }
                assert_eq!(
                    crashes(&host),
                    [MarkerKind::Exit {
                        exit_code: Some(137),
                        oom: oom_event || !inspected_after_restart,
                        stopped: false,
                    }],
                    "{order}, inspected after the restart: {inspected_after_restart}"
                );
            }
        }
    }

    #[tokio::test]
    async fn an_exit_does_not_take_the_oom_kill_of_a_later_run() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        for message in [
            event("start", T0 + 1, &[service]),
            event("die", T0 + 2, &[service, ("exitCode", "1")]),
            event("start", T0 + 3, &[service]),
            event("die", T0 + 5, &[service, ("exitCode", "137")]),
        ] {
            running.harvester.on_docker_event(message);
        }
        running.settle_exits(|| inspected(&exited(3, 5, 137, true)));
        assert_eq!(
            crashes(&host),
            [
                MarkerKind::Exit {
                    exit_code: Some(1),
                    oom: false,
                    stopped: false,
                },
                MarkerKind::Exit {
                    exit_code: Some(137),
                    oom: true,
                    stopped: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_signal_that_does_not_stop_the_container_does_not_hide_the_crash_after_it() {
        let service = ("ployz.service.id", SERVICE_ID);
        for signal in ["1", "SIGHUP", "USR1", "12", "18", "CONT", "28"] {
            let host = host().with_machine();
            let mut running = host.harvester();
            running
                .harvester
                .on_docker_event(event("start", T0 + 1, &[service]));
            running.harvester.on_docker_event(event(
                "kill",
                T0 + 2,
                &[service, ("signal", signal)],
            ));
            running
                .harvester
                .on_docker_event(event("die", T0 + 3, &[service, ("exitCode", "1")]));
            running.settle_exits(|| inspected(&exited(1, 3, 1, false)));
            assert_eq!(
                crashes(&host),
                [MarkerKind::Exit {
                    exit_code: Some(1),
                    oom: false,
                    stopped: false,
                }],
                "signal {signal}"
            );
        }
    }

    #[tokio::test]
    async fn a_kill_with_the_images_stop_signal_is_a_stop() {
        let service = ("ployz.service.id", SERVICE_ID);
        for code in [0, 143] {
            for gone_at_exit in [false, true] {
                let host = host().with_machine();
                let mut harvester = host.harvester();
                let id = host.id();
                harvester
                    .harvester
                    .on_inspected(id, stopping_on("SIGWINCH", &running(1)));
                harvester
                    .harvester
                    .on_docker_event(event("start", T0 + 1, &[service]));
                harvester.harvester.on_docker_event(event(
                    "kill",
                    T0 + 2,
                    &[service, ("signal", "28")],
                ));
                let exit_code = code.to_string();
                harvester.harvester.on_docker_event(event(
                    "die",
                    T0 + 3,
                    &[service, ("exitCode", &exit_code)],
                ));
                if gone_at_exit {
                    harvester.settle_exits(|| Inspected::Gone);
                } else {
                    harvester.settle_exits(|| stopping_on("WINCH", &exited(1, 3, code, false)));
                }
                assert_eq!(
                    host.markers(),
                    [(
                        T0 + 3,
                        MarkerKind::Exit {
                            exit_code: Some(code),
                            oom: false,
                            stopped: true,
                        }
                    )],
                    "exit {code}, gone at exit: {gone_at_exit}"
                );
                assert_eq!(crashes(&host), [], "exit {code}");
            }
        }
    }

    #[tokio::test]
    async fn a_stop_signal_other_than_term_still_counts_as_a_stop() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        running
            .harvester
            .on_docker_event(event("start", T0 + 1, &[service]));
        running
            .harvester
            .on_docker_event(event("kill", T0 + 2, &[service, ("signal", "3")]));
        running
            .harvester
            .on_docker_event(event("die", T0 + 3, &[service, ("exitCode", "131")]));
        running.settle_exits(|| inspected(&exited(1, 3, 131, false)));
        assert_eq!(crashes(&host), []);
    }

    #[tokio::test]
    async fn a_stop_signal_exit_of_a_run_observe_never_saw_is_a_stop() {
        let service = ("ployz.service.id", SERVICE_ID);
        let cases = [
            ("143", false, false, true),
            ("137", false, false, true),
            ("137", true, true, false),
            ("1", false, true, false),
        ];
        for (code, oom_killed, crashed, stopped) in cases {
            let host = host().with_machine();
            let mut running = host.harvester();
            running
                .harvester
                .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", code)]));
            let exit_code = code.parse().unwrap();
            running.settle_exits(|| inspected(&exited(0, 1, exit_code, oom_killed)));
            assert_eq!(
                host.markers(),
                [(
                    T0 + 1,
                    MarkerKind::Exit {
                        exit_code: code.parse().ok(),
                        oom: oom_killed,
                        stopped,
                    }
                )],
                "exit {code}, OOMKilled {oom_killed}"
            );
            assert_eq!(crashes(&host).len(), usize::from(crashed), "exit {code}");
        }

        let host = host().with_machine();
        let mut running = host.harvester();
        running
            .harvester
            .on_docker_event(event("start", T0 + 1, &[service]));
        running
            .harvester
            .on_docker_event(event("die", T0 + 2, &[service, ("exitCode", "143")]));
        running.settle_exits(|| inspected(&exited(1, 2, 143, false)));
        assert_eq!(
            crashes(&host).len(),
            1,
            "a seen run that exits 143 unasked crashed"
        );
    }

    impl Host {
        /// A harvester restarted after the last one saw an event at `last`,
        /// with the stream it opens.
        fn restarted_after(&self, last: i64) -> Running {
            let mut other = event("start", last, &[]);
            other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
            let mut before = self.harvester();
            let _stream = before.harvester.docker_events();
            before.harvester.on_docker_event(other);
            drop(before);
            let mut after = self.harvester();
            let _stream = after.harvester.docker_events();
            after
        }
    }

    #[tokio::test]
    async fn a_crash_while_observe_was_down_is_recorded_after_its_restart() {
        let service = ("ployz.service.id", SERVICE_ID);
        for code in [137, 143] {
            let host = host().with_machine();
            let mut running = host.restarted_after(T0);
            assert_eq!(running.harvester.stream_since, Some(T0));
            let exit_code = code.to_string();
            running.harvester.on_docker_event(event(
                "die",
                T0 + 1,
                &[service, ("exitCode", &exit_code)],
            ));
            running.settle_exits(|| inspected(&exited(0, 1, code, false)));
            assert_eq!(
                crashes(&host),
                [MarkerKind::Exit {
                    exit_code: Some(code),
                    oom: false,
                    stopped: false,
                }],
                "exit {code}"
            );
        }
    }

    #[tokio::test]
    async fn a_docker_stop_while_observe_was_down_is_a_stop_after_its_restart() {
        let service = ("ployz.service.id", SERVICE_ID);
        let kill = |at, signal| event("kill", at, &[service, ("signal", signal)]);
        let die = |at, code| event("die", at, &[service, ("exitCode", code)]);
        let stop = |at| event("stop", at, &[service]);
        let cases = [
            (
                "TERM",
                vec![kill(T0 + 1, "15"), die(T0 + 2, "143"), stop(T0 + 3)],
            ),
            (
                "TERM then KILL",
                vec![
                    kill(T0 + 1, "15"),
                    kill(T0 + 2, "9"),
                    die(T0 + 3, "137"),
                    stop(T0 + 4),
                ],
            ),
            (
                "kills before the resume point",
                vec![die(T0 + 1, "137"), stop(T0 + 2)],
            ),
        ];
        for (case, events) in cases {
            let host = host().with_machine();
            let mut running = host.restarted_after(T0);
            assert_eq!(running.harvester.stream_since, Some(T0), "{case}");
            for message in events {
                running.harvester.on_docker_event(message);
            }
            running.settle_exits(|| Inspected::Gone);
            assert_eq!(crashes(&host), [], "{case}");
            assert_eq!(host.markers().len(), 1, "{case}");
        }
    }

    #[tokio::test]
    async fn a_stop_signal_exit_after_a_restart_with_no_resume_point_is_a_stop() {
        let service = ("ployz.service.id", SERVICE_ID);
        for resume_from in [None, Some("not a time"), Some("")] {
            for code in ["137", "143"] {
                let host = host().with_machine();
                let mut before = host.harvester();
                before
                    .harvester
                    .on_docker_event(event("start", T0, &[service]));
                drop(before);
                match resume_from {
                    Some(contents) => fs::write(host.store.resume_from(), contents).unwrap(),
                    None => fs::remove_file(host.store.resume_from()).unwrap(),
                }
                let mut running = host.harvester();
                let _stream = running.harvester.docker_events();
                assert_eq!(running.harvester.stream_since, None, "{resume_from:?}");
                running.harvester.on_docker_event(event(
                    "die",
                    T0 + 1,
                    &[service, ("exitCode", code)],
                ));
                running.settle_exits(|| Inspected::Gone);
                assert_eq!(crashes(&host), [], "{resume_from:?}, exit {code}");
                assert_eq!(
                    read_resume_from(&host.store),
                    Some(T0 + 1),
                    "{resume_from:?}, exit {code}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_stop_through_dockers_stop_api_is_a_stop_whatever_signal_it_sends() {
        let service = ("ployz.service.id", SERVICE_ID);
        let kill = |at| event("kill", at, &[service, ("signal", "10")]);
        let stop = |at| event("stop", at, &[service]);
        let die = |at| event("die", at, &[service, ("exitCode", "1")]);
        let cases = [
            (
                "stop before die",
                vec![kill(T0 + 2), stop(T0 + 3), die(T0 + 4)],
                false,
            ),
            (
                "stop after die",
                vec![kill(T0 + 2), die(T0 + 3), stop(T0 + 4)],
                false,
            ),
            ("bare kill", vec![kill(T0 + 2), die(T0 + 3)], true),
        ];
        for (case, events, crashed) in cases {
            for seen_start in [true, false] {
                let host = host().with_machine();
                let mut running = host.harvester();
                if seen_start {
                    running
                        .harvester
                        .on_docker_event(event("start", T0 + 1, &[service]));
                }
                for message in events.clone() {
                    running.harvester.on_docker_event(message);
                }
                running.settle_exits(|| inspected(&exited(1, 4, 1, false)));
                assert_eq!(
                    crashes(&host),
                    if crashed {
                        vec![MarkerKind::Exit {
                            exit_code: Some(1),
                            oom: false,
                            stopped: false,
                        }]
                    } else {
                        vec![]
                    },
                    "{case}, start seen: {seen_start}"
                );
            }
        }
    }

    #[tokio::test]
    async fn containers_that_die_together_each_queue_a_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let other = "cd".repeat(32);
        let mut die = event("die", T0 + 1, &[service, ("exitCode", "1")]);
        running.harvester.on_docker_event(die.clone());
        die.actor.as_mut().unwrap().id = Some(other.clone());
        running.harvester.on_docker_event(die.clone());
        // A stream resumed from the newest event delivers both again.
        running.harvester.on_docker_event(die);
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| Inspected::Gone);
        let other = ContainerId::parse(&other).unwrap();
        while running.harvester.ledger.waits_on(&other) {
            running.harvester.on_inspected(other, Inspected::Gone);
        }
        assert_eq!(crashes(&host).len(), 2);
    }

    #[tokio::test]
    async fn a_crash_delivered_again_after_a_rescan_forgets_its_container_is_recorded_once() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let die = event(
            "die",
            T0 + 1,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        );
        running.harvester.on_docker_event(die.clone());
        running.settle_exits(|| Inspected::Gone);
        running.harvester.meet(HashSet::new());
        assert!(running.harvester.delivered.is_empty());
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host).len(), 1);
        assert_eq!(host.markers().len(), 1);
    }

    #[tokio::test]
    async fn a_stops_exit_delivered_again_after_a_rescan_forgets_its_container_queues_no_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let die = event("die", T0 + 2, &[service, ("exitCode", "1")]);
        running
            .harvester
            .on_docker_event(event("kill", T0 + 1, &[service, ("signal", "15")]));
        running.harvester.on_docker_event(die.clone());
        running
            .harvester
            .on_docker_event(event("stop", T0 + 3, &[service]));
        running.settle_exits(|| Inspected::Gone);
        running.harvester.meet(HashSet::new());
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host), []);
    }

    #[tokio::test]
    async fn a_restarted_harvester_records_a_crash_it_recorded_before_once() {
        let host = host().with_machine();
        let die = event(
            "die",
            T0 + 1,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        );
        for _ in 0..2 {
            let mut running = host.harvester();
            running.harvester.on_docker_event(die.clone());
            running.settle_exits(|| Inspected::Gone);
        }
        assert_eq!(crashes(&host).len(), 1);
    }

    #[tokio::test]
    async fn an_oom_kill_delivered_again_after_a_rescan_forgets_its_container_is_recorded_once() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let oom = event("oom", T0 + 1, &[("ployz.service.id", SERVICE_ID)]);
        running.harvester.on_docker_event(oom.clone());
        running.harvester.meet(HashSet::new());
        running.harvester.on_docker_event(oom);
        assert_eq!(host.markers(), [(T0 + 1, MarkerKind::Oom)]);
    }

    #[tokio::test]
    async fn a_crash_the_outbox_could_not_queue_is_queued_when_its_die_comes_again() {
        let host = host().with_machine();
        let die = event(
            "die",
            T0 + 1,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        );
        let outbox = host.store.crash_outbox();
        let mut running = host.harvester();
        fs::remove_dir_all(&outbox).unwrap();
        fs::write(&outbox, b"").unwrap();
        running.harvester.on_docker_event(die.clone());
        running.settle_exits(|| Inspected::Gone);
        fs::remove_file(&outbox).unwrap();
        let mut running = host.harvester();
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host).len(), 1);
    }

    #[tokio::test]
    async fn a_crash_delivered_again_after_cleanup_drops_its_container_is_recorded_once() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let die = event(
            "die",
            T0 + 1,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        );
        running.harvester.on_docker_event(die.clone());
        running.settle_exits(|| Inspected::Gone);
        running.harvester.meet(HashSet::new());
        running.harvester.clean();
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host).len(), 1);
    }

    #[tokio::test]
    async fn a_stops_exit_delivered_again_after_cleanup_drops_its_container_queues_no_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let mut other = event("start", T0, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other);
        let _stream = running.harvester.docker_events();
        let die = event("die", T0 + 2, &[service, ("exitCode", "1")]);
        running
            .harvester
            .on_docker_event(event("kill", T0 + 1, &[service, ("signal", "15")]));
        running.harvester.on_docker_event(die.clone());
        running
            .harvester
            .on_docker_event(event("stop", T0 + 3, &[service]));
        running.settle_exits(|| Inspected::Gone);
        running.harvester.meet(HashSet::new());
        running.harvester.clean();
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host), []);
    }

    #[tokio::test]
    async fn a_crash_the_outbox_refused_is_queued_after_a_later_crash_was() {
        let host = host().with_machine();
        let service = ("ployz.service.id", SERVICE_ID);
        let die = |at| event("die", at, &[service, ("exitCode", "1")]);
        let outbox = host.store.crash_outbox();
        let mut running = host.harvester();
        fs::remove_dir_all(&outbox).unwrap();
        fs::write(&outbox, b"").unwrap();
        running.harvester.on_docker_event(die(T0 + 1));
        running.settle_exits(|| Inspected::Unanswered);
        fs::remove_file(&outbox).unwrap();
        fs::create_dir(&outbox).unwrap();
        running
            .harvester
            .on_docker_event(event("start", T0 + 2, &[service]));
        running.harvester.on_docker_event(die(T0 + 3));
        running.settle_exits(|| Inspected::Unanswered);
        running.harvester.meet(HashSet::new());
        running.harvester.on_docker_event(die(T0 + 1));
        running.settle_exits(|| Inspected::Gone);
        let mut queued: Vec<i64> = Outbox::open(&host.store)
            .unwrap()
            .oldest(ship::CAP)
            .unwrap()
            .into_iter()
            .map(|entry| entry.marker.ts)
            .collect();
        queued.sort_unstable();
        assert_eq!(queued, [T0 + 1, T0 + 3]);
        let exits = host
            .markers()
            .into_iter()
            .filter(|(_, kind)| matches!(kind, MarkerKind::Exit { .. }))
            .count();
        assert_eq!(exits, 2);
    }

    fn queued(host: &Host) -> Vec<u64> {
        Outbox::open(&host.store)
            .unwrap()
            .oldest(ship::CAP)
            .unwrap()
            .into_iter()
            .map(|entry| entry.seq)
            .collect()
    }

    #[tokio::test]
    async fn a_crash_queued_before_its_settlement_was_finished_is_queued_once() {
        let host = host().with_machine();
        let die = event(
            "die",
            T0 + 1,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        );
        let finished = host.store.settled().join(format!("{ID}-exit-{}", T0 + 1));
        fs::create_dir_all(finished.join("blocked")).unwrap();
        let mut running = host.harvester();
        running.harvester.on_docker_event(die.clone());
        running.settle_exits(|| Inspected::Gone);
        let first = queued(&host);
        assert_eq!(first.len(), 1);
        fs::remove_dir_all(&finished).unwrap();
        let mut running = host.harvester();
        running.harvester.finish_settlements();
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(queued(&host), first);
        assert_eq!(host.markers().len(), 1);
    }

    #[tokio::test]
    async fn a_crash_the_open_stream_can_repeat_is_recorded_once_after_newer_events() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let mut other = event("start", T0 + 1, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other.clone());
        let _stream = running.harvester.docker_events();
        let die = event("die", T0 + 2, &[service, ("exitCode", "1")]);
        running.harvester.on_docker_event(die.clone());
        running.settle_exits(|| Inspected::Gone);
        other.time_nano = Some(T0 + 3);
        running.harvester.on_docker_event(other);
        running.harvester.meet(HashSet::new());
        running.harvester.clean();
        running.harvester.on_docker_event(die);
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(crashes(&host).len(), 1);
    }

    #[tokio::test]
    async fn a_settlement_is_forgotten_once_no_stream_can_repeat_its_event() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| Inspected::Gone);
        let settled = || fs::read_dir(host.store.settled()).unwrap().count();
        running.harvester.clean();
        assert_eq!(settled(), 1);
        let mut other = event("start", T0 + 2, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other);
        running.harvester.clean();
        assert_eq!(settled(), 0);
        assert_eq!(crashes(&host).len(), 1);
    }

    fn unfinished(host: &Host) -> usize {
        fs::read_dir(host.store.settled())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".pending")
            })
            .count()
    }

    #[tokio::test]
    async fn a_crash_cloud_took_is_not_queued_again_while_its_settlement_cannot_finish() {
        let host = host().with_machine();
        let service = ("ployz.service.id", SERVICE_ID);
        let finished = host.store.settled().join(format!("{ID}-exit-{}", T0 + 1));
        fs::create_dir_all(finished.join("blocked")).unwrap();
        let mut running = host.harvester();
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| Inspected::Gone);
        let shipped = queued(&host);
        assert_eq!(shipped.len(), 1);
        Outbox::open(&host.store).unwrap().ack(&shipped).unwrap();
        running.harvester.finish_settlements();
        assert!(queued(&host).is_empty());
        assert_eq!(unfinished(&host), 1);
        let mut other = event("start", T0 + 2 + UNFINISHED_FOR_NANOS, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other);
        running.harvester.finish_settlements();
        assert_eq!(unfinished(&host), 0);
        assert!(queued(&host).is_empty());
    }

    #[tokio::test]
    async fn a_crash_the_outbox_never_queued_is_not_given_up() {
        let host = host().with_machine();
        let service = ("ployz.service.id", SERVICE_ID);
        let outbox = host.store.crash_outbox();
        let mut running = host.harvester();
        fs::remove_dir_all(&outbox).unwrap();
        fs::write(&outbox, b"").unwrap();
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| Inspected::Gone);
        let mut other = event("start", T0 + 2 + UNFINISHED_FOR_NANOS, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other);
        running.harvester.finish_settlements();
        fs::remove_file(&outbox).unwrap();
        fs::create_dir(&outbox).unwrap();
        running.harvester.finish_settlements();
        assert_eq!(crashes(&host).len(), 1);
        assert_eq!(unfinished(&host), 0);
    }

    #[tokio::test]
    async fn a_resumed_stream_held_open_through_a_crash_loop_pins_few_settlements() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let mut other = event("start", T0, &[service]);
        other.actor.as_mut().unwrap().id = Some("cd".repeat(32));
        running.harvester.on_docker_event(other);
        let mut stream = Some(running.harvester.docker_events());
        for crash in 0..100 {
            let at = T0 + 1 + 2 * crash;
            running
                .harvester
                .on_docker_event(event("die", at, &[service, ("exitCode", "1")]));
            running
                .harvester
                .on_docker_event(event("start", at + 1, &[service]));
            running.settle_exits(|| Inspected::Unanswered);
        }
        running.harvester.upkeep(&mut stream);
        let settled = fs::read_dir(host.store.settled()).unwrap().count();
        assert!(settled < SETTLED_LIMIT, "{settled} settlements are kept");
        assert_eq!(crashes(&host).len(), 100);
    }

    #[tokio::test]
    async fn each_crash_of_a_crash_looping_container_is_recorded() {
        let host = host().with_machine();
        let service = ("ployz.service.id", SERVICE_ID);
        let die = |at| event("die", at, &[service, ("exitCode", "1")]);
        for (die_at, start_at) in [(T0 + 1, T0 + 2), (T0 + 3, T0 + 4)] {
            let mut running = host.harvester();
            running.harvester.on_docker_event(die(die_at));
            running
                .harvester
                .on_docker_event(event("start", start_at, &[service]));
            running.settle_exits(|| Inspected::Unanswered);
        }
        assert_eq!(crashes(&host).len(), 2);
    }

    #[tokio::test]
    async fn a_late_stop_of_an_earlier_run_does_not_hide_the_crash_of_the_live_run() {
        let service = ("ployz.service.id", SERVICE_ID);
        for settled_before_stop in [false, true] {
            let host = host().with_machine();
            let mut running = host.harvester();
            running
                .harvester
                .on_docker_event(event("start", T0 + 1, &[service]));
            running
                .harvester
                .on_docker_event(event("kill", T0 + 2, &[service, ("signal", "15")]));
            running
                .harvester
                .on_docker_event(event("die", T0 + 3, &[service, ("exitCode", "1")]));
            running
                .harvester
                .on_docker_event(event("start", T0 + 4, &[service]));
            if settled_before_stop {
                running.settle_exits(|| inspected(&running_after(4, 3)));
            }
            running
                .harvester
                .on_docker_event(event("stop", T0 + 5, &[service]));
            running.settle_exits(|| inspected(&running_after(4, 3)));
            running
                .harvester
                .on_docker_event(event("die", T0 + 6, &[service, ("exitCode", "1")]));
            running.settle_exits(|| inspected(&exited(4, 6, 1, false)));
            assert_eq!(
                crashes(&host),
                [MarkerKind::Exit {
                    exit_code: Some(1),
                    oom: false,
                    stopped: false,
                }],
                "first exit settled before the stop: {settled_before_stop}"
            );
        }
    }

    #[tokio::test]
    async fn a_realtime_stop_signal_matches_by_name_and_by_number() {
        let service = ("ployz.service.id", SERVICE_ID);
        for configured in ["SIGRTMIN+3", "rtmin+3", "37"] {
            let host = host().with_machine();
            let mut harvester = host.harvester();
            let id = host.id();
            harvester
                .harvester
                .on_inspected(id, stopping_on(configured, &running(1)));
            harvester
                .harvester
                .on_docker_event(event("start", T0 + 1, &[service]));
            harvester.harvester.on_docker_event(event(
                "kill",
                T0 + 2,
                &[service, ("signal", "37")],
            ));
            harvester.harvester.on_docker_event(event(
                "die",
                T0 + 3,
                &[service, ("exitCode", "1")],
            ));
            harvester.settle_exits(|| stopping_on(configured, &exited(1, 3, 1, false)));
            assert_eq!(crashes(&host), [], "StopSignal {configured}");
        }

        // A run observe never saw that exits on the realtime signal: 128 + 37.
        let host = host().with_machine();
        let mut harvester = host.harvester();
        harvester
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "165")]));
        harvester.settle_exits(|| stopping_on("SIGRTMIN+3", &exited(0, 1, 165, false)));
        assert_eq!(crashes(&host), [], "an unseen exit by RTMIN+3");
    }

    #[tokio::test]
    async fn a_late_oom_upgrades_an_unseen_runs_pending_exit_to_a_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "137")]));
        running
            .harvester
            .on_docker_event(event("oom", T0 + 2, &[service]));
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(
            crashes(&host),
            [MarkerKind::Exit {
                exit_code: Some(137),
                oom: true,
                stopped: false,
            }]
        );
    }

    #[tokio::test]
    async fn a_non_zero_exit_queues_one_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| inspected(&exited(0, 1, 1, false)));
        // A stream resumed from the newest event delivers it again.
        running
            .harvester
            .on_docker_event(event("die", T0 + 1, &[service, ("exitCode", "1")]));
        running.settle_exits(|| inspected(&exited(0, 1, 1, false)));
        assert_eq!(
            crashes(&host),
            [MarkerKind::Exit {
                exit_code: Some(1),
                oom: false,
                stopped: false,
            }]
        );
    }

    #[tokio::test]
    async fn a_clean_exit_or_a_stop_queues_no_crash() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let service = ("ployz.service.id", SERVICE_ID);
        let mut at = T0;
        let mut next = || {
            at += 1;
            at
        };
        running
            .harvester
            .on_docker_event(event("die", next(), &[service, ("exitCode", "0")]));
        running.settle_exits(|| Inspected::Unanswered);
        for code in ["143", "137"] {
            running
                .harvester
                .on_docker_event(event("start", next(), &[service]));
            running
                .harvester
                .on_docker_event(event("kill", next(), &[service, ("signal", "15")]));
            running
                .harvester
                .on_docker_event(event("kill", next(), &[service, ("signal", "9")]));
            running
                .harvester
                .on_docker_event(event("die", next(), &[service, ("exitCode", code)]));
            running.settle_exits(|| Inspected::Unanswered);
        }
        assert_eq!(crashes(&host), []);
        assert_eq!(
            host.markers()
                .iter()
                .filter(|(_, kind)| matches!(kind, MarkerKind::Exit { stopped: true, .. }))
                .count(),
            2
        );

        running
            .harvester
            .on_docker_event(event("start", next(), &[service]));
        running
            .harvester
            .on_docker_event(event("die", next(), &[service, ("exitCode", "2")]));
        running.settle_exits(|| Inspected::Unanswered);
        assert_eq!(crashes(&host).len(), 1);
    }

    #[tokio::test]
    async fn system_containers_and_hooks_leave_no_marker() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let mut system = event("die", T0, &[]);
        system.actor.as_mut().unwrap().attributes = Some(HashMap::from([
            ("ployzd.managed".to_owned(), "true".to_owned()),
            ("name".to_owned(), "ployz-corrosion".to_owned()),
        ]));
        running.harvester.on_docker_event(system);
        running.harvester.on_docker_event(event(
            "die",
            T0 + 1,
            &[
                ("ployz.service.id", SERVICE_ID),
                ("ployz.service.hook", "pre-deploy"),
            ],
        ));
        running.harvester.on_docker_event(event(
            "oom",
            T0 + 2,
            &[("ployz.service.id", "not-an-id")],
        ));
        assert_eq!(host.markers(), []);
    }

    fn listing(dir: &Path) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|entry| {
                        let entry = entry.unwrap();
                        let name = entry.file_name().into_string().unwrap();
                        let text =
                            fs::read_to_string(entry.path()).unwrap_or_else(|_| "<dir>".to_owned());
                        (name, text)
                    })
                    .filter(|(name, _)| !name.ends_with(".tmp"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn pair(name: impl Into<String>, text: impl Into<String>) -> (String, String) {
        (name.into(), text.into())
    }

    fn marker_json(ts: i64, code: i64) -> String {
        format!(
            r#"{{"machine_id":"{}","container_id":"{ID}","service_id":"{SERVICE_ID}","service":"default/web","deployment_id":"d1","replica":"web-1","ts":{ts},"kind":"exit","exit_code":{code},"oom":false,"stopped":false}}"#,
            "a".repeat(32)
        )
    }

    fn pending_json(ts: i64, code: i64, seq: Option<i64>) -> String {
        let seq = seq.map_or_else(|| "null".to_owned(), |seq| seq.to_string());
        format!(r#"{{"marker":{},"seq":{seq}}}"#, marker_json(ts, code))
    }

    fn entry_json(seq: i64, ts: i64, code: i64) -> String {
        format!(r#"{{"seq":"{seq}","marker":{}}}"#, marker_json(ts, code))
    }

    fn outbox_name(seq: i64) -> String {
        format!("{seq:020}")
    }

    fn crash_die(at: i64) -> EventMessage {
        event(
            "die",
            at,
            &[("ployz.service.id", SERVICE_ID), ("exitCode", "1")],
        )
    }

    #[tokio::test]
    async fn a_settled_crash_leaves_these_files_byte_for_byte() {
        let host = host().with_machine();
        let mut running = host.harvester();
        let ts = T0 + 1;
        running.harvester.on_docker_event(crash_die(ts));
        running.settle_exits(|| Inspected::Gone);

        assert_eq!(
            listing(&host.store.settled()),
            [pair(
                format!("{ID}-exit-{ts}"),
                pending_json(ts, 1, Some(ts))
            )]
        );
        assert_eq!(
            listing(&host.store.crash_outbox()),
            [
                pair(outbox_name(ts), entry_json(ts, ts, 1)),
                pair("last_seq", ts.to_string()),
            ]
        );
    }

    // Each phase fails the next step of one settlement, so the files after it
    // show the steps that ran before it and none that run after.
    #[tokio::test]
    async fn a_crash_settles_its_steps_in_order_and_a_failed_step_leaves_the_files_before_it() {
        let host = host().with_machine();
        let ts = T0 + 1;
        let next = ts + 1;
        let name = format!("{ID}-exit-{ts}");
        let pending = format!("{name}.pending");
        let outbox = host.store.crash_outbox();
        let mut running = host.harvester();

        // No seq can be reserved.
        fs::remove_dir_all(&outbox).unwrap();
        fs::write(&outbox, b"").unwrap();
        running.harvester.on_docker_event(crash_die(ts));
        running.settle_exits(|| Inspected::Gone);
        assert_eq!(
            listing(&host.store.settled()),
            [pair(&pending, pending_json(ts, 1, None))]
        );

        // A seq is reserved but cannot be saved, so nothing is queued.
        fs::remove_file(&outbox).unwrap();
        fs::create_dir(&outbox).unwrap();
        let blocked = host.store.settled().join(format!("{name}.tmp"));
        fs::create_dir(&blocked).unwrap();
        running.harvester.finish_settlements();
        assert_eq!(
            listing(&host.store.settled()),
            [pair(&pending, pending_json(ts, 1, None))]
        );
        assert_eq!(
            listing(&outbox),
            [
                pair(format!("{}.reserved", outbox_name(ts)), ""),
                pair("last_seq", ts.to_string()),
            ]
        );

        // The reserved seq is saved but the crash cannot be queued under it.
        fs::remove_dir(&blocked).unwrap();
        fs::create_dir(outbox.join(outbox_name(next))).unwrap();
        running.harvester.finish_settlements();
        assert_eq!(
            listing(&host.store.settled()),
            [pair(&pending, pending_json(ts, 1, Some(next)))]
        );
        assert_eq!(
            listing(&outbox),
            [
                pair(outbox_name(next), "<dir>"),
                pair(format!("{}.reserved", outbox_name(next)), ""),
                pair("last_seq", next.to_string()),
            ]
        );

        // The crash is queued but the settlement cannot be finished.
        fs::remove_dir(outbox.join(outbox_name(next))).unwrap();
        let finished = host.store.settled().join(&name);
        fs::create_dir_all(finished.join("blocked")).unwrap();
        running.harvester.finish_settlements();
        assert_eq!(
            listing(&host.store.settled()),
            [
                pair(&name, "<dir>"),
                pair(&pending, pending_json(ts, 1, Some(next))),
            ]
        );
        assert_eq!(
            listing(&outbox),
            [
                pair(outbox_name(next), entry_json(next, ts, 1)),
                pair("last_seq", next.to_string()),
            ]
        );

        // Finishing it queues nothing again.
        fs::remove_dir_all(&finished).unwrap();
        running.harvester.finish_settlements();
        assert_eq!(
            listing(&host.store.settled()),
            [pair(&name, pending_json(ts, 1, Some(next)))]
        );
        assert_eq!(
            listing(&outbox),
            [
                pair(outbox_name(next), entry_json(next, ts, 1)),
                pair("last_seq", next.to_string()),
            ]
        );
        assert_eq!(host.markers().len(), 1);
    }

    struct Left {
        seq: Option<i64>,
        reserved: bool,
        queued: bool,
        code: i64,
    }

    const LEFT: Left = Left {
        seq: None,
        reserved: false,
        queued: false,
        code: 1,
    };

    // The files an earlier daemon wrote, which the next one reads after an
    // upgrade in the middle of a settlement.
    #[tokio::test]
    async fn a_settlement_an_earlier_daemon_left_resumes_from_where_it_stopped() {
        let ts = T0 + 1;
        let held = Some(ts);
        // Each case: what was left, the seqs queued after, and the seq the
        // finished settlement names.
        let cases = [
            ("no seq", LEFT, vec![ts], held),
            (
                "seq reserved",
                Left {
                    seq: held,
                    reserved: true,
                    ..LEFT
                },
                vec![ts],
                held,
            ),
            (
                "crash already queued",
                Left {
                    seq: held,
                    queued: true,
                    ..LEFT
                },
                vec![ts],
                held,
            ),
            (
                "reserved seq gone and nothing queued",
                Left { seq: held, ..LEFT },
                vec![],
                held,
            ),
            ("clean exit", Left { code: 0, ..LEFT }, vec![], None),
        ];
        for (label, left, queued_seqs, finished_seq) in cases {
            let host = host().with_machine();
            let name = format!("{ID}-exit-{ts}");
            let outbox = host.store.crash_outbox();
            fs::create_dir_all(&outbox).unwrap();
            fs::create_dir_all(host.store.settled()).unwrap();
            let json = pending_json(ts, left.code, left.seq);
            fs::write(host.store.settled().join(format!("{name}.pending")), &json).unwrap();
            if left.reserved {
                fs::write(outbox.join(format!("{}.reserved", outbox_name(ts))), b"").unwrap();
            }
            if left.queued {
                fs::write(outbox.join(outbox_name(ts)), entry_json(ts, ts, left.code)).unwrap();
            }
            let mut running = host.harvester();
            running.harvester.finish_settlements();

            let files = listing(&outbox);
            let queued: Vec<i64> = files
                .iter()
                .filter(|(name, _)| name.len() == 20)
                .map(|(name, _)| name.parse().unwrap())
                .collect();
            assert_eq!(queued, queued_seqs, "{label}");
            if !queued.is_empty() {
                assert_eq!(
                    files.first(),
                    Some(&pair(outbox_name(ts), entry_json(ts, ts, 1))),
                    "{label}"
                );
            }
            assert!(
                files.iter().all(|(name, _)| !name.ends_with(".reserved")),
                "{label}"
            );
            assert_eq!(
                listing(&host.store.settled()),
                [pair(&name, pending_json(ts, left.code, finished_seq))],
                "{label}"
            );
        }
    }

    enum Late {
        Event(&'static str, i64, &'static [(&'static str, &'static str)]),
        Settle,
    }

    /// The exits the store keeps after `steps`, as (ts, oom, stopped).
    fn exits_after(steps: &[Late]) -> Vec<(i64, bool, bool)> {
        let host = host().with_machine();
        let mut running = host.harvester();
        for step in steps {
            match step {
                Late::Event(action, at, extra) => {
                    let extra: Vec<(&str, &str)> = [("ployz.service.id", SERVICE_ID)]
                        .into_iter()
                        .chain(extra.iter().copied())
                        .collect();
                    running
                        .harvester
                        .on_docker_event(event(action, T0 + at, &extra));
                }
                Late::Settle => running.settle_exits(|| Inspected::Unanswered),
            }
        }
        running.settle_exits(|| Inspected::Unanswered);
        host.markers()
            .into_iter()
            .filter_map(|(ts, kind)| match kind {
                MarkerKind::Exit { oom, stopped, .. } => Some((ts, oom, stopped)),
                MarkerKind::Oom | MarkerKind::Restart => None,
            })
            .collect()
    }

    type Case = (&'static str, Vec<Late>, Vec<(i64, bool, bool)>);

    const TERM: &[(&str, &str)] = &[("signal", "15")];
    const HUP: &[(&str, &str)] = &[("signal", "1")];
    const NO_SIGNAL: &[(&str, &str)] = &[("signal", "not-a-signal")];
    const FAILED: &[(&str, &str)] = &[("exitCode", "1")];
    const NONE: &[(&str, &str)] = &[];

    // A late stop, oom or kill belongs to the live run, else to the last
    // exit still queued, and a kill belongs to no exit.
    #[tokio::test]
    async fn a_late_stop_oom_or_kill_belongs_to_the_live_run_else_the_last_queued_exit() {
        use Late::{Event, Settle};
        let t = |at| T0 + at;
        let cases: Vec<Case> = vec![
            (
                "a kill of the live run stops its exit",
                vec![
                    Event("start", 1, NONE),
                    Event("kill", 2, TERM),
                    Event("die", 3, FAILED),
                ],
                vec![(t(3), false, true)],
            ),
            (
                "a kill after the exit belongs to no exit",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("kill", 3, TERM),
                ],
                vec![(t(2), false, false)],
            ),
            (
                "a kill after the exit is not taken by the next exit",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("kill", 3, TERM),
                    Event("die", 4, FAILED),
                ],
                vec![(t(2), false, false), (t(4), false, false)],
            ),
            (
                "a signal Docker does not name is no kill",
                vec![
                    Event("start", 1, NONE),
                    Event("kill", 2, NO_SIGNAL),
                    Event("stop", 3, NONE),
                    Event("die", 4, FAILED),
                ],
                vec![(t(4), false, false)],
            ),
            (
                "a stop after the exit belongs to the queued exit",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("stop", 3, NONE),
                ],
                vec![(t(2), false, true)],
            ),
            (
                "a stop of a live run nothing signalled and with no exit queued is lost",
                vec![
                    Event("start", 1, NONE),
                    Event("stop", 2, NONE),
                    Event("die", 3, FAILED),
                ],
                vec![(t(3), false, false)],
            ),
            (
                "a stop of a live run signalled with a signal that does not stop belongs to it",
                vec![
                    Event("start", 1, NONE),
                    Event("kill", 2, HUP),
                    Event("stop", 3, NONE),
                    Event("die", 4, FAILED),
                ],
                vec![(t(4), false, true)],
            ),
            (
                "a stop after the next start belongs to the queued exit of the run before",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("start", 3, NONE),
                    Event("stop", 4, NONE),
                    Event("die", 5, FAILED),
                ],
                vec![(t(2), false, true), (t(5), false, false)],
            ),
            (
                "a stop belongs to the last of two queued exits",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("start", 3, NONE),
                    Event("die", 4, FAILED),
                    Event("stop", 5, NONE),
                ],
                vec![(t(2), false, false), (t(4), false, true)],
            ),
            (
                "an oom of the live run is its exit's",
                vec![
                    Event("start", 1, NONE),
                    Event("oom", 2, NONE),
                    Event("die", 3, FAILED),
                ],
                vec![(t(3), true, false)],
            ),
            (
                "an oom after the exit belongs to the queued exit",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("oom", 3, NONE),
                ],
                vec![(t(2), true, false)],
            ),
            (
                "an oom belongs to the last of two queued exits",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("start", 3, NONE),
                    Event("die", 4, FAILED),
                    Event("oom", 5, NONE),
                ],
                vec![(t(2), false, false), (t(4), true, false)],
            ),
            (
                "an oom of a later run is not its earlier exit's",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Event("start", 3, NONE),
                    Event("oom", 4, NONE),
                    Event("die", 5, FAILED),
                ],
                vec![(t(2), false, false), (t(5), true, false)],
            ),
            (
                "an oom after its exit settled changes that exit no more",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Settle,
                    Event("oom", 3, NONE),
                ],
                vec![(t(2), false, false)],
            ),
            (
                "a stop after its exit settled changes that exit no more",
                vec![
                    Event("start", 1, NONE),
                    Event("die", 2, FAILED),
                    Settle,
                    Event("stop", 3, NONE),
                ],
                vec![(t(2), false, false)],
            ),
        ];
        for (label, steps, expected) in cases {
            assert_eq!(exits_after(&steps), expected, "{label}");
        }
    }

    #[test]
    fn docker_resumes_events_from_a_fractional_second() {
        assert_eq!(
            super::docker_time(1_760_000_000_000_000_042),
            "1760000000.000000042"
        );
    }

    #[tokio::test]
    async fn a_container_has_one_inspect_in_flight_and_events_queue_one_more() {
        let host = host().with_machine();
        let id = host.id();
        let mut running = host.harvester();
        let harvester = &mut running.harvester;
        harvester.on_docker_event(event("start", T0 + 4, &[]));
        assert_eq!(harvester.inspecting.get(&id), Some(&false));
        harvester.on_docker_event(event("die", T0 + 5, &[("exitCode", "1")]));
        harvester.on_docker_event(event("start", T0 + 6, &[]));
        harvester.on_docker_event(event("die", T0 + 7, &[("exitCode", "2")]));
        assert_eq!(harvester.inspecting.get(&id), Some(&true));
        assert_eq!(harvester.inspecting.len(), 1);
        harvester.on_inspected(id, inspected(&running_after(6, 5)));
        assert_eq!(harvester.inspecting.get(&id), Some(&false));
        harvester.on_inspected(id, inspected(&exited(6, 7, 2, false)));
        assert_eq!(harvester.inspecting.get(&id), None);
        assert!(!harvester.ledger.waits_on(&id));
    }
}
