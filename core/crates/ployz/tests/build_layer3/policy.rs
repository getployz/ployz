//! Actual host enforcement, cache retention, and administration on a selected Build Machine.

use ployz::build::Recipe;
use ployz_testkit::{Cluster, ClusterPlan};
use std::{fs, path::Path};
use tokio_util::sync::CancellationToken;

const POLICY: &str = "cpu_cores: 0.5\nmemory_bytes: 536870912\n";
const EVIDENCE: &str = "/tmp/ployz-policy-evidence";

struct Host {
    project: tempfile::TempDir,
    cluster: Cluster,
    selected: ployz_core::MachineId,
    client: ployz::connect::Client,
    image: String,
}

impl Host {
    async fn new() -> Self {
        let cluster = Cluster::create(
            ClusterPlan::new(&format!("l3-policy-808-{}", std::process::id()), 2).unwrap(),
        )
        .unwrap();
        let machines = cluster.initialize_two().await.unwrap();
        let selected = machines.get(1).unwrap().id;
        let client = ployz::connect::connect(
            Path::new("/missing-ployz-test-config"),
            Some(&cluster.api_address(0).unwrap()),
            None,
        )
        .await
        .unwrap();
        let host = Self {
            project: tempfile::tempdir().unwrap(),
            cluster,
            selected,
            client,
            image: format!("ployz-policy-808-{}:built", uuid::Uuid::new_v4()),
        };
        // The wrapper only records kernel/Docker evidence. Every operation and
        // resource limit is implemented by the real product and upstream tools.
        host.shell(&format!("mkdir -p {EVIDENCE} /root/.ployz; cp /usr/local/bin/docker /usr/local/bin/docker-policy-real"));
        let wrapper = format!(
            r#"#!/bin/sh
real='/usr/local/bin/docker-policy-real'
evidence='{EVIDENCE}'
observe() {{
    "$real" inspect "$container" --format '{{{{json .}}}}' > "$evidence/$kind.inspect.tmp" 2>/dev/null && mv "$evidence/$kind.inspect.tmp" "$evidence/$kind.inspect"
    "$real" exec "$container" sh -c 'cat /sys/fs/cgroup/cpu.max /sys/fs/cgroup/memory.max /sys/fs/cgroup/cpu.stat /sys/fs/cgroup/memory.events' > "$evidence/$kind.cgroup.tmp" 2>/dev/null && mv "$evidence/$kind.cgroup.tmp" "$evidence/$kind.cgroup"
}}
case "$1 $2" in
 'start --attach') kind=prepare; container="$3" ;;
 'buildx bake') kind=worker; container="buildx_buildkit_ployz-$(id -u)0" ;;
 'buildx rm') kind=worker; container="buildx_buildkit_ployz-$(id -u)0"; observe; exec "$real" "$@" ;;
 'rm --force') kind=prepare; container="$3"; observe; exec "$real" "$@" ;;
 *) exec "$real" "$@" ;;
esac
"$real" "$@" &
operation=$!
while kill -0 "$operation" 2>/dev/null; do observe; sleep 0.1; done
wait "$operation"
status=$?
observe
exit "$status"
"#
        );
        // The script is test-authored, not user-provided text.
        host.shell(&format!("cat > /usr/local/bin/docker <<'PLOYZ_AUDIT'\n{wrapper}\nPLOYZ_AUDIT\nchmod +x /usr/local/bin/docker"));
        host.configure(POLICY);
        host
    }

    fn shell(&self, script: &str) -> String {
        self.cluster.machine_shell(1, script).unwrap()
    }

    fn configure(&self, policy: &str) {
        self.shell(&format!(
            "cat > /root/.ployz/build.yaml <<'PLOYZ_POLICY'\n{policy}\nPLOYZ_POLICY"
        ));
    }

    /// Build the project on the selected Machine; the error text on failure.
    async fn build(&self, succeeds: bool) -> String {
        self.shell(&format!("rm -f {EVIDENCE}/worker.* {EVIDENCE}/prepare.*"));
        let project = self.project.path();
        let dockerfile = project.join("Dockerfile");
        let recipe = if dockerfile.exists() {
            Recipe::Dockerfile(dockerfile)
        } else {
            Recipe::Railpack { command: None }
        };
        let result = super::capture(project, &self.image, serde_json::json!({}), recipe)
            .execute_remote_images(
                &self.client,
                self.selected,
                CancellationToken::new(),
                |_| {},
            )
            .await;
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    fn write(&self, file: &str, content: &str) {
        fs::write(self.project.path().join(file), content).unwrap();
    }

    fn record(&self, kind: &str, extension: &str) -> String {
        self.shell(&format!("cat {EVIDENCE}/{kind}.{extension}"))
    }

    fn cpu_was_enforced(&self, kind: &str) {
        let record = self.record(kind, "cgroup");
        let mut lines = record.lines();
        assert_eq!(lines.next(), Some("50000 100000"));
        assert_eq!(lines.next(), Some("536870912"));
        assert!(
            record.lines().any(|line| line
                .strip_prefix("nr_throttled ")
                .is_some_and(|n| n.parse::<u64>().unwrap() > 0)),
            "work was never throttled: {record}"
        );
    }

    fn memory_was_enforced(&self, kind: &str) {
        let inspect: serde_json::Value =
            serde_json::from_str(&self.record(kind, "inspect")).unwrap();
        if inspect
            .pointer("/State/OOMKilled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return;
        }
        let record = self.record(kind, "cgroup");
        assert!(
            record.lines().any(|line| line
                .strip_prefix("oom_kill ")
                .is_some_and(|n| n.parse::<u64>().unwrap() > 0)),
            "no kernel OOM evidence: {record}"
        );
    }

    fn stamp(&self, path: &str) -> String {
        self.shell(&format!(
            "docker run --rm --pull never --entrypoint cat {} {path}",
            self.image
        ))
    }

    fn clear(&self) {
        self.shell("ployz machine build-cache-clear");
    }
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with current CLI and daemon"]
async fn selected_machine_build_resource_policy_and_cache_administration() {
    let host = Host::new().await;
    host.write("Dockerfile", "FROM alpine:3.23.3\nRUN dd if=/dev/zero bs=1M count=256 | sha256sum; cat /proc/sys/kernel/random/uuid > /built-at\n");
    host.build(true).await;
    host.cpu_was_enforced("worker");
    let first = host.stamp("/built-at");
    host.build(true).await;
    assert_eq!(
        host.stamp("/built-at"),
        first,
        "builder recreation lost retained cache"
    );

    // Each target is applied by upstream GC. Neither is a hard disk quota.
    for target in ["cache_bytes: 1", "min_free_bytes: 9000000000000000000"] {
        host.configure(&format!("{POLICY}{target}\n"));
        host.build(true).await;
        let before_gc = host.stamp("/built-at");
        host.build(true).await;
        assert_ne!(
            host.stamp("/built-at"),
            before_gc,
            "configured GC retained the build layer: {target}"
        );
    }
    host.configure(POLICY);
    let unrelated = format!("ployz-unrelated-808-{}", uuid::Uuid::new_v4());
    host.shell(&format!("docker volume create --label dev.ployz.test=unrelated {unrelated}; docker run --rm -v {unrelated}:/data alpine:3.23.3 sh -c 'echo preserved > /data/value'"));
    let before_clear = host.stamp("/built-at");
    host.clear();
    assert_eq!(
        host.stamp("/built-at"),
        before_clear,
        "clearing removed a usable completed image"
    );
    assert_eq!(
        host.shell(&format!(
            "docker run --rm -v {unrelated}:/data alpine:3.23.3 cat /data/value"
        )),
        "preserved\n"
    );
    host.shell(&format!("docker volume rm {unrelated}"));
    host.build(true).await;
    assert_ne!(
        host.stamp("/built-at"),
        before_clear,
        "explicit clearing kept the build layer"
    );

    host.write("Dockerfile", "FROM alpine:3.23.3\nRUN awk 'BEGIN {for(i=0;i<1000000;i++) a[i]=sprintf(\"%01024d\",i)}'\n");
    host.build(false).await;
    host.memory_was_enforced("worker");

    fs::remove_file(host.project.path().join("Dockerfile")).unwrap();
    host.write("package.json", &format!(r#"{{"name":"policy","version":"1.0.0","engines":{{"node":"22.14.0"}},"scripts":{{"build":"node build.js","start":"node index.js"}},"description":"{}"}}"#, "x".repeat(16 * 1024 * 1024)));
    host.write("index.js", "console.log('ready');");
    host.write("build.js", "const crypto=require('crypto'); for(let i=0;i<512;i++) crypto.createHash('sha256').update(Buffer.alloc(1048576)).digest(); require('fs').writeFileSync('built-at',crypto.randomUUID());");
    host.build(true).await;
    host.cpu_was_enforced("prepare");
    host.cpu_was_enforced("worker");
    let first = host.stamp("/app/built-at");
    host.build(true).await;
    assert_eq!(host.stamp("/app/built-at"), first);
    host.write(
        "build.js",
        "console.log(Buffer.alloc(1024*1024*1024,1).length);",
    );
    host.build(false).await;
    host.memory_was_enforced("worker");

    host.configure("cpu_cores: 0.5\nmemory_bytes: 134217728\n");
    host.write(
        "package.json",
        &format!(
            r#"{{"name":"policy","scripts":{{"start":"node index.js"}},"description":"{}"}}"#,
            "x".repeat(192 * 1024 * 1024)
        ),
    );
    let failure = host.build(false).await;
    assert!(failure.contains("Railpack preparation"), "{failure}");
    host.memory_was_enforced("prepare");
}
