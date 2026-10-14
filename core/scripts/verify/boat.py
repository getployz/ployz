"""Boat sandbox backend for verify-cluster's disposable-Machine contract."""

import fcntl
import hashlib
import host
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import threading
import time
import uuid

SNAPSHOT = "ployz-verify-024"
# The release ployzd install put in SNAPSHOT; Machines booting it skip the install.
SNAPSHOT_RELEASE = "0.2.4"
SIZE = "small"
# Launches per Machine before start() gives up on landing it on a node no peer uses.
PLACEMENTS = 3
MEMORY = 4 * host.GiB
TTL = int(os.environ.get("PLOYZ_VERIFY_BOAT_TTL", 6 * 3600))
# prepare-guest.sh pulls these into the Incus image; SNAPSHOT keeps no Docker images, so each Machine pulls them at boot.
IMAGES = ["caddy:2", "nginx:1.29-alpine", "nginx:1.27-alpine", "postgres:16-alpine", "alpine:3.20", "traefik/whoami:v1.10.3"]
UP_STATES = {"ready", "idle", "running", "cloning"}
REGISTRY = Path.home() / ".cache" / "ployz" / "verify" / "boat"
# prepare() bakes SNAPSHOT plus BAKE into a named snapshot whose authorized_keys holds this key, so Machines
# skip the image pulls and `boat exec`.
BOOTSTRAP = REGISTRY / "bootstrap"
BAKED = "ployz-verify-boat-"
# A Boat snapshot drops images no container uses, so BAKE leaves a created, never started holder per image.
# Boat restores containers after boot, so the holders stay; tmpfs over each image VOLUME keeps them volume-free.
# Boat keeps 10 named snapshots per account; bakes past the newest few are removed.
KEEP_BAKED = 3
BAKE_NAME = re.compile(re.escape(BAKED) + "[0-9a-f]{10}")
# Bakes this host made; another host or person may share the account and the name prefix.
BAKES = REGISTRY / "bakes.ledger"
BAKE = """
ufw disable >/dev/null
printf '%s\\n' "$@" | xargs -P8 -n1 docker pull -q >/dev/null
caddy_version=$(docker run --rm --entrypoint caddy caddy:2 version)
caddy_version=${caddy_version%% *}
docker tag caddy:2 "caddy:${caddy_version#v}"
for image in "$@" "caddy:${caddy_version#v}"; do
    mounts=$(docker image inspect -f '{{range $path, $_ := .Config.Volumes}}--mount=type=tmpfs,dst={{$path}} {{end}}' "$image")
    docker create --label ployz.verify.hold=1 $mounts "$image" >/dev/null
done
command -v zstd >/dev/null || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zstd >/dev/null || \
    { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zstd >/dev/null; }
sync
"""
# A clone can carry an sshd listener that systemd no longer tracks, so restarting ssh later fails with
# port 22 in use. Kill only listeners, so open SSH sessions survive.
LISTENER = shlex.quote(r"^sshd: [^ ]*sshd -D \[listener\]")
# A clone resumes while Boat's agent still queues systemd jobs, and restarting ssh under them can drop
# systemctl's bus connection. Two idle readings in a row mark the restore done.
SETTLE = ("idle=0; for i in $(seq 120); do if [ -z \"$(systemctl list-jobs --no-legend)\" ]; then idle=$((idle+1));"
          " [ $idle = 2 ] && break; else idle=0; fi; sleep 0.5; done; ")
# The ployz units live in /etc/systemd/system, where `systemctl mask` refuses them, so power off disables those.
PLOYZ_UNITS = "ployz.socket ployz.service ployz-volume-plugin.socket"
DOCKER_UNITS = "docker.service docker.socket containerd.service"


def cli():
    return [os.environ.get("BOAT_CLI", "boat"), "--no-update"]


def error(result):
    """The `{"event":"error"}` line a failed Boat command ends with, or None."""
    for line in reversed(result.stdout.decode(errors="replace").splitlines()):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict) and event.get("event") == "error":
            return event
    return None


def exited(result):
    """The stderr of a command `boat exec` ran that exited non-zero, or None."""
    try:
        ran = json.loads(result.stdout)
    except ValueError:
        return None
    if isinstance(ran, dict) and ran.get("exitCode"):
        return f"exit {ran['exitCode']}: {ran.get('stderr', '').strip()}"
    return None


def events(result):
    found = []
    for line in result.stdout.decode(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict):
            found.append(event)
    return found


def endpoint(text):
    """Host and port from a sandbox's sshEndpoint, such as 213.32.24.158:19040."""
    address, _, port = text.rpartition(":")
    return address.strip("[]"), int(port)


def boat_node(address):
    """The Boat node a sandbox runs on: each node hands out one IPv6 /80."""
    return str(ipaddress.IPv6Network(f"{address}/80", strict=False))


def registry(run_id):
    return REGISTRY / f"{run_id}.json"


def read(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return None


def write(path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(f".{os.getpid()}.{threading.get_ident()}.tmp")
    temporary.write_text(json.dumps(record, indent=2) + "\n")
    temporary.replace(path)


COMPRESSED = {}
COMPRESSING = threading.Lock()


def compressed(source):
    """zstd of `source`, made once per content for every Machine that receives it."""
    stat = os.stat(source)
    key = (os.path.realpath(source), stat.st_size, stat.st_mtime_ns)
    with COMPRESSING:
        if key not in COMPRESSED:
            COMPRESSED[key] = subprocess.run(["zstd", "-3", "-T0", "-qc", source], capture_output=True, check=True).stdout
        return COMPRESSED[key]


def bootstrap():
    """The public half of the key every baked snapshot authorizes, made on first use."""
    BOOTSTRAP.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (BOOTSTRAP / "key.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not (BOOTSTRAP / "id_ed25519").exists():
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "ployz-verify-boat",
                            "-f", BOOTSTRAP / "id_ed25519"], check=True)
    return (BOOTSTRAP / "id_ed25519.pub").read_text().strip()


def stale_bakes(named, current, ours, in_use):
    """Bakes to remove before baking `current`, oldest first: only bakes this host made that no run still uses."""
    older = sorted((snapshot for snapshot in named if snapshot["name"] in ours and BAKE_NAME.fullmatch(snapshot["name"])
                    and snapshot["name"] != current and snapshot["name"] not in in_use),
                   key=lambda snapshot: snapshot["createdAt"])
    return [snapshot["name"] for snapshot in older[:max(len(older) - (KEEP_BAKED - 1), 0)]]


def images_in_use():
    return {record["image"] for path in REGISTRY.glob("*.json") if (record := read(path)) and record.get("image")}


def boat_json(*args):
    result = subprocess.run([*cli(), *args, "--json"], stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
    if result.returncode:
        found = error(result) or {}
        raise RuntimeError(f"boat {args[0]} failed: {found.get('code') or result.stderr.decode(errors='replace').strip()}")
    return json.loads(result.stdout)


def status(state):
    return "Running" if state in UP_STATES else (state or "gone").capitalize()


def runs():
    """Every Boat verify run this user's registry holds, from any checkout."""
    records = [record for path in sorted(REGISTRY.glob("*.json")) if (record := read(path))]
    if not records:
        return []
    states = {sandbox["id"]: sandbox["state"] for sandbox in boat_json("list", "--all")["sandboxes"]}
    return [dict(id=record["run"], provider="boat", owner=record.get("owner", {}), created=record.get("created"),
                 machines=sorted((dict(name=name, sandbox=entry["id"], status=status(states.get(entry["id"])), rss=0,
                                       memory=MEMORY, created=entry.get("created", record.get("created")))
                                  for name, entry in record["sandboxes"].items() if entry["state"] != "deleted"),
                                 key=lambda machine: machine["name"]))
            for record in records]


def shortfalls(machines, limits):
    """Why Boat cannot start `machines` sandboxes now, or an empty list."""
    problems = []
    headroom = limits["maxActiveSandboxes"] - limits["activeSandboxes"]
    if machines > headroom:
        problems.append(f"Active sandboxes: need {machines}, {headroom} of {limits['maxActiveSandboxes']} free")
    if limits["starts"].get("unlimited"):
        return problems
    # Relaunches wait on a boot, so only the first placements share a minute; the rate_limited retry absorbs the rest.
    for window, need in [("minute", machines), ("hour", machines * PLACEMENTS), ("day", machines * PLACEMENTS)]:
        remaining = limits["starts"][window]["remaining"]
        if need > remaining:
            problems.append(f"Starts this {window}: need up to {need}, {remaining} left")
    return problems


def preflight(machines):
    problems = shortfalls(machines, boat_json("limits"))
    if problems:
        raise RuntimeError("Boat cannot start the run now.\n" + "\n".join(problems))


class Provider:
    def __init__(self, run, commands, core):
        self.run, self.commands, self.core = run, commands, core
        self.registry = registry(run["id"])
        self.lock = threading.Lock()
        self.private = Path(run["manifest"]).parent / "private"
        # A socket path must fit in 108 bytes, which the run directory does not.
        self.control = REGISTRY / "ssh"

    def boat(self, *args, **kwargs):
        return self.commands([*cli(), *args, "--json"], **kwargs)

    def record(self, update=None):
        with self.lock:
            record = read(self.registry)
            if update:
                update(record)
                write(self.registry, record)
            return record

    def sandbox(self, node):
        return self.record()["sandboxes"][node["name"]]

    def mark(self, name, **fields):
        self.record(lambda record: record["sandboxes"].setdefault(name, {}).update(fields))

    def create(self):
        if self.registry.exists():
            raise RuntimeError(f"Boat registry already exists for this run: {self.registry}")
        write(self.registry, dict(run=self.run["id"], manifest=self.run["manifest"], owner=self.run.get("owner", {}),
                                  image=self.run.get("image"), created=int(time.time()), sandboxes={}))

    def launch(self, node):
        before = time.time()
        for attempt in range(10):
            result = self.boat("new", "--from", self.run["image"], "--type", SIZE, "--ttl", TTL, check=False)
            found = events(result)
            created = next((event["id"] for event in found if event.get("event") == "created"), None)
            if created:
                self.mark(node["name"], id=created, state="running", created=int(before))
            failure = error(result)
            if not result.returncode and created:
                return created
            if failure and failure.get("code") == "boat_starting" and not created:
                time.sleep(min(2 ** attempt, 10))
                continue
            if failure and failure.get("code") == "rate_limited" and not created:
                # Other runs can spend this minute's starts after our preflight read them.
                time.sleep(15)
                continue
            if failure and failure.get("code") == "boat_direct_failed" and not created:
                # The create may have run without telling us its id; never retry it blind.
                known = {entry["id"] for path in REGISTRY.glob("*.json") for entry in (read(path) or {}).get("sandboxes", {}).values()}
                unknown = [sandbox["id"] for sandbox in boat_json("list", "--all")["sandboxes"]
                           if sandbox["id"] not in known and sandbox["createdAt"] >= time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(before - 5))]
                raise RuntimeError(f"{node['name']}: boat new failed with boat_direct_failed; sandboxes created since then "
                                   f"that no run owns: {', '.join(unknown) or 'none'}. Delete any that are yours.")
            raise RuntimeError(f"{node['name']}: boat new failed: {failure or result.stderr.decode(errors='replace')}")
        raise RuntimeError(f"{node['name']}: boat new kept answering boat_starting or rate_limited")

    def place(self, name, info):
        """Record a sandbox's address unless a peer in this run already holds its node; True when recorded."""
        placed = []

        def claim(record):
            taken = {boat_node(entry["ip"]) for other, entry in record["sandboxes"].items()
                     if other != name and entry.get("ip") and entry.get("state") != "deleted"}
            if boat_node(info["ip"]) not in taken:
                record["sandboxes"][name].update(ssh=info["sshEndpoint"], ip=info["ip"])
                placed.append(True)
        self.record(claim)
        return bool(placed)

    def start(self, node):
        for attempt in range(PLACEMENTS):
            sandbox = self.launch(node)
            info = json.loads(self.boat("info", sandbox).stdout)["sandbox"]
            if self.place(node["name"], info):
                break
            # Peers on one node share an on-link /80 and their WireGuard over IPv6 never handshakes.
            self.boat("delete", sandbox, "--yes")
        else:
            raise RuntimeError(f"{node['name']}: Boat kept placing the sandbox on a node another Machine uses")
        if self.run["image"] == SNAPSHOT:
            self.install_key(node, sandbox)
        self.wait_agent(node)
        # A baked sandbox already holds BAKE's result.
        bake = BAKE if self.run["image"] == SNAPSHOT else ""
        self.exec(node, ["bash", "-ec", f"""
{SETTLE}
main=$(systemctl show -p MainPID --value ssh.service)
for pid in $(pgrep -f {LISTENER}); do [ "$pid" = "$main" ] || kill "$pid"; done
if [ -n "$(ss -Hlun 'sport = :51820')" ]; then echo "UDP port 51820 is taken" >&2; exit 1; fi
{bake}""", "boot", self.run["corrosion"], *IMAGES], timeout=600)

    def install_key(self, node, sandbox):
        key = (self.private / "id_ed25519.pub").read_text().strip()
        install = (SETTLE + f"install -d -m 700 /root/.ssh && printf '%s\\n' {shlex.quote(key)} > /root/.ssh/authorized_keys"
                   " && chmod 600 /root/.ssh/authorized_keys && systemctl stop ssh.service ssh.socket"
                   f" && {{ pkill -f {LISTENER}; for i in $(seq 50); do pgrep -f {LISTENER} >/dev/null || break; sleep 0.1; done; }}"
                   " && systemctl start ssh.socket")
        deadline = time.monotonic() + 120
        while True:
            result = self.boat("exec", sandbox, "--timeout", "60", "--", "sudo", "sh", "-c", install, check=False)
            if not result.returncode:
                return
            failure = error(result)
            if (failure or {}).get("code") != "boat_starting" or time.monotonic() >= deadline:
                why = failure or exited(result) or result.stderr.decode(errors="replace")
                raise RuntimeError(f"{node['name']}: could not install the root key: {why}")
            time.sleep(1)

    def power_off(self, node):
        # No sync beyond the unit links: a reset loses the page cache, as `incus stop --force` does.
        # `boat stop` is not used because a resumed sandbox can land on another host with a new address.
        self.exec(node, ["bash", "-ec", f"""
systemctl mask {DOCKER_UNITS} >/dev/null
observe=
if [ -e /etc/systemd/system/ployz-observe.service ]; then observe=ployz-observe.service; fi
systemctl disable {PLOYZ_UNITS} $observe >/dev/null 2>&1
sync /etc/systemd/system /etc/systemd/system/*.wants
setsid sh -c 'sleep 1; echo b > /proc/sysrq-trigger' >/dev/null 2>&1 < /dev/null &
"""])
        time.sleep(5)
        host, port = endpoint(self.sandbox(node)["ssh"])
        self.commands(["ssh", *self.options(), "-p", port, "-O", "exit", "root@" + host], check=False)

    def power_on(self, node):
        self.wait_agent(node)
        self.exec(node, ["bash", "-ec", f"""
systemctl unmask {DOCKER_UNITS} >/dev/null
systemctl start docker
observe=
if [ -e /etc/systemd/system/ployz-observe.service ]; then observe=ployz-observe.service; fi
systemctl enable --now {PLOYZ_UNITS} $observe
"""])

    def state(self, node):
        if node["name"] in self.run.get("powered_off", []):
            return "STOPPED"
        sandbox = self.record()["sandboxes"].get(node["name"], {})
        if not sandbox.get("id") or sandbox.get("state") == "deleted":
            return ""
        result = self.boat("info", sandbox["id"], check=False)
        if result.returncode:
            return "" if (error(result) or {}).get("status") == 404 else "UNKNOWN"
        state = json.loads(result.stdout)["sandbox"]["state"]
        return "RUNNING" if state in UP_STATES else state.upper()

    def stop_cause(self, node):
        return None

    def wait_agent(self, node):
        deadline = time.monotonic() + 180
        while True:
            if not self.exec(node, ["true"], check=False, timeout=10).returncode:
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{node['name']}: root SSH to the sandbox did not answer")
            time.sleep(1)

    def options(self):
        self.control.mkdir(parents=True, exist_ok=True, mode=0o700)
        # The endpoint comes from the authenticated Boat API, and up regenerates host keys after the first connection.
        return ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15",
                "-i", str(self.private / "id_ed25519"), "-i", str(BOOTSTRAP / "id_ed25519"), "-o", "ControlMaster=auto", "-o", "ControlPersist=300",
                "-o", "ControlPath=" + str(self.control / (self.run["id"] + "-%C"))]

    def exec(self, node, args, **kwargs):
        host, port = endpoint(self.sandbox(node)["ssh"])
        return self.commands(["ssh", *self.options(), "-p", port, "root@" + host, shlex.join(args)], **kwargs)

    def push(self, node, source, destination):
        directory = os.path.dirname(destination)
        if self.run["image"] == SNAPSHOT or os.path.getsize(source) < 1 << 20:
            with open(source, "rb") as content:
                self.exec(node, ["sh", "-ec", 'install -d "$1"; cat > "$2.push"; chmod 0755 "$2.push"; mv "$2.push" "$2"',
                                 "push", directory, destination], stdin=content, timeout=600)
            return
        # The upload from this host is the slow link; zstd cuts a release binary about fivefold.
        self.exec(node, ["sh", "-ec", 'install -d "$1"; zstd -dqc > "$2.push"; chmod 0755 "$2.push"; mv "$2.push" "$2"',
                         "push", directory, destination], input=compressed(source), timeout=600)

    def address(self, node):
        address = json.loads(self.boat("info", self.sandbox(node)["id"]).stdout)["sandbox"]["ip"]
        if not address:
            raise RuntimeError(f"{node['name']}: sandbox has no IPv6 address")
        return address

    def ssh(self, node):
        host, port = endpoint(self.sandbox(node)["ssh"])
        return f"root@{host}:{port}"

    def destroy(self):
        record = read(self.registry)
        if record is None:
            return
        if record["run"] != self.run["id"]:
            raise RuntimeError(f"Refusing to delete another run's sandboxes: {self.registry}")
        failures = []
        for name, entry in record["sandboxes"].items():
            if entry.get("state") == "deleted" or not entry.get("id"):
                continue
            result = self.boat("delete", entry["id"], "--yes", check=False)
            failure = error(result) or {}
            if not result.returncode or failure.get("status") == 404:
                self.mark(name, state="deleted")
                continue
            if failure.get("status") != 403:
                failures.append(f"boat delete {entry['id']} failed: {failure or result.stderr.decode(errors='replace')}")
                continue
            if entry.get("state") != "stopped":
                self.boat("stop", entry["id"])
                self.mark(name, state="stopped")
            print(f"Stopped {entry['id']} ({name}): this Boat key may not delete it; it needs the sandbox.delete-own scope "
                  f"and to have created the sandbox.", file=sys.stderr)
        if failures:
            raise RuntimeError("\n".join(failures))
        if all(entry.get("state") == "deleted" for entry in read(self.registry)["sandboxes"].values()):
            self.registry.unlink()

    def prepare(self):
        if not shutil.which(cli()[0]):
            raise RuntimeError(f"Boat CLI not found: {cli()[0]}; install it or set BOAT_CLI")
        if not shutil.which("zstd"):
            raise RuntimeError("zstd not found; install it")
        self.run["bootstrap_release"] = SNAPSHOT_RELEASE
        self.run["corrosion"] = re.search(r'pub const IMAGE: &str = "([^"]+)"',
                                          (self.core / "crates/ployzd/src/corrosion/service.rs").read_text()).group(1)
        recipe = json.dumps([SNAPSHOT, BAKE, SETTLE, IMAGES, self.run["corrosion"], bootstrap()])
        self.run["image"] = BAKED + hashlib.sha256(recipe.encode()).hexdigest()[:10]
        # One baked snapshot per recipe; simultaneous runs don't bake it twice.
        with (REGISTRY / "bake.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            named = boat_json("snapshots")["named"]
            if any(snapshot["name"] == self.run["image"] and snapshot["status"] == "ready" for snapshot in named):
                return
            ours = read(BAKES) or []
            # Prune first; the account caps named snapshots.
            for name in stale_bakes(named, self.run["image"], ours, images_in_use()):
                if not self.boat("snapshot", "rm", name, check=False).returncode:
                    ours.remove(name)
            write(BAKES, ours)
            print(f"Baking Boat snapshot {self.run['image']} (first run of this recipe only).", file=sys.stderr)
            bake = Provider(dict(id="bake-" + uuid.uuid4().hex[:10], manifest=str(BOOTSTRAP / "bake" / "cluster.json"),
                                 owner=self.run.get("owner", {}), image=SNAPSHOT, corrosion=self.run["corrosion"]),
                            self.commands, self.core)
            bake.private = BOOTSTRAP
            node = dict(name="bake")
            try:
                bake.create()
                bake.start(node)
                self.boat("snapshot", bake.sandbox(node)["id"], self.run["image"], timeout=1800)
            except BaseException:
                try:
                    bake.destroy()
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    print(f"Deleting the bake sandbox also failed: {error}", file=sys.stderr)
                raise
            write(BAKES, [*ours, self.run["image"]])
            bake.destroy()
