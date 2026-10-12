#!/usr/bin/env python3
"""Exercise the volume move checks with a command model, without Incus, Cloud or ZFS."""

from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("volume_move", Path(__file__).resolve().parents[1] /
                                            "test-verify-volume-move.py")
volume_move = importlib.util.module_from_spec(spec)
spec.loader.exec_module(volume_move)

PHYSICAL = "verify-production_vol-1"


def entry(name, wire):
    return f'                {name}: (f, "{name}", Req, "f", Reply, CAP, "{wire}", Container),'


def catalog(names):
    kept = [entry("MetricHistory", "ployz.metrics.history.v1"), entry("InspectStorage", "ployz.storage.inspect.v1"),
            entry("PrepareVolumes", "ployz.storage.prepare.v1")]
    docker = [entry(name, f"ployz.volume.{name.lower()}.v1")
              for name in ("CreateVolume", "ListVolumes", "InspectVolume", "RemoveVolume")]
    moves = [entry(name, f"ployz.volume.{name.lower()}.v1") for name in names]
    return "\n".join(kept + docker + moves) + "\n"


MAIN_MOVES = ["InspectVolumeCopy", "Withdraw", "Freeze", "HandOver", "Thaw", "Close", "AcceptHandOff", "ClearFinal",
              "Promote", "StartHandedContainer", "Restore", "SeedVolume", "DeclareMirror", "BeginRound",
              "CommitSnapshots", "WarmSnapshot", "StartReceive", "InspectReceive", "PruneMirror", "DestroyMirror",
              "ForgetSnapshots", "ForgetLease", "DemoteVolume"]


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
        self.fired = False
        self.acks = []

    def other(self, machine):
        return "machine-2" if machine == "machine-1" else "machine-1"

    def __call__(self, args, **kwargs):
        if args[1] == "cli":
            code, output = self.cli(args[args.index("--") + 2:])
        elif args[1] == "exec":
            code, output = self.guest(args[args.index("--") - 1], args[-1])
        elif args[1] == "runner":
            code, output = 0, ""
        else:
            raise AssertionError(f"Unexpected helper command: {args}")
        return subprocess.CompletedProcess(args, code, output, "")

    def cli(self, args):
        if args[:2] == ["volume", "inspect"]:
            return 0, json.dumps(dict(id="1", environment=dict(project="verify", name="production")))
        if args[:2] == ["volume", "runs"]:
            return 0, json.dumps(dict(runs=list(reversed(self.runs))))
        if args[:2] == ["volume", "move"]:
            destination = args[-1]
            row = dict(id=f"run-{len(self.runs)}", kind="move", state="running")
            self.runs.append(row)
            if any(self.fault.values()):
                self.fired = True
                row["state"] = "failed"
            else:
                self.mirror, self.writer = self.writer, destination
                row["state"] = "done"
            return 0, json.dumps(row)
        if args[:2] == ["volume", "mirror"]:
            row = dict(id=f"run-{len(self.runs)}", kind="mirror", state="done")
            self.runs.append(row)
            if args[2] == "rm":
                self.mirror = None
            else:
                self.mirror = args[-1]
            return 0, json.dumps(row)
        if args[:2] == ["volume", "release"]:
            row = dict(id=f"run-{len(self.runs)}", kind="release", state="done")
            self.runs.append(row)
            return 0, json.dumps(row)
        return 0, "{}"

    def guest(self, machine, script):
        if "PLOYZD_FAULT" in script:
            value = script.split("PLOYZD_FAULT=", 2)[-1].split("'", 1)[0] if "echo" in script else None
            self.fault[machine] = value
            return 0, ""
        if "fault:" in script:
            return 0, "fault: killing the plugin after the record, before the effect\n" if self.fired else ""
        if script.startswith("for d in"):
            lines = []
            two_writers = self.mutation == "two-writers"
            root = "off" if machine == self.writer or (two_writers and machine == self.mirror) else "absent"
            fs = "on" if machine == self.mirror and not two_writers else "absent"
            lines += [f"ro ployz/ployz/{PHYSICAL} {root}", f"ro ployz/ployz-mirror/{PHYSICAL}/fs {fs}"]
            snapshots = 12 if self.mutation == "snapshots" else 2
            lines += [f"snap ployz/ployz/{PHYSICAL}@s{i}" for i in range(snapshots)]
            if machine == self.writer:
                lines.append("ct abc running db-1")
            elif self.mutation == "stopped" and machine == self.mirror:
                lines.append("ct def exited db-old")
            return 0, "\n".join(lines) + "\n"
        if "pg_isready" in script:
            return 0, ""
        if "SELECT id FROM ackprobe" in script:
            present = self.acks[:-1] if self.mutation == "lost" else self.acks
            return 0, "\n".join(str(ack) for ack in present) + "\n"
        return 0, ""


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
            provider="incus", state="ready", cloud=dict(dashboard=str(self.root / "dashboard")),
            machines=[dict(name="machine-1"), dict(name="machine-2")])))

    def tearDown(self):
        self.temp.cleanup()

    def checks(self, mutation=None):
        model = ClusterModel(mutation)
        clock = Clock()

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

    def test_catalog_on_main_names_every_extra_verb_and_the_missing_one(self):
        diff = volume_move.catalog_diff(catalog(MAIN_MOVES))
        self.assertEqual(len(diff["extra"]), 21)
        self.assertEqual(diff["missing"], ["SetVolumeCopy"])
        self.assertEqual(diff["kept_missing"], [])

    def test_catalog_passes_only_on_the_three_verbs_with_the_docker_verbs_kept(self):
        diff = volume_move.catalog_diff(catalog(["InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"]))
        self.assertEqual((diff["extra"], diff["missing"], diff["kept_missing"]), ([], [], []))
        dropped = catalog(["InspectVolumeCopy", "SetVolumeCopy", "SeedVolume"]).replace("MetricHistory:", "Gone:")
        self.assertEqual(volume_move.catalog_diff(dropped)["kept_missing"], ["MetricHistory"])

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

    def test_window_gap_counts_only_pairs_touching_the_window(self):
        acks = [(1, 0.0), (2, 1.0), (3, 30.0), (4, 31.0), (5, 100.0)]
        self.assertEqual(volume_move.window_gap(acks, 0.5, 31.0), (29.0, [2, 3]))
        self.assertEqual(volume_move.window_gap(acks, 30.5, 32.0), (69.0, [4, 5]))
        self.assertEqual(volume_move.window_gap([], 0, 1), (None, None))

    def test_downtime_reports_median_and_max_over_moves(self):
        results = [dict(op="move", probe=dict(longest_gap_seconds=gap)) for gap in (1.0, 9.0, 3.0)]
        results.append(dict(op="mirror", probe=dict(longest_gap_seconds=99.0)))
        self.assertEqual(volume_move.downtime(results), dict(moves=3, p50=3.0, max=9.0))

    def test_kill_table_covers_every_cell_with_known_columns(self):
        self.assertEqual({row["cell"] for row in volume_move.KILLS}, set(volume_move.CELLS))
        for row in volume_move.CASES + volume_move.soak_cases():
            with self.subTest(row["name"]):
                self.assertIn(row["op"], {"move", "mirror", "mirror-rm"})
                self.assertIn(row["machine"], {"writer", "other"})
                self.assertIn(row["action"], {"none", "cancel", "runner-kill", "kill-ployz", "kill-docker"})
                self.assertIn(row["recover"], {"rerun", "release"})
                self.assertIn(row["trigger"], {None, "fault", "receiving"})
                self.assertTrue(row["old_step"])
        self.assertEqual(len({row["name"] for row in volume_move.CASES}), len(volume_move.CASES))
        self.assertEqual(len(volume_move.soak_cases()), 3 + 9 * 5)

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

    def test_each_planted_defect_fails_the_run(self):
        for mutation in ("two-writers", "stopped", "snapshots", "lost"):
            with self.subTest(mutation):
                summary, _ = self.run_cases([volume_move.CASES[0]], mutation)
                self.assertFalse(summary["passed"])


if __name__ == "__main__":
    unittest.main()
