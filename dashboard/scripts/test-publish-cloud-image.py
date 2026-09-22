#!/usr/bin/env python3
"""Exercise publication ordering and superseded runs without contacting providers."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("publish-cloud-image.sh")
MOCKS = r"""
gh() {
  echo gh >> "$CALLS"
  [ "${GH_FAIL:-0}" = 0 ] || return 1
  if [ -f "$CALLS.pushed" ]; then echo "$HEAD_AFTER"; else echo "$HEAD_BEFORE"; fi
}
docker() {
  echo "docker $*" >> "$CALLS"
  if [ "$1" = login ]; then cat >/dev/null; fi
  if [ "$1" = push ]; then touch "$CALLS.pushed"; fi
}
npm() { echo "npm $*" >> "$CALLS"; }
railway() {
  echo "railway $*" >> "$CALLS"
  return "${RAILWAY_FAIL:-0}"
}
source "$SCRIPT"
"""


class PublishTests(unittest.TestCase):
    def run_publish(self, **overrides):
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory) / "calls"
            env = dict(os.environ, CALLS=str(calls), SCRIPT=str(SCRIPT),
                       GITHUB_SHA="current", GITHUB_REPOSITORY="getployz/ployz2",
                       GITHUB_ACTOR="test", GH_TOKEN="test", RAILWAY_TOKEN="test",
                       HEAD_BEFORE="current", HEAD_AFTER="current")
            env.update(overrides)
            result = subprocess.run(["bash", "-c", MOCKS], env=env, capture_output=True)
            return result.returncode, calls.read_text().splitlines()

    def test_publish_then_redeploy_both_from_source(self):
        code, calls = self.run_publish()
        self.assertEqual(code, 0)
        push = calls.index("docker push ghcr.io/getployz/ployz2-cloud:main")
        deploys = [line for line in calls if line.startswith("railway ")]
        self.assertEqual(len(deploys), 2)
        for line in deploys:
            self.assertIn("--from-source --yes --service ", line)
            self.assertGreater(calls.index(line), push)

    def test_superseded_before_publish(self):
        code, calls = self.run_publish(HEAD_BEFORE="newer")
        self.assertEqual(code, 0)
        self.assertEqual(calls, ["gh"])

    def test_superseded_after_push(self):
        code, calls = self.run_publish(HEAD_AFTER="newer")
        self.assertEqual(code, 0)
        self.assertTrue(any(line.startswith("docker push ") for line in calls))
        self.assertFalse(any(line.startswith("railway ") for line in calls))

    def test_provider_errors_fail(self):
        self.assertNotEqual(self.run_publish(GH_FAIL="1")[0], 0)
        code, calls = self.run_publish(RAILWAY_FAIL="1")
        self.assertNotEqual(code, 0)
        self.assertEqual(sum(line.startswith("railway ") for line in calls), 2)


if __name__ == "__main__":
    unittest.main()
