use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use ployz_core::{ContainerId, MachineId};

use super::{
    frame::{self, Event, Frames, Stream},
    harvest::Harvester,
    layout::{GAPS_FILE, LogFileName, StoreRoot},
};
use crate::test_dir::TestDir;

const IMAGE: &str = "alpine:3.23.3";
const LOG_CONFIG: [&str; 8] = [
    "--log-driver",
    "local",
    "--log-opt",
    "max-size=10m",
    "--log-opt",
    "max-file=3",
    "--log-opt",
    "compress=false",
];

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn docker_root() -> PathBuf {
    PathBuf::from(docker(&["info", "--format", "{{.DockerRootDir}}"]))
}

fn run_detached(labels: &[&str], script: &str) -> ContainerId {
    let mut args = vec!["run", "-d"];
    args.extend(LOG_CONFIG);
    for label in labels {
        args.extend(["--label", label]);
    }
    args.extend([IMAGE, "sh", "-c", script]);
    ContainerId::parse(docker(&args)).unwrap()
}

struct Removed(ContainerId);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", self.0.as_str()])
            .output();
    }
}

fn decode(files: &[PathBuf]) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    for file in files {
        let bytes = fs::read(file).unwrap();
        for event in Frames::new(&bytes) {
            match event {
                Event::Entry(entry) => {
                    let sink = match entry.stream {
                        Stream::Stdout => &mut out,
                        Stream::Stderr => &mut err,
                    };
                    frame::write_docker_style(&entry, sink).unwrap();
                }
                Event::Corrupt { offset, skipped } => {
                    panic!("{}: {skipped} corrupt bytes at {offset}", file.display())
                }
            }
        }
    }
    (out, err)
}

#[test]
#[ignore = "requires root, Docker, and alpine:3.23.3"]
fn decoding_matches_docker_logs_byte_for_byte() {
    let script = "printf 'out one\\n'; printf 'err one\\n' >&2; \
                  head -c 40000 /dev/zero | tr '\\0' a; printf '\\n'; \
                  printf 'err two\\n' >&2; printf 'no newline at the end'";
    let id = run_detached(&[], script);
    let _removed = Removed(id);
    docker(&["wait", id.as_str()]);

    let expected = Command::new("docker")
        .args(["logs", "--timestamps", id.as_str()])
        .output()
        .unwrap();
    assert!(expected.status.success());
    let log = docker_root()
        .join("containers")
        .join(id.as_str())
        .join("local-logs")
        .join("container.log");
    let (out, err) = decode(&[log]);
    assert_eq!(
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&expected.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&err),
        String::from_utf8_lossy(&expected.stderr)
    );
    assert!(out.len() > 40_000);
}

const LINES: u64 = 600_000;
const LINES_PER_BURST: u64 = 2_500;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root, Docker, and alpine:3.23.3"]
async fn every_line_through_six_rotations_and_removal_is_stored_exactly_once() {
    let docker_root = docker_root();
    let test_root =
        TestDir(docker_root.join(format!("ployz-observe-test-{}", MachineId::random())));
    fs::create_dir_all(&test_root.0).unwrap();
    let store = StoreRoot::under(&test_root.0);
    store.prepare().unwrap();
    let listener = tokio::net::UnixListener::bind(test_root.0.join("observe.sock")).unwrap();
    let client = bollard::Docker::connect_with_defaults().unwrap();
    let harvester = tokio::spawn({
        let docker_root = docker_root.clone();
        let store = store.clone();
        async move { Harvester::run(client, &docker_root, store, listener).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;

    let script = format!(
        "awk 'BEGIN {{ pad = sprintf(\"%90s\", \"\"); \
         for (i = 0; i < {LINES}; i++) {{ printf \"%08d%s\\n\", i, pad; \
         if (i % {LINES_PER_BURST} == {LINES_PER_BURST} - 1) {{ fflush(); system(\"sleep 0.1\") }} }} }}'"
    );
    let started = Instant::now();
    let id = run_detached(&["ployz.managed=true"], &script);
    let _removed = Removed(id);
    tokio::task::spawn_blocking(move || docker(&["wait", id.as_str()]))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    tokio::time::sleep(Duration::from_secs(1)).await;
    docker(&["rm", "-f", id.as_str()]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    harvester.abort();

    let dir = store.container(&id);
    let mut held: Vec<(u64, PathBuf)> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            LogFileName::parse(entry.file_name().as_encoded_bytes())
                .map(|name| (name.seq, entry.path()))
        })
        .collect();
    held.sort();
    let files: Vec<PathBuf> = held.into_iter().map(|(_, path)| path).collect();
    let (out, err) = decode(&files);
    assert!(err.is_empty());
    let mut counts: HashMap<u64, u32> = HashMap::new();
    for line in out
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let text = line.splitn(2, |byte| *byte == b' ').nth(1).unwrap();
        let index: u64 = std::str::from_utf8(text.get(..8).unwrap())
            .unwrap()
            .parse()
            .unwrap();
        *counts.entry(index).or_default() += 1;
    }
    let missing = (0..LINES)
        .filter(|index| !counts.contains_key(index))
        .count();
    let doubled = counts.values().filter(|count| **count > 1).count();
    let gaps = fs::read_to_string(dir.join(GAPS_FILE)).unwrap_or_default();
    let rotations = files.len().saturating_sub(1);
    eprintln!(
        "OBS-2 harvest: {LINES} lines in {:.1}s ({:.0} lines/s), {} files held, {rotations} rotations, {missing} missing, {doubled} doubled",
        elapsed.as_secs_f64(),
        LINES as f64 / elapsed.as_secs_f64(),
        files.len(),
    );
    assert!(rotations >= 6, "only {rotations} rotations");
    assert_eq!((missing, doubled), (0, 0));
    assert_eq!(counts.len() as u64, LINES);
    assert!(gaps.is_empty(), "unexpected gaps: {gaps}");
    assert!(Path::new(&dir).join("meta.json").exists());
}
