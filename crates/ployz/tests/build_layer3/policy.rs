//! Actual host enforcement, cache retention, and administration through both CLI locations.

use ployz_testkit::{Cluster, ClusterPlan};
use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf, process::Command};

const POLICY: &str = "cpu_cores: 0.5\nmemory_bytes: 536870912\n";

#[tokio::test]
#[ignore = "informing: requires Docker with cgroup v2, Buildx, and the containerd image store"]
async fn local_build_resource_policy_and_cache_administration() {
    exercise(false).await;
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with current CLI and daemon"]
async fn selected_machine_build_resource_policy_and_cache_administration() {
    exercise(true).await;
}

struct Host {
    root: PathBuf,
    remote: Option<(Cluster, ployz_core::MachineId)>,
    image: String,
}

impl Host {
    async fn new(remote: bool) -> Self {
        let root = std::env::temp_dir().join(format!("ployz-policy-808-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("project")).unwrap();
        fs::create_dir_all(root.join("home/.ployz")).unwrap();
        fs::create_dir_all(root.join("tools")).unwrap();
        let remote = if remote {
            let cluster = Cluster::create(
                ClusterPlan::new(&format!("l3-policy-808-{}", std::process::id()), 2).unwrap(),
            )
            .unwrap();
            let machines = cluster.initialize_two().await.unwrap();
            let selected = machines.get(1).unwrap().id;
            Some((cluster, selected))
        } else {
            None
        };
        let host = Self {
            root,
            remote,
            image: format!("ployz-policy-808-{}:built", uuid::Uuid::new_v4()),
        };
        // The wrapper only records kernel/Docker evidence. Every operation and
        // resource limit is implemented by the real product and upstream tools.
        if host.remote.is_some() {
            host.shell("mkdir -p /tmp/ployz-policy-evidence /root/.ployz; cp /usr/local/bin/docker /usr/local/bin/docker-policy-real");
        } else {
            fs::create_dir_all(host.root.join("evidence")).unwrap();
        }
        let real = if host.remote.is_some() {
            "/usr/local/bin/docker-policy-real".to_owned()
        } else {
            String::from_utf8(
                Command::new("sh")
                    .args(["-c", "command -v docker"])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned()
        };
        let evidence = host.evidence_directory();
        let wrapper = format!(
            r#"#!/bin/sh
real='{real}'
evidence='{evidence}'
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
        if let Some((cluster, _)) = &host.remote {
            let path = host.root.join("docker-wrapper");
            fs::write(&path, wrapper).unwrap();
            // machine_shell input is a script, not user-provided text.
            cluster.machine_shell(1, &format!("cat > /usr/local/bin/docker <<'PLOYZ_AUDIT'\n{}\nPLOYZ_AUDIT\nchmod +x /usr/local/bin/docker", fs::read_to_string(path).unwrap())).unwrap();
        } else {
            let path = host.root.join("tools/docker");
            fs::write(&path, wrapper).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        host.configure(POLICY);
        host
    }

    fn evidence_directory(&self) -> String {
        if self.remote.is_some() {
            "/tmp/ployz-policy-evidence".into()
        } else {
            self.root.join("evidence").display().to_string()
        }
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .current_dir(self.root.join("project"))
            .env("HOME", self.root.join("home"))
            .env("PLOYZ_CONFIG", self.root.join("home/config.yaml"))
            .env_remove("PLOYZ_CONNECT")
            .env_remove("PLOYZ_CONTEXT")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("tools").display(),
                    std::env::var("PATH").unwrap()
                ),
            );
        command
    }

    fn shell(&self, script: &str) -> String {
        if let Some((cluster, _)) = &self.remote {
            return cluster.machine_shell(1, script).unwrap();
        }
        let output = Command::new("sh").args(["-c", script]).output().unwrap();
        assert!(
            output.status.success(),
            "{script}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn configure(&self, policy: &str) {
        if self.remote.is_some() {
            self.shell(&format!(
                "cat > /root/.ployz/build.yaml <<'PLOYZ_POLICY'\n{policy}\nPLOYZ_POLICY"
            ));
        } else {
            fs::write(self.root.join("home/.ployz/build.yaml"), policy).unwrap();
        }
    }

    fn build(&self, succeeds: bool) -> String {
        self.shell(&format!(
            "rm -f {}/worker.* {}/prepare.*",
            self.evidence_directory(),
            self.evidence_directory()
        ));
        let mut command = self.cli();
        if let Some((cluster, _)) = &self.remote {
            command.args(["--connect", &cluster.api_address(0).unwrap()]);
        }
        command.arg("build");
        if let Some((_, selected)) = &self.remote {
            command.arg(format!("--remote={selected}"));
        }
        let output = command.output().unwrap();
        let log = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.success(), succeeds, "{log}");
        log
    }

    fn write(&self, file: &str, content: &str) {
        fs::write(self.root.join("project").join(file), content).unwrap();
    }

    fn record(&self, kind: &str, extension: &str) -> String {
        self.shell(&format!(
            "cat {}/{kind}.{extension}",
            self.evidence_directory()
        ))
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
        if self.remote.is_some() {
            self.shell("ployz machine build-cache-clear");
        } else {
            let output = self
                .cli()
                .args(["machine", "build-cache-clear"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if self.remote.is_none() {
            let _ = Command::new("docker")
                .args(["image", "rm", &self.image])
                .status();
            let _ = self.cli().args(["machine", "build-cache-clear"]).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn exercise(remote: bool) {
    let host = Host::new(remote).await;
    host.write(
        "compose.yaml",
        &format!(
            "name: policy\nservices:\n  app:\n    image: {}\n    build: .\n",
            host.image
        ),
    );
    host.write("Dockerfile", "FROM alpine:3.23.3\nRUN dd if=/dev/zero bs=1M count=256 | sha256sum; cat /proc/sys/kernel/random/uuid > /built-at\n");
    host.build(true);
    host.cpu_was_enforced("worker");
    let first = host.stamp("/built-at");
    host.build(true);
    assert_eq!(
        host.stamp("/built-at"),
        first,
        "builder recreation lost retained cache"
    );

    // Each target is applied by upstream GC. Neither is a hard disk quota.
    for target in ["cache_bytes: 1", "min_free_bytes: 9000000000000000000"] {
        host.configure(&format!("{POLICY}{target}\n"));
        host.build(true);
        let before_gc = host.stamp("/built-at");
        host.build(true);
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
    host.build(true);
    assert_ne!(
        host.stamp("/built-at"),
        before_clear,
        "explicit clearing kept the build layer"
    );

    host.write("Dockerfile", "FROM alpine:3.23.3\nRUN awk 'BEGIN {for(i=0;i<1000000;i++) a[i]=sprintf(\"%01024d\",i)}'\n");
    host.build(false);
    host.memory_was_enforced("worker");

    fs::remove_file(host.root.join("project/Dockerfile")).unwrap();
    host.write("package.json", &format!(r#"{{"name":"policy","version":"1.0.0","engines":{{"node":"22.14.0"}},"scripts":{{"build":"node build.js","start":"node index.js"}},"description":"{}"}}"#, "x".repeat(16 * 1024 * 1024)));
    host.write("index.js", "console.log('ready');");
    host.write("build.js", "const crypto=require('crypto'); for(let i=0;i<512;i++) crypto.createHash('sha256').update(Buffer.alloc(1048576)).digest(); require('fs').writeFileSync('built-at',crypto.randomUUID());");
    host.build(true);
    host.cpu_was_enforced("prepare");
    host.cpu_was_enforced("worker");
    let first = host.stamp("/app/built-at");
    host.build(true);
    assert_eq!(host.stamp("/app/built-at"), first);
    host.write(
        "build.js",
        "console.log(Buffer.alloc(1024*1024*1024,1).length);",
    );
    host.build(false);
    host.memory_was_enforced("worker");

    host.configure("cpu_cores: 0.5\nmemory_bytes: 134217728\n");
    host.write(
        "package.json",
        &format!(
            r#"{{"name":"policy","scripts":{{"start":"node index.js"}},"description":"{}"}}"#,
            "x".repeat(192 * 1024 * 1024)
        ),
    );
    let failure = host.build(false);
    assert!(failure.contains("Railpack preparation"), "{failure}");
    host.memory_was_enforced("prepare");
}
