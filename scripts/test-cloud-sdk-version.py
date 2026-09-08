#!/usr/bin/env python3
"""Rung 1: reject SDK drift before Cloud installs or deploys."""
import runpy
import json
from pathlib import Path
import tempfile
import unittest

check = runpy.run_path(str(Path(__file__).with_name("check-cloud-sdk-version.py")))["check"]


class CloudSdkVersionTest(unittest.TestCase):
    def test_published_sdk_and_every_binding_must_match(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            package = Path(directory) / "package.json"
            manifest.write_text('[workspace.package]\nversion = "1.2.3"\n')
            cloud = {
                "dependencies": {"@ployz/sdk": "1.2.3"},
                "optionalDependencies": {
                    f"@ployz/sdk-{platform}": "1.2.3"
                    for platform in ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64")
                },
            }
            package.write_text(json.dumps(cloud))
            check(manifest, package)
            sdk = manifest.parent / "crates/ployz-sdk/package.json"
            sdk.parent.mkdir(parents=True)
            sdk.write_text(json.dumps({"version": "1.2.3"}))
            cloud["dependencies"]["@ployz/sdk"] = "link:../crates/ployz-sdk"
            package.write_text(json.dumps(cloud))
            check(manifest, package)
            sdk.write_text(json.dumps({"version": "1.2.2"}))
            with self.assertRaisesRegex(ValueError, "@ployz/sdk"):
                check(manifest, package)
            cloud["dependencies"]["@ployz/sdk"] = "1.2.3"
            for section in cloud.values():
                for name in list(section):
                    for invalid in ("1.2.2", "^1.2.3", "file:../sdk.tgz", None):
                        with self.subTest(package=name, pin=invalid):
                            if invalid is None:
                                del section[name]
                            else:
                                section[name] = invalid
                            package.write_text(json.dumps(cloud))
                            with self.assertRaisesRegex(ValueError, name):
                                check(manifest, package)
                            section[name] = "1.2.3"


if __name__ == "__main__":
    unittest.main()
