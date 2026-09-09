#!/usr/bin/env python3
"""Rung 1: check routing and prevent an incomplete CI result from passing."""
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci.py").resolve()
ci = runpy.run_path(str(SCRIPT))


class CiTest(unittest.TestCase):
    def test_selects_checks_for_actual_consumers(self):
        cases = [
            (["DESIGN.md"], {"contracts"}),
            (["evidence/product-paths.tsv"], {"contracts"}),
            (["cloud/src/app.tsx"], {"contracts", "cloud"}),
            (["crates/ployz/tests/connect/relay.rs"], {"contracts", "rust-lint", "rust-tests", "macos-cli"}),
            (["crates/ployzd/src/main.rs"], {"contracts", "rust-lint", "rust-tests"}),
            (["crates/ployz-core/src/rpc.rs"], {"contracts", "rust-lint", "rust-tests", "macos-cli", "cloud", "sdk-types"}),
            (["scripts/build-cloud-sdk.sh"], {"contracts", "cloud"}),
            (["scripts/pack-release.sh"], {"contracts", "release-contracts"}),
        ]
        for paths, expected in cases:
            with self.subTest(paths=paths):
                self.assertEqual(ci["select"](paths), expected)
        for path in ["Cargo.lock", ".github/workflows/ci.yml", "crates/ployzd/Cargo.toml", "crates/new-config", "unknown.config"]:
            with self.subTest(path=path):
                self.assertEqual(ci["select"]([path]), ci["JOBS"])
        self.assertIn("compose", ci["select"](["crates/ployz/compose-helper/main.go"]))
        self.assertIn("cloud", ci["select"](["crates/ployz-sdk/tests/config-contract.mjs"]))

    def test_renames_and_deletions_include_the_old_production_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", *args], cwd=root, stderr=subprocess.DEVNULL, text=True).strip()
            git("init", "--quiet")
            source = root / "crates/ployz-core/src/rpc.rs"
            source.parent.mkdir(parents=True)
            source.write_text("production\n")
            git("add", ".")
            git("commit", "--quiet", "-m", "base")
            base = git("rev-parse", "HEAD")
            source.rename(root / "notes.md")
            git("add", "-A")
            git("commit", "--quiet", "-m", "move")
            env = {key: value for key, value in os.environ.items() if key not in {"GITHUB_OUTPUT", "GITHUB_STEP_SUMMARY"}}
            output = subprocess.check_output([sys.executable, str(SCRIPT), "select", base], cwd=root, env=env, text=True)
            self.assertIn("cloud", json.loads(output))
            failed = subprocess.run([sys.executable, str(SCRIPT), "select", "missing-base"], cwd=root, env=env, capture_output=True)
            self.assertNotEqual(failed.returncode, 0)

    def test_required_missing_failed_or_cancelled_checks_do_not_pass(self):
        selected = ["contracts", "rust-lint", "rust-tests"]
        needs = {job: {"result": "success" if job in selected else "skipped"} for job in ci["JOBS"]}
        needs["changes"] = {"result": "success", "outputs": {"jobs": json.dumps(selected)}}
        ci["check_result"](needs)
        for status in ["failure", "cancelled", "skipped"]:
            with self.subTest(status=status):
                needs["rust-tests"]["result"] = status
                with self.assertRaises(ValueError):
                    ci["check_result"](needs)
        needs["rust-tests"]["result"] = "success"
        del needs["rust-tests"]
        with self.assertRaises(KeyError):
            ci["check_result"](needs)
        needs["rust-tests"] = {"result": "success"}
        needs["changes"]["result"] = "failure"
        with self.assertRaises(ValueError):
            ci["check_result"](needs)
        needs["changes"]["result"] = "success"
        for jobs in [[], ["contracts", "unknown"], None]:
            needs["changes"]["outputs"]["jobs"] = json.dumps(jobs)
            with self.assertRaises(ValueError):
                ci["check_result"](needs)

    def test_cloud_stage_runs_alone_and_preserves_failure(self):
        script = SCRIPT.parent.parent / "cloud/scripts/pr-check.sh"
        with tempfile.TemporaryDirectory() as directory:
            pnpm = Path(directory) / "pnpm"
            pnpm.write_text('#!/bin/sh\nprintf "%s\\n" "$*"\nexit "${CHECK_EXIT:-0}"\n')
            pnpm.chmod(0o755)
            for status in [0, 7]:
                env = {**os.environ, "PATH": f"{directory}:{os.environ['PATH']}", "CHECK_EXIT": str(status)}
                result = subprocess.run(["bash", str(script), "typecheck"], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0 if status == 0 else 1)
                self.assertIn("\ntypecheck\n", result.stdout)
                self.assertEqual(result.stdout.count("Starting "), 1)
            result = subprocess.run(["bash", str(script), "unknown"], capture_output=True)
            self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
