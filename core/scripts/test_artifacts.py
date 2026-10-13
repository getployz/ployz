#!/usr/bin/env python3
"""Which edits change which artifact keys, and how the local cache keeps, returns and drops builds."""

import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import artifacts  # noqa: E402

KINDS = ["sdk", "ployzd", "ployz"]


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
        self.assertEqual(self.changed_by("crates/ployz-config-wasm/src/zz_probe.rs"), {"sdk"})
        self.assertEqual(self.changed_by("crates/ployz-core/src/zz_probe.rs"), set(KINDS))

    def test_workspace_files_move_every_key(self):
        self.assertEqual(self.changed_by("Cargo.toml", "\n"), set(KINDS))

    def test_tests_docs_and_generated_outputs_move_nothing(self):
        self.assertEqual(self.changed_by("crates/ployz-core/tests/zz_probe.rs"), set())
        self.assertEqual(self.changed_by("crates/ployz-sdk/zz_probe.md"), set())
        self.assertEqual(self.changed_by("crates/ployz-sdk/generated/config-wasm.mjs"), set())

    def test_build_options_are_part_of_the_key(self):
        self.assertNotEqual(artifacts.key("sdk", "features=eval"), artifacts.key("sdk", "features="))

    def test_a_clean_tree_has_its_commit_key(self):
        if artifacts.git("status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock", "scripts"):
            self.skipTest("the checkout has uncommitted changes")
        for kind in KINDS:
            self.assertEqual(artifacts.key(kind), artifacts.key(kind, rev="HEAD"))


class Cache(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        patch = mock.patch.object(artifacts, "CACHE", self.root / "cache")
        patch.start()
        self.addCleanup(patch.stop)

    def build(self, name, content):
        directory = self.root / name
        (directory / "generated").mkdir(parents=True)
        (directory / "ployz-sdk.node").write_bytes(content)
        (directory / "generated" / "config-wasm.mjs").write_bytes(content + b".mjs")
        return directory

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

    def test_prune_drops_entries_unused_for_a_month(self):
        found = artifacts.Key("ployzd", "f" * 32, {})
        (self.root / "built").mkdir()
        (self.root / "built" / "ployzd").write_bytes(b"daemon")
        artifacts.store(found, self.root / "built")
        month = time.time() - 31 * 86400
        os.utime(self.root / "cache" / "ployzd" / found.value, (month, month))
        artifacts.prune()
        self.assertIsNone(artifacts.fetch(found, self.root / "out"))


if __name__ == "__main__":
    unittest.main()
