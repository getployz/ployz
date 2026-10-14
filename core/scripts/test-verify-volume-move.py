#!/usr/bin/env python3
"""Prove the volume move RPC cut: the catalog, the lease scan, and moves under faults on real Machines.

  python3 scripts/test-verify-volume-move.py catalog
  python3 scripts/test-verify-volume-move.py lease
  python3 scripts/test-verify-volume-move.py run <manifest> [--case NAME ...] [--soak [--size-gb 8]]

`run` plays every case below in order, or only the named ones:
  clean             a move with no fault
  deploy-plain      a Deploy with no move: it lands this case's version on the writer, and its seconds are the baseline
  deploy-warm       a Deploy while B's warm pull is held: the move yields, the app stays on A, one requeue
  deploy-after-cut  a Deploy after the cut, before commit: the move yields, the app goes back to A, one requeue
  deploy-committed  a Deploy after commit: it follows the move to B, no requeue
  undo-before-cut   `volume release` while warm: the move is cancelled, the app stays on A
  undo-after-cut    `volume release` after commit: refused, the move finishes on B
  kill-*            the daemon killed inside each copy cell, one kill per cell

`run` needs a ready two-Machine vc, Incus or Boat, with Cloud and verify faults:
  PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults' \\
    scripts/verify-cluster up --machines 2 --daemon checkout --cloud
A Boat vc (the default) needs no lock. For an Incus vc (`up --provider incus`), hold /tmp/ployz-warm-move-vc.lock
across up, the run and down. `--soak` holds about 8 GB of Postgres per Machine copy; a Boat sandbox has that room,
and an Incus vc needs `up --provider incus --disk 48GiB --memory 4GiB`.
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

SERVICE = "db"
VOLUME = "data"
IMAGE = "postgres:16-alpine"
# A plain Deploy measured 10.3 s p50, 12.3 s p95 and 17.5 s max by CLI wall time on one Incus VM, and the disk
# flip it may meet holds at most 0.78 s. A Deploy that takes the lane adds one floor call and at most one promote
# (1.3 s max measured), so 20 s over the baseline covers the measured spread twice over and still fails a Deploy
# that sat out more than 20 s of a hold. The release by disarm alone passes such a Deploy, since the disarm follows
# it whenever it returns inside the hold. Wall time here can only fail a case; credit rests on the release.
DEPLOY_MARGIN = 20
# A SetVolumeCopy held 90 s outlasts Cloud's 10 s call, but Inngest retries the step for about 2.5 min, so the move
# survives the hold.
HOLD = 90
# A takeover ends the taken row in the same transaction, so a row still open this long after it never will.
TAKEN_LIMIT = 60
TAKEN = {"yielded", "cancelled"}
MOUNT_ERRORS = ("Provisioned Volume status has an invalid mountpoint", "error while mounting volume")

# `machine` is the side the effect runs on: the Volume's writer before the case, or the other Machine.
# `fault` is a daemon fault point, set on `fault_on` or on both Machines; only the side that serves it fires it.
# A `delay-rpc` holds every request to its cell, and the warm rounds and the final pull send the same one, so
# `arm="stopped"` arms it only once the first run's cut line is credited, past the warm rounds.
# `mirror` is whether the other Machine must hold a mirror before the case starts.
# `trigger` says when `action` runs: once a fault line appears in a journal, once `zfs receive` runs on
# `machine`, once the first run's cut has stopped every holder of the Volume on the writer, or on the first
# fault line `machine` logs for the first run's final pull after that cut; `committed` reads the first run's row
# in Cloud's Postgres. A `delay-rpc` names one cell, so it never slows the pull polls of another step past Cloud's
# timeout.
# `expect` names what `judge` checks beyond `assess`; a side name in it means that side's Machine.
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

def held(case):
    """Hold the case's cell for HOLD seconds on the destination, so the action provably lands inside it."""
    transition = case["cell"].replace(">", "To")
    return dict(case, fault_on="other", fault=f"delay-rpc:SetVolumeCopy.{transition}:{HOLD}", hold=HOLD)


CASES = [
    dict(name="clean", cell=None, step="move", op="move", machine="writer", fault=None, trigger=None,
         action="none", recover="rerun", expect=dict(move="done", writer="other")),
    dict(name="deploy-plain", cell=None, step="deploy", op=None, machine="writer", fault=None, trigger=None,
         action="deploy", recover="rerun", expect=dict(baseline=True, writer="writer", version=True)),
    held(dict(name="deploy-warm", cell="Mirror>Mirror", step="1-rounds", op="move", machine="other",
              mirror="present", trigger="fault", action="deploy", recover="rerun",
              expect=dict(move="yielded", requeues=1, writer="other", on_writer=True, inside_hold=True,
                          version=True))),
    held(dict(name="deploy-after-cut", cell="Mirror>Mirror", step="4-final", op="move", machine="other",
              arm="stopped", trigger="fault-after-stop", action="deploy", recover="rerun",
              expect=dict(move="yielded", requeues=1, writer="other", on_writer=True, inside_hold=True,
                          version=True))),
    held(dict(name="deploy-committed", cell="Mirror>Writer", step="5-writer", op="move", machine="other",
              trigger="committed", action="deploy", recover="rerun",
              expect=dict(move="done", requeues=0, writer="other", version=True))),
    held(dict(name="undo-before-cut", cell="Mirror>Mirror", step="1-rounds", op="move", machine="other",
              mirror="present", trigger="fault", action="release", recover="release",
              expect=dict(move="cancelled", release="done", writer="writer"))),
    held(dict(name="undo-after-cut", cell="Mirror>Writer", step="5-writer", op="move", machine="other",
              trigger="committed", action="release", recover="release",
              expect=dict(move="done", requeues=0, writer="other",
                          refusal=f"{VOLUME} is starting on {{other}}; it finishes there"))),
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


TERMINAL = {"done", "failed", "cancelled", "yielded", "lost", "not_started"}
LIVE = {"requested", "running"}
AUXILIARY = "Failed to send signal SIGKILL to auxiliary processes"
BUSY_LIMIT = 180
# The trigger window opens this long after the barrier restart returns. It is a hold, not a bound: Moby's
# containerStop runs without cancellation, so a `docker stop` dockerd already took is not sure to land within
# Docker's default 10 s.
STOP_HOLD = 12
DAEMON, PLUGIN = "ployz.service", "ployz-volume-plugin.service"
CGROUP = "/sys/fs/cgroup/system.slice"
RESTART = (f"test -d {CGROUP} || {{ echo 'no cgroup v2 system.slice'; exit 1; }}; "
           f"systemctl stop {DAEMON} {PLUGIN} || {{ echo 'failed stop'; exit 1; }}; "
           f"for u in {DAEMON} {PLUGIN}; do if test -d {CGROUP}/$u; then s=0; p=$(cat {CGROUP}/$u/cgroup.procs 2>/dev/null) || s=$?; "
           "echo \"procs $u $s $(printf %s \"$p\" | tr '\\n' ' ')\"; else echo \"procs $u absent\"; fi; done; "
           f"systemctl start {DAEMON} || {{ echo 'failed {DAEMON}'; exit 1; }}; "
           f"for i in $(seq 60); do systemctl is-active --quiet {DAEMON} && break; sleep 1; done; "
           f"systemctl is-active --quiet {DAEMON} || {{ echo 'failed {DAEMON}'; exit 1; }}; "
           "journalctl -n 0 --show-cursor -q; echo end")
# WriterToMirror logs this under the exclusive turn once every holder of the Volume stopped, naming the Volume and
# the want's target snapshot (local_volume_copy.rs).
CUT_LINE = "cut: holders stopped"
# faults.rs logs this when a held verb resumes, with `by=disarm` or `by=limit` and the held verb's identity.
RELEASE_LINE = "fault: releasing switch verb"
SNAPSHOT_BOUND = 8


def fault_script(value):
    """The file the daemons read a fault from, written whole so no check reads half a value."""
    if not value:
        return "rm -f /run/ployz/fault"
    return (f"mkdir -p /run/ployz && printf '%s\\n' {shlex.quote(value)} > /run/ployz/fault.tmp "
            "&& mv /run/ployz/fault.tmp /run/ployz/fault")


def cut_of(line):
    """The Volume, target and gen a cut line names, or None for any other line."""
    found = re.search(re.escape(CUT_LINE) + r" volume=(\S+) target=(\S+) gen=(\S+)", line)
    return found and found.groups()


def fault_fields(line):
    """The key=value fields a fault line carries after its message, or None for any other line."""
    _, found, rest = line.partition("fault: ")
    if not found:
        return None
    return {key: value.strip('"') for key, value in re.findall(r'(\w+)=("[^"]*"|\S+)', rest)}


def arming(case, side):
    """Each Machine's fault file value for the case."""
    if not case["fault"]:
        return {}
    targets = [side[case["fault_on"]]] if case.get("fault_on") else [side["writer"], side["other"]]
    return {machine: case["fault"] for machine in targets}


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


class Interrupted(BaseException):
    """SIGTERM or SIGHUP arrived. Not an Exception, so no handler on the way out mistakes it for a case failure."""


def interrupt(signum, frame):
    # One raise per process. A second signal would land inside the summary write and lose it.
    for each in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(each, signal.SIG_IGN)
    raise Interrupted(signal.Signals(signum).name)


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


def judge(result, side, baseline):
    """The case's own expectations, beyond the one-writer health `assess` checks."""
    expect, failures = result.get("expect", {}), []
    move, action = result.get("run") or {}, result.get("acted") or {}
    if "move" in expect and move.get("state") != expect["move"]:
        failures.append(f"move {move.get('id')} is {move.get('state')}, expected {expect['move']}")
    requeues = result.get("requeues", [])
    if "requeues" in expect and len(requeues) != expect["requeues"]:
        failures.append(f"{len(requeues)} requeued moves, expected {expect['requeues']}")
    failures += [f"requeued move {row['id']} ended {row['state']}, expected done"
                 for row in requeues if row.get("state") != "done"]
    if "writer" in expect and result.get("final_writer") != side[expect["writer"]]:
        failures.append(f"final writer {result.get('final_writer')}, expected {side[expect['writer']]}")
    if action.get("case") == "deploy":
        if action["exit_code"]:
            failures.append(f"the Deploy exited {action['exit_code']}: {action['output'][-300:]}")
        failures += [f"the Deploy printed '{error}': it met the Volume mid-cut" for error in action["mount_errors"]]
        if expect.get("on_writer") and action["running_after"] != [side["writer"]]:
            failures.append(f"after the Deploy {SERVICE} runs on {action['running_after']}, expected only {side['writer']}")
        if expect.get("inside_hold") and result.get("released") != "disarm":
            failures.append(f"the hold was released by {result.get('released')}, not the disarm after the Deploy: "
                            "the Deploy did not land inside it")
        if expect.get("inside_hold") and baseline is not None and action["seconds"] > baseline + DEPLOY_MARGIN:
            failures.append(f"the Deploy took {action['seconds']}s, over the {baseline}s baseline plus {DEPLOY_MARGIN}s")
        if expect.get("version") and result.get("version") != result["name"]:
            failures.append(f"{SERVICE} runs VC_DEPLOY={result.get('version')}, not this case's Deploy")
    if action.get("case") == "release":
        ended = next((row for row in result.get("later", []) if row["id"] == action["release"].get("id")),
                     action["release"])
        if "release" in expect and ended.get("state") != expect["release"]:
            failures.append(f"release ended {ended.get('state')}, expected {expect['release']}")
        if "refusal" in expect:
            text = expect["refusal"].format(**side)
            if ended.get("state") != "refused" or text not in json.dumps(ended.get("reply")):
                failures.append(f"release on a committed lane was not refused with '{text}': {json.dumps(ended)[:300]}")
            if action["changed"]:
                failures.append(f"release on a committed lane changed rows: {action['changed']}")
    return failures


def refused_run(row):
    """The run id a refused release names: Cloud's refusal carries the active lane it read inside the release."""
    reply = row.get("reply") if row.get("state") == "refused" else None
    error = reply.get("error") if isinstance(reply, dict) else None
    details = error.get("details") if isinstance(error, dict) else None
    run = details.get("run") if isinstance(details, dict) else None
    return run.get("id") if isinstance(run, dict) else None


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
        # Run id to the epoch Cloud recorded for it, and Machine name to the journal cursor its barrier read.
        self.epochs = {}
        self.cursor = {}

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
        # A run killed while armed leaves the fault file behind, and the first case would fire it.
        if failed := self.fault(None):
            raise RuntimeError(f"disarming a leftover fault failed on {', '.join(failed)}")
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
        """Arm or disarm without a restart, and return the Machines whose write failed: the daemons read the file on
        every check, and a restart would leave a Machine unanswered mid-move."""
        script = fault_script(value)
        targets = [machine] if machine else self.machines
        return [target for target in targets if self.sh(target, script, check=False).returncode]

    def arm(self, plan):
        if failed := [machine for machine, value in plan.items() if self.fault(value, machine)]:
            raise RuntimeError(f"arming the fault failed on {', '.join(failed)}")

    def journal(self, machine):
        """Every daemon and plugin journal entry on machine after its barrier cursor, as dict(cursor, boot_id, line),
        or None when the read did not finish or holds an entry it cannot parse."""
        result = self.sh(machine, f"journalctl -u {DAEMON} -u {PLUGIN} --after-cursor={shlex.quote(self.cursor[machine])} "
                                  "-o json --no-pager -q", check=False)
        if result.returncode:
            return None
        entries = []
        for text in result.stdout.splitlines():
            try:
                entry = json.loads(text)
            except ValueError:
                return None
            if not isinstance(entry, dict):
                return None
            message = entry.get("MESSAGE")
            entries.append(dict(cursor=entry.get("__CURSOR"), boot_id=entry.get("_BOOT_ID"),
                                line=message if isinstance(message, str) else ""))
        return entries

    def marks(self, run):
        """The epoch and gen Cloud recorded on run's row, as dict(epoch, gen). Gen is set when the row is inserted;
        the epoch stays None until the workflow takes one, and both stay None when the read fails."""
        if run.get("id") not in self.epochs:
            query = self.psql(f"select gen, coalesce(epoch, -1) from volume_run where id = '{run.get('id')}'")
            fields = query.stdout.strip().split("|")
            if query.returncode or len(fields) != 2 or not all(re.fullmatch(r"-?\d+", field) for field in fields):
                return dict(epoch=None, gen=None)
            gen, epoch = int(fields[0]), int(fields[1])
            if epoch < 0:
                return dict(epoch=None, gen=gen)
            self.epochs[run["id"]] = dict(epoch=epoch, gen=gen)
        return self.epochs[run["id"]]

    def want(self, case, marks):
        """The want fields a fault at case's step names for the first run, as field to pattern. Every want carries
        the run's gen, so a Forget, which names nothing else, is still this run's. None without a mark the want
        needs."""
        epoch, gen = marks["epoch"], marks["gen"]
        if gen is None:
            return None
        verb = case["fault"].split(":")[1]
        patterns = {"Snapshot": {"snapshot": rf"w-{epoch}-\d+"},
                    "Mirror": {"target": f"f-{epoch}" if case["step"] in ("3-cut", "4-final") else rf"w-{epoch}-\d+"},
                    "Writer": {"epoch": str(epoch)}}
        destination = verb.rsplit("To", 1)[-1]
        if destination not in patterns:
            return {"gen": str(gen)}
        return dict(patterns[destination], gen=str(gen)) if epoch is not None else None

    def its(self, case, marks, line):
        """The identity a fault line carries when it is the first run's: this Volume and the want its step implies.
        None for any other line. The verb needs no check: ployzd logs a fault line only for the armed verb."""
        fields, want = fault_fields(line), self.want(case, marks)
        if fields is None or want is None or fields.get("volume") != self.physical:
            return None
        if not all(re.fullmatch(pattern, fields.get(key, "")) for key, pattern in want.items()):
            return None
        return {key: fields[key] for key in ("volume", "target", "snapshot", "at", "epoch", "gen") if key in fields}

    def identity(self, case, marks, line):
        if case["trigger"] != "stopped":
            return self.its(case, marks, line)
        found = cut_of(line)
        if marks["epoch"] is None or found != (self.physical, f"f-{marks['epoch']}", str(marks["gen"])):
            return None
        return dict(volume=found[0], target=found[1], gen=found[2])

    def receiving(self, machine, epoch):
        """The receiving trigger: a receive into this Volume's mirror while its slot parent names a round pull of the
        first run's epoch. One shell; a reply that did not run to its end withholds credit."""
        if epoch is None:
            return None
        pattern = shlex.quote(f"zfs (recv|receive) .*ployz-mirror/{self.physical}/fs")
        parent = shlex.quote(f"{self.pool}/ployz-mirror/{self.physical}")
        result = self.sh(machine, f"set +e; pgrep -f {pattern} >/dev/null; echo \"pgrep $?\"; "
                                  f"zfs get -H -o value ployz:pull {parent}; echo \"zfs $?\"; echo end", check=False)
        lines = result.stdout.splitlines()
        if result.returncode or len(lines) != 4 or lines[0] != "pgrep 0" or lines[2:] != ["zfs 0", "end"]:
            return None
        if not re.fullmatch(rf"w-{epoch}-\d+", lines[1]):
            return None
        return dict(machine=machine, kind="receiving", line=f"ployz:pull={lines[1]}", cursor=None, boot_id=None,
                    identity=dict(volume=self.physical, target=lines[1]), poll_end_mono=time.monotonic())

    def committed(self, run):
        """The committed trigger: the first run's own row, read now, records the committed phase while it still
        runs. Cloud commits the move before it asks the new writer to start, so the action follows the commit."""
        query = self.psql(f"select phase, state from volume_run where id = '{run.get('id')}'")
        if query.returncode or query.stdout.strip() != "committed|running":
            return None
        return dict(machine=None, kind="committed", line=f"move {run['id']} phase committed state running",
                    cursor=None, boot_id=None, identity=dict(run=run["id"], phase="committed"),
                    poll_end_mono=time.monotonic())

    def trigger(self, case, side, run):
        """The event the trigger credits, or None: the first entry after the barrier cursor whose identity is the
        first run's. Position in one Machine's journal orders it; across Machines, Cloud's step order does."""
        if case["trigger"] == "committed":
            return self.committed(run)
        marks = self.marks(run)
        machine = side[case.get("trigger_machine", case["machine"])]
        if case["trigger"] == "receiving":
            return self.receiving(machine, marks["epoch"])
        entries = self.journal(machine)
        if entries is None:
            return None
        for entry in entries:
            if identity := self.identity(case, marks, entry["line"]):
                return dict(machine=machine, kind=case["trigger"], line=entry["line"][:300], cursor=entry["cursor"],
                            boot_id=entry["boot_id"], identity=identity, poll_end_mono=time.monotonic())
        return None

    def released(self, case, run, event):
        """How the credited hold ended: the `by` of the first release line after it with the same identity, or None
        before one. The harness disarms only after the action returned, so `disarm` proves the hold outlasted it."""
        entries = self.journal(event["machine"])
        cursors = [entry["cursor"] for entry in entries or []]
        if event["cursor"] not in cursors:
            return None
        for entry in entries[cursors.index(event["cursor"]) + 1:]:
            if RELEASE_LINE in entry["line"] and self.its(case, self.marks(run), entry["line"]) == event["identity"]:
                return fault_fields(entry["line"]).get("by")
        return None

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

    def proven_restart(self, machine, op):
        """Stop both units on machine, prove their control groups empty, start the daemon, and return the journal
        cursor read after it. Anything short of that skips the case: every credited event is ordered behind this
        restart."""
        result = self.sh(machine, RESTART, check=False)
        lines = result.stdout.splitlines()
        problems = []
        if result.returncode or lines[-1:] != ["end"]:
            problems.append(f"the restart exited {result.returncode}: {result.stdout.strip()[:200]}")
        cursor = None
        for line in lines:
            if line.startswith("procs "):
                fields = line.split()
                unit, status, pids = fields[1], fields[2] if len(fields) > 2 else "unread", fields[3:]
                if status == "absent":
                    continue
                if status != "0":
                    problems.append(f"cgroup.procs of {unit} was not read: cat exited {status}")
                elif pids:
                    problems.append(f"{unit} still holds {' '.join(pids)} after its stop")
            elif line.startswith("-- cursor: "):
                cursor = line.removeprefix("-- cursor: ").strip()
        if not cursor:
            problems.append("no journal cursor")
        if problems:
            raise Skipped(f"the clean-start restart is unproven on {machine} before {op}: {'; '.join(problems)}")
        return cursor

    def barrier(self, op):
        """Stop every Machine's daemon and plugin, prove no process of either outlived the stop, start the daemon
        again and record the journal cursor every credited event must follow. Then hold past any `docker stop` the
        daemon already handed to dockerd, and require the writer's db to accept connections.
        """
        for machine in self.machines:
            self.cursor[machine] = self.proven_restart(machine, op)
        self.sleep(STOP_HOLD)
        writer = self.writer()
        ready = self.sh(writer, f"c=$(/usr/bin/docker ps -q --filter volume={shlex.quote(self.physical)} "
                                "--filter status=running); test -n \"$c\" && /usr/bin/docker exec $c pg_isready -U postgres",
                        check=False)
        if ready.returncode or "accepting connections" not in ready.stdout:
            raise Skipped(f"the db on {writer} is not accepting connections after the clean-start hold before {op}")

    def runs(self):
        """Cloud's runs list, or None when the list failed or its reply holds no runs list. An unread list never
        stands in for what Cloud holds."""
        code, data = self.cli("volume", "runs", VOLUME, check=False)
        if code or not isinstance(data, dict) or not isinstance(data.get("runs"), list):
            return None
        return run_rows(data)

    def listed_runs(self, op):
        rows = self.runs()
        if rows is None:
            # An unread list would hide a run that crossed the clean start, the armed fault or the action.
            raise Skipped(f"could not list the Volume's runs before {op}")
        return rows

    def start_op(self, op, destination, arm=None, clean=False, wait=True):
        """Request op once the Volume is idle, or at once when wait is False.

        A clean start also waits out every admitted pull and zfs receive on every Machine, then restarts every
        Machine so no earlier work survives in a process, checks quiet again and marks since.
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
            holder = (self.wait_idle(deadline) if wait else None) or holder
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
                self.since = self.clock()
                checked = True
                if arm:
                    arm()
                    armed = True
                    # A run that started and ended around arm() may have fired the fault, and wait_trigger would
                    # credit its fault line to this op.
                    if others := [row for row in self.listed_runs(op) if row["id"] not in settled]:
                        holder = others[0]
                        skip = f"a run took the Volume while the fault was armed on {op}"
                        break
            code, data = self.cli(*args[op], check=False)
            refused = busy(data) if wait else None
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
        while self.clock() < deadline and (
                (rows := self.runs()) is None or (active := [row for row in rows if row.get("state") not in TERMINAL])):
            holder = holder or (active[0] if rows is not None else None)
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
        if row is None or row.get("state") in TERMINAL | {"refused"}:
            return row
        deadline = self.clock() + limit
        last = row
        while self.clock() < deadline:
            last = next((dict(row, **found) for found in self.runs() or [] if found["id"] == row["id"]), last)
            if last.get("state") in TERMINAL:
                return last
            self.sleep(4)
        return dict(last, state=f"still {last.get('state')} after {limit}s")

    def row(self, row):
        """row as Cloud lists it now, or None when the list is unread, lacks it, or gives it a state Cloud has no
        name for."""
        found = next((found for found in self.runs() or [] if found["id"] == row.get("id")), None)
        return found if found and found.get("state") in LIVE | TERMINAL else None

    def settle(self, known):
        """Wait out every row Cloud started since the case began: requeues, and releases onFailure started."""
        known, ended = set(known), []
        while True:
            rows = [row for row in self.listed_runs("the settle") if row["id"] not in known]
            if not rows:
                self.sleep(4)
                rows = [row for row in self.listed_runs("the settle") if row["id"] not in known]
                if not rows:
                    return ended[::-1]
            for row in rows:
                known.add(row["id"])
                ended.append(self.wait_run(row))

    def wait_trigger(self, case, row, side, limit=900):
        """The credited event, or None once the first run ends without one."""
        deadline = self.clock() + limit
        while self.clock() < deadline:
            if credited := self.trigger(case, side, row):
                return credited
            if (now := self.row(row)) and now["state"] in TERMINAL:
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

    def psql(self, sql):
        _, postgres = self.dashboard_env()
        return self.runner(["docker", "exec", postgres, "psql", "-U", "postgres", "-d", "ployz_cloud", "-Atc", sql],
                           capture_output=True, text=True, timeout=60)

    def query(self, sql):
        query = self.psql(sql)
        if query.returncode:
            raise RuntimeError(f"psql exited {query.returncode} on {sql!r}: {(query.stderr or '').strip()[:500]}")
        return query.stdout.strip()

    def inngest_run(self, row):
        """Cloud records the Inngest run that claimed a request; a request no run picked up has none."""
        return self.query(f"select inngest_run_id from volume_run where id = '{row['id']}'")

    def requeued(self, run):
        """The moves Cloud requested again for run, as rows: a Deploy that took run stores run's args as its resume,
        and its end requests a move with exactly those args, with no user, after the Deploy's own row."""
        first = f"(select volume_id, args, gen from volume_run where id = '{run['id']}')"
        rows = self.query(
            "select again.id, again.state from volume_run again, " + first + " as first "
            "where again.volume_id = first.volume_id and again.kind = 'move' and again.args = first.args "
            "and again.requested_by_user_id is null and exists (select 1 from volume_run took "
            "where took.volume_id = first.volume_id and took.kind = 'deploy' and took.args -> 'resume' = first.args "
            "and took.gen > first.gen and took.gen < again.gen) order by again.gen")
        return [dict(id=id, state=state) for id, state in (line.split("|", 1) for line in rows.splitlines())]

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

    def runner_kill(self, row):
        """verify-cluster's runner kill succeeds without killing anything when the recorded worker is already dead.
        The row is read right after the kill: the settle and the restart are not the action."""
        pid_file = Path(self.run["cloud"]["dashboard"]) / ".verify" / "run" / "worker.pid"
        old = int(pid_file.read_text())
        alive_before = alive(old)
        self.runner_state("kill")
        alive_after = alive(old)
        after = self.row(row)
        self.sleep(5)
        self.runner_state("start")
        new = int(pid_file.read_text())
        return [dict(kill="runner", pid=old, alive_before=alive_before, alive_after_kill=alive_after, new_pid=new,
                     landed=alive_before and not alive_after and new != old and alive(new))], after

    def running_on(self):
        return sorted(machine for machine, state in self.states().items()
                      if any(container["state"] == "running" for container in state["containers"]))

    def version(self):
        machine, container = self.running_container()
        if not container:
            return None
        env = self.sh(machine, f"/usr/bin/docker inspect -f '{{{{range .Config.Env}}}}{{{{println .}}}}{{{{end}}}}' "
                               f"{container} | grep '^VC_DEPLOY=' || true", check=False).stdout.strip()
        return env.partition("=")[2] or None

    def deploy(self, name):
        self.cli("set", f"{SERVICE}.env.VC_DEPLOY={name}")
        began = self.clock()
        result = self.command("cli", "--timeout", 1500, self.manifest, "--", "--json", "deploy", check=False,
                              timeout=1500)
        seconds = round(self.clock() - began, 2)
        output = result.stdout + result.stderr
        return dict(exit_code=result.returncode, seconds=seconds, running_after=self.running_on(),
                    mount_errors=[error for error in MOUNT_ERRORS if error in output], output=output[-1500:])

    def release(self, move):
        before = self.listed_runs("the release")
        row = self.start_op("release", None, wait=False)
        after = self.listed_runs("the release's changes")
        seen = {found["id"]: found["state"] for found in before}
        changed = [f"{found['id']} {seen.get(found['id'], 'new')} -> {found['state']}" for found in after
                   if seen.get(found["id"]) != found["state"]
                   and not (found["id"] == move.get("id") and found["state"] in ("running", "done"))]
        return dict(release=row, changed=changed)

    def act(self, case, side, row, result):
        """Carry out the action, and read the first run's row right after it returned: a live row, or the state the
        action itself gives it, proves the action landed inside the run. A Deploy or release records what it did in
        `acted`, a kill or cancel whether it landed in `action_evidence`; a cancel proves itself through the run's
        state instead of the row read."""
        action = case["action"]
        if action in ("deploy", "release"):
            acted = self.deploy(case["name"]) if action == "deploy" else self.release(row)
            result["acted"] = dict(case=action, **acted)
            result["row_after_action"] = self.row(row) if row else None
            return
        result["action_evidence"], result["row_after_action"] = self.kill(action, side[case["machine"]], row)

    def kill(self, action, machine, row):
        if action == "cancel":
            # Every later case needs the runner, so it starts again however the cancel ends.
            try:
                self.runner_state("kill")
                return self.cancel(row), None
            finally:
                self.runner_state("start")
        if action == "runner-kill":
            return self.runner_kill(row)
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
            after = self.row(row)
            fields = dict(line.split("=", 1) for line in kill.stdout.splitlines() if "=" in line)
            self.sleep(3)
            self.sh(machine, "systemctl start docker.service ployz.service", check=False)
            return [check_kill(dict(fields, kill=unit, machine=machine))], after
        return [], self.row(row)

    def outside_run(self, case, result):
        """The row read after the action must still be live, or hold the state the action itself gives it: a Deploy
        yields a warm move and a release cancels one, and either follows a committed move to its end, since Cloud
        waits on a lane past its return. Any other terminal row leaves the credited event and the action to a later
        run. A cancel proves itself through Cloud's accepted cancel and the run's end instead, and an unread row proves
        nothing."""
        event = result["triggered"]
        if case["action"] == "cancel":
            return None
        after = result.get("row_after_action")
        if after is None:
            what = "the trigger" if case["action"] == "none" else f"the {case['action']}"
            return (f"the first run's row could not be read after {what}, so nothing places it inside the credited "
                    f"{event['kind']} on {event['machine']}: {event['line']}")
        own = case.get("expect", {}).get("move") if case["action"] in ("deploy", "release") else None
        if after["state"] in LIVE or (after["state"] == own and own in TAKEN):
            return None
        if event["kind"] == "committed" and after["state"] == own and case["action"] == "deploy":
            raise Skipped(f"the Deploy returned after the first run {after['id']} ended {after['state']}, and a followed "
                          "committed lane leaves no row that places the Deploy inside it")
        if event["kind"] == "committed" and after["state"] == own:
            named = refused_run(result["acted"]["release"])
            if named == after["id"]:
                return None
            return (f"the release's refusal names run {named}, not the first run {after['id']}, so nothing places it "
                    f"inside the credited {event['kind']} on {event['machine']}: {event['line']}")
        if case["action"] == "none":
            return (f"the first run {after.get('id')} ended {after.get('state')} before the {event['kind']} on "
                    f"{event['machine']} could be credited: {event['line']}")
        return (f"the {case['action']} landed after the first run {after.get('id')} ended {after.get('state')}, "
                f"outside the credited {event['kind']} on {event['machine']}: {event['line']}")

    def requalify(self, case, side, result):
        """At the case end the credited event must still be the first qualifying line after the barrier cursor. No
        later line stands in for it."""
        event = result["triggered"]
        again = self.trigger(case, side, result["run"])
        if again is None or any(again[key] != event[key] for key in ("cursor", "boot_id", "line")):
            return (f"the credited {event['kind']} on {event['machine']} did not re-qualify at the case end: "
                    f"{json.dumps(again)[:300]}")
        return None

    def prepare(self, case, other):
        if case.get("mirror") == "present" and not self.has_mirror(other):
            self.wait_run(self.start_op("mirror", other))
        if case.get("mirror") == "absent" and self.has_mirror(other):
            self.wait_run(self.start_op("mirror-rm", other))

    def evidence(self, name):
        for machine in self.machines:
            # A clean case runs no barrier, so its journal is this boot's.
            after = f"--after-cursor={shlex.quote(cursor)}" if (cursor := self.cursor.get(machine)) else "-b"
            journal = self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service {after} --no-pager -o short-iso",
                              check=False, timeout=120).stdout
            (self.out / f"{name}-{machine}.journal").write_text(journal)
            listing = self.sh(machine, f"zfs list -t all -H -o name,used,refer,readonly -r {self.pool}",
                              check=False).stdout
            (self.out / f"{name}-{machine}.zfs").write_text(listing)

    def recover(self, case, destination):
        """Recover only an unhealthy end state, and only once every run the case started has ended."""
        states = self.states()
        if not placement(states) and self.healthy(limit=60):
            return [], []
        op = "release" if case["recover"] == "release" else case["op"] or "release"
        row = self.start_op(op, destination)
        reply = json.dumps(row.get("reply"))
        if row["state"] == "refused" and "busy" in reply.lower():
            return [row], [f"recovery {op} was refused as busy: {reply[:300]}"]
        return [self.wait_run(row)], []

    def run_case(self, case, baseline=None):
        self.case_name = case["name"]
        writer = self.writer()
        other = self.other(writer)
        side = dict(writer=writer, other=other)
        destination = other
        self.prepare(case, other)
        started = self.clock()
        known = {row["id"] for row in self.listed_runs(case["name"])}
        # Arm only once the Volume is idle: a cron sync crosses the same SetVolumeCopy points and would trip it.
        plan = arming(case, side)
        arm = (lambda: self.arm(plan)) if plan else None
        late = case.get("arm") == "stopped"
        # A trigger must be the first run's, so it starts behind the clean-start barrier on every Machine.
        row = (self.start_op(case["op"], destination, None if late else arm, clean=bool(case["trigger"]))
               if case["op"] else None)
        if case["trigger"]:
            started = self.since
        result = dict(case, writer_before=writer, destination=destination, started=started)
        if row and row.get("state") == "refused":
            # Busy refusals are skips and approvals are answered, so this is a defect; a rerun would pass it.
            raise RuntimeError(f"{case['op']} was refused: {json.dumps(row.get('reply'))[:1500]}")
        result["run"] = row
        if late and (cut := self.wait_trigger(dict(case, trigger="stopped", trigger_machine="writer"), row, side)):
            result["armed_after"] = cut
            arm()
        if case["trigger"]:
            credited = self.wait_trigger(case, row, side)
            result["triggered"] = credited
            if credited:
                # The action lands first: a read before it would delay it past the event the trigger credits.
                self.act(case, side, row, result)
        elif case["action"] != "none":
            self.act(case, side, row, result)
        if plan and (failed := self.fault(None)):
            raise RuntimeError(f"disarming the fault failed on {', '.join(failed)}")
        result["run"] = self.wait_run(row, limit=TAKEN_LIMIT if case.get("expect", {}).get("move") in TAKEN else 1800)
        if case.get("expect", {}).get("inside_hold") and result.get("triggered"):
            result["released"] = self.released(case, row, result["triggered"])
        result["later"] = self.settle(known | {row["id"]} if row and "id" in row else known)
        if row and "id" in row and "requeues" in case.get("expect", {}):
            result["requeues"] = self.requeued(row)
        if "expect" in case:
            result["recovery"], busy = self.recover(case, destination)
        else:
            # A case with no expectation reruns whatever did not end done, so it passes only on where that left it.
            result["recovery"], busy = [], []
            if result["run"].get("state") != "done":
                op = "release" if case["recover"] == "release" else case["op"]
                result["recovery"].append(self.wait_run(self.start_op(op, destination)))
        self.healthy()
        states = self.states()
        result.update(ended=self.clock(), states=states, final_writer=final_writer(states), runs=(self.runs() or [])[:6],
                      version=self.version())
        lost = self.lost()
        result["probe"] = dict(lost=lost)
        result["failures"] = assess(states, lost) + busy + judge(result, side, baseline)
        if case["trigger"] and not result.get("triggered"):
            result["failures"].append("the trigger never fired before the run ended")
        if result.get("triggered") and (unowned := self.outside_run(case, result) or self.unclaimed(result["run"])):
            result["failures"].append(unowned)
        if result.get("triggered") and result["triggered"]["kind"] not in ("receiving", "committed") and (
                stale := self.requalify(case, side, result)):
            result["failures"].append(stale)
        evidence = result.get("action_evidence")
        if (result.get("triggered") and case["action"] not in ("none", "deploy", "release")
                and not (evidence and all(e["landed"] for e in evidence))):
            result["failures"].append(f"no evidence the {case['action']} landed: {json.dumps(evidence)[:500]}")
        if case["action"] == "cancel" and result.get("triggered") and result["run"].get("state") not in ("cancelled", "lost"):
            result["failures"].append(f"the cancel did not land: the run ended {result['run'].get('state')}")
        if "expect" in case:
            # judge holds the case's own run to its expected state and writer, so only a recovery must end done.
            result["failures"] += [f"the recovery {row.get('id')} ended {row.get('state')}"
                                   for row in result["recovery"] if row.get("state") != "done"]
        else:
            last = (result["recovery"] or [result["run"]])[-1]
            if last.get("state") != "done":
                result["failures"].append(f"the last run ended {last.get('state')}")
            # A move or its rerun ends on the destination; a mirror op never moves the writer, and an undo's Release
            # restores it.
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
        self.evidence(case["name"])
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
        results, escaped, baseline = [], None, None
        try:
            self.setup()
            self.start_probe()
            for case in cases:
                try:
                    result = self.run_case(case, baseline)
                except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
                    result = dict(case, passed=False, failures=[f"{type(error).__name__}: {error}"[:2000]])
                    if isinstance(error, Skipped):
                        result["skipped"] = str(error)
                    try:
                        failed = self.fault(None) if case["fault"] else []
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
                if result.get("expect", {}).get("baseline") and result["passed"]:
                    baseline = result["acted"]["seconds"]
                if not heal["healed"]:
                    break
        except BaseException as error:
            escaped = f"{type(error).__name__}: {error}"[:2000]
            raise
        finally:
            report = cleanup = None
            try:
                report = self.stop_probe()
            except BaseException as error:
                # An error from the cases outranks one from the cleanup, and the one in flight keeps propagating.
                cleanup = None if escaped else error
                escaped = escaped or f"{type(error).__name__}: {error}"[:2000]
            try:
                summary = self.summarize(results, report, escaped, baseline)
            except Interrupted as error:
                # The first signal of the run landed here; the handler is disarmed now, so this write completes.
                summary = self.summarize(results, report, escaped or f"{type(error).__name__}: {error}", baseline)
            if cleanup:
                raise cleanup
        return summary

    def summarize(self, results, report, escaped, baseline=None):
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
            deploy_baseline_seconds=baseline,
            passed=not escaped and all(result["passed"] for result in results) and bool(report)
            and not report.get("lost"),
            error=escaped,
            cases=[dict(name=result["name"], cell=result.get("cell"), step=result.get("step"),
                        passed=result["passed"], skipped=result.get("skipped"),
                        final_writer=result.get("final_writer"),
                        run=(result.get("run") or {}).get("state"),
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
    if manifest["provider"] not in ["incus", "boat"] or manifest["state"] != "ready" or len(manifest["machines"]) != 2 \
            or "cloud" not in manifest:
        raise RuntimeError("Use a ready two-Machine Incus or Boat vc started with --cloud")
    checks = VolumeMove(args.manifest, size_gb=args.size_gb if args.soak else None)
    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupt)
    summary = checks.run_all(cases)
    print(json.dumps(dict(summary=str(checks.out / "summary.json"), passed=summary["passed"],
                          downtime=summary["downtime"])), flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError, OSError, subprocess.TimeoutExpired, Interrupted) as error:
        print(f"volume-move: {error}", file=sys.stderr)
        sys.exit(1)
