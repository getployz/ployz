#!/usr/bin/env python3
"""Exercise the volume move checks with a command model, without Incus, Cloud or ZFS."""

from contextlib import redirect_stdout
from datetime import datetime, timezone
import importlib.util
import io
import json
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("volume_move", Path(__file__).resolve().parents[1] /
                                            "test-verify-volume-move.py")
volume_move = importlib.util.module_from_spec(spec)
spec.loader.exec_module(volume_move)

PHYSICAL = "verify-production_vol-1"
CALL_LIMIT = 6000


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


class ClusterModel:
    """Two Machines and a Cloud that moves the Volume's writer, with optional planted defects."""

    def __init__(self, mutation=None):
        self.mutation = mutation
        self.writer = "machine-1"
        self.mirror = None
        self.runs = []
        self.fault = {"machine-1": None, "machine-2": None}
        self.journal = []
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
        self.stops = []

    @property
    def fired(self):
        return bool(self.journal)

    @fired.setter
    def fired(self, value):
        if value:
            for machine in self.offset:
                self.log_fault(machine)

    def log_fault(self, machine, at=None, text="fault: killing daemon inside the effect"):
        """The daemon on machine logs a fault line at host time at, stamped by machine's own clock."""
        self.journal.append((machine, self.clock() if at is None else at, text))

    def stop(self, machine, at=None):
        """The db Container on machine exits at host time at, and Docker stamps it in machine's clock."""
        self.stops.append((machine, self.clock() if at is None else at))

    def stamp(self, machine, at):
        """What machine's clock read at host time at."""
        return at + self.offset[machine] + self.rate[machine] * (at - 1000.0)

    def receiving_on(self, machine):
        return self.receiving is True or (isinstance(self.receiving, set) and machine in self.receiving)

    def at(self, moment, event):
        """Something happens in Cloud or on a Machine once the clock reaches moment."""
        self.events.append((moment, event))

    def task(self, machine, moment, name, event):
        """In-process work on machine that does something once the clock reaches moment, unless a restart drops it."""
        self.tasks.append((moment, machine, name, event))

    def restart(self, machine):
        """systemd stops the unit's whole cgroup: in-process tasks and zfs children die, the `pull` property stays."""
        self.restarts.append((machine, self.clock()))
        self.interrupted += [(machine, name) for _, where, name, _ in self.tasks if where == machine]
        self.tasks = [task for task in self.tasks if task[1] != machine]
        if self.receiving_on(machine):
            self.interrupted.append((machine, "zfs receive"))
            self.receiving = (set(self.offset) if self.receiving is True else set(self.receiving)) - {machine}

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
        return iso(self.clock() if self.clock else 1000.0)

    def holder(self):
        return next((hold for hold in self.holds if self.clock() < hold["until"]), None)

    def __call__(self, args, **kwargs):
        self.calls += 1
        if self.calls > CALL_LIMIT:
            raise Runaway(f"More than {CALL_LIMIT} helper commands; a wait loop is not sleeping")
        self.tick()
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
        """Cloud records the Inngest run that claims a request; an unclaimed one has none."""
        run_id = re.search(r"id = '([^']+)'", sql)[1]
        return 0, "" if run_id in self.unclaimed else f"inngest-{run_id}\n"

    def worker(self, state):
        """verify-cluster runner kills the recorded worker if it is alive, and start replaces a dead one."""
        pid = int(self.pid_file.read_text())
        if state == "kill" and self.kill_lands:
            self.workers.discard(pid)
        if state == "start" and pid not in self.workers:
            self.workers.add(pid + 1)
            self.pid_file.write_text(f"{pid + 1}\n")
        return 0, ""

    def cli(self, args):
        if args[0] == "volume" and args[1] in self.asks and "--approval" not in args:
            details = dict(approval=f"{args[1]}:digest", approval_id="approval-1")
            return 1, json.dumps(dict(error=dict(code="approval_required", details=details)))
        if args[:2] != ["volume", "runs"]:
            self.started.append(args)
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
            destination = args[-1]
            row = dict(id=f"run-{len(self.runs)}", kind="move", state="running", finished_at=self.now())
            self.runs.append(row)
            if any(self.fault.values()):
                self.fired = True
                row["state"] = "failed"
            else:
                self.mirror, self.writer = self.writer, destination
                row["state"] = "done"
            return 0, json.dumps(row)
        if args[:2] == ["volume", "mirror"]:
            row = dict(id=f"run-{len(self.runs)}", kind="mirror", state="done", finished_at=self.now())
            self.runs.append(row)
            # The armed kill lands inside the effect and the resumed run finishes it.
            if any(self.fault.values()):
                self.fired = True
            if args[2] == "rm":
                self.mirror = None
            else:
                self.mirror = args[-1]
            return 0, json.dumps(row)
        if args[:2] == ["volume", "release"]:
            row = dict(id=f"run-{len(self.runs)}", kind="release", state="done", finished_at=self.now())
            self.runs.append(row)
            return 0, json.dumps(row)
        return 0, "{}"

    def guest(self, machine, script):
        if "systemctl restart ployz.service" in script:
            self.restart(machine)
            if "PLOYZD_FAULT" not in script:
                return 0, ""
        if "ployz:pull" in script:
            return self.quiet(machine)
        if "PLOYZD_FAULT" in script:
            value = script.split("PLOYZD_FAULT=", 2)[-1].split("'", 1)[0] if "echo" in script else None
            if value is None and self.disarm_fails:
                return 255, ""
            held = self.holder() if self.clock else None
            if value and held:
                # The holder crosses the armed point, so the fault fires inside it and not in the case's run.
                self.tripped = held["id"]
                self.fired = True
            self.fault[machine] = value
            return 0, ""
        if script == "date +%s.%N":
            self.clock.now += self.latency[machine] / 2
            reading = self.stamp(machine, self.clock())
            self.clock.now += self.latency[machine] / 2
            return 0, f"{reading:.9f}\n"
        if "FinishedAt" in script:
            return 0, "".join(f"{self.stamp(machine, at):.9f}\n" for where, at in self.stops if where == machine)
        if "fault:" in script:
            if "-u ployz-volume-plugin.service" not in script:
                return 0, ""
            since = int(re.search(r"--since @(-?\d+)", script)[1])
            lines = [(self.stamp(machine, at), text) for where, at, text in self.journal
                     if where == machine and self.stamp(machine, at) >= since]
            if "-o cat" in script:
                return 0, "".join(f"{text}\n" for _, text in lines)
            return 0, "".join(f"{at:.6f} machine ployzd[1]: {text}\n" for at, text in lines)
        if script.startswith("for d in"):
            lines = []
            two_writers = self.mutation == "two-writers"
            root = "off" if machine == self.writer or (two_writers and machine == self.mirror) else "absent"
            fs = "on" if machine == self.mirror and not two_writers else "absent"
            lines += [f"ro ployz/ployz/{PHYSICAL} {root}", f"ro ployz/ployz-mirror/{PHYSICAL}/fs {fs}"]
            snapshots = 12 if self.mutation == "snapshots" else 2
            lines += [f"snap ployz/ployz/{PHYSICAL}@s{i}" for i in range(snapshots)]
            if machine == self.writer and self.mutation != "elsewhere":
                lines.append("ct abc running db-1")
            elif machine != self.writer and self.mutation in ("elsewhere", "two-running"):
                lines.append("ct xyz running db-2")
            elif self.mutation == "stopped" and machine == self.mirror:
                lines.append("ct def exited db-old")
            return 0, "\n".join(lines) + "\n"
        if "pgrep" in script:
            if self.unreadable.get(machine) == "exec":
                return 255, ""
            if self.unreadable.get(machine) == "pgrep":
                return 0, "no\n"
            return 0, "yes\n" if self.receiving_on(machine) else "no\n"
        if "pg_isready" in script:
            return 0, ""
        if "systemctl kill" in script:
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
        return 0, "\n".join(lines) + "\n"


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
    def __init__(self):
        self.now = 1000.0
        self.probe = None

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds
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
            probe = FakeProbe(self.root / "evidence" / f"ackprobe-db-{logs}.jsonl", model, clock)
            probe.stdout = stdout
            probe.ack()
            clock.probe = probe
            return probe
        checks = volume_move.VolumeMove(self.manifest, runner=model, spawn=spawn, sleep=clock.sleep, clock=clock,
                                        out=self.root / "out")
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

    def test_kill_table_covers_every_cell_once_with_its_own_fault_point(self):
        self.assertEqual(sorted(row["cell"] for row in volume_move.KILLS), sorted(volume_move.CELLS))
        for row in volume_move.KILLS:
            with self.subTest(row["name"]):
                self.assertEqual(row["fault"], "kill-daemon:SetVolumeCopy." + row["cell"].replace(">", "To"))

    def test_rows_use_known_columns_and_no_retired_fault(self):
        for row in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(row["name"]):
                self.assertIn(row["op"], {"move", "mirror", "mirror-rm"})
                self.assertIn(row["machine"], {"writer", "other"})
                self.assertIn(row.get("fault_on"), {None, "writer", "other"})
                self.assertIn(row.get("mirror"), {None, "present", "absent"})
                self.assertIn(row["action"], {"none", "cancel", "runner-kill", "kill-ployz", "kill-docker"})
                self.assertIn(row["recover"], {"rerun", "release"})
                self.assertIn(row["trigger"], {None, "fault", "receiving", "stopped", "fault-after-stop"})
                self.assertTrue(row["step"])
                self.assertNotIn("after-record", row["fault"] or "")
        self.assertEqual(len({row["name"] for row in volume_move.CASES}), len(volume_move.CASES))

    def test_delete_mirror_and_first_mirror_cells_run_their_own_op(self):
        ops = {row["cell"]: (row["op"], row.get("mirror")) for row in volume_move.KILLS}
        self.assertEqual(ops["Writer>Forget"], ("mirror-rm", "present"))
        self.assertEqual(ops["Mirror>Forget"], ("mirror-rm", "present"))
        self.assertEqual(ops["None>Mirror"], ("mirror", "absent"))
        self.assertEqual(ops["Mirror>Mirror"], ("move", "present"))

    def test_undo_before_cut_cancels_inside_the_cut_after_the_container_stopped(self):
        undo = {row["name"]: row for row in volume_move.CASES}
        before = undo["undo-before-cut"]
        self.assertEqual((before["fault_on"], before["fault"], before["trigger"], before["recover"]),
                         ("writer", "kill-daemon:SetVolumeCopy.WriterToMirror", "fault", "release"))
        after = undo["undo-after-cut"]
        self.assertEqual((after["fault_on"], after["fault"], after["trigger"]),
                         ("other", "delay-rpc:SetVolumeCopy.MirrorToWriter:90", "fault-after-stop"))

    def test_cases_keep_the_clean_move_both_undos_and_one_kill_per_cell(self):
        self.assertEqual([row["name"] for row in volume_move.CASES],
                         ["clean", "undo-before-cut", "undo-after-cut", *(row["name"] for row in volume_move.KILLS)])

    def test_soak_runs_the_four_switch_steps_under_every_action(self):
        self.assertEqual([step[0] for step in volume_move.SOAK_STEPS], ["1-rounds", "3-cut", "4-final", "5-writer"])
        self.assertEqual(len(volume_move.soak_cases()), 3 + 4 * 5)

    def test_every_fault_names_a_point_the_daemon_defines(self):
        source = (volume_move.CORE / "crates/ployzd/src/machine_api/local_volume_copy.rs").read_text()
        points = set(re.findall(r'"SetVolumeCopy\.(\w+)"', source))
        # CELLS lists the cells a case kills; no case kills the Writer raise or a restore seed's forget.
        self.assertEqual(points, {cell.replace(">", "To") for cell in volume_move.CELLS} | {"WriterToWriter", "SeedToForget"})
        point = "SetVolumeCopy\\.(" + "|".join(sorted(points)) + ")"
        for row in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(row["name"]):
                if row["fault"] is not None:
                    self.assertRegex(row["fault"], rf"^(kill-daemon:{point}|delay-rpc:{point}:\d+)$")

    def test_nth_after_counts_only_lines_after_the_stop(self):
        lines = volume_move.stamped("100.5 m-2 ployzd[1]: fault: round\n200.0 m-2 ployzd[1]: fault: final\n"
                                    "-- No entries --\n201.5 m-2 ployzd[1]: fault: writer\n")
        self.assertEqual([at for at, _ in lines], [100.5, 200.0, 201.5])
        self.assertTrue(volume_move.nth_after(lines, 150.0, 2).endswith("fault: writer"))
        self.assertTrue(volume_move.nth_after(lines, 150.0, 1).endswith("fault: final"))
        self.assertIsNone(volume_move.nth_after(lines, 200.5, 2))
        self.assertIsNone(volume_move.nth_after(lines, 300.0, 1))

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
        row = dict(volume_move.KILLS[0], name="rekeyed", fault="set-volume-copy:any-cell-at-all")
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
                            and (any(model.fault.values()) or not armed)):
                        return 1, "Cloud did not answer"
                    return cli(args)
                def arming(machine, script):
                    if "echo" in script and "PLOYZD_FAULT" in script and not model.holds:
                        model.hold(5, "cron-sync")
                        reply = guest(machine, script)
                        model.clock.now += 10
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
            if "echo" in script and "PLOYZD_FAULT" in script and not model.holds:
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
            if "echo" in script and "PLOYZD_FAULT" in script and not model.holds:
                model.hold(5, "cron-sync")
                reply = guest(machine, script)
                model.clock.now += 10
                return reply
            return guest(machine, script)
        def submitted(args):
            if args[0] == "volume" and args[1] in ("move", "mirror", "release") and any(model.fault.values()):
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

    def test_a_holder_that_ends_just_before_the_limit_skips_a_fault_case_without_starting_its_op(self):
        checks, model = self.checks()
        model.hold(volume_move.BUSY_LIMIT - 2)
        checks.case_name = volume_move.KILLS[0]["name"]
        armed = []
        with redirect_stdout(io.StringIO()), self.assertRaisesRegex(volume_move.Skipped, "before the fault"):
            checks.start_op("move", "machine-2", arm=lambda: armed.append(True))
        self.assertEqual((armed, model.started), ([], []))
        self.assertEqual([wait["holder"] for wait in checks.busy_waits], ["sync-1"])

        checks, model = self.checks()
        model.hold(volume_move.BUSY_LIMIT - 2)
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0]])
        self.assertIn("before the fault was armed", summary["cases"][0]["skipped"] or "", summary["cases"][0])

    def test_a_runs_reply_that_stalls_past_the_limit_skips_the_case_and_still_writes_the_summary(self):
        checks, model = self.checks()
        cli, stalled = model.cli, []
        def stall(args):
            if args[:2] == ["volume", "runs"] and checks.case_name == volume_move.KILLS[0]["name"] and not stalled:
                stalled.append(True)
                model.clock.now += volume_move.BUSY_LIMIT + 1
            return cli(args)
        model.cli = stall
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertIn("before the fault was armed", summary["cases"][0]["skipped"] or "", summary["cases"][0])
        self.assertTrue(summary["cases"][1]["passed"], summary["cases"][1])
        self.assertTrue((self.root / "out" / "summary.json").exists())

    def test_a_run_that_just_ended_does_not_replace_the_holder_already_seen(self):
        checks, model = self.checks()
        model.hold(100, "sync-a")
        model.just_ended = 1
        checks.case_name = "clean"
        with redirect_stdout(io.StringIO()):
            row = checks.start_op("move", "machine-2")
        self.assertEqual(row["state"], "done")
        self.assertEqual([wait["holder"] for wait in checks.busy_waits], ["sync-a"])

    def test_a_failed_disarm_stops_the_run_and_still_writes_the_summary(self):
        checks, model = self.checks()
        model.disarm_fails = True
        with redirect_stdout(io.StringIO()):
            summary = checks.run_all([volume_move.KILLS[0], volume_move.CASES[0]])
        self.assertEqual([case["name"] for case in summary["cases"]], [volume_move.KILLS[0]["name"]])
        self.assertIn("disarming the fault failed on machine-1, machine-2", summary["cases"][0]["failures"][-1])
        self.assertFalse(summary["passed"])
        self.assertEqual(json.loads((self.root / "out" / "summary.json").read_text())["passed"], False)

    def test_a_disarm_whose_daemon_never_comes_back_reports_the_machine(self):
        stubs = self.root / "stubs"
        stubs.mkdir()
        for name, body in (("touch", "exit 0"), ("sed", "exit 0"), ("sleep", "exit 0"), ("seq", "echo 1 2 3"),
                           ("systemctl", '[ "$1" = is-active ] && exec /bin/sh is-active; exit 0')):
            (stubs / name).write_text(f"#!/bin/sh\n{body}\n")
            (stubs / name).chmod(0o755)
        bash = shutil.which("bash")
        def guest(args, **kwargs):
            return subprocess.run([bash, *args[-2:]], cwd=self.root, env=dict(PATH=str(stubs)),
                                  capture_output=True, text=True)
        checks = volume_move.VolumeMove(self.manifest, runner=guest, out=self.root / "out")
        for active, failed in (("exit 0", []), ("exit 3", ["machine-1"])):
            with self.subTest(active):
                (self.root / "is-active").write_text(active)
                with redirect_stdout(io.StringIO()):
                    self.assertEqual(checks.fault(None, "machine-1", check=False), failed)

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
            if "PLOYZD_FAULT" in script and "echo" not in script:
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
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-after-cut")
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
                    if args[:2] == ["volume", "move"] and any(model.fault.values()) and not refused:
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
                if args[:2] == ["volume", "move"] and model.fired and not any(model.fault.values()):
                    return 1, json.dumps(dict(error=dict(code="internal", message="placement exploded")))
                return cli(args)
            return refusing
        for case, plant, last in ((volume_move.CASES[0], every_move_fails, "failed"),
                                  (kill, the_rerun_is_refused, "refused")):
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
                self.assertIn(f"the last run ended {last}", result["failures"])
                self.assertIn("the Volume ended on machine-1, not machine-2", result["failures"])

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
        self.assertEqual(result["failures"], ["the Volume ended on machine-1, not machine-2"])

    def test_mirror_cases_and_a_released_undo_pass_ending_on_the_writer_they_started_on(self):
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-after-cut")
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
                        if args[:2] == ["volume", "mirror"] and any(model.fault.values()):
                            model.fired = True
                            row = dict(id=f"run-{len(model.runs)}", kind="mirror", state="done", finished_at=model.now())
                            model.runs.append(row)
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
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-after-cut")
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
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-before-cut")
        for ended, failure in (("cancelled", None), ("lost", None),
                               ("done", "the cancel did not land: the run ended done"),
                               ("failed", "the cancel did not land: the run ended failed")):
            with self.subTest(ended):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                cli = model.cli

                def running(args, cli=cli):
                    if args[:2] == ["volume", "move"]:
                        model.fired = True
                        row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                        model.runs.append(row)
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
                self.assertTrue(result["triggered"], result)
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
                    self.assertIn(f"the first run {run['id']} ended {ended} at {run['finished_at']}",
                                  result["failures"][0])
                    self.assertIn("before the trigger", result["failures"][0])

    def test_a_triggered_case_fails_when_its_first_run_ended_with_no_end_time(self):
        kill = volume_move.KILLS[0]
        checks, model = self.checks()
        cli = model.cli

        def unstamped(args, cli=cli):
            if args[:2] == ["volume", "move"] and any(model.fault.values()):
                model.fired = True
                row = dict(id=f"run-{len(model.runs)}", kind="move", state="failed")
                model.runs.append(row)
                return 0, json.dumps(row)
            return cli(args)
        model.cli = unstamped
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([kill])["cases"][0]
        self.assertFalse(result["passed"], result)
        self.assertEqual(result["failures"], ["the first run run-0 ended failed with no finished_at, so the trigger "
                                              "cannot be tied to it"])

    def test_an_inngest_run_lookup_that_fails_raises_and_names_the_query(self):
        checks, model = self.checks()
        model.postgres = lambda sql: (1, "")
        with self.assertRaises(RuntimeError) as raised:
            checks.inngest_run(dict(id="run-7"))
        self.assertIn("select inngest_run_id from volume_run where id = 'run-7'", str(raised.exception))

    def test_an_undo_passes_only_when_cloud_accepted_its_cancel(self):
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-before-cut")
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
                        model.fired = True
                        row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                        model.runs.append(row)
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
                # The model has no Container stop clock, so the trigger fires as the cut would.
                checks.wait_trigger = lambda case, row, since, side: "machine-2: fault: delaying MirrorToWriter"
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

    def first_move(self, model, ends, state="failed", effect=None, claimed=True):
        """The case's first move is accepted and ends state ends seconds later; effect(begun) schedules what it does."""
        cli, first = model.cli, []

        def scripted(args):
            if args[:2] != ["volume", "move"] or first:
                return cli(args)
            row = dict(id=f"run-{len(model.runs)}", kind="move", state="running", finished_at=None)
            model.runs.append(row)
            first.append(row)
            begun, destination = model.clock(), args[-1]

            def end():
                row["state"], row["finished_at"] = state, iso(begun + ends)
                if state == "done":
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

    def test_a_fault_after_stop_is_ordered_on_host_time_whichever_clock_runs_ahead(self):
        case = next(case for case in volume_move.soak_cases() if case["name"] == "soak-4-final-runner-kill")
        # (name, Machine clock offsets, exec latency, host second of the final-pull fault, first run end, passes)
        for name, offsets, latency, final, ends, passes in (
                # Codex r3 finding 2: the warm-round fault at +2 is stamped +22, after the writer's stop at +10.
                ("trigger Machine 20 s ahead, no final fault", {"machine-2": 20.0}, 0.0, None, 11, False),
                ("writer 20 s ahead, a real final fault", {"machine-1": 20.0}, 0.0, 12, 13, True),
                ("no skew, a real final fault", {}, 0.0, 11, 13, True),
                ("small measured skew, a real final fault", {"machine-1": -0.2, "machine-2": 0.3}, 0.2, 11, 13, True)):
            with self.subTest(name):
                shutil.rmtree(self.root / "out", ignore_errors=True)
                checks, model = self.checks()
                model.offset.update(offsets)
                model.latency = {"machine-1": latency, "machine-2": latency}

                def effect(begun, model=model, final=final):
                    model.log_fault("machine-2", begun + 2, "fault: delaying switch verb")
                    model.at(begun + 10, lambda: model.stop("machine-1", begun + 10))
                    if final:
                        model.at(begun + final, lambda: model.log_fault("machine-2", begun + final,
                                                                        "fault: delaying switch verb"))
                self.first_move(model, ends, effect=effect)
                with redirect_stdout(io.StringIO()):
                    result = checks.run_all([case])["cases"][0]
                self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)
                if passes:
                    self.assertTrue(result["passed"], result)
                else:
                    self.assertFalse(result["passed"], result)
                    self.assertEqual(result["failures"], ["the trigger never fired before the run ended"], result)

    def test_a_fault_line_from_before_the_case_is_outside_its_window_on_a_clock_running_ahead(self):
        kill = volume_move.KILLS[0]
        self.assertEqual(kill["trigger"], "fault")
        checks, model = self.checks()
        model.offset["machine-2"] = 20.0
        prepare = checks.prepare

        def an_old_fault_line(case, other):
            prepare(case, other)
            model.log_fault("machine-2", model.clock() - 15)
        checks.prepare = an_old_fault_line
        self.first_move(model, 1)
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
                            model.at(begun + trips, lambda: model.stop("machine-1", begun + trips))
                        else:
                            model.at(begun + trips - 0.5, lambda: model.stop("machine-1", begun + trips - 0.5))
                            model.at(begun + trips, lambda: model.log_fault("machine-2", begun + trips))
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
                        self.assertIn(f"the first run {run['id']} ended failed at {run['finished_at']}, before the "
                                      "trigger", result["failures"][0])

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
        # A late RPC from a run that ended before the case logs the fault line while the request waits unclaimed.
        kill = volume_move.KILLS[0]
        checks, model = self.checks()
        first = self.first_move(model, 600, "lost", claimed=False,
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
        self.assertFalse(result["passed"], result)
        self.assertFalse(any("no Inngest run claimed" in failure for failure in result["failures"]), result)
        if not result.get("skipped"):
            self.assertIn("the trigger never fired before the run ended", result["failures"], result)

    def test_a_pull_admitted_before_its_receive_exists_is_not_credited_to_the_first_run(self):
        # Codex r4 finding 1 and Opus S1: no process exists at the clean start, and the receive spawns 30 s later.
        for name in ("soak-1-rounds-runner-kill", "soak-1-rounds-kill-ployz-writer", "soak-1-rounds-kill-docker-other"):
            with self.subTest(name):
                result, _ = self.run_with(name, lambda model: self.admit_pull(model, "machine-2", 30, 40))
                self.assert_not_credited(result)

    def test_a_cut_an_earlier_move_left_waiting_in_the_daemon_is_not_credited_to_the_first_run(self):
        # Codex r4's detached cut: the cut waits for the admission turn and stops the writer after the clean start.
        def waiting(model):
            model.task("machine-1", model.clock() + 20, "cut", lambda: model.stop("machine-1"))
        result, model = self.run_with("soak-3-cut-runner-kill", waiting)
        self.assert_not_credited(result)
        self.assertIn(("machine-1", "cut"), model.interrupted)

    def test_a_stop_already_handed_to_docker_before_the_barrier_is_not_credited_to_the_first_run(self):
        # dockerd outlives the daemon restart, and its default stop timeout lands the stop up to 10 s after it.
        def handed(model):
            moment = model.clock() + 8
            model.at(moment, lambda: model.stop("machine-1", moment))
        result, _ = self.run_with("soak-3-cut-runner-kill", handed)
        self.assert_not_credited(result)

    def test_a_clock_that_steps_after_calibration_fails_the_case(self):
        # Codex r4 finding 2 and Opus S2: a warm-round fault at +2, the writer's stop at +10, and maybe a final fault.
        for name, machine, step, final, ends, passes in (
                ("trigger Machine steps forward 20 s", "machine-2", 20.0, None, 11, False),
                ("writer steps back 9 s", "machine-1", -9.0, None, 11, False),
                ("no step, a real final fault", None, 0.0, 11, 13, True)):
            with self.subTest(name):
                def effect(begun, model=None, machine=machine, step=step, final=final):
                    model = self.model
                    if machine:
                        model.at(begun + 1, lambda: model.offset.update({machine: model.offset[machine] + step}))
                    model.log_fault("machine-2", begun + 2, "fault: delaying switch verb")
                    model.at(begun + 10, lambda: model.stop("machine-1", begun + 10))
                    if final:
                        model.at(begun + final, lambda: model.log_fault("machine-2", begun + final,
                                                                        "fault: delaying switch verb"))

                def keep(model):
                    self.model = model
                result, _ = self.run_with("soak-4-final-runner-kill", keep, ends, effect)
                self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)
                if passes:
                    self.assertTrue(result["passed"], result)
                else:
                    self.assertFalse(result["passed"], result)
                    self.assertTrue(any(failure.startswith(f"the clock on {machine} moved")
                                        for failure in result["failures"]), result["failures"])

    def test_a_trigger_is_owned_by_when_its_line_was_stamped_not_by_when_a_poll_saw_it(self):
        # Codex bot round 8: the fault trigger polls every second, so a fault the first run made at +1.2 is seen at
        # +2, after that run ended at +1.5. A fault a later run made after the first run ended still fails.
        for ends, trips, passes in ((1.5, 1.2, True), (1.2, 1.5, False)):
            with self.subTest(ends=ends, trips=trips):
                def effect(begun, model=None, trips=trips):
                    model = self.model
                    model.at(begun + trips, lambda: model.log_fault("machine-2", begun + trips))

                def keep(model):
                    self.model = model
                result, _ = self.run_with("kill-snapshot", keep, ends, effect)
                self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)
                if passes:
                    self.assertTrue(result["passed"], result)
                else:
                    self.assertFalse(result["passed"], result)
                    self.assertEqual(len(result["failures"]), 1, result["failures"])
                    self.assertIn("before the trigger", result["failures"][0])

    def test_a_slow_move_whose_clock_drifts_within_its_bound_passes(self):
        # Codex bot round 8: the case end can come some 4000 s after calibration, so a fixed 0.5 s allowance fails a
        # valid case whose clock slews 300 ppm. The fault lands at +600, the first run fails at +2390, and its rerun
        # takes 1790 s.
        def drifting(model):
            self.model = model
            model.rate["machine-2"] = 300e-6
            cli = model.cli

            def slow_rerun(args):
                if args[:2] != ["volume", "move"] or not first:
                    return cli(args)
                row = dict(id=f"run-{len(model.runs)}", kind="move", state="running", finished_at=None)
                model.runs.append(row)
                begun, destination = model.clock(), args[-1]

                def end():
                    row["state"], row["finished_at"] = "done", iso(begun + 1790)
                    model.mirror, model.writer = model.writer, destination
                model.at(begun + 1790, end)
                return 0, json.dumps(row)
            model.cli = slow_rerun

        def effect(begun, model=None):
            model = self.model
            model.at(begun + 600, lambda: model.log_fault("machine-2", begun + 600))
        case = next(case for case in volume_move.CASES if case["name"] == "kill-snapshot")
        shutil.rmtree(self.root / "out", ignore_errors=True)
        checks, model = self.checks()
        prepare = checks.prepare

        def prepared(case, other):
            prepare(case, other)
            drifting(model)
        checks.prepare = prepared
        first = self.first_move(model, 2390, effect=effect)
        with redirect_stdout(io.StringIO()):
            result = checks.run_all([case])["cases"][0]
        self.assertEqual((result["run"], result["recovery"]), ("failed", ["done"]), result)
        full = json.loads((self.root / "out" / "kill-snapshot.json").read_text())
        self.assertGreater(full["ended"] - full["started"], 3900, full["started"])
        self.assertTrue(result["passed"], result)

    def test_a_cancel_restarts_the_runner_exactly_once_even_when_its_lookup_raises(self):
        # Codex bot round 8: act kills the runner before cancel, and an inngest_run that raises inside cancel left
        # the runner dead for every later case.
        undo = next(case for case in volume_move.CASES if case["name"] == "undo-before-cut")
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
                    model.fired = True
                    row = dict(id=f"run-{len(model.runs)}", kind="move", state="running")
                    model.runs.append(row)
                    first.append(row)
                    return 0, json.dumps(row)

                def unreadable(sql, first=first):
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
        self.assertGreaterEqual(min(at for _, at in model.restarts) - self.planted, volume_move.BUSY_LIMIT)
        self.assertEqual(model.pulls, {"machine-2"})

    def test_each_planted_defect_fails_the_run(self):
        for mutation in ("two-writers", "stopped", "snapshots", "lost"):
            with self.subTest(mutation):
                summary, _ = self.run_cases([volume_move.CASES[0]], mutation)
                self.assertFalse(summary["passed"])


if __name__ == "__main__":
    unittest.main()
