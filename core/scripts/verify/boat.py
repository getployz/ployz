"""Boat sandbox backend for verify-cluster's disposable-Machine contract."""

import host
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

SNAPSHOT = "ployz-verify-024"
# The release ployzd install put in SNAPSHOT; Machines booting it skip the install.
SNAPSHOT_RELEASE = "0.2.4"
SIZE = "small"
MEMORY = 4 * host.GiB
TTL = int(os.environ.get("PLOYZ_VERIFY_BOAT_TTL", 6 * 3600))
# prepare-guest.sh pulls these into the Incus image; SNAPSHOT keeps no Docker images, so each Machine pulls them at boot.
IMAGES = ["caddy:2", "nginx:1.29-alpine", "nginx:1.27-alpine", "postgres:16-alpine", "alpine:3.20", "traefik/whoami:v1.10.3"]
UP_STATES = {"ready", "idle", "running", "cloning"}
REGISTRY = Path.home() / ".cache" / "ployz" / "verify" / "boat"
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
    for window in ["minute", "hour", "day"]:
        remaining = limits["starts"][window]["remaining"]
        if not limits["starts"].get("unlimited") and machines > remaining:
            problems.append(f"Starts this {window}: need {machines}, {remaining} left")
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
                                  created=int(time.time()), sandboxes={}))

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
            if failure and failure.get("code") == "boat_direct_failed" and not created:
                # The create may have run without telling us its id; never retry it blind.
                known = {entry["id"] for path in REGISTRY.glob("*.json") for entry in (read(path) or {}).get("sandboxes", {}).values()}
                unknown = [sandbox["id"] for sandbox in boat_json("list", "--all")["sandboxes"]
                           if sandbox["id"] not in known and sandbox["createdAt"] >= time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(before - 5))]
                raise RuntimeError(f"{node['name']}: boat new failed with boat_direct_failed; sandboxes created since then "
                                   f"that no run owns: {', '.join(unknown) or 'none'}. Delete any that are yours.")
            raise RuntimeError(f"{node['name']}: boat new failed: {failure or result.stderr.decode(errors='replace')}")
        raise RuntimeError(f"{node['name']}: boat new kept answering boat_starting")

    def start(self, node):
        sandbox = self.launch(node)
        info = json.loads(self.boat("info", sandbox).stdout)["sandbox"]
        self.mark(node["name"], ssh=info["sshEndpoint"], ip=info["ip"])
        key = (self.private / "id_ed25519.pub").read_text().strip()
        install = (f"install -d -m 700 /root/.ssh && printf '%s\\n' {shlex.quote(key)} > /root/.ssh/authorized_keys"
                   " && chmod 600 /root/.ssh/authorized_keys")
        deadline = time.monotonic() + 120
        while True:
            result = self.boat("exec", sandbox, "--timeout", "60", "--", "sudo", "sh", "-c", install, check=False)
            if not result.returncode:
                break
            if (error(result) or {}).get("code") != "boat_starting" or time.monotonic() >= deadline:
                raise RuntimeError(f"{node['name']}: could not install the root key: {error(result) or result.stderr.decode(errors='replace')}")
            time.sleep(1)
        self.wait_agent(node)
        # Incus guests have no firewall, and UFW drops WireGuard between sandboxes.
        self.exec(node, ["bash", "-ec", """
ufw disable >/dev/null
if [ -n "$(ss -Hlun 'sport = :51820')" ]; then echo "UDP port 51820 is taken" >&2; exit 1; fi
printf '%s\\n' "$@" | xargs -P8 -n1 docker pull -q >/dev/null
caddy_version=$(docker run --rm --entrypoint caddy caddy:2 version)
caddy_version=${caddy_version%% *}
docker tag caddy:2 "caddy:${caddy_version#v}"
""", "boot", self.run["corrosion"], *IMAGES], timeout=600)

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
                "-i", str(self.private / "id_ed25519"), "-o", "ControlMaster=auto", "-o", "ControlPersist=300",
                "-o", "ControlPath=" + str(self.control / "%C")]

    def exec(self, node, args, **kwargs):
        host, port = endpoint(self.sandbox(node)["ssh"])
        return self.commands(["ssh", *self.options(), "-p", port, "root@" + host, shlex.join(args)], **kwargs)

    def push(self, node, source, destination):
        directory = os.path.dirname(destination)
        with open(source, "rb") as content:
            self.exec(node, ["sh", "-ec", 'install -d "$1"; cat > "$2.push"; chmod 0755 "$2.push"; mv "$2.push" "$2"',
                             "push", directory, destination], stdin=content, timeout=600)

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
        for name, entry in record["sandboxes"].items():
            if entry.get("state") == "deleted" or not entry.get("id"):
                continue
            result = self.boat("delete", entry["id"], "--yes", check=False)
            failure = error(result) or {}
            if not result.returncode or failure.get("status") == 404:
                self.mark(name, state="deleted")
                continue
            if failure.get("status") != 403:
                raise RuntimeError(f"boat delete {entry['id']} failed: {failure or result.stderr.decode(errors='replace')}")
            if entry.get("state") != "stopped":
                self.boat("stop", entry["id"])
                self.mark(name, state="stopped")
            print(f"Stopped {entry['id']} ({name}): this Boat key may not delete it; it needs the sandbox.delete-own scope "
                  f"and to have created the sandbox.", file=sys.stderr)
        if all(entry.get("state") == "deleted" for entry in read(self.registry)["sandboxes"].values()):
            self.registry.unlink()

    def prepare(self):
        if not shutil.which(cli()[0]):
            raise RuntimeError(f"Boat CLI not found: {cli()[0]}; install it or set BOAT_CLI")
        self.run["image"] = SNAPSHOT
        self.run["bootstrap_release"] = SNAPSHOT_RELEASE
        self.run["corrosion"] = re.search(r'pub const IMAGE: &str = "([^"]+)"',
                                          (self.core / "crates/ployzd/src/corrosion/service.rs").read_text()).group(1)
