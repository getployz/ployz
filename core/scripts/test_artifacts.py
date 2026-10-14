#!/usr/bin/env python3
"""Which edits change which artifact keys, how the local cache keeps, returns and drops builds, and which CI uploads it takes."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import tomllib
import time
from types import SimpleNamespace
import unittest
from unittest import mock
import zipfile

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import artifacts  # noqa: E402

KINDS = ["sdk", "config-wasm", "ployzd", "ployz"]


def keys():
    return {kind: artifacts.key(kind).value for kind in KINDS}


class Keys(unittest.TestCase):
    def changed_by(self, path, text="// probe\n"):
        """The kinds whose key moves when path (relative to core/) is created or appended to."""
        before = keys()
        file = artifacts.CORE / path
        original = file.read_bytes() if file.exists() else None
        try:
            file.parent.mkdir(parents=True, exist_ok=True)
            with file.open("a") as output:
                output.write(text)
            after = keys()
        finally:
            if original is None:
                file.unlink()
            else:
                file.write_bytes(original)
        self.assertEqual(keys(), before, "restoring the file restores every key")
        return {kind for kind in KINDS if after[kind] != before[kind]}

    def test_a_crate_edit_moves_the_artifacts_built_from_it(self):
        self.assertEqual(self.changed_by("crates/ployzd/src/zz_probe.rs"), {"ployzd"})
        self.assertEqual(self.changed_by("crates/ployz/src/zz_probe.rs"), {"ployz", "sdk"})
        self.assertEqual(self.changed_by("crates/ployz-sdk/src/zz_probe.rs"), {"sdk"})
        self.assertEqual(self.changed_by("crates/ployz-config-wasm/src/zz_probe.rs"), {"sdk", "config-wasm"})
        self.assertEqual(self.changed_by("crates/ployz-core/src/zz_probe.rs"), set(KINDS))

    def test_a_docs_page_moves_every_artifact_that_embeds_it(self):
        self.assertEqual(self.changed_by("../docs/user/zz_probe.md", "probe\n"), {"ployz", "sdk"})

    def test_workspace_files_move_every_key(self):
        self.assertEqual(self.changed_by("Cargo.toml", "\n"), set(KINDS))

    def test_the_toolchain_moves_every_key(self):
        self.assertEqual(self.changed_by("rust-toolchain.toml", "\n"), set(KINDS))

    def test_every_rust_pin_names_the_toolchain_files_compiler(self):
        channel = tomllib.loads((artifacts.CORE / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
        repository = artifacts.CORE.parent
        pins = [("Dockerfile", r"FROM rust:([\d.]+)-"),
                (".github/actions/setup-rust/action.yml", r"dtolnay/rust-toolchain@([\d.]+)"),
                (".github/workflows/ci.yml", r"dtolnay/rust-toolchain@([\d.]+)"),
                (".github/workflows/release-contracts.yml", r"RUST_VERSION: ([\d.]+)"),
                ("core/Cargo.toml", r'rust-version = "([\d.]+)"')]
        for path, pattern in pins:
            found = re.findall(pattern, (repository / path).read_text())
            self.assertTrue(found, f"{path} pins no Rust version")
            self.assertEqual(set(found), {channel}, f"{path} pins a Rust other than core/rust-toolchain.toml")

    def test_tests_docs_and_generated_outputs_move_nothing(self):
        self.assertEqual(self.changed_by("crates/ployz-core/tests/zz_probe.rs"), set())
        self.assertEqual(self.changed_by("crates/ployz-sdk/zz_probe.md"), set())
        self.assertEqual(self.changed_by("crates/ployz-sdk/generated/config-wasm.mjs"), set())

    def test_build_options_are_part_of_the_key(self):
        self.assertNotEqual(artifacts.key("sdk", "features=eval"), artifacts.key("sdk", "features="))
        self.assertNotEqual(artifacts.key("sdk", "features= profile=dev"), artifacts.key("sdk", "features= profile=release"))

    def test_ci_looks_up_the_sdk_under_the_options_its_build_stores(self):
        script = (artifacts.CORE / "scripts" / "build-cloud-sdk.sh").read_text()
        stored = re.search(r'^release="(.*)"$', script, re.M)[1].replace("$features", "")
        workflow = (artifacts.CORE.parent / ".github" / "workflows" / "cloud.yml").read_text()
        looked_up = re.search(r'artifacts\.py lookup sdk --extra ("[^"]*"|\S+)', workflow)[1].strip('"')
        self.assertEqual(looked_up, stored)

    def test_a_local_build_stores_the_sdk_under_other_options_than_ci(self):
        def stored_extra(ci):
            """The --extra a cache-missing build stores the SDK under, run with stubbed tools in a scratch checkout."""
            with tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                (root / "scripts").mkdir()
                shutil.copy(artifacts.CORE / "scripts" / "build-cloud-sdk.sh", root / "scripts")
                (root / "scripts" / "build-config-wasm.sh").touch()
                (root / "crates" / "ployz-sdk").mkdir(parents=True)
                for profile in ["debug", "release"]:
                    (root / "target" / profile).mkdir(parents=True)
                    (root / "target" / profile / "libployz_sdk.so").touch()
                    (root / "target" / profile / "libployz_sdk.dylib").touch()
                subprocess.run(["git", "init", "-q"], cwd=root, check=True)
                stubs = root / "bin"
                stubs.mkdir()
                for tool, body in [("python3", 'echo "$*" >> "$STUB_LOG"\ncase "$2" in key) echo "key $*" ;; fetch) exit 1 ;; esac'),
                                   ("cargo", ":"), ("node", ":")]:
                    (stubs / tool).write_text(f"#!/usr/bin/env bash\n{body}\n")
                    (stubs / tool).chmod(0o755)
                env = {"PATH": f"{stubs}:{os.environ['PATH']}", "HOME": scratch, "STUB_LOG": str(root / "log"),
                       "CARGO_TARGET_DIR": str(root / "target"), **({"CI": "true"} if ci else {})}
                subprocess.run(["bash", root / "scripts" / "build-cloud-sdk.sh"], env=env, check=True)
                stores = re.findall(r"^scripts/artifacts\.py store sdk --extra (.*) --expect ", (root / "log").read_text(), re.M)
                self.assertEqual(len(stores), 1)
                return stores[0]

        self.assertEqual(stored_extra(ci=True), "features= profile=release")
        self.assertNotEqual(stored_extra(ci=False), stored_extra(ci=True))

    def test_a_clean_tree_has_its_commit_key(self):
        if artifacts.git("status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "scripts"):
            self.skipTest("the checkout has uncommitted changes")
        for kind in KINDS:
            self.assertEqual(artifacts.key(kind), artifacts.key(kind, rev="HEAD"))


class Scratch(unittest.TestCase):
    """A scratch cache and a fake GitHub serving self.uploads."""

    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        for name, value in [("CACHE", self.root / "cache"), ("gh", self.github)]:
            patch = mock.patch.object(artifacts, name, value)
            patch.start()
            self.addCleanup(patch.stop)
        self.uploads = []

    def upload(self, found, content, *, run, fork=False, branch="main", expired=False, digest=None):
        """Record a CI upload of found, newest first, as the artifacts API lists them."""
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as files:
            files.writestr("ployz-sdk.node", content)
            files.writestr("generated/config-wasm.mjs", content + b".mjs")
        archive = archive.getvalue()
        row = {"id": len(self.uploads), "name": found.artifact, "expired": expired,
               "digest": digest or "sha256:" + hashlib.sha256(archive).hexdigest(),
               "workflow_run": {"id": run, "head_sha": f"sha{run}", "head_branch": branch, "repository_id": 1,
                                "head_repository_id": 2 if fork else 1}}
        self.uploads.insert(0, (row, archive))

    def github(self, path, stdout=None, capture_output=False):
        if path.endswith("/zip"):
            stdout.write(next(archive for row, archive in self.uploads if path.endswith(f"/{row['id']}/zip")))
            return None
        name = path.split("name=")[1].split("&")[0]
        listing = {"artifacts": [row for row, _ in self.uploads if row["name"] == name]}
        return SimpleNamespace(stdout=json.dumps(listing).encode())

    def build(self, name, content):
        directory = self.root / name
        (directory / "generated").mkdir(parents=True)
        (directory / "ployz-sdk.node").write_bytes(content)
        (directory / "generated" / "config-wasm.mjs").write_bytes(content + b".mjs")
        return directory


class Cache(Scratch):
    def test_a_stored_build_comes_back_whole(self):
        found = artifacts.Key("sdk", "a" * 32, {"ployz-sdk": "digest"})
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))
        artifacts.store(found, self.build("built", b"addon"), revision="abc")
        meta = artifacts.fetch(found, self.root / "out")
        self.assertEqual(meta["revision"], "abc")
        self.assertEqual(meta["crates"], {"ployz-sdk": "digest"})
        self.assertEqual((self.root / "out" / "ployz-sdk.node").read_bytes(), b"addon")
        self.assertEqual((self.root / "out" / "generated" / "config-wasm.mjs").read_bytes(), b"addon.mjs")

    def test_a_second_store_of_one_key_keeps_the_first(self):
        found = artifacts.Key("sdk", "b" * 32, {})
        artifacts.store(found, self.build("first", b"first"))
        artifacts.store(found, self.build("second", b"second"))
        artifacts.fetch(found, self.root / "out")
        self.assertEqual((self.root / "out" / "ployz-sdk.node").read_bytes(), b"first")
        self.assertEqual([entry.name for entry in (self.root / "cache" / "sdk").iterdir()], ["b" * 32])

    def test_prune_drops_the_least_recently_used_beyond_the_cap(self):
        used, old, new = (artifacts.Key("sdk", letter * 32, {}) for letter in "cde")
        for index, found in enumerate([used, old, new]):
            artifacts.store(found, self.build(f"build-{index}", b"x" * 1000))
            os.utime(self.root / "cache" / "sdk" / found.value, (time.time() - 100 + index, time.time() - 100 + index))
        artifacts.fetch(used, self.root / "out")
        with mock.patch.object(artifacts, "CACHE_BYTES", 5500):  # two entries of about 2.3 kB fit, three don't
            artifacts.prune()
        self.assertEqual(sorted(entry.name for entry in (self.root / "cache" / "sdk").iterdir()),
                         sorted([used.value, new.value]))

    def test_a_miss_downloads_the_newest_live_upload_from_main_and_keeps_it(self):
        found = artifacts.Key("sdk", "1" * 32, {})
        self.upload(found, b"old", run=10)
        self.upload(found, b"main", run=11)
        self.upload(found, b"expired", run=12, expired=True)
        self.upload(found, b"fork", run=13, fork=True)
        self.upload(found, b"branch", run=14, branch="feature")
        meta = artifacts.fetch(found, self.root / "out")
        self.assertEqual((meta["run"], meta["revision"]), (11, "sha11"))
        self.assertEqual((self.root / "out" / "ployz-sdk.node").read_bytes(), b"main")
        self.assertEqual((self.root / "out" / "generated" / "config-wasm.mjs").read_bytes(), b"main.mjs")
        self.uploads.clear()
        self.assertEqual(artifacts.fetch(found, self.root / "elsewhere")["run"], 11)

    def test_only_fork_uploads_is_a_miss(self):
        found = artifacts.Key("sdk", "2" * 32, {})
        self.upload(found, b"fork", run=20, fork=True)
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))

    def test_only_uploads_from_other_branches_of_this_repository_is_a_miss(self):
        found = artifacts.Key("sdk", "5" * 32, {})
        self.upload(found, b"branch", run=50, branch="feature")
        self.upload(found, b"tag", run=51, branch="v0.3.0")
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))

    def test_a_download_that_does_not_match_its_digest_is_not_cached(self):
        found = artifacts.Key("sdk", "3" * 32, {})
        self.upload(found, b"tampered", run=30, digest="sha256:" + "0" * 64)
        with self.assertRaises(SystemExit):
            artifacts.fetch(found, self.root / "out")
        self.uploads.clear()
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))

    def test_without_gh_a_miss_is_a_miss(self):
        def missing(*args, **kwargs):
            raise FileNotFoundError("gh")
        with mock.patch.object(artifacts, "gh", missing):
            self.assertIsNone(artifacts.fetch(artifacts.Key("sdk", "4" * 32, {}), self.root / "out"))

    def test_prune_drops_entries_unused_for_a_month(self):
        found = artifacts.Key("ployzd", "f" * 32, {})
        (self.root / "built").mkdir()
        (self.root / "built" / "ployzd").write_bytes(b"daemon")
        artifacts.store(found, self.root / "built")
        month = time.time() - 31 * 86400
        os.utime(self.root / "cache" / "ployzd" / found.value, (month, month))
        artifacts.prune()
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))


class Main(Scratch):
    """Stale builds from main, in a scratch repository whose SDK depends on ployz-core."""

    def setUp(self):
        super().setUp()
        self.core = self.root / "core"
        patch = mock.patch.object(artifacts, "CORE", self.core)
        patch.start()
        self.addCleanup(patch.stop)
        self.write("Cargo.toml", '[workspace]\nmembers = ["crates/ployz-sdk", "crates/ployz-store", "crates/ployz-config-wasm", "crates/ployz-core"]\n')
        for name, dependencies in [("ployz-sdk", ["ployz-store", "ployz-core"]), ("ployz-store", []),
                                   ("ployz-config-wasm", ["ployz-core"]), ("ployz-core", [])]:
            self.write(f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\n[dependencies]\n' +
                       "".join(f'{dependency} = {{ path = "../{dependency}" }}\n' for dependency in dependencies))
            self.write(f"crates/{name}/src/lib.rs", "// one\n")
        self.write("crates/ployz-sdk/index.d.ts", "export {};\n")
        self.write("../docs/user/page.md", "page\n")
        # The repository holds core/ and the docs ployz-store embeds, as the real one does.
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=self.root, check=True)
        self.built = self.commit("built on main")
        self.store_build(b"main")
        self.write("crates/ployz-store/src/lib.rs", "// two\n")
        self.commit("main without a build")
        artifacts.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def write(self, path, text):
        (self.core / path).parent.mkdir(parents=True, exist_ok=True)
        (self.core / path).write_text(text)

    def commit(self, message):
        artifacts.git("add", "-A")
        artifacts.git("-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-q", "-m", message)
        return artifacts.git("rev-parse", "HEAD").decode().strip()

    def store_build(self, content):
        artifacts.store(artifacts.key("sdk"), self.build("main-build", content), revision=self.built)

    def stale(self):
        stderr = io.StringIO()
        with mock.patch.object(sys, "stderr", stderr):
            meta = artifacts.fetch_main(artifacts.key("sdk"), self.root / "out", "SDK=exact to build", same_surface=True)
        return meta, stderr.getvalue().strip()

    def test_an_edit_behind_an_unchanged_interface_takes_mains_newest_build_and_names_what_differs(self):
        self.write("crates/ployz-store/src/lib.rs", "// three\n")
        meta, said = self.stale()
        self.assertEqual(meta["revision"], self.built)
        self.assertEqual((self.root / "out" / "ployz-sdk.node").read_bytes(), b"main")
        self.assertEqual(said, f"sdk: stale main@{self.built[:9]} (ployz-store differs) -- SDK=exact to build")

    def test_an_edit_to_the_embedded_config_wasm_builds_though_the_interface_is_unchanged(self):
        for crate in ["ployz-config-wasm", "ployz-core"]:
            with self.subTest(crate):
                self.write(f"crates/{crate}/src/lib.rs", "// three\n")
                meta, said = self.stale()
                self.assertIsNone(meta)
                self.assertFalse((self.root / "out").exists())
                self.assertEqual(said, f"sdk: config-wasm differs from main@{self.built[:9]}, so building")
                self.write(f"crates/{crate}/src/lib.rs", "// one\n")

    def test_an_interface_edit_builds(self):
        self.write("crates/ployz-sdk/index.d.ts", "export declare function added(): void;\n")
        meta, said = self.stale()
        self.assertIsNone(meta)
        self.assertFalse((self.root / "out").exists())
        self.assertIn("interface differs from main@", said)

    def test_main_without_a_build_builds(self):
        shutil.rmtree(self.root / "cache")
        meta, said = self.stale()
        self.assertIsNone(meta)
        self.assertEqual(said, "sdk: main has no build in its last 20 commits")


class Slots(Scratch):
    def test_a_third_build_waits_for_one_of_two_to_finish(self):
        third = threading.Event()

        def build():
            with artifacts.build_slot():
                third.set()

        with contextlib.redirect_stderr(io.StringIO()) as said:
            with artifacts.build_slot():
                with artifacts.build_slot():
                    waiter = threading.Thread(target=build)
                    waiter.start()
                    self.assertFalse(third.wait(1.5), "both slots are taken")
                self.assertTrue(third.wait(5), "a slot freed up")
            waiter.join()
        self.assertEqual(said.getvalue(), "waiting for one of 2 build slots\n")


if __name__ == "__main__":
    unittest.main()
