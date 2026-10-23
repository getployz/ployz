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
            (["core/DESIGN.md"], {"contracts"}),
            (["docs/user/services/domains.md"], {"contracts", "rust-lint", "rust-tests"}),
            (["docs/agents/landing.md"], {"contracts"}),
            (["core/relay/compose.yaml"], {"contracts"}),
            (["dashboard/src/app.tsx"], {"contracts", "cloud"}),
            (["core/crates/ployz/tests/management_connect.rs"], {"contracts", "rust-lint", "rust-tests", "macos-cli"}),
            (["core/crates/ployzd/src/main.rs"], {"contracts", "rust-lint", "rust-tests"}),
            (["core/crates/ployz-core/src/rpc.rs"], {"contracts", "rust-lint", "rust-tests", "uptime", "macos-cli", "cloud"}),
            (["core/crates/ployz-core/tests/rpc.rs"], {"contracts", "rust-lint", "rust-tests", "macos-cli"}),
            (["core/crates/ployz-store/src/lib.rs"], {"contracts", "rust-lint", "rust-tests", "macos-cli", "cloud"}),
            (["core/crates/ployz-store/tests/snapshots/agent_tools.json"], {"contracts", "rust-lint", "rust-tests", "macos-cli", "cloud"}),
            (["dashboard/uptime/src/main.rs"], {"contracts", "uptime"}),
            (["dashboard/uptime/Cargo.lock"], {"contracts", "uptime"}),
            (["dashboard/uptime/Dockerfile"], {"contracts", "uptime"}),
            (["dashboard/drizzle/20261011044514_uptime_checks/migration.sql"], {"contracts", "cloud", "uptime"}),
            (["dashboard/src/modules/uptime/incident-fence.ts"], {"contracts", "cloud", "uptime"}),
            (["core/scripts/build-cloud-sdk.sh"], {"contracts", "cloud"}),
            (["core/scripts/check-sdk-types.sh"], {"contracts", "cloud"}),
            (["core/scripts/pack-release.sh"], {"contracts", "release-contracts"}),
            (["core/scripts/verify/boat.py", "core/scripts/verify/test_boat.py"], {"contracts"}),
            (["core/scripts/verify-cluster", "core/scripts/verify/releases.py"], {"contracts"}),
            (["core/scripts/verify/dns-soak.sh"], {"contracts", "rust-lint", "rust-tests", "macos-cli"}),
            (["dashboard/scripts/verify/up.sh", "dashboard/scripts/verify/runner.mjs"], {"contracts"}),
            (["dashboard/scripts/verify/up.sh", "dashboard/scripts/verify/seed.ts"], {"contracts", "cloud"}),
            (["dashboard/scripts/pr-check.sh"], {"contracts", "cloud"}),
        ]
        for paths, expected in cases:
            with self.subTest(paths=paths):
                self.assertEqual(ci["select"](paths), expected)
        for path in [".github/workflows/ci.yml", "core/crates/new-config", "core/crates/not-a-member/src/lib.rs", "unknown.config"]:
            with self.subTest(path=path):
                self.assertEqual(ci["select"]([path]), ci["JOBS"] - {"release-contracts"})
        for path in ["core/Cargo.lock", "core/Cargo.toml", "core/crates/ployzd/Cargo.toml", "core/.cargo/config.toml", ".github/workflows/release.yml", ".github/workflows/release-contracts.yml"]:
            with self.subTest(path=path):
                self.assertEqual(ci["select"]([path]), ci["JOBS"])
        for path in ["core/Cargo.lock", "core/Cargo.toml", "core/crates/ployzd/Cargo.toml", "core/.cargo/config.toml"]:
            with self.subTest(path=path, pull_request=True):
                self.assertNotIn("release-contracts", ci["select"]([path], pull_request=True))
        for path in [".github/workflows/release-contracts.yml", "core/.goreleaser.yaml"]:
            with self.subTest(path=path, pull_request=True):
                self.assertIn("release-contracts", ci["select"]([path], pull_request=True))
        self.assertIn("release-contracts", ci["select"]([".github/workflows/ci.yml", "core/.goreleaser.yaml"]))
        self.assertIn("cloud", ci["select"](["core/crates/ployz-sdk/tests/config-contract.mjs"]))
        for path in ["core/crates/ployz/src/main.rs", ".github/workflows/ci.yml"]:
            with self.subTest(path=path, pull_request=True):
                self.assertNotIn("macos-cli", ci["select"]([path], pull_request=True))

    def test_store_postgres_reruns_only_when_its_inputs_change(self):
        for paths in [["core/crates/ployz-store/src/lib.rs"], ["core/crates/ployz-core/src/rpc.rs"], ["docs/user/services/domains.md"],
                      ["core/crates/ployz/tests/fixtures/published-manifest.json"], ["core/Cargo.lock"], [".github/workflows/ci.yml"], ["unknown.config"]]:
            with self.subTest(paths=paths):
                self.assertTrue(ci["store_postgres"](paths, pull_request=True))
        for paths in [["core/crates/ployz/src/main.rs"], ["core/crates/ployzd/src/main.rs"], ["dashboard/src/app.tsx"]]:
            with self.subTest(paths=paths):
                self.assertFalse(ci["store_postgres"](paths, pull_request=True))
                self.assertTrue(ci["store_postgres"](paths))
        # STORE_INPUTS names ployz-store's path dependencies by hand; a new one must join it.
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=SCRIPT.parent.parent / "core"))
        path_deps = {p["name"]: {d["name"] for d in p["dependencies"] if d.get("path")} for p in metadata["packages"]}
        self.assertEqual(path_deps["ployz-store"] | path_deps["ployz-core"], {"ployz-core"})

    def test_only_dns_soak_reaches_rust_from_the_verify_harness(self):
        # select runs no Rust check for core/scripts/verify/; a crate that reads another of its files must join dns-soak.sh.
        read = subprocess.check_output(["git", "grep", "-ho", "scripts/verify/[A-Za-z0-9_.-]*", "--", "core/crates"], cwd=SCRIPT.parent.parent, text=True)
        self.assertEqual(read.split(), ["scripts/verify/dns-soak.sh"])

    def test_sdk_and_config_wasm_crates_follow_their_path_dependencies(self):
        self.assertEqual(
            ci["sdk_and_config_wasm_crates"](),
            {"ployz-sdk", "ployz-config-wasm", "ployz", "ployz-core", "ployz-store", "ployz-build"},
        )

    def test_agent_tool_changes_run_the_tool_tests(self):
        # Cloud's `pnpm test` drives the sidebar's tools against the Store; rust-tests drives them through `ployz mcp`.
        for path in ["core/crates/ployz-store/src/tools/mod.rs", "core/crates/ployz/src/handlers/mcp.rs", "dashboard/src/modules/agent/agent-tools.ts"]:
            with self.subTest(path=path):
                self.assertIn("cloud", ci["select"]([path]))
        for path in ["core/crates/ployz-store/src/tools/mod.rs", "core/crates/ployz/src/handlers/mcp.rs"]:
            with self.subTest(path=path):
                self.assertIn("rust-tests", ci["select"]([path]))

    def test_renames_and_deletions_include_the_old_production_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", *args], cwd=root, stderr=subprocess.DEVNULL, text=True).strip()
            git("init", "--quiet")
            source = root / "core/crates/ployz-core/src/rpc.rs"
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

    def test_a_merge_queue_run_selects_mains_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", *args], cwd=root, stderr=subprocess.DEVNULL, text=True).strip()
            git("init", "--quiet")
            manifest = root / "core/Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text("[workspace]\n")
            git("add", ".")
            git("commit", "--quiet", "-m", "base")
            base = git("rev-parse", "HEAD")
            manifest.write_text("[workspace]\nresolver = \"3\"\n")
            git("commit", "--quiet", "-am", "bump")
            env = {key: value for key, value in os.environ.items() if key not in {"GITHUB_OUTPUT", "GITHUB_STEP_SUMMARY"}}
            # The queue run is main's only run, so it also runs what a PR leaves to main.
            for event, runs in [("pull_request", False), ("merge_group", True)]:
                output = subprocess.check_output([sys.executable, str(SCRIPT), "select", base], cwd=root, env={**env, "GITHUB_EVENT_NAME": event}, text=True)
                for job in ["macos-cli", "release-contracts"]:
                    with self.subTest(event=event, job=job):
                        self.assertEqual(job in json.loads(output), runs)

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

    def test_ci_ready_skips_a_draft_and_runs_after_any_check_result(self):
        workflow = (SCRIPT.parent.parent / ".github/workflows/ci.yml").read_text()
        job = workflow.split("\n  ready:\n", 1)[1].split("\n\n  ", 1)[0]
        condition = next(line for line in job.splitlines() if line.startswith("    if:"))
        # always(): a failed or cancelled check must still reach the step that fails CI ready.
        self.assertIn("always()", condition)
        # A draft can't merge, and the queue runs CI ready again, so a draft reports skipped.
        self.assertIn("!github.event.pull_request.draft", condition)
        self.assertNotIn("exit 1", job)

    def test_cloud_stage_runs_alone_and_preserves_failure(self):
        script = SCRIPT.parent.parent / "dashboard/scripts/pr-check.sh"
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
