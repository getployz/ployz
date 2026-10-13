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
import hashlib
import json
from pathlib import Path
import re
import shlex
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True

CORE = Path(__file__).resolve().parents[1]
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
    later = [line for at, line in lines if at > moment]
    return later[nth - 1] if len(later) >= nth else None


TERMINAL = {"done", "failed", "cancelled", "lost", "not_started"}
AUXILIARY = "Failed to send signal SIGKILL to auxiliary processes"
BUSY_LIMIT = 180
SNAPSHOT_BOUND = 8
SERVICE = "db"
VOLUME = "data"
IMAGE = "postgres:16-alpine"


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
    if not error or not str(error.get("message", "")).startswith("VolumeBusy"):
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


def assess(states, lost, *, bound=SNAPSHOT_BOUND):
    failures = []
    copies = writable_copies(states)
    if len(copies) != 1:
        failures.append(f"{len(copies)} writable copies: {copies}")
    if lost is None:
        failures.append("acknowledged ids could not be read back from the writer")
    elif lost:
        failures.append(f"{len(lost)} acknowledged ids lost, first {lost[:10]}")
    containers = [(machine, container) for machine, state in states.items() for container in state["containers"]]
    stopped = [f"{machine}:{container['name']}:{container['state']}" for machine, container in containers
               if container["state"] != "running"]
    if stopped:
        failures.append(f"stopped db Containers: {stopped}")
    running = [machine for machine, container in containers if container["state"] == "running"]
    if len(running) != 1:
        failures.append(f"{len(running)} running db Containers: {running}")
    elif copies and running[0] != copies[0].split(":", 1)[0]:
        failures.append(f"db runs on {running[0]} but the writable copy is {copies}")
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
                 sleep=time.sleep, clock=time.time, size_gb=None, out=None):
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

    def command(self, *args, check=True, timeout=600):
        command = [str(HELPER), *map(str, args)]
        result = self.runner(command, capture_output=True, text=True, timeout=timeout + 60)
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
        self.cli("set", f"{SERVICE}.env.POSTGRES_PASSWORD=vc-only-password", f"{SERVICE}.env.PGDATA=/data/pgdata")
        size = f"{self.size_gb * 2 + 2}GB" if self.size_gb else "2GB"
        self.cli("volume", "add", VOLUME, "--size", size, "--mount", f"{SERVICE}:/data")
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

    def fault(self, value, machine=None):
        script = "touch /etc/default/ployz; sed -i '/^PLOYZD_FAULT=/d' /etc/default/ployz; "
        if value:
            script += f"echo {shlex.quote('PLOYZD_FAULT=' + value)} >> /etc/default/ployz; "
        script += ("systemctl restart ployz.service; systemctl stop ployz-volume-plugin.service || true; "
                   "for i in $(seq 60); do systemctl is-active --quiet ployz.service && break; sleep 1; done")
        for target in [machine] if machine else self.machines:
            self.sh(target, script)

    def fired(self, since):
        for machine in self.machines:
            lines = self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service --since @{int(since) - 2} --no-pager -o cat "
                                     "| grep -F 'fault:' || true", check=False).stdout.strip()
            if lines:
                return f"{machine}: {lines.splitlines()[0][:300]}"
        return None

    def fault_lines(self, machine, since):
        return stamped(self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service --since @{int(since) - 2} --no-pager -o short-unix "
                                        "| grep -F 'fault:' || true", check=False).stdout)

    def stopped_at(self, machine, since):
        result = self.sh(machine, f"/usr/bin/docker ps -aq --filter volume={shlex.quote(self.physical)} --filter status=exited "
                                  "| xargs -r /usr/bin/docker inspect -f '{{.State.FinishedAt}}' | xargs -r -n1 date +%s.%N -d",
                         check=False)
        times = [float(word) for word in result.stdout.split() if re.fullmatch(r"\d+\.\d+", word)]
        times = [at for at in times if at >= since]
        return min(times) if times else None

    def stopped(self, writer, since):
        at = self.stopped_at(writer, since)
        return f"{writer}: db stopped at {at}" if at else None

    def fault_after_stop(self, writer, machine, since, nth):
        stopped = self.stopped_at(writer, since)
        line = None if stopped is None else nth_after(self.fault_lines(machine, since), stopped, nth)
        return f"{machine}: {line[:300]}" if line else None

    def receiving(self, machine):
        result = self.sh(machine, "pgrep -f 'zfs (recv|receive)' >/dev/null && echo yes || echo no", check=False)
        return result.stdout.strip() == "yes"

    def runs(self):
        _, data = self.cli("volume", "runs", VOLUME, check=False)
        return run_rows(data)

    def start_op(self, op, destination):
        args = dict(move=["volume", "move", VOLUME, "--to", destination],
                    mirror=["volume", "mirror", VOLUME, "--to", destination],
                    release=["volume", "release", VOLUME])
        args["mirror-rm"] = ["volume", "mirror", "rm", f"{VOLUME}-{destination}"]
        began = self.clock()
        deadline = began + BUSY_LIMIT
        holder = None
        while True:
            holder = self.wait_idle(deadline) or holder
            code, data = self.cli(*args[op], check=False)
            refused = busy(data)
            holder = holder or refused
            if not refused or self.clock() >= deadline:
                break
            self.sleep(4)
        if holder:
            record = dict(case=self.case_name, op=op, holder=holder.get("id"), kind=holder.get("kind"),
                          waited_seconds=round(self.clock() - began, 1),
                          started_by_harness=holder.get("id") in self.started_runs)
            self.busy_waits.append(record)
            print(json.dumps(dict(volume_busy_wait=record)), flush=True)
        if refused:
            raise Skipped(f"VolumeBusy past {BUSY_LIMIT}s on {op}: run {refused.get('id')} ({refused.get('kind')})")
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
        details = error["details"]
        token = (self.manifest.parent / "private" / "cloud-token").read_text().strip()
        status, text = http("POST", f"{self.run['cloud']['url']}/api/cli/approvals/{details['approval_id']}",
                            dict(approve=dict(digest=details["approval"])), token=token)
        print(json.dumps(dict(approve=details["approval_id"], status=status, reply=text[:200])), flush=True)
        return details["approval_id"] if 200 <= status < 300 else None

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
        machine = side[case.get("trigger_machine", case["machine"])]
        triggers = {
            "fault": lambda: self.fired(since),
            "receiving": lambda: f"{machine}: zfs receive running" if self.receiving(machine) else None,
            "stopped": lambda: self.stopped(side["writer"], since),
            "fault-after-stop": lambda: self.fault_after_stop(side["writer"], machine, since, case.get("nth", 1)),
        }
        deadline = self.clock() + limit
        while self.clock() < deadline:
            seen = triggers[case["trigger"]]()
            if seen:
                return seen
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

    def cancel(self, row):
        environment, postgres = self.dashboard_env()
        query = self.runner(["docker", "exec", postgres, "psql", "-U", "postgres", "-d", "ployz_cloud", "-Atc",
                             f"select inngest_run_id from volume_run where id = '{row['id']}'"],
                            capture_output=True, text=True, timeout=60)
        run_id = query.stdout.strip()
        base = environment["INNGEST_BASE_URL"]
        status, text = http("DELETE", f"{base}/v1/runs/{run_id}")
        evidence = [f"DELETE {run_id} {status} {text[:200]}"]
        if status not in (200, 202, 204):
            mutation = "mutation($envID: UUID!, $runID: ULID!) { cancelRun(envID: $envID, runID: $runID) { id } }"
            for env_id in ("00000000-0000-4000-b000-000000000000", "00000000-0000-0000-0000-000000000000"):
                status, text = http("POST", f"{base}/v0/gql", dict(query=mutation, variables=dict(envID=env_id, runID=run_id)))
                evidence.append(f"gql {env_id} {status} {text[:200]}")
                if status == 200 and '"errors"' not in text:
                    break
        return evidence

    def act(self, action, machine, row):
        if action == "cancel":
            self.runner_state("kill")
            return self.cancel(row)
        if action == "runner-kill":
            self.runner_state("kill")
            self.sleep(5)
            self.runner_state("start")
        elif action in ("kill-ployz", "kill-docker"):
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
            journal = self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service --since @{int(since) - 2} --no-pager -o short-iso",
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
        started = self.clock()
        result = dict(case, writer_before=writer, destination=destination, started=started)
        if case["fault"]:
            self.fault(case["fault"], side.get(case.get("fault_on")))
        row = self.start_op(case["op"], destination)
        result["run"] = row
        if case["trigger"]:
            result["triggered"] = self.wait_trigger(case, row, started, side)
            if result["triggered"]:
                result["action_evidence"] = self.act(case["action"], side[case["machine"]], row)
        if case["fault"]:
            self.fault(None)
        if case["action"] == "cancel":
            self.runner_state("start")
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
        if case["trigger"] and not result.get("triggered"):
            result["failures"].append("the trigger never fired before the run ended")
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
            if self.healthy(limit=5) and len(writable_copies(self.states())) == 1:
                break
            try:
                attempts.append(dict(op=op, run=self.wait_run(self.start_op(op, destination))))
            except Skipped as skip:
                attempts.append(dict(op=op, skipped=str(skip)))
        machine = self.healthy()
        return dict(attempts=attempts, healthy_on=machine)

    def run_all(self, cases):
        self.out.mkdir(parents=True, exist_ok=True)
        self.setup()
        self.start_probe()
        results = []
        try:
            for case in cases:
                try:
                    results.append(self.run_case(case))
                except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
                    result = dict(case, passed=False, failures=[f"{type(error).__name__}: {error}"[:2000]])
                    if isinstance(error, Skipped):
                        result["skipped"] = str(error)
                    if case["fault"]:
                        self.fault(None)
                    result["heal"] = self.heal(self.other(self.writer()))
                    (self.out / f"{case['name']}.json").write_text(json.dumps(result, indent=2) + "\n")
                    results.append(result)
        finally:
            report = self.stop_probe()
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
            passed=all(result["passed"] for result in results) and bool(report) and not report.get("lost"),
            cases=[dict(name=result["name"], cell=result.get("cell"), step=result.get("step"),
                        passed=result["passed"], skipped=result.get("skipped"),
                        final_writer=result.get("final_writer"),
                        run=result.get("run", {}).get("state"),
                        recovery=[row.get("state") for row in result.get("recovery", [])],
                        gap=result.get("probe", {}).get("longest_gap_seconds"), failures=result["failures"])
                   for result in results],
            volume_busy_waits=self.busy_waits)
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
