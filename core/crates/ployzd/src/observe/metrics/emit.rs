//! Every 10 s, each Volume's IO and the Machine's disk breakdown as OTLP metrics to the
//! Collector's local receiver, which ships them to Cloud beside the metrics it scrapes.
//!
//! The Collector runs only on a Machine connected to Cloud, so elsewhere a send is
//! refused and the reading is dropped.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ployz_core::{MachineLayout, Namespace};
use serde_json::{Value, json};
use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};

use super::{
    disk::{self, DiskBreakdown, SlowReading},
    zfs_io::{self, DatasetIo},
    zfs_usage::{ZFS_LIST_TIMEOUT, ZfsUsageReader},
};
use crate::{dataset_layout::Place, observe::layout::StoreRoot};

const INTERVAL: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const ENDPOINT: &str = "http://127.0.0.1:4320/v1/metrics";

/// Emits until the task is dropped.
pub(crate) async fn run(store: StoreRoot) {
    let (latest, slow) = watch::channel(SlowReading::default());
    let zfs = ZfsUsageReader::new(PathBuf::from("zfs"), ZFS_LIST_TIMEOUT);
    tokio::join!(
        disk::run(
            store.clone(),
            zfs,
            MachineLayout::production().machine_pool_file(),
            latest,
        ),
        emit_every_interval(store, slow),
    );
}

async fn emit_every_interval(store: StoreRoot, slow: watch::Receiver<SlowReading>) {
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        .expect("a plain HTTP client builds");
    let mut volumes = VolumeSeries::default();
    let mut ticks = interval(INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let at = now_nanos();
        let datasets = zfs_io::read(std::path::Path::new(zfs_io::KSTAT_ROOT));
        let disk = disk::filesystem(store.path()).map(|fs| disk::breakdown(fs, *slow.borrow()));
        let body = body(at, &volumes.read(at, datasets), disk);
        match client.post(ENDPOINT).json(&body).send().await {
            Ok(response) if !response.status().is_success() => {
                tracing::debug!(status = %response.status(), "the Collector refused ployzd's metrics");
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "cannot send ployzd's metrics to the Collector"),
        }
    }
}

/// One Volume's IO since its series started.
#[derive(Debug, PartialEq, Eq)]
struct VolumeIo {
    namespace: Namespace,
    volume: String,
    start_nanos: i64,
    io: DatasetIo,
}

/// When each Volume's cumulative series started. A kstat counts from its dataset's
/// mount, so a counter below its last reading starts a new series.
#[derive(Default)]
struct VolumeSeries(HashMap<String, (i64, DatasetIo)>);

impl VolumeSeries {
    fn read(
        &mut self,
        at: i64,
        datasets: std::collections::BTreeMap<String, DatasetIo>,
    ) -> Vec<VolumeIo> {
        let mut read = Vec::new();
        let mut seen = HashMap::new();
        for (dataset, io) in datasets {
            let Some((namespace, volume)) = Place::in_any_pool(&dataset)
                .writer_root()
                .and_then(|physical| physical.split_once('_'))
                .and_then(|(namespace, volume)| Some((Namespace::parse(namespace).ok()?, volume)))
            else {
                continue;
            };
            let start_nanos = match self.0.get(&dataset) {
                Some((start, last))
                    if io.read_bytes >= last.read_bytes && io.write_bytes >= last.write_bytes =>
                {
                    *start
                }
                _ => at,
            };
            read.push(VolumeIo {
                namespace,
                volume: volume.to_owned(),
                start_nanos,
                io,
            });
            seen.insert(dataset, (start_nanos, io));
        }
        self.0 = seen;
        read
    }
}

fn body(at: i64, volumes: &[VolumeIo], disk: Option<DiskBreakdown>) -> Value {
    let attribute =
        |key: &str, value: &str| json!({ "key": key, "value": { "stringValue": value } });
    let point = |attributes: Vec<Value>, start: Option<i64>, value: u64| {
        let mut point = serde_json::Map::new();
        point.insert("attributes".into(), json!(attributes));
        point.insert("timeUnixNano".into(), json!(at.to_string()));
        point.insert("asInt".into(), json!(value.to_string()));
        if let Some(start) = start {
            point.insert("startTimeUnixNano".into(), json!(start.to_string()));
        }
        Value::Object(point)
    };
    let volume_sum = |name: &str, bytes: fn(&DatasetIo) -> u64| {
        let points: Vec<Value> = volumes
            .iter()
            .map(|volume| {
                point(
                    vec![
                        attribute("ployz.namespace", volume.namespace.as_str()),
                        attribute("ployz.volume", &volume.volume),
                    ],
                    Some(volume.start_nanos),
                    bytes(&volume.io),
                )
            })
            .collect();
        json!({
            "name": name,
            "unit": "By",
            "sum": { "aggregationTemporality": 2, "isMonotonic": true, "dataPoints": points },
        })
    };
    let mut metrics = vec![
        volume_sum("ployz.volume.io.read_bytes", |io| io.read_bytes),
        volume_sum("ployz.volume.io.write_bytes", |io| io.write_bytes),
    ];
    if let Some(disk) = disk {
        let points: Vec<Value> = [
            ("total", Some(disk.total)),
            ("free", Some(disk.free)),
            ("volumes", disk.volumes),
            ("mirror_slots", disk.mirror_slots),
            ("observe_store", disk.observe_store),
            ("other", disk.other),
        ]
        .into_iter()
        .filter_map(|(kind, bytes)| Some(point(vec![attribute("kind", kind)], None, bytes?)))
        .collect();
        metrics.push(json!({
            "name": "ployz.disk.bytes",
            "unit": "By",
            "gauge": { "dataPoints": points },
        }));
    }
    json!({
        "resourceMetrics": [{
            "resource": { "attributes": [attribute("service.name", "ployzd")] },
            "scopeMetrics": [{ "scope": { "name": "ployzd" }, "metrics": metrics }],
        }]
    })
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::Namespace;

    use super::{DatasetIo, VolumeIo, VolumeSeries};

    fn io(read_bytes: u64, write_bytes: u64) -> DatasetIo {
        DatasetIo {
            read_bytes,
            write_bytes,
        }
    }

    #[test]
    fn a_volume_is_named_by_its_namespace_and_a_remount_starts_a_new_series() {
        let mut series = VolumeSeries::default();
        let datasets = |data: DatasetIo| {
            BTreeMap::from([
                ("tank/ployz/prod_data".to_owned(), data),
                ("tank/ployz-mirror/prod_data/fs".to_owned(), io(9, 9)),
                ("tank/other".to_owned(), io(9, 9)),
            ])
        };
        let volume = |start_nanos, io| VolumeIo {
            namespace: Namespace::parse("prod").unwrap(),
            volume: "data".to_owned(),
            start_nanos,
            io,
        };
        assert_eq!(
            series.read(1, datasets(io(10, 20))),
            [volume(1, io(10, 20))]
        );
        assert_eq!(
            series.read(2, datasets(io(15, 25))),
            [volume(1, io(15, 25))]
        );
        assert_eq!(series.read(3, datasets(io(15, 4))), [volume(3, io(15, 4))]);
    }
}
