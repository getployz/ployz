#!/usr/bin/env python3
"""Exercise the volume move checks with a command model, without Incus, Cloud or ZFS."""

from contextlib import redirect_stdout
from datetime import datetime, timezone
import importlib.util
import io
import json
import re
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("volume_move", Path(__file__).resolve().parents[1] /
                                            "test-verify-volume-move.py")
volume_move = importlib.util.module_from_spec(spec)
spec.loader.exec_module(volume_move)

PHYSICAL = "verify-production_vol-1"
CALL_LIMIT = 6000
DAEMON, PLUGIN = "ployz.service", "ployz-volume-plugin.service"
FAULTED_RUN = 10.0

# The cancel undos the table ran before release replaced them: the cancel and runner checks still hold to them.
CANCEL_UNDOS = {
    "undo-before-cut": dict(name="undo-before-cut", cell="Writer>Writer", step="3-cut", op="move", machine="writer",
                            fault_on="writer", fault="kill-daemon:SetVolumeCopy.WriterToMirror", trigger="fault",
                            action="cancel", recover="release"),
    "undo-after-cut": dict(name="undo-after-cut", cell="Mirror>Writer", step="5-writer", op="move", machine="other",
                           fault_on="other", fault="delay-rpc:SetVolumeCopy.MirrorToWriter:90",
                           trigger="fault-after-stop", action="cancel", recover="release"),
}


def arms(script):
    return "/run/ployz/fault.tmp" in script


def disarms(script):
    return script == volume_move.fault_script(None)


class Runaway(Exception):
    """Not one of the errors run_all catches, so a loop that never sleeps fails the test instead of hanging it."""


def iso(at):
    """Cloud's finished_at, as JavaScript's toISOString writes it."""
    return datetime.fromtimestamp(at, timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def entry(name, wire):
    return f'                {name}: (f, "{name}", Req, "f", Reply, CAP, "{wire}", Container),'


def catalog(names):
    kept = [entry("MetricHistory", "ployz.metrics.history.v1"), entry("InspectStorage", "ployz.storage.inspect.v1"),
            entry("PrepareVolumes", "ployz.storage.prepare.v1")]
    docker = [entry(name, f"ployz.volume.{name.lower()}.v1")
              for name in ("CreateVolume", "ListVolumes", "InspectVolume", "RemoveVolume")]
    moves = [entry(name, f"ployz.volume.{name.lower()}.v1") for name in names]
    return "\n".join(kept + docker + moves) + "\n"


OLD_MOVES = ["InspectVolumeCopy", "SeedVolume", "Withdraw", "Freeze"]


def state(readonly, containers=(), snapshots=0):
    return dict(readonly=readonly, containers=[dict(id=f"c{i}", state=s, name="db") for i, s in enumerate(containers)],
                snapshots=[f"snap{i}" for i in range(snapshots)])


def healthy_states():
    return {"machine-1": state({"root": "off", "fs": "absent"}, ["running"], 2),
            "machine-2": state({"root": "absent", "fs": "on"}, [], 2)}


STEP = 2.0
PLAIN_DEPLOY = 12.0
MOUNT_FAILURE = ("Error: invalid Docker Provisioned Volume observation: Provisioned Volume status has an invalid "
                 "mountpoint\nrollback: Docker 500: error while mounting volume\n")


class ClusterModel:
    """Two Machines and a Cloud whose moves run step by step on the test clock, with optional planted defects.

    A move with a fault armed other than a held case's delay fails at once. Any other move runs in a lane, step by
    step, so a fault armed mid-move still holds a later step. A Deploy takes a warm lane (the move yields and is
    requeued once) and follows a committed one to B; release takes a warm lane back to A and refuses a committed
    one. A delay fault holds a step until the fault clears, which frees the step to run; a kill fault fails the row
    once cleared, and onFailure releases.
    """

    def __init__(self, mutation=None):
        self.mutation = mutation
        self.copies = {"machine-1": "writer", "machine-2": None}
        self.running = {"machine-1": "v0"}
        self.pending = "v0"
        self.current = "v0"
        self.runs = []
        self.lane = None
        self.queued = []
        self.fault = {"machine-1": None, "machine-2": None}
        self.journal = []
        # A verb ployzd holds on a Machine, as its release lines by what ends the hold.
        self.verb_holds = {}
        self.acks = []
        self.asks = set()
        self.started = []
        self.clock = None
        self.holds = []
        self.tripped = None
        self.disarm_fails = False
        self.just_ended = 0
        self.calls = 0
        self.unclaimed = set()
        self.kill_lands = True
        self.pid_file = None
        self.workers = set()
        self.receiving = False
        self.events = []
        # Work that lives inside a daemon or plugin process: a restart of that Machine drops it.
        self.tasks = []
        self.interrupted = []
        self.restarts = []
        self.pulls = set()
        self.unreadable = {}
        # Each Machine's clock runs offset seconds ahead of the host, and one exec takes latency seconds.
        self.offset = {"machine-1": 0.0, "machine-2": 0.0}
        self.latency = {"machine-1": 0.0, "machine-2": 0.0}
        # A clock that slews gains rate seconds per host second from the model's start, on top of its offset.
        self.rate = {"machine-1": 0.0, "machine-2": 0.0}
        # Cloud's clock stamps its rows the same way.
        self.cloud = dict(offset=0.0, rate=0.0)
        # Each journal line keeps the stamp its Machine's clock gave it when it happened.
        self.kills = []
        # A Machine whose db is mid-stop answers pg_isready with "rejecting connections".
        self.stopping = set()
        # The systemd units on each Machine, and the PIDs still running.
        self.ids = 0
        self.alive = set()
        self.units = {machine: {DAEMON: self.process(), PLUGIN: self.process()} for machine in self.offset}
        # Restart plants: the plugin stop exits nonzero and its work runs on, the old plugin process outlives its
        # stop, or the plugin is socket-activated again during the hold.
        self.plugin_stop_fails = set()
        self.survives = set()
        self.reactivated = set()
        # A clean-start read that exits 0 with this reply, and trigger polls that exit nonzero with valid output.
        self.quiet_reply = {}
        self.poll_fails = False
        # journald on each Machine: one boot id, and a cursor is a position in the journal within that boot.
        self.boot = {"machine-1": "boot-1a", "machine-2": "boot-2a"}
        # Journal positions journald vacuumed: a read skips them and every other entry keeps its cursor.
        self.vacuumed = set()
        # The `ployz:pull` target a Machine's slot parent names while its receive runs, when not the first run's own
        # first round; and a Machine whose receive sits in D state and stays in the plugin's cgroup after the stop.
        self.pull_targets = {}
        self.stuck_receive = set()
        # A Machine whose plugin cgroup.procs cannot be read, and a restart reply cut short: "exit 1", "no end" or
        # "no cursor".
        self.unread_procs = set()
        self.restart_reply = {}
        # Machine to a line its journal read emits first that is not a JSON object.
        self.garbage_journal = {}

    def process(self):
        self.ids += 1
        self.alive.add(100 + self.ids)
        return dict(pid=100 + self.ids, invocation=f"inv-{self.ids}", state="active")

    def faulted(self):
        return any(self.fault.values())

    @property
    def writer(self):
        return next((machine for machine, copy in self.copies.items() if copy == "writer"), None)

    @writer.setter
    def writer(self, machine):
        """A test that places the writer by hand also runs the app there."""
        self.place("writer", machine)
        self.running = {machine: self.current} if machine else {}

    @property
    def mirror(self):
        return next((machine for machine, copy in self.copies.items() if copy == "mirror"), None)

    @mirror.setter
    def mirror(self, machine):
        self.place("mirror", machine)

    def place(self, kind, machine):
        for held, copy in self.copies.items():
            if copy == kind:
                self.copies[held] = None
        if machine:
            self.copies[machine] = kind

    @property
    def fired(self):
        return bool(self.journal)

    @fired.setter
    def fired(self, value):
        if value:
            for machine in self.offset:
                self.log_fault(machine)

    def log_fault(self, machine, at=None, text=None, **identity):
        """The daemon on machine logs a fault line at host time at, stamped then by machine's own clock. Without text
        it is the armed fault's own line, naming the Volume and the want it fired inside, or whatever identity names
        instead."""
        def log(at):
            line = self.fault_text(machine, **identity) if text is None else text
            self.journal.append((machine, self.stamp(machine, at), line))
        self.when(at, log)

    def fault_text(self, machine, by=None, **identity):
        """What ployzd logs when the fault armed on machine fires inside the first run's step: the message and verb from the
        arming, then the Volume and the want's identity as Cloud sends it for that step (volume-run.inngest.ts: rounds
        pull w-<epoch>-<n>, the cut and the final pull f-<epoch>, the writer want its epoch, forget nothing)."""
        armed = (self.fault[machine] or "").split(":")
        if armed[0] not in ("kill-daemon", "delay-rpc"):
            return "fault: killing daemon inside the effect"
        verb = armed[1]
        head = (f'fault: releasing switch verb verb="{verb}" by={by}' if by
                else f'fault: killing daemon inside the effect verb="{verb}"' if armed[0] == "kill-daemon"
                else f'fault: delaying switch verb verb="{verb}" secs={armed[2]}')
        epoch = identity.pop("epoch", self.epoch(self.lane["row"]["id"] if self.lane else self.runs[-1]["id"])
                            if self.runs else 1)
        fields = dict(volume=PHYSICAL)
        if verb.endswith("ToMirror"):
            final = verb == "SetVolumeCopy.WriterToMirror" or self.cut_done(epoch)
            fields["target"] = f"f-{epoch}" if final else f"w-{epoch}-0"
        elif verb.endswith("ToSnapshot"):
            fields["snapshot"] = f"w-{epoch}-0"
        elif verb.endswith("ToWriter"):
            fields.update(at=f"guid-{epoch}", epoch=epoch)
        fields["gen"] = self.gen(epoch)
        fields.update(identity)
        return head + "".join(f" {key}={value}" for key, value in fields.items())

    def cut_done(self, epoch):
        """Whether a cut line for this Volume and epoch is in any journal, so a pull now is the final one."""
        return any(f"cut: holders stopped volume={PHYSICAL} target=f-{epoch}" in text for _, _, text in self.journal)

    def cursor(self, machine, index):
        return f"s={machine};i={index};b={self.boot[machine]}"

    def journal_json(self, machine, script):
        """journalctl -o json after a cursor: every later line of this Machine, each with its own cursor and boot."""
        if f"-u {PLUGIN}" not in script:
            return 0, ""
        self.clock.advance(self.latency[machine])
        after = re.search(r"--after-cursor=(\S+)", script)[1].strip("'\"")
        found = re.fullmatch(rf"s={machine};i=(-?\d+);b=(\S+)", after)
        if not found or found[2] != self.boot[machine]:
            return 1, ""
        lines = [dict(__CURSOR=self.cursor(machine, index), _BOOT_ID=self.boot[machine], _SYSTEMD_UNIT=DAEMON,
                      __REALTIME_TIMESTAMP=str(int(at * 1e6)), MESSAGE=text)
                 for index, (where, at, text) in enumerate(self.journal)
                 if where == machine and index > int(found[1]) and index not in self.vacuumed]
        garbage = self.garbage_journal.get(machine)
        return (1 if self.poll_fails else 0), ("" if garbage is None else garbage + "\n") + "".join(
            json.dumps(line) + "\n" for line in lines)

    def tail_cursor(self, machine):
        return f"-- cursor: {self.cursor(machine, len(self.journal) - 1)}\n"

    def proven_restart(self, machine, script):
        """The barrier that stops both units, reads each unit's cgroup.procs, starts the daemon, arms what the script
        arms and ends with the journal's tail cursor when asked: a pid listed is one that outlived its stop."""
        old_plugin = self.units[machine][PLUGIN]["pid"]
        code, out = self.restart(machine, script)
        if code:
            return code, out
        plugin = [str(old_plugin)] if old_plugin in self.alive else []
        if machine in self.stuck_receive:
            plugin.append("4242")
        # systemd drops an emptied control group; one with a leftover stays, as does one `cat` cannot read.
        status = "1" if machine in self.unread_procs else "0"
        lines = [f"procs {DAEMON} absent",
                 f"procs {PLUGIN} {status} " + " ".join(plugin) if plugin or status != "0" else f"procs {PLUGIN} absent"]
        if "--show-cursor" in script and self.restart_reply.get(machine) != "no cursor":
            lines.append(self.tail_cursor(machine).rstrip("\n"))
        if self.restart_reply.get(machine) == "no end":
            return 0, "\n".join(lines) + "\n"
        return (1 if self.restart_reply.get(machine) == "exit 1" else 0), "\n".join(lines + ["end"]) + "\n"

    def arm(self, machine, script):
        """The fault file write: the daemons read it on their next check, so arming or disarming needs no restart."""
        value = (found := re.search(r"printf '%s\\n' '?([^'\s]+)'? >", script)) and found[1]
        if value is None and self.disarm_fails:
            return 255, ""
        held = self.holder() if self.clock else None
        if value and held:
            # The holder crosses the armed point, so the fault fires inside it and not in the case's run.
            self.tripped = held["id"]
            self.fired = True
        self.fault[machine] = value
        if value is None:
            self.disarm(machine)
        else:
            self.recheck(machine)
        return 0, ""

    def pull_probe(self, machine):
        """The receiving read: the receive process, then the slot parent's `ployz:pull` with zfs's exit code."""
        unreadable = self.unreadable.get(machine)
        if unreadable == "exec":
            return 255, ""
        pgrep = 2 if unreadable == "pgrep" else 0 if self.receiving_on(machine) else 1
        lines = [f"pgrep {pgrep}"]
        if unreadable == "pull":
            lines.append("zfs 1")
        else:
            own = f"w-{self.epoch(self.runs[-1]['id'])}-0" if self.runs and pgrep == 0 else "-"
            lines += [self.pull_targets.get(machine, own), "zfs 0"]
        return (1 if self.poll_fails else 0), "\n".join(lines + ["end"]) + "\n"

    def epoch(self, run_id):
        """The epoch Cloud's takeEpoch gives run_id: one above every earlier run on the Volume."""
        return int(run_id.rsplit("-", 1)[1]) + 1

    def gen(self, epoch):
        """The gen Cloud's identity column gave the run that took epoch: apart from the epoch, so a check that reads
        one for the other fails."""
        return epoch + 99

    def cut(self, machine, at, volume=PHYSICAL, target=None, begins=None, finishes=True, gen=None):
        """A cut on machine, for the latest run's target unless another is named. WriterToMirror begins at begins, a
        quarter second before at unless set, and logs the delay fault's line then when one is armed there. The cut
        logs that every holder stopped a tenth of a second after at unless it never gets that far."""
        target = target or f"f-{self.epoch(self.runs[-1]['id'])}"
        gen = gen or self.gen(self.epoch(self.runs[-1]["id"]))
        begins = at - 0.25 if begins is None else begins

        def delayed():
            if (self.fault[machine] or "").startswith("delay-rpc:SetVolumeCopy.WriterToMirror:"):
                self.log_fault(machine, begins, volume=volume, target=target)
        self.at(begins, delayed)
        if finishes:
            self.log_fault(machine, at + 0.1, f" INFO cut: holders stopped volume={volume} target={target} gen={gen}")

    def when(self, at, event):
        at = self.clock() if at is None else at
        if at > self.clock():
            self.at(at, lambda: event(at))
        else:
            event(at)

    def stamp(self, machine, at):
        """What machine's clock reads for host time at, with its offset as it is now."""
        return at + self.offset[machine] + self.rate[machine] * (at - 1000.0)

    def receiving_on(self, machine):
        return self.receiving is True or (isinstance(self.receiving, set) and machine in self.receiving)

    def at(self, moment, event):
        """Something happens in Cloud or on a Machine once the clock reaches moment."""
        self.events.append((moment, event))

    def task(self, machine, moment, name, event):
        """In-process work on machine that does something once the clock reaches moment, unless a restart drops it."""
        self.tasks.append((moment, machine, name, event))

    def restart(self, machine, script):
        """systemd stops the unit's whole cgroup: in-process tasks and zfs children die, the `pull` property stays.
        A plugin whose stop fails keeps its pulls and receives."""
        self.restarts.append((machine, self.clock()))
        units, kept = self.units[machine], machine in self.plugin_stop_fails
        dropped = [task for task in self.tasks if task[1] == machine and not (kept and task[2] == "pull")]
        self.interrupted += [(machine, name) for _, _, name, _ in dropped]
        self.tasks = [task for task in self.tasks if task not in dropped]
        if self.receiving_on(machine) and not kept:
            self.interrupted.append((machine, "zfs receive"))
            self.receiving = (set(self.offset) if self.receiving is True else set(self.receiving)) - {machine}
        self.alive.discard(units[DAEMON]["pid"])
        invocation = units[DAEMON]["invocation"]
        units[DAEMON] = self.process()
        if kept:
            masked = f"{PLUGIN} || true" in script
            return (0, "") if masked else (1, f"failed {PLUGIN}\n")
        survivor = units[PLUGIN]["pid"] if machine in self.survives else 0
        self.alive.discard(units[PLUGIN]["pid"] if not survivor else None)
        units[PLUGIN] = self.process() if machine in self.reactivated else dict(pid=survivor, invocation="", state="inactive")
        return 0, ""

    def tick(self):
        due = [event for moment, event in self.events if self.clock and self.clock() >= moment]
        self.events = [(moment, event) for moment, event in self.events if not (self.clock and self.clock() >= moment)]
        due += [event for moment, _, _, event in self.tasks if self.clock and self.clock() >= moment]
        self.tasks = [task for task in self.tasks if not (self.clock and self.clock() >= task[0])]
        for event in due:
            event()

    def other(self, machine):
        return "machine-2" if machine == "machine-1" else "machine-1"

    def hold(self, seconds, run_id="sync-1", listed=True):
        """Another run holds the Volume for seconds after the previous hold ends, as a cron sync would."""
        start = self.holds[-1]["until"] if self.holds else self.clock()
        self.holds.append(dict(id=run_id, start=start, until=start + seconds, listed=listed))

    def now(self):
        """What Cloud's clock reads now."""
        at = self.clock() if self.clock else 1000.0
        return iso(at + self.cloud["offset"] + self.cloud["rate"] * (at - 1000.0))

    def close(self, row, state, after=0.0):
        """Cloud closes row as state, after seconds from now when given: a faulted run stays open while its daemon
        restarts and the step reruns."""
        def end():
            row["state"], row["finished_at"] = state, self.now()
        if after and self.clock:
            self.at(self.clock() + after, end)
        else:
            end()

    def holder(self):
        return next((hold for hold in self.holds if self.clock() < hold["until"]), None)

    def row(self, kind, state="done", requeue_of=None):
        row = dict(id=f"run-{len(self.runs)}", kind=kind, state=state, phase="warm",
                   finished_at=None if state == "running" else self.now(), requeue_of=requeue_of)
        self.runs.append(row)
        return row

    def end(self, row, state):
        row["state"], row["finished_at"] = state, self.now()

    def __call__(self, args, **kwargs):
        self.calls += 1
        if self.calls > CALL_LIMIT:
            raise Runaway(f"More than {CALL_LIMIT} helper commands; a wait loop is not sleeping")
        self.tick()
        if self.clock:
            self.advance()
        if args[0] == "docker":
            code, output = self.postgres(args[-1])
        elif args[1] == "cli":
            code, output = self.cli(args[args.index("--") + 2:])
        elif args[1] == "exec":
            code, output = self.guest(args[args.index("--") - 1], args[-1])
        elif args[1] == "runner":
            code, output = self.worker(args[2])
        else:
            raise AssertionError(f"Unexpected helper command: {args}")
        return subprocess.CompletedProcess(args, code, output, "")

    def postgres(self, sql):
        """Cloud's rows: the gen set at insert and the epoch the workflow takes, the phase and state, the Inngest run
        that claims a request (an unclaimed one has no epoch and no run), and the moves a Deploy requeued.

        A cancel looks up the lane's Inngest run while the runner is down, so the model ends the lane there."""
        run_id = re.search(r"id = '([^']+)'", sql)[1]
        row = next((row for row in self.runs if row["id"] == run_id), None)
        if sql.startswith("select gen, coalesce(epoch, -1) "):
            epoch = self.epoch(run_id)
            return 0, f"{self.gen(epoch)}|{-1 if run_id in self.unclaimed else epoch}\n"
        if sql.startswith("select phase, state "):
            return 0, f"{row['phase']}|{row['state']}\n" if row else ""
        if sql.startswith("select again.id, again.state "):
            # Postgres answers what the query asks: without the Deploy that took run, any later move matches.
            taken = "took.args -> 'resume' = first.args" in sql
            later = self.runs[self.runs.index(row) + 1:] if row else []
            return 0, "".join(f"{again['id']}|{again['state']}\n" for again in later if again["kind"] == "move"
                              and (again.get("requeue_of") == run_id if taken else True))
        if (self.lane and self.lane["row"]["id"] == run_id and self.pid_file.exists()
                and int(self.pid_file.read_text()) not in self.workers):
            self.take("cancelled")
            self.running = {self.writer: self.current}
        return 0, "" if run_id in self.unclaimed else f"inngest-{run_id}\n"

    def worker(self, state):
        """verify-cluster runner kills the recorded worker if it is alive, and start replaces a dead one."""
        pid = int(self.pid_file.read_text())
        if state == "kill":
            self.kills.append(self.clock())
        if state == "kill" and self.kill_lands:
            self.workers.discard(pid)
        if state == "start" and pid not in self.workers:
            self.workers.add(pid + 1)
            self.pid_file.write_text(f"{pid + 1}\n")
        return 0, ""

    def start_move(self, row, destination):
        source = self.writer
        if source == destination:
            self.end(row, "done")
            return
        first = "MirrorToMirror" if self.copies[destination] == "mirror" else "NoneToMirror"
        stages = [("snapshot", source, "WriterToSnapshot"), ("round", destination, first),
                  ("round", destination, "MirrorToMirror"), ("cut", source, "WriterToMirror"),
                  ("final", destination, "MirrorToMirror"), ("start", destination, "MirrorToWriter")]
        self.lane = dict(row=row, source=source, destination=destination, stages=stages, index=-1,
                         until=self.clock(), held=None, killed=None)

    def advance(self):
        while self.lane is None and self.queued:
            self.start_move(*self.queued.pop(0))
        while self.lane and not self.lane["killed"] and self.clock() >= self.lane["until"]:
            lane = self.lane
            if lane["index"] >= 0:
                self.finish(lane)
            lane["index"] += 1
            if lane["index"] == len(lane["stages"]):
                self.end(lane["row"], "done")
                self.lane = None
                while self.lane is None and self.queued:
                    self.start_move(*self.queued.pop(0))
            else:
                self.enter(lane)

    def enter(self, lane):
        name, machine, transition = lane["stages"][lane["index"]]
        at, fault = lane["until"], self.fault[machine] or ""
        if name == "start":
            lane["row"]["phase"] = "committed"
        delay = re.fullmatch(r"delay-rpc:SetVolumeCopy\.(\w+):(\d+)", fault)
        lane["held"] = None
        if delay and delay[1] == transition:
            self.log_fault(machine, at)
            self.hold_verb(machine, at + int(delay[2]))
            lane["until"], lane["held"] = at + int(delay[2]) + STEP, machine
        elif fault and not fault.startswith("delay-rpc:") and (
                fault == f"kill-daemon:SetVolumeCopy.{transition}" or not fault.startswith("kill-daemon:")):
            self.log_fault(machine, at)
            lane["killed"] = machine
        else:
            lane["until"] = at + STEP

    def finish(self, lane):
        name, machine, _ = lane["stages"][lane["index"]]
        if name == "round":
            self.copies[machine] = "mirror"
        elif name == "cut":
            self.running.pop(machine, None)
            epoch = self.epoch(lane["row"]["id"])
            gen = 999 if self.mutation == "cut-other-gen" else self.gen(epoch)
            self.log_fault(machine, lane["until"],
                           f" INFO cut: holders stopped volume={PHYSICAL} target=f-{epoch} gen={gen}")
            self.copies[machine] = "mirror"
        elif name == "start":
            self.copies[machine] = "writer"
            self.running[machine] = self.current

    def take(self, state):
        """End the lane's row as `state` in one step and put the app back on the source, promoting it if cut."""
        lane, self.lane = self.lane, None
        self.clock.now = self.clock() + STEP
        if not (state == "yielded" and self.mutation == "yielded-running"):
            self.end(lane["row"], state)
        self.copies[lane["source"]] = "writer"
        if self.copies[lane["destination"]] == "writer":
            self.copies[lane["destination"]] = "mirror"
        return lane

    def recheck(self, machine):
        """A pull's next check, a second on, resends its request, so a delay armed mid-pull holds that check."""
        lane = self.lane
        if lane is None or lane["index"] < 0 or lane["held"]:
            return
        name, at_machine, transition = lane["stages"][lane["index"]]
        if (name in ("round", "final") and at_machine == machine
                and (self.fault[machine] or "").startswith(f"delay-rpc:SetVolumeCopy.{transition}:")):
            lane["until"] = self.clock() + 1
            self.enter(lane)

    def hold_verb(self, machine, until):
        """ployzd holds the verb until the fault file clears or the hold's limit passes, whatever Cloud does with the
        run meanwhile, and logs which ended it with the held verb's identity."""
        hold = {by: self.fault_text(machine, by=by) for by in ("disarm", "limit")}
        self.verb_holds[machine] = hold
        self.at(until, lambda: self.release_verb(machine, "limit", hold))

    def release_verb(self, machine, by, hold=None):
        if machine in self.verb_holds and (hold is None or self.verb_holds[machine] is hold):
            self.log_fault(machine, None, self.verb_holds.pop(machine)[by])

    def disarm(self, machine):
        """Clearing a fault frees a held step, which then runs; a killed row fails."""
        self.release_verb(machine, "disarm")
        lane = self.lane
        if lane and lane["held"] == machine:
            lane["until"], lane["held"] = min(lane["until"], self.clock() + STEP), None
        if lane and lane["killed"] == machine:
            committed = lane["row"]["phase"] == "committed"
            self.lane = None
            self.end(lane["row"], "failed")
            side = lane["destination"] if committed else lane["source"]
            self.copies[side], self.copies[self.other(side)] = "writer", "mirror"
            self.running = {side: self.current}
            self.row("release")

    def deploy(self):
        lane, version = self.lane, self.pending
        if lane and self.mutation == "mount-error":
            self.running.clear()
            return 1, MOUNT_FAILURE
        if lane and lane["row"]["phase"] == "committed":
            self.clock.now = max(self.clock(), lane["until"])
            self.advance()
        elif lane:
            if self.mutation == "deploy-waits":
                self.clock.now = max(self.clock(), lane["until"])
            lane = self.take("yielded")
            requeues = {"requeue-none": 0, "requeue-twice": 2}.get(self.mutation, 1)
            # An unrelated move, say one a user asked for, lands where the requeue would.
            of = None if self.mutation == "requeue-unrelated" else lane["row"]["id"]
            self.queued += [(self.row("move", "running", requeue_of=of), lane["destination"])
                            for _ in range(requeues)]
        if lane or version != self.current:
            # A Deploy with nothing new to roll out, like the setup's, returns at once.
            self.clock.now += PLAIN_DEPLOY
        self.current = version
        self.running = {self.writer: version}
        if lane and self.mutation == "deploy-two-machines":
            self.running[lane["destination"]] = version
        return 0, json.dumps(dict(deployed=version))

    def release(self):
        lane = self.lane
        if lane and lane["row"]["phase"] == "committed":
            if self.mutation == "release-changes-row":
                self.row("release", "failed")
            message = f"{volume_move.VOLUME} is starting on {lane['destination']}; it finishes there"
            return 1, json.dumps(dict(error=dict(code="conflict", message=message)))
        if lane:
            self.take("cancelled")
            self.running = {self.writer: self.current}
        return 0, json.dumps(self.row("release"))

    def cli(self, args):
        if args[0] == "volume" and args[1] in self.asks and "--approval" not in args:
            details = dict(approval=f"{args[1]}:digest", approval_id="approval-1")
            return 1, json.dumps(dict(error=dict(code="approval_required", details=details)))
        if args[:2] != ["volume", "runs"]:
            self.started.append(args)
        if args[0] == "set":
            self.pending = next((arg.partition("=")[2] for arg in args if ".env.VC_DEPLOY=" in arg), self.pending)
            return 0, "{}"
        if args[0] == "deploy":
            return self.deploy()
        if args[:2] == ["volume", "inspect"]:
            return 0, json.dumps(dict(id="1", environment=dict(project="verify", name="production")))
        held = self.holder() if self.clock else None
        if args[:2] == ["volume", "runs"]:
            # Cloud lists ended runs too, newest first.
            holds = [dict(id=hold["id"], kind="sync", state="running" if hold is held else "done")
                     for hold in reversed(self.holds) if hold["listed"] and hold["start"] <= self.clock()]
            return 0, json.dumps(dict(runs=holds + list(reversed(self.runs))))
        if held and args[:2] in (["volume", "move"], ["volume", "mirror"], ["volume", "release"]):
            run = dict(id=held["id"], kind="sync", state="running")
            message = f"VolumeBusy: run {held['id']} (sync) is running"
            return 1, json.dumps(dict(error=dict(code="conflict", message=message, details=dict(run=run))))
        if self.just_ended and args[:2] in (["volume", "move"], ["volume", "mirror"], ["volume", "release"]):
            self.just_ended -= 1
            message = "A run on this Volume just ended. Try again."
            return 1, json.dumps(dict(error=dict(code="conflict", message=message, details=None)))
        if args[:2] == ["volume", "move"]:
            row = self.row("move", "running")
            held = any((fault or "").startswith("delay-rpc:") and fault.endswith(f":{volume_move.HOLD}")
                       for fault in self.fault.values())
            if self.faulted() and not held and not self.lane:
                self.fired = True
                self.close(row, "failed", FAULTED_RUN)
            else:
                self.queued.append((row, args[-1]))
                self.advance()
            return 0, json.dumps(row)
        if args[:2] == ["volume", "mirror"]:
            row = self.row("mirror", "running")
            # The armed kill lands inside the effect and the resumed run finishes it.
            faulted = self.faulted()
            if faulted:
                self.fired = True
            self.mirror = None if args[2] == "rm" else args[-1]
            self.close(row, "done", FAULTED_RUN if faulted else 0.0)
            return 0, json.dumps(row)
        if args[:2] == ["volume", "release"]:
            return self.release()
        return 0, "{}"

    def guest(self, machine, script):
        if "cgroup.procs" in script:
            return self.proven_restart(machine, script)
        if "--show-cursor" in script:
            return 0, self.tail_cursor(machine)
        if "--after-cursor" in script:
            return self.journal_json(machine, script)
        if "zfs get" in script and "ployz:pull" in script:
            return self.pull_probe(machine)
        if "ployz:pull" in script:
            if machine in self.quiet_reply:
                return 0, self.quiet_reply[machine]
            return self.quiet(machine)
        if "/run/ployz/fault" in script:
            return self.arm(machine, script)
        polled = 1 if self.poll_fails else 0
        if "Config.Env" in script:
            return 0, f"VC_DEPLOY={self.running[machine]}\n" if machine in self.running else ""
        if script.startswith("for d in"):
            lines = []
            two_writers = self.mutation == "two-writers"
            copy = self.copies[machine]
            root = "off" if copy == "writer" or (two_writers and copy == "mirror") else "absent"
            fs = "on" if copy == "mirror" and not two_writers else "absent"
            lines += [f"ro ployz/ployz/{PHYSICAL} {root}", f"ro ployz/ployz-mirror/{PHYSICAL}/fs {fs}"]
            snapshots = 12 if self.mutation == "snapshots" else 2
            lines += [f"snap ployz/ployz/{PHYSICAL}@s{i}" for i in range(snapshots)]
            if machine in self.running and self.mutation != "elsewhere":
                lines.append("ct abc running db-1")
            elif machine not in self.running and self.mutation in ("elsewhere", "two-running"):
                lines.append("ct xyz running db-2")
            elif self.mutation == "stopped" and copy == "mirror":
                lines.append("ct def exited db-old")
            return 0, "\n".join(lines) + "\n"
        if "pgrep" in script:
            if self.unreadable.get(machine) == "exec":
                return 255, ""
            code = 2 if self.unreadable.get(machine) == "pgrep" else 0 if self.receiving_on(machine) else 1
            if "echo yes" in script:
                return polled, "yes\n" if code == 0 else "no\n"
            return polled, f"pgrep {code}\n"
        if "pg_isready" in script:
            if machine in self.stopping or machine not in self.running:
                return 1, "/var/run/postgresql:5432 - rejecting connections\n"
            return 0, "/var/run/postgresql:5432 - accepting connections\n"
        if "systemctl kill" in script:
            self.clock.advance(self.latency[machine])
            self.kills.append(self.clock())
            after, invocation = ("2", "b") if self.kill_lands else ("1", "a")
            return 0, f"rc=0\nout=\nbefore=1\ninvocation=a\nafter={after}\ninvocation_after={invocation}\n"
        if "SELECT id FROM ackprobe" in script:
            present = self.acks[:-1] if self.mutation == "lost" else self.acks
            return 0, "\n".join(str(ack) for ack in present) + "\n"
        return 0, ""

    def quiet(self, machine):
        """The clean start's read: every dataset's `ployz:pull` and every zfs receive, each with its exit code."""
        unreadable = self.unreadable.get(machine)
        if unreadable == "exec":
            return 255, ""
        zfs = 1 if unreadable == "pull" else 0
        pgrep = 2 if unreadable == "pgrep" else 0 if self.receiving_on(machine) else 1
        lines = [f"zfs {zfs}", f"pgrep {pgrep}"]
        if not zfs:
            pull = "move-1" if machine in self.pulls else "-"
            lines += ["pull ployz\t-", f"pull ployz/ployz-mirror/{PHYSICAL}\t{pull}",
                      f"pull ployz/ployz-mirror/{PHYSICAL}/fs\t-"]
        if not pgrep:
            lines.append(f"recv 4242 /usr/sbin/zfs receive -u -s ployz/ployz-mirror/{PHYSICAL}/fs")
        return 0, "\n".join(lines + ["end"]) + "\n"


class FakeProbe:
    def __init__(self, log, model, clock):
        self.log, self.model, self.clock = log, model, clock
        self.stdout = None

    def poll(self):
        return None

    def ack(self):
        ack = len(self.model.acks) + 1
        self.model.acks.append(ack)
        with self.log.open("a") as handle:
            handle.write(json.dumps(dict(at=self.clock(), t=self.clock(), ack=ack)) + "\n")

    def send_signal(self, _):
        pass

    def wait(self, timeout):
        lost = [ack for ack in self.model.acks if self.model.mutation == "lost"][-1:]
        self.stdout.write(json.dumps(dict(acked=len(self.model.acks), lost=lost, longest_gap_seconds=1.0)))
        return 0


class Clock:
    """Time as the fake cluster shares it: advance is time passing for everyone. The host reads it through wall(),
    its realtime clock, which a step moves and a slew stretches."""

    def __init__(self):
        self.now = 1000.0
        self.probe = None
        self.offset = 0.0
        self.rate = 0.0

    def __call__(self):
        return self.now

    def wall(self):
        return self.now + self.offset + self.rate * (self.now - 1000.0)

    def advance(self, seconds):
        self.now += seconds

    def step(self, delta):
        self.offset += delta

    def sleep(self, seconds):
        self.advance(seconds)
        if self.probe:
            self.probe.ack()


class VolumeMoveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "evidence").mkdir()
        self.manifest = self.root / "manifest.json"
        self.manifest.write_text(json.dumps(dict(
            provider="incus", state="ready", cloud=dict(dashboard=str(self.root / "dashboard"), url="http://cloud"),
            machines=[dict(name="machine-1"), dict(name="machine-2")])))
        run = self.root / "dashboard" / ".verify" / "run"
        run.mkdir(parents=True)
        (run / "env").write_text("INNGEST_BASE_URL=http://inngest\n")

    def tearDown(self):
        self.temp.cleanup()

    def checks(self, mutation=None):
        model = ClusterModel(mutation)
        clock = Clock()
        model.clock = clock
        model.pid_file = self.root / "dashboard" / ".verify" / "run" / "worker.pid"
        model.pid_file.write_text("100\n")
        model.workers = {100}
        patcher = mock.patch.object(volume_move, "alive", lambda pid: pid in model.workers, create=True)
        patcher.start()
        self.addCleanup(patcher.stop)

        def spawn(command, stdout, stderr):
            logs = len(list((self.root / "evidence").iterdir()))
            probe = FakeProbe(self.root / "evidence" / f"ackprobe-db-{logs}.jsonl", model, clock.wall)
            probe.stdout = stdout
            probe.ack()
            clock.probe = probe
            return probe
        checks = volume_move.VolumeMove(self.manifest, runner=model, spawn=spawn, sleep=clock.sleep,
                                        clock=clock.wall, out=self.root / "out")
        return checks, model

    def run_cases(self, cases, mutation=None):
        checks, model = self.checks(mutation)
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all(cases)
        return summary, model

    def test_catalog_with_old_verbs_names_every_extra_verb_and_the_missing_one(self):
        diff = volume_move.catalog_diff(catalog(OLD_MOVES))
        self.assertEqual(diff["extra"], ["Freeze", "Withdraw"])
        self.assertEqual(diff["missing"], ["SetVolumeCopy"])
        self.assertEqual(diff["kept_missing"], [])

    def test_catalog_passes_only_on_the_three_verbs_with_the_docker_verbs_kept(self):
        diff = volume_move.catalog_diff(catalog(["InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"]))
        self.assertEqual((diff["extra"], diff["missing"], diff["kept_missing"]), ([], [], []))
        dropped = catalog(["InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"]).replace("MetricHistory:", "Gone:")
        self.assertEqual(volume_move.catalog_diff(dropped)["kept_missing"], ["MetricHistory"])

    def test_catalog_passes_without_the_docker_verbs_rpc_simplify_removes(self):
        def without(text, *names):
            return "".join(line for line in text.splitlines(keepends=True)
                           if not any(f"{name}:" in line for name in names))

        text = without(catalog(["InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"]), "CreateVolume", "InspectVolume")
        diff = volume_move.catalog_diff(text)
        self.assertEqual((diff["extra"], diff["missing"], diff["kept_missing"]), ([], [], []))
        self.assertEqual(volume_move.catalog_diff(without(text, "ListVolumes"))["kept_missing"], ["ListVolumes"])

    def test_catalog_parses_the_real_file(self):
        diff = volume_move.catalog_diff(volume_move.CATALOG.read_text())
        self.assertIn("InspectVolumeCopy", diff["move_verbs"])
        self.assertEqual(diff["kept_missing"], [])

    def test_lease_scan_finds_each_planted_name(self):
        planted = [
            ("core/crates/a.rs", "struct Lease" + "Record {}"),
            ("core/crates/b.rs", "let p = P" + "os { seq: 1 };"),
            ("core/crates/ployzd/src/volume_plugin/c.rs", "lease.rec" + "ord(step)?;"),
            ("dashboard/src/d.ts", "kill_after" + "_record"),
            ("core/crates/ployzd/src/volume_plugin/le" + "ase.rs", ""),
            ("core/crates/e.rs", "mod le" + "ase;"),
        ]
        hits = volume_move.lease_hits(planted)
        self.assertEqual({hit["path"] for hit in hits}, {path for path, _ in planted})

    def test_lease_scan_ignores_near_misses_and_itself(self):
        near = [("core/crates/a.rs", "metrics.record();\nlet pos = 1;\nPosition::new()\nrecorder(x)")]
        self.assertEqual(volume_move.lease_hits(near), [])
        self.assertEqual(volume_move.lease_hits([("core/crates/ployz-store/x.rs", "store.rec" + "ord(&id)")]), [])
        own = [(str(path), path.read_text()) for path in (Path(volume_move.__file__), Path(__file__))]
        self.assertEqual(volume_move.lease_hits(own), [])

    def test_assess_passes_one_writer_and_catches_each_defect(self):
        self.assertEqual(volume_move.assess(healthy_states(), []), [])
        two = healthy_states()
        two["machine-2"]["readonly"]["fs"] = "off"
        stopped = healthy_states()
        stopped["machine-2"]["containers"].append(dict(id="x", state="exited", name="db"))
        elsewhere = healthy_states()
        elsewhere["machine-1"]["containers"] = []
        elsewhere["machine-2"]["containers"] = [dict(id="x", state="running", name="db")]
        snapshots = healthy_states()
        snapshots["machine-1"]["snapshots"] = [str(i) for i in range(volume_move.SNAPSHOT_BOUND + 1)]
        for name, states, lost in [("two", two, []), ("stopped", stopped, []), ("elsewhere", elsewhere, []),
                                   ("snapshots", snapshots, []), ("lost", healthy_states(), [7]),
                                   ("unread", healthy_states(), None)]:
            with self.subTest(name):
                self.assertNotEqual(volume_move.assess(states, lost), [])

    def test_final_writer_names_the_single_writable_copy(self):
        self.assertEqual(volume_move.final_writer(healthy_states()), "machine-1")
        two = healthy_states()
        two["machine-2"]["readonly"]["fs"] = "off"
        self.assertIsNone(volume_move.final_writer(two))

    def test_window_gap_belongs_to_the_case_where_the_gap_began(self):
        acks = [(1, 0.0), (2, 1.0), (3, 30.0), (4, 31.0), (5, 100.0)]
        self.assertEqual(volume_move.window_gap(acks, 0.5, 31.0), (29.0, [2, 3]))
        self.assertEqual(volume_move.window_gap(acks, 30.5, 32.0), (69.0, [4, 5]))
        self.assertEqual(volume_move.window_gap(acks, 31.5, 120.0), (None, None))
        self.assertEqual(volume_move.window_gap([], 0, 1), (None, None))

    def test_downtime_reports_median_and_max_over_moves(self):
        results = [dict(op="move", probe=dict(longest_gap_seconds=gap)) for gap in (1.0, 9.0, 3.0)]
        results.append(dict(op="mirror", probe=dict(longest_gap_seconds=99.0)))
        self.assertEqual(volume_move.downtime(results), dict(moves=3, p50=3.0, max=9.0))

    CELLS = ["Writer>Mirror", "Writer>Snapshot", "Writer>Forget", "Mirror>Writer", "Mirror>Mirror", "Mirror>Forget",
             "None>Mirror"]

    def test_kill_table_covers_every_cell_once_with_its_own_fault_point(self):
        self.assertEqual(sorted(row["cell"] for row in volume_move.KILLS), sorted(self.CELLS))
        for row in volume_move.KILLS:
            with self.subTest(row["name"]):
                self.assertEqual(row["fault"], "kill-daemon:SetVolumeCopy." + row["cell"].replace(">", "To"))

    def test_rows_use_known_columns_and_no_retired_fault(self):
        for row in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(row["name"]):
                self.assertIn(row["op"], {None, "move", "mirror", "mirror-rm"})
                self.assertIn(row["machine"], {"writer", "other"})
                self.assertIn(row.get("fault_on"), {None, "writer", "other"})
                self.assertIn(row.get("mirror"), {None, "present", "absent"})
                self.assertIn(row["action"], {"none", "deploy", "release", "runner-kill", "kill-ployz", "kill-docker"})
                self.assertIn(row["recover"], {"rerun", "release"})
                self.assertIn(row["trigger"], {None, "fault", "receiving", "stopped", "fault-after-stop",
                                               "committed"})
                self.assertIn(row.get("arm"), {None, "stopped"})
                self.assertTrue(row["step"])
                self.assertNotIn("after-record", row["fault"] or "")
        self.assertEqual(len({row["name"] for row in volume_move.CASES}), len(volume_move.CASES))

    def test_delete_mirror_and_first_mirror_cells_run_their_own_op(self):
        ops = {row["cell"]: (row["op"], row.get("mirror")) for row in volume_move.KILLS}
        self.assertEqual(ops["Writer>Forget"], ("mirror-rm", "present"))
        self.assertEqual(ops["Mirror>Forget"], ("mirror-rm", "present"))
        self.assertEqual(ops["None>Mirror"], ("mirror", "absent"))
        self.assertEqual(ops["Mirror>Mirror"], ("move", "present"))

    def test_held_cases_hold_the_lane_phase_their_action_needs_on_the_destination(self):
        cases = {row["name"]: row for row in volume_move.CASES}
        expected = {"deploy-warm": ("MirrorToMirror", "fault"), "undo-before-cut": ("MirrorToMirror", "fault"),
                    "deploy-after-cut": ("MirrorToMirror", "fault-after-stop"),
                    "deploy-committed": ("MirrorToWriter", "committed"), "undo-after-cut": ("MirrorToWriter", "committed")}
        for name, (transition, trigger) in expected.items():
            with self.subTest(name):
                row = cases[name]
                self.assertEqual((row["fault_on"], row["fault"], row["trigger"]),
                                 ("other", f"delay-rpc:SetVolumeCopy.{transition}:{row['hold']}", trigger))

    def test_cases_keep_the_clean_move_the_deploys_both_undos_and_one_kill_per_cell(self):
        self.assertEqual([row["name"] for row in volume_move.CASES],
                         ["clean", "deploy-plain", "deploy-warm", "deploy-after-cut", "deploy-committed",
                          "undo-before-cut", "undo-after-cut", *(row["name"] for row in volume_move.KILLS)])

    def test_soak_runs_the_four_switch_steps_under_every_action(self):
        self.assertEqual([step[0] for step in volume_move.SOAK_STEPS], ["1-rounds", "3-cut", "4-final", "5-writer"])
        self.assertEqual(len(volume_move.soak_cases()), 3 + 4 * 5)

    def test_every_fault_names_a_point_the_daemon_defines(self):
        source = (volume_move.CORE / "crates/ployzd/src/machine_api/local_volume_copy.rs").read_text()
        points = set(re.findall(r'"SetVolumeCopy\.(\w+)"', source))
        # No case kills the Writer raise or a restore seed's forget.
        self.assertEqual(points, {cell.replace(">", "To") for cell in self.CELLS} | {"WriterToWriter", "SeedToForget"})
        point = "SetVolumeCopy\\.(" + "|".join(sorted(points)) + ")"
        for row in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(row["name"]):
                if row["fault"] is not None:
                    self.assertRegex(row["fault"], rf"^(kill-daemon:{point}|delay-rpc:{point}:\d+)$")

    def test_case_selection_keeps_named_cases_in_table_order_and_refuses_unknown_names(self):
        names = [row["name"] for row in volume_move.CASES]
        self.assertEqual(volume_move.select_cases(volume_move.CASES, []), volume_move.CASES)
        picked = volume_move.select_cases(volume_move.CASES, ["kill-cut", "clean"])
        self.assertEqual([row["name"] for row in picked], [name for name in names if name in {"clean", "kill-cut"}])
        with self.assertRaises(RuntimeError):
            volume_move.select_cases(volume_move.CASES, ["clean", "kill-nothing"])

    def test_busy_names_the_run_a_volume_busy_refusal_holds_and_nothing_else(self):
        run = dict(id="run-9", kind="sync", state="running")
        refusal = dict(error=dict(code="conflict", message="VolumeBusy: run run-9 (sync) is running; volume runs "
                                  "run-9 shows it", details=dict(run=run)))
        self.assertEqual(volume_move.busy(refusal), run)
        ended = dict(error=dict(code="conflict", message="A run on this Volume just ended. Try again.", details=None))
        self.assertEqual(volume_move.busy(ended), dict(id="ended"))
        for name, reply in [("other conflict", dict(error=dict(code="conflict", message="Volume data is not "
                                                                "attached.", details=None))),
                            ("approval", dict(error=dict(code="approval_required", details=dict(approval="x")))),
                            ("success", dict(id="run-1", kind="move", state="requested")),
                            ("raw", dict(raw="VolumeBusy")), ("none", None)]:
            with self.subTest(name):
                self.assertIsNone(volume_move.busy(reply))

    def test_kill_passes_on_rc_0_or_an_auxiliary_refusal_after_the_main_process_died(self):
        moved = dict(before="41", after="77", invocation="a", invocation_after="b")
        stayed = dict(before="41", after="41", invocation="a", invocation_after="a")
        auxiliary = f"{volume_move.AUXILIARY}: Input/output error"
        self.assertTrue(volume_move.check_kill(dict(moved, rc="0", out=""))["landed"])
        self.assertTrue(volume_move.check_kill(dict(moved, rc="1", out=auxiliary))["landed"])
        self.assertTrue(volume_move.check_kill(dict(stayed, invocation_after="b", rc="1", out=auxiliary))["landed"])
        for name, fields in [("no change", dict(stayed, rc="1", out=auxiliary)),
                             ("other text", dict(moved, rc="1", out="Failed to kill unit: Access denied")),
                             ("rc 5", dict(moved, rc="5", out=auxiliary))]:
            with self.subTest(name), self.assertRaises(RuntimeError):
                volume_move.check_kill(fields)

    def test_clean_move_passes_and_records_the_new_writer(self):
        summary, _ = self.run_cases([volume_move.CASES[0]])
        self.assertTrue(summary["passed"], summary)
        self.assertEqual(summary["cases"][0]["final_writer"], "machine-2")
        self.assertTrue((self.root / "out" / "clean.json").exists())
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_case_code_does_not_read_the_fault_string(self):
        row = dict(volume_move.KILLS[0], name="rekeyed", fault="kill-daemon:SetVolumeCopy.AnyCellAtAll")
        summary, model = self.run_cases([row])
        self.assertTrue(summary["passed"], summary)
        self.assertTrue(model.fired)
        self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})

    def test_a_release_that_asks_a_human_is_approved_and_retried_but_a_move_is_not(self):
        checks, model = self.checks()
        model.asks = {"release", "move"}
        (self.root / "private").mkdir()
        (self.root / "private" / "cloud-token").write_text("token\n")
        posts = []

        def http(method, url, body=None, token=None):
            posts.append((method, url, body, token))
            return 200, "{}"
        with mock.patch.object(volume_move, "http", http), redirect_stdout(io.StringIO()):
            released = checks.start_op("release", "machine-2")
            moved = checks.start_op("move", "machine-2")
        self.assertEqual(released["state"], "done")
        self.assertEqual(released["approved"], "approval-1")
        self.assertEqual(posts, [("POST", "http://cloud/api/cli/approvals/approval-1",
                                  {"approve": {"digest": "release:digest"}}, "token")])
        self.assertEqual(model.started, [["volume", "release", volume_move.VOLUME, "--approval", "approval-1"]])
        self.assertEqual(moved["state"], "refused")

    def test_a_volume_held_past_the_limit_skips_the_case_disarms_its_fault_and_the_next_case_runs(self):
        # An unlisted holder looks idle, so the fault is armed and the first refusal skips the case at once.
        for listed, seconds, reason in ((True, volume_move.BUSY_LIMIT + 20, "before the fault was armed"),
                                        (False, 20, "while the fault was armed")):
            with self.subTest(holder_listed=listed):
                checks, model = self.checks()
                model.hold(seconds, listed=listed)
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
                skipped, clean = summary["cases"]
                self.assertIn(reason, skipped["skipped"] or "", skipped)
                self.assertFalse(skipped["passed"])
                self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})
                self.assertTrue(clean["passed"], clean)
                self.assertEqual(clean["final_writer"], "machine-2")
                self.assertEqual([(wait["case"], wait["holder"]) for wait in summary["volume_busy_waits"]],
                                 [(skipped["name"], "sync-1"), (clean["name"], "sync-1")])

    def test_a_kill_case_arms_its_fault_only_after_the_holder_ends(self):
        checks, model = self.checks()
        model.hold(30)
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0]])
        self.assertIsNone(model.tripped)
        self.assertTrue(summary["passed"], summary)
        self.assertEqual([wait["holder"] for wait in summary["volume_busy_waits"]], ["sync-1"])

    def test_a_holder_that_outlasts_the_heal_skips_its_ops_and_stops_the_run(self):
        checks, model = self.checks("two-writers")
        model.mirror = "machine-2"
        model.hold(volume_move.BUSY_LIMIT * 3 + 60)
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        skipped = json.loads((self.root / "out" / f"{volume_move.KILLS[0]['name']}.json").read_text())
        self.assertTrue(skipped["skipped"], skipped)
        self.assertEqual([attempt["op"] for attempt in skipped["heal"]["attempts"]], ["move", "release"])
        self.assertTrue(all(attempt.get("skipped") for attempt in skipped["heal"]["attempts"]), skipped["heal"])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.KILLS[0]["name"]])
        self.assertIn("the run stopped after this case: after the heal, ", skipped["failures"][-1])
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_failed_case_the_heal_cannot_fix_stops_the_run_and_keeps_its_own_failures(self):
        checks, model = self.checks("two-writers")
        cli = model.cli
        def held_after_the_move(args):
            reply = cli(args)
            if args[:2] == ["volume", "move"] and not model.holds:
                model.hold(volume_move.BUSY_LIMIT * 3 + 60)
            return reply
        model.cli = held_after_the_move
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.CASES[0], volume_move.KILLS[0]])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.CASES[0]["name"]])
        failures = summary["cases"][0]["failures"]
        self.assertGreater(len(failures), 1, failures)
        self.assertIn("the run stopped after this case: after the heal, ", failures[-1])
        case = json.loads((self.root / "out" / f"{volume_move.CASES[0]['name']}.json").read_text())
        self.assertIn("states", case)

    def test_a_heal_that_leaves_any_assessed_failure_stops_the_run(self):
        for mutation, left in (("elsewhere", "but the writable copy is"), ("two-running", "2 running db Containers"),
                               ("stopped", "stopped db Containers"), ("snapshots", "snapshots, over"),
                               ("lost", "acknowledged ids lost")):
            with self.subTest(mutation):
                checks, model = self.checks()
                setup = checks.setup
                def placed_wrong():
                    setup()
                    model.mutation = mutation
                checks.setup = placed_wrong
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([volume_move.CASES[0], volume_move.KILLS[0]])
                self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.CASES[0]["name"]])
                failures = summary["cases"][0]["failures"]
                self.assertIn("the run stopped after this case: after the heal, ", failures[-1])
                self.assertIn(left, failures[-1])
                self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_runs_list_that_fails_while_arming_skips_the_case(self):
        for armed in (False, True):
            with self.subTest(armed=armed):
                checks, model = self.checks()
                cli, guest = model.cli, model.guest
                def failing(args):
                    if (args[:2] == ["volume", "runs"] and checks.case_name == volume_move.KILLS[0]["name"]
                            and (model.faulted() or not armed)):
                        return 1, "Cloud did not answer"
                    return cli(args)
                def arming(machine, script):
                    if arms(script) and not model.holds:
                        model.hold(5, "cron-sync")
                        reply = guest(machine, script)
                        model.clock.advance(10)
                        return reply
                    return guest(machine, script)
                model.cli, model.guest = failing, arming
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
                skipped, clean = summary["cases"]
                self.assertIn("could not list the Volume's runs", skipped.get("skipped") or "", skipped)
                self.assertFalse(skipped["passed"])
                self.assertEqual(model.tripped, "cron-sync" if armed else None)
                self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})
                self.assertTrue(clean["passed"], clean)

    def test_a_skip_names_the_run_that_refused_last(self):
        checks, model = self.checks()
        model.hold(10, "sync-a")
        model.hold(volume_move.BUSY_LIMIT + 20, "sync-b", listed=False)
        checks.case_name = "clean"
        with redirect_stdout(io.StringIO()), self.assertRaisesRegex(volume_move.Skipped, "sync-b"):
            checks.start_op("move", "machine-2")
        self.assertEqual([wait["holder"] for wait in checks.busy_waits], ["sync-b"])

    def test_a_sync_that_takes_the_volume_while_the_fault_is_armed_skips_the_case(self):
        checks, model = self.checks()
        guest = model.guest

        def arming(machine, script):
            if arms(script) and not model.holds:
                model.hold(30, "cron-sync")
            return guest(machine, script)
        model.guest = arming
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0]])
        case = summary["cases"][0]
        self.assertEqual(model.tripped, "cron-sync")
        self.assertIn("while the fault was armed", case["skipped"] or "", case)
        self.assertFalse(case["passed"])
        self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})
        self.assertEqual([wait["holder"] for wait in summary["volume_busy_waits"]], ["cron-sync"])

    def test_a_sync_that_fires_the_armed_fault_and_ends_before_the_op_skips_the_case(self):
        checks, model = self.checks()
        cli, guest, armed_ops = model.cli, model.guest, []

        def arming(machine, script):
            if arms(script) and not model.holds:
                model.hold(5, "cron-sync")
                reply = guest(machine, script)
                model.clock.advance(10)
                return reply
            return guest(machine, script)
        def submitted(args):
            if args[0] == "volume" and args[1] in ("move", "mirror", "release") and model.faulted():
                armed_ops.append(args)
            return cli(args)
        model.cli, model.guest = submitted, arming
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        skipped, clean = summary["cases"]
        self.assertEqual(model.tripped, "cron-sync")
        self.assertIn("while the fault was armed", skipped.get("skipped") or "", skipped)
        self.assertFalse(skipped["passed"])
        self.assertEqual(armed_ops, [])
        self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})
        self.assertTrue(clean["passed"], clean)
        self.assertEqual([wait["holder"] for wait in summary["volume_busy_waits"]], ["cron-sync"])

    def test_a_sync_running_when_the_fault_would_be_armed_is_waited_out_before_arming(self):
        checks, model = self.checks()
        listed_runs = checks.listed_runs

        def a_sync_starts(op):
            if not model.holds:
                model.hold(5, "cron-sync")
            return listed_runs(op)
        checks.listed_runs = a_sync_starts
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0]])
        self.assertIsNone(model.tripped)
        self.assertTrue(summary["passed"], summary)
        self.assertEqual([wait["holder"] for wait in summary["volume_busy_waits"]], ["cron-sync"])

    def test_a_holder_that_ends_just_before_the_limit_arms_the_fault_only_after_the_barrier(self):
        checks, model = self.checks()
        model.hold(volume_move.BUSY_LIMIT - 2)
        checks.case_name = volume_move.KILLS[0]["name"]
        armed = []
        with redirect_stdout(io.StringIO()):
            checks.start_op("move", "machine-2", arm=lambda: armed.append(model.clock()))
        self.assertEqual(len(armed), 1)
        self.assertGreater(armed[0], max(at for _, at in model.restarts))
        self.assertGreaterEqual(min(at for _, at in model.restarts), 1000 + volume_move.BUSY_LIMIT - 2)
        self.assertEqual([wait["holder"] for wait in checks.busy_waits], ["sync-1"])

        checks, model = self.checks()
        setup = checks.setup
        def held_after_setup():
            setup()
            model.hold(volume_move.BUSY_LIMIT - 2)
        checks.setup = held_after_setup
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0]])
        self.assertTrue(summary["cases"][0]["passed"], summary["cases"][0])

    def test_a_runs_reply_that_stalls_past_the_limit_then_shows_quiet_runs_the_case_and_still_writes_the_summary(self):
        checks, model = self.checks()
        cli, start_op, stalled, starting = model.cli, checks.start_op, [], []
        def stall(args):
            if args[:2] == ["volume", "runs"] and starting == [volume_move.KILLS[0]["name"]] and not stalled:
                stalled.append(True)
                model.clock.advance(volume_move.BUSY_LIMIT + 1)
            return cli(args)
        def starts(*args, **kwargs):
            starting.append(checks.case_name)
            return start_op(*args, **kwargs)
        model.cli, checks.start_op = stall, starts
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertEqual(stalled, [True])
        self.assertTrue(summary["cases"][0]["passed"], summary["cases"][0])
        self.assertTrue(summary["cases"][1]["passed"], summary["cases"][1])
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_run_that_just_ended_does_not_replace_the_holder_already_seen(self):
        checks, model = self.checks()
        model.hold(100, "sync-a")
        model.just_ended = 1
        checks.case_name = "clean"
        with redirect_stdout(io.StringIO()):
            row = checks.wait_run(checks.start_op("move", "machine-2"))
        self.assertEqual(row["state"], "done")
        self.assertEqual([wait["holder"] for wait in checks.busy_waits], ["sync-a"])

    def test_a_failed_disarm_stops_the_run_and_still_writes_the_summary(self):
        checks, model = self.checks()
        guest = model.guest
        def failing_once_armed(machine, script):
            model.disarm_fails = model.disarm_fails or arms(script)
            return guest(machine, script)
        model.guest = failing_once_armed
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.KILLS[0]["name"]])
        self.assertIn("disarming the fault failed on machine-1, machine-2", summary["cases"][0]["failures"][-1])
        self.assertFalse(summary["passed"])
        self.assertEqual(json.loads((self.root / "out" / "summary.json").read_text())["passed"], False)

    def test_the_fault_file_holds_the_armed_value_and_disarming_removes_it(self):
        bash = shutil.which("bash")
        def run(value):
            script = volume_move.fault_script(value).replace("/run/ployz", str(self.root / "run"))
            subprocess.run([bash, "-c", script], check=True)
        path = self.root / "run" / "fault"
        run("delay-rpc:SetVolumeCopy.MirrorToMirror:90")
        self.assertEqual(path.read_text(), "delay-rpc:SetVolumeCopy.MirrorToMirror:90\n")
        self.assertEqual(sorted(entry.name for entry in path.parent.iterdir()), ["fault"])
        run(None)
        self.assertFalse(path.exists())

    def test_a_runs_list_that_times_out_in_the_heal_stops_the_run_and_still_writes_the_summary(self):
        checks, model = self.checks("two-writers")
        cli, heal, healing = model.cli, checks.heal, []
        def stalled(args):
            if healing and args[:2] == ["volume", "runs"]:
                raise subprocess.TimeoutExpired(args, 660)
            return cli(args)
        def heal_stalled(destination):
            healing.append(destination)
            return heal(destination)
        model.cli, checks.heal = stalled, heal_stalled
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.CASES[0], volume_move.KILLS[0]])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.CASES[0]["name"]])
        self.assertIn("the run stopped after this case: TimeoutExpired", summary["cases"][0]["failures"][-1])
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_cleanup_that_cannot_read_the_machines_stops_the_run_and_still_writes_the_summary(self):
        checks, model = self.checks()
        guest, disarmed = model.guest, []
        def unreadable(machine, script):
            if checks.case_name and disarms(script):
                disarmed.append(machine)
            if disarmed and script.startswith("for d in"):
                return 255, ""
            return guest(machine, script)
        model.guest = unreadable
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.KILLS[0]["name"]])
        self.assertIn("the run stopped after this case", summary["cases"][0]["failures"][-1])
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_release_approval_missing_a_field_or_the_token_fails_its_case_and_still_writes_the_summary(self):
        undo = CANCEL_UNDOS["undo-after-cut"]
        complete = dict(approval="release:digest", approval_id="approval-1")
        for name, error, token, missing in (
                ("no details", dict(code="approval_required"), True, " without details:"),
                ("no approval id", dict(code="approval_required", details=dict(approval="release:digest")), True,
                 " without approval_id:"),
                ("no approval", dict(code="approval_required", details=dict(approval_id="approval-1")), True,
                 " without approval:"),
                ("no token file", dict(code="approval_required", details=complete), False,
                 f", but there is no Cloud token at {self.root / 'private' / 'cloud-token'}")):
            with self.subTest(name):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                shutil.rmtree(self.root / "private", ignore_errors=True)
                if token:
                    (self.root / "private").mkdir()
                    (self.root / "private" / "cloud-token").write_text("token\n")
                checks, model = self.checks()
                cli = model.cli

                def asking(args, cli=cli, error=error):
                    if args[:2] == ["volume", "release"] and "--approval" not in args:
                        return 1, json.dumps(dict(error=error))
                    return cli(args)
                model.cli = asking
                with mock.patch.object(volume_move, "http", lambda *args, **kwargs: (200, "{}")), \
                        redirect_stdout(io.StringIO()):
                    summary = checks.run_all([volume_move.CASES[0], undo])
                written = json.loads((self.root / "out" / "summary.json").read_text())
                self.assertEqual([case["name"] for case in written["cases"]], ["clean", "undo-after-cut"])
                clean, failed = written["cases"]
                self.assertTrue(clean["passed"], clean)
                self.assertFalse(failed["passed"])
                self.assertTrue(failed["failures"][0].startswith(f"RuntimeError: Cloud asked to approve the release{missing}"),
                                failed)
                self.assertTrue((self.root / "out" / "undo-after-cut.json").exists())
                self.assertFalse(summary["passed"])

    def test_an_error_that_escapes_the_case_loop_fails_the_summary_names_the_error_and_re_raises(self):
        broken = {key: value for key, value in volume_move.CASES[0].items() if key != "trigger"}
        broken["name"] = "broken"
        checks, _ = self.checks()
        with redirect_stdout(io.StringIO()), self.assertRaises(KeyError):
            checks.run_all([volume_move.CASES[0], broken])
        written = json.loads((self.root / "out" / "summary.json").read_text())
        self.assertEqual([(case["name"], case["passed"]) for case in written["cases"]], [("clean", True)])
        self.assertEqual(written["error"], "KeyError: 'trigger'")
        self.assertFalse(written["passed"])

    def test_a_helper_that_fails_to_start_fails_its_case_names_the_command_and_still_writes_the_summary(self):
        kill = volume_move.KILLS[0]
        self.assertEqual(kill["name"], "kill-snapshot")
        for failing_call in (5, 20, 40):
            with self.subTest(failing_call):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                runner, calls = checks.runner, []

                def failing(args, runner=runner, calls=calls, failing_call=failing_call, **kwargs):
                    calls.append(args)
                    if len(calls) == failing_call:
                        raise PermissionError(13, "Permission denied", args[0])
                    return runner(args, **kwargs)
                checks.runner = failing
                escaped = None
                with redirect_stdout(io.StringIO()):
                    try:
                        checks.run_all([volume_move.CASES[0], kill])
                    except RuntimeError as error:
                        escaped = str(error)
                written = json.loads((self.root / "out" / "summary.json").read_text())
                self.assertFalse(written["passed"])
                command = " ".join(map(str, calls[failing_call - 1][1:8]))
                reasons = [escaped or ""] + [written.get("error") or ""] + \
                          [failure for case in written["cases"] for failure in case["failures"]]
                named = [reason for reason in reasons if "PermissionError" in reason and command in reason]
                self.assertTrue(named, (command, reasons))

    def test_a_fault_case_whose_move_is_refused_fails_and_names_the_reply_even_when_another_run_trips_the_fault(self):
        kill = volume_move.KILLS[0]
        self.assertEqual(kill["name"], "kill-snapshot")
        for fault_line in (True, False):
            with self.subTest(fault_line_after_the_read=fault_line):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                cli, listed_runs, refused = model.cli, checks.listed_runs, []

                def refusing(args, cli=cli, refused=refused):
                    if args[:2] == ["volume", "move"] and model.faulted() and not refused:
                        refused.append(args)
                        return 1, json.dumps(dict(error=dict(code="internal", message="placement exploded")))
                    return cli(args)

                def a_sync_trips_the_fault(op, listed_runs=listed_runs, refused=refused, fault_line=fault_line):
                    listed = listed_runs(op)
                    if refused and fault_line:
                        model.fired = True
                    return listed
                model.cli, checks.listed_runs = refusing, a_sync_trips_the_fault
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([kill])
                case = summary["cases"][0]
                self.assertEqual(len(refused), 1)
                self.assertFalse(case["passed"], case)
                self.assertFalse(summary["passed"])
                self.assertTrue(any("refused" in failure and "placement exploded" in failure
                                    for failure in case["failures"]), case["failures"])
                self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})

    def test_a_case_whose_last_run_did_not_end_done_fails_on_that_run_and_on_where_the_volume_ended(self):
        kill = volume_move.KILLS[0]
        self.assertEqual(kill["name"], "kill-snapshot")

        def every_move_fails(cli):
            def failing(args):
                if args[:2] == ["volume", "move"]:
                    row = dict(id=f"run-{len(model.runs)}", kind="move", state="failed")
                    model.runs.append(row)
                    return 0, json.dumps(row)
                return cli(args)
            return failing

        def the_rerun_is_refused(cli):
            def refusing(args):
                if args[:2] == ["volume", "move"] and model.fired and not model.faulted():
                    return 1, json.dumps(dict(error=dict(code="internal", message="placement exploded")))
                return cli(args)
            return refusing
        # The clean case holds its own run to its expectation, so it names the move rather than the last run.
        for case, plant, expected in (
                (volume_move.CASES[0], every_move_fails,
                 ["move run-0 is failed, expected done", "final writer machine-1, expected machine-2"]),
                (kill, the_rerun_is_refused,
                 ["the last run ended refused", "the Volume ended on machine-1, not machine-2"])):
            with self.subTest(case["name"]):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                model.cli = plant(model.cli)
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([case])
                result = summary["cases"][0]
                self.assertFalse(result["passed"], result)
                self.assertFalse(summary["passed"])
                self.assertEqual(result["final_writer"], "machine-1")
                for failure in expected:
                    self.assertIn(failure, result["failures"])

    def test_a_move_that_ends_done_without_moving_the_writer_fails_on_where_the_volume_ended(self):
        checks, model = self.checks()
        cli = model.cli

        def stays(args):
            if args[:2] == ["volume", "move"]:
                row = dict(id=f"run-{len(model.runs)}", kind="move", state="done")
                model.runs.append(row)
                return 0, json.dumps(row)
            return cli(args)
        model.cli = stays
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.CASES[0]])
        result = summary["cases"][0]
        self.assertFalse(result["passed"], result)
        self.assertEqual(result["failures"], ["final writer machine-1, expected machine-2"])

    def test_mirror_cases_and_a_released_undo_pass_ending_on_the_writer_they_started_on(self):
        undo = CANCEL_UNDOS["undo-after-cut"]
        cases = [case for case in volume_move.KILLS if case["op"] != "move"] + [undo]
        self.assertEqual([case["name"] for case in cases],
                         ["kill-first-pull", "kill-forget-mirror", "kill-forget-writer", "undo-after-cut"])
        (self.root / "private").mkdir()
        (self.root / "private" / "cloud-token").write_text("token\n")
        checks, model = self.checks()
        model.asks = {"release"}
        with mock.patch.object(volume_move, "http", lambda *args, **kwargs: (200, "{}")), \
                redirect_stdout(io.StringIO()):
            summary = checks.run_all(cases)
        for result in summary["cases"]:
            self.assertEqual(result["final_writer"], "machine-1", result)
            self.assertNotIn("the Volume ended on", " ".join(result["failures"]), result)
        self.assertEqual(summary["cases"][-1]["recovery"], ["done"])

    def test_a_mirror_case_passes_only_when_the_mirror_ends_present_and_a_mirror_rm_case_only_when_it_ends_gone(self):
        expected = {"mirror": "the mirror on machine-2 ended absent, not present",
                    "mirror-rm": "the mirror on machine-2 ended present, not absent"}
        for case in [case for case in volume_move.KILLS if case["op"] != "move"]:
            for path in ("changed nothing", "did its work"):
                with self.subTest(case=case["name"], path=path):
                    checks, model = self.checks()
                    cli = model.cli

                    def unchanged(args, model=model, cli=cli):
                        # Only the case's own op runs with the fault armed; prepare's mirror still works.
                        if args[:2] == ["volume", "mirror"] and model.faulted():
                            row = dict(id=f"run-{len(model.runs)}", kind="mirror", state="running", finished_at=None)
                            model.runs.append(row)
                            model.fired = True
                            model.close(row, "done", FAULTED_RUN)
                            return 0, json.dumps(row)
                        return cli(args)
                    if path == "changed nothing":
                        model.cli = unchanged
                    with redirect_stdout(io.StringIO()):
                        result = checks.run_all([case])["cases"][0]
                    self.assertEqual(result["run"], "done", result)
                    if path == "changed nothing":
                        self.assertFalse(result["passed"], result)
                        self.assertEqual(result["failures"], [expected[case["op"]]])
                    else:
                        self.assertTrue(result["passed"], result)
                        self.assertEqual(model.mirror, "machine-2" if case["op"] == "mirror" else None)

    def test_a_release_approval_cloud_rejects_fails_the_case_and_names_the_status(self):
        undo = CANCEL_UNDOS["undo-after-cut"]
        (self.root / "private").mkdir()
        (self.root / "private" / "cloud-token").write_text("token\n")
        checks, model = self.checks()
        model.asks = {"release"}
        with mock.patch.object(volume_move, "http", lambda *args, **kwargs: (403, '{"error":"forbidden"}')), \
                redirect_stdout(io.StringIO()):
            summary = checks.run_all([undo])
        failures = summary["cases"][0]["failures"]
        self.assertTrue(failures[0].startswith("RuntimeError: Cloud refused the approval approval-1 with 403"),
                        failures)
        self.assertFalse([args for args in model.started if "--approval" in args], model.started)

    def test_an_undo_passes_only_when_its_cancel_ends_the_run(self):
        undo = CANCEL_UNDOS["undo-before-cut"]
        for ended, failure in (("cancelled", None), ("lost", None),
                               ("done", "the cancel did not land: the run ended done"),
                               ("failed", "the cancel did not land: the run ended failed")):
            with self.subTest(ended):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                cli = model.cli

                def running(args, cli=cli):
                    if args[:2] == ["volume", "move"]:
                        row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                        model.runs.append(row)
                        model.fired = True
                        return 0, json.dumps(row)
                    return cli(args)

                def cancel(row, ended=ended):
                    found = next(run for run in model.runs if run["id"] == row["id"])
                    found["state"], found["finished_at"] = ended, model.now()
                    if ended == "done":
                        model.mirror, model.writer = model.writer, model.other(model.writer)
                    return [dict(kill="cancel", attempts=[f"DELETE {row['id']} 200 "], landed=True)]
                model.cli, checks.cancel = running, cancel
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([undo])
                result = json.loads((self.root / "out" / "undo-before-cut.json").read_text())
                self.assertTrue(result.get("triggered"), result.get("failures"))
                if failure:
                    self.assertFalse(result["passed"], result)
                    self.assertIn(failure, result["failures"])
                else:
                    self.assertTrue(result["passed"], result)
                    self.assertEqual(result["final_writer"], "machine-1")

    def test_a_triggered_case_passes_only_when_its_first_run_had_not_ended_before_the_trigger_was_seen(self):
        cases = {case["name"]: case for case in volume_move.CASES + volume_move.soak_cases()}
        # (case, how the first run ends, when it ends, when the trigger appears, whether the case may pass)
        for name, ended, ends, trips, passes in (
                # Opus's repro: the claimed move fails before its snapshot, and a later sync trips the armed kill.
                ("kill-snapshot", "failed", 0.5, 1.0, False),
                # Codex's repro: the claimed move fails before any SetVolumeCopy, and a later sync's pull trips it.
                ("kill-pull", "failed", 0.5, 1.0, False),
                # A request no run picked up closes lost, and a sync after it trips the armed kill.
                ("kill-snapshot", "lost", 0.5, 1.0, False),
                # pgrep also matches a later sync's receive.
                ("soak-1-rounds-kill-ployz-writer", "failed", 0.25, 0.5, False),
                # The kill lands inside the first run's effect, and the run ends after it.
                ("kill-snapshot", "failed", 1.5, 1.0, True),
                ("kill-pull", "failed", 1.5, 1.0, True),
                # A late kill, and the first run still ends done.
                ("kill-snapshot", "done", 1.5, 1.0, True),
                ("soak-1-rounds-kill-ployz-writer", "failed", 0.75, 0.5, True)):
            with self.subTest(case=name, first_run=ended, ends=ends, trips=trips):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                case, cli, first = cases[name], model.cli, []

                def scripted(args, cli=cli, case=case, first=first, ended=ended, ends=ends, trips=trips):
                    if args[:2] != ["volume", "move"] or first:
                        return cli(args)
                    row = dict(id=f"run-{len(model.runs)}", kind="move", state="running", finished_at=None)
                    model.runs.append(row)
                    first.append(row)
                    begun, destination = model.clock(), args[-1]

                    def end(row=row, ended=ended):
                        row["state"], row["finished_at"] = ended, iso(begun + ends)
                        if ended == "done":
                            model.mirror, model.writer = model.writer, destination
                    if ended == "lost":
                        model.unclaimed.add(row["id"])

                    def trip(case=case):
                        if case["trigger"] == "receiving":
                            model.receiving = True
                        else:
                            model.fired = True
                    model.at(begun + ends, end)
                    model.at(begun + trips, trip)
                    return 0, json.dumps(row)
                model.cli = scripted
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([case])["cases"][0]
                self.assertEqual(result["final_writer"], "machine-2", result)
                self.assertEqual(result["run"], ended)
                self.assertEqual(result["recovery"], [] if ended == "done" else ["done"])
                if passes:
                    self.assertTrue(result["passed"], result)
                else:
                    run = first[0]
                    self.assertFalse(result["passed"], result)
                    self.assertEqual(len(result["failures"]), 1, result["failures"])
                    if ended == "lost":
                        expected = "the trigger never fired before the run ended"
                    elif case["action"] == "none":
                        expected = f"the first run {run['id']} ended {ended} before the {case['trigger']} on"
                    else:
                        expected = f"the {case['action']} landed after the first run {run['id']} ended {ended}"
                    self.assertTrue(result["failures"][0].startswith(expected), result["failures"])

    def test_an_inngest_run_lookup_that_fails_raises_and_names_the_query(self):
        checks, model = self.checks()
        model.postgres = lambda sql: (1, "")
        with self.assertRaises(RuntimeError) as raised:
            checks.inngest_run(dict(id="run-7"))
        self.assertIn("select inngest_run_id from volume_run where id = 'run-7'", str(raised.exception))

    def test_an_undo_passes_only_when_cloud_accepted_its_cancel(self):
        undo = CANCEL_UNDOS["undo-before-cut"]
        errors = '{"errors":[{"message":"run not found"}]}'
        for name, replies, accepted in (
                ("DELETE 200", [(200, "")], True),
                ("DELETE 404 then GraphQL accepts", [(404, "not found"), (200, errors), (200, '{"data":{}}')], True),
                ("DELETE 404 and both GraphQL attempts error", [(404, "not found"), (200, errors), (200, errors)],
                 False)):
            with self.subTest(name):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                cli, asked = model.cli, []

                def running(args, cli=cli):
                    if args[:2] == ["volume", "move"]:
                        row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                        model.runs.append(row)
                        model.fired = True
                        return 0, json.dumps(row)
                    return cli(args)

                def inngest(method, url, body=None, token=None, replies=replies, asked=asked):
                    asked.append((method, url))
                    if len(asked) == len(replies):
                        # Cancelled when Cloud accepted it; otherwise reconciliation closes the run lost on its own.
                        model.runs[0]["state"] = "cancelled" if accepted else "lost"
                        model.runs[0]["finished_at"] = model.now()
                    return replies[len(asked) - 1]
                model.cli = running
                with mock.patch.object(volume_move, "http", inngest), redirect_stdout(io.StringIO()):
                    checks.run_all([undo])
                result = json.loads((self.root / "out" / "undo-before-cut.json").read_text())
                self.assertEqual(len(asked), len(replies))
                self.assertEqual(asked[0], ("DELETE", "http://inngest/v1/runs/inngest-run-0"))
                self.assertEqual(result["recovery"][-1]["state"], "done")
                self.assertEqual(result["final_writer"], "machine-1")
                if accepted:
                    self.assertTrue(result["passed"], result)
                else:
                    self.assertFalse(result["passed"], result)
                    self.assertEqual(len(result["failures"]), 1, result["failures"])
                    self.assertTrue(result["failures"][0].startswith("no evidence the cancel landed"),
                                    result["failures"])

    def test_a_soak_kill_passes_only_when_it_lands(self):
        cases = {case["name"]: case for case in volume_move.soak_cases()}
        for name, plant, landed in (
                ("soak-5-writer-runner-kill", None, True),
                ("soak-5-writer-runner-kill", "no-op kill", False),
                ("soak-5-writer-runner-kill", "stale worker pid", False),
                ("soak-5-writer-kill-ployz-writer", None, True),
                ("soak-5-writer-kill-ployz-writer", "no-op kill", False),
                ("soak-5-writer-kill-docker-other", None, True),
                ("soak-5-writer-kill-docker-other", "no-op kill", False)):
            with self.subTest(case=name, plant=plant):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                if plant == "no-op kill":
                    model.kill_lands = False
                if plant == "stale worker pid":
                    model.workers = {7}

                def effect(begun, model=model):
                    model.cut("machine-1", begun + 2)
                    model.log_fault("machine-2", begun + 4)
                self.first_move(model, 10, effect=effect)
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([cases[name]])["cases"][0]
                self.assertEqual(result["final_writer"], "machine-2", result)
                self.assertEqual(result["recovery"], ["done"])
                if landed:
                    self.assertTrue(result["passed"], result)
                else:
                    action = cases[name]["action"]
                    self.assertFalse(result["passed"], result)
                    self.assertEqual(len(result["failures"]), 1, result["failures"])
                    self.assertTrue(result["failures"][0].startswith(f"no evidence the {action} landed"),
                                    result["failures"])

    def first_move(self, model, ends, state="failed", effect=None, claimed=True, op=("volume", "move")):
        """The case's first op is accepted and ends state ends seconds later; effect(begun) schedules what it does."""
        cli, first = model.cli, []

        def scripted(args):
            if args[:len(op)] != list(op) or first:
                return cli(args)
            row = dict(id=f"run-{len(model.runs)}", kind=op[1], state="running", finished_at=None)
            model.runs.append(row)
            first.append(row)
            begun, destination = model.clock(), args[-1]

            def end():
                row["state"], row["finished_at"] = state, iso(begun + ends)
                if state == "done" and op[1] == "move":
                    model.mirror, model.writer = model.writer, destination
            if not claimed:
                model.unclaimed.add(row["id"])
            model.at(begun + ends, end)
            if effect:
                effect(begun)
            return 0, json.dumps(row)
        model.cli = scripted
        return first

    def test_a_receive_an_earlier_sync_left_running_is_not_credited_to_the_first_run(self):
        cases = {case["name"]: case for case in volume_move.soak_cases()}
        # Codex r3 finding 1: a sync holds the Volume, starts a receive and ends while the receive runs on; the first
        # move sends no effect and fails, or is never claimed and closes lost.
        for name, state, ends, receive_ends in (
                ("soak-1-rounds-runner-kill", "failed", 1, 30),
                ("soak-1-rounds-kill-ployz-writer", "failed", 1, 30),
                ("soak-1-rounds-kill-docker-other", "failed", 1, 30),
                ("soak-1-rounds-runner-kill", "lost", 600, 30),
                ("soak-1-rounds-runner-kill", "failed", 1, None)):
            with self.subTest(case=name, first_run=state, receive_ends=receive_ends):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                prepare = checks.prepare

                def a_sync_left_a_receive(case, other, prepare=prepare, model=model, receive_ends=receive_ends):
                    prepare(case, other)
                    model.hold(4, "sync-1")
                    model.receiving = {"machine-2"}
                    if receive_ends:
                        model.at(model.clock() + receive_ends, lambda: setattr(model, "receiving", False))
                checks.prepare = a_sync_left_a_receive
                self.first_move(model, ends, state, claimed=state != "lost")
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([cases[name]])["cases"][0]
                self.assertFalse(result["passed"], result)
                if receive_ends:
                    self.assertEqual((result["run"], result["recovery"]), (state, ["done"]), result)
                    self.assertIn("the trigger never fired before the run ended", result["failures"], result)
                else:
                    self.assertIn(f"a zfs receive still ran on machine-2 past {volume_move.BUSY_LIMIT}s before move",
                                  result.get("skipped") or "", result)

    def test_a_fault_line_from_before_the_barrier_is_behind_its_cursor_whatever_the_clock_says(self):
        # A Forget want names no epoch, and a rebuilt Cloud counts gens again from the start, so an earlier
        # invocation's forget of this Volume logged the same line, gen included; only the cursor puts it behind the
        # barrier.
        kill = next(case for case in volume_move.KILLS if case["name"] == "kill-forget-writer")
        checks, model = self.checks()
        model.offset["machine-1"] = 20.0
        prepare = checks.prepare

        def an_old_fault_line(case, other):
            prepare(case, other)
            model.log_fault("machine-1", model.clock() - 15,
                            f'fault: killing daemon inside the effect verb="SetVolumeCopy.WriterToForget" volume={PHYSICAL} '
                            f'gen={model.gen(model.epoch(f"run-{len(model.runs)}"))}')
        checks.prepare = an_old_fault_line
        self.first_move(model, 1, op=("volume", "mirror", "rm"))
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([kill])["cases"][0]
        self.assertFalse(result["passed"], result)
        self.assertEqual(result["failures"], ["the trigger never fired before the run ended"], result)

    def test_a_stop_or_a_fault_after_it_seen_after_the_first_run_ended_is_not_its(self):
        cases = {case["name"]: case for case in volume_move.soak_cases()}
        for name in ("soak-3-cut-kill-ployz-writer", "soak-4-final-kill-ployz-writer"):
            # Both triggers poll every 0.5 s, so the run ends and the trigger appears between two polls.
            for ends, trips, passes in ((0.75, 1.0, False), (1.5, 1.0, True)):
                with self.subTest(case=name, ends=ends, trips=trips):
                    shutil.rmtree(self.root / "out", ignore_errors=True)
                    checks, model = self.checks()
                    case = cases[name]

                    def effect(begun, model=model, case=case, trips=trips):
                        if case["trigger"] == "stopped":
                            model.cut("machine-1", begun + trips - 0.1)
                        else:
                            model.cut("machine-1", begun + trips - 0.5)
                            model.log_fault("machine-2", begun + trips)
                    first = self.first_move(model, ends, effect=effect)
                    with redirect_stdout(io.StringIO()):
                        result = checks.run_all([case])["cases"][0]
                    self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)
                    if passes:
                        self.assertTrue(result["passed"], result)
                    else:
                        run = first[0]
                        self.assertFalse(result["passed"], result)
                        self.assertEqual(len(result["failures"]), 1, result["failures"])
                        self.assertTrue(result["failures"][0].startswith(
                            f"the kill-ployz landed after the first run {run['id']} ended failed"), result["failures"])

    def test_a_first_run_killed_inside_a_receive_it_started_passes(self):
        cases = {case["name"]: case for case in volume_move.soak_cases()}
        for name in ("soak-1-rounds-kill-ployz-writer", "soak-1-rounds-kill-docker-other", "soak-1-rounds-runner-kill"):
            with self.subTest(name):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()

                def effect(begun, model=model):
                    model.at(begun + 0.5, lambda: setattr(model, "receiving", {"machine-2"}))
                    model.at(begun + 2, lambda: setattr(model, "receiving", False))
                self.first_move(model, 2, effect=effect)
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([cases[name]])["cases"][0]
                self.assertTrue(result["passed"], result)
                self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)

    def test_a_sync_that_starts_and_ends_between_the_clean_start_and_an_unarmed_request_skips_the_case(self):
        case = next(case for case in volume_move.soak_cases() if case["name"] == "soak-1-rounds-runner-kill")
        self.assertIsNone(case["fault"])
        checks, model = self.checks()
        self.first_move(model, 1)
        scripted, slipped = model.cli, []

        def a_sync_slips_in(args):
            if args[:2] == ["volume", "move"] and not slipped:
                slipped.append(True)
                now = model.clock()
                model.holds.append(dict(id="sync-2", start=now, until=now, listed=True))
                model.receiving = {"machine-2"}
                model.at(now + 5, lambda: setattr(model, "receiving", False))
            return scripted(args)
        model.cli = a_sync_slips_in
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([case])["cases"][0]
        self.assertFalse(result["passed"], result)
        self.assertIn("a run took the Volume between the clean start and move: run sync-2 (sync)",
                      result.get("skipped") or "", result)

    def test_a_triggered_case_fails_when_no_inngest_run_claimed_its_first_run(self):
        # A Forget want carries no epoch, so its fault line is credited by Volume alone: a late RPC from a
        # run that ended before the case logs that line while the request waits unclaimed.
        kill = next(case for case in volume_move.KILLS if case["name"] == "kill-forget-writer")
        checks, model = self.checks()
        first = self.first_move(model, 600, "lost", claimed=False, op=("volume", "mirror", "rm"),
                                effect=lambda begun: model.at(begun + 1, lambda: model.log_fault("machine-1")))
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([kill])["cases"][0]
        self.assertEqual((result["run"], result["recovery"]), ("lost", ["done"]), result)
        self.assertEqual(result["failures"], [f"the first run {first[0]['id']} ended lost and no Inngest run claimed "
                                              "it, so the trigger evidence is not its"], result)

    def admit_pull(self, model, machine, spawns, ends):
        """An earlier sync's pull is admitted on machine: `pull` is set now, its receive spawns and ends later."""
        now = model.clock()
        model.pulls.add(machine)

        def finish():
            model.receiving = False
            model.pulls.discard(machine)

        def spawn():
            model.receiving = {machine}
            model.task(machine, now + ends, "pull", finish)
        model.task(machine, now + spawns, "pull", spawn)

    def run_with(self, name, before=None, ends=60, effect=None):
        """Run one case whose first move is claimed and ends failed; before(model) runs right after prepare."""
        case = next(case for case in volume_move.CASES + volume_move.soak_cases() if case["name"] == name)
        shutil.rmtree(self.root / "out", ignore_errors=True)
        checks, model = self.checks()
        prepare = checks.prepare

        def prepared(case, other):
            prepare(case, other)
            if before:
                before(model)
        checks.prepare = prepared
        self.first_move(model, ends, effect=effect)
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([case])["cases"][0]
        return result, model

    def assert_not_credited(self, result):
        """No credited event survives: either nothing qualified before the run ended, or the row read after the
        action was already terminal."""
        self.assertFalse(result["passed"], result)
        self.assertFalse(any("no Inngest run claimed" in failure for failure in result["failures"]), result)
        if not result.get("skipped"):
            self.assertTrue("the trigger never fired before the run ended" in result["failures"]
                            or any(" could be credited: " in failure for failure in result["failures"]), result)

    def test_a_pull_admitted_before_its_receive_exists_is_not_credited_to_the_first_run(self):
        # Codex r4 finding 1 and Opus S1: no process exists at the clean start, and the receive spawns 30 s later.
        for name in ("soak-1-rounds-runner-kill", "soak-1-rounds-kill-ployz-writer", "soak-1-rounds-kill-docker-other"):
            with self.subTest(name):
                result, _ = self.run_with(name, lambda model: self.admit_pull(model, "machine-2", 30, 40))
                self.assert_not_credited(result)

    def test_a_receive_whose_pull_names_an_older_epoch_is_not_credited_to_the_first_run(self):
        # A late step of the closed run admits its own round pull after the clean start, and that receive runs while
        # the first run, epoch 2, works.
        def late_pull(begun):
            model = self.model
            model.at(begun + 1, lambda: (model.pull_targets.update({"machine-2": "w-1-0"}),
                                         setattr(model, "receiving", {"machine-2"})))
        result, _ = self.run_with("soak-1-rounds-runner-kill", self.close_an_earlier_run, 5, late_pull)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_pull_the_first_run_admitted_with_no_receive_running_is_not_credited(self):
        # The plugin sets `ployz:pull` when it admits the round pull, before the receive spawns, and keeps it after
        # the receive ends; only a running receive puts the kill inside it.
        def admitted(begun):
            model = self.model
            model.at(begun + 1, lambda: model.pull_targets.update({"machine-2": "w-1-0"}))
        result, _ = self.run_with("soak-1-rounds-runner-kill", lambda model: setattr(self, "model", model), 5, admitted)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_cut_an_earlier_move_left_waiting_in_the_daemon_is_not_credited_to_the_first_run(self):
        # Codex r4's detached cut: the cut waits for the admission turn and stops the writer after the clean start.
        def waiting(model):
            model.task("machine-1", model.clock() + 20, "cut", lambda: model.cut("machine-1", model.clock()))
        result, model = self.run_with("soak-3-cut-runner-kill", waiting)
        self.assert_not_credited(result)
        self.assertIn(("machine-1", "cut"), model.interrupted)

    def test_a_cancel_restarts_the_runner_exactly_once_even_when_its_lookup_raises(self):
        # Codex bot round 8: act kills the runner before cancel, and an inngest_run that raises inside cancel left
        # the runner dead for every later case.
        undo = CANCEL_UNDOS["undo-before-cut"]
        clean = volume_move.CASES[0]
        for raises in (False, True):
            with self.subTest(raises=raises):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                states, worker, cli, first = [], model.worker, model.cli, []

                def recording(state, worker=worker, states=states):
                    states.append(state)
                    return worker(state)

                def running(args, cli=cli, first=first):
                    if args[:2] != ["volume", "move"] or first:
                        return cli(args)
                    row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                    model.runs.append(row)
                    model.fired = True
                    first.append(row)
                    return 0, json.dumps(row)

                def unreadable(sql, first=first, postgres=model.postgres):
                    # Only the claim lookup fails; the epoch read still lets the trigger credit its line.
                    if sql.startswith("select gen, "):
                        return postgres(sql)
                    first[0]["state"], first[0]["finished_at"] = "failed", model.now()
                    return 1, ""

                def cancel(row):
                    found = next(run for run in model.runs if run["id"] == row["id"])
                    found["state"], found["finished_at"] = "cancelled", model.now()
                    return [dict(kill="cancel", attempts=[f"DELETE {row['id']} 200 "], landed=True)]
                model.worker, model.cli = recording, running
                if raises:
                    model.postgres = unreadable
                else:
                    checks.cancel = cancel
                with redirect_stdout(io.StringIO()):
                    summary = checks.run_all([undo, clean])
                self.assertEqual(states, ["kill", "start"])
                self.assertIn(int(model.pid_file.read_text()), model.workers)
                results = {result["name"]: result for result in summary["cases"]}
                self.assertEqual(set(results), {undo["name"], clean["name"]}, summary)
                self.assertTrue(results[clean["name"]]["passed"], results[clean["name"]])
                if raises:
                    self.assertIn("psql exited 1", results[undo["name"]]["failures"][0])

    def test_a_clean_start_read_that_does_not_prove_it_ran_skips_the_case(self):
        # Codex r4 finding 3: the clean-start read fails while the leftover it would have shown is real, and the
        # trigger polls that follow succeed.
        for unreadable in ("exec", "pgrep", "pull"):
            with self.subTest(unreadable):
                def leftover(model, unreadable=unreadable):
                    model.unreadable = {"machine-2": unreadable}
                    if unreadable == "pull":
                        self.admit_pull(model, "machine-2", 30, 40)
                    else:
                        model.receiving = {"machine-2"}
                        model.at(model.clock() + 30, lambda: setattr(model, "receiving", False))
                    cli = model.cli

                    def readable_once_requested(args):
                        if args[:2] == ["volume", "move"]:
                            model.unreadable = {}
                        return cli(args)
                    model.cli = readable_once_requested
                result, _ = self.run_with("soak-1-rounds-runner-kill", leftover)
                self.assertFalse(result["passed"], result)
                self.assertIn("could not read", result.get("skipped") or "", result)
                self.assertIn("on machine-2", result.get("skipped") or "", result)

    def test_the_barrier_waits_out_a_pull_and_a_receive_instead_of_killing_them(self):
        def receive_in_run(begun, model=None):
            model = self.model
            model.at(begun + 0.5, lambda: setattr(model, "receiving", {"machine-2"}))
            model.at(begun + 2, lambda: setattr(model, "receiving", False))
        for name, leftover in (
                ("a pull admitted with no process yet", lambda model: self.admit_pull(model, "machine-2", 30, 40)),
                ("a receive already running", lambda model: (
                    model.pulls.add("machine-2"), setattr(model, "receiving", {"machine-2"}),
                    model.task("machine-2", model.clock() + 20, "pull", lambda: (
                        setattr(model, "receiving", False), model.pulls.discard("machine-2")))))):
            with self.subTest(name):
                def before(model, leftover=leftover):
                    self.model = model
                    leftover(model)
                result, model = self.run_with("soak-1-rounds-runner-kill", before, 2, receive_in_run)
                self.assertTrue(result["passed"], result)
                self.assertEqual(model.interrupted, [])
                self.assertEqual(model.pulls, set())

    def test_a_pull_a_restart_left_set_is_ended_by_the_barrier_at_the_limit(self):
        # A restart stops a pull's task but leaves `pull` set until a run asks again; waiting on it would skip every
        # later case.
        def stale(model):
            model.pulls.add("machine-2")

        def receive_in_run(begun, model=None):
            model = self.model
            model.at(begun + 0.5, lambda: setattr(model, "receiving", {"machine-2"}))
            model.at(begun + 2, lambda: setattr(model, "receiving", False))

        def before(model):
            self.model, self.planted = model, model.clock()
            stale(model)
        result, model = self.run_with("soak-1-rounds-runner-kill", before, 2, receive_in_run)
        self.assertTrue(result["passed"], result)
        self.assertEqual(model.interrupted, [])
        self.assertGreaterEqual(min(at for _, at in model.restarts if at >= self.planted) - self.planted,
                                volume_move.BUSY_LIMIT)
        self.assertEqual(model.pulls, {"machine-2"})

    def run_cut(self, name="soak-3-cut-runner-kill", plant=None, ends=10, effect=None):
        """Run one case whose first run cuts machine-1 at +2; plant(model) runs right after prepare."""
        def before(model):
            self.model = model
            if plant:
                plant(model)

        def cut(begun):
            self.model.cut("machine-1", begun + 2)
            if effect:
                effect(self.model, begun)
        return self.run_with(name, before, ends, cut)

    def test_the_action_runs_right_after_the_trigger_is_credited(self):
        # Codex r8 E: a kill meant for the 6 s MirrorToMirror hold that starts at +14 must land before +20, so
        # nothing slow may run between the trigger and the kill, not even with 1 s execs.
        begun = []

        def before(model):
            self.model = model
            model.latency = {"machine-1": 1.0, "machine-2": 1.0}

        def effect(at):
            begun.append(at)
            self.model.cut("machine-1", at + 10)
            self.model.log_fault("machine-2", at + 14)
        result, model = self.run_with("soak-4-final-runner-kill", before, 30, effect)
        self.assertTrue(result["passed"], result)
        self.assertLess(model.kills[0], begun[0] + 20, (model.kills, begun))

    def close_an_earlier_run(self, model):
        """An earlier move on the Volume whose row Cloud already closed. It took epoch 1, so the first run takes 2."""
        self.model = model
        model.runs.append(dict(id=f"run-{len(model.runs)}", kind="move", state="failed", finished_at=model.now()))

    def test_a_late_step_of_a_closed_run_is_not_credited_to_the_first_run(self):
        # Round 9 open false pass 1: a step of an earlier run outlives its row and cuts the writer after the clean
        # start. Its cut names that run's target, f-1, and the first run asked for f-2.
        result, _ = self.run_with("soak-3-cut-runner-kill", self.close_an_earlier_run, 10,
                                  lambda begun: self.model.cut("machine-1", begun + 2, target="f-1"))
        self.assert_not_credited(result)

    def run_alone(self, effect):
        """Run soak-3-cut-runner-kill with only the cut effect(model, begun) makes."""
        return self.run_with("soak-3-cut-runner-kill", lambda model: setattr(self, "model", model), 10,
                             lambda begun: effect(self.model, begun))

    def test_a_cut_line_for_another_volume_is_not_credited(self):
        # Round 9's other-Volume gap: the db also mounts a second Volume, whose cut on the same writer stops it.
        result, _ = self.run_alone(lambda model, begun: model.cut("machine-1", begun + 2,
                                                                  volume="verify-production_vol-2"))
        self.assert_not_credited(result)

    def test_a_cut_line_for_another_target_is_not_credited(self):
        result, _ = self.run_alone(lambda model, begun: model.cut("machine-1", begun + 2, target="f-7"))
        self.assert_not_credited(result)

    def test_a_service_act_stop_inside_the_cut_is_credited_only_once_the_cut_line_follows(self):
        # Round 9 open false pass 2: the first run's cut begins at +1.75 and drains, and a service act stops the db
        # at +2. The case's action belongs after the cut has every holder stopped.
        with self.subTest("the cut never gets that far"):
            result, _ = self.run_alone(lambda model, begun: (
                model.cut("machine-1", begun + 6, begins=begun + 1.75, finishes=False)))
            self.assert_not_credited(result)
        with self.subTest("the cut line follows at +6.1"):
            begun = []
            result, model = self.run_alone(lambda model, at: (
                begun.append(at), model.cut("machine-1", at + 6, begins=at + 1.75)))
            self.assertTrue(result["passed"], result)
            self.assertGreaterEqual(model.kills[0], begun[0] + 6.1, (model.kills, begun))

    def test_arming_puts_only_the_cases_own_fault_on_its_machines(self):
        side = dict(writer="machine-1", other="machine-2")
        for case in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(case["name"]):
                expect = {}
                if case["fault"]:
                    targets = [side[case["fault_on"]]] if case.get("fault_on") else ["machine-1", "machine-2"]
                    expect.update({machine: case["fault"] for machine in targets})
                self.assertEqual(volume_move.arming(case, side), expect)

    def test_a_cut_is_credited_only_from_this_runs_cut_line(self):
        # Codex r8 A: a fault line that names no Volume or want is not this run's, whatever lands after since.
        with self.subTest("a fault line without identity"):
            result, _ = self.run_with("soak-4-final-runner-kill", lambda model: model.log_fault(
                "machine-2", model.clock() + 35, "fault: delaying switch verb"))
            self.assert_not_credited(result)
        with self.subTest("this run's cut"):
            result, model = self.run_cut()
            self.assertTrue(result["passed"], result)
            self.assertIn(" INFO cut: holders stopped volume=verify-production_vol-1 target=f-1 gen=100",
                          [text for machine, _, text in model.journal if machine == "machine-1"])
        with self.subTest("a cut line for this target from another gen"):
            result, _ = self.run_with("soak-3-cut-runner-kill", lambda model: setattr(self, "model", model), 10,
                                      lambda begun: self.model.cut("machine-1", begun + 2, gen=999))
            self.assert_not_credited(result)

    def test_a_writer_db_that_is_not_accepting_connections_after_the_hold_skips_the_case(self):
        def stopping(model):
            model.stopping.add("machine-1")
            model.at(model.clock() + 13, lambda: model.stopping.discard("machine-1"))
        result, _ = self.run_cut(plant=stopping)
        self.assertFalse(result["passed"], result)
        self.assertIn("the db on machine-1 is not accepting connections after the clean-start hold before move",
                      result.get("skipped") or "", result)

    def test_a_clean_start_restart_that_does_not_prove_fresh_processes_skips_the_case(self):
        # Codex r8 C: `|| true` hid a plugin stop that failed, and nothing checked the daemon and plugin were new.
        for name, plant, machine in (
                ("plugin stop fails", lambda model: model.plugin_stop_fails.add("machine-2"), "machine-2"),
                ("old plugin outlives its stop", lambda model: (
                    # setup's restart left the plugin inactive; prepare's volume create socket-activated it again
                    model.units["machine-2"].__setitem__(PLUGIN, model.process()), model.survives.add("machine-2")),
                 "machine-2"),
                ("plugin socket-activated again", lambda model: model.reactivated.add("machine-2"), None)):
            with self.subTest(name):
                result, _ = self.run_cut(plant=plant)
                if machine:
                    self.assertFalse(result["passed"], result)
                    self.assertIn(f"the clean-start restart is unproven on {machine} before move",
                                  result.get("skipped") or "", result)
                else:
                    self.assertTrue(result["passed"], result)
        with self.subTest("a pull outlives a failed plugin stop"):
            def kept_pull(model):
                model.plugin_stop_fails.add("machine-2")
                self.admit_pull(model, "machine-2", 230, 260)
            result, _ = self.run_with("soak-1-rounds-runner-kill", kept_pull)
            self.assertFalse(result["passed"], result)
            self.assertIn("the clean-start restart is unproven on machine-2 before move", result.get("skipped") or "",
                          result)

    def test_a_clean_start_read_that_is_cut_short_or_unknown_skips_the_case(self):
        for name, reply in (
                ("cut short", "zfs 0\npgrep 1\npull ployz\t-\n"),
                ("pgrep matched with no record", f"zfs 0\npgrep 0\npull ployz\t-\npull ployz/ployz-mirror/{PHYSICAL}\t-\n"
                                                 "end\n"),
                ("an unknown line", "zfs 0\npgrep 1\npull ployz\t-\nwarning: something\nend\n")):
            with self.subTest(name):
                result, _ = self.run_cut(plant=lambda model, reply=reply: model.quiet_reply.update({"machine-2": reply}))
                self.assertFalse(result["passed"], result)
                self.assertIn("could not read", result.get("skipped") or "", result)
                self.assertIn("on machine-2", result.get("skipped") or "", result)

    def test_a_trigger_poll_that_exits_nonzero_withholds_credit(self):
        def failing(model):
            self.model = model
            model.poll_fails = True

        def receive(begun):
            self.model.at(begun + 0.5, lambda: setattr(self.model, "receiving", {"machine-2"}))
            self.model.at(begun + 2, lambda: setattr(self.model, "receiving", False))
        for name, ends, effect in (
                ("soak-3-cut-runner-kill", 10, lambda begun: self.model.cut("machine-1", begun + 2)),
                ("kill-snapshot", 3, lambda begun: self.model.log_fault("machine-1", begun + 1)),
                ("soak-1-rounds-runner-kill", 2, receive)):
            with self.subTest(name):
                result, _ = self.run_with(name, failing, ends, effect)
                self.assert_not_credited(result)

    def test_quiet_seen_on_the_poll_after_the_deadline_runs_the_barrier(self):
        def receive(model):
            model.receiving = {"machine-2"}
            model.at(model.clock() + 178, lambda: setattr(model, "receiving", False))
        result, model = self.run_cut(plant=receive)
        self.assertTrue(result["passed"], result)
        self.assertEqual(model.interrupted, [])

    def test_each_planted_defect_fails_the_run(self):
        for mutation in ("two-writers", "stopped", "snapshots", "lost"):
            with self.subTest(mutation):
                summary, _ = self.run_cases([volume_move.CASES[0]], mutation)
                self.assertFalse(summary["passed"])

    def test_a_vm_clock_stepped_ahead_and_back_across_a_warm_round_fault_does_not_credit_it(self):
        # Codex r6 1: machine-2 runs 20 s ahead only while its warm-round pull logs its fault, so the stamp lands
        # after the cut line's and no later reading shows any movement. The line names a round target, not f-<epoch>.
        def effect(begun):
            model = self.model
            model.at(begun + 1, lambda: model.offset.update({"machine-2": 20.0}))
            model.log_fault("machine-2", begun + 2)
            model.at(begun + 9, lambda: model.offset.update({"machine-2": 0.0}))
            model.cut("machine-1", begun + 10)
        result, _ = self.run_with("soak-4-final-runner-kill", lambda model: setattr(self, "model", model), 60, effect)
        self.assert_not_credited(result)

    def test_a_host_clock_jump_that_stamped_the_row_ahead_does_not_credit_a_fault_after_it_ended(self):
        # Codex r6 1: the worker stamped finished_at during a +20 s host jump between two samples, so the first run
        # looks open when its own fault point fires 0.2 s after the row closed.
        def effect(begun):
            model = self.model
            model.at(begun + 0.1, lambda: model.runs[0].update(finished_at=iso(begun + 20.1)))
            model.log_fault("machine-1", begun + 0.3)
        result, _ = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 0.1, effect)
        self.assert_not_credited(result)

    def test_a_credited_fault_line_vacuumed_before_the_case_end_fails_the_case(self):
        # journald vacuums the credited line; a retry that crossed the armed point again may have logged a later one,
        # which must not stand in for it.
        for retried in (False, True):
            with self.subTest(retried=retried):
                def effect(begun):
                    model = self.model
                    line = model.fault_text("machine-1")
                    model.log_fault("machine-1", begun + 1, line)
                    if retried:
                        model.log_fault("machine-1", begun + 2.5, line)
                    model.at(begun + 3, lambda: model.vacuumed.add(next(
                        index for index, (where, _, text) in enumerate(model.journal)
                        if where == "machine-1" and text.startswith("fault: "))))
                result, _ = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 5, effect)
                self.assertTrue(any("did not re-qualify at the case end" in failure for failure in result["failures"]),
                                result)

    def test_a_persistent_vm_clock_change_cannot_replace_the_credited_fault_with_a_later_one(self):
        # Codex r6 2, Opus r7 1: machine-2 moves 20 s ahead and stays there. The warm-round fault then stamps after
        # the cut, the kill lands on it, and the case-end recheck finds the genuine final-pull fault instead.
        begun = []

        def effect(at):
            model = self.model
            begun.append(at)
            model.at(at + 1, lambda: model.offset.update({"machine-2": 20.0}))
            model.log_fault("machine-2", at + 2)
            model.cut("machine-1", at + 10)
            model.log_fault("machine-2", at + 12)
        result, model = self.run_with("soak-4-final-runner-kill", lambda model: setattr(self, "model", model), 60,
                                      effect)
        self.assertTrue(result["passed"], result)
        credited = json.loads((self.root / "out" / "soak-4-final-runner-kill.json").read_text())["triggered"]
        self.assertIn(f"target=f-{model.epoch(model.runs[0]['id'])}", json.dumps(credited), credited)
        self.assertGreaterEqual(model.kills[0], begun[0] + 12, (model.kills, begun))

    def test_no_clock_read_runs_in_a_case(self):
        # Codex r6 3 asked that a failed `date +%s.%N` never decide credit; now no case reads a Machine clock at all.
        reads = []

        def plant(model):
            guest = model.guest

            def watched(machine, script):
                if re.search(r"date \+|timedatectl|FinishedAt", script):
                    reads.append(script)
                return guest(machine, script)
            model.guest = watched
        result, _ = self.run_cut(plant=plant)
        self.assertTrue(result["passed"], result)
        self.assertEqual(reads, [])

    SIGNALLED = """
import os, shutil, signal, sys
from verify import test_volume_move as T
tests = T.VolumeMoveTests("setUp")
tests.setUp()
checks, model = tests.checks()
send = lambda: os.kill(os.getpid(), signal.SIG)
if MID_RUN:
    tests.first_move(model, 30, effect=lambda begun: model.at(begun + 1, send))
if IN_SUMMARY:
    read_acks, summarize = T.volume_move.read_acks, checks.summarize
    def armed(*args):
        T.volume_move.read_acks = lambda path: (send(), read_acks(path))[1]
        return summarize(*args)
    checks.summarize = armed
T.volume_move.VolumeMove = lambda *args, **kwargs: checks
sys.argv = ["volume-move", "run", str(tests.manifest), "--case", "kill-snapshot"]
try:
    T.volume_move.main()
finally:
    if (checks.out / "summary.json").exists():
        shutil.copy(checks.out / "summary.json", sys.OUT)
"""

    def signalled(self, name, mid_run, in_summary):
        """Run main() in a child that signals itself one second into the first run, inside summarize, or both,
        and return the summary it left."""
        out = self.root / f"{name}-{mid_run}-{in_summary}.json"
        script = self.SIGNALLED.replace("signal.SIG", f"signal.{name}").replace("sys.OUT", repr(str(out))) \
            .replace("MID_RUN", repr(mid_run)).replace("IN_SUMMARY", repr(in_summary))
        run = subprocess.run([sys.executable, "-c", script], cwd=Path(volume_move.__file__).parent,
                             capture_output=True, text=True, timeout=120)
        self.assertTrue(out.exists(), f"no summary.json after {name}: {run.stderr[-2000:]}")
        summary = json.loads(out.read_text())
        self.assertFalse(summary["passed"], summary)
        self.assertIn(name, summary["error"] or "", summary)

    def test_a_sigterm_or_sighup_mid_run_leaves_a_summary(self):
        # Codex r6 4, Opus r7 3: the signal arrives one second into the first run, through main.
        for name in ("SIGTERM", "SIGHUP"):
            with self.subTest(name):
                self.signalled(name, mid_run=True, in_summary=False)

    def test_a_signal_during_summarize_leaves_a_summary(self):
        # Builder 1 left a second signal inside summarize as an open window; the first signal may land there too.
        for name in ("SIGTERM", "SIGHUP"):
            for mid_run in (True, False):
                with self.subTest(name, second=mid_run):
                    self.signalled(name, mid_run=mid_run, in_summary=True)

    def test_a_cleanup_step_that_raises_writes_a_failing_summary(self):
        checks, model = self.checks()

        def raising():
            raise RuntimeError("the probe log is gone")
        checks.stop_probe = raising
        with redirect_stdout(io.StringIO()), self.assertRaises(RuntimeError):
            checks.run_all([volume_move.CASES[0]])
        self.assertTrue((self.root / "out" / "summary.json").exists(), "no summary.json after the cleanup error")
        summary = json.loads((self.root / "out" / "summary.json").read_text())
        self.assertFalse(summary["passed"], summary)
        self.assertIn("the probe log is gone", summary["error"] or "", summary)

    def test_a_d_state_receive_that_outlives_the_plugin_stop_skips_the_case(self):
        # Opus r7 2: the old plugin's MainPID is gone, but its zfs receive sits in D state and stays in the unit's
        # control group after the stop.
        result, _ = self.run_cut(plant=lambda model: model.stuck_receive.add("machine-2"))
        self.assertFalse(result["passed"], result)
        self.assertIn("the clean-start restart is unproven on machine-2 before move", result.get("skipped") or "",
                      result)

    def test_a_cgroup_procs_read_that_fails_skips_the_case(self):
        # A `cat` that failed printed an empty procs line, which read as a proven-empty control group.
        result, _ = self.run_cut(plant=lambda model: model.unread_procs.add("machine-2"))
        self.assertFalse(result["passed"], result)
        self.assertIn("the clean-start restart is unproven on machine-2 before move", result.get("skipped") or "",
                      result)
        self.assertIn("cgroup.procs of ployz-volume-plugin.service was not read: cat exited 1",
                      result.get("skipped") or "", result)

    def test_a_restart_reply_that_exits_nonzero_or_stops_short_skips_the_case(self):
        for reply, problem in (("exit 1", "the restart exited 1"), ("no end", "the restart exited 0"),
                               ("no cursor", "no journal cursor")):
            with self.subTest(reply):
                result, _ = self.run_cut(plant=lambda model, reply=reply: model.restart_reply.update({"machine-2": reply}))
                self.assertFalse(result["passed"], result)
                self.assertIn("the clean-start restart is unproven on machine-2 before move",
                              result.get("skipped") or "", result)
                self.assertIn(problem, result.get("skipped") or "", result)

    def test_setup_disarms_a_fault_an_earlier_run_left_behind_before_anything_else(self):
        # A run killed while armed leaves the fault file behind, and the first case would fire it.
        checks, model = self.checks()
        model.fault["machine-2"] = "kill-daemon:SetVolumeCopy.MirrorToMirror"
        cli, seen = model.cli, []

        def recording(args):
            seen.append(dict(model.fault))
            return cli(args)
        model.cli = recording
        checks.setup()
        self.assertEqual(seen[0], {"machine-1": None, "machine-2": None}, seen)
        self.assertEqual(model.restarts, [])

    def test_a_leftover_fault_that_cannot_be_disarmed_stops_the_run_before_any_case(self):
        checks, model = self.checks()
        model.disarm_fails = True
        with redirect_stdout(io.StringIO()), self.assertRaises(RuntimeError) as raised:
            checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertIn("disarming a leftover fault failed on machine-1, machine-2", str(raised.exception))
        summary = json.loads((self.root / "out" / "summary.json").read_text())
        self.assertEqual(summary["cases"], [])
        self.assertIn("disarming a leftover fault failed", summary["error"])
        self.assertFalse(summary["passed"])

    def run_forget(self, **identity):
        """Run kill-forget-writer whose first mirror rm fails after 3 s; its fault line at +1 carries identity."""
        kill = next(case for case in volume_move.KILLS if case["name"] == "kill-forget-writer")
        checks, model = self.checks()
        self.first_move(model, 3, op=("volume", "mirror", "rm"),
                        effect=lambda begun: model.log_fault("machine-1", begun + 1, **identity))
        with redirect_stdout(io.StringIO()):
            checks.run_all([kill])
        return self.recorded(kill["name"])

    def recorded(self, name):
        """The case's whole record, with the credited event the summary leaves out."""
        return json.loads((self.root / "out" / f"{name}.json").read_text())

    def test_a_forget_fault_line_is_credited_by_the_volume_and_gen(self):
        # A Forget want names nothing, so its line carries the Volume and the run's gen.
        result = self.run_forget()
        self.assertEqual(result["triggered"]["identity"], dict(volume=PHYSICAL, gen="101"), result)
        self.assertTrue(result["passed"], result)
        result = self.run_forget(volume="other_vol")
        self.assertIsNone(result["triggered"], result)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_forget_fault_line_from_another_gen_is_refused(self):
        # Another run's Forget on the same Volume names nothing either; only the gen tells it from this run's.
        result = self.run_forget(gen="999")
        self.assertIsNone(result["triggered"], result)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_fault_line_before_cloud_records_an_epoch_is_not_credited(self):
        # The want names the first run's epoch; until Cloud records one, no line can be matched against it.
        def before(model):
            self.model = model
            postgres = model.postgres
            model.postgres = lambda sql: (0, postgres(sql)[1].split("|")[0] + "|-1") if sql.startswith("select gen, ") else postgres(sql)
        result, _ = self.run_with("kill-snapshot", before, 3, lambda begun: self.model.log_fault("machine-1", begun + 1))
        self.assertIsNone(self.recorded("kill-snapshot")["triggered"], result)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_receive_is_not_looked_for_before_cloud_records_an_epoch(self):
        # The receiving trigger needs the epoch to name the round pull; without one no shell runs on the Machine.
        scripts = []

        def before(model):
            self.model = model
            postgres, guest = model.postgres, model.guest
            model.postgres = lambda sql: (0, postgres(sql)[1].split("|")[0] + "|-1") if sql.startswith("select gen, ") else postgres(sql)
            model.guest = lambda machine, script: (scripts.append(script), guest(machine, script))[1]
            self.admit_pull(model, "machine-2", 1, 2)
        result, _ = self.run_with("soak-1-rounds-runner-kill", before, 3)
        self.assertIsNone(self.recorded("soak-1-rounds-runner-kill")["triggered"], result)
        self.assertFalse([script for script in scripts if "pgrep -f" in script], result)

    def test_the_first_qualifying_entry_after_the_cursor_is_the_one_stored(self):
        def effect(begun):
            self.model.log_fault("machine-1", begun + 1)
            self.model.log_fault("machine-1", begun + 1)
        result, model = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 5, effect)
        self.assertTrue(result["passed"], result)
        first = next(index for index, (where, _, text) in enumerate(model.journal)
                     if where == "machine-1" and text.startswith("fault: "))
        self.assertEqual(self.recorded("kill-snapshot")["triggered"]["cursor"], model.cursor("machine-1", first), result)

    def test_trigger_polling_stops_once_the_first_run_is_terminal(self):
        # The run ends at +3 and a line of this Volume lands at +10: nothing after the end may be credited, so the
        # harness stops looking rather than wait for it.
        result, model = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 3,
                                      lambda begun: self.model.log_fault("machine-1", begun + 10))
        self.assertIsNone(self.recorded("kill-snapshot")["triggered"], result)
        self.assertIn("the trigger never fired before the run ended", result["failures"], result)
        self.assertFalse(any("could be credited" in failure for failure in result["failures"]), result)

    def test_a_journal_entry_that_cannot_be_parsed_withholds_credit(self):
        # A read that holds a line it cannot place may hide the credited one's order, so it credits nothing.
        for garbage in ("not json", "[]"):
            with self.subTest(garbage):
                def before(model, garbage=garbage):
                    self.model = model
                    model.garbage_journal["machine-1"] = garbage
                result, _ = self.run_with("kill-snapshot", before, 3,
                                          lambda begun: self.model.log_fault("machine-1", begun + 1))
                self.assertIsNone(self.recorded("kill-snapshot")["triggered"], result)
                self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_fault_line_from_an_older_epoch_is_not_credited(self):
        # A late step of the closed epoch-1 run reaches its fault point after the clean start, naming snapshot w-1-0.
        result, _ = self.run_with("kill-snapshot", self.close_an_earlier_run, 3,
                                  lambda begun: self.model.log_fault("machine-1", begun + 1, epoch=1))
        self.assert_not_credited(result)

    def test_a_fault_line_from_another_gen_with_this_runs_epoch_is_not_credited(self):
        # A later run that reuses the epoch, say after a requeue, names the same snapshot; only its gen differs.
        result, _ = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 3,
                                  lambda begun: self.model.log_fault("machine-1", begun + 1, gen=999))
        self.assert_not_credited(result)

    def test_a_fault_line_for_another_volume_is_not_credited(self):
        result, _ = self.run_with("kill-snapshot", lambda model: setattr(self, "model", model), 3,
                                  lambda begun: self.model.log_fault("machine-1", begun + 1, volume="other_vol"))
        self.assert_not_credited(result)

    def test_a_round_pull_fault_line_after_the_cut_is_not_credited_as_the_final_pull(self):
        # The line is a MirrorToMirror delay on the other Machine after the cut line, but it names a round target.
        def effect(begun):
            model = self.model
            model.cut("machine-1", begun + 10)
            model.log_fault("machine-2", begun + 12, target=f"w-{model.epoch(model.runs[-1]['id'])}-1")
        result, _ = self.run_with("soak-4-final-runner-kill", lambda model: setattr(self, "model", model), 60, effect)
        self.assert_not_credited(result)

    def test_an_action_that_lands_after_the_first_runs_row_is_terminal_is_not_credited(self):
        # Every exec on machine-2 takes 1 s, so the kill sent on the fault seen at +15 lands after the run ends at +15.5.
        begun = []

        def before(model):
            self.model = model
            model.latency["machine-2"] = 1.0

        def effect(at):
            begun.append(at)
            self.model.cut("machine-1", at + 10)
            self.model.log_fault("machine-2", at + 14)
        result, model = self.run_with("soak-4-final-kill-ployz-other", before, 15.5, effect)
        self.assertGreater(model.kills[0], begun[0] + 15.5, (model.kills, begun))
        self.assertFalse(result["passed"], result)
        self.assertTrue(any(failure.startswith("the kill-ployz landed after the first run")
                            for failure in result["failures"]), result["failures"])

    CLOCKS = ("machine-1", "machine-2", "cloud", "host")

    def perturb(self, model, label, number, seen):
        """Every clock the harness could read starts off, slews, and keeps stepping 20 s either way while the run
        goes on, as seed `label number` draws it. Only the host's drift is bounded, to 60 s: it feeds deadlines, and
        a deadline that crossed would change what the case does, not what it credits. Slews alternate sign with the
        number so two seeds cover both."""
        rng, clock, drift = random.Random(f"{label} {number}"), model.clock, dict.fromkeys(self.CLOCKS, 0.0)

        def move(name, delta):
            if name == "host":
                clock.step(delta)
            elif name == "cloud":
                model.cloud["offset"] += delta
            else:
                model.offset[name] += delta

        def slew(name, rate):
            if name == "host":
                clock.rate = rate
            elif name == "cloud":
                model.cloud["rate"] = rate
            else:
                model.rate[name] = rate

        def arm(name):
            delta = rng.choice((-20.0, 20.0))
            if name == "host" and abs(drift[name] + delta) > 60:
                delta = -delta

            def step():
                drift[name] += delta
                move(name, delta)
                seen[name].add("stepped ahead" if delta > 0 else "stepped behind")
                arm(name)
            model.at(model.clock() + rng.uniform(0.5, 30), step)
        for index, name in enumerate(self.CLOCKS):
            move(name, rng.uniform(-30, 30))
            rate = rng.uniform(0.01, 0.1) * (1 if (number + index) % 2 else -1)
            slew(name, rate)
            seen[name].add("slewed fast" if rate > 0 else "slewed slow")
            arm(name)

    def clock_corpus(self):
        """Every case clean, each planted defect, and the credit scenarios the tests above settle, each with the
        verdict its own test asserts. (label, passes, cases, driver kwargs)"""
        every = volume_move.CASES + volume_move.soak_cases()
        cases = {case["name"]: case for case in every}
        # The undos cancel through Inngest, which only their own tests fake; the fake fires no trigger on its own
        # for the 1-rounds, 3-cut and 4-final soak steps.
        unfired = [case for case in every if case["step"] in ("1-rounds", "3-cut", "4-final") and "soak" in case["name"]]
        corpus = [("every clean and kill case", True,
                   [case for case in every if "undo" not in case["name"] and case not in unfired], {}),
                  ("every soak case whose trigger never fires", False, unfired, {})]
        corpus += [(f"planted {mutation}", False, [cases["clean"]], dict(mutation=mutation))
                   for mutation in ("two-writers", "stopped", "snapshots", "lost")]
        corpus += [(f"planted {mutation}", False, [cases["deploy-plain"], cases[name]], dict(mutation=mutation))
                   for mutation, name in (("deploy-waits", "deploy-warm"), ("requeue-unrelated", "deploy-warm"),
                                          ("yielded-running", "deploy-warm"), ("requeue-none", "deploy-after-cut"),
                                          ("cut-other-gen", "deploy-after-cut"))]

        def trip(model, begun, case, trips):
            def fire():
                if case["trigger"] == "receiving":
                    model.receiving = True
                else:
                    model.fired = True
            model.at(begun + trips, fire)
        for name, state, ends, trips, passes in (
                ("kill-snapshot", "failed", 0.5, 1.0, False), ("kill-pull", "failed", 0.5, 1.0, False),
                ("kill-snapshot", "lost", 0.5, 1.0, False), ("soak-1-rounds-kill-ployz-writer", "failed", 0.25, 0.5, False),
                ("kill-snapshot", "failed", 1.5, 1.0, True), ("kill-pull", "failed", 1.5, 1.0, True),
                ("kill-snapshot", "done", 1.5, 1.0, True), ("soak-1-rounds-kill-ployz-writer", "failed", 0.75, 0.5, True)):
            corpus.append((f"{name} ends {state} at {ends}, trips at {trips}", passes, [cases[name]],
                           dict(ends=ends, state=state, claimed=state != "lost",
                                effect=lambda model, begun, case=cases[name], trips=trips: trip(model, begun, case, trips))))

        def sync_left_a_receive(model, receive_ends):
            model.hold(4, "sync-1")
            model.receiving = {"machine-2"}
            if receive_ends:
                model.at(model.clock() + receive_ends, lambda: setattr(model, "receiving", False))
        for name, state, ends, receive_ends in (
                ("soak-1-rounds-runner-kill", "failed", 1, 30), ("soak-1-rounds-runner-kill", "lost", 600, 30),
                ("soak-1-rounds-runner-kill", "failed", 1, None)):
            corpus.append((f"{name} after a sync left a receive ending {receive_ends}", False, [cases[name]],
                           dict(before=lambda model, receive_ends=receive_ends: sync_left_a_receive(model, receive_ends),
                                ends=ends, state=state, claimed=state != "lost")))

        def old_fault_line(model):
            model.offset["machine-2"] = 20.0
            model.log_fault("machine-2", model.clock() - 15)
        corpus.append(("fault line from before the barrier", False, [volume_move.KILLS[0]],
                       dict(before=old_fault_line, ends=1)))
        for name in ("soak-3-cut-kill-ployz-writer", "soak-4-final-kill-ployz-writer"):
            for ends, trips, passes in ((0.75, 1.0, False), (1.5, 1.0, True)):
                def late(model, begun, case=cases[name], trips=trips):
                    if case["trigger"] == "stopped":
                        model.cut("machine-1", begun + trips - 0.1)
                    else:
                        model.cut("machine-1", begun + trips - 0.5)
                        model.log_fault("machine-2", begun + trips)
                corpus.append((f"{name} ends at {ends}, trigger at {trips}", passes, [cases[name]],
                               dict(ends=ends, effect=late)))

        def inside_a_receive(model, begun):
            model.at(begun + 0.5, lambda: setattr(model, "receiving", {"machine-2"}))
            model.at(begun + 2, lambda: setattr(model, "receiving", False))
        corpus.append(("killed inside its own receive", True, [cases["soak-1-rounds-runner-kill"]],
                       dict(ends=2, effect=inside_a_receive)))
        corpus.append(("pull admitted before its receive exists", False, [cases["soak-1-rounds-runner-kill"]],
                       dict(before=lambda model: self.admit_pull(model, "machine-2", 30, 40))))
        corpus.append(("cut an earlier move left waiting", False, [cases["soak-3-cut-runner-kill"]],
                       dict(before=lambda model: model.task("machine-1", model.clock() + 20, "cut",
                                                            lambda: model.cut("machine-1", model.clock())),
                            ends=30)))
        cut = cases["soak-3-cut-runner-kill"]
        corpus.append(("this run's cut", True, [cut], dict(ends=10, effect=lambda m, b: m.cut("machine-1", b + 2))))
        corpus.append(("late step of a closed run", False, [cut],
                       dict(before=self.close_an_earlier_run, ends=10,
                            effect=lambda m, b: m.cut("machine-1", b + 2, target="f-1"))))
        corpus.append(("cut line for another volume", False, [cut],
                       dict(ends=10, effect=lambda m, b: m.cut("machine-1", b + 2, volume="verify-production_vol-2"))))
        corpus.append(("cut line for another target", False, [cut],
                       dict(ends=10, effect=lambda m, b: m.cut("machine-1", b + 2, target="f-7"))))

        final = cases["soak-4-final-runner-kill"]

        def stepped_across_a_round_fault(model, begun):
            model.at(begun + 1, lambda: model.offset.update({"machine-2": 20.0}))
            model.log_fault("machine-2", begun + 2)
            model.at(begun + 9, lambda: model.offset.update({"machine-2": 0.0}))
            model.cut("machine-1", begun + 10)
        corpus.append(("vm clock stepped across a warm round fault", False, [final],
                       dict(ends=60, effect=stepped_across_a_round_fault)))

        def row_stamped_ahead(model, begun):
            model.at(begun + 0.1, lambda: model.runs[0].update(finished_at=iso(begun + 20.1)))
            model.log_fault("machine-1", begun + 0.3)
        corpus.append(("host jump stamped the row ahead", False, [cases["kill-snapshot"]],
                       dict(ends=0.1, effect=row_stamped_ahead)))

        def persistent_change(model, begun):
            model.at(begun + 1, lambda: model.offset.update({"machine-2": 20.0}))
            model.log_fault("machine-2", begun + 2)
            model.cut("machine-1", begun + 10)
            model.log_fault("machine-2", begun + 12)
        corpus.append(("persistent vm clock change", True, [final], dict(ends=60, effect=persistent_change)))
        corpus.append(("fault line from an older epoch", False, [cases["kill-snapshot"]],
                       dict(before=self.close_an_earlier_run, ends=3,
                            effect=lambda m, b: m.log_fault("machine-1", b + 1, epoch=1))))
        corpus.append(("fault line for another volume", False, [cases["kill-snapshot"]],
                       dict(ends=3, effect=lambda m, b: m.log_fault("machine-1", b + 1, volume="other_vol"))))

        def round_pull_after_the_cut(model, begun):
            model.cut("machine-1", begun + 10)
            model.log_fault("machine-2", begun + 12, target=f"w-{model.epoch(model.runs[-1]['id'])}-1")
        corpus.append(("round pull fault line after the cut", False, [final],
                       dict(ends=60, effect=round_pull_after_the_cut)))

        def cut_then_fault(model, begun):
            model.cut("machine-1", begun + 10)
            model.log_fault("machine-2", begun + 14)
        corpus.append(("action lands after the row is terminal", False, [cases["soak-4-final-kill-ployz-other"]],
                       dict(before=lambda model: model.latency.update({"machine-2": 1.0}), ends=15.5,
                            effect=cut_then_fault)))
        corpus.append(("action right after the trigger with slow execs", True, [final],
                       dict(before=lambda model: model.latency.update({"machine-1": 1.0, "machine-2": 1.0}), ends=30,
                            effect=cut_then_fault)))
        return corpus

    def verdict(self, cases, perturb=None, mutation=None, before=None, ends=None, state="failed", claimed=True,
                effect=None):
        """Run cases on a fresh model and keep what decides the run: the summary minus the host-timed gap, and each
        credited event minus its monotonic poll time. With ends the first op is scripted as first_move does."""
        shutil.rmtree(self.root / "out", ignore_errors=True)
        checks, model = self.checks(mutation)
        self.model = model
        if perturb:
            perturb(model)
        prepare = checks.prepare

        def prepared(case, other):
            prepare(case, other)
            if before:
                before(model)
        checks.prepare = prepared
        if ends is not None:
            self.first_move(model, ends, state, effect and (lambda begun: effect(model, begun)), claimed)
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all(cases)
        decided = []
        for case in summary["cases"]:
            out = self.root / "out" / f"{case['name']}.json"
            credited = json.loads(out.read_text()).get("triggered") if out.exists() else None
            if credited:
                credited = {key: value for key, value in credited.items() if key != "poll_end_mono"}
            decided.append(dict({key: value for key, value in case.items() if key != "gap"}, triggered=credited))
        return dict(passed=summary["passed"], error=summary["error"], cases=decided)

    def test_no_clock_step_or_slew_anywhere_changes_any_verdict(self):
        # The four facts never read a clock, so every Machine, Cloud and the host may step and slew at random while
        # each case the tests above settle runs, and the verdict and the credited event stay the same.
        seeds = range(4)
        seen = {name: set() for name in self.CLOCKS}
        for label, passes, cases, kwargs in self.clock_corpus():
            with self.subTest(label):
                plain = self.verdict(cases, **kwargs)
                self.assertEqual(plain["passed"], passes, plain)
                for number in seeds:
                    perturbed = self.verdict(cases, lambda model, number=number: self.perturb(model, label, number, seen),
                                             **kwargs)
                    self.assertEqual(perturbed, plain, f"seed `{label} {number}`")
        for name in self.CLOCKS:
            self.assertEqual(seen[name], {"stepped ahead", "stepped behind", "slewed fast", "slewed slow"}, name)

    def test_each_deploy_and_undo_case_passes_against_a_correct_model(self):
        summary, model = self.run_cases(volume_move.CASES[:7])
        self.assertTrue(summary["passed"], json.dumps(summary["cases"], indent=1))
        runs = {case["name"]: case["run"] for case in summary["cases"]}
        self.assertEqual([runs[name] for name in ("deploy-warm", "deploy-after-cut", "deploy-committed",
                                                  "undo-before-cut", "undo-after-cut")],
                         ["yielded", "yielded", "done", "cancelled", "done"])
        result = json.loads((self.root / "out" / "deploy-after-cut.json").read_text())
        self.assertEqual(result["acted"]["running_after"], [result["writer_before"]])
        self.assertEqual([row["state"] for row in result["requeues"]], ["done"])
        self.assertEqual(model.fault, {"machine-1": None, "machine-2": None})

    def test_a_committed_trigger_credits_only_the_first_runs_own_row_while_it_runs(self):
        # The committed credit is a row read: the first run's row must record the committed phase and still run.
        case = next(case for case in volume_move.CASES if case["name"] == "deploy-committed")
        for label, reads in (("this run committed and running", None),
                             ("committed but already ended", "committed|done"),
                             ("running but not yet committed", "copying|running")):
            with self.subTest(label):
                self.setUp()
                checks, model = self.checks()
                seen, postgres = [], model.postgres

                def read(sql, reads=reads, postgres=postgres, seen=seen):
                    if sql.startswith("select phase, state "):
                        seen.append(sql)
                        if reads:
                            return 0, reads
                    return postgres(sql)
                model.postgres = read
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([case])["cases"][0]
                first = model.runs[0]["id"] if reads else None
                self.assertTrue(seen and all("id = 'run-" in sql for sql in seen), seen)
                if reads is None:
                    self.assertTrue(result["passed"], result)
                    self.assertEqual(self.recorded(case["name"])["triggered"]["identity"]["phase"], "committed")
                else:
                    self.assertIsNotNone(first)
                    self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_each_deploy_and_undo_defect_fails_with_a_message_naming_it(self):
        cases = {row["name"]: row for row in volume_move.CASES}
        planted = [
            ("deploy-two-machines", "deploy-warm", "after the Deploy db runs on ['machine-1', 'machine-2']"),
            ("deploy-waits", "deploy-warm", "the hold was released by limit, not the disarm after the Deploy"),
            ("requeue-twice", "deploy-warm", "2 requeued moves, expected 1"),
            ("requeue-none", "deploy-after-cut", "0 requeued moves, expected 1"),
            ("release-changes-row", "undo-after-cut", "release on a committed lane changed rows"),
            ("yielded-running", "deploy-warm", "is still running after 60s, expected yielded"),
            ("mount-error", "deploy-after-cut",
             "the Deploy printed 'Provisioned Volume status has an invalid mountpoint'"),
            ("requeue-unrelated", "deploy-warm", "0 requeued moves, expected 1"),
            ("cut-other-gen", "deploy-after-cut", "the trigger never fired before the run ended"),
        ]
        for mutation, name, message in planted:
            with self.subTest(mutation):
                self.setUp()
                summary, _ = self.run_cases([cases["deploy-plain"], cases[name]], mutation)
                failures = summary["cases"][-1]["failures"]
                self.assertFalse(summary["passed"])
                self.assertTrue(any(message in failure for failure in failures), failures)


if __name__ == "__main__":
    unittest.main()
