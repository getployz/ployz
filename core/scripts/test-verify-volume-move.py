#!/usr/bin/env python3
"""Prove the volume move RPC cut: the catalog, the lease scan, and moves under faults on real Machines.

  python3 scripts/test-verify-volume-move.py catalog
  python3 scripts/test-verify-volume-move.py lease
  python3 scripts/test-verify-volume-move.py run <manifest> [--soak [--size-gb 8]]

`run` needs a ready two-Machine Incus vc with Cloud and verify faults:
  PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults' \\
    scripts/verify-cluster up --machines 2 --daemon checkout --cloud
Hold /tmp/ployz-warm-move-vc.lock across up, the run and down. `--soak` holds about 8 GB of Postgres per
Machine copy, so give it room with `up --disk 48GiB --memory 4GiB`.
Results land in the run's evidence/volume-move-<time>/, one JSON per case and summary.json.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import shlex
import signal
import statistics
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True

CORE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(CORE / "scripts" / "verify"))
from cloud_recovery import alive  # noqa: E402

REPO = CORE.parent
HELPER = CORE / "scripts" / "verify-cluster"
CATALOG = CORE / "crates" / "ployz-core" / "src" / "rpc_catalog.rs"

DOCKER_VOLUME_VERBS = {"CreateVolume", "InspectStorage", "PrepareVolumes", "ListVolumes", "InspectVolume", "RemoveVolume"}
# CreateVolume and InspectVolume are dead verbs another change removes, so neither has to stay.
KEPT_VERBS = DOCKER_VOLUME_VERBS - {"CreateVolume", "InspectVolume"} | {"MetricHistory"}
MOVE_VERBS = {"InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"}

LEASE_ROOTS = ["core/crates", "core/scripts", "dashboard/src"]
LEASE_PATTERNS = {
    "lease record type": (re.compile(r"\bLease" + r"Record\b"), ""),
    "lease module": (re.compile(r"\bmod le" + r"ase\b"), ""),
    "switch position struct": (re.compile(r"\bP" + r"os\b"), ""),
    "after-step kill fault": (re.compile(r"\bkill_after" + r"_record\b"), ""),
    "call of the step writer": (re.compile(r"\brec" + r"ord\((?!\))"),
                                ("core/crates/ployzd/src/volume_plugin/", "core/crates/ployz-core/src/volume_switch.rs")),
}
LEASE_FILE = "le" + "ase.rs"

CELLS = ["Writer>Mirror", "Writer>Snapshot", "Writer>Forget", "Mirror>Writer", "Mirror>Mirror", "Mirror>Forget",
         "None>Mirror"]

# `machine` is the side the effect runs on: the Volume's writer before the case, or the other Machine.
# `fault` is a daemon fault point, set on `fault_on` or on both Machines; only the side that serves it fires it.
# `mirror` is whether the other Machine must hold a mirror before the case starts.
# `trigger` says when `action` runs: once a fault line appears in a journal, once `zfs receive` runs on
# `machine`, once the writer's db Container stops (the cut), or on the `nth` fault line `machine` logs after
# that stop. A `delay-rpc` names one cell, so it never slows the pull polls of another step past Cloud's timeout.
KILLS = [
    # A restart check, not a mid-effect resume: the kill lands after the take, so the resend finds the snapshot and only prunes.
    dict(name="kill-snapshot", cell="Writer>Snapshot", step="1-rounds", op="move", machine="writer",
         fault="kill-daemon:SetVolumeCopy.WriterToSnapshot", trigger="fault", action="none", recover="rerun"),
    dict(name="kill-pull", cell="Mirror>Mirror", step="1-rounds", op="move", machine="other", mirror="present",
         fault="kill-daemon:SetVolumeCopy.MirrorToMirror", trigger="fault", action="none", recover="rerun"),
    dict(name="kill-first-pull", cell="None>Mirror", step="mirror", op="mirror", machine="other", mirror="absent",
         fault="kill-daemon:SetVolumeCopy.NoneToMirror", trigger="fault", action="none", recover="rerun"),
    dict(name="kill-cut", cell="Writer>Mirror", step="3-cut", op="move", machine="writer",
         fault="kill-daemon:SetVolumeCopy.WriterToMirror", trigger="fault", action="none", recover="rerun"),
    dict(name="kill-writer", cell="Mirror>Writer", step="5-writer", op="move", machine="other",
         fault="kill-daemon:SetVolumeCopy.MirrorToWriter", trigger="fault", action="none", recover="rerun"),
    # A restart check, not a mid-effect resume: the kill lands before the destroy, which has no midpoint.
    dict(name="kill-forget-mirror", cell="Mirror>Forget", step="mirror rm", op="mirror-rm", machine="other",
         mirror="present", fault="kill-daemon:SetVolumeCopy.MirrorToForget", trigger="fault", action="none",
         recover="rerun"),
    dict(name="kill-forget-writer", cell="Writer>Forget", step="mirror rm", op="mirror-rm", machine="writer",
         mirror="present", fault="kill-daemon:SetVolumeCopy.WriterToForget", trigger="fault", action="none",
         recover="rerun"),
]

CASES = [
    dict(name="clean", cell=None, step="move", op="move", machine="writer", fault=None, trigger=None,
         action="none", recover="rerun"),
    dict(name="undo-before-cut", cell="Writer>Writer", step="3-cut", op="move", machine="writer",
         fault_on="writer", fault="kill-daemon:SetVolumeCopy.WriterToMirror", trigger="fault", action="cancel",
         recover="release"),
    dict(name="undo-after-cut", cell="Mirror>Writer", step="5-writer", op="move", machine="other",
         fault_on="other", fault="delay-rpc:SetVolumeCopy.MirrorToWriter:90", trigger="fault-after-stop",
         action="cancel", recover="release"),
    *KILLS,
]

SOAK_STEPS = [
    ("1-rounds", "other", None, "receiving", 1),
    ("3-cut", "writer", None, "stopped", 1),
    ("4-final", "other", "delay-rpc:SetVolumeCopy.MirrorToMirror:6", "fault-after-stop", 1),
    ("5-writer", "other", "delay-rpc:SetVolumeCopy.MirrorToWriter:20", "fault-after-stop", 1),
]
SOAK_ACTIONS = [("runner-kill", None), ("kill-ployz", "writer"), ("kill-ployz", "other"),
                ("kill-docker", "writer"), ("kill-docker", "other")]


def soak_cases():
    clean = [dict(CASES[0], name=f"soak-clean-{number}") for number in range(1, 4)]
    faulted = []
    for step, side, fault, trigger, nth in SOAK_STEPS:
        for action, target in SOAK_ACTIONS:
            faulted.append(dict(
                name=f"soak-{step}-{action}" + (f"-{target}" if target else ""), cell=None, step=step,
                op="move", machine=target or side, trigger_machine=side, fault_on=side, fault=fault,
                trigger=trigger, nth=nth, action=action, recover="rerun",
                mirror="absent" if trigger == "receiving" else None))
    return clean + faulted


def stamped(output):
    lines = []
    for line in output.splitlines():
        at, _, rest = line.partition(" ")
        try:
            lines.append((float(at), rest))
        except ValueError:
            continue
    return lines


def nth_after(lines, moment, nth):
    later = [(at, line) for at, line in lines if at > moment]
    return later[nth - 1] if len(later) >= nth else None


TERMINAL = {"done", "failed", "cancelled", "lost", "not_started"}
AUXILIARY = "Failed to send signal SIGKILL to auxiliary processes"
BUSY_LIMIT = 180
# The trigger window opens this long after the barrier restart returns. It is a hold, not a bound: Moby's
# containerStop runs without cancellation, so a `docker stop` dockerd already took is not sure to land within
# Docker's default 10 s.
STOP_HOLD = 12
DAEMON, PLUGIN = "ployz.service", "ployz-volume-plugin.service"
RESTART = (f"systemctl restart {DAEMON} || {{ echo 'failed {DAEMON}'; exit 1; }}; "
           f"systemctl stop {PLUGIN} || {{ echo 'failed {PLUGIN}'; exit 1; }}; "
           f"for i in $(seq 60); do systemctl is-active --quiet {DAEMON} && break; sleep 1; done; "
           f"systemctl is-active --quiet {DAEMON} || {{ echo 'failed {DAEMON}'; exit 1; }}")
# WriterToMirror logs this as it starts, before it stops the writer's db, so a stop after it on the writer's own
# clock is this run's cut. The delay is 0, so it changes nothing the run does.
WITNESS = "delay-rpc:SetVolumeCopy.WriterToMirror:0"
STOP_TRIGGERS = {"stopped", "fault-after-stop"}
# Every unit that could step or slew a lab VM's clock. setup turns NTP off, and each clean start and case end checks
# that none of these is active.
SYNC_UNITS = ("systemd-timesyncd", "chrony", "chronyd", "ntp", "ntpsec", "openntpd")
# The host's realtime clock against CLOCK_MONOTONIC_RAW: a slew moves the difference at most chrony's 83333 ppm, and
# two reads around a sample cost at most HOST_JITTER. Anything faster is a step, and every host conversion is void.
HOST_SLEW = 0.084
HOST_JITTER = 0.005
SNAPSHOT_BOUND = 8
SERVICE = "db"
VOLUME = "data"
IMAGE = "postgres:16-alpine"


def is_witness(line):
    return "fault: delaying switch verb" in line and "SetVolumeCopy.WriterToMirror" in line


def witnessed_stop(witnesses, stops):
    """The first stop after this run's first cut witness. Both are the writer's own stamps, so no conversion sits
    between them."""
    if not witnesses:
        return None
    return min((at for at in stops if at > min(witnesses)), default=None)


def arming(case, side):
    """Each Machine's PLOYZD_FAULT for the case: its own fault, and the cut witness on the writer when a stop is the
    trigger. A daemon holds one fault, so a writer that already carries one cannot also witness."""
    plan = {}
    if case["fault"]:
        targets = [side[case["fault_on"]]] if case.get("fault_on") else [side["writer"], side["other"]]
        plan.update({machine: case["fault"] for machine in targets})
    if case["trigger"] in STOP_TRIGGERS:
        if side["writer"] in plan:
            raise RuntimeError(f"{case['name']} arms {plan[side['writer']]} on the writer, so it cannot witness the cut")
        plan[side["writer"]] = WITNESS
    return plan


class HostWatch:
    """Samples the host's realtime clock against CLOCK_MONOTONIC_RAW, which no time sync moves.

    The excursion of their difference over a case is the host's part of every movement margin, and a jump between
    two samples voids every host conversion in the case.
    """

    def __init__(self, realtime=time.time, monotonic=lambda: time.clock_gettime(time.CLOCK_MONOTONIC_RAW),
                 thread=True, every=0.5):
        self.realtime, self.monotonic, self.every = realtime, monotonic, every
        self.lock = threading.Lock()
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True) if thread else None
        self.last, self.low, self.high, self.jumps = None, None, None, []

    def loop(self):
        while not self.done.wait(self.every):
            self.sample()

    def start(self):
        if self.thread:
            self.thread.start()

    def stop(self):
        self.done.set()
        if self.thread and self.thread.is_alive():
            self.thread.join(timeout=5)

    def sample(self):
        with self.lock:
            mono = self.monotonic()
            diff = self.realtime() - mono
            if self.last:
                then, before = self.last
                if abs(diff - before) > HOST_SLEW * (mono - then) + HOST_JITTER:
                    self.jumps.append(diff - before)
            self.last = (mono, diff)
            self.low = diff if self.low is None else min(self.low, diff)
            self.high = diff if self.high is None else max(self.high, diff)

    def mark(self):
        with self.lock:
            self.last, self.low, self.high, self.jumps = None, None, None, []
        self.sample()

    def excursion(self):
        self.sample()
        with self.lock:
            return self.high - self.low, list(self.jumps)


def trigger_not_its(run, latest):
    """Cloud lets one run act on a Volume at a time, so a trigger that may have happened after the first run ended
    may be a later run's.

    Both times are on the host: latest is the last host time the trigger could have happened, from its own stamp
    plus every uncertainty, or from the poll that saw it when it has no stamp. The worker stamps finished_at.
    """
    if run.get("state") not in TERMINAL:
        return None
    if not run.get("finished_at"):
        return (f"the first run {run.get('id')} ended {run.get('state')} with no finished_at, so the trigger "
                "cannot be tied to it")
    ended = datetime.fromisoformat(run["finished_at"].replace("Z", "+00:00")).timestamp()
    if ended < latest:
        when = datetime.fromtimestamp(latest, timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        return (f"the first run {run.get('id')} ended {run.get('state')} at {run['finished_at']}, before the trigger "
                f"could have happened as late as {when}, so the trigger evidence is not its")
    return None


def catalog_diff(text):
    entries = {}
    for match in re.finditer(r"^\s*(\w+):\s*\(([^()]*)\),\s*$", text, re.M):
        fields = [field.strip().strip('"') for field in match[2].split(",")]
        if len(fields) >= 7:
            entries[match[1]] = fields[6]
    moves = {name for name, wire in entries.items() if wire.startswith("ployz.volume.")} - DOCKER_VOLUME_VERBS
    return dict(move_verbs=sorted(moves), extra=sorted(moves - MOVE_VERBS), missing=sorted(MOVE_VERBS - moves),
                kept_missing=sorted(KEPT_VERBS - set(entries)))


def tracked_files(repo=REPO):
    listed = subprocess.run(["git", "-C", str(repo), "ls-files", "-z", "--", *LEASE_ROOTS],
                            capture_output=True, check=True).stdout.decode()
    for relative in filter(None, listed.split("\0")):
        path = repo / relative
        try:
            yield relative, path.read_text()
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue


def lease_hits(files):
    hits = []
    for relative, text in files:
        if Path(relative).name == LEASE_FILE:
            hits.append(dict(pattern="lease file", path=relative, line=0, text=""))
        for number, line in enumerate(text.splitlines(), 1):
            for label, (pattern, scope) in LEASE_PATTERNS.items():
                if relative.startswith(scope) and pattern.search(line):
                    hits.append(dict(pattern=label, path=relative, line=number, text=line.strip()[:160]))
    return hits


def parse_json(text):
    text = (text or "").strip()
    for start in [0] + [index for index, char in enumerate(text) if char in "{["][::-1]:
        try:
            return json.loads(text[start:])
        except json.JSONDecodeError:
            continue
    return None


def clock_report(clocks):
    return {machine: dict(offset=round(offset, 6), uncertainty=round(error, 6)) for machine, (offset, error) in clocks.items()}


def run_rows(data):
    rows = []

    def walk(node):
        if isinstance(node, dict):
            if {"state", "kind", "id"} <= node.keys():
                rows.append(node)
                return
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)
    walk(data)
    return rows


def select_cases(cases, names):
    unknown = sorted(set(names) - {case["name"] for case in cases})
    if unknown:
        raise RuntimeError(f"Unknown cases: {unknown}")
    return [case for case in cases if not names or case["name"] in names]


class Skipped(RuntimeError):
    pass


def busy(reply):
    error = reply.get("error") if isinstance(reply, dict) else None
    message = str(error.get("message", "")) if error else ""
    if message.startswith("A run on this Volume just ended"):
        return dict(id="ended")
    if not message.startswith("VolumeBusy"):
        return None
    return (error.get("details") or {}).get("run") or dict(id="unknown")


def check_kill(fields):
    """On these guests `systemctl kill` exits 1 when it cannot signal the unit's auxiliary processes, though the
    main process died. Accept that only when the unit's MainPID or InvocationID moved."""
    landed = fields.get("before") != fields.get("after") or fields.get("invocation") != fields.get("invocation_after")
    if fields.get("rc") != "0" and not (fields.get("rc") == "1" and AUXILIARY in fields.get("out", "") and landed):
        raise RuntimeError(f"systemctl kill failed: {fields}")
    return dict(fields, landed=landed)


def machine_state(output):
    state = dict(readonly={}, containers=[], snapshots=[])
    for line in output.splitlines():
        kind, _, rest = line.partition(" ")
        if kind == "ro":
            dataset, _, value = rest.rpartition(" ")
            state["readonly"][dataset] = value
        elif kind == "ct":
            identifier, state_name, name = (rest.split(" ", 2) + ["", ""])[:3]
            state["containers"].append(dict(id=identifier, state=state_name, name=name))
        elif kind == "snap" and rest:
            state["snapshots"].append(rest)
    return state


def writable_copies(states):
    return sorted(f"{machine}:{dataset}" for machine, state in states.items()
                  for dataset, value in state["readonly"].items() if value == "off")


def final_writer(states):
    copies = writable_copies(states)
    return copies[0].split(":", 1)[0] if len(copies) == 1 else None


def placement(states):
    copies = writable_copies(states)
    containers = [(machine, container) for machine, state in states.items() for container in state["containers"]]
    running = [machine for machine, container in containers if container["state"] == "running"]
    failures = [] if len(copies) == 1 else [f"{len(copies)} writable copies: {copies}"]
    if len(running) != 1:
        failures.append(f"{len(running)} running db Containers: {running}")
    elif copies and running[0] != copies[0].split(":", 1)[0]:
        failures.append(f"db runs on {running[0]} but the writable copy is {copies}")
    return failures


def assess(states, lost, *, bound=SNAPSHOT_BOUND):
    failures = placement(states)
    if lost is None:
        failures.append("acknowledged ids could not be read back from the writer")
    elif lost:
        failures.append(f"{len(lost)} acknowledged ids lost, first {lost[:10]}")
    stopped = [f"{machine}:{container['name']}:{container['state']}" for machine, state in states.items()
               for container in state["containers"] if container["state"] != "running"]
    if stopped:
        failures.append(f"stopped db Containers: {stopped}")
    for machine, state in states.items():
        if len(state["snapshots"]) > bound:
            failures.append(f"{machine} holds {len(state['snapshots'])} snapshots, over {bound}")
    return failures


def read_acks(path):
    acks = []
    if not path or not Path(path).exists():
        return acks
    for line in Path(path).read_text().splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "ack" in event:
            acks.append((event["ack"], event["at"]))
    return sorted(acks, key=lambda ack: ack[1])


def window_gap(acks, start, end):
    gaps = [(later[1] - earlier[1], earlier[0], later[0]) for earlier, later in zip(acks, acks[1:])
            if start <= earlier[1] < end]
    if not gaps:
        return None, None
    gap = max(gaps)
    return round(gap[0], 3), [gap[1], gap[2]]


def downtime(results):
    gaps = [result["probe"]["longest_gap_seconds"] for result in results
            if result.get("op") == "move" and result.get("probe", {}).get("longest_gap_seconds") is not None]
    if not gaps:
        return dict(moves=0, p50=None, max=None)
    return dict(moves=len(gaps), p50=round(statistics.median(gaps), 3), max=max(gaps))


class VolumeMove:
    def __init__(self, manifest, *, pool="ployz", runner=subprocess.run, spawn=subprocess.Popen,
                 sleep=time.sleep, clock=time.time, size_gb=None, out=None, host=None):
        self.manifest = Path(manifest)
        self.run = json.loads(self.manifest.read_text())
        self.pool = pool
        self.runner, self.spawn, self.sleep, self.clock = runner, spawn, sleep, clock
        self.size_gb = size_gb
        self.machines = [machine["name"] for machine in self.run["machines"][:2]]
        self.out = Path(out) if out else self.manifest.parent / "evidence" / f"volume-move-{time.strftime('%Y%m%dT%H%M%S')}"
        self.physical = None
        self.probe = None
        self.probe_log = None
        self.case_name = None
        self.started_runs = set()
        self.busy_waits = []
        # Machine name to (offset, uncertainty) in seconds: its clock reads offset ahead of the host's.
        self.clocks = {}
        # Machine name to the most its offset has measurably moved since the clean start, uncertainty included.
        self.moved = {}
        self.host = host or HostWatch()
        self.ntp_restored = None

    def command(self, *args, check=True, timeout=600):
        command = [str(HELPER), *map(str, args)]
        try:
            result = self.runner(command, capture_output=True, text=True, timeout=timeout + 60)
        except OSError as error:
            raise RuntimeError(f"Could not run {' '.join(command[1:8])}: {type(error).__name__}: {error}") from error
        print(json.dumps(dict(command=command[1:8], exit_code=result.returncode,
                              stdout=result.stdout[-600:], stderr=result.stderr[-600:])), flush=True)
        if check and result.returncode:
            raise RuntimeError(f"Command failed: {command}\n{result.stdout[-2000:]}\n{result.stderr[-2000:]}")
        return result

    def sh(self, machine, script, *, check=True, timeout=300):
        return self.command("exec", "--timeout", timeout, self.manifest, machine, "--", "bash", "-ec", script,
                            check=check, timeout=timeout)

    def cli(self, *args, check=True, timeout=600):
        result = self.command("cli", "--timeout", timeout, self.manifest, "--", "--json", *args,
                              check=check, timeout=timeout)
        return result.returncode, parse_json(result.stdout) or parse_json(result.stderr) or dict(
            raw=(result.stdout + result.stderr)[-1500:])

    def other(self, machine):
        return self.machines[1] if machine == self.machines[0] else self.machines[0]

    def root(self):
        return f"{self.pool}/ployz/{self.physical}"

    def fs(self):
        return f"{self.pool}/ployz-mirror/{self.physical}/fs"

    def state(self, machine):
        datasets = " ".join(shlex.quote(dataset) for dataset in (self.root(), self.fs()))
        script = (f"for d in {datasets}; do echo \"ro $d $(zfs get -H -o value readonly $d 2>/dev/null || echo absent)\"; done\n"
                  f"for d in {datasets}; do zfs list -H -t snapshot -o name -d 1 $d 2>/dev/null | sed 's/^/snap /'; done\n"
                  f"/usr/bin/docker ps -a --no-trunc --filter volume={shlex.quote(self.physical)} "
                  "--format 'ct {{.ID}} {{.State}} {{.Names}}'")
        return machine_state(self.sh(machine, script).stdout)

    def states(self):
        return {machine: self.state(machine) for machine in self.machines}

    def writer(self):
        states = self.states()
        return final_writer(states) or next(
            (machine for machine, state in states.items()
             if any(container["state"] == "running" for container in state["containers"])), self.machines[0])

    def has_mirror(self, machine):
        return self.state(machine)["readonly"].get(self.fs()) not in (None, "absent")

    def running_container(self):
        for machine, state in self.states().items():
            running = [container for container in state["containers"] if container["state"] == "running"]
            if running:
                return machine, running[0]["id"]
        return None, None

    def healthy(self, limit=300):
        deadline = self.clock() + limit
        while True:
            machine, container = self.running_container()
            if container and not self.sh(machine, f"/usr/bin/docker exec {container} pg_isready -U postgres",
                                         check=False).returncode:
                return machine
            if self.clock() >= deadline:
                return None
            self.sleep(3)

    def setup(self):
        first, second = self.machines
        self.cli("server", "set", second, "--accepts-services=false")
        self.cli("project", "new", "verify")
        self.cli("link", "--project", "verify")
        self.cli("service", "add", SERVICE, "--image", IMAGE)
        self.cli("set", f"{SERVICE}.env.POSTGRES_PASSWORD=vc-only-password")
        size = f"{self.size_gb * 2 + 2}GB" if self.size_gb else "2GB"
        # The image ships files at this path, so every move makes Docker populate at the handover (#1626).
        self.cli("volume", "add", VOLUME, "--size", size, "--mount", f"{SERVICE}:/var/lib/postgresql/data")
        self.cli("deploy", timeout=1500)
        self.cli("server", "set", second, "--accepts-services=true")
        _, inspect = self.cli("volume", "inspect", VOLUME)
        environment = inspect["environment"]
        self.physical = f"{environment['project']}-{environment['name']}_vol-{inspect['id']}"
        self.sh(second, f"/usr/bin/docker pull -q {IMAGE}; /usr/bin/docker volume create -d ployz -o size=1g seed && "
                        "for i in $(seq 10); do /usr/bin/docker volume rm seed && exit 0; sleep 1; done; exit 1",
                timeout=900)
        if self.healthy() != first:
            raise RuntimeError(f"{SERVICE} never became ready on {first}")
        if self.size_gb:
            machine, container = self.running_container()
            scale = max(1, round(self.size_gb * 1024 / 15))
            self.sh(machine, f"/usr/bin/docker exec {container} pgbench -i -q -s {scale} -U postgres postgres",
                    timeout=7200)
        for machine in self.machines:
            self.sh(machine, "timedatectl set-ntp false")
        self.measure_clocks()

    def measure_clocks(self):
        self.clocks = self.read_clocks()
        self.moved = {}
        return clock_report(self.clocks)

    def read_clocks(self, samples=3):
        """Read each Machine's clock against the host's, keeping the sample with the shortest round trip.

        The reading was taken somewhere between the host times before and after the exec, so the offset is
        exact to within half the round trip.
        """
        clocks = {}
        for machine in self.machines:
            best = None
            for _ in range(samples):
                before = self.clock()
                reading = self.sh(machine, "date +%s.%N", check=False).stdout.strip()
                after = self.clock()
                try:
                    sample = (float(reading) - (before + after) / 2, (after - before) / 2)
                except ValueError:
                    raise RuntimeError(f"could not read the clock on {machine}: {reading[:200]!r}") from None
                best = sample if best is None or sample[1] < best[1] else best
            clocks[machine] = best
        return clocks

    def margins(self):
        """How far a stamp converted with the clean start's offsets may sit from its host time.

        Lab VMs run with time sync off, so a VM's offset to host realtime moves smoothly between two readings, and
        its measured movement bounds it. That movement is measured again at the trigger and at the case end, which
        can come long after the 900 s trigger wait: wait_run allows 1800 s, a recovery another 1800 s, and healthy
        300 s. Both calibration uncertainties and the host's own excursion add to it.
        """
        span = self.host.excursion()[0]
        return {machine: 2 * self.clocks[machine][1] + self.moved.get(machine, 0.0) + span
                for machine in self.machines}

    def recheck(self, case, side, since, when, readings, result):
        """Measure every offset again and grant the trigger's credit again under the margins that movement gives.
        Its last possible host time only ever grows."""
        try:
            clocks = self.read_clocks()
        except RuntimeError as error:
            return [f"{error} at {when}"]
        readings.append(dict(at=when, clocks=clock_report(clocks)))
        _, jumps = self.host.excursion()
        if jumps:
            return [f"the host clock jumped {jumps[0]:+.3f}s during the case, so no host conversion holds"]
        for machine, (offset, error) in clocks.items():
            self.moved[machine] = max(self.moved.get(machine, 0.0), abs(offset - self.clocks[machine][0]) + error)
        if case["trigger"] == "receiving" or not result.get("triggered"):
            return []
        margins = self.margins()
        found = self.trigger(case, side, since, margins)
        if not found:
            moved = ", ".join(f"{machine} {self.moved[machine]:.3f}s" for machine in self.machines)
            return [f"the trigger no longer holds once clock movement since the clean start is counted at {when}: "
                    f"{result['triggered']} ({moved})"]
        result["triggered_at"] = max(result["triggered_at"], found[1])
        return []

    def sync_problems(self, machine):
        """Why time sync is not proven off on machine: an active sync unit, NTP on, or a reply cut short."""
        script = ("set +e; for u in " + " ".join(SYNC_UNITS) + "; do echo \"unit $u $(systemctl is-active $u)\"; done; "
                  "echo \"ntp $(timedatectl show -p NTP --value)\"; echo end")
        result = self.sh(machine, script, check=False)
        lines = result.stdout.splitlines()
        if result.returncode or lines[-1:] != ["end"]:
            return [f"exit {result.returncode}, {result.stdout[:200]!r}"]
        problems, seen = [], set()
        for line in lines[:-1]:
            kind, _, rest = line.partition(" ")
            if kind == "unit" and len(rest.split(" ")) == 2 and rest.split(" ")[0] in SYNC_UNITS:
                unit, state = rest.split(" ")
                seen.add(unit)
                if state not in ("inactive", "failed"):
                    problems.append(f"{unit} is {state}")
            elif kind == "ntp" and "NTP" not in seen:
                seen.add("NTP")
                if rest != "no":
                    problems.append(f"NTP is {rest or 'unread'}")
            else:
                problems.append(f"unknown line {line[:100]!r}")
        missing = sorted((set(SYNC_UNITS) | {"NTP"}) - seen)
        return problems + ([f"no reply for {', '.join(missing)}"] if missing else [])

    def window(self, machine, since):
        """journalctl's --since for a host time, in machine's clock, with slack the host-side filter discards."""
        return int(since + self.clocks[machine][0]) - 2

    def start_probe(self):
        evidence = self.manifest.parent / "evidence"
        before = set(evidence.glob(f"ackprobe-{SERVICE}-*.jsonl"))
        self.out.mkdir(parents=True, exist_ok=True)
        self.probe_output = (self.out / "probe-report.json").open("w")
        self.probe_errors = (self.out / "probe.stderr").open("w")
        self.probe = self.spawn([str(HELPER), "probe", str(self.manifest), SERVICE], stdout=self.probe_output,
                                stderr=self.probe_errors)
        deadline = self.clock() + 300
        while not (self.probe_log and read_acks(self.probe_log)):
            created = sorted(set(evidence.glob(f"ackprobe-{SERVICE}-*.jsonl")) - before)
            self.probe_log = created[-1] if created else None
            if self.probe.poll() is not None or self.clock() >= deadline:
                raise RuntimeError("The probe never acknowledged an insert")
            self.sleep(1)

    def stop_probe(self):
        if not self.probe:
            return None
        self.probe.send_signal(signal.SIGINT)
        try:
            self.probe.wait(timeout=600)
        except subprocess.TimeoutExpired:
            self.probe.kill()
        self.probe_output.close()
        self.probe_errors.close()
        return parse_json((self.out / "probe-report.json").read_text())

    def lost(self):
        acked = {ack for ack, _ in read_acks(self.probe_log)}
        machine, container = self.running_container()
        if not container:
            return None
        result = self.sh(machine, f"/usr/bin/docker exec {container} psql -U postgres -Atc 'SELECT id FROM ackprobe'",
                         check=False, timeout=600)
        if result.returncode:
            return None
        return sorted(acked - {int(line) for line in result.stdout.split() if line.isdigit()})

    def fault(self, value, machine=None, check=True):
        script = "touch /etc/default/ployz; sed -i '/^PLOYZD_FAULT=/d' /etc/default/ployz; "
        if value:
            script += f"echo {shlex.quote('PLOYZD_FAULT=' + value)} >> /etc/default/ployz; "
        script += RESTART
        targets = [machine] if machine else self.machines
        return [target for target in targets if self.sh(target, script, check=check).returncode]

    def journal(self, machine, since):
        """machine's fault lines with their raw stamps, or None when the read did not finish."""
        result = self.sh(machine, f"set -o pipefail; journalctl -u {DAEMON} -u {PLUGIN} "
                                  f"--since @{self.window(machine, since)} --no-pager -o short-unix "
                                  "| { grep -F 'fault:' || test $? = 1; }", check=False)
        return None if result.returncode else stamped(result.stdout)

    def fault_lines(self, machine, since, margins):
        """machine's fault lines, not the cut witness, as (host time, line), kept only when after since by more
        than its margin. An unread journal gives None."""
        lines = self.journal(machine, since)
        if lines is None:
            return None
        offset = self.clocks[machine][0]
        return [(at - offset, line) for at, line in lines
                if not is_witness(line) and at - offset - margins[machine] >= since]

    def fired(self, since, margins):
        found = [(at + margins[machine], f"{machine}: {line[:300]}")
                 for machine in self.machines if (lines := self.fault_lines(machine, since, margins))
                 for at, line in lines[:1]]
        return min(found)[::-1] if found else None

    def stopped_at(self, writer, since, margins):
        """The writer's first db stop after this run's cut witness, in host time. The witness must be after since by
        more than the writer's margin; the stop is ordered against it on the writer's own clock."""
        lines = self.journal(writer, since)
        result = self.sh(writer, f"set -o pipefail; /usr/bin/docker ps -aq --filter volume={shlex.quote(self.physical)} "
                                 "--filter status=exited | xargs -r /usr/bin/docker inspect -f '{{.State.FinishedAt}}' "
                                 "| xargs -r -n1 date +%s.%N -d", check=False)
        if lines is None or result.returncode:
            return None
        offset = self.clocks[writer][0]
        witnesses = [at for at, line in lines if is_witness(line) and at - offset - margins[writer] >= since]
        stops = [float(word) for word in result.stdout.split() if re.fullmatch(r"\d+\.\d+", word)]
        stop = witnessed_stop(witnesses, stops)
        return None if stop is None else stop - offset

    def stopped(self, writer, since, margins):
        at = self.stopped_at(writer, since, margins)
        return (f"{writer}: db stopped at {at}", at + margins[writer]) if at is not None else None

    def fault_after_stop(self, writer, machine, since, nth, margins):
        """Each margin counts against the trigger, so movement can delay crediting a fault but never move it past
        the stop."""
        stopped = self.stopped_at(writer, since, margins)
        if stopped is None:
            return None
        lines = self.fault_lines(machine, since, margins)
        if lines is None:
            return None
        found = nth_after([(at - margins[machine], line) for at, line in lines], stopped + margins[writer], nth)
        if not found:
            return None
        at, line = found
        return f"{machine}: {line[:300]}", at + 2 * margins[machine]

    def receiving(self, machine):
        """The receiving trigger: a receive into this Volume's mirror. A poll that did not finish withholds credit."""
        pattern = shlex.quote(f"zfs (recv|receive) .*ployz-mirror/{self.physical}/fs")
        result = self.sh(machine, f"set +e; pgrep -f {pattern} >/dev/null; echo \"pgrep $?\"", check=False)
        return not result.returncode and result.stdout.strip() == "pgrep 0"

    def trigger(self, case, side, since, margins):
        """What the trigger saw, and the last host time it could have happened, or None."""
        machine = side[case.get("trigger_machine", case["machine"])]
        if case["trigger"] == "fault":
            return self.fired(since, margins)
        if case["trigger"] == "receiving":
            # A process has no stamp, so the poll that saw it bounds when it ran.
            return (f"{machine}: zfs receive running", self.clock()) if self.receiving(machine) else None
        if case["trigger"] == "stopped":
            return self.stopped(side["writer"], since, margins)
        return self.fault_after_stop(side["writer"], machine, since, case.get("nth", 1), margins)

    def busy_work(self, machine, op):
        """What on machine could still act on a Volume: datasets with a `ployz:pull` set and zfs receive processes.

        The plugin sets `pull` when it admits a pull or a seed, before any process exists, and clears it when the
        work ends. A read that does not prove it ran to its end, or that holds a line it cannot place, skips the
        case, since it may have hidden either.
        """
        script = ("set +e; "
                  f"pulls=$(zfs list -H -o name,ployz:pull -t filesystem -r {shlex.quote(self.pool)}); echo \"zfs $?\"; "
                  "recv=$(pgrep -af 'zfs (recv|receive)'); echo \"pgrep $?\"; "
                  "printf '%s\\n' \"$pulls\" | sed '/^$/d; s/^/pull /'; printf '%s\\n' \"$recv\" | sed '/^$/d; s/^/recv /'; "
                  "echo end")
        result = self.sh(machine, script, check=False)
        lines = result.stdout.splitlines()
        codes = dict(line.split(" ", 1) for line in lines[:2] if " " in line)
        body = lines[2:-1]
        unknown = [line for line in body if not line.startswith(("pull ", "recv "))]
        if result.returncode or set(codes) != {"zfs", "pgrep"} or lines[-1:] != ["end"] or unknown:
            raise Skipped(f"could not read the pulls and receives on {machine} before {op}: "
                          f"exit {result.returncode}, {result.stdout[:200]!r}")
        pulls = [line[5:].split("\t") for line in body if line.startswith("pull ")]
        receives = [line[5:] for line in body if line.startswith("recv ")]
        if codes["zfs"] != "0" or not any(fields[0] == self.pool for fields in pulls) or any(len(f) != 2 for f in pulls):
            raise Skipped(f"could not read ployz:pull on {machine} before {op}: {result.stdout[:200]!r}")
        if (codes["pgrep"], bool(receives)) not in (("0", True), ("1", False)):
            raise Skipped(f"could not read the zfs receives on {machine} before {op}: "
                          f"pgrep exited {codes['pgrep']} with {len(receives)} records")
        return ([("a pull", f"ployz:pull={value} on {name}") for name, value in pulls if value != "-"]
                + [("a zfs receive", line) for line in receives])

    def units(self, machine, op, pids=()):
        """Each unit's MainPID, ActiveState and InvocationID on machine, and which of pids still run."""
        script = ("set +e; for u in " + f"{DAEMON} {PLUGIN}" + "; do echo \"unit $u $(systemctl show -p MainPID "
                  "-p ActiveState -p InvocationID $u | tr '\\n' ' ')\"; done; "
                  f"for p in {' '.join(str(pid) for pid in pids)}; do kill -0 $p 2>/dev/null && echo \"alive $p\"; done; "
                  "echo end")
        result = self.sh(machine, script, check=False)
        lines = result.stdout.splitlines()
        units, alive = {}, set()
        for line in lines[:-1]:
            kind, _, rest = line.partition(" ")
            if kind == "unit" and rest.split(" ", 1)[0] in (DAEMON, PLUGIN):
                unit, _, fields = rest.partition(" ")
                units[unit] = dict(field.split("=", 1) for field in fields.split() if "=" in field)
            elif kind == "alive" and rest.isdigit():
                alive.add(int(rest))
            else:
                units = {}
                break
        if result.returncode or lines[-1:] != ["end"] or set(units) != {DAEMON, PLUGIN} or any(
                not {"MainPID", "ActiveState", "InvocationID"} <= set(unit) for unit in units.values()):
            raise Skipped(f"could not read {DAEMON} and {PLUGIN} on {machine} before {op}: "
                          f"exit {result.returncode}, {result.stdout[:200]!r}")
        return units, alive

    def barrier(self, op):
        """Restart every Machine's daemon and stop its plugin, so no in-process task from earlier work outlives it,
        then hold past any `docker stop` the daemon already handed to dockerd.

        The restart counts only once it is proven: the old processes are gone, the daemon runs a new invocation, and
        the plugin is stopped or socket-activated again as a new one. After the hold, time sync must be off and the
        writer's db must accept connections, so a stop the trigger credits is this run's.
        """
        for machine in self.machines:
            before, _ = self.units(machine, op)
            pids = [int(unit["MainPID"]) for unit in before.values() if unit["MainPID"].isdigit() and int(unit["MainPID"])]
            restart = self.sh(machine, RESTART, check=False)
            after, alive = self.units(machine, op, pids)
            daemon, plugin = after[DAEMON], after[PLUGIN]
            problems = ([f"the restart exited {restart.returncode}: {restart.stdout.strip()[:200]}"] if restart.returncode else []) + [
                f"process {pid} from before still runs" for pid in sorted(alive)]
            if daemon["ActiveState"] != "active" or daemon["InvocationID"] in ("", before[DAEMON]["InvocationID"]):
                problems.append(f"{DAEMON} is {daemon['ActiveState']} on invocation {daemon['InvocationID'] or 'none'}")
            if plugin["ActiveState"] != "inactive" and plugin["InvocationID"] in ("", before[PLUGIN]["InvocationID"]):
                problems.append(f"{PLUGIN} is {plugin['ActiveState']} on invocation {plugin['InvocationID'] or 'none'}")
            if problems:
                raise Skipped(f"the clean-start restart is unproven on {machine} before {op}: {'; '.join(problems)}")
        self.sleep(STOP_HOLD)
        for machine in self.machines:
            if problems := self.sync_problems(machine):
                raise Skipped(f"time sync is not proven off on {machine} before {op}: {'; '.join(problems)}")
        writer = self.writer()
        ready = self.sh(writer, f"c=$(/usr/bin/docker ps -q --filter volume={shlex.quote(self.physical)} "
                                "--filter status=running); test -n \"$c\" && /usr/bin/docker exec $c pg_isready -U postgres",
                        check=False)
        if ready.returncode or "accepting connections" not in ready.stdout:
            raise Skipped(f"the db on {writer} is not accepting connections after the clean-start hold before {op}")

    def runs(self):
        _, data = self.cli("volume", "runs", VOLUME, check=False)
        return run_rows(data)

    def listed_runs(self, op):
        code, data = self.cli("volume", "runs", VOLUME, check=False)
        if code:
            # An unread list would hide a run that crossed the clean start or the armed fault.
            raise Skipped(f"could not list the Volume's runs before {op}")
        return run_rows(data)

    def start_op(self, op, destination, arm=None, clean=False):
        """Request op once the Volume is idle.

        A clean start also waits out every admitted pull and zfs receive on every Machine, then restarts every
        Machine so no earlier work survives in a process, checks quiet again and measures the clocks into since.
        It skips the case when a run appears after that check."""
        args = dict(move=["volume", "move", VOLUME, "--to", destination],
                    mirror=["volume", "mirror", VOLUME, "--to", destination],
                    release=["volume", "release", VOLUME])
        args["mirror-rm"] = ["volume", "mirror", "rm", f"{VOLUME}-{destination}"]
        began = self.clock()
        deadline = began + BUSY_LIMIT
        holder = skip = receiver = waited_on = work = None
        armed = checked = restarted = False
        stale = set()
        clean = clean or bool(arm)
        while True:
            holder = self.wait_idle(deadline) or holder
            if clean and not checked:
                listed = self.listed_runs(op)
                active = [row for row in listed if row.get("state") not in TERMINAL]
                found = [] if active else [(machine, item) for machine in self.machines
                                           for item in self.busy_work(machine, op) if (machine, item) not in stale]
                if active:
                    holder, receiver = active[0], None
                elif found:
                    (receiver, work), waited_on = found[0], found[0][0]
                else:
                    receiver = None
                if self.clock() >= deadline and (active or found):
                    # A restart leaves `pull` set until a run asks again, so a pull with no receive by the limit is
                    # left to the barrier, which ends any task behind it.
                    if found and not restarted and all(kind == "a pull" for _, (kind, _) in found):
                        stale |= set(found)
                    else:
                        skip = (f"{work[0]} still ran on {receiver} past {BUSY_LIMIT}s before {op}: {work[1][:200]}"
                                if receiver
                                else f"VolumeBusy past {BUSY_LIMIT}s before the fault was armed on {op}" if arm
                                else f"VolumeBusy past {BUSY_LIMIT}s before {op}")
                        break
                elif active or found:
                    restarted = False
                    self.sleep(4)
                    continue
                if not restarted:
                    self.barrier(op)
                    restarted = True
                    continue
                settled = {row["id"] for row in listed}
                self.calibration = self.measure_clocks()
                self.since = self.clock()
                checked = True
                if arm:
                    arm()
                    armed = True
                    # A run that started and ended while arm() restarted the daemon may have fired the fault, and
                    # wait_trigger would credit its fault line to this op.
                    if others := [row for row in self.listed_runs(op) if row["id"] not in settled]:
                        holder = others[0]
                        skip = f"a run took the Volume while the fault was armed on {op}"
                        break
            code, data = self.cli(*args[op], check=False)
            refused = busy(data)
            if refused and not (refused["id"] == "ended" and holder):
                holder = refused
            if not refused:
                break
            if armed:
                # A retry would let the fault fire inside that run and pass the case on its fault line.
                skip = f"a run took the Volume while the fault was armed on {op}"
                break
            if self.clock() >= deadline:
                skip = f"VolumeBusy past {BUSY_LIMIT}s on {op}"
                break
            # The run that refused this request may leave work behind, so start clean again after it.
            checked = restarted = False
            self.sleep(4)
        if checked and not skip:
            accepted = {row["id"] for row in run_rows(data)} if code == 0 else set()
            self.started_runs |= accepted
            if others := [row for row in self.listed_runs(op) if row["id"] not in settled | accepted]:
                holder = others[0]
                skip = (f"a run took the Volume while the fault was armed on {op}" if armed
                        else f"a run took the Volume between the clean start and {op}")
        if holder or waited_on:
            holder_row = holder or {}
            record = dict(case=self.case_name, op=op, holder=holder_row.get("id"), kind=holder_row.get("kind"),
                          receive_on=waited_on, stale=sorted(f"{machine}: {detail}" for machine, (_, detail) in stale),
                          waited_seconds=round(self.clock() - began, 1),
                          started_by_harness=holder_row.get("id") in self.started_runs)
            self.busy_waits.append(record)
            print(json.dumps(dict(volume_busy_wait=record)), flush=True)
        if skip:
            raise Skipped(f"{skip}: run {holder.get('id')} ({holder.get('kind')})" if holder else skip)
        approval = self.approve(data) if op == "release" else None
        if approval:
            code, data = self.cli(*args[op], "--approval", approval, check=False)
        rows = run_rows(data) if code == 0 else []
        row = rows[0] if rows else dict(state="refused", reply=data)
        if row.get("id"):
            self.started_runs.add(row["id"])
        return dict(row, approved=approval) if approval else row

    def wait_idle(self, deadline):
        """A cron sync or a run left from an earlier case holds the Volume; wait it out until the deadline."""
        holder = None
        while self.clock() < deadline and (active := [row for row in self.runs() if row.get("state") not in TERMINAL]):
            holder = holder or active[0]
            self.sleep(4)
        return holder

    def approve(self, reply):
        """Approve as the operator would in Cloud. A Release after the cut forgets the new copy, so Cloud asks."""
        error = reply.get("error") if isinstance(reply, dict) else None
        if not error or error.get("code") != "approval_required":
            return None
        details = error.get("details") or {}
        missing = ["details"] if not details else [field for field in ("approval_id", "approval") if not details.get(field)]
        if missing:
            raise RuntimeError(f"Cloud asked to approve the release without {' or '.join(missing)}: {reply}")
        token_file = self.manifest.parent / "private" / "cloud-token"
        if not token_file.exists():
            raise RuntimeError(f"Cloud asked to approve the release, but there is no Cloud token at {token_file}")
        token = token_file.read_text().strip()
        status, text = http("POST", f"{self.run['cloud']['url']}/api/cli/approvals/{details['approval_id']}",
                            dict(approve=dict(digest=details["approval"])), token=token)
        print(json.dumps(dict(approve=details["approval_id"], status=status, reply=text[:200])), flush=True)
        if not 200 <= status < 300:
            raise RuntimeError(f"Cloud refused the approval {details['approval_id']} with {status}: {text[:500]}")
        return details["approval_id"]

    def wait_run(self, row, limit=1800):
        if row.get("state") in TERMINAL | {"refused"}:
            return row
        deadline = self.clock() + limit
        last = row
        while self.clock() < deadline:
            last = next((dict(row, **found) for found in self.runs() if found["id"] == row["id"]), last)
            if last.get("state") in TERMINAL:
                return last
            self.sleep(4)
        return dict(last, state=f"still {last.get('state')} after {limit}s")

    def wait_trigger(self, case, row, since, side, limit=900):
        """What the trigger saw, and the last host time it could have happened."""
        margins = {machine: error for machine, (_, error) in self.clocks.items()}
        deadline = self.clock() + limit
        while self.clock() < deadline:
            if found := self.trigger(case, side, since, margins):
                return dict(seen=found[0], latest=found[1])
            current = next((found for found in self.runs() if found["id"] == row.get("id")), row)
            if current.get("state") in TERMINAL | {"refused"}:
                return None
            self.sleep(1 if case["trigger"] == "fault" else 0.5)
        return None

    def runner_state(self, state):
        self.command("runner", state, self.manifest, timeout=180)

    def dashboard_env(self):
        dashboard = Path(self.run["cloud"]["dashboard"])
        pairs = (line.split("=", 1) for line in (dashboard / ".verify" / "run" / "env").read_text().splitlines()
                 if "=" in line)
        return dict(pairs), "ployz-verify-" + hashlib.sha1(str(dashboard.parent).encode()).hexdigest()[:10]

    def inngest_run(self, row):
        """Cloud records the Inngest run that claimed a request; a request no run picked up has none."""
        _, postgres = self.dashboard_env()
        sql = f"select inngest_run_id from volume_run where id = '{row['id']}'"
        query = self.runner(["docker", "exec", postgres, "psql", "-U", "postgres", "-d", "ployz_cloud", "-Atc", sql],
                            capture_output=True, text=True, timeout=60)
        if query.returncode:
            raise RuntimeError(f"psql exited {query.returncode} on {sql!r}: {(query.stderr or '').strip()[:500]}")
        return query.stdout.strip()

    def unclaimed(self, run):
        """A request no Inngest run claimed sent nothing to a Machine, so whatever the trigger saw is another run's."""
        if self.inngest_run(run):
            return None
        return (f"the first run {run.get('id')} ended {run.get('state')} and no Inngest run claimed it, so the trigger "
                "evidence is not its")

    def cancel(self, row):
        run_id = self.inngest_run(row)
        base = self.dashboard_env()[0]["INNGEST_BASE_URL"]
        status, text = http("DELETE", f"{base}/v1/runs/{run_id}")
        attempts = [f"DELETE {run_id} {status} {text[:200]}"]
        landed = 200 <= status < 300
        if not landed:
            mutation = "mutation($envID: UUID!, $runID: ULID!) { cancelRun(envID: $envID, runID: $runID) { id } }"
            for env_id in ("00000000-0000-4000-b000-000000000000", "00000000-0000-0000-0000-000000000000"):
                status, text = http("POST", f"{base}/v0/gql", dict(query=mutation, variables=dict(envID=env_id, runID=run_id)))
                attempts.append(f"gql {env_id} {status} {text[:200]}")
                landed = status == 200 and '"errors"' not in text
                if landed:
                    break
        return [dict(kill="cancel", attempts=attempts, landed=landed)]

    def runner_kill(self):
        """verify-cluster's runner kill succeeds without killing anything when the recorded worker is already dead."""
        pid_file = Path(self.run["cloud"]["dashboard"]) / ".verify" / "run" / "worker.pid"
        old = int(pid_file.read_text())
        alive_before = alive(old)
        self.runner_state("kill")
        alive_after = alive(old)
        self.sleep(5)
        self.runner_state("start")
        new = int(pid_file.read_text())
        return [dict(kill="runner", pid=old, alive_before=alive_before, alive_after_kill=alive_after, new_pid=new,
                     landed=alive_before and not alive_after and new != old and alive(new))]

    def act(self, action, machine, row):
        if action == "cancel":
            # Every later case needs the runner, so it starts again however the cancel ends.
            try:
                self.runner_state("kill")
                return self.cancel(row)
            finally:
                self.runner_state("start")
        if action == "runner-kill":
            return self.runner_kill()
        if action in ("kill-ployz", "kill-docker"):
            unit = "ployz.service" if action == "kill-ployz" else "docker.service"
            kill = self.sh(machine, f"""
before=$(systemctl show -p MainPID --value {unit}); invocation=$(systemctl show -p InvocationID --value {unit})
rc=0; out=$(systemctl kill -s KILL {unit} 2>&1) || rc=$?
sleep 1
echo "rc=$rc"; echo "out=$(printf %s "$out" | tr '\\n' ' ')"; echo "before=$before"; echo "invocation=$invocation"
echo "after=$(systemctl show -p MainPID --value {unit})"
echo "invocation_after=$(systemctl show -p InvocationID --value {unit})"
""", check=False)
            fields = dict(line.split("=", 1) for line in kill.stdout.splitlines() if "=" in line)
            self.sleep(3)
            self.sh(machine, "systemctl start docker.service ployz.service", check=False)
            return [check_kill(dict(fields, kill=unit, machine=machine))]
        return []

    def prepare(self, case, other):
        if case.get("mirror") == "present" and not self.has_mirror(other):
            self.wait_run(self.start_op("mirror", other))
        if case.get("mirror") == "absent" and self.has_mirror(other):
            self.wait_run(self.start_op("mirror-rm", other))

    def evidence(self, name, since):
        for machine in self.machines:
            journal = self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service --since @{self.window(machine, since)} --no-pager -o short-iso",
                              check=False, timeout=120).stdout
            (self.out / f"{name}-{machine}.journal").write_text(journal)
            listing = self.sh(machine, f"zfs list -t all -H -o name,used,refer,readonly -r {self.pool}",
                              check=False).stdout
            (self.out / f"{name}-{machine}.zfs").write_text(listing)

    def run_case(self, case):
        self.case_name = case["name"]
        writer = self.writer()
        other = self.other(writer)
        side = dict(writer=writer, other=other)
        destination = other
        self.prepare(case, other)
        clocks = self.measure_clocks()
        started = self.clock()
        # Arm only once the Volume is idle: a cron sync crosses the same SetVolumeCopy points and would trip it.
        plan = arming(case, side)
        arm = (lambda: [self.fault(value, machine) for machine, value in plan.items()]) if plan else None
        self.host.mark()
        # A trigger must be the first run's, so it starts behind the clean-start barrier on every Machine.
        row = self.start_op(case["op"], destination, arm, clean=bool(case["trigger"]))
        if case["trigger"]:
            started, clocks = self.since, self.calibration
        readings = [dict(at="the clean start", clocks=clocks)]
        result = dict(case, writer_before=writer, destination=destination, started=started, clocks=readings)
        if row.get("state") == "refused":
            # Busy refusals are skips and approvals are answered, so this is a defect; a rerun would pass it.
            raise RuntimeError(f"{case['op']} was refused: {json.dumps(row.get('reply'))[:1500]}")
        result["run"] = row
        if case["trigger"]:
            found = self.wait_trigger(case, row, started, side)
            result["triggered"] = found and found["seen"]
            if found:
                result["triggered_at"] = found["latest"]
                # The action lands first: every read before it delays it past the moment the trigger names.
                result["action_evidence"] = self.act(case["action"], side[case["machine"]], row)
                result["recheck"] = self.recheck(case, side, started, "the trigger", readings, result)
        if plan:
            self.fault(None)
        result["run"] = self.wait_run(row)
        result["recovery"] = []
        if result["run"].get("state") != "done":
            op = "release" if case["recover"] == "release" else case["op"]
            result["recovery"].append(self.wait_run(self.start_op(op, destination)))
        self.healthy()
        states = self.states()
        result.update(ended=self.clock(), states=states, final_writer=final_writer(states), runs=self.runs()[:6])
        lost = self.lost()
        result["probe"] = dict(lost=lost)
        result["failures"] = assess(states, lost)
        if case["trigger"]:
            result["failures"] += result.pop("recheck", []) + self.recheck(case, side, started, "the case end",
                                                                             readings, result)
            result["failures"] += [f"time sync is not proven off on {machine} at the case end: {'; '.join(problems)}"
                                   for machine in self.machines if (problems := self.sync_problems(machine))]
        if case["trigger"] and not result.get("triggered"):
            result["failures"].append("the trigger never fired before the run ended")
        if result.get("triggered") and (unowned := trigger_not_its(result["run"], result["triggered_at"])
                                        or self.unclaimed(result["run"])):
            result["failures"].append(unowned)
        evidence = result.get("action_evidence")
        if result.get("triggered") and case["action"] != "none" and not (evidence and all(e["landed"] for e in evidence)):
            result["failures"].append(f"no evidence the {case['action']} landed: {json.dumps(evidence)[:500]}")
        if case["action"] == "cancel" and result.get("triggered") and result["run"].get("state") not in ("cancelled", "lost"):
            result["failures"].append(f"the cancel did not land: the run ended {result['run'].get('state')}")
        last = (result["recovery"] or [result["run"]])[-1]
        if last.get("state") != "done":
            result["failures"].append(f"the last run ended {last.get('state')}")
        # A move or its rerun ends on the destination; a mirror op never moves the writer, and an undo's Release restores it.
        expected = destination if case["op"] == "move" and case["recover"] == "rerun" else writer
        if result["final_writer"] != expected:
            result["failures"].append(f"the Volume ended on {result['final_writer']}, not {expected}")
        if case["op"] in ("mirror", "mirror-rm"):
            want = "present" if case["op"] == "mirror" else "absent"
            got = "absent" if states[destination]["readonly"].get(self.fs()) in (None, "absent") else "present"
            if got != want:
                result["failures"].append(f"the mirror on {destination} ended {got}, not {want}")
        result["passed"] = not result["failures"]
        if not result["passed"]:
            result["heal"] = self.heal(destination)
        result["settled"] = self.clock()
        self.evidence(case["name"], started)
        (self.out / f"{case['name']}.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(dict(case=case["name"], passed=result["passed"], final_writer=result["final_writer"],
                              failures=result["failures"])), flush=True)
        return result

    def heal(self, destination):
        attempts = []
        for op in ("move", "release"):
            if self.healthy(limit=5) and not placement(self.states()):
                break
            try:
                attempts.append(dict(op=op, run=self.wait_run(self.start_op(op, destination))))
            except Skipped as skip:
                attempts.append(dict(op=op, skipped=str(skip)))
        machine = self.healthy()
        states = self.states()
        unhealed = assess(states, self.lost()) + ([] if machine else ["no database answers"])
        return dict(attempts=attempts, healthy_on=machine, writable=writable_copies(states), unhealed=unhealed,
                    healed=not unhealed)

    def run_all(self, cases):
        self.out.mkdir(parents=True, exist_ok=True)
        results, escaped = [], None
        try:
            self.host.start()
            self.setup()
            self.start_probe()
            for case in cases:
                try:
                    result = self.run_case(case)
                except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
                    result = dict(case, passed=False, failures=[f"{type(error).__name__}: {error}"[:2000]])
                    if isinstance(error, Skipped):
                        result["skipped"] = str(error)
                    try:
                        failed = (self.fault(None, check=False)
                                  if case["fault"] or case["trigger"] in STOP_TRIGGERS else [])
                        if failed:
                            raise RuntimeError(f"disarming the fault failed on {', '.join(failed)}")
                        result["heal"] = self.heal(self.other(self.writer()))
                    except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as cleanup:
                        result["heal"] = dict(healed=False, error=f"{type(cleanup).__name__}: {cleanup}")
                heal = result.get("heal", dict(healed=True))
                if not heal["healed"]:
                    # A later case would run on what this one left, armed or unhealed, and pass or fail on it.
                    reason = heal.get("error") or f"after the heal, {'; '.join(heal['unhealed'])}"
                    result["failures"].append(f"the run stopped after this case: {reason}"[:2000])
                (self.out / f"{case['name']}.json").write_text(json.dumps(result, indent=2) + "\n")
                results.append(result)
                if not heal["healed"]:
                    break
        except BaseException as error:
            escaped = f"{type(error).__name__}: {error}"[:2000]
            raise
        finally:
            self.host.stop()
            # Lab VMs need their clocks synced again once the cases no longer read them.
            self.ntp_restored = {machine: not self.sh(machine, "timedatectl set-ntp true", check=False).returncode
                                 for machine in self.machines}
            report = self.stop_probe()
            summary = self.summarize(results, report, escaped)
        return summary

    def summarize(self, results, report, escaped):
        acks = read_acks(self.probe_log)
        for result in results:
            if "settled" in result:
                gap, between = window_gap(acks, result["started"], result["settled"])
                result["probe"].update(longest_gap_seconds=gap, longest_gap_between=between)
                (self.out / f"{result['name']}.json").write_text(json.dumps(result, indent=2) + "\n")
        summary = dict(
            manifest=str(self.manifest), head=subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"],
                                                              capture_output=True, text=True).stdout.strip(),
            physical=self.physical, size_gb=self.size_gb, probe=report, downtime=downtime(results),
            passed=not escaped and all(result["passed"] for result in results) and bool(report)
            and not report.get("lost"),
            error=escaped,
            cases=[dict(name=result["name"], cell=result.get("cell"), step=result.get("step"),
                        passed=result["passed"], skipped=result.get("skipped"),
                        final_writer=result.get("final_writer"),
                        run=result.get("run", {}).get("state"),
                        recovery=[row.get("state") for row in result.get("recovery", [])],
                        gap=result.get("probe", {}).get("longest_gap_seconds"), failures=result["failures"])
                   for result in results],
            volume_busy_waits=self.busy_waits, ntp_restored=self.ntp_restored)
        (self.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        return summary


def http(method, url, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"} | ({"authorization": f"Bearer {token}"} if token else {})
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read().decode()[:600]
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()[:600]
    except OSError as error:
        return 0, str(error)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="action", required=True)
    commands.add_parser("catalog")
    commands.add_parser("lease")
    run = commands.add_parser("run")
    run.add_argument("manifest", type=Path)
    run.add_argument("--soak", action="store_true")
    run.add_argument("--size-gb", type=int, default=8, help="Postgres data for --soak, in GB")
    run.add_argument("--case", action="append", default=[], help="Run only this case; repeatable")
    args = parser.parse_args()
    if args.action == "catalog":
        diff = catalog_diff(CATALOG.read_text())
        print(json.dumps(diff, indent=2))
        return 0 if not (diff["extra"] or diff["missing"] or diff["kept_missing"]) else 1
    if args.action == "lease":
        hits = lease_hits(tracked_files())
        for hit in hits:
            print(f"{hit['path']}:{hit['line']}: [{hit['pattern']}] {hit['text']}")
        print(json.dumps(dict(hits=len(hits))))
        return 1 if hits else 0
    cases = select_cases(soak_cases() if args.soak else CASES, args.case)
    manifest = json.loads(args.manifest.read_text())
    if manifest["provider"] != "incus" or manifest["state"] != "ready" or len(manifest["machines"]) != 2 \
            or "cloud" not in manifest:
        raise RuntimeError("Use a ready two-Machine Incus vc started with --cloud")
    checks = VolumeMove(args.manifest, size_gb=args.size_gb if args.soak else None)
    summary = checks.run_all(cases)
    print(json.dumps(dict(summary=str(checks.out / "summary.json"), passed=summary["passed"],
                          downtime=summary["downtime"])), flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        print(f"volume-move: {error}", file=sys.stderr)
        sys.exit(1)
