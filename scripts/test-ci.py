#!/usr/bin/env python3
"""Rung 1: check routing and prevent an incomplete CI result from passing."""
import fnmatch
import json
import os
import re
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
            (["docs/agents/domain.md"], {"contracts"}),
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

    def test_sdk_and_config_wasm_crates_follow_their_path_dependencies(self):
        self.assertEqual(
            ci["sdk_and_config_wasm_crates"](),
            {"ployz-sdk", "ployz-config-wasm", "ployz", "ployz-core", "ployz-store", "ployz-build"},
        )

    def test_pull_requests_test_changed_crates_and_their_dependents(self):
        cli = {"ployz", "ployz-sdk", "ployz-testkit", "ployzd"}
        store = cli | {"ployz-store"}
        cases = [
            (["core/crates/ployzd/tests/dial_stdio.rs"], {"ployzd"}),
            (["core/crates/ployz-verify/tests/model.rs"], {"ployz-verify"}),
            (["core/crates/ployz-config-wasm/tests/lower.rs"], {"ployz-config-wasm"}),
            # ployz's cli_shape test parses every crate's source.
            (["core/crates/ployzd/src/main.rs"], cli),
            (["core/crates/ployz-verify/src/lib.rs"], cli | {"ployz-verify"}),
            (["core/crates/ployz/src/main.rs"], cli),
            # A dev-dependency's dependents run their tests, but their own dependents keep their builds.
            (["core/crates/ployz-testkit/tests/cluster.rs"], {"ployz-testkit", "ployz", "ployzd"}),
            (["core/crates/ployz-store/src/lib.rs"], store),
            (["core/crates/ployz-build/src/lib.rs"], cli | {"ployz-build"}),
            (["core/crates/ployz-sdk/tests/node_logs.js"], cli),
            (["docs/user/services/domains.md"], store),
            (["core/crates/ployz/tests/fixtures/published-manifest.json"], store),
            (["dashboard/evals/agent/machines.json"], {"ployz-testkit", "ployz", "ployzd"}),
            (["core/crates/ployzd/tests/dial_stdio.rs", "dashboard/src/app.tsx", "core/DESIGN.md"], {"ployzd"}),
        ]
        for paths, expected in cases:
            with self.subTest(paths=paths):
                self.assertEqual(ci["rust_packages"](paths, pull_request=True), expected)
                self.assertIsNone(ci["rust_packages"](paths))
        for path in ["core/crates/ployz-core/src/rpc.rs", "core/Cargo.lock", "core/Cargo.toml", "core/crates/ployzd/Cargo.toml",
                     "core/rust-toolchain.toml", "core/.cargo/config.toml", ".github/workflows/ci.yml",
                     "core/crates/not-a-member/src/lib.rs", "core/crates/new-config", "unknown.config"]:
            with self.subTest(path=path):
                self.assertIsNone(ci["rust_packages"](["core/crates/ployzd/src/main.rs", path], pull_request=True))

    def test_nextest_filter_names_the_selection(self):
        self.assertEqual(ci["nextest_filter"](None), "all()")
        self.assertEqual(ci["nextest_filter"]({"ployzd", "ployz-sdk"}), "package(ployz-sdk) | package(ployzd)")

    def test_crate_inputs_cover_every_file_a_crate_reads_outside_itself(self):
        # A PR changing a file one crate reads from another's directory must still test the reader.
        core = SCRIPT.parent.parent / "core"
        packages = ci["workspace_packages"]()
        crates = {Path(package["manifest_path"]).parent: package for package in packages}
        normal_deps = {
            package["name"]: {dep["name"] for dep in package["dependencies"] if dep.get("path") and dep["kind"] in (None, "build")}
            for package in packages
        }
        def dependencies(name):
            found, pending = set(), [name]
            while pending:
                for dep in normal_deps[pending.pop()] - found:
                    found.add(dep)
                    pending.append(dep)
            return found
        # Literals that resolve outside their crate but are not inputs.
        not_inputs = {("crates/ployz/tests/connect/unix_session.rs", "../..")}  # the workspace's target directory
        reads = re.compile(r'(include_str!|include_bytes!|#\[path\s*=|\.join|concat!\(env!\("CARGO_MANIFEST_DIR"\),)\(?\s*"([^"]*\.\.[^"]*)"')
        for crate, package in crates.items():
            readable = {Path(other["manifest_path"]).parent for other in packages if other["name"] in dependencies(package["name"])}
            patterns = [pattern for pattern, name in ci["CRATE_INPUTS"] if name == package["name"]]
            for source in crate.rglob("*.rs"):
                if "target" in source.relative_to(crate).parts:
                    continue
                for context, literal in reads.findall(source.read_text()):
                    bases = (crate,) if context.startswith("concat!") else (source.parent, crate)
                    target = next(((base / literal.lstrip("/")).resolve() for base in bases if (base / literal.lstrip("/")).exists()), None)
                    if target is None or target.is_relative_to(crate) or (str(source.relative_to(core)), literal) in not_inputs:
                        continue
                    if any(target.is_relative_to(other) for other in readable):
                        continue
                    relative = target.relative_to(core.parent).as_posix()
                    covered = any(pattern.startswith(relative + "/*") for pattern in patterns) if target.is_dir() else any(
                        fnmatch.fnmatchcase(relative, pattern) for pattern in patterns
                    )
                    with self.subTest(source=str(source.relative_to(core)), literal=literal):
                        self.assertTrue(covered, f"add a CRATE_INPUTS pattern for {relative!r} read by {package['name']!r}")
        for pattern, name in ci["CRATE_INPUTS"]:
            with self.subTest(pattern=pattern):
                self.assertTrue(any(core.parent.glob(pattern.removesuffix("/*")) ), pattern)
                self.assertIn(name, normal_deps)

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

    def test_select_writes_the_rust_test_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", *args], cwd=root, stderr=subprocess.DEVNULL, text=True).strip()
            git("init", "--quiet")
            git("commit", "--quiet", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            source = root / "core/crates/ployzd/tests/dial_stdio.rs"
            source.parent.mkdir(parents=True)
            source.write_text("#[test]\nfn dial() {}\n")
            git("add", ".")
            git("commit", "--quiet", "-m", "daemon")
            def outputs(event, *args):
                output = root / "output"
                output.write_text("")
                env = {key: value for key, value in os.environ.items() if key != "GITHUB_STEP_SUMMARY"}
                subprocess.check_output([sys.executable, str(SCRIPT), "select", *args], cwd=root, env={**env, "GITHUB_EVENT_NAME": event, "GITHUB_OUTPUT": str(output)})
                return dict(line.split("=", 1) for line in output.read_text().splitlines())
            pr = outputs("pull_request", base)
            self.assertEqual((pr["nextest_filter"], pr["postgres"]), ("package(ployzd)", "false"))
            for event, args in [("push", [base]), ("pull_request", []), ("workflow_dispatch", [])]:
                with self.subTest(event=event, args=args):
                    full = outputs(event, *args)
                    self.assertEqual((full["nextest_filter"], full["postgres"]), ("all()", "true"))

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
