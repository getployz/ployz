#!/usr/bin/env python3
"""What deploy-bench reports for saved Deployments: each step's median and spread, and the time no step covered."""

from importlib.machinery import SourceFileLoader
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SCRIPT = Path(__file__).resolve().parent / "deploy-bench"
loader = SourceFileLoader("deploy_bench", str(SCRIPT))
spec = importlib.util.spec_from_loader("deploy_bench", loader)
bench = importlib.util.module_from_spec(spec)
loader.exec_module(bench)


def view(number, status, *steps):
    return {"number": number, "status": status, "steps": list(steps)}


def step(name, started_at, ended_at=None, service=None):
    out = {"name": name, "started_at": started_at}
    if ended_at is not None:
        out["ended_at"] = ended_at
    if service:
        out["service"] = service
    return out


RUNS = [
    view(
        1,
        "applied",
        step("receiving", 0, 100),
        step("preparing", 100, 400),
        step("fetching_source", 150, 300),
        step("building", 1000, 3000, "api"),
        step("starting", 3000, 3500, "api"),
    ),
    view(
        2,
        "applied",
        step("receiving", 0, 200),
        step("preparing", 200, 500),
        step("building", 500, 2500, "api"),
        step("starting", 2600, 3000, "api"),
    ),
    view(
        3,
        "failed",
        step("receiving", 0, 300),
        step("preparing", 300, 700),
        step("building", 700, 5700, "api"),
        step("starting", 5700, service="api"),
    ),
]


def by_step(rows):
    return {row["step"]: row for row in rows}


class Summary(unittest.TestCase):
    def test_each_step_has_its_median_and_spread_across_the_runs_that_ended_it(self):
        rows = by_step(bench.summarize(RUNS))
        self.assertEqual(
            rows["building api"],
            {"step": "building api", "runs": 3, "median_ms": 2000, "min_ms": 2000, "max_ms": 5000},
        )
        self.assertEqual(rows["receiving"]["median_ms"], 200)
        self.assertEqual(rows["starting api"]["runs"], 2)
        self.assertEqual(rows["starting api"]["median_ms"], 450)
        self.assertEqual(rows["fetching_source"]["runs"], 1)

    def test_the_gap_is_the_time_inside_the_steps_none_of_them_covered(self):
        rows = by_step(bench.summarize(RUNS))
        self.assertEqual((rows["(no step)"]["min_ms"], rows["(no step)"]["max_ms"]), (0, 600))
        self.assertEqual(rows["(no step)"]["median_ms"], 100)
        self.assertEqual(rows["(total)"]["max_ms"], 5700)

    def test_steps_print_in_deploy_order(self):
        names = [row["step"] for row in bench.summarize(RUNS)]
        self.assertEqual(
            names,
            ["receiving", "fetching_source", "preparing", "building api", "starting api", "(no step)", "(total)"],
        )

    def test_it_summarizes_saved_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for run in RUNS:
                path = Path(directory) / f"{run['number']}.json"
                path.write_text(json.dumps(run))
                paths.append(str(path))
            ran = subprocess.run(
                [sys.executable, str(SCRIPT), "--json", "--from", *paths],
                capture_output=True,
                text=True,
                check=True,
            )
        summary = json.loads(ran.stdout)
        self.assertEqual(summary["runs"], 3)
        self.assertEqual(by_step(summary["steps"])["preparing"]["median_ms"], 300)


if __name__ == "__main__":
    unittest.main()
