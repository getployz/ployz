#!/usr/bin/env python3
"""Release channels and downloads follow install.sh's PLOYZ_CHANNEL_URL and PLOYZ_GITHUB_URL."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import releases  # noqa: E402


class Channel(unittest.TestCase):
    def resolved_from(self, environment):
        asked = []

        def commands(argv):
            asked.append(argv[-1])
            return subprocess.CompletedProcess(argv, 0, stdout=b"v0.3.4\n")

        with tempfile.TemporaryDirectory() as core, mock.patch.dict(os.environ, environment, clear=True):
            Path(core, "Cargo.toml").write_text('[workspace.package]\nversion = "0.3.3"\n')
            self.assertEqual(releases.resolve("stable", commands, Path(core)), "0.3.4")
        return asked

    def test_a_channel_resolves_on_ployz_sh_by_default(self):
        self.assertEqual(self.resolved_from({}), ["https://ployz.sh/v0/stable"])
        self.assertEqual(self.resolved_from({"PLOYZ_CHANNEL_URL": ""}), ["https://ployz.sh/v0/stable"])

    def test_a_channel_resolves_where_ployz_channel_url_says(self):
        self.assertEqual(
            self.resolved_from({"PLOYZ_CHANNEL_URL": "https://channels.example.test/"}),
            ["https://channels.example.test/v0/stable"],
        )


class Download(unittest.TestCase):
    def base(self, environment):
        with mock.patch.dict(os.environ, environment, clear=True):
            return releases.download_base("0.3.4")

    def test_a_release_downloads_from_ployz_github_url(self):
        self.assertEqual(self.base({}), "https://github.com/getployz/ployz/releases/download/v0.3.4/")
        self.assertEqual(self.base({"PLOYZ_GITHUB_URL": ""}), "https://github.com/getployz/ployz/releases/download/v0.3.4/")
        self.assertEqual(
            self.base({"PLOYZ_GITHUB_URL": "https://staging.example.test/ployz/"}),
            "https://staging.example.test/ployz/releases/download/v0.3.4/",
        )

    def staged(self, environment):
        with mock.patch.dict(os.environ, environment, clear=True):
            return releases.staged(), releases.mirror_environment()

    def test_a_staged_release_caches_apart_and_reaches_the_guest_installer(self):
        self.assertEqual(self.staged({}), ("", []))
        self.assertEqual(self.staged({"PLOYZ_GITHUB_URL": "https://github.com/getployz/ployz/"}), ("", []))
        suffix, environment = self.staged({"PLOYZ_GITHUB_URL": "https://staging.example.test/ployz"})
        self.assertRegex(suffix, r"^-[0-9a-f]{12}$")
        self.assertEqual(environment, ["PLOYZ_GITHUB_URL=https://staging.example.test/ployz"])


if __name__ == "__main__":
    unittest.main()
