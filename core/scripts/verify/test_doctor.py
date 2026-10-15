#!/usr/bin/env python3
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
scripts = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("verify_cluster", str(scripts / "verify-cluster"))
cluster = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cluster)

DRAINING = "a" * 64
FORCED = f" WARN forcing retire after 300 s; never acked: [] container={DRAINING} service=app/web"
STARTED = f" INFO retire clock started; forcing after 300 s container={DRAINING} service=app/web"
FAILED = f" WARN failed to retire Container, retrying in 5 s container={DRAINING} error=refused"


class Journal:
    def __init__(self, lines):
        self.lines = lines

    def exec(self, node, command):
        with tempfile.TemporaryDirectory() as bin_dir:
            journalctl = Path(bin_dir) / "journalctl"
            journalctl.write_text("#!/bin/sh\ncat\n")
            journalctl.chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
            journal = "".join(line + "\n" for line in self.lines).encode()
            return subprocess.run(command, input=journal, capture_output=True, check=True, env=env)


class OverdueDrains(unittest.TestCase):
    def setUp(self):
        self.run_ = {"machines": [{"name": "machine-1", "machine_id": "m1", "ssh": "root@machine-1"}]}
        ps = {"containers": [{"machine_id": "m1", "container_id": DRAINING, "draining": True,
                              "resolved_spec": {"name": "web"}}]}
        saved = cluster.cli
        cluster.cli = lambda run, commands, args: subprocess.CompletedProcess(args, 0, json.dumps(ps))
        self.addCleanup(setattr, cluster, "cli", saved)

    def overdue(self, *lines):
        return cluster.overdue_drains(self.run_, None, Journal(list(lines)))

    def test_a_forced_current_episode_fails(self):
        self.assertEqual(self.overdue(STARTED, FORCED),
                         ["machine-1: web Container aaaaaaaaaaaa still drains past 300 s; see journalctl -u ployz"])

    def test_a_failed_force_then_rollback_does_not_fail_a_fresh_retirement(self):
        self.assertEqual(self.overdue(STARTED, FORCED, FAILED, STARTED), [])

    def test_a_force_interrupted_by_a_restart_does_not_fail_a_fresh_retirement(self):
        self.assertEqual(self.overdue(STARTED, FORCED, STARTED), [])

    def test_a_retirement_forced_again_after_a_fresh_start_fails(self):
        self.assertEqual(len(self.overdue(STARTED, FORCED, STARTED, FORCED)), 1)

if __name__ == "__main__":
    unittest.main()
