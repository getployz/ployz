//! Every 10 s, one raw row per running Ployz container and one for the
//! Machine, appended to the hour's segment. At start and on the first tick of
//! each hour, closed hours are folded into rollups.
//!
//! Docker is asked only which containers run and, once per new container,
//! for its pid and labels. Counters come from files: the cgroup named by the
//! `0::` line of `/proc/<pid>/cgroup`, the pid's `net/dev`, and the host's
//! procfs. A container that is gone yields no row rather than zeros.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use bollard::{
    Docker, models::ContainerInspectResponse, query_parameters::ListContainersOptionsBuilder,
};
use ployz_core::{
    ContainerId, MachineId, Namespace, QualifiedService, ServiceId, ServiceName,
    metrics::{CpuJiffies, HostMemory, HostSample, InterfaceBytes, MetricSample},
};
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::{
    cgroup::{self, MemoryLimit},
    host::{self, HostCounters},
    net::{self, NetCounters},
    rollup,
    segment::{self, Row},
};
use crate::{
    docker::{LABEL_HOOK, LABEL_MANAGED, LABEL_SERVICE_ID},
    machine::LocalMachineRecord,
    observe::{
        harvest,
        layout::{SegmentName, StoreRoot},
    },
};

const INTERVAL: Duration = Duration::from_secs(10);
const DOCKER_TIMEOUT: Duration = Duration::from_secs(5);
const LABEL_SYSTEM_MANAGED: &str = "ployzd.managed";
const MACHINE_FILE: &str = "machine.json";
const CGROUP_V2_PREFIX: &str = "0::";
const LOOPBACK: &str = "lo";
/// The host end of each container's veth pair. Its bytes are the
/// container's, already in that container's row and again on the uplink.
const VETH_PREFIX: &str = "veth";

/// Where the sampler reads procfs and the cgroup v2 hierarchy.
#[derive(Clone, Debug)]
struct Roots {
    proc: PathBuf,
    cgroup: PathBuf,
}

impl Roots {
    fn system() -> Self {
        Self {
            proc: PathBuf::from("/proc"),
            cgroup: PathBuf::from("/sys/fs/cgroup"),
        }
    }
}

/// A running container the sampler reads, as Docker described it when the
/// sampler first saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tracked {
    id: ContainerId,
    pid: u32,
    /// The `0::` path of the pid's cgroup, relative to the hierarchy root.
    cgroup: String,
    labels: Labels,
    /// Shares the Machine's network namespace, whose `net/dev` holds every
    /// interface on the Machine rather than the container's own traffic.
    host_network: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Labels {
    /// Both present for a Service's container, both absent for a system one.
    service_id: Option<ServiceId>,
    service: Option<QualifiedService>,
    deployment_id: Option<String>,
    replica: String,
}

/// Samples until the task is dropped.
pub(crate) async fn run(docker: Docker, store: StoreRoot, data_dir: PathBuf) {
    let roots = Roots::system();
    let mut tracked: HashMap<ContainerId, Tracked> = HashMap::new();
    let mut ticks = interval(INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut folded: Option<SegmentName> = None;
    loop {
        ticks.tick().await;
        refresh(&docker, &roots, &mut tracked).await;
        let containers: Vec<Tracked> = tracked.values().cloned().collect();
        let (roots, store, data_dir) = (roots.clone(), store.clone(), data_dir.clone());
        let ts = harvest::now_nanos();
        let hour = SegmentName::containing(ts);
        let fold = hour != folded;
        folded = hour;
        let tick = tokio::task::spawn_blocking(move || {
            let gone = tick(&roots, &store, &data_dir, &containers, ts);
            if fold && let Err(error) = rollup::fold_pending(&store, SystemTime::now()) {
                tracing::warn!(%error, "cannot fold metric hours");
            }
            gone
        });
        match tick.await {
            Ok(Ok(gone)) => {
                for id in gone {
                    tracked.remove(&id);
                }
            }
            Ok(Err(error)) => tracing::warn!(%error, "cannot append metric rows"),
            Err(error) => tracing::warn!(%error, "metric sample task failed"),
        }
    }
}

/// Brings `tracked` in line with the Ployz containers Docker runs. When
/// Docker does not answer, the last known set is sampled; a container that
/// stopped meanwhile drops out because its pid no longer names its cgroup.
async fn refresh(docker: &Docker, roots: &Roots, tracked: &mut HashMap<ContainerId, Tracked>) {
    let options = ListContainersOptionsBuilder::default().build();
    let summaries = match timeout(DOCKER_TIMEOUT, docker.list_containers(Some(options))).await {
        Ok(Ok(summaries)) => summaries,
        Ok(Err(error)) => {
            tracing::debug!(%error, "cannot list containers to sample");
            return;
        }
        Err(_) => {
            tracing::debug!("listing containers to sample timed out");
            return;
        }
    };
    let running: HashSet<ContainerId> = summaries
        .into_iter()
        .filter(|summary| summary.labels.as_ref().is_some_and(sampled))
        .filter_map(|summary| ContainerId::parse(summary.id?).ok())
        .collect();
    tracked.retain(|id, _| running.contains(id));
    for id in running {
        if tracked.contains_key(&id) {
            continue;
        }
        let inspect = docker.inspect_container(id.as_str(), None);
        let inspected = match timeout(DOCKER_TIMEOUT, inspect).await {
            Ok(Ok(inspected)) => inspected,
            Ok(Err(error)) => {
                tracing::debug!(container = %id, %error, "cannot inspect container to sample");
                continue;
            }
            Err(_) => {
                tracing::debug!(container = %id, "inspecting container to sample timed out");
                continue;
            }
        };
        if let Some(container) = track(&roots.proc, id, &inspected) {
            tracked.insert(id, container);
        }
    }
}

/// Ployz's Service and system containers. A pre-deploy Hook carries its
/// Service's labels but is not one of its replicas, so it stays out of the
/// Service's numbers.
fn sampled(labels: &HashMap<String, String>) -> bool {
    (labels.contains_key(LABEL_MANAGED) || labels.contains_key(LABEL_SYSTEM_MANAGED))
        && !labels.contains_key(LABEL_HOOK)
}

fn track(
    proc_root: &Path,
    id: ContainerId,
    inspected: &ContainerInspectResponse,
) -> Option<Tracked> {
    let meta = harvest::container_meta(inspected)?;
    let pid = inspected
        .state
        .as_ref()?
        .pid
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)?;
    let service_id = inspected
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(LABEL_SERVICE_ID))
        .and_then(|label| ServiceId::parse(label.clone()).ok());
    let service = meta.namespace.as_deref().zip(meta.service.as_deref());
    let service = service.and_then(|(namespace, name)| {
        Some(QualifiedService::new(
            Namespace::parse(namespace).ok()?,
            ServiceName::parse(name).ok()?,
        ))
    });
    // A row names its Service both ways or neither, so a half-labelled
    // container is booked like a system one.
    let (service_id, service) = match (service_id, service) {
        (Some(id), Some(service)) => (Some(id), Some(service)),
        _ => (None, None),
    };
    Some(Tracked {
        id,
        pid,
        cgroup: cgroup_path(proc_root, pid)?,
        labels: Labels {
            service_id,
            service,
            deployment_id: meta.deployment,
            replica: meta.replica,
        },
        host_network: inspected
            .host_config
            .as_ref()
            .and_then(|host| host.network_mode.as_deref())
            == Some("host"),
    })
}

/// The pid's cgroup v2 path. Under the systemd driver Docker leaves
/// `CgroupParent` empty, so the kernel's own record is the one to trust.
fn cgroup_path(proc_root: &Path, pid: u32) -> Option<String> {
    let membership = fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    membership
        .lines()
        .find_map(|line| line.strip_prefix(CGROUP_V2_PREFIX))
        .map(str::to_owned)
}

/// Reads one sample and appends it. Returns the containers that are gone.
fn tick(
    roots: &Roots,
    store: &StoreRoot,
    data_dir: &Path,
    containers: &[Tracked],
    ts: i64,
) -> io::Result<Vec<ContainerId>> {
    let Some(machine_id) = machine_id(data_dir) else {
        tracing::debug!("no Machine id yet; skipping the metric sample");
        return Ok(Vec::new());
    };
    let (rows, gone) = sample(roots, machine_id, ts, containers);
    segment::append(store, &rows)?;
    Ok(gone)
}

/// Read every tick: a reset gives the Machine a new id.
pub(in crate::observe) fn machine_id(data_dir: &Path) -> Option<MachineId> {
    let bytes = fs::read(data_dir.join(MACHINE_FILE)).ok()?;
    serde_json::from_slice::<LocalMachineRecord>(&bytes)
        .ok()
        .map(|record| record.id())
}

/// One row per live container, then the Machine's row. M7 fills the
/// Machine row's `disk`. Marker rows come from the harvester's Docker events.
fn sample(
    roots: &Roots,
    machine_id: MachineId,
    ts: i64,
    containers: &[Tracked],
) -> (Vec<Row>, Vec<ContainerId>) {
    let mut rows = Vec::with_capacity(containers.len() + 1);
    let mut gone = Vec::new();
    for container in containers {
        // A pid that exited, or was reused by another process, no longer
        // names the container's cgroup.
        if cgroup_path(&roots.proc, container.pid).as_deref() != Some(container.cgroup.as_str()) {
            gone.push(container.id);
            continue;
        }
        let dir = roots.cgroup.join(container.cgroup.trim_start_matches('/'));
        let counters = cgroup::read_cgroup(&dir);
        let net = (!container.host_network)
            .then(|| net::read_net_dev(&roots.proc, container.pid))
            .flatten();
        rows.push(Row::Container(container_sample(
            machine_id, ts, container, &counters, net,
        )));
    }
    rows.push(Row::Host(host_sample(
        machine_id,
        ts,
        host::read_host(&roots.proc, &[]),
    )));
    (rows, gone)
}

fn container_sample(
    machine_id: MachineId,
    ts: i64,
    container: &Tracked,
    counters: &cgroup::CgroupCounters,
    net: Option<NetCounters>,
) -> MetricSample {
    let labels = &container.labels;
    MetricSample {
        machine_id,
        container_id: container.id,
        service_id: labels.service_id,
        service: labels.service.clone(),
        deployment_id: labels.deployment_id.clone(),
        replica: labels.replica.clone(),
        ts,
        cpu_usec: counters.cpu_usec,
        mem_bytes: counters.mem_bytes,
        mem_limit: match counters.mem_limit {
            Some(MemoryLimit::Bytes(bytes)) => Some(bytes),
            Some(MemoryLimit::Unlimited) | None => None,
        },
        rx: net.map(|net| net.rx),
        tx: net.map(|net| net.tx),
        io_r: counters.io.map(|io| io.read),
        io_w: counters.io.map(|io| io.write),
    }
}

/// Loopback and veth pairs are left out: neither leaves the Machine, and one
/// veth per container would grow every row with the container count.
fn host_sample(machine_id: MachineId, ts: i64, host: HostCounters) -> HostSample {
    HostSample {
        machine_id,
        ts,
        cpu: host.cpu.map(|cpu| CpuJiffies {
            busy: cpu.busy,
            idle: cpu.idle,
        }),
        memory: match (host.memory.total, host.memory.available) {
            (Some(total), Some(available)) => Some(HostMemory { total, available }),
            _ => None,
        },
        interfaces: host
            .interfaces
            .unwrap_or_default()
            .into_iter()
            .filter(|interface| {
                interface.name != LOOPBACK && !interface.name.starts_with(VETH_PREFIX)
            })
            .map(|interface| {
                (
                    interface.name,
                    InterfaceBytes {
                        rx: interface.rx,
                        tx: interface.tx,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        disk: None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{MetricRange, MetricScope},
    };

    use super::{Labels, Roots, Tracked, cgroup_path, sample, tick};
    use crate::{
        machine::{LocalMachineBody, LocalMachineRecord},
        network::WireGuardPrivateKey,
        observe::{
            layout::StoreRoot,
            metrics::segment::{self, Row, Selected},
        },
        test_dir::TestDir,
    };

    const SEC: i64 = 1_000_000_000;
    /// 2025-10-09T08:00:00Z.
    const T0: i64 = 1_759_996_800 * SEC;
    const NET_DEV_HEADER: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
";

    struct Fixture {
        dir: TestDir,
        roots: Roots,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TestDir::new("ployzd-metrics-sampler");
            let roots = Roots {
                proc: dir.0.join("proc"),
                cgroup: dir.0.join("cgroup"),
            };
            fs::create_dir_all(&roots.proc).unwrap();
            fs::create_dir_all(&roots.cgroup).unwrap();
            fs::write(roots.proc.join("stat"), "cpu  100 0 50 800 20 0 5 0 0 0\n").unwrap();
            fs::write(
                roots.proc.join("meminfo"),
                "MemTotal:        2048 kB\nMemAvailable:    1024 kB\n",
            )
            .unwrap();
            fs::create_dir_all(roots.proc.join("net")).unwrap();
            fs::write(
                roots.proc.join("net/dev"),
                format!(
                    "{NET_DEV_HEADER}    lo: 9 1 0 0 0 0 0 0 9 1 0 0 0 0 0 0\n  eth0: 500 5 0 0 0 0 0 0 600 6 0 0 0 0 0 0\nployz-wg: 70 1 0 0 0 0 0 0 80 1 0 0 0 0 0 0\nveth1a2b: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n"
                ),
            )
            .unwrap();
            Self { dir, roots }
        }

        /// A running container: its pid's cgroup line, its namespace's
        /// `net/dev`, and its cgroup counters.
        fn start(&self, n: u32) -> Tracked {
            let pid = 1000 + n;
            let cgroup = format!("/system.slice/docker-{n}.scope");
            let proc_dir = self.roots.proc.join(pid.to_string());
            fs::create_dir_all(proc_dir.join("net")).unwrap();
            fs::write(proc_dir.join("cgroup"), format!("0::{cgroup}\n")).unwrap();
            fs::write(
                proc_dir.join("net/dev"),
                format!(
                    "{NET_DEV_HEADER}    lo: 9 1 0 0 0 0 0 0 9 1 0 0 0 0 0 0\n  eth0: {} 5 0 0 0 0 0 0 {} 6 0 0 0 0 0 0\n",
                    100 * n,
                    200 * n
                ),
            )
            .unwrap();
            let dir = self.roots.cgroup.join(cgroup.trim_start_matches('/'));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("cpu.stat"), format!("usage_usec {}\n", 10 * n)).unwrap();
            fs::write(dir.join("memory.current"), "4096\n").unwrap();
            fs::write(dir.join("memory.stat"), "inactive_file 1024\n").unwrap();
            fs::write(dir.join("memory.max"), "max\n").unwrap();
            fs::write(dir.join("io.stat"), "8:0 rbytes=7 wbytes=9 rios=1 wios=1\n").unwrap();
            assert_eq!(
                cgroup_path(&self.roots.proc, pid).as_deref(),
                Some(cgroup.as_str())
            );
            Tracked {
                id: container(n),
                pid,
                cgroup,
                labels: Labels {
                    service_id: Some(service(n)),
                    service: Some(qualified(n)),
                    deployment_id: Some(format!("deploy-{n}")),
                    replica: format!("web-{n}"),
                },
                host_network: false,
            }
        }

        /// The container exits: the kernel drops its pid and its cgroup.
        fn stop(&self, container: &Tracked) {
            fs::remove_dir_all(self.roots.proc.join(container.pid.to_string())).unwrap();
            fs::remove_dir_all(
                self.roots
                    .cgroup
                    .join(container.cgroup.trim_start_matches('/')),
            )
            .unwrap();
        }

        fn store(&self) -> StoreRoot {
            let docker_root = self.dir.0.join("docker");
            fs::create_dir_all(&docker_root).unwrap();
            let store = StoreRoot::under(&docker_root);
            store.prepare().unwrap();
            store
        }

        fn data_dir(&self, machine: Option<MachineId>) -> std::path::PathBuf {
            let data_dir = self.dir.0.join("data");
            fs::create_dir_all(&data_dir).unwrap();
            if let Some(machine) = machine {
                let record = LocalMachineRecord::parse(
                    LocalMachineBody::Uninitialized { id: machine },
                    WireGuardPrivateKey::from_bytes([0; 32]),
                )
                .unwrap();
                fs::write(
                    data_dir.join(super::MACHINE_FILE),
                    serde_json::to_vec(&record).unwrap(),
                )
                .unwrap();
            }
            data_dir
        }
    }

    fn machine() -> MachineId {
        MachineId::parse("a".repeat(32)).unwrap()
    }

    fn service(n: u32) -> ServiceId {
        ServiceId::parse(format!("{n:02x}").repeat(16)).unwrap()
    }

    fn qualified(n: u32) -> QualifiedService {
        QualifiedService::parse(format!("default/web-{n}")).unwrap()
    }

    fn container(n: u32) -> ContainerId {
        ContainerId::parse(format!("{n:08x}").repeat(8)).unwrap()
    }

    #[test]
    fn ployz_containers_are_sampled_and_hooks_are_not() {
        use crate::docker::{LABEL_HOOK, LABEL_MANAGED};
        let labels = |keys: &[&str]| -> std::collections::HashMap<String, String> {
            keys.iter()
                .map(|key| ((*key).to_owned(), String::new()))
                .collect()
        };
        assert!(super::sampled(&labels(&[LABEL_MANAGED])));
        assert!(super::sampled(&labels(&[super::LABEL_SYSTEM_MANAGED])));
        assert!(
            !super::sampled(&labels(&[LABEL_MANAGED, LABEL_HOOK])),
            "a pre-deploy Hook is not a replica"
        );
        assert!(!super::sampled(&labels(&["com.example.other"])));
    }

    #[test]
    fn a_container_that_disappears_between_ticks_yields_no_row() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let data_dir = fixture.data_dir(Some(machine()));
        let web = fixture.start(1);
        let api = fixture.start(2);
        let containers = vec![web.clone(), api.clone()];

        let gone = tick(&fixture.roots, &store, &data_dir, &containers, T0).unwrap();
        assert!(gone.is_empty());

        fixture.stop(&api);
        let gone = tick(
            &fixture.roots,
            &store,
            &data_dir,
            &containers,
            T0 + 10 * SEC,
        )
        .unwrap();
        assert_eq!(gone, vec![api.id]);

        let range = MetricRange {
            since_nanos: T0,
            until_nanos: T0 + 20 * SEC,
        };
        let rows_of = |n: u32| {
            let Selected::Service(rows) = segment::read(
                &store,
                range,
                &MetricScope::Service {
                    service: qualified(n),
                },
            )
            .unwrap() else {
                panic!("a Service scope selects Service rows");
            };
            rows
        };
        let web_rows = rows_of(1);
        assert_eq!(
            web_rows.iter().map(|row| row.ts).collect::<Vec<_>>(),
            [T0, T0 + 10 * SEC]
        );
        let api_rows = rows_of(2);
        assert_eq!(api_rows.iter().map(|row| row.ts).collect::<Vec<_>>(), [T0]);

        let row = web_rows.first().unwrap();
        assert_eq!(row.machine_id, machine());
        assert_eq!(row.container_id, web.id);
        assert_eq!(row.service_id, Some(service(1)));
        assert_eq!(row.service, Some(qualified(1)));
        assert_eq!(row.deployment_id.as_deref(), Some("deploy-1"));
        assert_eq!(row.replica, "web-1");
        assert_eq!(row.cpu_usec, Some(10));
        assert_eq!(row.mem_bytes, Some(3072));
        assert_eq!(row.mem_limit, None, "an unlimited cgroup has no limit");
        assert_eq!((row.rx, row.tx), (Some(100), Some(200)));
        assert_eq!((row.io_r, row.io_w), (Some(7), Some(9)));

        let Selected::Machine(hosts) = segment::read(&store, range, &MetricScope::Machine).unwrap()
        else {
            panic!("the Machine scope selects host rows");
        };
        assert_eq!(hosts.len(), 2);
        let host = hosts.first().unwrap();
        assert_eq!(host.cpu.map(|cpu| (cpu.busy, cpu.idle)), Some((155, 820)));
        assert_eq!(
            host.memory.map(|memory| (memory.total, memory.available)),
            Some((2048 * 1024, 1024 * 1024))
        );
        assert_eq!(
            host.interfaces
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["eth0", "ployz-wg"],
            "the Machine keeps its mesh interface apart and drops loopback and veths"
        );
        assert_eq!(host.disk, None);
    }

    #[test]
    fn a_reused_pid_in_another_cgroup_counts_as_gone() {
        let fixture = Fixture::new();
        let web = fixture.start(1);
        fs::write(
            fixture.roots.proc.join(web.pid.to_string()).join("cgroup"),
            "0::/user.slice/session-1.scope\n",
        )
        .unwrap();
        let (rows, gone) = sample(&fixture.roots, machine(), T0, std::slice::from_ref(&web));
        assert_eq!(gone, vec![web.id]);
        assert!(matches!(rows.as_slice(), [Row::Host(_)]));
    }

    #[test]
    fn a_missing_counter_stays_unknown() {
        let fixture = Fixture::new();
        let web = fixture.start(1);
        let dir = fixture
            .roots
            .cgroup
            .join(web.cgroup.trim_start_matches('/'));
        fs::remove_file(dir.join("io.stat")).unwrap();
        fs::remove_file(fixture.roots.proc.join(web.pid.to_string()).join("net/dev")).unwrap();
        let (rows, gone) = sample(&fixture.roots, machine(), T0, &[web]);
        assert!(gone.is_empty());
        let Some(Row::Container(row)) = rows.first() else {
            panic!("the first row is the container's");
        };
        assert_eq!(row.cpu_usec, Some(10));
        assert_eq!((row.io_r, row.io_w), (None, None));
        assert_eq!((row.rx, row.tx), (None, None));
    }

    #[test]
    fn a_service_container_is_keyed_by_name_as_well_as_id() {
        use crate::docker::{LABEL_MANAGED, LABEL_NAMESPACE, LABEL_SERVICE_ID, LABEL_SERVICE_NAME};
        use bollard::models::{ContainerConfig, ContainerInspectResponse, ContainerState};

        let fixture = Fixture::new();
        let web = fixture.start(1);
        let inspect = |labels: &[(&str, String)]| ContainerInspectResponse {
            config: Some(ContainerConfig {
                labels: Some(
                    labels
                        .iter()
                        .map(|(key, value)| ((*key).to_owned(), value.clone()))
                        .collect(),
                ),
                ..Default::default()
            }),
            state: Some(ContainerState {
                pid: Some(i64::from(web.pid)),
                running: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        let labelled = inspect(&[
            (LABEL_MANAGED, String::new()),
            (LABEL_SERVICE_ID, service(1).to_string()),
            (LABEL_NAMESPACE, "default".to_owned()),
            (LABEL_SERVICE_NAME, "web-1".to_owned()),
        ]);
        let tracked = super::track(&fixture.roots.proc, web.id, &labelled).unwrap();
        assert_eq!(tracked.labels.service_id, Some(service(1)));
        assert_eq!(tracked.labels.service, Some(qualified(1)));

        let half = inspect(&[
            (LABEL_MANAGED, String::new()),
            (LABEL_SERVICE_ID, service(1).to_string()),
        ]);
        let tracked = super::track(&fixture.roots.proc, web.id, &half).unwrap();
        assert_eq!(
            (tracked.labels.service_id, tracked.labels.service),
            (None, None),
            "a container without a Service name is booked like a system one"
        );
    }

    #[test]
    fn a_host_network_container_books_no_network_bytes() {
        use bollard::models::{
            ContainerConfig, ContainerInspectResponse, ContainerState, HostConfig,
        };

        let fixture = Fixture::new();
        let corrosion = fixture.start(1);
        let inspected = ContainerInspectResponse {
            config: Some(ContainerConfig {
                labels: Some([(super::LABEL_SYSTEM_MANAGED.to_owned(), String::new())].into()),
                ..Default::default()
            }),
            state: Some(ContainerState {
                pid: Some(i64::from(corrosion.pid)),
                running: Some(true),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                network_mode: Some("host".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let tracked = super::track(&fixture.roots.proc, corrosion.id, &inspected).unwrap();

        let (rows, _) = sample(&fixture.roots, machine(), T0, &[tracked]);
        let Some(Row::Container(row)) = rows.first() else {
            panic!("the first row is the container's");
        };
        assert_eq!(row.cpu_usec, Some(10));
        assert_eq!(
            (row.rx, row.tx),
            (None, None),
            "the Machine's interfaces are not the container's traffic"
        );
    }

    #[test]
    fn no_machine_id_skips_the_tick() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let data_dir = fixture.data_dir(None);
        let web = fixture.start(1);
        let gone = tick(&fixture.roots, &store, &data_dir, &[web], T0).unwrap();
        assert!(gone.is_empty());
        let Selected::Machine(hosts) = segment::read(
            &store,
            MetricRange {
                since_nanos: T0,
                until_nanos: T0 + SEC,
            },
            &MetricScope::Machine,
        )
        .unwrap() else {
            panic!("the Machine scope selects host rows");
        };
        assert!(hosts.is_empty());
    }
}
